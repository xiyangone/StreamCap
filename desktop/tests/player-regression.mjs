// Real browser decoding and dragging; independent of Leptos fixture state and platform services.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createReadStream } from 'node:fs';
import { Readable } from 'node:stream';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, realpath, stat, writeFile } from 'node:fs/promises';
import { dirname, extname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { chromium } from 'playwright';

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dist = await realpath(join(desktop, 'dist'));
const publishedHtml = await readFile(join(dist, 'index.html'), 'utf8');
const stylesheetAssets = new Map();
const stylesheetLinks = [];
const localAssetOrigin = 'http://127.0.0.1';
const insideDist = file => {
  const path = relative(dist, file);
  return path !== '' && path !== '..' && !path.startsWith('..' + sep) && !isAbsolute(path);
};
for (const [tag] of publishedHtml.matchAll(/<link\b[^>]*>/gi)) {
  const attributes = {};
  for (const match of tag.matchAll(/\s(rel|href|integrity)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/gi)) {
    attributes[match[1].toLowerCase()] = match[2] ?? match[3] ?? match[4];
  }
  if (!attributes.rel?.toLowerCase().split(/\s+/).includes('stylesheet')) continue;
  assert.ok(attributes.href, 'A published stylesheet must have a local href');
  const url = new URL(attributes.href, localAssetOrigin + '/');
  assert.equal(url.origin, localAssetOrigin, 'Player CSS must not load from an external origin');
  assert.equal(url.username + url.password + url.search + url.hash, '', 'Published CSS URLs cannot carry credentials, queries or fragments');
  const file = resolve(dist, '.' + decodeURIComponent(url.pathname));
  assert.ok(insideDist(file), 'CSS paths must stay inside dist');
  assert.equal(extname(file).toLowerCase(), '.css', 'Only published CSS files may be loaded');
  const physical = await realpath(file);
  assert.ok(insideDist(physical), 'CSS symlinks or junctions must not escape dist');
  const bytes = await readFile(physical);
  assert.ok(!/@(?:source|plugin)\b|@import\s+["']tailwindcss/.test(bytes.toString()), 'Player regression requires compiled CSS, never source directives');
  if (attributes.integrity) {
    const verified = attributes.integrity.split(/\s+/).some(value => {
      const match = /^(sha256|sha384|sha512)-([A-Za-z0-9+/=]+)$/.exec(value);
      return match && createHash(match[1]).update(bytes).digest('base64') === match[2];
    });
    assert.ok(verified, 'Published stylesheet bytes must match dist/index.html integrity');
  }
  assert.ok(!stylesheetAssets.has(url.pathname), 'Published CSS links must be unambiguous');
  const escapeAttribute = value => value.replaceAll('&', '&amp;').replaceAll('"', '&quot;');
  stylesheetAssets.set(url.pathname, { bytes, path: relative(dist, physical), sha256: createHash('sha256').update(bytes).digest('hex') });
  stylesheetLinks.push(`<link rel="stylesheet" href="${escapeAttribute(url.pathname)}"${attributes.integrity ? ` integrity="${escapeAttribute(attributes.integrity)}"` : ''}>`);
}
assert.ok(stylesheetAssets.size > 0, 'Build dist before running player regression; no CSS fallback is permitted');
const publishedCss = [...stylesheetAssets.values()].map(asset => asset.bytes.toString()).join('\n');
assert.match(publishedCss, /\.ui-btn\b/, 'Published CSS includes the actual button library');
assert.match(publishedCss, /\.ui-select\b/, 'Published CSS includes the actual select library');
const allowed = join(desktop, 'tests', 'artifacts');
const artifactRoot = resolve(process.env.STREAMCAP_TEST_ARTIFACTS || allowed);
assert.ok(artifactRoot === allowed || artifactRoot.startsWith(allowed + sep));
await mkdir(artifactRoot, { recursive: true });
const output = await mkdtemp(join(artifactRoot, 'player-'));
const ffmpeg = process.env.STREAMCAP_TEST_FFMPEG || 'ffmpeg';
const run = promisify(execFile);
const mp4 = join(output, 'sample.mp4'), ts = join(output, 'sample.ts'), audioTs = join(output, 'audio.ts');
const offsetMp4 = join(output, 'offset.mp4');
await run(ffmpeg, ['-v','error','-nostdin','-f','lavfi','-i','testsrc2=size=320x180:rate=15','-t','18','-an','-c:v','libx264','-preset','ultrafast','-g','15','-pix_fmt','yuv420p','-movflags','+faststart',mp4], { windowsHide: true });
await run(ffmpeg, ['-v','error','-nostdin','-i',mp4,'-c','copy','-f','mpegts',ts], { windowsHide: true });
await run(ffmpeg, ['-v','error','-nostdin','-itsoffset','0.375','-i',mp4,'-f','lavfi','-i','sine=frequency=440:sample_rate=44100','-t','18','-c:v','copy','-c:a','aac','-movflags','+faststart',offsetMp4], { windowsHide: true });
await run(ffmpeg, ['-v','error','-nostdin','-f','lavfi','-i','sine=frequency=440:sample_rate=44100','-t','4','-c:a','aac','-f','mpegts',audioTs], { windowsHide: true });
const nativeBackend = process.env.STREAMCAP_TEST_BACKEND;
if (nativeBackend) { const url=new URL(nativeBackend); assert.equal(url.hostname,'127.0.0.1'); assert.notEqual(url.port,'6059'); }
const result = { passed: false, cases: [], performance: [], output, segments: 0, maxActiveSegments: 0, errors: [],
  stylesheets: [...stylesheetAssets.values()].map(({ path, sha256 }) => ({ path, sha256 })) };
const durationFor = path => path === 'short.ts' ? 0.8 : path === 'audio.wma' ? 4 : 18;
let active = 0, growing = false, nativeCopyByteLimit = 0;
const children = new Set();
const json = (res, value) => { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(value)); };
const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://localhost');
    if (url.pathname === '/') {
      res.setHeader('Content-Type','text/html; charset=utf-8');
      res.end(`<!doctype html><html lang="zh-CN" data-theme="light"><head>${stylesheetLinks.join('')}</head><body style="padding:36px;background:#e9eef6"><main style="max-width:960px;margin:auto"><div class="media-preview"><video class="video-player" playsinline aria-label="录制视频预览"></video></div></main><script src="/mpegts.js"></script><script type="module">import {attachMedia,activePlayerCount} from '/media-player.js';let dispose;window.beginPreview=path=>{dispose?.();const host=document.querySelector('.media-preview');host.dataset.path=path;dispose=attachMedia(host.querySelector('video'),location.origin,path,(phase,text)=>{host.dataset.playerState=phase;host.dataset.message=text;});};window.endPreview=()=>{dispose?.();dispose=null;};window.playerCount=activePlayerCount;window.frameHash=()=>{const v=document.querySelector('video'),c=document.createElement('canvas');c.width=16;c.height=9;const x=c.getContext('2d');x.drawImage(v,0,0,16,9);return [...x.getImageData(0,0,16,9).data].reduce((h,v)=>Math.imul(h^v,16777619)>>>0,2166136261);};</script></body></html>`); return;
    }
    if (stylesheetAssets.has(url.pathname)) {
      res.setHeader('Content-Type', 'text/css');
      res.end(stylesheetAssets.get(url.pathname).bytes); return;
    }
    const assets = { '/media-player.js': ['dist/media-player.js','application/javascript'], '/mpegts.js': ['dist/media/mpegts.js','application/javascript'] };
    if (assets[url.pathname]) { const [file, mime] = assets[url.pathname]; res.setHeader('Content-Type',mime); res.end(await readFile(join(desktop,file))); return; }
    if (nativeBackend && ['/api/media/info','/api/media/transcode'].includes(url.pathname) && url.searchParams.get('path')==='native-copy.ts') {
      const abort=new AbortController();res.once('close',()=>abort.abort());
      const byteLimit=url.pathname==='/api/media/transcode'?nativeCopyByteLimit:0;
      try {
        const response=await fetch(nativeBackend+url.pathname+url.search,{signal:abort.signal});
        if(res.destroyed){await response.body.cancel();return;}
        const headers={'Content-Type':response.headers.get('content-type')};
        for(const key of ['x-streamcap-offset','x-streamcap-preview-mode']){const value=response.headers.get(key);if(value!=null)headers[key]=value;}
        res.writeHead(response.status,headers);const stream=Readable.fromWeb(response.body);stream.on('error',()=>res.destroy());
        if (byteLimit) {
          // Bound this fixture's delivered media, keeping the real backend stream
          // open until the next seek cancels it. Fast machines must also cold-seek.
          let delivered=0;
          for await (const chunk of stream) {
            const bytes=chunk.subarray(0,byteLimit-delivered);
            delivered+=bytes.length;res.write(bytes);
            if(delivered===byteLimit) {
              await new Promise(resolve=>{
                if(abort.signal.aborted)resolve();
                else abort.signal.addEventListener('abort',resolve,{once:true});
              });
              break;
            }
          }
          if(!res.destroyed)res.end();
        } else stream.pipe(res);
      } catch(error) { if(!abort.signal.aborted)throw error; }
      return;
    }
    if (url.pathname === '/api/media/info') { json(res, { format: url.searchParams.get('path').split('.').at(-1), nativeCompatible: url.searchParams.get('path')!=='legacy.mp4', compatibilityAvailable: true, durationSeconds:durationFor(url.searchParams.get('path')), isRecording:growing, seekable:true, size:(await stat(ts)).size }); return; }
    if (url.pathname === '/api/media/transcode') {
      const path = url.searchParams.get('path'), duration = durationFor(path);
      const start = Number(url.searchParams.get('start') || 0); assert.ok(Number.isFinite(start) && start >= 0 && start < duration);
      result.segments++; active++; result.maxActiveSegments = Math.max(result.maxActiveSegments,active);
      const inputRate = path === 'slow.ts' ? ['-readrate','2'] : [];
      const length = Math.min(duration-start, path === 'chunked.ts' ? 4 : Infinity);
      const codec = path === 'audio.wma' ? ['-c:a','copy'] : ['-an','-c:v','libx264','-preset','ultrafast','-tune','zerolatency','-g','15'];
      const child = spawn(ffmpeg, ['-v','error','-nostdin',...inputRate,'-ss',String(start),'-i',path === 'audio.wma' ? audioTs : ts,'-t',String(length),...codec,'-f','mpegts','-muxdelay','0','pipe:1'], { windowsHide:true, stdio:['ignore','pipe','ignore'] });
      children.add(child); child.once('exit',()=>{children.delete(child);active--;});
      res.once('close',()=>{if(child.exitCode===null)child.kill();}); child.on('error',error=>res.destroy(error));
      res.writeHead(200,{'Content-Type':'video/mp2t','Cache-Control':'no-store'}); child.stdout.pipe(res); return;
    }
    if (url.pathname === '/api/videos') {
      const input = url.searchParams.get('path') === 'offset.mp4' ? offsetMp4 : mp4;
      const length = (await stat(input)).size; const match = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
      const start = match ? Number(match[1]) : 0, end = match && match[2] ? Math.min(Number(match[2]),length-1) : length-1;
      if (start >= length) { res.writeHead(416,{'Content-Range':'bytes */'+length});res.end();return; }
      const headers = {'Content-Type':'video/mp4','Content-Length':end-start+1,'Accept-Ranges':'bytes'};
      if (match) headers['Content-Range']=`bytes ${start}-${end}/${length}`;
      res.writeHead(match?206:200,headers);const file=createReadStream(input,{start,end});res.once('close',()=>file.destroy());file.pipe(res);return;
    }
    res.writeHead(404);res.end();
  } catch(error) { result.errors.push(error.message);if(!res.headersSent)res.writeHead(500);res.end(); }
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser = await chromium.launch({headless:true,args:['--autoplay-policy=no-user-gesture-required']});
const page = await browser.newPage({viewport:{width:1180,height:820}});
page.on('pageerror', error => result.errors.push(error.message));
const base = 'http://127.0.0.1:' + server.address().port;
const loadedStylesheets = new Set();
page.on('response', response => {
  const path = new URL(response.url()).pathname;
  if (stylesheetAssets.has(path)) {
    if (response.status() === 200) loadedStylesheets.add(path);
    else result.errors.push(`Published CSS returned ${response.status()}: ${path}`);
  }
});
await page.route('**/*', route => {
  const url = new URL(route.request().url());
  if (url.origin === base) return route.continue();
  result.errors.push('Blocked non-fixture browser request: ' + url.origin + url.pathname);
  return route.abort('blockedbyclient');
});
const at = async seconds => page.waitForFunction(seconds => {
  const v=document.querySelector('video');return v.readyState>=2&&!v.seeking&&Math.abs(Number(v.dataset.timelinePosition)-seconds)<0.6;
},seconds,{timeout:8000});
const step = async (name, action) => {const start=performance.now();await action();result.cases.push({name,passed:true,milliseconds:Math.round(performance.now()-start)});console.log('PASS '+name);};
try {
  await page.goto(base);
  await page.waitForFunction(()=>typeof window.beginPreview==='function');
  assert.deepEqual([...loadedStylesheets].sort(), [...stylesheetAssets.keys()].sort(), 'Every dist stylesheet must actually load in the browser');
  await step('native MP4 with a positive first timestamp starts at its real buffered range',async()=>{
    await page.evaluate(()=>{
      const v=document.querySelector('video'),get=Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype,'buffered').get;
      // WebView2 reports the A/V intersection; Chromium can include the empty video edit at zero.
      Object.defineProperty(v,'buffered',{configurable:true,get(){const b=get.call(this);return {length:b.length,start:i=>Math.max(.375,b.start(i)),end:i=>b.end(i)};}});
      window.beginPreview('offset.mp4');
    });
    await page.waitForFunction(()=>{const v=document.querySelector('video');return v.currentTime>.6&&!v.paused;},null,{timeout:5000});
    const state=await page.locator('video').evaluate(v=>({start:v.buffered.start(0),frame:Number(v.dataset.framePosition),mode:v.dataset.playerMode}));
    assert.ok(state.start>.15,JSON.stringify(state));assert.ok(state.frame>=state.start);assert.equal(state.mode,'native');
    assert.equal(await page.getByRole('group',{name:'播放与跳转',exact:true}).getByRole('button').count(),3);
    assert.equal(await page.getByRole('group',{name:'声音控制',exact:true}).getByRole('slider',{name:'音量',exact:true}).count(),1);
    assert.equal(await page.getByRole('group',{name:'播放选项',exact:true}).getByRole('combobox',{name:'播放速度',exact:true}).count(),1);
    assert.equal(await page.getByRole('button',{name:'暂停',exact:true}).getAttribute('aria-keyshortcuts'),'Space');
    const controlStyles = await page.evaluate(() => {
      const button = document.querySelector('.seek-play'), select = document.querySelector('.seek-rate');
      const measure = node => {
        const style = getComputedStyle(node), box = node.getBoundingClientRect();
        return { width: box.width, height: box.height, radius: parseFloat(style.borderRadius),
          fontSize: parseFloat(style.fontSize), borderWidth: parseFloat(style.borderTopWidth),
          background: style.backgroundColor };
      };
      const matchesLibraryRule = (rules, token, node) => [...rules].some(rule => {
        if (rule.selectorText?.includes(token)) {
          try { if (node.matches(rule.selectorText)) return true; } catch { /* Nested selectors are checked through their parent rule. */ }
        }
        return rule.cssRules && matchesLibraryRule(rule.cssRules, token, node);
      });
      return {
        rootFontSize: parseFloat(getComputedStyle(document.documentElement).fontSize),
        bodyFontSize: parseFloat(getComputedStyle(document.body).fontSize),
        button: measure(button), select: measure(select),
        buttonLibrary: button.classList.contains('ui-btn') && [...document.styleSheets].some(sheet => matchesLibraryRule(sheet.cssRules, '.ui-btn', button)),
        selectLibrary: select.classList.contains('ui-select') && [...document.styleSheets].some(sheet => matchesLibraryRule(sheet.cssRules, '.ui-select', select)),
      };
    });
    assert.equal(controlStyles.rootFontSize, 16);
    assert.equal(controlStyles.bodyFontSize, 14);
    assert.equal(controlStyles.buttonLibrary, true, 'A loaded library rule styles the playback button');
    assert.equal(controlStyles.selectLibrary, true, 'A loaded library rule styles the speed selector');
    assert.ok(controlStyles.button.width >= 32 && controlStyles.button.height >= 32 && controlStyles.button.radius >= 4);
    assert.notEqual(controlStyles.button.background, 'rgba(0, 0, 0, 0)', 'The primary playback button has its real styled background');
    assert.ok(controlStyles.select.width >= 48 && controlStyles.select.height >= 28 && controlStyles.select.borderWidth >= 1 && controlStyles.select.fontSize >= 13);
    result.controlStyles = controlStyles;
    await page.getByRole('button',{name:'暂停',exact:true}).click();
    await page.evaluate(()=>{const range=document.querySelector('.seek-range');range.value='0';range.dispatchEvent(new Event('input',{bubbles:true}));range.dispatchEvent(new Event('change',{bubbles:true}));});
    await page.waitForFunction(()=>{const v=document.querySelector('video');return !v.seeking&&!document.querySelector('.media-preview').classList.contains('is-seeking')&&Math.abs(v.currentTime-.375)<.08;});
    await page.evaluate(()=>{window.endPreview();delete document.querySelector('video').buffered;});
  });
  await step('native startup respects browser-suspended preload with real buffered frames',async()=>{
    await page.evaluate(()=>{
      const v=document.querySelector('video'),get=Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype,'buffered').get;
      Object.defineProperty(v,'buffered',{configurable:true,get(){const b=get.call(this);return {length:b.length,start:i=>b.start(i),end:i=>Math.min(2.7,b.end(i))};}});
      window.beginPreview('sample.mp4');
    });
    await page.waitForFunction(()=>{const v=document.querySelector('video');return v.currentTime>.2&&!v.paused;},null,{timeout:5000});
    const state=await page.locator('video').evaluate(v=>({ahead:Number(v.dataset.bufferAheadSeconds),required:Number(v.dataset.requiredBufferSeconds),frame:Number(v.dataset.framePosition)}));
    assert.ok(state.ahead<3&&state.ahead>=state.required&&Number.isFinite(state.frame),JSON.stringify(state));
    await page.evaluate(()=>{window.endPreview();delete document.querySelector('video').buffered;});
  });
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
  await step('late media events cannot overwrite explicit pause or play intent',async()=>{
    await page.evaluate(()=>window.beginPreview('sample.mp4'));
    await page.waitForFunction(()=>{const v=document.querySelector('video');return v.currentTime>.2&&!v.paused;});
    await page.getByRole('button',{name:'暂停',exact:true}).click();
    const stalePlayLabel=await page.locator('video').evaluate(async v=>{
      await new Promise(resolve=>setTimeout(resolve,50));
      v.dispatchEvent(new Event('play'));v.dispatchEvent(new Event('playing'));
      return document.querySelector('.seek-play').getAttribute('aria-label');
    });
    assert.equal(stalePlayLabel,'播放','delayed notifications cannot rewrite the last user command');
    assert.equal(await page.locator('video').evaluate(v=>v.paused),true);
    assert.equal(await page.getByRole('button',{name:'播放',exact:true}).count(),1,'a delayed play notification must not replace paused intent');
    await page.evaluate(()=>{const r=document.querySelector('.seek-range');r.value='9';r.dispatchEvent(new Event('input',{bubbles:true}));r.dispatchEvent(new Event('change',{bubbles:true}));});
    await page.waitForFunction(()=>!document.querySelector('.media-preview').classList.contains('is-seeking'));
    assert.equal(await page.locator('video').evaluate(v=>v.paused),true,'seeking must preserve explicit pause');
    await page.getByRole('button',{name:'播放',exact:true}).click();
    await page.waitForFunction(()=>!document.querySelector('video').paused);
    await page.locator('video').evaluate(v=>v.dispatchEvent(new Event('pause')));
    assert.equal(await page.getByRole('button',{name:'暂停',exact:true}).count(),1,'a delayed pause notification must not replace play intent');
    await page.evaluate(()=>window.endPreview());
  });
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
  for (const action of ['buffered seek', 'segment seek', 'buffered seek with failed info', 'latest', 'drag', 'continue']) {
    await step('delayed growing-file EOF respects '+action, async()=>{
      const pattern = '**/api/media/info?path=eof.ts';
      let held, capture, holding = false;
      const intercepted = new Promise(resolve => { capture = resolve; });
      const handler = route => {
        if (holding && !held) { held = route; capture(); return; }
        return route.continue();
      };
      await page.route(pattern, handler);
      growing = true;
      try {
        await page.evaluate(()=>window.beginPreview('eof.ts'));
        await page.waitForFunction(()=>{
          const video=document.querySelector('video');
          return video.readyState>=2 && Number(video.dataset.timelinePosition)>15.1
            && !document.querySelector('.media-preview').classList.contains('is-seeking');
        });
        // The initial growing-file segment starts at 15s. Load a complete buffered
        // timeline for local seeks, but retain that segment for the cold-seek case.
        if (action !== 'segment seek' && action !== 'continue') {
          await page.evaluate(()=>{
            const range=document.querySelector('.seek-range');
            range.value='0';range.dispatchEvent(new Event('input',{bubbles:true}));
            range.dispatchEvent(new Event('change',{bubbles:true}));
          });
          await page.waitForFunction(()=>{
            const video=document.querySelector('video');
            return !document.querySelector('.media-preview').classList.contains('is-seeking')
              && video.buffered.length && video.buffered.end(video.buffered.length-1)>17;
          });
          await page.evaluate(()=>{
            const range=document.querySelector('.seek-range');
            range.value='10';range.dispatchEvent(new Event('input',{bubbles:true}));
            range.dispatchEvent(new Event('change',{bubbles:true}));
          });
          await at(10);
        }
        await page.waitForFunction(()=>!document.querySelector('.media-preview').classList.contains('is-seeking'));
        holding = true;
        const before = await page.locator('video').evaluate(video=>{
          // Keep a real decoded frame while delivering a delayed EOF observation.
          // An internal media pause deliberately leaves the user's play intent intact.
          video.pause();
          const value={position:Number(video.dataset.timelinePosition),loads:Number(video.dataset.segmentLoads)};
          video.dispatchEvent(new Event('ended'));video.dispatchEvent(new Event('ended'));
          return value;
        });
        let timeout;
        await Promise.race([intercepted,new Promise((_,reject)=>{timeout=setTimeout(()=>reject(Error('EOF info request was not intercepted')),5000);})])
          .finally(()=>clearTimeout(timeout));
        if (action.includes('seek')) {
          await page.evaluate(()=>{
            const range=document.querySelector('.seek-range');
            range.value='2';range.dispatchEvent(new Event('input',{bubbles:true}));
            range.dispatchEvent(new Event('change',{bubbles:true}));
          });
          await at(2);
          await page.waitForFunction(()=>!document.querySelector('.media-preview').classList.contains('is-seeking'));
          await page.locator('video').evaluate(video=>video.pause());
          assert.equal(await page.locator('video').getAttribute('data-seek-kind'),action==='segment seek'?'segment':'buffered');
        } else if (action === 'latest') {
          await page.getByRole('button',{name:'回到最新',exact:true}).click();
        } else if (action === 'drag') {
          await page.locator('.seek-range').dispatchEvent('pointerdown');
        }
        const response = page.waitForResponse(response=>response.url()===base+'/api/media/info?path=eof.ts');
        holding = false;
        await held.fulfill({status:action.endsWith('failed info')?503:200,contentType:'application/json',body:JSON.stringify({
          format:'ts',nativeCompatible:true,compatibilityAvailable:true,durationSeconds:18,isRecording:true,seekable:true,
        })});
        held = null;
        await response;
        if (action === 'continue' || action === 'latest') {
          await at(action==='latest'?15:before.position);
          await page.waitForFunction(()=>!document.querySelector('video').paused && !document.querySelector('.media-preview').classList.contains('is-seeking'));
        }
        // Cover the old 800ms continuation timer as well as the promise completion.
        await page.waitForTimeout(950);
        const after = await page.locator('video').evaluate(video=>({
          position:Number(video.dataset.timelinePosition),loads:Number(video.dataset.segmentLoads),
          state:document.querySelector('.media-preview').dataset.playerState,scrubbing:video.dataset.scrubbing,
        }));
        assert.equal(after.loads,before.loads+Number(action==='segment seek'||action==='continue'),'only the current command may start a segment');
        assert.notEqual(after.state,'error','a superseded info failure must not overwrite the newer playback state');
        if (action.includes('seek')) assert.ok(Math.abs(after.position-2)<0.6,JSON.stringify(after));
        if (action==='latest') assert.ok(after.position>=15&&after.position<17,JSON.stringify(after));
        if (action==='drag') { assert.equal(after.scrubbing,'true');assert.ok(Math.abs(after.position-before.position)<0.2,JSON.stringify(after)); }
      } finally {
        holding = false;
        if (held) await held.abort().catch(()=>{});
        await page.evaluate(()=>window.endPreview());
        await page.unroute(pattern,handler);
        growing = false;
      }
    });
  }
  for (const format of ['mp4','ts']) {
    await step(format+' continuous input presents fresh frames before motion stops', async()=>{
      await page.evaluate(path=>window.beginPreview(path),'sample.'+format);
      await page.waitForFunction(()=>{const v=document.querySelector('video');return v.currentTime>.2&&v.buffered.length>0&&v.buffered.end(v.buffered.length-1)>17;});
      await page.getByRole('button',{name:'暂停',exact:true}).click();
      const before=result.segments;
      const motion=await page.evaluate(async()=>{
        const v=document.querySelector('video'),range=document.querySelector('.seek-range');
        const probe=window.motionProbe={active:true,start:performance.now(),frames:[],inputs:[]};
        const frame=(now,metadata)=>{if(!probe.active)return;probe.frames.push({at:now,time:Number(v.dataset.seekOffset)+metadata.mediaTime});probe.handle=v.requestVideoFrameCallback(frame);};
        probe.handle=v.requestVideoFrameCallback(frame);range.focus();range.dispatchEvent(new PointerEvent('pointerdown',{bubbles:true}));
        for(let i=0;i<80;i++){
          await new Promise(requestAnimationFrame);
          const fraction=i<40?.15+.7*i/39:.85-.65*(i-40)/39;
          range.value=String(18*fraction);probe.inputs.push({at:performance.now(),target:Number(range.value)});range.dispatchEvent(new Event('input',{bubbles:true}));
        }
        probe.end=probe.inputs.at(-1).at;probe.target=probe.inputs.at(-1).target;
        return {inputEvents:probe.inputs.length,durationMs:probe.end-probe.start,presentedDuringMotion:probe.frames.filter(f=>f.at<=probe.end).length,target:probe.target};
      });
      assert.equal(motion.inputEvents,80);
      assert.ok(motion.presentedDuringMotion>=10,'continuous movement must not starve the decoder: '+JSON.stringify(motion));
      await page.waitForFunction(()=>{const p=window.motionProbe,v=document.querySelector('video');return !v.seeking&&!document.querySelector('.media-preview').classList.contains('is-seeking')&&Math.abs(Number(v.dataset.framePosition)-p.target)<.15&&p.frames.some(f=>f.at>=p.end&&Math.abs(f.time-p.target)<.15);});
      const final=await page.evaluate(()=>{
        const p=window.motionProbe,v=document.querySelector('video'),range=document.querySelector('.seek-range');p.active=false;v.cancelVideoFrameCallback(p.handle);
        const f=p.frames.find(f=>Math.abs(f.time-p.target)<.15&&f.at>=p.end);
        range.dispatchEvent(new Event('change',{bubbles:true}));range.dispatchEvent(new PointerEvent('pointerup',{bubbles:true}));
        return {finalFrameLatencyMs:f?f.at-p.end:null,framePosition:Number(v.dataset.framePosition),presentationEvidence:v.dataset.presentationEvidence,paused:v.paused};
      });
      assert.equal(final.presentationEvidence,'video-frame-callback');assert.equal(final.paused,true);
      assert.ok(Math.abs(final.framePosition-motion.target)<.15);
      assert.ok(final.finalFrameLatencyMs!=null&&final.finalFrameLatencyMs<1000);
      assert.equal(result.segments,before,'warm dragging must not create compatibility workers');
      result.performance.push({format,...motion,...final});await page.evaluate(()=>window.endPreview());
    });
  }
  await step('startup waits for real buffered media instead of starting on the first frame',async()=>{
    await page.evaluate(()=>window.beginPreview('slow.ts'));
    await page.waitForFunction(()=>document.querySelector('video').readyState>=2);
    const initial=await page.locator('video').evaluate(v=>({paused:v.paused,ahead:Number(v.dataset.bufferAheadSeconds),required:Number(v.dataset.requiredBufferSeconds)}));
    assert.equal(initial.paused,true);assert.ok(initial.ahead<initial.required,JSON.stringify(initial));
    await page.waitForFunction(()=>document.querySelector('video').currentTime>.2);
    const ready=await page.locator('video').evaluate(v=>({ahead:Number(v.dataset.bufferAheadSeconds),required:Number(v.dataset.requiredBufferSeconds)}));
    assert.ok(ready.ahead+.03>=ready.required,JSON.stringify(ready));await page.evaluate(()=>window.endPreview());
  });
  await step('short media and compatibility segment tails do not wait for unavailable buffer',async()=>{
    await page.evaluate(()=>window.beginPreview('short.ts'));await page.waitForFunction(()=>document.querySelector('video').currentTime>.1);await page.evaluate(()=>window.endPreview());
    await page.evaluate(()=>window.beginPreview('chunked.ts'));await page.waitForFunction(()=>document.querySelector('video').currentTime>.2);
    await page.evaluate(()=>{const range=document.querySelector('.seek-range');range.value='3.7';range.dispatchEvent(new Event('input',{bubbles:true}));range.dispatchEvent(new Event('change',{bubbles:true}));});
    await page.waitForFunction(()=>Number(document.querySelector('video').dataset.timelinePosition)>4.3,null,{timeout:8000});await page.evaluate(()=>window.endPreview());
  });
  await step('audio-only compatibility media does not wait for a nonexistent video frame',async()=>{
    await page.evaluate(()=>window.beginPreview('audio.wma'));
    await page.waitForFunction(()=>document.querySelector('video').currentTime>.2,null,{timeout:8000});
    assert.equal(await page.locator('video').getAttribute('data-presentation-evidence'),'audio-data');await page.evaluate(()=>window.endPreview());
  });
  await step('closing during preparation releases media, callbacks and workers',async()=>{
    await page.evaluate(()=>window.beginPreview('slow.ts'));
    await page.waitForFunction(()=>document.querySelector('video').readyState>=2);await page.evaluate(()=>window.endPreview());
    assert.equal(await page.evaluate(()=>window.playerCount()),0);assert.equal(await page.locator('.seek-controls,.player-freeze').count(),0);
    await page.waitForTimeout(100);assert.equal(active,0);
  });
  if(nativeBackend) await step('Rust stream-copy preserves resolution, timestamps and real decoded frame on forward/backward seeks',async()=>{
    nativeCopyByteLimit=128*1024;
    try {
    await page.evaluate(()=>window.beginPreview('native-copy.ts'));
    await page.waitForFunction(()=>{const v=document.querySelector('video');return v.readyState>=2&&!v.seeking&&v.currentTime>.1;});
    await page.getByRole('button',{name:'暂停',exact:true}).click();
    assert.equal(await page.locator('video').getAttribute('data-preview-mode'),'copy');
    assert.deepEqual(await page.locator('video').evaluate(v=>[v.videoWidth,v.videoHeight]),[640,360]);
    const frame=()=>page.locator('video').evaluate(v=>{const c=document.createElement('canvas');c.width=c.height=1;const x=c.getContext('2d');x.drawImage(v,0,0,1,1);return [...x.getImageData(0,0,1,1).data].slice(0,3);});
    let coldSeeks=0;
    for(const [target,channel] of [[156.6,2],[3.6,0],[146.4,2],[1.2,0]]) {
      const slider=page.getByRole('slider',{name:'播放进度',exact:true}),box=await slider.boundingBox(),duration=Number(await page.locator('video').getAttribute('data-timeline-duration'));
      const loads=Number(await page.locator('video').getAttribute('data-segment-loads'));
      const buffered=await page.locator('video').evaluate(v=>Array.from({length:v.buffered.length},(_,i)=>[
        Number(v.dataset.seekOffset)+v.buffered.start(i),Number(v.dataset.seekOffset)+v.buffered.end(i),
      ]));
      assert.ok(!buffered.some(([start,end])=>target>=start&&target<=end),`cold seek target ${target} was already buffered: ${JSON.stringify(buffered)}`);
      const started=performance.now();await page.mouse.click(box.x+6+(box.width-12)*target/duration,box.y+box.height/2);await at(target);
      await page.waitForFunction(()=>!document.querySelector('.media-preview').classList.contains('is-seeking'));
      const milliseconds=Math.round(performance.now()-started),pixel=await frame();
      if(!(pixel[channel]>190&&pixel[channel===0?2:0]<50)) console.log('SEEK_DIAGNOSTIC '+JSON.stringify(await page.locator('video').evaluate(v=>({data:{...v.dataset},time:v.currentTime,ready:v.readyState,src:v.currentSrc,buffered:Array.from({length:v.buffered.length},(_,i)=>[v.buffered.start(i),v.buffered.end(i)]),message:document.querySelector('.media-preview').dataset.message}))));
      assert.ok(pixel[channel]>190&&pixel[channel===0?2:0]<50,`wrong decoded frame at ${target}: ${pixel}`);
      assert.ok(milliseconds<1800,`seek must decode promptly, not just move the slider: ${milliseconds}ms`);
      const reloaded=Number(await page.locator('video').getAttribute('data-segment-loads'))>loads;
      assert.ok(reloaded,`unbuffered target ${target} must request a backend segment`);
      if(reloaded)coldSeeks++;
      assert.equal(await page.locator('video').getAttribute('data-preview-mode'),'copy');
      result.cases.push({name:'native copied seek',target,milliseconds,pixel});
    }
    assert.ok(coldSeeks>=2,'cold forward and backward backend seeks are required');
    } finally { nativeCopyByteLimit=0; }
    await page.evaluate(()=>window.beginPreview('native-copy.ts'));
    await page.waitForFunction(()=>document.querySelector('video').currentTime>.1);
    await page.getByRole('button',{name:'暂停',exact:true}).click();
    await page.locator('video').evaluate(v=>{v.playbackRate=16;});
    const continuousLoads=Number(await page.locator('video').getAttribute('data-segment-loads'));
    await page.getByRole('button',{name:'播放',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('video').currentTime>125,null,{timeout:30000});
    assert.equal(Number(await page.locator('video').getAttribute('data-segment-loads')),continuousLoads,'no decoder rebuild at 120 seconds');
    result.cases.push({name:'continuous Rust playback crosses 120 seconds without a reload',passed:true});
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
  await step('incompatible MP4 uses compatibility mode and decoding errors remain visible',async()=>{
    await page.evaluate(()=>window.beginPreview('legacy.mp4'));
    await page.waitForFunction(()=>{const v=document.querySelector('video');return v?.readyState>=2&&v.dataset.playerMode==='timeline';});
    await page.evaluate(()=>{const video=document.querySelector('video');video.dispatchEvent(new Event('error'));video.dispatchEvent(new Event('playing'));});
    assert.equal(await page.locator('.media-preview').getAttribute('data-player-state'),'error');
    await page.evaluate(()=>window.endPreview());assert.equal(await page.evaluate(()=>window.playerCount()),0);
  });
  assert.deepEqual(result.errors,[]);result.passed=true;
} catch(error) {result.failure=error.stack;result.diagnostic=await page.locator('video').evaluate(v=>({data:{...v.dataset},time:v.currentTime,duration:v.duration,ready:v.readyState,paused:v.paused,seeking:v.seeking,error:v.error?.message,buffered:Array.from({length:v.buffered.length},(_,i)=>[v.buffered.start(i),v.buffered.end(i)]),state:document.querySelector('.media-preview').dataset.playerState,message:document.querySelector('.media-preview').dataset.message})).catch(()=>null);console.log('PLAYER_DIAGNOSTIC '+JSON.stringify(result.diagnostic));await page.screenshot({path:join(output,'failure.png')}).catch(()=>{});throw error;}
finally {await browser.close();for(const child of children)child.kill();await new Promise(resolve=>server.close(resolve));await writeFile(join(output,'result.json'),JSON.stringify(result,null,2));console.log('Player evidence: '+output);}
