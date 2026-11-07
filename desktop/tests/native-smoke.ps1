# Isolated full-Tauri lifecycle smoke. Never starts an installed executable or accesses its profile.
param([string]$Executable = (Join-Path $PSScriptRoot '..\src-tauri\target\release\streamcap-tauri.exe'),[ValidateSet('Choice','Tray')][string]$ExitMode='Choice')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'process-identity.ps1')
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
$profile = Join-Path $run '数据 data 中文'
[IO.Directory]::CreateDirectory($profile) | Out-Null
$working = Join-Path $run '启动目录 中文 # 空格'
[IO.Directory]::CreateDirectory($working) | Out-Null
$sourceExecutable = $Executable
$candidateDirectory = [IO.Path]::GetFullPath((Join-Path (Split-Path -Parent $Executable) ('迁移目录 中文 # ' + [Guid]::NewGuid().ToString('N'))))
if (-not $candidateDirectory.StartsWith($targetRoot,[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $candidateDirectory)) {throw 'Use a fresh relocation directory inside the project target'}
[IO.Directory]::CreateDirectory($candidateDirectory) | Out-Null
$Executable = Join-Path $candidateDirectory 'StreamCap.exe'
[IO.File]::Copy($sourceExecutable,$Executable,$false)
$candidateHash = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash
if ($candidateHash -ne (Get-FileHash -LiteralPath $sourceExecutable -Algorithm SHA256).Hash) {throw 'Relocated executable differs from the candidate'}
$temp = Join-Path $run 'tmp'; [IO.Directory]::CreateDirectory($temp) | Out-Null
$testFfmpeg=if($env:STREAMCAP_TEST_FFMPEG){$env:STREAMCAP_TEST_FFMPEG}else{(Get-Command ffmpeg -ErrorAction Stop).Source}
$mediaDirectory=Join-Path $profile 'downloads';[IO.Directory]::CreateDirectory($mediaDirectory)|Out-Null
& $testFfmpeg -v error -nostdin -n -f lavfi -i 'testsrc2=size=160x120:rate=10' -f lavfi -i 'sine=frequency=440:sample_rate=48000' -t 60 -c:v libx264 -preset ultrafast -tune zerolatency -g 10 -pix_fmt yuv420p -c:a aac -f mpegts (Join-Path $run 'native-preview.ts')
if($LASTEXITCODE -ne 0){throw 'Synthetic native preview fixture failed'}
$stdout = Join-Path $run 'app.stdout.log'; $stderr = Join-Path $run 'app.stderr.log'
$debugPort = 0
$webviewDataDirectory = Join-Path $profile 'webview/EBWebView'
$debugPortFile = Join-Path $webviewDataDirectory 'DevToolsActivePort'
$arguments = @('--data-dir',$profile,'--api-port','0','--smoke-seconds','120')
$environment = @{TEMP=$temp;TMP=$temp;PATH=''}
# ProcessStartInfo preserves an actually empty child PATH on Windows.
$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName=$Executable;$startInfo.WorkingDirectory=$working
$startInfo.UseShellExecute=$false;$startInfo.CreateNoWindow=$true
$startInfo.RedirectStandardOutput=$true;$startInfo.RedirectStandardError=$true
foreach ($argument in $arguments) {$startInfo.ArgumentList.Add($argument)}
foreach ($name in @('WEBVIEW2_USER_DATA_FOLDER','WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS')) {$startInfo.Environment.Remove($name)|Out-Null}
foreach ($entry in $environment.GetEnumerator()) {$startInfo.Environment[$entry.Key]=[string]$entry.Value}
$process = [Diagnostics.Process]::Start($startInfo)
$stdoutRead = $process.StandardOutput.ReadToEndAsync()
$stderrRead = $process.StandardError.ReadToEndAsync()
$owned = @{}
$probeProcess = $null
$rootIdentity = $null
$webviewConfiguration = @()
function Complete-OwnedProcessMetadata([object[]]$Snapshot,[hashtable]$Owned) {
    foreach ($current in $Snapshot) {
        $saved=$Owned[[int]$current.ProcessId]
        if (-not (Test-ProcessInstance $saved $current)) {continue}
        foreach ($field in @('ExecutablePath','CommandLine')) {
            if (-not $saved.$field -and $current.$field) {$saved.$field=$current.$field}
        }
        if ($saved.ExecutablePath -and -not $saved.PSObject.Properties['ExecutablePathSource']) {
            $saved | Add-Member -NotePropertyName ExecutablePathSource -NotePropertyValue 'CIM'
        }
    }
}
function Update-OwnedProcesses {
    $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CreationDate,CommandLine)
    Add-OwnedProcessSnapshot $all $rootIdentity $owned
    Complete-OwnedProcessMetadata $all $owned
    foreach ($current in @($all | Where-Object {$_.Name -eq 'ffmpeg.exe'})) {
        $saved=$owned[[int]$current.ProcessId]
        if (-not (Test-ProcessInstance $saved $current) -or $saved.ExecutablePath) {continue}
        $nativeProcess=$null
        try {
            $nativeProcess=Get-Process -Id ([int]$current.ProcessId) -ErrorAction Stop
            # CIM timestamps have microsecond precision. Never accept a recycled PID.
            if ([Math]::Abs(($nativeProcess.StartTime.ToUniversalTime()-$current.CreationDate.ToUniversalTime()).Ticks) -ge 10) {continue}
            $actualPath=$nativeProcess.Path
            if ($actualPath) {
                $saved.ExecutablePath=$actualPath
                $saved | Add-Member -NotePropertyName ExecutablePathSource -NotePropertyValue 'process-module' -Force
            }
        } catch {
            # A process can exit during sampling. Keep it unverified until a later observation.
        } finally {
            if ($null -ne $nativeProcess) {$nativeProcess.Dispose()}
        }
    }
    if (@($owned.Values | Where-Object {$_.Name -match '^(python|pythonw|node|streamcap-resolver)(\.exe)?$'}).Count -gt 0) {throw 'External interpreter or resolver was started'}
}
function Get-OwnedWebViewConfiguration {
    foreach ($browser in @($owned.Values | Where-Object {$_.Name -eq 'msedgewebview2.exe' -and $_.ParentProcessId -eq $rootIdentity.ProcessId})) {
        $directoryMatch=[regex]::Match([string]$browser.CommandLine,'(?:^|\s)--user-data-dir=(?:"(?<quoted>[^"]+)"|(?<plain>\S+))')
        $directory=if($directoryMatch.Success){$directoryMatch.Groups['quoted'].Value+$directoryMatch.Groups['plain'].Value}else{$null}
        [pscustomobject]@{processId=$browser.ProcessId;executable=$browser.ExecutablePath;dataDirectory=$directory;debugFlags=@([regex]::Matches([string]$browser.CommandLine,'--remote-debugging-(?:port|address)=[^\s"]+') | ForEach-Object Value)}
    }
}
try {
    $rootIdentity = Get-CimInstance Win32_Process -Filter "ProcessId=$($process.Id)" | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CreationDate
    if ($process.HasExited -or $null -eq $rootIdentity.CreationDate -or
        [Math]::Abs(($rootIdentity.CreationDate.ToUniversalTime() - $process.StartTime.ToUniversalTime()).TotalSeconds) -gt 1) {
        throw 'Cannot establish the candidate process identity'
    }
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
    $manifest = Get-Content -LiteralPath (Join-Path $desktop 'ffmpeg-bundle.json') -Raw | ConvertFrom-Json
    $toolStatus = Invoke-RestMethod -Uri "$base/api/tools/status" -TimeoutSec 3
    if (-not $toolStatus.bundled -or -not $toolStatus.ffmpegReady -or -not $toolStatus.ffprobeReady -or $toolStatus.version -ne $manifest.version) {throw 'Embedded recording tools are not ready without PATH'}
    $runtimeTools = Join-Path $profile ('runtime/ffmpeg/' + $manifest.archiveSha256)
    foreach ($spec in $manifest.files) {
        $extracted = Get-Item -LiteralPath (Join-Path $runtimeTools $spec.name) -Force
        if ($extracted.PSIsContainer -or ($extracted.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $extracted.Length -ne $spec.size -or
            (Get-FileHash -LiteralPath $extracted.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -cne $spec.sha256) {throw "Embedded file verification failed: $($spec.name)"}
    }
    $debugOwner = $null
    for ($i=0;$i -lt 40;$i++) {
        Update-OwnedProcesses
        $webviewConfiguration=@(Get-OwnedWebViewConfiguration)
        if (@($webviewConfiguration | Where-Object {-not $_.dataDirectory -or -not [string]::Equals([IO.Path]::GetFullPath($_.dataDirectory),[IO.Path]::GetFullPath($webviewDataDirectory),[StringComparison]::OrdinalIgnoreCase)}).Count) {
            throw 'WebView2 is not using this isolated profile; refusing to attach'
        }
        if (Test-Path -LiteralPath $debugPortFile) {
            $portText=Get-Content -LiteralPath $debugPortFile -TotalCount 1 -ErrorAction SilentlyContinue
            $publishedPort=0
            if ([int]::TryParse([string]$portText,[ref]$publishedPort) -and $publishedPort -gt 0 -and $publishedPort -le 65535) {$debugPort=$publishedPort}
        }
        if ($debugPort -gt 0) {$debugOwner=Get-NetTCPConnection -State Listen -LocalPort $debugPort -ErrorAction SilentlyContinue | Select-Object -First 1}
        if ($debugOwner -and $owned.ContainsKey([int]$debugOwner.OwningProcess)) {break}
        Start-Sleep -Milliseconds 100
    }
    if (-not $webviewConfiguration.Count) {throw 'The isolated WebView2 browser configuration is unavailable'}
    if ($debugPort -eq 0) {throw 'The isolated WebView2 profile did not publish a valid debug port'}
    if (-not $debugOwner -or -not $owned.ContainsKey([int]$debugOwner.OwningProcess)) {throw 'Debug listener is not owned by this isolated application; refusing to attach'}
    $debugIdentity=Get-OwnedWebViewListener $rootIdentity $debugPort
    $probe = @'
import { chromium } from 'playwright';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import { createServer } from 'node:http';
import { setTimeout as sleep } from 'node:timers/promises';
const [port,output,base,exitMode,profile]=process.argv.slice(2);
const deleteOriginal=exitMode==='Tray';
let page;let kuaishouSessionValidation=null;const diagnostic=[];
const eventually=async(read,predicate,label,timeout=15000)=>{
  const deadline=Date.now()+timeout;
  while(Date.now()<deadline){const value=await read();if(predicate(value))return value;await sleep(100);}
  throw Error('Timed out waiting for '+label);
};
const openCardMenu=async card=>{
  const menu=card.locator('details.card-menu');
  assert.equal(await menu.count(),1);
  assert.equal(await menu.getAttribute('name'),'streamcap-task-menu');
  const trigger=menu.getByRole('button',{name:'更多操作',exact:true});
  if(!await menu.evaluate(node=>node.open))await trigger.click();
  await eventually(()=>menu.evaluate(node=>node.open),Boolean,'native card menu opens');
  await menu.locator('.card-menu-panel').waitFor({state:'visible'});
  return menu;
};
try {
  const browser=await chromium.connectOverCDP('http://127.0.0.1:'+port);
  page=await eventually(()=>browser.contexts().flatMap(context=>context.pages()).find(page=>page.url().includes('tauri.localhost')),Boolean,'isolated native app webview');
  const targetSession=await browser.newBrowserCDPSession();
  const windowTargets=targets=>targets.filter(target=>['page','webview'].includes(target.type));
  const initialTargets=windowTargets((await targetSession.send('Target.getTargets')).targetInfos);
  assert.equal(initialTargets.length,1,'the isolated app must start with only its main webview');
  assert.ok(initialTargets[0].url.includes('tauri.localhost'));
  const mainTargetId=initialTargets[0].targetId,unexpectedWindows=new Set();
  targetSession.on('Target.targetCreated',({targetInfo})=>{
    if(windowTargets([targetInfo]).length&&targetInfo.targetId!==mainTargetId)unexpectedWindows.add(targetInfo.targetId);
  });
  await targetSession.send('Target.setDiscoverTargets',{discover:true});
  const assertNoVerificationWindow=async()=>{
    const targets=windowTargets((await targetSession.send('Target.getTargets')).targetInfos);
    assert.deepEqual(targets.map(target=>target.targetId),[mainTargetId],'no verification child webview may exist');
    assert.equal(unexpectedWindows.size,0,'no transient or hidden verification child webview may be created');
  };
  page.setDefaultTimeout(12000);
  const errors=[];page.on('pageerror',error=>{errors.push(String(error));diagnostic.push(String(error));});
  page.on('console',message=>{if(message.type()==='error')diagnostic.push(message.text());});
  page.on('requestfailed',request=>diagnostic.push(request.url()+': '+request.failure()?.errorText));
  const assetFailures=[];
  page.on('response',response=>{
    if(new URL(response.url()).hostname==='tauri.localhost'&&['script','stylesheet','image','font'].includes(response.request().resourceType())&&response.status()>=400)assetFailures.push(response.url()+': '+response.status());
  });
  const navigation=await page.reload({waitUntil:'domcontentloaded'});
  const documentHeaders=await navigation.allHeaders();
  await page.waitForFunction(()=>document.body?.innerText.includes('StreamCap')&&window.__STREAMCAP_NATIVE_SMOKE__?.requests.some(r=>r.path==='/api/status'&&r.status===200),{},{timeout:15000});
  await page.waitForFunction(()=>{const image=document.querySelector('.sidebar-brand img');return image?.complete&&image.naturalWidth>0&&image.naturalHeight>0;});
  const logo=await page.locator('.sidebar-brand img').evaluate(image=>({source:image.currentSrc,width:image.naturalWidth,height:image.naturalHeight}));
  assert.equal(new URL(logo.source).origin,new URL(page.url()).origin,'brand image must load from the executable assets');
  assert.ok(await page.locator('.sidebar svg').count()>0,'navigation icons must render');
  assert.ok(await page.evaluate(()=>document.styleSheets.length)>0,'bundled styles must load');
  const native=()=>page.evaluate(async()=>(await import('/tauri-api/core.js')).invoke('desktop_ready'));
  assert.equal((await native()).decorated,false,'the web UI and the window must share one title bar');
  assert.equal((await native()).trayAvailable,true);
  await page.getByRole('button',{name:'最大化窗口',exact:true}).click();
  await eventually(native,state=>state.maximized,'native maximize');
  await page.getByRole('button',{name:'还原窗口',exact:true}).click();
  await eventually(native,state=>!state.maximized,'native restore');
  const shell=await page.locator('.app-shell').boundingBox();assert.equal(shell.x,0);assert.equal(shell.y,0);
  assert.equal(await page.locator('.sidebar').evaluate(el=>getComputedStyle(el).borderRadius),'0px');
  await page.screenshot({path:path.join(output,'native-window.png'),fullPage:true,animations:'disabled'});
  // Rejected IPC only: do not create platform tasks, sessions, or verification pages.
  const removedActions=['status','open','smoke-open','smoke-open-background','complete','cancel','smoke-close'];
  for(const action of removedActions){
    const rejection=await page.evaluate(async action=>{
      const {invoke}=await import('/tauri-api/core.js');
      try{await invoke('desktop_kuaishou_verification',{action,recId:'removed-native-command'});return '';}
      catch(error){return String(error);}
    },action);
    assert.match(rejection,/desktop_kuaishou_verification.*(?:not found|not allowed)|Command not found/i,action+' must be rejected before entering a command handler');
    await assertNoVerificationWindow();
  }
  assert.equal((await native()).closing,false);
  const kuaishouInvoke=args=>page.evaluate(async args=>(await import('/tauri-api/core.js')).invoke('desktop_kuaishou_session',args),args);
  const deniedKuaishou=await kuaishouInvoke({action:'open'}).then(()=>'',error=>String(error));
  assert.match(deniedKuaishou,/隔离验收/,'smoke mode must reject real platform navigation');
  const badFixture=await kuaishouInvoke({action:'smoke-probe',fixtureUrl:'https://live.kuaishou.com/'}).then(()=>'',error=>String(error));
  assert.match(badFixture,/本机|夹具/,'smoke callbacks cannot become an external request proxy');
  assert.equal((await kuaishouInvoke({action:'smoke-status'})).workerExists,false);
  let repeatedKuaishouRequests=0;
  const kuaishouFixture=createServer((request,response)=>{
    const url=new URL(request.url,'http://127.0.0.1');
    if(url.pathname!=='/kuaishou-fixture'){response.writeHead(404);response.end();return;}
    let sample=url.searchParams.get('case');
    if(sample==='repeated')sample=++repeatedKuaishouRequests===1?'live':'offline';
    const data={result:sample==='offline'?671:sample==='captcha'?400002:1,
      author:{id:sample==='wrong'?'other':'fixture',name:'Fixture'},
      liveStream:{playUrls:[{url:'https://media.invalid/native-fixture.flv'}]}};
    response.writeHead(200,{'Content-Type':'application/json','Cache-Control':'no-store'});
    response.end(JSON.stringify({data}));
  });
  await new Promise(resolve=>kuaishouFixture.listen(0,'127.0.0.1',resolve));
  try{
    const fixtureUrl='http://127.0.0.1:'+kuaishouFixture.address().port+'/kuaishou-fixture';
    const live=await kuaishouInvoke({action:'smoke-probe',fixtureUrl:fixtureUrl+'?case=live'});
    assert.equal(live.isLive,true);assert.equal(live.anchorMatches,true);assert.equal(live.hasSource,true);
    const offline=await kuaishouInvoke({action:'smoke-probe',fixtureUrl:fixtureUrl+'?case=offline'});
    assert.equal(offline.isLive,false);assert.equal(offline.hasSource,false);
    const captcha=await kuaishouInvoke({action:'smoke-probe',fixtureUrl:fixtureUrl+'?case=captcha'});
    assert.equal(captcha.accessState,'captcha');assert.equal(captcha.hasSource,false);
    const wrong=await kuaishouInvoke({action:'smoke-probe',fixtureUrl:fixtureUrl+'?case=wrong'});
    assert.equal(wrong.hasSource,false);assert.match(wrong.error,/目标主播/);
    const repeatedUrl=fixtureUrl+'?case=repeated';
    assert.equal((await kuaishouInvoke({action:'smoke-probe',fixtureUrl:repeatedUrl})).isLive,true);
    assert.equal((await kuaishouInvoke({action:'smoke-probe',fixtureUrl:repeatedUrl})).isLive,false,'same-URL recheck must read the new document');
    assert.equal(repeatedKuaishouRequests,2);
    assert.equal((await kuaishouInvoke({action:'smoke-status'})).portalExists,false,'background checks must not open a login window');
    const shared=await kuaishouInvoke({action:'smoke-session',fixtureUrl:fixtureUrl+'?case=live'});
    kuaishouSessionValidation=shared;
    assert.equal(shared.sharedCookie,true,'portal and worker must share their isolated website session');
    assert.equal(shared.websiteChangePreserved,true,'unchanged saved input must not overwrite website cookie updates');
    assert.equal(shared.sourceCookieReplaced,true,'changed saved input must replace the previous native domain cookie');
    assert.equal(shared.sourceCookieCleared,true,'clearing saved input must remove both host and domain cookies');
    assert.equal(shared.remoteIpcBlocked,true,'the website window must not invoke main-window commands');
  }finally{
    await kuaishouInvoke({action:'smoke-close'});
    await eventually(()=>kuaishouInvoke({action:'smoke-status'}),value=>!value.workerExists&&!value.portalExists,'Kuaishou helper window cleanup');
    kuaishouFixture.closeAllConnections();
    await new Promise(resolve=>kuaishouFixture.close(resolve));
  }
  await assertNoVerificationWindow();
  // Actual WebView2 decoding and automatic conversion; only generated, loopback media is used.
  const fixture=await fs.readFile(path.join(output,'native-preview.ts'));
  const mediaSource=createServer((_req,res)=>{res.writeHead(200,{'Content-Type':'video/mp2t','Content-Length':fixture.length});res.end(fixture);});
  await new Promise(resolve=>mediaSource.listen(0,'127.0.0.1',resolve));
  const mediaUrl='http://127.0.0.1:'+mediaSource.address().port+'/live.ts';
  const relativeRoot='录制 captures';
  await fs.mkdir(path.join(profile,relativeRoot));
  const configured=await fetch(base+'/api/settings',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({userConfig:{live_save_path:relativeRoot,convert_to_mp4:true,delete_original:deleteOriginal,video_format:'TS',folder_name_platform:false,folder_name_author:false,folder_name_time:false,segmented_recording_enabled:false}})});assert.equal(configured.status,200);
  const created=await fetch(base+'/api/recordings',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({url:mediaUrl,streamerName:'原生媒体验收'})});assert.equal(created.status,200);const mediaTask=(await created.json()).created[0];
  await page.getByRole('navigation',{name:'主导航'}).getByRole('link',{name:'录制任务',exact:true}).click();
  const mediaCard=page.locator('article[data-rec-id="'+mediaTask.recId+'"]');
  await mediaCard.getByRole('button',{name:'单次录制',exact:true}).click();
  await mediaCard.getByRole('button',{name:'停止录制',exact:true}).waitFor();
  const firstMenu=await openCardMenu(mediaCard);
  const menuTrigger=firstMenu.getByRole('button',{name:'更多操作',exact:true});
  await menuTrigger.press('Escape');
  await eventually(()=>firstMenu.evaluate(node=>node.open),value=>!value,'native Escape closes task menu');
  assert.equal(await menuTrigger.evaluate(node=>node===document.activeElement),true);
  await mediaCard.getByRole('button',{name:'预览录制文件',exact:true}).click();const previewModal=page.getByRole('dialog',{name:'录制预览',exact:true});
  await page.waitForFunction(()=>{const video=document.querySelector('video');return video?.videoWidth===160&&video.currentTime>0.5;},null,{timeout:20000});
  const liveSlider=previewModal.getByRole('slider',{name:'播放进度',exact:true});await liveSlider.waitFor();
  await eventually(()=>liveSlider.getAttribute('max'),value=>Number(value)>5,'growing recording timeline',18000);
  for(const fraction of [.75,.2]){
    const max=Number(await liveSlider.getAttribute('max'));const box=await liveSlider.boundingBox();
    await page.mouse.move(box.x+box.width*.5,box.y+box.height/2);await page.mouse.down();await page.mouse.move(box.x+6+(box.width-12)*fraction,box.y+box.height/2,{steps:6});
    await page.waitForFunction(({target})=>{const video=document.querySelector('video');return video?.readyState>=2&&!video.seeking&&video.dataset.scrubbing==='true'&&Math.abs(Number(video.dataset.timelinePosition)-target)<1;},{target:max*fraction},{timeout:15000});
    await page.mouse.up();
    await page.waitForFunction(()=>!document.querySelector('video').paused);
  }
  await previewModal.getByRole('button',{name:'回到最新',exact:true}).click();
  await page.waitForFunction(()=>{const video=document.querySelector('video');return video?.readyState>=2&&Number(video.dataset.timelinePosition)>2;},null,{timeout:15000});
  assert.equal((await(await fetch(base+'/api/recordings')).json()).find(r=>r.recId===mediaTask.recId).isRecording,true,'seeking must not stop recording');
  await page.screenshot({path:path.join(output,'native-ts-live-preview.png'),fullPage:true,animations:'disabled'});
  await page.locator('video').evaluate(v=>{window.nativeMiniVideo=v;window.nativeMiniTime=Number(v.dataset.timelinePosition);});
  await previewModal.getByRole('button',{name:'最小化预览',exact:true}).click();
  await page.getByRole('navigation',{name:'主导航'}).getByRole('link',{name:'偏好设置',exact:true}).click();
  await page.getByRole('button',{name:'网络与检测',exact:true}).click();
  await page.getByRole('textbox',{name:/检测间隔/}).focus();
  assert.equal(await previewModal.evaluate(node=>node.matches(':modal')),false);
  assert.equal(await page.locator('video').evaluate(v=>v===window.nativeMiniVideo),true);
  await page.waitForFunction(()=>{const v=document.querySelector('video');return v===window.nativeMiniVideo&&!v.paused&&Number(v.dataset.timelinePosition)>window.nativeMiniTime+.1;});
  await page.screenshot({path:path.join(output,'native-mini-preview-settings.png'),fullPage:true,animations:'disabled'});
  await previewModal.getByRole('button',{name:'恢复预览',exact:true}).click();
  assert.equal(await previewModal.evaluate(node=>node.matches(':modal')),true);
  assert.equal(await page.locator('video').evaluate(v=>v===window.nativeMiniVideo),true);
  assert.equal(await previewModal.locator('.directory-note,.modal-actions').count(),0);
  await previewModal.getByRole('button',{name:'关闭对话框',exact:true}).click();
  await eventually(()=>page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),count=>count===0,'disposed player');
  await page.getByRole('navigation',{name:'主导航'}).getByRole('link',{name:'录制任务',exact:true}).click();
  await mediaCard.getByRole('button',{name:'停止录制',exact:true}).click();
  const completedJob=await eventually(async()=>{const response=await fetch(base+'/api/media/jobs');assert.equal(response.status,200);return (await response.json()).jobs.find(j=>j.taskId===mediaTask.recId&&j.state==='complete');},Boolean,'verified media job',25000);
  assert.equal(completedJob.deleteOriginal,deleteOriginal);assert.equal(completedJob.sourceRemoved,deleteOriginal);
  await mediaCard.getByRole('button',{name:'预览录制文件',exact:true}).click();
  await previewModal.getByRole('listitem').filter({hasText:/\.mp4/}).getByRole('button').click();
  await page.waitForFunction(()=>{const video=document.querySelector('video');return video?.videoWidth===160&&video.currentTime>0.5;},null,{timeout:15000});
  await page.screenshot({path:path.join(output,'native-mp4-preview.png'),fullPage:true,animations:'disabled'});
  await previewModal.getByRole('button',{name:'关闭对话框',exact:true}).click();
  await eventually(()=>page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),count=>count===0,'disposed player');
  const mediaFiles=await(await fetch(base+'/api/recordings/'+mediaTask.recId+'/files')).json();
  assert.equal(mediaFiles.files.some(f=>f.name===path.basename(completedJob.source)),!deleteOriginal);assert.ok(mediaFiles.files.some(f=>f.name===path.basename(completedJob.output)));
  const outputRoot=path.join(profile,relativeRoot);
  const sourceExists=await fs.stat(path.join(outputRoot,completedJob.source)).then(()=>true,error=>{if(error.code==='ENOENT')return false;throw error;});
  assert.equal(sourceExists,!deleteOriginal);assert.ok((await fs.stat(path.join(outputRoot,completedJob.output))).size>0);
  await new Promise(resolve=>{mediaSource.closeAllConnections();mediaSource.close(resolve);});
  const mediaValidation={miniPreviewAcrossRoutes:true,tsLiveDecoded:true,liveSeekingForwardAndBackward:true,returnToLatest:true,mp4Decoded:true,originalTsKept:!deleteOriginal,originalTsRemoved:deleteOriginal,previewDisposed:true};
  const modal=page.getByRole('dialog',{name:'关闭 StreamCap',exact:true});
  const requestClose=async()=>{await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await modal.waitFor({state:'visible'});};
  await requestClose();assert.equal((await native()).closePending,true);
  await page.reload({waitUntil:'domcontentloaded'});await modal.waitFor({state:'visible'});
  await page.screenshot({path:path.join(output,'native-close-options.png'),fullPage:true,animations:'disabled'});
  await modal.getByRole('button',{name:'取消',exact:true}).click();await modal.waitFor({state:'hidden'});
  assert.equal((await native()).closing,false);assert.equal((await native()).closePending,false);
  await requestClose();await page.keyboard.press('Escape');await modal.waitFor({state:'hidden'});
  assert.equal((await native()).closePending,false);
  await requestClose();await modal.getByRole('switch',{name:'记住我的选择'}).click();assert.equal(await modal.getByRole('switch',{name:'记住我的选择'}).getAttribute('aria-checked'),'true');await modal.getByRole('button',{name:/最小化到托盘/}).click();await modal.waitFor({state:'hidden'});
  const tray=await native();assert.equal(tray.visible,false);assert.equal(tray.closing,false);assert.equal(tray.closePending,false);
  const status=await (await fetch(base+'/api/status')).json();assert.equal(status.ok,true);assert.equal(status.resolverReady,true);
  const settings=await (await fetch(base+'/api/settings')).json();assert.equal(settings.userConfig.close_action,'tray');
  await page.evaluate(async()=>(await import('/tauri-api/core.js')).invoke('desktop_window_action',{action:'close'}));
  await eventually(native,state=>!state.closePending,'remembered native close');
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
  await assertNoVerificationWindow();
  assert.deepEqual(assetFailures,[],'relocating the executable must not break local resources');
  const relocationValidation={logo,stylesLoaded:true,iconsRendered:true,relativeRecordingRoot:outputRoot,assetFailures};
  const nativeSurfaceValidation={removedCommandRejected:true,rejectedActions:removedActions,noVerificationSubwindow:true};
  const result={passed:true,security,mediaValidation,nativeSurfaceValidation,kuaishouSessionValidation,relocationValidation,title:await page.title(),checks:['frameless-shell','maximize-restore','flush-layout','close-cancel','escape-cancel','tray-keeps-backend','remember-tray','close-preference-change',exitMode==='Tray'?'tray-menu-exit':'explicit-dialog-exit'],transport:stats,exitMode};
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
  const player = await page?.evaluate(() => {
    const video=document.querySelector('video'), slider=document.querySelector('.seek-range');
    return video ? {time:video.currentTime,duration:video.duration,paused:video.paused,seeking:video.seeking,
      readyState:video.readyState,networkState:video.networkState,error:video.error?.code??null,
      buffered:Array.from({length:video.buffered.length},(_,i)=>[video.buffered.start(i),video.buffered.end(i)]),
      data:{...video.dataset},slider:slider?{value:slider.value,max:slider.max,rect:slider.getBoundingClientRect().toJSON()}:null,
      state:document.querySelector('.media-preview')?.dataset.playerState,visibility:document.visibilityState} : null;
  }).catch(()=>null);
  const transport = await page?.evaluate(()=>window.__STREAMCAP_NATIVE_SMOKE__).catch(()=>null);
  await page?.screenshot({path:path.join(output,'failure.png'),animations:'disabled'}).catch(()=>{});
  await fs.writeFile(path.join(output,'ui-result.json'),JSON.stringify({passed:false,error:String(error),stack:error.stack,diagnostic,kuaishouSessionValidation,player,transport,url:page?.url(),body:page?await page.locator('body').innerText().catch(()=>null):null},null,2));
  console.error(String(error));process.exit(1);
}
'@
    $probePath=Join-Path $run 'native-probe.mjs'
    [IO.File]::WriteAllText($probePath,$probe,[Text.UTF8Encoding]::new($false))
    $probeArguments=@(('"'+$probePath+'"'),"$debugPort",('"'+$run+'"'),"$base","$ExitMode",('"'+$profile+'"'))
    $probeProcess=Start-Process -FilePath (Get-Command node -ErrorAction Stop).Source -ArgumentList $probeArguments -WorkingDirectory $desktop -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $run 'ui-probe.log') -RedirectStandardError (Join-Path $run 'ui-probe.error.log')
    $probeDeadline=[DateTime]::UtcNow.AddSeconds(110)
    while(-not $probeProcess.HasExited){
        Update-OwnedProcesses
        if([DateTime]::UtcNow -gt $probeDeadline){throw 'Native UI probe timed out'}
        Start-Sleep -Milliseconds 200
    }
    $probeProcess.WaitForExit()
    Update-OwnedProcesses
    Get-Content -LiteralPath (Join-Path $run 'ui-probe.log')
    if($probeProcess.ExitCode -ne 0){Get-Content -LiteralPath (Join-Path $run 'ui-probe.error.log');throw 'Native UI probe failed'}
    while (-not $process.HasExited) {
        Update-OwnedProcesses
        if ([DateTime]::UtcNow -gt $deadline.AddSeconds(80)) {throw 'Application shutdown timed out'}
        Start-Sleep -Milliseconds 250
    }
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {throw "Application exit code=$($process.ExitCode)"}
    $shutdown = Get-Content -LiteralPath (Join-Path $profile 'shutdown.json') -Raw | ConvertFrom-Json
    if (-not $shutdown.shutdownComplete -or $shutdown.activeRecordings -ne 0 -or $shutdown.pendingMediaJobs -ne 0 -or $shutdown.activePreviews -ne 0) {throw 'Ordered shutdown incomplete'}
    $remaining = @()
    for ($i=0;$i -lt 30;$i++) {
        $current = @{}
        foreach ($p in @(Get-CimInstance Win32_Process | Select-Object ProcessId,CreationDate)) {$current[[int]$p.ProcessId]=$p}
        $remaining=@($owned.Keys | Where-Object {Test-ProcessInstance $owned[$_] $current[$_]})
        if ($remaining.Count -eq 0) {break}
        Start-Sleep -Milliseconds 200
    }
    if ($remaining.Count -gt 0) {throw "Owned child processes remain: $($remaining -join ',')"}
    if (@($owned.Values | Where-Object {$_.Name -eq 'msedgewebview2.exe'}).Count -eq 0) {throw 'No system WebView2 process was observed'}
    $listening = $false
    try {Invoke-RestMethod -Uri "$base/api/status" -TimeoutSec 1 | Out-Null; $listening=$true} catch {}
    if ($listening) {throw 'HTTP listener remained after application exit'}
    if (Get-NetTCPConnection -State Listen -LocalPort $debugPort -ErrorAction SilentlyContinue) {throw 'WebView2 debug listener remained after application exit'}
    $recorders=@($owned.Values | Where-Object {$_.Name -eq 'ffmpeg.exe'})
    $expectedRecorder=[IO.Path]::GetFullPath((Join-Path $runtimeTools 'ffmpeg.exe'))
    [ordered]@{expectedExecutable=$expectedRecorder;observations=@($recorders|Select-Object ProcessId,ParentProcessId,Name,CreationDate,ExecutablePath,ExecutablePathSource)} |
        ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $run 'recorder-paths.log') -Encoding utf8
    if (-not $recorders.Count -or @($recorders | Where-Object {-not $_.ExecutablePath -or -not [string]::Equals([IO.Path]::GetFullPath($_.ExecutablePath),$expectedRecorder,[StringComparison]::OrdinalIgnoreCase)}).Count) {throw 'Recording must use the extracted executable only'}
    if (Test-Path -LiteralPath (Join-Path $working '录制 captures')) {throw 'Recording incorrectly used the startup working directory'}
    if (@(Get-ChildItem -LiteralPath $candidateDirectory -File).Count -ne 1 -or @(Get-ChildItem -LiteralPath $candidateDirectory -Directory).Count) {throw 'Runtime assets must not depend on sidecar files next to the relocated EXE'}
    $ui=Get-Content -LiteralPath (Join-Path $run 'ui-result.json') -Raw | ConvertFrom-Json
    $result=[ordered]@{passed=$true;executable=$Executable;sourceExecutable=$sourceExecutable;sha256=$candidateHash.ToLowerInvariant();startupDirectory=$working;emptyPath=$true;bundledToolStatus=$toolStatus;resolver='native';webviewConfiguration=$webviewConfiguration;debugListenerOwnership=$debugIdentity;exitMode=$ExitMode;ownedChildNames=@($owned.Values.Name|Sort-Object -Unique);ownedProcessIdentities=@($owned.Values|Select-Object ProcessId,ParentProcessId,Name,CreationDate,ExecutablePath,ExecutablePathSource);ownedProcessesRemaining=$remaining.Count;apiListenerClosed=$true;webviewListenerClosed=$true;shutdown=$shutdown;ui=$ui;profile=$profile}
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $run 'result.json') -Encoding utf8
    $result | ConvertTo-Json -Depth 8
} catch {
    $failureMessage=$_.Exception.Message
    $connections=if($debugPort -gt 0){@(Get-NetTCPConnection -LocalPort $debugPort -ErrorAction SilentlyContinue | Select-Object LocalAddress,LocalPort,State,OwningProcess)}else{@()}
    [ordered]@{passed=$false;error=$failureMessage;executable=$Executable;root=$rootIdentity;webviewConfiguration=@(Get-OwnedWebViewConfiguration);debugPort=$debugPort;debugPortFilePresent=(Test-Path -LiteralPath $debugPortFile);connections=$connections;ownedProcesses=@($owned.Values|Select-Object ProcessId,ParentProcessId,Name,CreationDate,ExecutablePath,ExecutablePathSource)} |
        ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $run 'result.json') -Encoding utf8
    throw
} finally {
    if ($null -ne $probeProcess -and -not $probeProcess.HasExited) {$probeProcess.Kill($true);$probeProcess.WaitForExit(10000)|Out-Null}
    if (-not $process.HasExited) {$process.Kill($true);$process.WaitForExit(10000)|Out-Null}
    foreach ($capture in @(@($stdoutRead,$stdout),@($stderrRead,$stderr))) {
        if ($capture[0].Wait(5000)) {[IO.File]::WriteAllText($capture[1],$capture[0].GetAwaiter().GetResult(),[Text.UTF8Encoding]::new($false))}
    }
}
