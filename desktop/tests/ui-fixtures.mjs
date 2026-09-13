import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { dirname, extname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright';

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dist = join(desktop, 'dist');
const apiOrigin = 'http://127.0.0.1:6059';
const defaults = {
  record_quality: 'OD', video_format: 'TS', video_segment_time: '1800',
  segmented_recording_enabled: false, loop_time_seconds: '600',
  live_save_path: 'X:/Fixture/Recordings', recording_space_threshold: '1.0',
  folder_name_platform: true, folder_name_author: true, folder_name_time: false,
  filename_includes_title: true, theme_mode: 'light', theme_color: 'blue', is_grid_view: true, close_action: 'ask',
};
const inheritance = {
  quality: ['quality', 'record_quality'], record_format: ['recordFormat', 'video_format'],
  segment_record: ['segmentRecord', 'segmented_recording_enabled'],
  segment_time: ['segmentTime', 'video_segment_time'],
};
const sampleDate = 1789171200;

export function seedRecordings() {
  return [
    ['云间电台', 'douyin', '抖音', true, true, true, '清晨爵士 · 给生活一点慢节奏'],
    ['山野里的阿鹿', 'bilibili', '哔哩哔哩', false, false, true, '走进山野，看看不一样的世界'],
    ['小宇的游戏时光', 'huya', '虎牙', false, false, false, '今晚一起探索新的冒险'],
    ['海边散步', 'douyin', '抖音', false, true, true, '海浪与日落，今天也很美好'],
    ['不加糖音乐现场', 'bilibili', '哔哩哔哩', false, false, false, '把喜欢的旋律唱给你听'],
    ['深夜代码台', 'twitch', 'Twitch', false, false, true, 'Build something lovely, one line at a time'],
  ].map(([name, key, platform, recording, live, monitor, title], index) => ({
    recId: 'fixture-' + (index + 1), url: 'https://' + key + '.example.invalid/live/' + (index + 1),
    streamerName: name, platform, platformKey: key, recordFormat: 'TS', quality: 'OD',
    segmentRecord: false, segmentTime: '1800', monitorStatus: monitor, isLive: live,
    isRecording: recording, recordingError: null, liveTitle: title, speed: recording ? '2.4 MB/s' : null,
    recordingDir: recording ? 'X:/Fixture/Recordings/云间电台' : null,
    inheritedFields: Object.keys(inheritance), videoBitrate: null,
  }));
}

function initialState({ empty = false, offline = false } = {}) {
  return {
    recordings: empty ? [] : seedRecordings(), userConfig: {}, defaultConfig: structuredClone(defaults),
    cookies: { douyin: 'fixture-douyin-not-a-credential', bilibili: 'fixture-bilibili-not-a-credential' },
    offline, resolverReady: true, requests: [], unexpected: [], nextId: 7,
    failures: [], delays: [], qrPhase: 'waiting', qrCancelled: 0,
    native: { maximized: false, visible: true, closing: false, trayAvailable: true, pending: false, calls: [], failRemember: false },
    files: [
      { name: '云间电台', isDir: true, size: 734003200, path: '云间电台', modified: sampleDate },
      { name: '海边日落.ts', isDir: false, size: 128450560, path: '海边日落.ts', modified: sampleDate },
      { name: '清晨片段.wav', isDir: false, size: 16044, path: '清晨片段.wav', modified: sampleDate },
      { name: '录制说明.txt', isDir: false, size: 512, path: '录制说明.txt', modified: sampleDate },
      { name: '周末音乐.ts', isDir: false, size: 734003200, path: '云间电台/周末音乐.ts', modified: sampleDate },
    ],
  };
}

function wave() {
  const bytes = Buffer.alloc(16044);
  bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8);
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22);
  bytes.writeUInt32LE(8000, 24); bytes.writeUInt32LE(16000, 28);
  bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34); bytes.write('data', 36);
  bytes.writeUInt32LE(16000, 40);
  return bytes;
}

