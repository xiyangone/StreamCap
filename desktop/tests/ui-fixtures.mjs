import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { dirname, extname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from 'playwright';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const apiContracts = JSON.parse(await readFile(join(desktop, 'tests/api-contracts.json'), 'utf8'));
const ffmpegBundle = JSON.parse(await readFile(join(desktop, 'ffmpeg-bundle.json'), 'utf8'));
const dist = join(desktop, 'dist');
const apiOrigin = 'http://127.0.0.1:6059';
let mediaSequence = 0;
export function seedMediaJob(overrides = {}) {
  return {
    id: `media-fixture-${++mediaSequence}`, taskId: null,
    source: 'fixture.ts', output: 'fixture.mp4', state: 'waiting',
    message: '等待转 MP4；原 TS 保留', deleteOriginal: false, sourceRemoved: false,
    stagePercent: null, stageEtaSeconds: null, startedAt: null, finishedAt: null,
    elapsedSeconds: 0, queuePosition: null, ...overrides,
  };
}
const defaults = {
  language: 'zh_CN', record_quality: 'OD', video_format: 'TS', video_segment_time: '1800',
  generate_time_subtitle_file: false, scheduled_shutdown_enabled: false, scheduled_shutdown_time: '23:00', execute_custom_script: false, custom_script_command: '', system_notification_enabled: false, stream_start_notification_enabled: false, stream_end_notification_enabled: false,
  segmented_recording_enabled: false, loop_time_seconds: '600', convert_to_mp4: false, delete_original: false,
  live_save_path: 'X:/Fixture/Recordings', recording_space_threshold: '1.0',
  folder_name_platform: true, folder_name_author: true, folder_name_time: false,
  filename_includes_title: true, theme_mode: 'light', is_grid_view: true, close_action: 'ask',
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
    isRecording: recording, recordedSeconds: recording ? 3661 : 0, recordingError: null, checkError: null, verificationRequired: false, liveTitle: title, speed: recording ? '2.4 MB/s' : null,
    recordingDir: recording ? 'X:/Fixture/Recordings/云间电台' : null,
    inheritedFields: Object.keys(inheritance), videoBitrate: null,
    checkState: 'idle', checkWaitReason: recording ? 'recording' : monitor ? 'interval' : 'paused',
    queuePosition: null, checkReadyAt: null, recoveryAt: null, recoveryAttempt: 0,
    nextCheckAt: monitor && !recording ? Math.floor(Date.now() / 1000) + 600 : null,
  }));
}

