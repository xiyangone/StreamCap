# Isolated full-Tauri lifecycle smoke. Never starts an installed executable or accesses its profile.
param([string]$Executable = (Join-Path $PSScriptRoot '..\src-tauri\target\release\streamcap-tauri.exe'),[ValidateSet('Choice','Tray')][string]$ExitMode='Choice')
$ErrorActionPreference = 'Stop'
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$desktop = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetRoot = [IO.Path]::GetFullPath((Join-Path $desktop 'src-tauri\target')) + [IO.Path]::DirectorySeparatorChar
if (-not $Executable.StartsWith($targetRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Smoke tests only start project build outputs, never the installed E-drive application.' }
$allowedArtifacts=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot 'artifacts'))
$artifactRoot=if($env:STREAMCAP_TEST_ARTIFACTS){[IO.Path]::GetFullPath($env:STREAMCAP_TEST_ARTIFACTS)}else{$allowedArtifacts}
if($artifactRoot -ne $allowedArtifacts -and -not $artifactRoot.StartsWith($allowedArtifacts+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Invalid artifact directory'}
[IO.Directory]::CreateDirectory($artifactRoot)|Out-Null
$run = Join-Path $artifactRoot ('native-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,6))
if (Test-Path -LiteralPath $run) {throw 'Smoke output collision'}
$profile = Join-Path $run 'data'
[IO.Directory]::CreateDirectory($profile) | Out-Null
$temp = Join-Path $run 'tmp'; [IO.Directory]::CreateDirectory($temp) | Out-Null
$stdout = Join-Path $run 'app.stdout.log'; $stderr = Join-Path $run 'app.stderr.log'
$debugListener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0)
$debugListener.Start(); $debugPort = $debugListener.LocalEndpoint.Port; $debugListener.Stop()
$arguments = @('--data-dir',('"' + $profile + '"'),'--api-port','0','--smoke-seconds','90')
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
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
const [port,output,base,exitMode]=process.argv.slice(1);
let page;const diagnostic=[];
try {
  const browser=await chromium.connectOverCDP('http://127.0.0.1:'+port);
  page=browser.contexts().flatMap(context=>context.pages()).find(page=>page.url().includes('tauri.localhost'));
  if(!page)throw Error('The isolated native app webview was not found');
  page.setDefaultTimeout(12000);
  const errors=[];page.on('pageerror',error=>{errors.push(String(error));diagnostic.push(String(error));});
  page.on('console',message=>{if(message.type()==='error')diagnostic.push(message.text());});
  page.on('requestfailed',request=>diagnostic.push(request.url()+': '+request.failure()?.errorText));
  const navigation=await page.reload({waitUntil:'domcontentloaded'});
  const documentHeaders=await navigation.allHeaders();
  await page.waitForFunction(()=>document.body?.innerText.includes('StreamCap')&&window.__STREAMCAP_NATIVE_SMOKE__?.requests.some(r=>r.path==='/api/status'&&r.status===200),{},{timeout:15000});
  const native=()=>page.evaluate(async()=>(await import('/tauri-api/core.js')).invoke('desktop_ready'));
  assert.equal((await native()).decorated,false,'the web UI and the window must share one title bar');
  assert.equal((await native()).trayAvailable,true);
  await page.getByRole('button',{name:'最大化窗口',exact:true}).click();
  await page.waitForFunction(async()=>(await (await import('/tauri-api/core.js')).invoke('desktop_ready')).maximized);
  await page.getByRole('button',{name:'还原窗口',exact:true}).click();
  await page.waitForFunction(async()=>!(await (await import('/tauri-api/core.js')).invoke('desktop_ready')).maximized);
  const shell=await page.locator('.app-shell').boundingBox();assert.equal(shell.x,0);assert.equal(shell.y,0);
  assert.equal(await page.locator('.sidebar').evaluate(el=>getComputedStyle(el).borderRadius),'0px');
  await page.screenshot({path:path.join(output,'native-window.png'),fullPage:true,animations:'disabled'});
  const modal=page.getByRole('dialog',{name:'关闭 StreamCap',exact:true});
  const requestClose=async()=>{await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await modal.waitFor({state:'visible'});};
  await requestClose();assert.equal((await native()).closePending,true);
  await page.reload({waitUntil:'domcontentloaded'});await modal.waitFor({state:'visible'});
  await page.screenshot({path:path.join(output,'native-close-options.png'),fullPage:true,animations:'disabled'});
  await modal.getByRole('button',{name:'取消',exact:true}).click();await modal.waitFor({state:'hidden'});
  assert.equal((await native()).closing,false);assert.equal((await native()).closePending,false);
  await requestClose();await page.keyboard.press('Escape');await modal.waitFor({state:'hidden'});
  assert.equal((await native()).closePending,false);
  await requestClose();await modal.getByRole('checkbox',{name:'记住我的选择'}).check();await modal.getByRole('button',{name:/最小化到托盘/}).click();await modal.waitFor({state:'hidden'});
  const tray=await native();assert.equal(tray.visible,false);assert.equal(tray.closing,false);assert.equal(tray.closePending,false);
  const status=await (await fetch(base+'/api/status')).json();assert.equal(status.ok,true);assert.equal(status.resolverReady,true);
  const settings=await (await fetch(base+'/api/settings')).json();assert.equal(settings.userConfig.close_action,'tray');
  await page.evaluate(async()=>(await import('/tauri-api/core.js')).invoke('desktop_window_action',{action:'close'}));
  await page.waitForFunction(async()=>!(await (await import('/tauri-api/core.js')).invoke('desktop_ready')).closePending);
  await modal.waitFor({state:'hidden'});assert.equal((await native()).closing,false);
  const update=await fetch(base+'/api/settings',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({userConfig:{close_action:'ask'}})});assert.equal(update.status,200);
  await requestClose();await modal.getByRole('button',{name:'取消',exact:true}).click();await modal.waitFor({state:'hidden'});
  const stats=await page.evaluate(()=>window.__STREAMCAP_NATIVE_SMOKE__);
  assert.deepEqual(stats.errors,[]);assert.deepEqual(errors,[]);assert.ok(stats.eventSources.length>0&&stats.eventSources.every(url=>url.startsWith(base+'/')));
  assert.ok(stats.requests.every(request=>request.status===200));
  const security=await page.evaluate(async()=>{
    const {invoke}=await import('/tauri-api/core.js');
    let denied=false;try{await invoke('plugin:window|set_title',{label:'main',title:'Unauthorized title'});}catch{denied=true;}
    const policy=document.querySelector('meta[http-equiv="Content-Security-Policy"]')?.content ?? '';
    return {globalAbsent:typeof window.__TAURI__==='undefined',denied,policy,origin:window.__STREAMCAP_RUNTIME__?.apiOrigin};
  });
  security.policy=documentHeaders['content-security-policy']||security.policy;
  assert.equal(security.globalAbsent,true);assert.equal(security.denied,true);assert.equal(security.origin,base);
  assert.ok(security.policy.includes("default-src 'none'"));assert.ok(security.policy.includes(base));
  security.inlineScriptBlocked=await page.evaluate(()=>new Promise(resolve=>{
    const script=document.createElement('script');
    const finish=blocked=>{document.removeEventListener('securitypolicyviolation',onViolation);script.remove();delete window.__streamcapCspProbe;resolve(blocked);};
    const onViolation=event=>{if(event.effectiveDirective.startsWith('script-src')&&event.blockedURI==='inline')finish(true);};
    document.addEventListener('securitypolicyviolation',onViolation);script.textContent='window.__streamcapCspProbe=true';document.head.append(script);
    setTimeout(()=>finish(false),1000);
  }));
  assert.equal(security.inlineScriptBlocked,true);
  const result={passed:true,security,title:await page.title(),checks:['frameless-shell','maximize-restore','flush-layout','close-cancel','escape-cancel','tray-keeps-backend','remember-tray','close-preference-change',exitMode==='Tray'?'tray-menu-exit':'explicit-dialog-exit'],transport:stats,exitMode};
  await fs.writeFile(path.join(output,'ui-result.json'),JSON.stringify(result,null,2));
  if(exitMode==='Tray') {
    await requestClose();await modal.getByRole('button',{name:/最小化到托盘/}).click();await modal.waitFor({state:'hidden'});
    await page.evaluate(async()=>(await import('/tauri-api/core.js')).invoke('desktop_smoke_tray_quit')).catch(error=>{if(!/closed|Target.*destroyed/i.test(String(error)))throw error;});
  } else {
    await requestClose();await modal.getByRole('button',{name:/退出应用/}).click().catch(error=>{if(!/closed|Target.*destroyed/i.test(String(error)))throw error;});
  }
  console.log(JSON.stringify(result));
  process.exit(0);
} catch(error) {
  await fs.writeFile(path.join(output,'ui-result.json'),JSON.stringify({passed:false,error:String(error),stack:error.stack,diagnostic,url:page?.url(),body:page?await page.locator('body').innerText().catch(()=>null):null},null,2));
  console.error(String(error));process.exit(1);
}
'@
    Push-Location $desktop
    try {& node --input-type=module -e $probe "$debugPort" "$run" "$base" "$ExitMode" 2>&1 | Tee-Object -LiteralPath (Join-Path $run 'ui-probe.log'); if ($LASTEXITCODE -ne 0) {throw 'Native UI probe failed'}} finally {Pop-Location}
    while (-not $process.HasExited) {
        Update-OwnedProcesses
        if ([DateTime]::UtcNow -gt $deadline.AddSeconds(80)) {throw 'Application shutdown timed out'}
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
    $result=[ordered]@{passed=$true;executable=$Executable;resolver='native';exitMode=$ExitMode;ownedChildNames=@($owned.Values.Name|Sort-Object -Unique);ownedProcessesRemaining=$remaining.Count;apiListenerClosed=$true;webviewListenerClosed=$true;shutdown=$shutdown;ui=$ui;profile=$profile}
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $run 'result.json') -Encoding utf8
    $result | ConvertTo-Json -Depth 8
} finally {
    if (-not $process.HasExited) {$process.Kill($true);$process.WaitForExit(10000)|Out-Null}
}