export async function createHarness(label, options = {}) {
  // No production backend, resolver, user configuration or recording files are opened.
  await readFile(join(dist, 'index.html'));
  const allowedArtifacts = join(desktop, 'tests', 'artifacts');
  const artifactsRoot = resolve(process.env.STREAMCAP_TEST_ARTIFACTS || allowedArtifacts);
  assert.ok(artifactsRoot === allowedArtifacts || artifactsRoot.startsWith(allowedArtifacts + sep), 'Artifacts must remain under tests/artifacts');
  await mkdir(artifactsRoot, { recursive: true });
  const runDir = await mkdtemp(join(artifactsRoot, label + '-'));
  const temp = join(runDir, 'tmp');
  await mkdir(temp);
  process.env.TEMP = temp;
  process.env.TMP = temp;
  const state = initialState(options);
  const streams = new Set();
  const blocked = [];
  const runtimeErrors = [];
  const consoleErrors = [];
  const emit = (name, value) => {
    for (const stream of streams) stream.write('event: ' + name + '\ndata: ' + JSON.stringify(value) + '\n\n');
  };
  const config = () => ({ ...state.defaultConfig, ...state.userConfig });
  const updateInherited = (record) => {
    for (const field of record.inheritedFields) {
      const mapping = inheritance[field];
      if (mapping) record[mapping[0]] = config()[mapping[1]];
    }
  };
  const applyEdit = (record, body) => {
    for (const [key, value] of Object.entries(body.changes ?? {})) {
      record[key] = value;
      for (const [field, [apiKey]] of Object.entries(inheritance)) {
        if (key === apiKey) record.inheritedFields = record.inheritedFields.filter((item) => item !== field);
      }
    }
    record.inheritedFields = [...new Set([...record.inheritedFields, ...(body.followGlobal ?? [])])];
    updateInherited(record);
    emit('update', record);
  };
  const json = (res, value, status = 200) => {
    res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
    res.end(JSON.stringify(value));
  };
  const server = createServer(async (req, res) => {
    res.setHeader('Access-Control-Allow-Origin', '*');
    res.setHeader('Access-Control-Allow-Methods', 'GET,POST,PUT,DELETE,OPTIONS');
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type,Range');
    res.setHeader('Access-Control-Expose-Headers', 'Content-Range');
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('X-Content-Type-Options', 'nosniff');
    if (req.method === 'OPTIONS') { res.writeHead(204); res.end(); return; }
    try {
      const url = new URL(req.url, 'http://127.0.0.1');
      const path = url.pathname;
      if (!path.startsWith('/api/')) {
        const decoded = decodeURIComponent(path);
        const asset = resolve(dist, '.' + decoded);
        if (asset !== dist && !asset.startsWith(dist + sep)) { json(res, { detail: 'Invalid path' }, 400); return; }
        const extension = extname(asset);
        const file = extension ? asset : join(dist, 'index.html');
        const mime = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css', '.png': 'image/png', '.svg': 'image/svg+xml' };
        try { const data = await readFile(file); res.writeHead(200, { 'Content-Type': mime[extname(file)] ?? 'application/octet-stream' }); res.end(data); }
        catch { res.writeHead(404); res.end(); }
        return;
      }
      const chunks = [];
      for await (const chunk of req) chunks.push(chunk);
      const raw = Buffer.concat(chunks).toString();
      const body = raw ? JSON.parse(raw) : {};
      const method = req.method;
      state.requests.push({ method, path, query: url.search, body: structuredClone(body) });
      const waitIndex = state.delays.findIndex((item) => item.method === method && item.path === path);
      if (waitIndex >= 0) await delay(state.delays.splice(waitIndex, 1)[0].ms);
      const failIndex = state.failures.findIndex((item) => item.method === method && item.path === path);
      if (failIndex >= 0) { const fail = state.failures.splice(failIndex, 1)[0]; json(res, { detail: fail.message }, fail.status); return; }
      if (state.offline) { json(res, { detail: '模拟本地服务暂时不可用' }, 503); return; }
      if (path === '/api/status') {
        json(res, { ok: true, version: '0.1.0', activeRecordings: state.recordings.filter((r) => r.isRecording).length, totalRecordings: state.recordings.length, resolverReady: state.resolverReady }); return;
      }
      if (path === '/api/events') {
        res.writeHead(200, { 'Content-Type': 'text/event-stream', Connection: 'keep-alive' });
        res.flushHeaders(); res.write(': isolated-fixture\n\n'); streams.add(res);
        res.on('close', () => streams.delete(res)); return;
      }
      if (path === '/api/recordings' && method === 'GET') { json(res, state.recordings); return; }
      if (path === '/api/recordings' && method === 'POST') {
        const created = (body.items ?? []).map((item) => {
          const base = seedRecordings()[2];
          const record = { ...base, recId: 'fixture-' + state.nextId++, url: item.url, streamerName: item.streamerName || '新直播间', platform: '自定义流', platformKey: 'custom', liveTitle: null, inheritedFields: Object.keys(inheritance) };
          updateInherited(record);
          if (item.quality) { record.quality = item.quality; record.inheritedFields = record.inheritedFields.filter((key) => key !== 'quality'); }
          state.recordings.push(record); emit('update', record); return record;
        });
        json(res, { created }); return;
      }
      if (path === '/api/recordings/batch-edit' && method === 'POST') {
        const ids = body.recIds;
        if (!Array.isArray(ids) || !ids.length || ids.some((id) => !state.recordings.some((r) => r.recId === id))) { json(res, { detail: '必须明确选择有效任务' }, 400); return; }
        state.recordings.filter((r) => ids.includes(r.recId)).forEach((r) => applyEdit(r, body));
        json(res, { updated: ids.length }); return;
      }
      if (path === '/api/recordings/delete' && method === 'POST') {
        const ids = body.recIds ?? [];
        state.recordings = state.recordings.filter((r) => !ids.includes(r.recId)); emit('delete', ids);
        json(res, { deleted: ids.length }); return;
      }
      const match = path.match(/^\/api\/recordings\/([^/]+)(?:\/(monitor|check|start|stop|files))?$/);
      if (match) {
        const id = decodeURIComponent(match[1]); const action = match[2];
        const record = state.recordings.find((r) => r.recId === id);
        if (!record) { json(res, { detail: '任务不存在' }, 404); return; }
        if (action === 'files') {
          json(res, { dir: 'X:/Fixture/Recordings/云间电台', files: [{ name: '周末音乐.ts', path: '云间电台/周末音乐.ts', size: 734003200, modified: sampleDate }] }); return;
        }
        if (method === 'DELETE') {
          state.recordings = state.recordings.filter((r) => r.recId !== id); emit('delete', [id]); json(res, { deleted: true }); return;
        }
        if (method === 'PUT') { applyEdit(record, body); json(res, { updated: true }); return; }
        if (method === 'POST') {
          if (action === 'monitor') { record.monitorStatus = !record.monitorStatus; if (!record.monitorStatus) record.isRecording = false; }
          if (action === 'start' || action === 'check') { if (!record.streamerName.trim()) record.streamerName = '自动识别主播'; record.isRecording = true; record.isLive = true; record.recordingError = null; record.speed = '1.8 MB/s'; }
          if (action === 'stop') { record.isRecording = false; record.recordingError = null; record.speed = null; }
          emit('update', record); json(res, { ok: true }); return;
        }
      }
      if (path === '/api/settings') {
        if (method === 'PUT') {
          Object.assign(state.userConfig, body.userConfig);
          for (const record of state.recordings) { updateInherited(record); emit('update', record); }
          emit('settings', {});
        }
        json(res, { defaultConfig: state.defaultConfig, userConfig: state.userConfig }); return;
      }
      if (path === '/api/cookies') {
        if (method === 'PUT') for (const [key, value] of Object.entries(body.cookies ?? {})) { if (value) state.cookies[key] = value; else delete state.cookies[key]; }
        json(res, { cookies: state.cookies }); return;
      }
      if (path === '/api/storage') {
        if (method === 'DELETE') {
          const target = url.searchParams.get('path');
          state.files = state.files.filter((f) => f.path !== target && !f.path.startsWith(target + '/'));
          json(res, { deleted: true }); return;
        }
        const folder = url.searchParams.get('subfolder') ?? '';
        const items = state.files.filter((f) => f.path.split('/').slice(0, -1).join('/') === folder);
        json(res, { root: 'X:/Fixture/Recordings', items, totalSize: items.reduce((sum, item) => sum + item.size, 0) }); return;
      }
      if (path === '/api/videos') {
        const bytes = wave();
        if ((url.searchParams.get('path') ?? '').endsWith('.wav')) { res.writeHead(200, { 'Content-Type': 'audio/wav', 'Content-Length': bytes.length }); res.end(bytes); }
        else json(res, { detail: '此模拟媒体不可内嵌播放' }, 415);
        return;
      }
      if (path.startsWith('/api/qr/kuaishou/')) {
        if (path.endsWith('/cancel')) { state.qrCancelled++; json(res, { ok: true }); return; }
        const messages={waiting:'请使用快手扫码',scanned:'已扫码，请在手机上确认登录',verifying:'手机已确认，正在验证直播站登录状态',success:'已验证账号：测试快手账号',error:'手机已确认，但直播站未返回可验证的登录状态。登录信息未保存，请稍后重试。',expired:'二维码已过期'};
        json(res, { sessionId: 'fixture-qr-session', state: state.qrPhase, message: messages[state.qrPhase], imageBase64: state.qrPhase==='waiting'?'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aK9sAAAAASUVORK5CYII=':'', secondsLeft: state.qrPhase==='waiting'?120:0, cookies: state.qrPhase === 'success' ? 'fixture-qr-not-a-credential' : null }); return;
      }
      state.unexpected.push(method + ' ' + path);
      json(res, { detail: 'Unexpected isolated fixture request' }, 501);
    } catch (error) {
      state.unexpected.push(String(error));
      if (!res.headersSent) json(res, { detail: 'Fixture failed' }, 500); else res.end();
    }
  });
  await new Promise((done, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', done); });
  const base = 'http://127.0.0.1:' + server.address().port;
  let browser;
  const closeServer = async () => {
    for (const stream of streams) stream.end();
    server.closeAllConnections();
    await new Promise((done) => server.close(done));
  };
  try { browser = await chromium.launch({ headless: true }); }
  catch (error) { await closeServer(); throw error; }
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'zh-CN', colorScheme: 'light', reducedMotion: 'reduce', serviceWorkers: 'block' });
  await context.route('**/*', async (route) => {
    const url = new URL(route.request().url());
    if (url.origin === apiOrigin && url.pathname.startsWith('/api/')) {
      await route.continue({ url: base + url.pathname + url.search }); return;
    }
    if (url.origin === base && url.pathname === '/tauri-api/core.js') {
      await route.fulfill({contentType:'text/javascript',body:"export function isTauri(){return true} export const invoke=(...args)=>window.__streamcapNativeMock.core.invoke(...args);"}); return;
    }
    if (url.origin === base && url.pathname === '/tauri-api/event.js') {
      await route.fulfill({contentType:'text/javascript',body:"export const listen=(...args)=>window.__streamcapNativeMock.event.listen(...args);"}); return;
    }
    if (url.origin === base) { await route.continue(); return; }
    blocked.push(url.origin + url.pathname);
    await route.abort('blockedbyclient');
  });
  const nativeStatus=()=>({maximized:state.native.maximized,visible:state.native.visible,closing:state.native.closing,trayAvailable:state.native.trayAvailable});
  await context.exposeBinding('__streamcapNativeFixture',async(_source,command,args={})=>{
    const native=state.native;native.calls.push({command,args:structuredClone(args)});let closeRequest;
    if(command==='desktop_ready')return {value:nativeStatus(),closeRequest:native.pending?{activeRecordings:state.recordings.filter(r=>r.isRecording).length,trayAvailable:native.trayAvailable}:undefined};
    if(command==='desktop_theme'){native.theme=args.theme;return {value:null};}
    if(command==='desktop_window_action'){
      if(args.action==='maximize')native.maximized=!native.maximized;
      else if(args.action==='minimize')native.minimized=true;
      else if(args.action==='close'){
        const policy=config().close_action;
        if(policy==='exit')native.closing=true;
        else if(policy==='tray'&&native.trayAvailable)native.visible=false;
        else{native.pending=true;closeRequest={activeRecordings:state.recordings.filter(r=>r.isRecording).length,trayAvailable:native.trayAvailable};}
      }else if(args.action!=='drag')throw Error('Unexpected native fixture action');
    }else if(command==='desktop_close_choice'){
      if(!native.pending)throw Error('没有待处理的关闭请求');
      if(args.choice==='cancel')native.pending=false;
      else{
        if(args.choice==='tray'&&!native.trayAvailable)throw Error('系统托盘不可用');
        if(args.remember&&native.failRemember){native.failRemember=false;throw Error('关闭偏好保存失败，窗口保持打开，请重试');}
        if(args.remember){state.userConfig.close_action=args.choice;emit('settings',{});}
        if(args.choice==='tray')native.visible=false;else if(args.choice==='exit')native.closing=true;else throw Error('Unexpected native fixture choice');
        native.pending=false;
      }
    }else{state.unexpected.push('NATIVE '+command);throw Error('Unexpected native fixture command');}
    return {value:nativeStatus(),windowState:nativeStatus(),closeRequest};
  });
  await context.addInitScript(apiOrigin=>{
    Object.defineProperty(window,'__STREAMCAP_RUNTIME__',{value:Object.freeze({apiOrigin})});
    const events=new Map();
    const emit=(name,payload)=>{for(const callback of events.get(name)??[])callback({event:name,payload});};
    window.__streamcapNativeFixtureEmit=emit;
    window.__streamcapNativeMock={event:{listen:async(name,callback)=>{let callbacks=events.get(name);if(!callbacks){callbacks=new Set();events.set(name,callbacks);}callbacks.add(callback);return ()=>callbacks.delete(callback);}},core:{invoke:async(command,args={})=>{
      const result=await window.__streamcapNativeFixture(command,args);if(result.windowState)emit('streamcap:window-state',result.windowState);if(result.closeRequest)emit('streamcap:close-requested',result.closeRequest);return result.value;
    }}};
  }, apiOrigin);
  const page = await context.newPage();
  page.setDefaultTimeout(7000);
  page.on('pageerror', (error) => runtimeErrors.push(error.stack ?? error.message));
  page.on('console', (message) => { if (message.type() === 'error') consoleErrors.push(message.text()); });
  return {
    base, state, page, context, runDir, blocked, runtimeErrors, consoleErrors, emit,
    failRecording(id, message) { const record = state.recordings.find(r => r.recId === id); assert.ok(record); record.isRecording = false; record.speed = null; record.recordingError = message; emit('update', record); },
    async restoreNative() {state.native.visible=true;state.native.minimized=false;await page.evaluate(value=>window.__streamcapNativeFixtureEmit("streamcap:window-state",value),nativeStatus());},
    failNext(method, path, message = '模拟操作失败', status = 409) { state.failures.push({ method, path, message, status }); },
    delayNext(method, path, ms = 700) { state.delays.push({ method, path, ms }); },
    setOffline(value) { state.offline = value; if (value) { for (const stream of streams) stream.end(); streams.clear(); } },
    async shot(name) { const file = join(runDir, name + '.png'); await page.screenshot({ path: file, animations: 'disabled' }); return file; },
    async report(value) { await writeFile(join(runDir, 'result.json'), JSON.stringify({ ...value, isolated: true, blocked, runtimeErrors, unexpectedApi: state.unexpected, requestCount: state.requests.length }, null, 2)); },
    async close() { await browser.close(); await closeServer(); },
  };
}

