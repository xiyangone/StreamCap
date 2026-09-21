import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createHarness, eventually, go, hidden, inspectLayout, streamPreviewFixture, visible, waitTheme } from './ui-fixtures.mjs';

const h = await createHarness('interaction');
const { page, state } = h;
const steps = [];
const card = (id) => page.locator('article[data-rec-id="' + id + '"]');
const dialog = (name) => page.getByRole('dialog', { name, exact: true });
const requests = (method, path) => state.requests.filter((r) => r.method === method && r.path === path);
const record = (id) => state.recordings.find((r) => r.recId === id);
const waitState = (read, predicate, message) => eventually(read, predicate, message);
const navigate = async (name) => { await page.getByRole('navigation', { name: '主导航' }).getByRole('link', { name }).click(); };
const fetchFixture = route => {
  const url = new URL(route.request().url());
  // Route.fetch bypasses context routing; never contact the application's default port.
  return route.fetch({ url: h.base + url.pathname + url.search, maxRedirects: 0, timeout: 5000 });
};
let failure;
async function step(name, action) {
  await action();
  assert.deepEqual(h.runtimeErrors, [], '浏览器出现未处理运行时错误');
  steps.push(name);
  console.log('PASS ' + name);
}

try {
  await step('预览夹具在准备中取消、流中取消和正常结束后均归零', async () => {
    for (const phase of ['already-closed', 'preparing', 'streaming', 'complete']) {
      const counters = { activePreviews: 0, maxActivePreviews: 0, previewStarts: [] };
      const response = new EventEmitter();
      const chunks = [];
      response.destroyed = phase === 'already-closed';
      response.writeHead = () => { assert.equal(response.destroyed, false); };
      response.write = chunk => { assert.equal(response.destroyed, false); chunks.push(chunk); };
      const close = () => { response.destroyed = true; response.emit('close'); };
      response.end = close;
      let release, prepared = false;
      const bytes = new Promise(resolve => { release = resolve; });
      const pending = streamPreviewFixture(response, () => { prepared = true; return bytes; }, counters, { path: '/api/media/transcode', start: 0 });
      try {
        assert.equal(prepared, phase !== 'already-closed');
        if (phase === 'preparing') close();
        release(Buffer.alloc(8192));
        if (phase === 'streaming') {
          await waitState(() => counters.activePreviews, n => n === 1, '流式夹具进入活动状态');
          close();
        }
        await pending;
        close();
        assert.equal(counters.activePreviews, 0, phase + ' 不得泄漏或重复递减');
        const started = phase === 'streaming' || phase === 'complete';
        assert.equal(counters.maxActivePreviews, Number(started));
        assert.equal(counters.previewStarts.length, Number(started));
        assert.equal(chunks.length > 0, started);
      } finally {
        close(); release(Buffer.alloc(0)); await pending;
      }
    }
  });
  await step('真实 WASM 启动、初始任务与 SSE 连接', async () => {
    await go(page, h.base, '/home');
    await waitState(() => page.locator('.recording-card').count(), (n) => n === 6, '初始任务数量');
    await visible(page.getByText('本地实时更新', { exact: true }));
    const layout = await inspectLayout(page);
    assert.equal(layout.dialogs, 0); assert.ok(layout.glass.includes('blur'));
    await h.shot('01-home-light');
  });
  await step('慢于两秒的文件列表仍能加载，且只保留一个在途请求', async () => {
    await navigate('录制任务');
    const pattern = '**/api/recordings/fixture-1/files';
    let active = 0, maximum = 0, completed = 0, count = 0;
    const handler = async route => {
      count++; active++; maximum = Math.max(maximum, active);
      try {
        const response = await fetchFixture(route);
        await new Promise(resolve => setTimeout(resolve, 2600));
        await route.fulfill({ response }); completed++;
      } finally { active--; }
    };
    await page.route(pattern, handler);
    const modal = dialog('录制预览');
    try {
      await card('fixture-1').getByRole('button', { name: '预览录制文件' }).click();
      await visible(modal.locator('.loading-state'));
      await waitState(() => modal.locator('.preview-file').count(), n => n > 0, '慢响应首次预览列表');
      await hidden(modal.locator('.loading-state'));
      await waitState(() => completed, n => n >= 2, '完成后才安排下一次刷新');
      assert.equal(maximum, 1, '同一预览目标不得并发刷新');
      await h.shot('regression-slow-preview');
      await modal.getByRole('button', { name: '关闭对话框', exact: true }).click();
      const closedCount = count;
      await new Promise(resolve => setTimeout(resolve, 2250));
      assert.equal(count, closedCount, '关闭弹窗后不得继续刷新');
    } finally {
      if (await modal.isVisible()) await page.keyboard.press('Escape');
      await page.unroute(pattern, handler);
    }
  });
  await step('旧列表快照不能恢复 SSE 已删除任务或回退更新', async () => {
    const original = structuredClone(state.recordings);
    const pattern = '**/api/recordings';
    let release, captured;
    const gate = new Promise(resolve => { release = resolve; });
    const ready = new Promise(resolve => { captured = resolve; });
    const handler = async route => {
      const response = await fetchFixture(route);
      captured(); await gate; await route.fulfill({ response });
    };
    await page.route(pattern, handler);
    try {
      const completed = page.waitForResponse(response => new URL(response.url()).pathname === '/api/recordings' && response.status() === 200);
      h.emit('resync', {}); await ready;
      state.recordings = state.recordings.filter(rec => rec.recId !== 'fixture-2');
      h.emit('delete', ['fixture-2']);
      record('fixture-3').streamerName = '更新不能被旧快照覆盖'; h.emit('update', record('fixture-3'));
      await waitState(() => card('fixture-2').count(), n => n === 0, 'SSE 删除已应用');
      await visible(card('fixture-3').getByRole('button', { name: '更新不能被旧快照覆盖', exact: true }));
      release(); await completed;
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await card('fixture-2').count(), 0, '删除任务被旧快照恢复');
      await visible(card('fixture-3').getByRole('button', { name: '更新不能被旧快照覆盖', exact: true }));
      await h.shot('regression-stale-recording-snapshot');
    } finally {
      release(); await page.unroute(pattern, handler);
      state.recordings = original; h.emit('resync', {});
      await waitState(() => page.locator('.recording-card').count(), n => n === original.length, '恢复隔离夹具');
    }
  });
  await step('旧媒体队列快照不能回退 SSE 已完成状态', async () => {
    const original = structuredClone(state.mediaJobs);
    await card('fixture-1').getByRole('button', { name: '预览录制文件' }).click();
    const modal = dialog('录制预览');
    await visible(modal.locator('.media-preview'));
    const source = await modal.locator('.media-preview').getAttribute('data-path');
    const job = { id: 'snapshot-regression', taskId: 'fixture-1', source, output: source + '.mp4', state: 'waiting', sourceRemoved: false, deleteOriginal: false, message: '专项任务等待处理' };
    state.mediaJobs.push(job); h.emit('mediaJob', job);
    await visible(modal.getByText('专项任务等待处理', { exact: true }));
    const pattern = '**/api/media/jobs';
    let release, captured;
    const gate = new Promise(resolve => { release = resolve; });
    const ready = new Promise(resolve => { captured = resolve; });
    const handler = async route => {
      const response = await fetchFixture(route); captured(); await gate; await route.fulfill({ response });
    };
    await page.route(pattern, handler);
    try {
      const completed = page.waitForResponse(response => new URL(response.url()).pathname === '/api/media/jobs' && response.status() === 200);
      h.emit('resync', {}); await ready;
      job.state = 'complete'; job.message = '专项任务已经完成'; h.emit('mediaJob', job);
      await visible(modal.getByText('专项任务已经完成', { exact: true }));
      release(); await completed;
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await visible(modal.getByText('专项任务已经完成', { exact: true }));
      assert.equal(await modal.getByText('专项任务等待处理', { exact: true }).count(), 0);
      await h.shot('regression-stale-job-snapshot');
    } finally {
      release(); await page.unroute(pattern, handler);
      await modal.getByRole('button', { name: '关闭对话框', exact: true }).click();
      state.mediaJobs = original; h.emit('resync', {});
      await navigate('总览');
    }
  });
  await step('明暗主题即时切换与样式实际生效', async () => {
    await page.getByRole('button', { name: '切换到深色' }).click();
    await waitTheme(page, 'dark');
    const bg = await page.locator('body').evaluate((e) => getComputedStyle(e).backgroundColor);
    assert.equal(bg, 'rgb(16, 23, 37)');
    await h.shot('02-home-dark');
    await page.getByRole('button', { name: '切换到浅色' }).click();
    await waitTheme(page, 'light');
  });
  await step('搜索、平台筛选、空态与网格/列表', async () => {
    await navigate('录制任务');
    await page.getByRole('textbox', { name: '搜索直播间' }).fill('山野');
    await waitState(() => page.locator('.recording-card').count(), (n) => n === 1, '搜索结果');
    await page.getByRole('textbox', { name: '搜索直播间' }).fill('不存在的直播间');
    await visible(page.getByText('没有匹配的直播间', { exact: true }));
    await page.getByRole('button', { name: '清除筛选' }).click();
    await page.getByRole('combobox', { name: '筛选平台' }).selectOption('bilibili');
    await waitState(() => page.locator('.recording-card').count(), (n) => n === 2, '平台筛选');
    await page.getByRole('combobox', { name: '筛选平台' }).selectOption('');
    await page.getByRole('button', { name: '列表视图', exact: true }).click();
    await visible(page.locator('.cards-grid.list-view'));
    await h.shot('03-recordings-list');
    await page.getByRole('button', { name: '网格视图', exact: true }).click();
  });
  await step('弹窗焦点隔离、Escape 与恢复焦点', async () => {
    const trigger = page.getByRole('button', { name: '添加直播间', exact: true });
    await trigger.click();
    const modal = dialog('添加直播间'); await visible(modal);
    assert.equal(await modal.evaluate((el) => el.contains(document.activeElement)), true);
    for (let i = 0; i < 8; i++) await page.keyboard.press('Tab');
    assert.equal(await modal.evaluate((el) => el.contains(document.activeElement)), true);
    await page.keyboard.press('Escape'); await hidden(modal);
    assert.equal(await trigger.evaluate((el) => el === document.activeElement), true);
  });
  await step('添加校验、逐行名称与清晰度、无额外任务写入', async () => {
    await page.getByRole('button', { name: '添加直播间', exact: true }).click();
    const modal = dialog('添加直播间');
    const urls = modal.getByRole('textbox', { name: /直播间地址/ });
    const before = requests('POST', '/api/recordings').length;
    await urls.fill('file:///private');
    await modal.getByRole('button', { name: '添加直播间', exact: true }).click();
    await visible(modal.getByRole('alert'));
    assert.equal(requests('POST', '/api/recordings').length, before);
    await urls.fill('2,https://custom.example.invalid/one,测试甲\nhttps://custom.example.invalid/two,测试乙\nhttps://custom.example.invalid/one,不应覆盖甲');
    await h.shot('04-add-dialog');
    await modal.getByRole('button', { name: '添加直播间', exact: true }).click();
    await hidden(modal);
    await waitState(() => state.recordings.length, (n) => n === 8, '添加两条任务');
    await visible(page.getByText('已添加 2 个直播间，跳过 1 个重复地址',{exact:true}));
    const body = requests('POST', '/api/recordings').at(-1).body;
    assert.equal(body.items.length,3,'send duplicate lines to the authoritative backend for counting');
    assert.equal(body.items[0].quality, 'HD'); assert.equal(body.items[1].streamerName, '测试乙');
    assert.equal(record('fixture-7').monitorStatus,true);assert.equal(record('fixture-8').monitorStatus,true);
    await visible(card('fixture-8').getByText('自动监控，开播即录',{exact:true}));
    assert.equal(requests('POST','/api/recordings/fixture-8/start').length,0,'添加不依赖手动开录请求');
    const unchanged = structuredClone(state.recordings);
    await page.getByRole('button', { name: '添加直播间', exact: true }).click();
    await urls.fill('https://custom.example.invalid/one\nhttps://custom.example.invalid/one');
    await modal.getByRole('button', { name: '添加直播间', exact: true }).click();
    await hidden(modal);
    await visible(page.getByText('已添加 0 个直播间，跳过 2 个重复地址',{exact:true}));
    assert.deepEqual(state.recordings,unchanged,'all-duplicate imports must not mutate existing records');
  });
  await step('单项编辑、字段契约和实时卡片更新', async () => {
    await card('fixture-3').getByRole('button', { name: '编辑任务' }).click();
    const modal = dialog('编辑直播间');
    await modal.getByRole('textbox', { name: '主播名称' }).fill('小宇 · 已编辑');
    await modal.getByRole('combobox', { name: '清晰度', exact: true }).selectOption('HD');
    await modal.getByRole('combobox', { name: '录制格式', exact: true }).selectOption('MKV');
    await h.shot('05-edit-dialog');
    await modal.getByRole('button', { name: '保存修改', exact: true }).click();
    await hidden(modal);
    await visible(card('fixture-3').getByText('小宇 · 已编辑', { exact: true }));
    assert.equal(record('fixture-3').quality, 'HD'); assert.equal(record('fixture-3').recordFormat, 'MKV');
    assert.ok(!record('fixture-3').inheritedFields.includes('quality'));
  });
  await step('批量编辑仅限选中 ID，保持原值不静默提交', async () => {
    await card('fixture-3').getByRole('checkbox').check();
    await card('fixture-5').getByRole('checkbox').check();
    const untouched = structuredClone(state.recordings.filter((r) => !['fixture-3', 'fixture-5'].includes(r.recId)));
    await page.getByRole('button', { name: '批量编辑', exact: true }).click();
    const modal = dialog('批量编辑任务');
    const before = requests('POST', '/api/recordings/batch-edit').length;
    await modal.getByRole('button', { name: '应用到所选任务' }).click();
    await visible(modal.getByText('请至少选择一项需要修改的设置', { exact: true }));
    assert.equal(requests('POST', '/api/recordings/batch-edit').length, before);
    await modal.getByRole('combobox', { name: '清晰度', exact: true }).selectOption('SD');
    await h.shot('06-batch-dialog');
    await modal.getByRole('button', { name: '应用到所选任务' }).click();
    await hidden(modal);
    assert.deepEqual(requests('POST', '/api/recordings/batch-edit').at(-1).body.recIds.sort(), ['fixture-3', 'fixture-5']);
    assert.deepEqual(state.recordings.filter((r) => !['fixture-3', 'fixture-5'].includes(r.recId)), untouched);
    assert.equal(record('fixture-3').recordFormat, 'MKV');
    await page.getByRole('button', { name: '取消选择' }).click();
  });
  await step('恢复跟随全局，覆盖项保持独立', async () => {
    await card('fixture-3').getByRole('button', { name: '编辑任务' }).click();
    const modal = dialog('编辑直播间');
    await modal.getByRole('combobox', { name: '清晰度', exact: true }).selectOption('__global');
    await modal.getByRole('combobox', { name: '录制格式', exact: true }).selectOption('__global');
    await modal.getByRole('button', { name: '保存修改', exact: true }).click(); await hidden(modal);
    await navigate('偏好设置');
    await page.getByRole('combobox', { name: '默认清晰度', exact: true }).selectOption('UHD');
    await page.getByRole('combobox', { name: '默认录制格式', exact: true }).selectOption('MP4');
    await page.locator('.settings-savebar').getByRole('button', { name: '保存修改' }).click();
    await waitState(() => state.userConfig.record_quality, (v) => v === 'UHD', '全局清晰度写入');
    await hidden(page.locator('.settings-savebar'));
    assert.equal(record('fixture-3').quality, 'UHD'); assert.equal(record('fixture-3').recordFormat, 'MP4');
    assert.equal(record('fixture-5').quality, 'SD');
    await h.shot('07-settings-recording');
  });
  await step('设置数值校验和失败反馈不伪报成功', async () => {
    await page.getByRole('button', { name: '网络与检测', exact: true }).click();
    const interval = page.getByRole('textbox', { name: /检测间隔/ });
    await interval.fill('0');
    const before = requests('PUT', '/api/settings').length;
    await page.locator('.settings-savebar').getByRole('button', { name: '保存修改' }).click();
    await visible(page.getByText(/检测间隔应为/)); assert.equal(requests('PUT', '/api/settings').length, before);
    await interval.fill('120');
    h.failNext('PUT', '/api/settings', '模拟磁盘不可写');
    await page.locator('.settings-savebar').getByRole('button', { name: '保存修改' }).click();
    await visible(page.getByText('模拟磁盘不可写', { exact: true }));
    assert.notEqual(state.userConfig.loop_time_seconds, '120');
    await page.locator('.settings-savebar').getByRole('button', { name: '保存修改' }).click();
    await waitState(() => state.userConfig.loop_time_seconds, (v) => v === '120', '重试保存');
    await hidden(page.locator('.settings-savebar'));
  });
  await step('录制结束转 MP4 开关按显式选择保存，不改变 TS 与检测间隔', async () => {
    await page.getByRole('button',{name:'录制与存储',exact:true}).click();
    const interval=state.userConfig.loop_time_seconds,format=state.userConfig.video_format;
    const toggle=page.getByRole('switch',{name:'录制结束转 MP4',exact:true});await toggle.check();
    await page.locator('.settings-savebar').getByRole('button',{name:'保存修改'}).click();
    await waitState(()=>state.userConfig.convert_to_mp4,v=>v===true,'启用转换设置');await hidden(page.locator('.settings-savebar'));
    assert.equal(state.userConfig.loop_time_seconds,interval);assert.equal(state.userConfig.video_format,format);
    await toggle.uncheck();await page.locator('.settings-savebar').getByRole('button',{name:'保存修改'}).click();
    await waitState(()=>state.userConfig.convert_to_mp4,v=>v===false,'关闭转换设置');await hidden(page.locator('.settings-savebar'));
  });
  await step('系统主题随系统实时变化，强调色与持久化', async () => {
    await page.getByRole('button', { name: '外观与窗口', exact: true }).click();
    await page.getByRole('button', { name: '跟随系统', exact: true }).click();
    await waitState(() => state.userConfig.theme_mode, (v) => v === 'system', '主题保存');
    await page.emulateMedia({ colorScheme: 'dark' });
    await waitTheme(page, 'dark');
    await visible(page.getByRole('button', { name: '切换到浅色' }));
    assert.equal(await page.evaluate(()=>typeof window.__TAURI__), 'undefined');
    await page.getByRole('button', { name: '鸢尾紫' }).click();
    await page.waitForFunction(() => document.documentElement.dataset.accent === 'purple');
    await h.shot('08-settings-dark-purple');
    await page.emulateMedia({ colorScheme: 'light' });
    await waitTheme(page, 'light');
    await page.getByRole('button', { name: '晴空蓝' }).click();
    await page.getByRole('button', { name: '浅色', exact: true }).click();
    await waitState(() => state.userConfig.theme_mode, (v) => v === 'light', '浅色恢复');
  });
  await step('窗口一体化、原生按钮、关闭取消与托盘偏好',async()=>{
    const bounds=await page.locator('.app-shell').boundingBox();assert.equal(bounds.x,0);assert.equal(bounds.y,0);
    assert.equal(await page.locator('.sidebar').evaluate(el=>getComputedStyle(el).borderRadius),'0px');
    assert.equal(await page.locator('.device-avatar').count(),0);
    assert.equal(await page.getByRole('group',{name:'窗口控制'}).getByRole('button').count(),3);
    await page.getByRole('button',{name:'最大化窗口',exact:true}).click();await visible(page.getByRole('button',{name:'还原窗口',exact:true}));
    await page.getByRole('button',{name:'还原窗口',exact:true}).click();await visible(page.getByRole('button',{name:'最大化窗口',exact:true}));
    await page.getByRole('button',{name:'最小化窗口',exact:true}).click();await waitState(()=>state.native.minimized,Boolean,'原生最小化调用');await h.restoreNative();
    await navigate('偏好设置');await page.getByRole('button',{name:'外观与窗口',exact:true}).click();
    const preference=page.getByRole('combobox',{name:'关闭窗口时',exact:true});await visible(preference);assert.equal(await preference.inputValue(),'ask');
    assert.equal(await page.locator('.titlebar-drag-zone').getAttribute('data-tauri-drag-region'),'deep');
    const close=dialog('关闭 StreamCap');await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await visible(close);
    await page.reload({waitUntil:'domcontentloaded'});await visible(close);
    await close.getByRole('button',{name:'取消',exact:true}).click();await hidden(close);assert.equal(state.native.closing,false);assert.equal(state.native.visible,true);
    await page.getByRole('button',{name:'外观与窗口',exact:true}).click();
    await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await visible(close);await page.keyboard.press('Escape');await hidden(close);assert.equal(state.native.closing,false);
    await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await visible(close);await close.getByRole('checkbox',{name:'记住我的选择'}).check();state.native.failRemember=true;
    await close.getByRole('button',{name:/最小化到托盘/}).click();await visible(close.getByRole('alert'));assert.equal(state.native.visible,true);assert.equal(state.userConfig.close_action,undefined);
    await close.getByRole('button',{name:/最小化到托盘/}).click();await hidden(close);assert.equal(state.native.visible,false);assert.equal(state.native.closing,false);assert.equal(state.userConfig.close_action,'tray');
    await h.restoreNative();await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await waitState(()=>state.native.visible,v=>!v,'记住托盘后直接隐藏');await hidden(close);await h.restoreNative();
    await preference.selectOption('ask');await waitState(()=>state.userConfig.close_action,v=>v==='ask','恢复每次询问');
    state.native.trayAvailable=false;await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await visible(close);assert.equal(await close.getByRole('button',{name:/最小化到托盘/}).isDisabled(),true);
    await close.getByRole('button',{name:'取消',exact:true}).click();await hidden(close);state.native.trayAvailable=true;
    await h.shot('window-preferences');
  });
  await step('登录信息按平台局部保存，扫码仅在隔离服务模拟', async () => {
    await page.getByRole('button', { name: '平台登录', exact: true }).click();
    const input = page.getByRole('textbox', { name: '平台 Cookie' });
    await visible(input);
    const untouched = state.cookies.bilibili;
    await input.fill('fixture-edited-not-a-credential');
    await page.getByRole('button', { name: '保存登录信息' }).click();
    await waitState(() => state.cookies.douyin, (v) => v === 'fixture-edited-not-a-credential', 'Cookie 局部保存');
    assert.deepEqual(requests('PUT', '/api/cookies').at(-1).body.cookies, { douyin: 'fixture-edited-not-a-credential' });
    assert.equal(state.cookies.bilibili, untouched);
    await page.getByRole('textbox', { name: '搜索平台' }).fill('快手');
    await page.locator('.cookie-platform-list').getByRole('button', { name: '快手直播', exact: true }).click();
    await page.getByRole('button', { name: '扫码登录' }).click();
    const modal = dialog('快手扫码登录');
    await visible(modal.getByText('二维码有效期：120 秒', { exact: true }));
    await modal.getByRole('button', { name: '关闭', exact: true }).click(); await hidden(modal);
    await waitState(() => state.qrCancelled, (n) => n >= 1, '关闭取消模拟扫码');
    state.qrPhase = 'scanned';
    await page.getByRole('button', { name: '扫码登录' }).click();
    await visible(modal.getByText('等待手机确认', { exact: true }));
    assert.equal(await modal.getByRole('img', {name:'快手登录二维码'}).count(),0);
    state.qrPhase = 'verifying';
    await visible(modal.getByText('手机已确认', { exact: true }));
    state.qrPhase = 'error';
    await visible(modal.getByText('登录未完成', { exact: true }));
    await visible(modal.getByText(/手机已确认，但直播站未返回/));
    assert.equal(await modal.getByRole('img', {name:'快手登录二维码'}).count(),0);
    assert.equal(await modal.getByRole('button', {name:'保存登录信息'}).count(),0);
    assert.equal(state.cookies.kuaishou,undefined);
    await h.shot('qr-verification-failure');
    state.qrPhase = 'success';
    await modal.getByRole('button', { name: '重新获取' }).click();
    await visible(modal.getByText('已验证账号：测试快手账号', { exact: true }));
    assert.equal(state.cookies.kuaishou, undefined, '验证成功也必须等待明确保存');
    await visible(modal.getByText('登录信息尚未保存，点击下方按钮完成登录。',{exact:true}));
    assert.equal(await modal.locator('.qr-code-container').count(),0,'终态不保留二维码边框');
    h.failNext('PUT','/api/cookies','模拟登录信息保存失败',500);
    await modal.getByRole('button',{name:'保存登录信息',exact:true}).click();
    await visible(modal.getByRole('alert'));assert.equal(state.cookies.kuaishou,undefined);
    await modal.getByRole('button',{name:'保存登录信息',exact:true}).click();
    await hidden(modal);
    await waitState(() => state.cookies.kuaishou, (v) => v === 'fixture-qr-not-a-credential', '弹窗中明确保存模拟扫码结果');
    assert.equal(state.cookies.bilibili,untouched);
    await visible(page.locator('.account-status'));
    await h.shot('qr-account-saved');
  });
  await step('转换、清理与时间字幕为真实可保存开关，自动化不再显示占位卡片', async () => {
    await page.getByRole('button', {name:'录制与存储',exact:true}).click();
    await page.getByRole('switch',{name:'录制结束转 MP4',exact:true}).check();
    await page.getByRole('switch',{name:'转换成功后清理源 TS',exact:true}).check();
    await page.getByRole('switch',{name:'生成时间字幕',exact:true}).check();
    await page.locator('.settings-savebar').getByRole('button',{name:'保存修改',exact:true}).click();
    await waitState(()=>state.userConfig.delete_original,Boolean,'清理设置保存');assert.equal(state.userConfig.generate_time_subtitle_file,true);
    await page.getByRole('button', { name: '自动化与通知', exact: true }).click();
    assert.equal(await page.locator('.unavailable-row').count(),0);
    await visible(page.getByRole('switch',{name:'定时关机',exact:true}));
    await visible(page.getByRole('switch',{name:'录制后执行脚本',exact:true}));
    await visible(page.getByRole('switch',{name:'系统通知',exact:true}));
    await h.shot('automation-and-notifications');
  });
  await step('监控、录制、停止与失败反馈', async () => {
    await navigate('录制任务');
    await card('fixture-5').getByRole('button', { name: '开启监控', exact: true }).click();
    await waitState(() => record('fixture-5').monitorStatus, Boolean, '监控启用');
    await card('fixture-5').getByRole('button', { name: '单次录制', exact: true }).click();
    await waitState(() => record('fixture-5').isRecording, Boolean, '开始录制');
    assert.equal(await card('fixture-5').getByRole('button', { name: '编辑任务' }).isDisabled(), true);
    assert.equal(await card('fixture-5').getByRole('button', { name: '删除任务' }).isDisabled(), true);
    await card('fixture-5').getByRole('button', { name: '停止录制', exact: true }).click();
    await waitState(() => record('fixture-5').isRecording, (v) => !v, '停止录制');
    h.failNext('POST', '/api/recordings/fixture-5/start', '模拟 FFmpeg 启动失败');
    await card('fixture-5').getByRole('button', { name: '单次录制', exact: true }).click();
    await visible(page.getByText('模拟 FFmpeg 启动失败', { exact: true }));
    assert.equal(record('fixture-5').isRecording, false);
  });
  await step('快手待验证不冒充未开播或录制失败，并保留手动验证入口', async () => {
    const original=structuredClone(record('fixture-5'));
    Object.assign(record('fixture-5'),{platformKey:'kuaishou',platform:'快手直播',checkError:'快手需要完成滑块验证（400002）',verificationRequired:true,isLive:true,isRecording:false,recordingError:null});h.emit('update',record('fixture-5'));
    await visible(card('fixture-5').getByText('待验证',{exact:true}));
    await visible(card('fixture-5').getByText('请在快手窗口完成验证',{exact:true}));
    const attentionCount=state.recordings.filter(r=>!r.isRecording&&(r.verificationRequired||r.checkError||r.recordingError)).length;
    const attentionTab=page.getByRole('button',{name:/^需关注/});
    assert.ok((await attentionTab.innerText()).includes(String(attentionCount)));
    await attentionTab.click();assert.equal(await page.locator('.recording-card').count(),attentionCount);
    await visible(card('fixture-5'));
    await navigate('总览');
    assert.equal(await page.locator('.stat-item').filter({hasText:'需关注'}).locator('strong').innerText(),String(attentionCount));
    await visible(page.getByText('有需要处理的任务，请查看下方提示。',{exact:true}));
    assert.equal(await page.locator('.recording-card').first().getAttribute('data-rec-id'),'fixture-5');
    await navigate('录制任务');
    assert.equal(await card('fixture-5').getByRole('button',{name:'单次录制',exact:true}).isDisabled(),true);
    assert.equal(await card('fixture-5').getByRole('button',{name:'检测直播状态',exact:true}).isDisabled(),true);
    assert.equal(await card('fixture-5').locator('.card-recording-error').count(),0);
    await card('fixture-5').getByRole('button',{name:'重新验证',exact:true}).click();
    await waitState(()=>state.native.verification,v=>v?.active===true&&v.recId==='fixture-5','只打开当前快手验证入口');
    await h.shot('kuaishou-verification-required');
    Object.assign(record('fixture-5'),original);h.emit('update',record('fixture-5'));
    await hidden(card('fixture-5').getByRole('button',{name:'重新验证',exact:true}));
  });
  await step('限流、页面检查和登录提示不再冒充验证码', async () => {
    const original=structuredClone(record('fixture-5'));
    for(const [access,label,inspect] of [['cooldown','平台冷却中',false],['pageCheck','页面待检查',true],['loginRequired','需要登录',false],['loginPrompt','登录提示',true]]){
      Object.assign(record('fixture-5'),{platformKey:'kuaishou',platform:'快手直播',accessState:access,checkError:'页面暂不可读',verificationRequired:false,isLive:true,isRecording:false,recordingError:null});h.emit('update',record('fixture-5'));
      await visible(card('fixture-5').getByText(label,{exact:true}));
      assert.equal(await card('fixture-5').getByRole('button',{name:'重新验证',exact:true}).count(),0);
      assert.equal(await card('fixture-5').getByText('请在快手窗口完成验证',{exact:true}).count(),0);
      assert.equal(await card('fixture-5').getByRole('button',{name:'检查页面',exact:true}).count(),inspect?1:0);
      assert.equal(record('fixture-5').isLive,true,'an unreadable page is not evidence of offline');
    }
    Object.assign(record('fixture-5'),original,{accessState:original.accessState??''});h.emit('update',record('fixture-5'));
  });
  await step('卡片操作区在列表和网格、宽窄窗口保持对齐', async () => {
    for(const width of [1280,800,420]) {
      await page.setViewportSize({width,height:960});
      for(const list of [true,false]) {
        await page.getByRole('button',{name:list?'列表视图':'网格视图',exact:true}).click();
        const target=card('fixture-5');await target.scrollIntoViewIfNeeded();
        const layout=await target.evaluate(card=>{
          const rect=element=>{const r=element.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,cy:(r.top+r.bottom)/2};};
          const actions=card.querySelector('.card-actions');const buttons=[...actions.querySelectorAll('button')].map(rect);
          return {card:rect(card),buttons,primary:[...card.querySelectorAll('.card-primary-actions button')].map(rect),tools:[...card.querySelectorAll('.card-tools button')].map(rect),floating:card.querySelectorAll('.card-monitor-row button').length,overflow:document.documentElement.scrollWidth>innerWidth+1};
        });
        assert.equal(layout.floating,0);assert.equal(layout.overflow,false);
        for(const button of layout.buttons) assert.ok(button.left>=layout.card.left&&button.right<=layout.card.right+1,'操作按钮不越过卡片');
        for(const group of [layout.primary,layout.tools]) assert.ok(Math.max(...group.map(r=>r.cy))-Math.min(...group.map(r=>r.cy))<=1,'同组按钮中线对齐');
        if(list&&width===1280)assert.ok(Math.max(...layout.buttons.map(r=>r.cy))-Math.min(...layout.buttons.map(r=>r.cy))<=1,'宽屏列表全部按钮同一中线');
        await h.shot('actions-'+width+'-'+(list?'list':'grid'));
      }
    }
    await page.setViewportSize({width:1280,height:900});
    await page.getByRole('button',{name:'网格视图',exact:true}).click();
  });
  await step('自动补名、直播中录制失败与重试恢复保持一致', async () => {
    const original = structuredClone(record('fixture-5'));
    Object.assign(record('fixture-5'), { streamerName: '', liveTitle: '测试直播标题', isLive: true, isRecording: false, recordingError: null });
    h.emit('update', record('fixture-5'));
    await visible(card('fixture-5').getByRole('button', { name: '未命名直播间', exact: true }));
    assert.equal(await card('fixture-5').locator('.card-subtitle').innerText(), '测试直播标题');
    await card('fixture-5').getByRole('button', { name: '单次录制', exact: true }).click();
    await visible(card('fixture-5').getByRole('button', { name: '自动识别主播', exact: true }));
    h.failRecording('fixture-5', '播放地址返回 HTTP 404');
    await visible(card('fixture-5').locator('.card-recording-error'));
    assert.equal(await card('fixture-5').locator('.badge').innerText(), '录制异常');
    assert.match(await card('fixture-5').locator('.card-recording-error').innerText(), /录制失败：播放地址返回 HTTP 404/);
    assert.equal(await card('fixture-5').getByRole('button', { name: '单次录制', exact: true }).isEnabled(), true);
    for (const width of [1280, 800, 375]) {
      await page.setViewportSize({ width, height: 900 });
      await card('fixture-5').locator('.card-recording-error').scrollIntoViewIfNeeded();
      const bounds = await card('fixture-5').locator('.card-recording-error').boundingBox();
      assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= width + 1, '错误提示不能溢出视口');
      await h.shot('recording-failure-' + width);
    }
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.getByRole('button', { name: '列表视图', exact: true }).click();
    await card('fixture-5').locator('.card-recording-error').scrollIntoViewIfNeeded();
    await h.shot('recording-failure-list-light');
    await page.getByRole('button', { name: '切换到深色' }).click();
    await waitTheme(page, 'dark');
    await h.shot('recording-failure-list-dark');
    await page.getByRole('button', { name: '切换到浅色' }).click();
    await waitTheme(page, 'light');
    await page.getByRole('button', { name: '网格视图', exact: true }).click();
    await card('fixture-5').getByRole('button', { name: '单次录制', exact: true }).click();
    await hidden(card('fixture-5').locator('.card-recording-error'));
    assert.equal(await card('fixture-5').locator('.badge').innerText(), '录制中');
    await card('fixture-5').getByRole('button', { name: '停止录制', exact: true }).click();
    await visible(card('fixture-5').getByRole('button', { name: '单次录制', exact: true }));
    assert.equal(await card('fixture-5').locator('.card-recording-error').count(), 0);
    assert.equal(await card('fixture-5').locator('.badge').innerText(), '直播未录制');
    Object.assign(record('fixture-5'), original);
    h.emit('update', record('fixture-5'));
    await visible(card('fixture-5').getByRole('button', { name: original.streamerName, exact: true }));
  });
  await step('录制、直播未录制和下播复核各归一类，旧直播状态不重复计数', async () => {
    const r=record('fixture-5'), original=structuredClone(r);
    const filter=name=>page.getByRole('button',{name:new RegExp('^'+name)});
    const all=()=>filter('全部').click();
    Object.assign(r,{isLive:true,isRecording:true,monitorStatus:true,recordingError:null,checkError:null,verificationRequired:false,accessState:'',checkState:'idle'}); h.emit('update',r);
    await filter('录制中').click();await visible(card(r.recId));
    await filter('直播未录制').click();await hidden(card(r.recId));
    Object.assign(r,{isRecording:false});h.emit('update',r);await visible(card(r.recId));
    assert.equal(await card(r.recId).locator('.badge').innerText(),'直播未录制');
    Object.assign(r,{checkState:'rechecking',recordedSeconds:2584.9});h.emit('update',r);await hidden(card(r.recId));
    await all();await visible(card(r.recId).getByText('录制结束，复核中',{exact:true}));
    assert.equal(await card(r.recId).locator('.card-check-error').count(),0);
    Object.assign(r,{checkState:'idle',checkError:'平台返回空响应（未验证直播状态）'});h.emit('update',r);
    await visible(card(r.recId).getByText('录制已结束，直播状态待确认',{exact:true}));
    await filter('直播未录制').click();await hidden(card(r.recId));
    Object.assign(r,{isLive:false,liveTitle:null,checkError:null});h.emit('update',r);
    await filter('等待开播').click();await visible(card(r.recId));
    assert.equal(await card(r.recId).locator('.badge').innerText(),'等待开播');
    assert.equal(await card(r.recId).locator('.card-check-error').count(),0);
    const counts=await page.locator('.filter-tab span').allTextContents();
    assert.equal(counts.slice(1).reduce((a,n)=>a+Number(n),0),Number(counts[0]));
    Object.assign(r,original);h.emit('update',r);await all();
  });
  await step('删除确认、取消与 SSE 删除后无信号生命周期错误', async () => {
    const before = requests('DELETE', '/api/recordings/fixture-7').length;
    await card('fixture-7').getByRole('button', { name: '删除任务' }).click();
    const modal = dialog('移除直播间'); await visible(modal);
    await modal.getByRole('button', { name: '取消', exact: true }).click(); await hidden(modal);
    assert.equal(requests('DELETE', '/api/recordings/fixture-7').length, before);
    await card('fixture-7').getByRole('button', { name: '删除任务' }).click();
    await modal.getByRole('button', { name: '确认删除' }).click();
    await hidden(card('fixture-7'));
    await waitState(() => requests('DELETE', '/api/recordings/fixture-7').length, (n) => n === before + 1, '仅删除选定任务');
    assert.ok(record('fixture-8'));
  });
  await step('任务次要操作统一图标，悬停与键盘焦点显示禁用原因', async () => {
    const tools=card('fixture-1').locator('.card-tools');
    assert.equal(await tools.getByRole('button').count(),4);
    const bounds=await tools.getByRole('button').evaluateAll(nodes=>nodes.map(node=>({width:node.getBoundingClientRect().width,height:node.getBoundingClientRect().height,text:node.textContent.trim()})));
    assert.ok(bounds.every(item=>item.width===bounds[0].width&&item.height===bounds[0].height&&item.text===''));
    const edit=tools.getByRole('button',{name:'编辑任务'});assert.equal(await edit.isDisabled(),true);
    const hint=edit.locator('..');await hint.focus();
    assert.equal(await hint.evaluate(node=>getComputedStyle(node,'::after').visibility),'visible');
    assert.match(await hint.getAttribute('data-tooltip'),/请先停止录制/);
    await tools.getByRole('button',{name:'预览录制文件'}).hover();
    await h.shot('card-icon-toolbar');
  });
  await step('TS 录后与录中预览实际播放，关闭后释放播放器', async () => {
    for(const live of [false,true]) {
      state.previewLive=live;
      await card('fixture-1').getByRole('button',{name:'预览录制文件'}).click();const modal=dialog('录制预览');
      await visible(modal.getByLabel('录制视频预览'));
      assert.equal(await modal.locator('.directory-note,.modal-actions').count(),0);
      assert.equal(await modal.getByRole('button',{name:'转为 MP4',exact:true}).count(),0);
      assert.equal(await modal.getByRole('button',{name:'关闭对话框',exact:true}).count(),1);
      await page.waitForFunction(()=>{const video=document.querySelector('video');return video&&video.videoWidth===160&&video.currentTime>0.5;},null,{timeout:15000});
      const slider=modal.getByRole('slider',{name:'播放进度',exact:true});await visible(slider);
      for(const fraction of [.8,.2]){
        const box=await slider.boundingBox();await page.mouse.move(box.x+box.width*.5,box.y+box.height/2);await page.mouse.down();await page.mouse.move(box.x+6+(box.width-12)*fraction,box.y+box.height/2,{steps:8});
        await page.waitForFunction(({fraction})=>{const video=document.querySelector('video');const at=Number(video?.dataset.timelinePosition);return Math.abs(at-6*fraction)<.5&&video?.readyState>=2&&!video.seeking&&video.dataset.scrubbing==='true';},{fraction},{timeout:15000});
        await page.mouse.up();
        await page.waitForFunction(()=>!document.querySelector('video').paused);
      }
      if(live){await modal.getByRole('button',{name:'回到最新',exact:true}).click();await page.waitForFunction(()=>{const video=document.querySelector('video');return Number(video?.dataset.timelinePosition)>=2.8&&video?.readyState>=2&&video.currentTime>.15;});}
      await h.shot(live?'preview-ts-growing':'preview-ts-completed');
      if(!live){
        await page.setViewportSize({width:1100,height:750});
        const layout=await modal.evaluate(node=>{const body=node.querySelector('.modal-body').getBoundingClientRect(),files=node.querySelector('.preview-file-list').getBoundingClientRect(),controls=node.querySelector('.seek-controls').getBoundingClientRect();return {bottom:body.bottom,filesBottom:files.bottom,controlsBottom:controls.bottom};});
        assert.ok(layout.filesBottom<=layout.bottom+1&&layout.controlsBottom<=layout.bottom+1,'默认桌面窗口应同时显示完整播放控件与当前文件列表');
        await h.shot('preview-default-window');await page.setViewportSize({width:1280,height:900});
      }
      await modal.getByRole('button',{name:'关闭对话框'}).click();await hidden(modal);
      await waitState(()=>page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),count=>count===0,'播放器实例全部释放');
      await waitState(()=>state.activePreviews,n=>n===0,'关闭预览释放增量连接');
    }
    state.previewLive=false;
  });
  await step('全屏内操作及退出全屏保持同一个预览和播放位置', async () => {
    await card('fixture-1').getByRole('button',{name:'预览录制文件'}).click();const modal=dialog('录制预览');
    await page.waitForFunction(()=>document.querySelector('video')?.readyState>=2);
    const source=await modal.locator('.media-preview').getAttribute('data-path');
    await modal.getByRole('button',{name:'暂停',exact:true}).click();
    await modal.getByRole('button',{name:'全屏',exact:true}).click();await page.waitForFunction(()=>Boolean(document.fullscreenElement));
    await page.getByRole('button',{name:'前进 10 秒',exact:true}).click();
    assert.equal(await modal.count(),1);assert.equal(await page.locator('video').count(),1);
    await page.getByRole('button',{name:'退出全屏',exact:true}).click();await page.waitForFunction(()=>!document.fullscreenElement);
    await visible(modal);assert.equal(await modal.locator('.media-preview').getAttribute('data-path'),source);
    await modal.getByRole('combobox',{name:'播放速度'}).selectOption('1.5');
    assert.equal(await modal.locator('video').evaluate(video=>video.playbackRate),1.5);
    await modal.getByRole('button',{name:'全屏',exact:true}).click();await page.waitForFunction(()=>Boolean(document.fullscreenElement));
    await page.keyboard.press('Escape');await page.waitForFunction(()=>!document.fullscreenElement);
    await visible(modal);assert.equal(await modal.locator('.media-preview').getAttribute('data-path'),source);
    await modal.getByRole('button',{name:'关闭对话框',exact:true}).click();await hidden(modal);
  });
  await step('媒体库筛选、目录导航与真正的内嵌音频加载', async () => {
    await navigate('媒体库');
    await waitState(() => page.locator('.file-table tbody tr').count(), (n) => n === 4, '根目录文件');
    await h.shot('10-storage');
    await page.getByRole('button', { name: '音频', exact: true }).click();
    assert.equal(await page.locator('.file-table tbody tr').count(), 1);
    await page.getByRole('button', { name: /^清晨片段.wav/ }).click();
    const modal = dialog('媒体预览');
    await visible(modal.getByLabel('录制音频预览'));
    await page.waitForFunction(() => document.querySelector('audio')?.readyState >= 1);
    await modal.getByRole('button', { name: '关闭对话框' }).click(); await hidden(modal);
    assert.equal(await page.locator('audio').count(), 0);
    await page.getByRole('button', { name: '全部文件', exact: true }).click();
    await page.getByRole('button', { name: /^云间电台.*文件夹/ }).click();
    await visible(page.getByRole('button', { name: /^周末音乐.ts/ }));
    await page.getByRole('button', { name: '返回媒体库根目录' }).click();
    await visible(page.getByRole('button', { name: /^录制说明.txt/ }));
  });
  await step('媒体库删除只修改模拟目录并要求确认', async () => {
    await page.getByRole('button', { name: '回收录制说明.txt', exact: true }).click();
    const modal = dialog('移至回收站'); await visible(modal);
    await modal.getByRole('button', { name: '取消', exact: true }).click();
    assert.ok(state.files.some((f) => f.name === '录制说明.txt'));
    await page.getByRole('button', { name: '回收录制说明.txt', exact: true }).click();
    await modal.getByRole('button', { name: '移至回收站' }).click(); await hidden(modal);
    await waitState(() => state.files.some((f) => f.name === '录制说明.txt'), (v) => !v, '模拟文件删除');
  });
  await step('媒体库转 MP4 显示状态并保留原 TS，拒绝同名覆盖', async () => {
    await page.getByRole('button',{name:'返回媒体库根目录'}).click();
    await page.getByRole('button',{name:'转为MP4：海边日落.ts',exact:true}).click();
    await waitState(()=>state.mediaJobs.at(-1)?.state,s=>s==='complete','转换任务状态完成');
    await visible(page.getByRole('button',{name:/^海边日落.mp4/}));
    assert.ok(state.files.some(f=>f.path==='海边日落.ts'));
    const convert=page.getByRole('button',{name:'转为MP4：海边日落.ts',exact:true});
    assert.equal(await convert.isDisabled(),true);assert.match(await convert.innerText(),/转 MP4/);
    assert.equal(await convert.getAttribute('title'),'同名 MP4 已存在');
  });
  await step('异步预览期间切页，不访问已销毁信号', async () => {
    await navigate('录制任务');
    h.delayNext('GET', '/api/recordings/fixture-1/files', 700);
    await card('fixture-1').getByRole('button', { name: '预览录制文件' }).click();
    await page.keyboard.press('Escape');
    await navigate('总览');
    await visible(page.getByRole('heading', { name: '录制工作台', exact: true }));
  });
  await step('区分解析不可用与后端断线，保留任务并自动恢复', async () => {
    state.resolverReady = false;
    await visible(page.locator('.workbench-service').getByText('解析暂不可用', { exact: true }));
    assert.equal(await card('fixture-2').getByRole('button', { name: '单次录制', exact: true }).isDisabled(), true);
    const count = state.recordings.length;
    h.setOffline(true);
    await visible(page.locator('.connection-banner'));
    assert.equal(await page.locator('.recording-card').count(), Math.min(count, 6));
    h.setOffline(false); state.resolverReady = true;
    await hidden(page.locator('.connection-banner'));
    await visible(page.getByText('本地实时更新', { exact: true }));
    assert.equal(state.recordings.length, count);
  });
  await step('SSE 更新直接反映在现有卡片', async () => {
    record('fixture-2').streamerName = '阿鹿 · 实时更新'; h.emit('update', record('fixture-2'));
    await visible(page.getByText('阿鹿 · 实时更新', { exact: true }));
  });
  await step('轮询与 SSE 的四种启动交错均结束加载、保留错误并自动恢复', async () => {
    for (const oldStatus of [200, 500]) {
      for (const newestFirst of [false, true]) {
        const pattern = '**/api/recordings', eventsPattern = '**/api/events';
        const eventsGate = Promise.withResolvers(), oldGate = Promise.withResolvers(), latestGate = Promise.withResolvers(), recoveryGate = Promise.withResolvers();
        const oldDone = Promise.withResolvers(), latestDone = Promise.withResolvers();
        const routeErrors = [], oldSnapshot = structuredClone(state.recordings);
        let count = 0;
        const eventsHandler = async route => {
          try { await eventsGate.promise; await route.fallback(); }
          catch (error) { routeErrors.push(String(error)); }
        };
        const handler = async route => {
          const attempt = ++count;
          try {
            if (attempt === 1) { eventsGate.resolve(); await oldGate.promise; }
            else if (attempt === 2) await latestGate.promise;
            else { await recoveryGate.promise; await route.fallback(); return; }
            const status = attempt === 1 ? oldStatus : 500;
            await route.fulfill({ status, contentType: 'application/json', headers: { 'Access-Control-Allow-Origin': '*' }, body: JSON.stringify(status === 200 ? oldSnapshot : { detail: '模拟启动快照失败' }) });
          } catch (error) { routeErrors.push(String(error)); }
          finally { if (attempt === 1) oldDone.resolve(); else if (attempt === 2) latestDone.resolve(); }
        };
        await page.route(eventsPattern, eventsHandler);
        await page.route(pattern, handler);
        try {
          await page.goto(h.base + '/home', { waitUntil: 'domcontentloaded' });
          await waitState(() => count, n => n === 2, '首次轮询和 SSE 快照均已进入受控交错');
          const finishOld = async () => {
            oldGate.resolve(); await oldDone.promise;
            await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
          };
          if (!newestFirst) await finishOld();
          const statusCount = requests('GET', '/api/status').length;
          latestGate.resolve(); await latestDone.promise;
          await visible(page.locator('.connection-banner'));
          await hidden(page.locator('.skeleton-grid'));
          if (newestFirst) await finishOld();
          await waitState(() => count, n => n >= 3, '最新快照失败后自动重试');
          assert.ok(requests('GET', '/api/status').length > statusCount, '重试前已读取健康状态');
          await visible(page.locator('.connection-banner'));
          assert.equal(await page.locator('.skeleton-grid').count(), 0);
          assert.equal(await page.locator('.recording-card').count(), 0, '旧快照不得冒充初始化成功');
          await h.shot(`startup-error-${oldStatus}-${newestFirst ? 'latest-first' : 'old-first'}`);
          const jobsRestored = page.waitForResponse(response => new URL(response.url()).pathname === '/api/media/jobs' && response.status() === 200);
          recoveryGate.resolve();
          await jobsRestored;
          await waitState(() => page.locator('.recording-card').count(), n => n === Math.min(state.recordings.length, 6), '快照真实加载成功后恢复列表');
          await hidden(page.locator('.connection-banner'));
          assert.deepEqual(routeErrors, []);
        } finally {
          eventsGate.resolve(); oldGate.resolve(); latestGate.resolve(); recoveryGate.resolve();
          await page.unroute(pattern, handler); await page.unroute(eventsPattern, eventsHandler);
        }
      }
    }
  });
  await step('媒体队列快照失败保留已有任务，健康状态不会提前清除错误', async () => {
    const pattern = '**/api/media/jobs', recoveryGate = Promise.withResolvers();
    const routeErrors = [];
    const before = await page.locator('.recording-card').count();
    let count = 0;
    const handler = async route => {
      try {
        if (++count === 1) await route.fulfill({ status: 500, contentType: 'application/json', headers: { 'Access-Control-Allow-Origin': '*' }, body: JSON.stringify({ detail: '模拟媒体队列读取失败' }) });
        else { await recoveryGate.promise; await route.fallback(); }
      } catch (error) { routeErrors.push(String(error)); }
    };
    await page.route(pattern, handler);
    try {
      h.emit('resync', {});
      await visible(page.locator('.connection-banner'));
      await waitState(() => count, n => n >= 2, '媒体队列失败后自动重试');
      await visible(page.locator('.connection-banner'));
      assert.equal(await page.locator('.recording-card').count(), before);
      assert.equal(await page.locator('.skeleton-grid').count(), 0);
      recoveryGate.resolve();
      await hidden(page.locator('.connection-banner'));
      assert.deepEqual(routeErrors, []);
    } finally { recoveryGate.resolve(); await page.unroute(pattern, handler); }
  });
  await step('安装与关机都有准确确认文案，取消不产生副作用',async()=>{
    await navigate('偏好设置');
    const before=requests('POST','/api/tools/install').length;
    await page.getByRole('button',{name:'安装 FFmpeg',exact:true}).click();const install=dialog('安装录制工具');await visible(install);
    await visible(install.getByRole('button',{name:'确认安装',exact:true}));assert.equal(await install.getByRole('button',{name:'确认删除',exact:true}).count(),0);
    await install.getByRole('button',{name:'取消',exact:true}).click();assert.equal(requests('POST','/api/tools/install').length,before);
    await page.getByRole('button',{name:'选择文件夹',exact:true}).click();await visible(page.locator('.settings-savebar'));assert.ok(state.native.calls.some(call=>call.command==='desktop_window_action'&&call.args.action==='pick-directory'));await page.getByRole('button',{name:'放弃修改',exact:true}).click();
    await page.getByRole('button',{name:'自动化与通知',exact:true}).click();await page.getByRole('spinbutton',{name:'关机倒计时（小时）',exact:true}).fill('2');await page.getByRole('button',{name:'启动倒计时',exact:true}).click();const timer=dialog('启动关机倒计时');await visible(timer);await timer.getByRole('button',{name:'启动倒计时',exact:true}).click();await waitState(()=>state.quickShutdown,value=>value===2,'只设置模拟倒计时');
    await h.triggerShutdown(60);await visible(page.locator('.shutdown-banner'));await page.getByRole('button',{name:'取消关机',exact:true}).click();await hidden(page.locator('.shutdown-banner'));assert.equal(state.native.systemShutdown,false);assert.equal(state.quickShutdown,null);
    await h.shot('native-shutdown-cancelled');
  });
  await step('真实视频画面截图、直播源预览与关闭清理',async()=>{
    await navigate('录制任务');await card('fixture-1').getByRole('button',{name:'预览录制文件'}).click();const modal=dialog('录制预览');await visible(modal);
    await page.waitForFunction(()=>document.querySelector('video')?.readyState>=2&&document.querySelector('video').videoWidth>0);
    const capture=modal.getByRole('button',{name:'保存截图',exact:true});
    const initialTheme=await page.locator('html').getAttribute('data-theme');
    try {
      for(const theme of ['light','dark']){
        await page.locator('html').evaluate((el,value)=>{el.dataset.theme=value;},theme);
        const contrast=await capture.evaluate(el=>{
          const luminance=color=>{const channels=color.match(/[\d.]+/g).slice(0,3).map(Number).map(n=>{const s=n/255;return s<=.04045?s/12.92:((s+.055)/1.055)**2.4;});return channels[0]*.2126+channels[1]*.7152+channels[2]*.0722;};
          const foreground=luminance(getComputedStyle(el).color),background=luminance(getComputedStyle(el.closest('.seek-controls')).backgroundColor);
          return (Math.max(foreground,background)+.05)/(Math.min(foreground,background)+.05);
        });
        assert.ok(contrast>=4.5,'截图图标在 '+theme+' 主题的对比度必须达到 4.5:1');
      }
    } finally {await page.locator('html').evaluate((el,value)=>{if(value===null)delete el.dataset.theme;else el.dataset.theme=value;},initialTheme);}
    await capture.click();await waitState(()=>requests('POST','/api/media/screenshot').length,count=>count>=1,'截图已通过模拟 API');
    await modal.getByRole('button',{name:'预览直播源',exact:true}).click();await waitState(()=>requests('GET','/api/recordings/fixture-1/preview').length,count=>count>=1,'直播源独立预览');
    await page.keyboard.press('Escape');await hidden(modal);await waitState(()=>state.activePreviews,count=>count===0,'直播预览连接关闭');
  });
  await step('源 TS 清理后，已打开的媒体预览自动切换到可播放 MP4',async()=>{
    await navigate('媒体库');await page.getByRole('button',{name:'返回媒体库根目录'}).click();await page.getByRole('button',{name:/^海边日落.ts/}).click();const modal=dialog('媒体预览');await visible(modal);await page.waitForFunction(()=>document.querySelector('video')?.readyState>=2);
    const source='海边日落.ts',output='海边日落.mp4';state.files=state.files.filter(file=>file.path!==source);
    const job={id:'cleanup-live-fixture',taskId:null,source,output,state:'complete',deleteOriginal:true,sourceRemoved:true,message:'MP4 已生成并校验，源 TS 已清理'};state.mediaJobs.push(job);h.emit('mediaJob',job);
    await page.waitForFunction(()=>document.querySelector('.media-preview')?.dataset.path==='海边日落.mp4'&&document.querySelector('video')?.readyState>=2);await h.shot('preview-after-cleanup');
    await modal.getByRole('button',{name:'关闭对话框'}).click();await hidden(modal);
  });
  await step('语言切换、快捷键、上次页面恢复和手动更新检查',async()=>{
    const checks=state.requests.filter(request=>/\/(check|start)$/.test(request.path)).length;
    await navigate('偏好设置');await page.getByRole('button',{name:'外观与窗口',exact:true}).click();await page.getByRole('combobox',{name:'界面语言',exact:true}).selectOption('en');
    await waitState(()=>page.locator('html').getAttribute('lang'),value=>value==='en','英文界面');await visible(page.getByRole('heading',{name:'Preferences',exact:true}));
    await page.keyboard.press('Control+3');await visible(page.getByRole('heading',{name:'Media library',exact:true}));await page.goto(h.base+'/');await visible(page.getByRole('heading',{name:'Media library',exact:true}));
    await page.keyboard.press('Control+2');await visible(card('fixture-2').getByRole('button',{name:record('fixture-2').streamerName,exact:true}));await h.shot('english-recordings');
    await page.keyboard.press('Control+5');await page.getByRole('button',{name:'Check for updates',exact:true}).click();await visible(page.getByRole('link',{name:'v0.1.1-fixture',exact:true}));assert.equal(requests('GET','/api/tools/update').length,1);
    await page.keyboard.press('Control+,');await page.getByRole('button',{name:'Appearance and window',exact:true}).click();await page.getByRole('combobox',{name:'Interface language',exact:true}).selectOption('zh_CN');await waitState(()=>page.locator('html').getAttribute('lang'),value=>value==='zh-CN','恢复中文');
    assert.equal(state.requests.filter(request=>/\/(check|start)$/.test(request.path)).length,checks,'切页和重载不触发额外平台检查');
  });
  await step('工作台突出录制方案、实时耗时和待处理任务',async()=>{
    await navigate('总览');
    await visible(page.getByRole('heading',{name:'当前录制方案',exact:true}));
    await visible(page.getByText('转 MP4 · 校验后清理 TS',{exact:true}));
    const selected=record('fixture-2');const original=structuredClone(selected);
    Object.assign(selected,{isRecording:false,recordingError:'工作台异常夹具'});h.emit('update',selected);
    await waitState(()=>page.locator('.recording-card').first().getAttribute('data-rec-id'),id=>id==='fixture-2','失败任务置顶');
    await h.shot('workbench-attention');
    await navigate('录制任务');await page.getByRole('button',{name:/^需关注/}).click();
    await visible(card('fixture-2'));assert.equal(await card('fixture-1').count(),0);
    await page.getByRole('button',{name:/^全部/}).click();
    Object.assign(selected,original);h.emit('update',selected);
  });
  await step('明确退出而非托盘时显示收尾状态',async()=>{
    await page.getByRole('button',{name:'关闭窗口',exact:true}).click();const close=dialog('关闭 StreamCap');await visible(close);
    await close.getByRole('button',{name:/退出应用/}).click();await hidden(close);await visible(page.getByText('正在安全退出',{exact:true}));assert.equal(state.native.closing,true);
  });
  await step('最终安全检查：无真实网络、未处理请求或浏览器异常', async () => {
    assert.deepEqual(h.blocked, []); assert.deepEqual(state.unexpected, []); assert.deepEqual(h.runtimeErrors, []);
    const appErrors = h.consoleErrors.filter((message) => !message.startsWith('Failed to load resource:'));
    assert.deepEqual(appErrors, [], '应用 console.error');
  });
} catch (error) {
  failure = error;
  await h.shot('failure').catch(() => {});
  console.error(error.stack ?? error);
} finally {
  await h.report({ passed: !failure, steps, failure: failure ? String(failure.stack ?? failure) : null });
  await h.close();
}
console.log(JSON.stringify({ passed: !failure, checks: steps.length, artifacts: h.runDir }));
if (failure) process.exitCode = 1;
