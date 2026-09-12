# Isolated full-Tauri lifecycle smoke. Never starts an installed executable or accesses its profile.
param([string]$Executable = (Join-Path $PSScriptRoot '..\src-tauri\target\release\streamcap-tauri.exe'))
$ErrorActionPreference = 'Stop'
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$desktop = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetRoot = [IO.Path]::GetFullPath((Join-Path $desktop 'src-tauri\target')) + [IO.Path]::DirectorySeparatorChar
if (-not $Executable.StartsWith($targetRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Smoke tests only start project build outputs, never the installed E-drive application.' }
$artifactRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'artifacts')).Path
$run = Join-Path $artifactRoot ('native-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,6))
if (Test-Path -LiteralPath $run) {throw 'Smoke output collision'}
$profile = Join-Path $run 'data'
[IO.Directory]::CreateDirectory($profile) | Out-Null
$temp = Join-Path $run 'tmp'; [IO.Directory]::CreateDirectory($temp) | Out-Null
$stdout = Join-Path $run 'app.stdout.log'; $stderr = Join-Path $run 'app.stderr.log'
$debugListener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0)
$debugListener.Start(); $debugPort = $debugListener.LocalEndpoint.Port; $debugListener.Stop()
$arguments = @('--data-dir',('"' + $profile + '"'),'--api-port','0','--smoke-seconds','30')
$environment = @{TEMP=$temp;TMP=$temp;WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=$debugPort --remote-debugging-address=127.0.0.1"}
$process = Start-Process -FilePath $Executable -ArgumentList $arguments -WorkingDirectory $run -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -Environment $environment
$owned = @{}
function Update-OwnedProcesses {
    $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath)
    do {
        $count = $owned.Count
        $parents = @([int]$process.Id) + @($owned.Keys)
        foreach ($p in $all) {if ($parents -contains [int]$p.ParentProcessId) {$owned[[int]$p.ProcessId]=$p}}
    } while ($count -ne $owned.Count)
    if (@($owned.Values | Where-Object {$_.Name -match '^(python|pythonw|node|streamcap-resolver)(\.exe)?$'}).Count -gt 0) {throw 'External interpreter or resolver was started'}
}
try {
    $readyFile = Join-Path $profile 'ready.json'
    $deadline = [DateTime]::UtcNow.AddSeconds(25)
    while (-not (Test-Path -LiteralPath $readyFile)) {
        if ($process.HasExited) {throw "Application exited before ready, code=$($process.ExitCode)"}
        if ([DateTime]::UtcNow -gt $deadline) {throw 'Application ready timed out'}
        Update-OwnedProcesses
        Start-Sleep -Milliseconds 200
    }
    $ready = Get-Content -LiteralPath $readyFile -Raw | ConvertFrom-Json
    if ($ready.dataDirectory -ne $profile) {throw 'Incorrect isolated data directory'}
    $base = 'http://' + $ready.address
    $status = Invoke-RestMethod -Uri "$base/api/status" -TimeoutSec 3
    if ($status.resolverMode -ne 'native' -or $status.totalRecordings -ne 0 -or -not $status.resolverReady) {throw 'Native status contract failed'}
    $debugOwner = $null
    for ($i=0;$i -lt 40;$i++) {
        Update-OwnedProcesses
        $debugOwner=Get-NetTCPConnection -State Listen -LocalPort $debugPort -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($debugOwner) {break}
        Start-Sleep -Milliseconds 100
    }
    if (-not $debugOwner -or -not $owned.ContainsKey([int]$debugOwner.OwningProcess)) {throw 'Debug listener is not owned by this isolated application; refusing to attach'}
    $probe = @'
import { chromium } from 'playwright';
import fs from 'node:fs/promises';
import path from 'node:path';
const [port,output,base]=process.argv.slice(1);
try {
  const browser=await chromium.connectOverCDP('http://127.0.0.1:'+port);
  const page=browser.contexts().flatMap(context=>context.pages()).find(page=>page.url().includes('tauri.localhost'));
  if(!page)throw Error('The isolated native app webview was not found');
  await page.waitForFunction(()=>document.body?.innerText.includes('StreamCap')&&window.__STREAMCAP_NATIVE_SMOKE__?.requests.some(r=>r.path==='/api/status'&&r.status===200),{},{timeout:12000});
  const snapshot=await page.locator('body').innerText();
  const stats=await page.evaluate(()=>window.__STREAMCAP_NATIVE_SMOKE__);
  const passed=snapshot.length>100&&snapshot.includes('录制')&&stats.errors.length===0&&stats.eventSources.length>0&&stats.eventSources.every(url=>url.startsWith(base+'/'))&&stats.requests.every(request=>request.status===200);
  await page.screenshot({path:path.join(output,'native-window.png'),fullPage:true});
  const result={passed,title:await page.title(),bodyCharacters:snapshot.length,transport:stats};
  await fs.writeFile(path.join(output,'ui-result.json'),JSON.stringify(result,null,2));
  console.log(JSON.stringify(result));
  // End only this validator. Dropping its CDP socket must not close the WebView2 browser.
  process.exit(passed?0:1);
} catch(error) {
  await fs.writeFile(path.join(output,'ui-result.json'),JSON.stringify({passed:false,error:String(error)},null,2));
  console.error(String(error));process.exit(1);
}
'@
    Push-Location $desktop
    try {& node --input-type=module -e $probe "$debugPort" "$run" "$base" 2>&1 | Tee-Object -LiteralPath (Join-Path $run 'ui-probe.log'); if ($LASTEXITCODE -ne 0) {throw 'Native UI probe failed'}} finally {Pop-Location}
    while (-not $process.HasExited) {
        Update-OwnedProcesses
        if ([DateTime]::UtcNow -gt $deadline.AddSeconds(30)) {throw 'Application shutdown timed out'}
        Start-Sleep -Milliseconds 250
    }
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {throw "Application exit code=$($process.ExitCode)"}
    $shutdown = Get-Content -LiteralPath (Join-Path $profile 'shutdown.json') -Raw | ConvertFrom-Json
    if (-not $shutdown.shutdownComplete -or $shutdown.activeRecordings -ne 0) {throw 'Ordered shutdown incomplete'}
    $remaining = @()
    for ($i=0;$i -lt 30;$i++) {
        $remaining=@($owned.Keys | Where-Object {Get-Process -Id $_ -ErrorAction SilentlyContinue})
        if ($remaining.Count -eq 0) {break}
        Start-Sleep -Milliseconds 200
    }
    if ($remaining.Count -gt 0) {throw "Owned child processes remain: $($remaining -join ',')"}
    if (@($owned.Values | Where-Object {$_.Name -eq 'msedgewebview2.exe'}).Count -eq 0) {throw 'No system WebView2 process was observed'}
    $listening = $false
    try {Invoke-RestMethod -Uri "$base/api/status" -TimeoutSec 1 | Out-Null; $listening=$true} catch {}
    if ($listening) {throw 'HTTP listener remained after application exit'}
    if (Get-NetTCPConnection -State Listen -LocalPort $debugPort -ErrorAction SilentlyContinue) {throw 'WebView2 debug listener remained after application exit'}
    $ui=Get-Content -LiteralPath (Join-Path $run 'ui-result.json') -Raw | ConvertFrom-Json
    $result=[ordered]@{passed=$true;executable=$Executable;resolver='native';ownedChildNames=@($owned.Values.Name|Sort-Object -Unique);ownedProcessesRemaining=$remaining.Count;apiListenerClosed=$true;webviewListenerClosed=$true;shutdown=$shutdown;ui=$ui;profile=$profile}
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $run 'result.json') -Encoding utf8
    $result | ConvertTo-Json -Depth 8
} finally {
    if (-not $process.HasExited) {$process.Kill($true);$process.WaitForExit(10000)|Out-Null}
}