export async function eventually(read, predicate, message, timeout = 8000) {
  const deadline = Date.now() + timeout;
  let value;
  do { value = await read(); if (predicate(value)) return value; await delay(50); } while (Date.now() < deadline);
  assert.fail(message + ': ' + JSON.stringify(value));
}

export async function waitTheme(page, theme) {
  await page.waitForFunction((expected) => {
    const dark = expected === 'dark';
    const body = getComputedStyle(document.body);
    const sidebar = document.querySelector('.sidebar');
    return document.documentElement.dataset.theme === expected
      && body.backgroundColor === (dark ? 'rgb(16, 23, 37)' : 'rgb(240, 244, 250)')
      && body.color === (dark ? 'rgb(236, 241, 251)' : 'rgb(25, 38, 61)')
      && sidebar && getComputedStyle(sidebar).backgroundColor === (dark ? 'rgba(27, 37, 56, 0.85)' : 'rgba(255, 255, 255, 0.82)');
  }, theme);
}

export async function visible(locator) { await locator.waitFor({ state: 'visible' }); }
export async function hidden(locator) { await locator.waitFor({ state: 'hidden' }); }
export async function go(page, base, path) {
  await page.goto(base + path, { waitUntil: 'domcontentloaded' });
  await visible(page.locator('.app-shell'));
  await visible(page.locator('.page h1'));
  await hidden(page.locator('.skeleton-grid'));
}

