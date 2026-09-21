// Real browser decoding and dragging; independent of Leptos fixture state and platform services.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createReadStream } from 'node:fs';
import { Readable } from 'node:stream';
import { mkdir, mkdtemp, readFile, stat, writeFile } from 'node:fs/promises';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { chromium } from 'playwright';

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const allowed = join(desktop, 'tests', 'artifacts');
const artifactRoot = resolve(process.env.STREAMCAP_TEST_ARTIFACTS || allowed);
assert.ok(artifactRoot === allowed || artifactRoot.startsWith(allowed + sep));
await mkdir(artifactRoot, { recursive: true });
const output = await mkdtemp(join(artifactRoot, 'player-'));
const ffmpeg = process.env.STREAMCAP_TEST_FFMPEG || 'ffmpeg';
const run = promisify(execFile);
const mp4 = join(output, 'sample.mp4'), ts = join(output, 'sample.ts');
await run(ffmpeg, ['-v','error','-nostdin','-f','lavfi','-i','testsrc2=size=320x180:rate=15','-t','18','-an','-c:v','libx264','-preset','ultrafast','-g','15','-pix_fmt','yuv420p','-movflags','+faststart',mp4], { windowsHide: true });
await run(ffmpeg, ['-v','error','-nostdin','-i',mp4,'-c','copy','-f','mpegts',ts], { windowsHide: true });
const nativeBackend = process.env.STREAMCAP_TEST_BACKEND;
if (nativeBackend) { const url=new URL(nativeBackend); assert.equal(url.hostname,'127.0.0.1'); assert.notEqual(url.port,'6059'); }
const result = { passed: false, cases: [], output, segments: 0, maxActiveSegments: 0, errors: [] };
let active = 0, growing = false;
const children = new Set();
const json = (res, value) => { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(value)); };
const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://localhost');
    if (url.pathname === '/') {
      res.setHeader('Content-Type','text/html; charset=utf-8');
      res.end(`<!doctype html><html lang="zh-CN"><head><link rel="stylesheet" href="/styles.css"></head><body style="padding:36px;background:#e9eef6"><main style="max-width:960px;margin:auto"><div class="media-preview"><video class="video-player" playsinline aria-label="录制视频预览"></video></div></main><script src="/mpegts.js"></script><script type="module">import {attachMedia,activePlayerCount} from '/media-player.js';let dispose;window.beginPreview=path=>{dispose?.();const host=document.querySelector('.media-preview');host.dataset.path=path;dispose=attachMedia(host.querySelector('video'),location.origin,path,(phase,text)=>{host.dataset.playerState=phase;host.dataset.message=text;});};window.endPreview=()=>{dispose?.();dispose=null;};window.playerCount=activePlayerCount;window.frameHash=()=>{const v=document.querySelector('video'),c=document.createElement('canvas');c.width=16;c.height=9;const x=c.getContext('2d');x.drawImage(v,0,0,16,9);return [...x.getImageData(0,0,16,9).data].reduce((h,v)=>Math.imul(h^v,16777619)>>>0,2166136261);};</script></body></html>`); return;
    }
    const assets = { '/styles.css': ['styles.css','text/css'], '/media-player.js': ['assets/media-player.js','application/javascript'], '/mpegts.js': ['node_modules/mpegts.js/dist/mpegts.js','application/javascript'] };
    if (assets[url.pathname]) { const [file, mime] = assets[url.pathname]; res.setHeader('Content-Type',mime); res.end(await readFile(join(desktop,file))); return; }
    if (nativeBackend && ['/api/media/info','/api/media/transcode'].includes(url.pathname) && url.searchParams.get('path')==='native-copy.ts') {
      const abort=new AbortController();res.once('close',()=>abort.abort());
      try {
        const response=await fetch(nativeBackend+url.pathname+url.search,{signal:abort.signal});
        if(res.destroyed){await response.body.cancel();return;}
        const headers={'Content-Type':response.headers.get('content-type')};
        for(const key of ['x-streamcap-offset','x-streamcap-preview-mode']){const value=response.headers.get(key);if(value!=null)headers[key]=value;}
        res.writeHead(response.status,headers);const stream=Readable.fromWeb(response.body);stream.on('error',()=>res.destroy());stream.pipe(res);
      } catch(error) { if(!abort.signal.aborted)throw error; }
      return;
    }
    if (url.pathname === '/api/media/info') { json(res, { format: url.searchParams.get('path').split('.').at(-1), durationSeconds:18, isRecording:growing, seekable:true, size:(await stat(ts)).size }); return; }
    if (url.pathname === '/api/media/transcode') {
      const start = Number(url.searchParams.get('start') || 0); assert.ok(Number.isFinite(start) && start >= 0 && start < 18);
      result.segments++; active++; result.maxActiveSegments = Math.max(result.maxActiveSegments,active);
      const child = spawn(ffmpeg, ['-v','error','-nostdin','-ss',String(start),'-i',ts,'-t',String(18-start),'-an','-c:v','libx264','-preset','ultrafast','-tune','zerolatency','-g','15','-f','mpegts','-muxdelay','0','pipe:1'], { windowsHide:true, stdio:['ignore','pipe','ignore'] });
      children.add(child); child.once('exit',()=>{children.delete(child);active--;});
      res.once('close',()=>{if(child.exitCode===null)child.kill();}); child.on('error',error=>res.destroy(error));
      res.writeHead(200,{'Content-Type':'video/mp2t','Cache-Control':'no-store'}); child.stdout.pipe(res); return;
    }
    if (url.pathname === '/api/videos') {
      const length = (await stat(mp4)).size; const match = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
      const start = match ? Number(match[1]) : 0, end = match && match[2] ? Math.min(Number(match[2]),length-1) : length-1;
      if (start >= length) { res.writeHead(416,{'Content-Range':'bytes */'+length});res.end();return; }
      const headers = {'Content-Type':'video/mp4','Content-Length':end-start+1,'Accept-Ranges':'bytes'};
      if (match) headers['Content-Range']=`bytes ${start}-${end}/${length}`;
      res.writeHead(match?206:200,headers);const file=createReadStream(mp4,{start,end});res.once('close',()=>file.destroy());file.pipe(res);return;
    }
    res.writeHead(404);res.end();
  } catch(error) { result.errors.push(error.message);if(!res.headersSent)res.writeHead(500);res.end(); }
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser = await chromium.launch({headless:true,args:['--autoplay-policy=no-user-gesture-required']});
const page = await browser.newPage({viewport:{width:1180,height:820}});
page.on('pageerror', error => result.errors.push(error.message));
const at = async seconds => page.waitForFunction(seconds => {
  const v=document.querySelector('video');return v.readyState>=2&&!v.seeking&&Math.abs(Number(v.dataset.timelinePosition)-seconds)<0.6;
},seconds,{timeout:8000});
const step = async (name, action) => {const start=performance.now();await action();result.cases.push({name,passed:true,milliseconds:Math.round(performance.now()-start)});console.log('PASS '+name);};
try {
  await page.goto('http://127.0.0.1:'+server.address().port);
  await page.waitForFunction(()=>typeof window.beginPreview==='function');
  for (const format of ['mp4','ts']) {
    await step(format+' pauses, updates decoded frames during drag, and preserves paused intent', async()=>{
      await page.evaluate(path=>window.beginPreview(path),'sample.'+format);
      await page.waitForFunction(()=>{const v=document.querySelector('video');return v.readyState>=2&&v.currentTime>.2&&Number(v.dataset.timelineDuration)>17;});
      await page.getByRole('button',{name:'暂停',exact:true}).click();
      if(format==='ts')await page.waitForFunction(()=>{const v=document.querySelector('video');return v.buffered.length>0&&v.buffered.end(v.buffered.length-1)>17;});
      const initial=await page.evaluate(()=>window.frameHash()); const before=result.segments;
      const slider=page.getByRole('slider',{name:'播放进度',exact:true});const box=await slider.boundingBox();
      const x=f=>box.x+6+(box.width-12)*f, y=box.y+box.height/2;
      await page.mouse.move(x(.1),y);await page.mouse.down();await page.mouse.move(x(.8),y,{steps:8});
      await at(14.4);
      assert.equal(await page.locator('video').getAttribute('data-scrubbing'),'true','must decode before pointer release');
      const forward=await page.evaluate(()=>window.frameHash());assert.notEqual(forward,initial,'decoded image must change during drag');
      await page.mouse.move(x(.2),y,{steps:8});await at(3.6);
      assert.notEqual(await page.evaluate(()=>window.frameHash()),forward,'reverse drag must update the actual decoded image');
      await page.screenshot({path:join(output,'dragging-'+format+'.png')});
      await page.mouse.up();assert.equal(await page.locator('video').evaluate(v=>v.paused),true,'scrubbing a paused video must not autoplay');
      assert.equal(result.segments,before,'buffered/native seeks must not open new transcode streams');
      await page.getByRole('combobox',{name:'播放速度'}).selectOption('1.5');assert.equal(await page.locator('video').evaluate(v=>v.playbackRate),1.5);
      await page.getByRole('button',{name:'播放',exact:true}).click();await page.waitForFunction(()=>!document.querySelector('video').paused);
      await page.evaluate(()=>window.endPreview());assert.equal(await page.evaluate(()=>window.playerCount()),0);
    });
  }
  await step('growing TS distant scrubs coalesce work and display a frame before release', async()=>{
    growing=true;await page.evaluate(()=>window.beginPreview('sample.ts'));
    await page.waitForFunction(()=>{const v=document.querySelector('video');return v.readyState>=2&&Number(v.dataset.timelinePosition)>15.1;});
    await page.getByRole('combobox',{name:'播放速度'}).selectOption('1.5');
    const before=result.segments; const slider=page.getByRole('slider',{name:'播放进度',exact:true});const box=await slider.boundingBox();
    const x=f=>box.x+6+(box.width-12)*f,y=box.y+box.height/2;
    await page.mouse.move(x(.8),y);await page.mouse.down();
    for(const fraction of [.7,.6,.5,.4,.3,.2])await page.mouse.move(x(fraction),y,{steps:3});
    await at(3.6);assert.equal(await page.locator('video').getAttribute('data-scrubbing'),'true');
    assert.equal(await page.locator('video').evaluate(v=>v.playbackRate),1.5,'segment replacement must preserve playback speed');
    await page.mouse.up();await page.waitForFunction(()=>!document.querySelector('video').paused);
    assert.ok(result.segments-before<=4,'rapid scrubs must keep only a bounded number of segment restarts');
    await page.getByRole('button',{name:'回到最新',exact:true}).click();await at(15);
    await page.evaluate(()=>window.endPreview());growing=false;
  });
  if(nativeBackend) await step('Rust stream-copy preserves resolution, timestamps and real decoded frame on forward/backward seeks',async()=>{
    await page.evaluate(()=>window.beginPreview('native-copy.ts'));
    await page.waitForFunction(()=>{const v=document.querySelector('video');return v.readyState>=2&&!v.seeking&&v.currentTime>.1;});
    await page.getByRole('button',{name:'暂停',exact:true}).click();
    assert.equal(await page.locator('video').getAttribute('data-preview-mode'),'copy');
    assert.deepEqual(await page.locator('video').evaluate(v=>[v.videoWidth,v.videoHeight]),[640,360]);
    const frame=()=>page.locator('video').evaluate(v=>{const c=document.createElement('canvas');c.width=c.height=1;const x=c.getContext('2d');x.drawImage(v,0,0,1,1);return [...x.getImageData(0,0,1,1).data].slice(0,3);});
    for(const [target,channel] of [[156.6,2],[3.6,0],[146.4,2],[1.2,0]]) {
      const slider=page.getByRole('slider',{name:'播放进度',exact:true}),box=await slider.boundingBox(),duration=Number(await page.locator('video').getAttribute('data-timeline-duration'));
      const loads=Number(await page.locator('video').getAttribute('data-segment-loads'));
      const started=performance.now();await page.mouse.click(box.x+6+(box.width-12)*target/duration,box.y+box.height/2);await at(target);
      await page.waitForFunction(()=>!document.querySelector('.media-preview').classList.contains('is-seeking'));
      const milliseconds=Math.round(performance.now()-started),pixel=await frame();
      if(!(pixel[channel]>190&&pixel[channel===0?2:0]<50)) console.log('SEEK_DIAGNOSTIC '+JSON.stringify(await page.locator('video').evaluate(v=>({data:{...v.dataset},time:v.currentTime,ready:v.readyState,src:v.currentSrc,buffered:Array.from({length:v.buffered.length},(_,i)=>[v.buffered.start(i),v.buffered.end(i)]),message:document.querySelector('.media-preview').dataset.message}))));
      assert.ok(pixel[channel]>190&&pixel[channel===0?2:0]<50,`wrong decoded frame at ${target}: ${pixel}`);
      assert.ok(milliseconds<1800,`seek must decode promptly, not just move the slider: ${milliseconds}ms`);
      assert.ok(Number(await page.locator('video').getAttribute('data-segment-loads'))>loads,'must test an unbuffered backend seek, not just an already-loaded segment');
      assert.equal(await page.locator('video').getAttribute('data-preview-mode'),'copy');
      result.cases.push({name:'native copied seek',target,milliseconds,pixel});
    }
    await page.evaluate(()=>window.endPreview());
  });
  await step('keyboard seeking, narrow controls and repeated cleanup',async()=>{
    await page.evaluate(()=>window.beginPreview('sample.mp4'));
    await page.waitForFunction(()=>document.querySelector('video').currentTime>.2);
    await page.getByRole('button',{name:'暂停',exact:true}).click();
    const slider=page.getByRole('slider',{name:'播放进度',exact:true});await slider.focus();await page.keyboard.press('End');
    await page.waitForFunction(()=>Number(document.querySelector('video').dataset.timelinePosition)>17);
    await page.setViewportSize({width:480,height:700});await page.screenshot({path:join(output,'player-narrow.png')});
    assert.equal(await page.locator('.media-preview').evaluate(el=>el.scrollWidth<=el.clientWidth+1),true);
    await page.evaluate(()=>{for(let i=0;i<8;i++){window.beginPreview('sample.mp4');window.endPreview();}});
    assert.equal(await page.evaluate(()=>window.playerCount()),0);
    assert.equal(await page.locator('.seek-controls,.player-freeze').count(),0);
  });
  assert.deepEqual(result.errors,[]);result.passed=true;
} catch(error) {result.failure=error.stack;await page.screenshot({path:join(output,'failure.png')}).catch(()=>{});throw error;}
finally {await browser.close();for(const child of children)child.kill();await new Promise(resolve=>server.close(resolve));await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));console.log('Player evidence: '+output);}