function initialState({ empty = false, offline = false } = {}) {
  return {
    recordings: empty ? [] : seedRecordings(), userConfig: {}, defaultConfig: structuredClone(defaults),
    cookies: { douyin: 'fixture-douyin-not-a-credential', bilibili: 'fixture-bilibili-not-a-credential' },
    offline, resolverReady: true, requests: [], unexpected: [], nextId: 7, mediaJobs: [], previewLive: false, activePreviews: 0, maxActivePreviews: 0, previewStarts: [],
    failures: [], delays: [], qrPhase: 'waiting', qrCancelled: 0, accounts: {}, quickShutdown: null,
    tools: { bundled: true, version: ffmpegBundle.version, ffmpegReady: true, ffprobeReady: true },
    native: { maximized: false, visible: true, closing: false, trayAvailable: true, pending: false, calls: [], failRemember: false, systemShutdown:false, shutdownSeconds:null },
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

export async function streamPreviewFixture(res, prepareBytes, state, preview) {
  if (res.destroyed) return;
  let closed = false, counted = false;
  // Preparation can outlive the client; observe closure before the first await.
  res.once('close', () => {
    closed = true;
    if (counted) { state.activePreviews--; counted = false; }
  });
  const bytes = await prepareBytes();
  if (closed || res.destroyed) return;
  counted = true;
  state.activePreviews++;
  state.maxActivePreviews = Math.max(state.maxActivePreviews, state.activePreviews);
  state.previewStarts.push(preview);
  res.writeHead(200, { 'Content-Type': 'video/mp2t', 'X-StreamCap-Offset': String(preview.start ?? 0), 'X-StreamCap-Preview-Mode': 'transcode' });
  for (let offset = 0; offset < bytes.length && !closed; offset += 4096) {
    res.write(bytes.subarray(offset, offset + 4096));
    await delay(60);
  }
  if (!closed) res.end();
}

export async function createHarness(label, options = {}) {
  // No production backend, resolver, user configuration or recording files are opened.
  await readFile(join(dist, 'index.html'));
  // CSS-only iteration is explicitly marked in evidence; release verification leaves this off.
  const sourceStylesPreview = process.env.STREAMCAP_TEST_SOURCE_CSS === '1';
  const previewStyles = sourceStylesPreview ? await readFile(join(desktop, 'build/ui.css')) : null;
  const previewIntegrity = previewStyles && 'sha384-' + createHash('sha384').update(previewStyles).digest('base64');
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
  const responseGates = { settingsRead: [], accountRead: [], accountWrite: [] };
  const responseReleases = new Set();
  const holdNextResponse = kind => {
    let received, release;
    const arrived = new Promise(resolve => { received = resolve; });
    const wait = new Promise(resolve => { release = resolve; });
    const gate = { received, wait, release: () => { responseReleases.delete(gate.release); release(); } };
    responseGates[kind].push(gate);
    responseReleases.add(gate.release);
    return { received: arrived, release: gate.release };
  };
  const waitForResponseGate = async kind => {
    const gate = responseGates[kind].shift();
    if (gate) { gate.received(); await gate.wait; }
  };
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
  let tsPromise;
  let mp4Promise;
  const mp4Bytes=()=>mp4Promise??=promisify(execFile)(process.env.STREAMCAP_TEST_FFMPEG||'ffmpeg',['-v','error','-nostdin','-f','lavfi','-i','testsrc2=size=160x120:rate=10','-f','lavfi','-i','sine=frequency=440:sample_rate=48000','-t','4','-c:v','libx264','-preset','ultrafast','-g','10','-pix_fmt','yuv420p','-c:a','aac','-movflags','frag_keyframe+empty_moov','-f','mp4','pipe:1'],{windowsHide:true,encoding:'buffer',timeout:15000,maxBuffer:4*1024*1024}).then(({stdout})=>stdout);
  const tsBytes = () => tsPromise ??= promisify(execFile)(process.env.STREAMCAP_TEST_FFMPEG || 'ffmpeg', ['-v','error','-nostdin','-f','lavfi','-i','testsrc2=size=160x120:rate=10','-f','lavfi','-i','sine=frequency=440:sample_rate=48000','-t','6','-c:v','libx264','-preset','ultrafast','-tune','zerolatency','-g','10','-pix_fmt','yuv420p','-c:a','aac','-f','mpegts','-muxdelay','0','pipe:1'], { windowsHide: true, encoding: 'buffer', timeout: 15000, maxBuffer: 4*1024*1024 }).then(({stdout}) => stdout);
  let tsFilePromise;
  const segmentBytes=async(start)=>{
    const file=await (tsFilePromise??=tsBytes().then(async bytes=>{const file=join(temp,'seek-source.ts');await writeFile(file,bytes);return file;}));
    const {stdout}=await promisify(execFile)(process.env.STREAMCAP_TEST_FFMPEG||'ffmpeg',['-v','error','-nostdin','-ss',String(start),'-i',file,'-t',String(Math.max(.3,6-start)),'-c:v','libx264','-preset','ultrafast','-tune','zerolatency','-c:a','aac','-f','mpegts','-muxdelay','0','pipe:1'],{windowsHide:true,encoding:'buffer',timeout:15000,maxBuffer:4*1024*1024});return stdout;
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
        const file = sourceStylesPreview && /^\/ui(?:-[a-f0-9]+)?\.css$/.test(decoded)
          ? join(desktop, 'build/ui.css') : extension ? asset : join(dist, 'index.html');
        const mime = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css', '.png': 'image/png', '.svg': 'image/svg+xml' };
        try {
          let data = sourceStylesPreview && file === join(desktop, 'build/ui.css') ? previewStyles : await readFile(file);
          if (previewStyles && extname(file) === '.html') {
            data = Buffer.from(data.toString().replace(/<link\b[^>]*>/g, tag =>
              /rel="stylesheet"/.test(tag) && /href="\/ui(?:-[a-f0-9]+)?\.css"/.test(tag)
                ? tag.replace(/integrity="[^"]*"/, `integrity="${previewIntegrity}"`) : tag));
          }
          res.writeHead(200, { 'Content-Type': mime[extname(file)] ?? 'application/octet-stream' }); res.end(data);
        }
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
        const heartbeat = setInterval(() => res.write('event: heartbeat\ndata: {}\n\n'), 5000);
        res.on('close', () => { clearInterval(heartbeat); streams.delete(res); }); return;
      }
      if (path === '/api/recordings' && method === 'GET') { json(res, state.recordings); return; }
      if (path === '/api/recordings' && method === 'POST') {
        const created = [], skipped = [];
        for (const item of body.items ?? []) {
          if (state.recordings.some(record => record.url === item.url)) { skipped.push({url:item.url,reason:'duplicate'}); continue; }
          const base = seedRecordings()[2];
          const record = { ...base, recId: 'fixture-' + state.nextId++, url: item.url, streamerName: item.streamerName || '新直播间', platform: '自定义流', platformKey: 'custom', monitorStatus: true, liveTitle: null, inheritedFields: Object.keys(inheritance) };
          updateInherited(record);
          if (item.quality) { record.quality = item.quality; record.inheritedFields = record.inheritedFields.filter((key) => key !== 'quality'); }
          state.recordings.push(record); created.push(record); emit('update', record);
        }
        json(res, { created, skipped }); return;
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
          json(res, { dir: 'X:/Fixture/Recordings/云间电台', files: state.files.filter(file=>!file.isDir&&file.path.startsWith('云间电台/')&&/\.(ts|mp4|mkv|flv|wav|wma)$/i.test(file.path)) }); return;
        }
        if (method === 'DELETE') {
          state.recordings = state.recordings.filter((r) => r.recId !== id); emit('delete', [id]); json(res, { deleted: true }); return;
        }
        if (method === 'PUT') { applyEdit(record, body); json(res, { updated: true }); return; }
        if (method === 'POST') {
          if (action === 'monitor') { record.monitorStatus = !record.monitorStatus; if (!record.monitorStatus) record.isRecording = false;
            Object.assign(record,{queuePosition:null,recoveryAt:null,recoveryAttempt:0,checkReadyAt:null,checkState:'idle',
              checkWaitReason:record.monitorStatus?'interval':'paused',nextCheckAt:record.monitorStatus?Math.floor(Date.now()/1000)+600:null}); }
          if (action === 'start' || action === 'check') { if (!record.streamerName.trim()) record.streamerName = '自动识别主播'; record.isRecording = true; record.isLive = true; record.recordingError = null; record.speed = '1.8 MB/s'; }
          if (action === 'stop') { record.isRecording = false; record.recordingError = null; record.speed = null;
            Object.assign(record,{queuePosition:null,recoveryAt:null,recoveryAttempt:0,checkReadyAt:null,checkState:'idle',checkWaitReason:record.monitorStatus?'interval':'paused'}); }
          emit('update', record); json(res, { ok: true }); return;
        }
      }
      if (path === '/api/settings') {
        if (method === 'PUT') {
          const patch = body.userConfig ?? {};
          if (Object.hasOwn(patch, 'theme_color') || Object.hasOwn(patch, 'theme_mode') && !['light','dark'].includes(patch.theme_mode)) {
            json(res, {detail:'主题模式无效'}, 400); return;
          }
          const changed = Object.keys(patch).filter(key => JSON.stringify(state.userConfig[key]) !== JSON.stringify(patch[key]));
          Object.assign(state.userConfig, patch);
          if (changed.some(key => Object.values(inheritance).some(([, setting]) => setting === key))) {
            for (const record of state.recordings) {
              const before = JSON.stringify(record);
              updateInherited(record);
              if (JSON.stringify(record) !== before) emit('update', record);
            }
          }
          if (changed.length) emit('settings', { changed });
          json(res, { ok: true, changed }); return;
        }
        const snapshot = structuredClone({ defaultConfig: state.defaultConfig, userConfig: state.userConfig });
        await waitForResponseGate('settingsRead');
        json(res, snapshot); return;
      }
      if (path === '/api/accounts') {
        if (method === 'PUT') {
          Object.assign(state.accounts[body.platform] ??= {}, body.changes ?? {});
          await waitForResponseGate('accountWrite');
          json(res, { ok: true }); return;
        }
        const snapshot = Object.fromEntries(Object.entries(state.accounts).map(([key, value]) => [key, {
          username: value.username ?? '', accountType: value.accountType ?? '',
          hasPassword: !!value.password, hasAccessToken: !!value.accessToken,
        }]));
        await waitForResponseGate('accountRead');
        json(res, snapshot); return;
      }
      if(path==='/api/tools/status'){json(res,state.tools);return;}
      if(path==='/api/tools/update'){json(res,apiContracts.updateCheck);return;}
      if(path==='/api/automation/shutdown'){state.quickShutdown=body.hours??null;json(res,{ok:true});return;}
      if(path==='/api/media/screenshot'){assert.ok(Buffer.from(body.pngBase64,'base64').subarray(0,8).equals(Buffer.from([137,80,78,71,13,10,26,10])));json(res,{path:'fixture-screenshot.png'});return;}
      if (path === '/api/cookies') {
        if (method === 'PUT') for (const [key, value] of Object.entries(body.cookies ?? {})) { if (value) state.cookies[key] = value; else delete state.cookies[key]; }
        json(res, { cookies: state.cookies }); return;
      }
      if (path === '/api/storage/size') {
        const entry=state.files.find(item=>item.path===url.searchParams.get('path'));
        setTimeout(()=>{if(!res.destroyed)json(res,{size:entry?.size||0});},state.storageSizeDelay||25);return;
      }
      if (path === '/api/storage') {
        if (method === 'DELETE') {
          const target = url.searchParams.get('path');
          state.files = state.files.filter((f) => f.path !== target && !f.path.startsWith(target + '/'));
          json(res, { deleted: true }); return;
        }
        const folder = url.searchParams.get('subfolder') ?? '';
        const items = state.files.filter((f) => f.path.split('/').slice(0, -1).join('/') === folder);
        json(res, { root: 'X:/Fixture/Recordings', items:items.map(item=>({...item,size:item.isDir?0:item.size,sizeKnown:!item.isDir})), totalSizeKnown:!items.some(item=>item.isDir), totalSize: items.filter(item=>!item.isDir).reduce((sum, item) => sum + item.size, 0) }); return;
      }
      if (path === '/api/media/jobs') { json(res, { jobs: state.mediaJobs, ready: true, activePreviews: state.activePreviews }); return; }
      if (path === '/api/media/info') { const file = url.searchParams.get('path') || ''; json(res, { format: file.split('.').at(-1), nativeCompatible:!file.endsWith('.ts'), compatibilityAvailable:true, isRecording: state.previewLive, durationSeconds: 6, seekable: true, size: file.endsWith('.ts') ? (await tsBytes()).length : 16044 }); return; }
      if (path === '/api/media/remux') {
        const source = body.path; const output = source.replace(/\.ts$/i, '.mp4');
        if (!state.files.some(f => f.path === source && !f.isDir)) { json(res, {detail:'源 TS 不存在'},404); return; }
        const active = state.mediaJobs.find(job => job.source === source && ['waiting','running','verifying','cleaning'].includes(job.state));
        if (active) { json(res, {job:active}); return; }
        if (state.files.some(f => f.path === output)) { json(res, {detail:'同名 MP4 已存在，未覆盖'},409); return; }
        const linkedTasks=[...new Set(state.mediaJobs.filter(job=>job.source===source&&job.taskId).map(job=>job.taskId))];
        const taskId=linkedTasks.length===1?linkedTasks[0]:null;
        const job=seedMediaJob({source,output,taskId,deleteOriginal:body.deleteOriginal??false});state.mediaJobs.push(job);emit('mediaJob',job);json(res,{job});
        setTimeout(()=>{job.state='running';job.startedAt=Math.floor(Date.now()/1000);job.stagePercent=35;job.message='正在无损转封装为 MP4';emit('mediaJob',job);},60);
        setTimeout(()=>{job.state='verifying';job.stagePercent=null;job.message='正在校验 MP4 音视频与时长';emit('mediaJob',job);},150);
        setTimeout(()=>{job.state='complete';job.stagePercent=100;job.finishedAt=Math.floor(Date.now()/1000);job.message='MP4 已生成并校验；原 TS 保留';state.files.push({name:output.split('/').at(-1),isDir:false,size:24000,path:output,modified:sampleDate});emit('mediaJob',job);},300);return;
      }
      if (path === '/api/media/transcode' || /^\/api\/recordings\/[^/]+\/preview$/.test(path)) {
        const start = Number(url.searchParams.get('start') || 0);
        await streamPreviewFixture(res, () => path === '/api/media/transcode' ? segmentBytes(start) : tsBytes(), state, { start, path });
        return;
      }
      if (path === '/api/videos') {
        const file=url.searchParams.get('path')??'';
        const bytes=file.endsWith('.wav')?wave():file.endsWith('.ts')?await tsBytes():file.endsWith('.mp4')?await mp4Bytes():null;
        if(!bytes){json(res,{detail:'模拟文件不支持'},415);return;}
        const range=/^bytes=(\d+)-(\d*)$/.exec(req.headers.range??'');const start=range?Number(range[1]):0;const end=range&&range[2]?Math.min(Number(range[2]),bytes.length-1):bytes.length-1;
        if(start>=bytes.length){res.writeHead(416,{'Content-Range':'bytes */'+bytes.length});res.end();return;}
        const headers={'Content-Type':file.endsWith('.ts')?'video/mp2t':file.endsWith('.mp4')?'video/mp4':'audio/wav','Content-Length':end-start+1,'Accept-Ranges':'bytes'};if(range)headers['Content-Range']='bytes '+start+'-'+end+'/'+bytes.length;res.writeHead(range?206:200,headers);res.end(bytes.subarray(start,end+1));return;
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
  const nativeStatus=()=>({maximized:state.native.maximized,visible:state.native.visible,closing:state.native.closing,trayAvailable:state.native.trayAvailable,systemShutdown:state.native.systemShutdown,shutdownSeconds:state.native.shutdownSeconds});
  await context.exposeBinding('__streamcapNativeFixture',async(_source,command,args={})=>{
    const native=state.native;native.calls.push({command,args:structuredClone(args)});let closeRequest;
    if(command==='desktop_ready')return {value:nativeStatus(),closeRequest:native.pending?{activeRecordings:state.recordings.filter(r=>r.isRecording).length,trayAvailable:native.trayAvailable}:undefined};
    if(command==='desktop_theme'){native.theme=args.theme;return {value:null};}
    if(command==='desktop_kuaishou_session'){
      if(!['open','retry'].includes(args.action)||Object.hasOwn(args,'fixtureUrl'))throw Error('Unexpected Kuaishou session action');
      if(native.kuaishouDelay)await new Promise(resolve=>setTimeout(resolve,native.kuaishouDelay));
      if(native.kuaishouFailure){const message=native.kuaishouFailure;native.kuaishouFailure=null;throw Error(message);}
      if(args.action==='retry')return {value:{restored:native.kuaishouRestored??1}};
      native.kuaishouOpened=(native.kuaishouOpened??0)+1;
      return {value:{opened:true}};
    }
    if(command==='desktop_window_action'){
      if(args.action==='pick-directory')return {value:'X:/Fixture/Picked'};
      if(args.action==='cancel-shutdown'){native.systemShutdown=false;native.shutdownSeconds=null;state.quickShutdown=null;return {value:null,windowState:nativeStatus()};}
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
    async triggerShutdown(seconds=60){state.native.systemShutdown=true;state.native.shutdownSeconds=seconds;await page.evaluate(value=>window.__streamcapNativeFixtureEmit('streamcap:window-state',value),nativeStatus());},
    async restoreNative() {state.native.visible=true;state.native.minimized=false;await page.evaluate(value=>window.__streamcapNativeFixtureEmit("streamcap:window-state",value),nativeStatus());},
    failNext(method, path, message = '模拟操作失败', status = 409) { state.failures.push({ method, path, message, status }); },
    delayNext(method, path, ms = 700) { state.delays.push({ method, path, ms }); },
    holdNextSettingsRead: () => holdNextResponse('settingsRead'),
    holdNextAccountRead: () => holdNextResponse('accountRead'),
    holdNextAccountWrite: () => holdNextResponse('accountWrite'),
    setOffline(value) { state.offline = value; if (value) { for (const stream of streams) stream.end(); streams.clear(); } },
    async shot(name) { const file = join(runDir, name + '.png'); await page.screenshot({ path: file, animations: 'disabled' }); return file; },
    async report(value) { await writeFile(join(runDir, 'result.json'), JSON.stringify({ ...value, sourceStylesPreview, isolated: true, blocked, runtimeErrors, consoleErrors, unexpectedApi: state.unexpected, requestCount: state.requests.length }, null, 2)); },
    async close() { for (const release of responseReleases) release(); await browser.close(); await closeServer(); },
  };
}

export async function eventually(read, predicate, message, timeout = 8000) {
  const deadline = Date.now() + timeout;
  let value;
  do { value = await read(); if (predicate(value)) return value; await delay(50); } while (Date.now() < deadline);
  assert.fail(message + ': ' + JSON.stringify(value));
}

export const themeColors = {
  light: { canvas: 'rgb(237, 240, 246)', surface: 'rgba(255, 255, 255, 0.76)', overlay: 'rgba(255, 255, 255, 0.96)', text: 'rgb(38, 50, 68)', sidebar: 'rgba(248, 250, 255, 0.76)' },
  dark: { canvas: 'rgb(17, 21, 30)', surface: 'rgba(29, 35, 48, 0.8)', overlay: 'rgba(32, 39, 52, 0.97)', text: 'rgb(231, 235, 241)', sidebar: 'rgba(22, 28, 39, 0.82)' },
};

export async function waitTheme(page, theme) {
  assert.ok(Object.hasOwn(themeColors, theme), 'Use an explicit supported theme');
  await page.waitForFunction(({ expected, colors }) => {
    const body = getComputedStyle(document.body);
    const sidebar = document.querySelector('.sidebar');
    return document.documentElement.dataset.theme === expected
      && body.backgroundColor === colors.canvas
      && body.color === colors.text
      && sidebar && getComputedStyle(sidebar).backgroundColor === colors.sidebar;
  }, { expected: theme, colors: themeColors[theme] });
}

export async function visible(locator) { await locator.waitFor({ state: 'visible' }); }
export async function hidden(locator) { await locator.waitFor({ state: 'hidden' }); }
export async function setSwitch(locator, checked) {
  await visible(locator);
  assert.equal(await locator.getAttribute('role'), 'switch', 'Binary preferences use the shared switch');
  const current = await locator.getAttribute('aria-checked');
  assert.ok(current === 'true' || current === 'false', 'A switch exposes its current state');
  if (current !== String(checked)) await locator.click();
  await eventually(() => locator.getAttribute('aria-checked'), value => value === String(checked), 'Switch state updates');
}
export async function openCardMenu(card) {
  const menu = card.locator('details.card-menu');
  assert.equal(await menu.count(), 1, 'Each recording card owns exactly one action menu');
  assert.equal(await menu.getAttribute('name'), 'streamcap-task-menu', 'Card menus share an exclusive group');
  const trigger = menu.getByRole('button', { name: '更多操作', exact: true });
  await visible(trigger);
  if (!await menu.evaluate(node => node.open)) await trigger.click();
  await eventually(() => menu.evaluate(node => node.open), Boolean, 'Card action menu opens');
  await visible(menu.locator('.card-menu-panel'));
  await eventually(() => menu.evaluate(node => {
    const bounds = node.querySelector('.card-menu-panel').getBoundingClientRect();
    const viewport = node.closest('#main-content').getBoundingClientRect();
    return bounds.top >= viewport.top - 1 && bounds.bottom <= viewport.bottom + 1;
  }), Boolean, 'The entire action menu fits inside the scroll viewport');
  return menu;
}
export async function closeCardMenu(menu) {
  const trigger = menu.getByRole('button', { name: '更多操作', exact: true });
  await trigger.press('Escape');
  await eventually(() => menu.evaluate(node => node.open), value => !value, 'Escape closes the card action menu');
  assert.equal(await trigger.evaluate(node => node === document.activeElement), true, 'Escape restores menu trigger focus');
}
export async function cardMenuAction(card, name) {
  const menu = await openCardMenu(card);
  await menu.locator('.card-menu-panel').getByRole('button', { name, exact: true }).click();
  await eventually(() => menu.evaluate(node => node.open), value => !value, 'Selecting an action closes its menu');
}
export async function assertSingleLineActions(buttons) {
  const labels = await buttons.evaluateAll(nodes => nodes.flatMap(button => {
    const bounds = button.getBoundingClientRect(), walker = document.createTreeWalker(button, NodeFilter.SHOW_TEXT);
    const rectangles = [];
    let text = '', node;
    while ((node = walker.nextNode())) {
      if (!node.textContent.trim() || node.parentElement.closest('svg,[aria-hidden="true"],[hidden],.sr-only')) continue;
      const range = document.createRange();
      range.selectNodeContents(node);
      rectangles.push(...[...range.getClientRects()].filter(rect => rect.width > 0 && rect.height > 0));
      text += node.textContent.trim();
    }
    if (!text || !rectangles.length) return [];
    const labelStyle = getComputedStyle(button.querySelector('span:not([aria-hidden="true"])') || button);
    const lineHeight = parseFloat(labelStyle.lineHeight) || parseFloat(labelStyle.fontSize) * 1.5;
    return [{
      text, whiteSpace: labelStyle.whiteSpace, lineHeight,
      textHeight: Math.max(...rectangles.map(rect => rect.bottom)) - Math.min(...rectangles.map(rect => rect.top)),
      left: Math.min(...rectangles.map(rect => rect.left)), right: Math.max(...rectangles.map(rect => rect.right)),
      buttonLeft: bounds.left, buttonRight: bounds.right,
    }];
  }));
  assert.ok(labels.length > 0, 'The action area contains measurable text labels');
  for (const label of labels) {
    assert.ok(label.textHeight <= label.lineHeight + 2, 'Short action text must remain on one line: ' + JSON.stringify(label));
    assert.ok(label.left >= label.buttonLeft - 1 && label.right <= label.buttonRight + 1, 'Action text must not spill beyond its button: ' + JSON.stringify(label));
  }
  return labels;
}
export async function assertDialogStyles(modal, theme, requiredControls = []) {
  await visible(modal);
  const frame = await modal.evaluate(node => {
    const box = node.getBoundingClientRect(), style = getComputedStyle(node);
    const body = node.querySelector('.modal-body');
    return {
      left: box.left, right: box.right, top: box.top, bottom: box.bottom,
      width: box.width, height: box.height, viewportWidth: innerWidth, viewportHeight: innerHeight,
      background: style.backgroundColor, color: style.color, backdrop: style.backdropFilter,
      border: style.borderTopStyle, borderWidth: parseFloat(style.borderTopWidth),
      overflow: node.scrollWidth > node.clientWidth + 2 || !!body && body.scrollWidth > body.clientWidth + 2,
    };
  });
  assert.ok(frame.width >= 300 && frame.height > 80, 'The dialog has a usable measured surface');
  assert.ok(frame.left >= 0 && frame.right <= frame.viewportWidth + 1 && frame.top >= 0 && frame.bottom <= frame.viewportHeight + 1, 'The dialog stays inside the viewport');
  assert.equal(frame.background, themeColors[theme].overlay, 'Dialog uses the high-opacity glass overlay for readable content');
  assert.equal(frame.color, themeColors[theme].text);
  assert.match(frame.backdrop, /blur\(18px\)/, 'Dialog shares the bounded glass material');
  assert.equal(frame.border, 'solid');
  assert.ok(frame.borderWidth >= 1, 'Native dialog boundary remains visible');
  assert.equal(frame.overflow, false, 'Dialog controls must not cause horizontal scrolling');
  const fields = modal.locator('input:not([type="hidden"]):not([type="checkbox"]):not([type="radio"]):not([type="range"]), textarea, select');
  const controls = [];
  for (const field of await fields.all()) {
    if (!await field.isVisible()) continue;
    await field.scrollIntoViewIfNeeded();
    const control = await field.evaluate(node => {
      const style = getComputedStyle(node), box = node.getBoundingClientRect();
      const kind = node.tagName.toLowerCase();
      const libraryClass = kind === 'textarea' ? 'ui-textarea' : kind === 'select' ? 'ui-select' : 'ui-input';
      return {
        kind, libraryClass, hasLibraryClass: node.classList.contains(libraryClass),
        width: box.width, height: box.height, left: box.left, right: box.right,
        viewportWidth: innerWidth, fontSize: parseFloat(style.fontSize),
        border: style.borderTopStyle, borderWidth: parseFloat(style.borderTopWidth),
        borderColor: style.borderTopColor, background: style.backgroundColor,
      };
    });
    assert.equal(control.hasLibraryClass, true, `A visible ${control.kind} must use ${control.libraryClass}`);
    assert.ok(control.width >= 80 && control.height >= (control.kind === 'textarea' ? 64 : 32), 'Form controls retain usable dimensions: ' + JSON.stringify(control));
    assert.ok(control.fontSize >= 13, 'Form control text remains readable');
    assert.ok(control.left >= 0 && control.right <= control.viewportWidth + 1, 'Visible form controls stay inside the window');
    assert.equal(control.border, 'solid');
    assert.ok(control.borderWidth >= 1, 'Compiled library fields retain a visible border');
    assert.ok(!/^(?:transparent|rgba\([^)]*,\s*0(?:\.0+)?\))$/.test(control.borderColor) && !/\/\s*0(?:\.0+)?%?\s*\)$/.test(control.borderColor), 'Field borders must not be transparent');
    assert.notEqual(control.borderColor, control.background, 'Field border contrasts with its background');
    controls.push(control);
  }
  for (const kind of requiredControls) assert.ok(controls.some(control => control.kind === kind), `The dialog exposes its expected ${kind} control`);
  const buttons = modal.locator('button:visible');
  assert.ok(await buttons.count() > 0, 'Every dialog retains a visible action');
  for (const button of await buttons.all()) {
    const styled = await button.evaluate(node => {
      const box = node.getBoundingClientRect();
      return node.classList.contains('ui-btn') && box.height >= 24 && box.width >= 24;
    });
    assert.equal(styled, true, 'Visible dialog actions use sized library buttons');
  }
  await modal.locator('.modal-body').evaluate(node => { node.scrollTop = 0; });
  return { frame, controls };
}
export async function go(page, base, path) {
  await page.goto(base + path, { waitUntil: 'domcontentloaded' });
  await visible(page.locator('.app-shell'));
  await visible(page.locator('.page h1'));
  await hidden(page.locator('.skeleton-grid, .overview-loading'));
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
    const surfaces = {
      '/home': '.home-page .overview-main',
      '/recordings': '.page .task-workspace',
      '/storage': '.page .file-manager',
      '/settings': '.page .settings-content',
      '/about': '.page .about-content',
    };
    const surfaceSelector = surfaces[location.pathname];
    const panel = surfaceSelector && document.querySelector(surfaceSelector);
    const panelStyle = panel && getComputedStyle(panel);
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
      overflow,
      surface: panelStyle && {
        selector: surfaceSelector,
        ...rect(surfaceSelector),
        background: panelStyle.backgroundColor,
        border: panelStyle.borderTopStyle,
        borderWidth: parseFloat(panelStyle.borderTopWidth),
        radius: parseFloat(panelStyle.borderTopLeftRadius),
        backdrop: panelStyle.backdropFilter,
      },
      baseFontSize: parseFloat(getComputedStyle(document.documentElement).fontSize),
      bodyFontSize: parseFloat(getComputedStyle(document.body).fontSize),
      headingFontSize: document.querySelector('.page h1') && parseFloat(getComputedStyle(document.querySelector('.page h1')).fontSize),
      dialogs: document.querySelectorAll('dialog[open]').length,
    };
  });
}