export async function inspectLayout(page) {
  return page.evaluate(() => {
    const rect = (selector) => {
      const node = document.querySelector(selector);
      if (!node) return null;
      const bounds = node.getBoundingClientRect();
      return { width: bounds.width, height: bounds.height, top: bounds.top, left: bounds.left };
    };
    const content = document.querySelector('.content-area');
    const overflow = Array.from(document.querySelectorAll('.page *')).filter((el) => {
      if (!el.getClientRects().length || ['svg', 'path', 'circle', 'rect', 'line', 'polyline', 'polygon', 'i'].includes(el.tagName.toLowerCase())) return false;
      if (el.closest('[aria-hidden="true"]') || el.closest('.table-container') || el.closest('.filter-tabs')) return false;
      const b = el.getBoundingClientRect();
      return b.width > 0 && b.right > innerWidth + 2;
    }).slice(0, 5).map((el) => el.tagName + '.' + String(el.className));
    return {
      title: document.querySelector('h1')?.textContent, theme: document.documentElement.dataset.theme,
      shell: rect('.app-shell'), sidebar: rect('.sidebar'), heading: rect('h1'),
      bodyOverflow: document.documentElement.scrollWidth > innerWidth,
      contentOverflow: !!content && content.scrollWidth > content.clientWidth + 2,
      overflow, glass: getComputedStyle(document.querySelector('.glass')).backdropFilter,
      dialogs: document.querySelectorAll('dialog[open]').length,
    };
  });
}
