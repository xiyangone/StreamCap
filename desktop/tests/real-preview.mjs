// Explicit real-media desktop regression. Never loads the user's monitoring profile.
// Requires an E-drive installed/candidate EXE and an explicitly named read-only media root.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright';

const options={};
for(let i=2;i<process.argv.length;i++){const arg=process.argv[i];if(arg==='--leave-open')options.leaveOpen=true;else{assert.ok(['--exe','--media-root','--records','--expected-sha256'].includes(arg),'Unknown option '+arg);options[arg.slice(2)]=process.argv[++i];}}
assert.ok(options.exe&&options['media-root']&&options['expected-sha256'],'--exe, --media-root and --expected-sha256 are required');
const exe=resolve(options.exe),mediaRoot=resolve(options['media-root']);
assert.equal(basename(exe).toLowerCase().endsWith('.exe'),true);
const desktop=resolve(dirname(fileURLToPath(import.meta.url)),'..');const allowed=join(desktop,'tests','artifacts');const artifactRoot=resolve(process.env.STREAMCAP_TEST_ARTIFACTS||allowed);
assert.ok(artifactRoot===allowed||artifactRoot.startsWith(allowed+sep));await mkdir(artifactRoot,{recursive:true});const output=await mkdtemp(join(artifactRoot,'real-preview-'));
const sha256=async path=>{const hash=createHash('sha256');for await(const chunk of createReadStream(path))hash.update(chunk);return hash.digest('hex').toUpperCase();};
const hash=await sha256(exe);assert.equal(hash,options['expected-sha256'].toUpperCase(),'Do not test an unexpected binary');
const inventory=async root=>{const items=[];for(const entry of await readdir(root,{withFileTypes:true})){if(entry.isSymbolicLink())continue;const path=join(root,entry.name);if(entry.isDirectory())items.push(...await inventory(path));else if(entry.isFile()){const s=await stat(path);items.push({path,size:s.size,mtimeMs:s.mtimeMs});}}return items.sort((a,b)=>a.path.localeCompare(b.path));};
const before=await inventory(mediaRoot);const selected=before.filter(f=>f.path.toLowerCase().endsWith('.mp4')).sort((a,b)=>b.mtimeMs-a.mtimeMs).slice(0,2);assert.ok(selected.length>0,'No actual MP4 recording available');
for(const file of selected)file.sha256=await sha256(file.path);
const reserve=async port=>{const server=createServer();await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(port,'127.0.0.1',resolve);});return server;};
const ownerCheck=await reserve(6059).catch(()=>{throw Error('Port 6059 is already occupied; refusing to interfere with another instance');});await new Promise(resolve=>ownerCheck.close(resolve));
const reservation=await reserve(0);const cdpPort=reservation.address().port;await new Promise(resolve=>reservation.close(resolve));
const profile=join(output,'data');await mkdir(join(profile,'config'),{recursive:true});
const temp=join(output,'tmp');await mkdir(temp);
const records=options.records?JSON.parse(await readFile(resolve(options.records),'utf8')):[];
const lifecycleId='isolated-lifecycle-regression';
const lifecycleName='隔离回归任务（不访问真实房间）';
const outsideHour=String((new Date().getHours()+12)%24).padStart(2,'0')+':00';
const isolatedRecord={rec_id:lifecycleId,url:'http://127.0.0.1:1/disabled-fixture.ts',streamer_name:lifecycleName,monitor_status:false,scheduled_recording:true,scheduled_start_time:outsideHour,monitor_hours:'1',enabled_message_push:false,only_notify_no_record:true,recording_dir:null};
await writeFile(join(profile,'config','recordings.json'),JSON.stringify([...records.map(record=>({...record,monitor_status:false,scheduled_recording:false,enabled_message_push:false})),isolatedRecord],null,2));
await writeFile(join(profile,'config','user_settings.json'),JSON.stringify({live_save_path:mediaRoot,language:'zh_CN',theme_mode:'light',theme_color:'blue',is_grid_view:false,loop_time_seconds:'4500',close_action:'exit',convert_to_mp4:false,delete_original:false,generate_time_subtitle_file:false,scheduled_shutdown_enabled:false,execute_custom_script:false,system_notification_enabled:false,stream_start_notification_enabled:false,stream_end_notification_enabled:false},null,2));
const report={passed:false,exe,sha256:hash,mediaRoot,profile,output,cdpPort,actualMedia:true,productionProfileLoaded:false,cases:[],requests:[],errors:[]};
const child=spawn(exe,['--data-dir',profile],{cwd:dirname(exe),env:{...process.env,TEMP:temp,TMP:temp,WEBVIEW2_USER_DATA_FOLDER:join(profile,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-port='+cdpPort+' --remote-debugging-address=127.0.0.1'},windowsHide:true,stdio:'ignore'});report.pid=child.pid;
let browser,page;
const wait=async (read,accept,label,timeout=20000)=>{const start=performance.now();let last;while(performance.now()-start<timeout){try{last=await read();if(accept(last))return last;}catch(error){last=error.message;}if(child.exitCode!==null)throw Error('Native EXE exited: '+child.exitCode);await delay(80);}throw Error(label+': '+JSON.stringify(last));};
const get=async path=>{const res=await fetch('http://127.0.0.1:6059'+path,{signal:AbortSignal.timeout(3000)});assert.equal(res.ok,true);return res.json();};
const finish=async()=>{
  if(page&&!page.isClosed()){
    const dialog=page.getByRole('dialog');if(await dialog.count())await page.keyboard.press('Escape');
    await page.getByRole('button',{name:'关闭窗口',exact:true}).click().catch(()=>{});
  }
  await wait(()=>child.exitCode,v=>v!==null,'native close',30000);
  await browser?.close().catch(()=>{});
};
try {
  await wait(async()=>{const r=await fetch('http://127.0.0.1:'+cdpPort+'/json/version');return r.ok;},Boolean,'WebView2 debug endpoint');
  browser=await chromium.connectOverCDP('http://127.0.0.1:'+cdpPort);
  page=await wait(()=>browser.contexts().flatMap(c=>c.pages()).find(p=>p.url().includes('tauri.localhost')),Boolean,'native main page');
  page.on('pageerror',error=>report.errors.push(error.message));
  page.on('request',request=>{const url=new URL(request.url());if(url.pathname.startsWith('/api/'))report.requests.push({method:request.method(),path:url.pathname});});
  await page.getByRole('link',{name:'媒体库',exact:true}).waitFor();
  report.status=await get('/api/status');const runtime=await get('/api/recordings');assert.equal(runtime.filter(r=>r.monitorStatus||r.isRecording).length,0);
  assert.ok(report.status.buildId,'candidate must report its build fingerprint');
  const apiOrigin='http://127.0.0.1:6059';
  const taskFile=join(profile,'config','recordings.json');
  const diskBefore=await readFile(taskFile,'utf8');
  const memoryBefore=runtime.find(record=>record.recId===lifecycleId);
  assert.ok(memoryBefore.scheduledRecording&&memoryBefore.onlyNotifyNoRecord&&!memoryBefore.monitorStatus);
  const locker=spawn(process.env.STREAMCAP_TEST_PWSH||'pwsh',['-NoProfile','-Command',"$ErrorActionPreference='Stop';$file=[IO.File]::Open($env:STREAMCAP_LOCK_FILE,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite);try{[Console]::WriteLine('LOCKED');[Console]::ReadLine()|Out-Null}finally{$file.Dispose()}"],{env:{...process.env,STREAMCAP_LOCK_FILE:taskFile},windowsHide:true,stdio:['pipe','pipe','pipe']});
  let lockReady=false,lockError='';locker.stdout.on('data',data=>{if(String(data).includes('LOCKED'))lockReady=true;});locker.stderr.on('data',data=>{lockError+=String(data);});locker.on('error',error=>{lockError=error.message;});
  try {
    await wait(()=>({ready:lockReady,exit:locker.exitCode,error:lockError}),value=>{if(value.error||value.exit!==null)throw Error('Isolated file-lock helper failed: '+value.error);return value.ready;},'isolated task file lock',8000);
    const response=await fetch(apiOrigin+'/api/recordings/'+lifecycleId+'/monitor',{method:'POST',signal:AbortSignal.timeout(8000)});
    assert.equal(response.status,500,'real Windows replacement failure must be reported');
    assert.equal(await readFile(taskFile,'utf8'),diskBefore,'failed monitor update changed disk');
    assert.deepEqual((await get('/api/recordings')).find(record=>record.recId===lifecycleId),memoryBefore,'failed monitor update changed memory');
    report.nativeLifecycle={writeFailureStatus:response.status,memoryUnchanged:true,diskUnchanged:true};
  } finally {
    locker.stdin.end('\n');
    await wait(()=>locker.exitCode,value=>value!==null,'file-lock helper shutdown',8000);
  }
  const toggles=await Promise.all(Array.from({length:12},async()=>{
    const response=await fetch(apiOrigin+'/api/recordings/'+lifecycleId+'/monitor',{method:'POST',signal:AbortSignal.timeout(10000)});
    assert.equal(response.status,200);return (await response.json()).monitorStatus;
  }));
  assert.equal(toggles.filter(Boolean).length,6);
  assert.equal((await get('/api/recordings')).filter(record=>record.monitorStatus||record.isRecording).length,0);
  assert.equal(JSON.parse(await readFile(taskFile,'utf8')).find(record=>record.rec_id===lifecycleId).monitor_status,false);
  report.nativeLifecycle.concurrentToggles=12;report.nativeLifecycle.enabledResponses=6;report.nativeLifecycle.finalMonitoring=false;
  await page.getByRole('link',{name:'录制任务',exact:true}).click();
  await page.getByRole('textbox',{name:'搜索直播间'}).fill(lifecycleName);
  const lifecycleCard=page.locator('article[data-rec-id="'+lifecycleId+'"]');await lifecycleCard.waitFor();
  const listPattern=apiOrigin+'/api/recordings';let releaseSnapshot,capturedSnapshot;
  const snapshotGate=new Promise(resolve=>{releaseSnapshot=resolve;});const snapshotReady=new Promise(resolve=>{capturedSnapshot=resolve;});
  const delayedList=async route=>{const response=await route.fetch();capturedSnapshot();await snapshotGate;await route.fulfill({response});};
  await page.route(listPattern,delayedList);
  try {
    const completed=page.waitForResponse(response=>response.url()===listPattern&&response.status()===200);
    await page.getByRole('button',{name:'刷新列表',exact:true}).click();await snapshotReady;
    const deleted=await fetch(listPattern+'/'+lifecycleId,{method:'DELETE',signal:AbortSignal.timeout(8000)});assert.equal(deleted.status,200);
    await lifecycleCard.waitFor({state:'detached'});
    releaseSnapshot();await completed;
    await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
    assert.equal(await lifecycleCard.count(),0,'real SSE deletion was undone by a delayed snapshot');
    report.nativeLifecycle.staleSnapshotIgnored=true;report.nativeLifecycle.isolatedFixtureDeleted=true;
    await page.screenshot({path:join(output,'native-lifecycle-regression.png')});
  } finally {releaseSnapshot();await page.unroute(listPattern,delayedList);}
  await page.getByRole('textbox',{name:'搜索直播间'}).fill('');
  console.log('PASS real native persistence, 12 concurrent toggles and SSE snapshot regression');
  await page.getByRole('link',{name:'媒体库',exact:true}).click();
  for(const [index,file] of selected.entries()){
    await page.getByRole('button',{name:'返回媒体库根目录'}).click();
    const parts=relative(mediaRoot,file.path).split(sep);let openedAt=0;
    for(const part of parts){const name=page.locator('button.file-name').filter({has:page.locator('strong').filter({hasText:part})});openedAt=performance.now();await name.first().click();}
    const modal=page.getByRole('dialog',{name:'媒体预览',exact:true});await modal.waitFor();
    await page.waitForFunction(()=>{const v=document.querySelector('video');return v?.readyState>=2&&v.videoWidth>0&&v.currentTime>.2;},null,{timeout:20000});
    const info=await page.locator('video').evaluate(v=>({duration:v.duration,width:v.videoWidth,height:v.videoHeight}));
    const test={file:relative(mediaRoot,file.path),sha256:file.sha256,info,openMilliseconds:Math.round(performance.now()-openedAt),seeks:[],passed:false};report.cases.push(test);
    await modal.getByRole('button',{name:'暂停',exact:true}).click();
    const slider=modal.getByRole('slider',{name:'播放进度',exact:true});
    const transcodes=report.requests.filter(r=>r.path==='/api/media/transcode').length;
    for(const fraction of [.8,.2,.6,.35]){
      const box=await slider.boundingBox();const target=info.duration*fraction,start=performance.now();
      await page.mouse.move(box.x+6+(box.width-12)*.5,box.y+box.height/2);await page.mouse.down();await page.mouse.move(box.x+6+(box.width-12)*fraction,box.y+box.height/2,{steps:8});
      await page.waitForFunction(target=>{const v=document.querySelector('video');return v?.readyState>=2&&!v.seeking&&v.dataset.scrubbing==='true'&&Math.abs(v.currentTime-target)<1;},target,{timeout:8000});
      const frame=await page.locator('video').evaluate(v=>{const c=document.createElement('canvas');c.width=32;c.height=18;const x=c.getContext('2d');x.drawImage(v,0,0,32,18);return {position:v.currentTime,pixels:[...x.getImageData(0,0,32,18).data].reduce((sum,n)=>sum+n,0),latency:Number(v.dataset.seekLatencyMs),paused:v.paused};});
      assert.ok(frame.pixels>32*18*255,'decoded frame must contain image data');test.seeks.push({target,actual:frame.position,milliseconds:Math.round(performance.now()-start),playerLatencyMs:frame.latency,decodedBeforeRelease:true});
      await page.mouse.up();assert.equal(await page.locator('video').evaluate(v=>v.paused),true,'paused state must survive scrubbing');
    }
    assert.equal(report.requests.filter(r=>r.path==='/api/media/transcode').length,transcodes,'real MP4 seeks must not transcode');
    for(const theme of ['light','dark']){
      await page.evaluate(theme=>{document.documentElement.dataset.theme=theme;},theme);
      await page.screenshot({path:join(output,'real-'+index+'-'+theme+'.png')});
    }
    await page.evaluate(()=>{document.documentElement.dataset.theme='light';});
    await modal.getByRole('button',{name:'全屏',exact:true}).click();await page.waitForFunction(()=>Boolean(document.fullscreenElement));await page.screenshot({path:join(output,'real-'+index+'-fullscreen.png')});
    await page.getByRole('button',{name:'退出全屏',exact:true}).click();await page.waitForFunction(()=>!document.fullscreenElement);
    await modal.getByRole('button',{name:'全屏',exact:true}).click();await page.waitForFunction(()=>Boolean(document.fullscreenElement));
    await page.keyboard.press('Escape');await page.waitForFunction(()=>!document.fullscreenElement);await modal.waitFor();test.escapeRetainedPreview=true;
    await modal.getByRole('combobox',{name:'播放速度'}).selectOption('1.5');assert.equal(await page.locator('video').evaluate(v=>v.playbackRate),1.5);
    await modal.getByRole('button',{name:'播放',exact:true}).click();await page.waitForFunction(()=>!document.querySelector('video').paused);
    await modal.getByRole('button',{name:'关闭对话框',exact:true}).click();
    assert.equal(await page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),0);
    assert.equal((await get('/api/media/jobs')).activePreviews,0);
    test.passed=true;console.log('PASS real file '+basename(file.path));
  }
  const task=runtime.find(record=>record.streamerName===basename(dirname(selected[0].path)));
  if(task){
    await page.getByRole('link',{name:'录制任务',exact:true}).click();
    await page.getByRole('textbox',{name:'搜索直播间'}).fill(task.streamerName);
    const card=page.locator('.recording-card').filter({has:page.getByRole('button',{name:task.streamerName,exact:true})});
    await page.screenshot({path:join(output,'real-task-actions.png')});
    const filesPattern='http://127.0.0.1:6059/api/recordings/'+encodeURIComponent(task.recId)+'/files';
    let activeLists=0,maximumLists=0,completedLists=0;const nativeListMilliseconds=[];
    const delayedFiles=async route=>{activeLists++;maximumLists=Math.max(maximumLists,activeLists);try{const started=performance.now();const response=await route.fetch();nativeListMilliseconds.push(Math.round(performance.now()-started));await delay(2600);await route.fulfill({response});completedLists++;}finally{activeLists--;}};
    await page.route(filesPattern,delayedFiles);
    await card.getByRole('button',{name:'预览录制文件'}).click();const dialog=page.getByRole('dialog',{name:'录制预览',exact:true});
    await page.waitForFunction(()=>document.querySelector('video')?.readyState>=2);
    await dialog.getByRole('button',{name:'暂停',exact:true}).click();
    const layout=await dialog.evaluate(node=>{const body=node.querySelector('.modal-body').getBoundingClientRect(),files=node.querySelector('.preview-file-list').getBoundingClientRect(),controls=node.querySelector('.seek-controls').getBoundingClientRect();return {bottom:body.bottom,filesBottom:files.bottom,controlsBottom:controls.bottom};});
    assert.ok(layout.filesBottom<=layout.bottom+1&&layout.controlsBottom<=layout.bottom+1,'actual task preview must fit controls and the current file at the default window height');
    await wait(()=>completedLists,value=>value>=2,'real EXE slow listing refresh',12000);
    assert.equal(maximumLists,1);assert.ok(await dialog.locator('.preview-file').count()>0);
    report.taskPreview={passed:true,layout,slowResponseDelayMs:2600,maximumConcurrentLists:maximumLists,nativeListMilliseconds};await page.screenshot({path:join(output,'real-task-preview.png')});
    await dialog.getByRole('button',{name:'关闭对话框',exact:true}).click();
    await page.unroute(filesPattern,delayedFiles);
  }
  assert.equal(report.requests.some(r=>/(\/check|\/start|\/monitor)$/.test(r.path)),false,'real preview must not schedule platform requests');
  assert.deepEqual(report.errors,[]);
  const after=await inventory(mediaRoot);assert.deepEqual(after,before.map(({sha256,...entry})=>entry),'preview must not modify, add or delete recorded files');
  for(const file of selected)assert.equal(await sha256(file.path),file.sha256);
  report.mediaUnchanged=true;report.passed=true;
  if(options.leaveOpen){report.leftOpen=true;await page.getByRole('link',{name:'录制任务',exact:true}).click();await writeFile(join(output,'result.json'),JSON.stringify(report,null,2));await writeFile(join(output,'ready-for-desktop.json'),JSON.stringify({pid:child.pid,cdpPort,exe,profile},null,2));console.log('READY_FOR_DESKTOP '+JSON.stringify({pid:child.pid,cdpPort,output}));await new Promise(resolve=>child.once('exit',resolve));await browser.close().catch(()=>{});}
  else await finish();
  report.exitCode=child.exitCode;
} catch(error){report.failure=error.stack;await page?.screenshot({path:join(output,'failure.png')}).catch(()=>{});await finish().catch(()=>{if(child.exitCode===null)child.kill();});throw error;}
finally {await writeFile(join(output,'result.json'),JSON.stringify(report,null,2));console.log('Real preview evidence: '+output);}
