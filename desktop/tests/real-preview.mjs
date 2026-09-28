// Explicit real-media verification: candidate-only, random ports, fresh profile, immutable input.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {createReadStream} from 'node:fs';
import {mkdir,mkdtemp,readFile,readdir,stat,writeFile,realpath} from 'node:fs/promises';
import {basename,dirname,join,relative,resolve,sep} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawn,execFile} from 'node:child_process';
import {createServer} from 'node:net';
import {setTimeout as delay} from 'node:timers/promises';
import {chromium} from 'playwright';
const args={};for(let i=2;i<process.argv.length;i+=2){assert.ok(['--exe','--media-root','--expected-sha256'].includes(process.argv[i]));args[process.argv[i].slice(2)]=process.argv[i+1];}
assert.ok(args.exe&&args['media-root']&&args['expected-sha256']);
const desktop=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const exe=await realpath(resolve(args.exe)),mediaRoot=await realpath(resolve(args['media-root']));
assert.ok(exe.startsWith(join(desktop,'src-tauri','target')+sep),'Only a project candidate may be started');
const artifacts=resolve(process.env.STREAMCAP_TEST_ARTIFACTS||join(desktop,'tests','artifacts'));
assert.ok(artifacts===join(desktop,'tests','artifacts')||artifacts.startsWith(join(desktop,'tests','artifacts')+sep));
await mkdir(artifacts,{recursive:true});const output=await mkdtemp(join(artifacts,'real-preview-'));const profile=join(output,'data'),tmp=join(output,'tmp');await mkdir(tmp);
const hash=async file=>{const h=createHash('sha256');for await(const bytes of createReadStream(file))h.update(bytes);return h.digest('hex');};
assert.equal(await hash(exe),args['expected-sha256'].toLowerCase());
const inventory=async dir=>{const result=[];for(const entry of await readdir(dir,{withFileTypes:true})){if(entry.isSymbolicLink())continue;const file=join(dir,entry.name);if(entry.isDirectory())result.push(...await inventory(file));else if(entry.isFile()){const s=await stat(file);result.push({file,size:s.size,mtime:s.mtimeMs});}}return result;};
const before=await inventory(mediaRoot);const selected=before.filter(f=>/\.(mp4|ts|flv|mkv)$/i.test(f.file)).sort((a,b)=>b.size-a.size).slice(0,2);assert.ok(selected.length,'No real recording found');for(const f of selected)f.sha256=await hash(f.file);
const reserve=createServer();await new Promise(r=>reserve.listen(0,'127.0.0.1',r));const port=reserve.address().port;await new Promise(r=>reserve.close(r));
const startedAfter=new Date().toISOString();
const child=spawn(exe,['--data-dir',profile,'--api-port','0','--smoke-seconds','120'],{cwd:output,env:{...process.env,TEMP:tmp,TMP:tmp,WEBVIEW2_USER_DATA_FOLDER:join(profile,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-port='+port+' --remote-debugging-address=127.0.0.1'},windowsHide:true,stdio:'ignore'});
let spawnError;child.on('error',e=>{spawnError=e;});
const report={passed:false,acceptance:'presented frames, real pointer input and 20 target seeks per file',exe,pid:child.pid,actualMedia:true,productionProfileLoaded:false,output,cases:[],errors:[],requests:[]};let browser,page,base;
const wait=async(read,accept,label,ms=18000)=>{const until=Date.now()+ms;while(Date.now()<until){if(spawnError)throw spawnError;try{const v=await read();if(accept(v))return v;}catch{}await delay(100);}throw Error('Timed out: '+label);};
const measureSeek = async target => page.evaluate(target => new Promise(resolve => {
 const video=document.querySelector('video'),range=document.querySelector('.seek-range');
 const began=performance.now();let handle;
 const timer=setTimeout(()=>{video.cancelVideoFrameCallback(handle);resolve({target,error:'target frame timeout',position:video.currentTime,state:document.querySelector('.media-preview').dataset.playerState});},8000);
 const frame=(now,metadata)=>{
  const actual=Number(video.dataset.seekOffset)+metadata.mediaTime;
  if(Math.abs(actual-target)<.15){clearTimeout(timer);resolve({target,actual,decodedFrameMs:Math.max(0,now-began),method:'DOM input to video-frame compositor callback'});}
  else handle=video.requestVideoFrameCallback(frame);
 };
 handle=video.requestVideoFrameCallback(frame);range.value=String(target);
 range.dispatchEvent(new Event('input',{bubbles:true}));range.dispatchEvent(new Event('change',{bubbles:true}));
}),target);
const measureContinuousDrag = async duration => {
 const motion=await page.evaluate(async duration=>{
  const video=document.querySelector('video'),range=document.querySelector('.seek-range');
  const probe=window.streamcapMotion={active:true,frames:[],inputs:[],events:[],start:performance.now()};
  const frame=(now,metadata)=>{if(!probe.active)return;probe.frames.push({at:now,time:Number(video.dataset.seekOffset)+metadata.mediaTime});probe.handle=video.requestVideoFrameCallback(frame);};
  const event=e=>probe.events.push({type:e.type,at:performance.now(),visibility:document.visibilityState});
  probe.event=event;for(const name of ['blur','pointerup','pointercancel'])range.addEventListener(name,event);
  probe.handle=video.requestVideoFrameCallback(frame);range.focus();range.dispatchEvent(new PointerEvent('pointerdown',{bubbles:true}));
  for(let i=0;i<80;i++){
   // Fixed-duration input trace across 60/144/240Hz screens; keep frame requirements unchanged.
   await new Promise(resolve=>setTimeout(resolve,Math.max(0,probe.start+(i+1)*1000/60-performance.now())));
   await new Promise(requestAnimationFrame);const fraction=i<40?.1+.8*i/39:.9-.7*(i-40)/39;
   range.value=String(duration*fraction);probe.inputs.push({at:performance.now(),target:Number(range.value)});range.dispatchEvent(new Event('input',{bubbles:true}));
  }
  probe.end=probe.inputs.at(-1).at;probe.target=probe.inputs.at(-1).target;
  return {durationMs:probe.end-probe.start,inputEvents:probe.inputs.length,presentedDuringMotion:probe.frames.filter(f=>f.at<=probe.end).length,target:probe.target,method:'80 DOM range inputs over at least 1.33s, aligned to animation frames'};
 },duration);
 assert.equal(motion.inputEvents,80);assert.ok(motion.presentedDuringMotion>=10,'continuous frame starvation: '+JSON.stringify(motion));
 await page.waitForFunction(()=>{const p=window.streamcapMotion,v=document.querySelector('video');return !v.seeking&&!document.querySelector('.media-preview').classList.contains('is-seeking')&&p.frames.some(f=>f.at>=p.end&&Math.abs(f.time-p.target)<.15);},null,{timeout:10000});
 const final=await page.evaluate(()=>{
  const p=window.streamcapMotion,v=document.querySelector('video'),range=document.querySelector('.seek-range');p.active=false;v.cancelVideoFrameCallback(p.handle);
  for(const name of ['blur','pointerup','pointercancel'])range.removeEventListener(name,p.event);
  const f=p.frames.find(f=>f.at>=p.end&&Math.abs(f.time-p.target)<.15);range.dispatchEvent(new Event('change',{bubbles:true}));
  return {finalFrameLatencyMs:Math.max(0,f.at-p.end),framePosition:f.time,paused:v.paused,events:p.events};
 });
 assert.equal(final.paused,true);assert.ok(final.finalFrameLatencyMs<1000);return {...motion,...final};
};
const percentile = (values,p) => [...values].sort((a,b)=>a-b)[Math.max(0,Math.ceil(values.length*p)-1)];
try {
 const ready=await wait(async()=>JSON.parse(await readFile(join(profile,'ready.json'),'utf8')),Boolean,'candidate ready');assert.equal(ready.pid,child.pid);assert.equal(resolve(ready.dataDirectory),profile);base='http://'+ready.address;
 // Verify the exact launched process and loopback WebView owner before any CDP request.
 report.debugListenerOwnership=await wait(async()=>{
  if(child.exitCode!==null)return null;
  const value=await new Promise((yes,no)=>execFile(process.env.STREAMCAP_TEST_PWSH||'pwsh',
   ['-NoProfile','-File',join(desktop,'tests','process-identity.ps1'),'-Mode','CheckListener','-RootPid',String(child.pid),'-CandidateExecutable',exe,'-StartedAfter',startedAfter,'-Port',String(port)],
   {windowsHide:true},(error,stdout)=>error?no(error):yes(stdout)));
  return JSON.parse(value);
 },value=>value?.confirmed&&value.rootPid===child.pid&&value.port===port,'owned debug listener identity');
 await wait(async()=>{const r=await fetch('http://127.0.0.1:'+port+'/json/version');return r.ok;},Boolean,'owned debug listener');
 const changed=await fetch(base+'/api/settings',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({userConfig:{live_save_path:mediaRoot,convert_to_mp4:false,delete_original:false,scheduled_shutdown_enabled:false,execute_custom_script:false}})});assert.equal(changed.status,200);
 assert.equal((await(await fetch(base+'/api/recordings')).json()).length,0);
 browser=await chromium.connectOverCDP('http://127.0.0.1:'+port);page=await wait(()=>browser.contexts().flatMap(c=>c.pages()).find(p=>p.url().includes('tauri.localhost')),Boolean,'main webview');page.setDefaultTimeout(12000);
 page.on('pageerror',e=>report.errors.push(e.message));page.on('response',r=>{const u=new URL(r.url());if(u.pathname.startsWith('/api/'))report.requests.push({path:u.pathname,status:r.status()});});
 await page.route('**/api/**',route=>['GET','HEAD','OPTIONS'].includes(route.request().method())?route.continue():route.abort());
 await page.reload({waitUntil:'domcontentloaded'});await page.getByRole('navigation',{name:'主导航'}).getByRole('link',{name:'媒体库',exact:true}).click();
 for(const [index,file]of selected.entries()){
  await page.getByRole('button',{name:'返回媒体库根目录',exact:true}).click();const parts=relative(mediaRoot,file.file).split(sep);
  for(const part of parts.slice(0,-1))await page.locator('.file-name').filter({hasText:part}).click();
  const began=performance.now();await page.locator('.file-name').filter({hasText:basename(file.file)}).click();const modal=page.getByRole('dialog',{name:'媒体预览',exact:true});await modal.waitFor();
  await page.waitForFunction(()=>{const v=document.querySelector('video');return v?.videoWidth>0&&v.readyState>=2&&v.currentTime>.2;},null,{timeout:20000});
  const info=await page.locator('video').evaluate(v=>({duration:Number(v.dataset.timelineDuration)||v.duration,width:v.videoWidth,height:v.videoHeight,mode:v.dataset.playerMode,segmentLoads:Number(v.dataset.segmentLoads),bufferedSeconds:Array.from({length:v.buffered.length},(_,i)=>v.buffered.end(i)-v.buffered.start(i)).reduce((a,b)=>a+b,0)}));
  const result={file:relative(mediaRoot,file.file),sha256:file.sha256,firstFrameMs:Math.round(performance.now()-began),...info,seeks:[]};report.cases.push(result);
  await page.locator('video').evaluate(video=>{
    const trace=window.streamcapPlaybackTrace=[];
    const snapshot=(event,extra={})=>{trace.push({event,at:performance.now(),time:video.currentTime,paused:video.paused,seeking:video.seeking,readyState:video.readyState,state:document.querySelector('.media-preview')?.dataset.playerState,data:{...video.dataset},...extra});if(trace.length>160)trace.shift();};
    for(const name of ['play','playing','pause','seeking','seeked','waiting','ended','emptied'])video.addEventListener(name,()=>snapshot(name));
    for(const name of ['play','pause']){const original=video[name];video[name]=function(...args){snapshot(name+'()', {stack:new Error().stack});return original.apply(this,args);};}
    document.querySelector('.seek-play').addEventListener('click',()=>snapshot('play-button-click'));
  });
  await modal.getByRole('button',{name:'暂停',exact:true}).click();const slider=modal.getByRole('slider',{name:'播放进度',exact:true});
  assert.equal(await page.locator('video').evaluate(v=>typeof v.requestVideoFrameCallback),'function','performance acceptance requires presentation callbacks');
  // Keep a real pointer path for geometry and input behavior; time decoding separately.
  const box=await slider.boundingBox(),pointerTarget=info.duration*.75;
  await page.mouse.move(box.x+6+(box.width-12)*.1,box.y+box.height/2);await page.mouse.down();
  await page.mouse.move(box.x+6+(box.width-12)*.75,box.y+box.height/2,{steps:20});
  await page.waitForFunction(target=>{const v=document.querySelector('video');return v.dataset.scrubbing==='true'&&!v.seeking&&Math.abs(Number(v.dataset.framePosition)-target)<.15;},pointerTarget,{timeout:15000});
  await page.mouse.up();assert.equal(await page.locator('video').evaluate(v=>v.paused),true);result.pointerDragPassed=true;
  result.continuousDrag=await measureContinuousDrag(info.duration);
  for(const fraction of [.75,.2,.9,.08,.53,.31,.83,.12,.67,.42,.96,.26,.62,.16,.88,.37,.72,.04,.47,.58]){
   const measurement=await measureSeek(info.duration*fraction);assert.equal(measurement.error,undefined,JSON.stringify(measurement));
   await page.waitForFunction(()=>!document.querySelector('.media-preview').classList.contains('is-seeking'));
   assert.equal(await page.locator('video').evaluate(v=>v.paused),true,'paused intent changed at target '+measurement.target);result.seeks.push(measurement);
  }
  const latencies=result.seeks.map(s=>s.decodedFrameMs);
  result.seekSummary={count:latencies.length,p50Ms:percentile(latencies,.5),p95Ms:percentile(latencies,.95),maxMs:Math.max(...latencies)};
  assert.ok(result.seekSummary.p95Ms<500,'real-media P95 regression: '+JSON.stringify(result.seekSummary));
  await page.screenshot({path:join(output,'real-'+index+'.png'),animations:'disabled'});
  await modal.getByRole('button',{name:'全屏',exact:true}).click();await page.waitForFunction(()=>!!document.fullscreenElement);await page.keyboard.press('Escape');await page.waitForFunction(()=>!document.fullscreenElement);await modal.waitFor();
  await modal.getByRole('button',{name:'关闭对话框',exact:true}).click();await page.waitForFunction(async()=>(await import('/media-player.js')).activePlayerCount()===0);result.passed=true;
 }
 assert.deepEqual(report.errors,[]);report.passed=true;
}catch(e){
 report.failure=String(e);report.stack=e.stack;
 report.playbackTrace=await page?.evaluate(()=>window.streamcapPlaybackTrace).catch(()=>null);
 report.playerFailure=await page?.evaluate(()=>{const v=document.querySelector('video');return v?{time:v.currentTime,duration:v.duration,paused:v.paused,seeking:v.seeking,readyState:v.readyState,networkState:v.networkState,width:v.videoWidth,height:v.videoHeight,error:v.error?.code??null,buffered:Array.from({length:v.buffered.length},(_,i)=>[v.buffered.start(i),v.buffered.end(i)]),data:{...v.dataset},state:document.querySelector('.media-preview')?.dataset.playerState,visibility:document.visibilityState}:null;}).catch(()=>null);
 await page?.screenshot({path:join(output,'failure.png')}).catch(()=>{});process.exitCode=1;
}
finally{
 for(const file of selected){const unchanged=(await hash(file.file))===file.sha256;if(!unchanged){report.passed=false;report.failure='Original recording changed';process.exitCode=1;}}report.originalHashesUnchanged=!report.failure?.includes('Original');
 if(page&&!page.isClosed())await page.evaluate(async()=>(await import('/tauri-api/core.js')).invoke('desktop_smoke_tray_quit')).catch(()=>{});
 await browser?.close().catch(()=>{});await wait(()=>child.exitCode,v=>v!==null,'candidate exit',125000).catch(e=>{report.passed=false;report.shutdownFailure=String(e);process.exitCode=1;});
 report.candidateExited=child.exitCode!==null;
 const after=await inventory(mediaRoot);report.originalInventoryUnchanged=JSON.stringify(before.map(({file,size,mtime})=>({file,size,mtime})))===JSON.stringify(after);if(!report.originalInventoryUnchanged){report.passed=false;process.exitCode=1;}
 await writeFile(join(output,'result.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
}
