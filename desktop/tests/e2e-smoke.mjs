import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { apiContracts, cardMenuAction, closeCardMenu, createHarness, eventually, go, hidden, inspectLayout, openCardMenu, seedMediaJob, seedRecordings, setSwitch, streamPreviewFixture, themeColors, visible, waitTheme } from './ui-fixtures.mjs';

const h = await createHarness('interaction');
const { page, state } = h;
const steps = [];
const loadResults = [];
const storageLoadResults = [];
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
async function openRecordingPreview(target) {
  const button = target.getByRole('button', { name: '预览录制文件', exact: true });
  await visible(button);
  assert.equal(await target.locator('.card-menu-panel').getByRole('button', { name: '预览录制文件', exact: true }).count(), 0, '预览不再藏在二级菜单');
  await button.click();
}
async function readFixtureJobs() {
  const response = await fetch(h.base + '/api/media/jobs');
  assert.equal(response.status, 200, '只读取当前隔离夹具的媒体队列');
  return (await response.json()).jobs;
}
async function assertFullWidthPage(route) {
  const home = route === '/home';
  const container = page.locator(home ? '.home-page' : '.storage-page');
  const removed = home
    ? '.overview-aside, .overview-health, .overview-queue-section, .overview-policy'
    : '.file-jobs-sidebar, .media-jobs, .media-history, .media-history-attention';
  assert.equal(await container.locator(removed).count(), 0, '侧栏必须从 DOM 删除，而不是折叠或隐藏');
  const surface = container.locator(home ? '.overview-main' : '.file-manager');
  await visible(surface);
  const outer = await container.boundingBox(), inner = await surface.boundingBox();
  assert.ok(outer && inner && inner.width > 0);
  assert.ok(Math.abs(inner.x - outer.x) <= 1 && Math.abs(inner.width - outer.width) <= 1, '主体占满页面宽度，不为已删除侧栏预留空列');
  if (!home) {
    const table = container.locator('.file-table');
    await visible(table);
    const tableBounds = await table.boundingBox(), viewport = await container.locator('.table-container').boundingBox();
    assert.ok(viewport && Math.abs(viewport.width - (inner.width - 2)) <= 1, '文件表除面板边框外占满文件管理器');
    const listClientWidth=await container.locator('.table-container').evaluate(node=>node.clientWidth);
    if (inner.width >= 620) assert.ok(tableBounds && Math.abs(tableBounds.width - listClientWidth) <= 1, '桌面文件表除系统滚动槽外占满主体宽度');
  }
}
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
    await waitState(() => page.locator('.overview-task-row').count(), (n) => n === 6, '工作台初始任务数量');
    await assertFullWidthPage('/home');
    await visible(page.getByText('本地实时更新', { exact: true }));
    const layout = await inspectLayout(page);
    assert.equal(layout.dialogs, 0);
    assert.equal(layout.surface.background, 'rgba(255, 255, 255, 0.76)');
    assert.ok(layout.surface.width > 0 && layout.surface.height > 0, '工作台实际承载区可见');
    assert.equal(layout.bodyOverflow, false);
    assert.equal(layout.contentOverflow, false);
    assert.deepEqual(layout.overflow, []);
    assert.match(layout.surface.backdrop, /blur\(18px\)/);
    assert.equal(layout.baseFontSize, 16);
    assert.equal(layout.bodyFontSize, 14);
    assert.equal(layout.headingFontSize, 25);
    await h.shot('01-home-light');
    await navigate('录制任务');
    await waitState(() => page.locator('.recording-card').count(), (n) => n === 6, '任务页显示同一初始集合');
  });
  await step('侧栏始终使用完整熊猫标识，明暗主题与紧凑侧栏保持比例', async () => {
    const originalTheme = state.userConfig.theme_mode;
    const originalViewport = page.viewportSize();
    try {
      for (const theme of ['light', 'dark']) {
        state.userConfig.theme_mode = theme; h.emit('settings', {});
        await waitTheme(page, theme);
        for (const width of [1100, 800]) {
          await page.setViewportSize({ width, height: 750 });
          const brand = page.locator('.sidebar-brand .brand-mark');
          const panda = brand.locator('img');
          await visible(panda);
          assert.equal(await brand.locator('svg').count(), 0);
          assert.equal(await panda.getAttribute('src'), '/logo-mark.png');
          await page.waitForFunction(() => {
            const image = document.querySelector('.brand-mark img');
            return image?.complete && image.naturalWidth > 0 && image.naturalHeight > 0;
          });
          const geometry = await panda.evaluate(image => ({
            width: image.clientWidth, height: image.clientHeight,
            fit: getComputedStyle(image).objectFit,
            parent: getComputedStyle(image.parentElement).backgroundColor,
          }));
          assert.deepEqual(geometry, { width: 30, height: 32, fit: 'contain', parent: 'rgba(0, 0, 0, 0)' });
          await h.shot('panda-' + theme + '-' + width);
        }
      }
    } finally {
      if (originalTheme === undefined) delete state.userConfig.theme_mode;
      else state.userConfig.theme_mode = originalTheme;
      h.emit('settings', {});
      await waitTheme(page, originalTheme ?? 'light');
      await page.setViewportSize(originalViewport);
    }
  });
  await step('快手访问异常不再显示人工检查入口', async () => {
    const target=state.recordings[1];
    const saved=structuredClone(target);
    try {
      for(const accessState of ['cooldown','loginRequired','unavailable']) {
        Object.assign(target,{platformKey:'kuaishou',isRecording:false,checkState:'idle',accessState,verificationRequired:false,checkError:'本地访问受限回归'});
        h.emit('update',target);
        assert.equal(await card(target.recId).getByRole('button',{name:/^(检查页面|重新验证)$/}).count(),0);
        await visible(card(target.recId).getByText('检测失败：本地访问受限回归',{exact:true}));
      }
    } finally {Object.assign(target,saved);h.emit('update',target);}
  });
  await step('后台错误显示并去重，成功事件不会盖掉未处理告警', async () => {
    h.emit('snack', apiContracts.backgroundError);
    await visible(page.locator('.toast.error'));
    assert.equal(await page.locator('.toast span').innerText(), apiContracts.backgroundError.text);
    h.emit('snack', apiContracts.backgroundError);
    h.emit('snack', apiContracts.backgroundSuccess);
    const original = record('fixture-2').streamerName;
    record('fixture-2').streamerName = '后台事件顺序已确认'; h.emit('update', record('fixture-2'));
    await visible(card('fixture-2').getByRole('button', {name:'后台事件顺序已确认',exact:true}));
    assert.equal(await page.locator('.toast').count(), 1);
    assert.equal(await page.locator('.toast span').innerText(), apiContracts.backgroundError.text);
    record('fixture-2').streamerName = original; h.emit('update', record('fixture-2'));
    await page.getByRole('button', {name:'关闭提示',exact:true}).click();
    h.emit('snack', apiContracts.backgroundSuccess);
    await visible(page.locator('.toast:not(.error)'));
    assert.equal(await page.locator('.toast span').innerText(), apiContracts.backgroundSuccess.text);
    await page.getByRole('button', {name:'关闭提示',exact:true}).click();
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
      await openRecordingPreview(card('fixture-1'));
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
  await step('旧列表快照不能恢复 SSE 已移除任务或回退更新', async () => {
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
      assert.equal(await card('fixture-2').count(), 0, '移除任务被旧快照恢复');
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
    await openRecordingPreview(card('fixture-1'));
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
    assert.equal(bg, themeColors.dark.canvas);
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
    await page.locator('.empty-panel').getByRole('button', { name: '清除筛选' }).click();
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
  await step('弹窗内 Enter 和空格不误关窗口，真正的背景点击仍可关闭', async () => {
    const before = requests('PUT', '/api/recordings/fixture-3').length;
    await cardMenuAction(card('fixture-3'), '编辑任务');
    const modal = dialog('编辑直播间'); await visible(modal);
    await modal.getByRole('textbox', {name:'主播名称',exact:true}).fill('键盘编辑草稿');
    const follow = modal.getByRole('button', {name:'跟随全局',exact:true}).first();
    for (const key of ['Enter', 'Space']) {
      await follow.focus(); await page.keyboard.press(key);
      await visible(modal);
      assert.equal(await modal.getByRole('textbox', {name:'主播名称',exact:true}).inputValue(), '键盘编辑草稿');
    }
    assert.equal(requests('PUT', '/api/recordings/fixture-3').length, before);
    await page.mouse.click(1, 1); await hidden(modal);
    assert.equal(requests('PUT', '/api/recordings/fixture-3').length, before);
  });
  await step('单项编辑、字段契约和实时卡片更新', async () => {
    await cardMenuAction(card('fixture-3'), '编辑任务');
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
  await step('单卡请求期间锁定批量、筛选与选择，完成后全部恢复', async () => {
    const original = structuredClone(record('fixture-3'));
    await card('fixture-5').getByRole('checkbox').check();
    h.delayNext('POST', '/api/recordings/fixture-3/start', 1500);
    await card('fixture-3').getByRole('button', { name:'单次录制', exact:true }).click();
    await visible(card('fixture-3').locator('.card-record[aria-busy="true"]'));
    assert.equal(await page.getByRole('button', { name:'批量编辑', exact:true }).isDisabled(), true);
    assert.equal(await page.getByRole('button', { name:'列表视图', exact:true }).isDisabled(), true);
    assert.equal(await page.getByRole('textbox', { name:'搜索直播间', exact:true }).isDisabled(), true);
    assert.equal(await page.locator('.task-checkbox:not(:disabled)').count(), 0);
    await waitState(() => record('fixture-3').isRecording, Boolean, '单卡请求完成');
    const stop = card('fixture-3').getByRole('button', { name:'停止录制', exact:true });
    await waitState(() => stop.isEnabled(), Boolean, '同步完成后可以停止');
    await stop.click();
    await waitState(() => record('fixture-3').isRecording, value => !value, '停止测试录制');
    await waitState(() => page.getByRole('button', { name:'取消选择', exact:true }).isEnabled(), Boolean, '操作计数回到零');
    await page.getByRole('button', { name:'取消选择', exact:true }).click();
    Object.assign(record('fixture-3'), original); h.emit('update', record('fixture-3'));
  });
  await step('筛选隐藏的选择明确计数，批量编辑仍使用完整选择集合', async () => {
    const originals = ['fixture-3','fixture-5'].map(id => structuredClone(record(id)));
    await card('fixture-3').getByRole('checkbox').check();
    await card('fixture-5').getByRole('checkbox').check();
    assert.equal(await page.locator('.select-all input').evaluate(input => input.indeterminate), true);
    const search = page.getByRole('textbox', { name:'搜索直播间', exact:true });
    await search.fill(await card('fixture-3').locator('.card-name').innerText());
    await visible(page.getByText('当前可见 1 项 · 隐藏 1 项', { exact:true }));
    assert.equal(await page.locator('.select-all input').evaluate(input => input.checked && !input.indeterminate), true);
    await page.getByRole('button', { name:'批量编辑', exact:true }).click();
    const modal = dialog('批量编辑任务');
    await modal.getByRole('combobox', { name:'清晰度', exact:true }).selectOption('HD');
    h.delayNext('POST', '/api/recordings/batch-edit', 900);
    await modal.getByRole('button', { name:'应用到所选任务', exact:true }).click();
    await waitState(() => modal.getByRole('button', { name:'应用到所选任务', exact:true }).getAttribute('aria-busy'), value => value === 'true', '批量保存忙碌状态');
    assert.equal(await modal.getByRole('button', { name:'取消', exact:true }).isDisabled(), true);
    await hidden(modal);
    assert.deepEqual(requests('POST','/api/recordings/batch-edit').at(-1).body.recIds.sort(), ['fixture-3','fixture-5']);
    await search.fill('');
    await page.getByRole('button', { name:'取消选择', exact:true }).click();
    for (const value of originals) { Object.assign(record(value.recId), value); h.emit('update', record(value.recId)); }
  });
  await step('恢复跟随全局，覆盖项保持独立', async () => {
    await cardMenuAction(card('fixture-3'), '编辑任务');
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
  await step('展开菜单不透字，明暗主题和窄窗口均有清晰反馈', async () => {
    const originalTheme = state.userConfig.theme_mode;
    const assertReadable = ({ background, color }) => {
      const channels = value => value.match(/[\d.]+/g).map(Number);
      const bg = channels(background), fg = channels(color);
      assert.ok(bg.length === 3 || bg[3] === 1, '浮层必须完全不透明');
      const luminance = rgb => rgb.slice(0, 3).map(value => {
        const n = value / 255;
        return n <= .04045 ? n / 12.92 : ((n + .055) / 1.055) ** 2.4;
      }).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
      const a = luminance(bg), b = luminance(fg);
      assert.ok((Math.max(a, b) + .05) / (Math.min(a, b) + .05) >= 4.5, '菜单文字对比度至少为 4.5:1');
    };
    try {
      for (const theme of ['light', 'dark']) {
        state.userConfig.theme_mode = theme; h.emit('settings', {});
        await waitTheme(page, theme);
        for (const width of [1100, 800]) {
          await page.setViewportSize({ width, height: 750 });
          for (const name of ['默认清晰度', '默认录制格式']) {
            const select = page.getByRole('combobox', { name, exact: true });
            const original = await select.inputValue();
            const closedBorder = await select.evaluate(el => getComputedStyle(el).borderColor);
            await select.click();
            const opened = await select.evaluate(el => {
              const picker = getComputedStyle(el, '::picker(select)');
              const selected = getComputedStyle(el.querySelector('option:checked'));
              return { open: el.matches(':open'), border: getComputedStyle(el).borderColor,
                picker: { background: picker.backgroundColor, color: picker.color },
                selected: { background: selected.backgroundColor, color: selected.color } };
            });
            assert.equal(opened.open, true);
            assert.notEqual(opened.border, closedBorder, '展开后触发按钮边界发生可见变化');
            assertReadable(opened.picker); assertReadable(opened.selected);
            if (name === '默认清晰度') await h.shot('dropdown-' + theme + '-' + width);
            await select.press('ArrowDown');
            await page.keyboard.press('Escape');
            assert.equal(await select.evaluate(el => el.matches(':open')), false);
            assert.equal(await select.inputValue(), original, '取消菜单不更改选择');
            assert.equal(await select.evaluate(el => el === document.activeElement), true);
          }
        }
        await navigate('录制任务');
        const menu = await openCardMenu(card('fixture-5'));
        assertReadable(await menu.locator('.card-menu-panel').evaluate(el => {
          const s = getComputedStyle(el); return { background: s.backgroundColor, color: s.color };
        }));
        const trigger = await menu.locator('summary').evaluate(el => {
          const s = getComputedStyle(el); return { background: s.backgroundColor, color: s.color };
        });
        assertReadable(trigger);
        await h.shot('actions-menu-' + theme);
        await closeCardMenu(menu);
        await navigate('偏好设置');
      }
    } finally {
      state.userConfig.theme_mode = originalTheme; h.emit('settings', {});
      await waitTheme(page, originalTheme);
      await page.setViewportSize({ width: 1280, height: 900 });
    }
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
    await waitState(() => interval.isEnabled(), value => value, '保存后的设置刷新已结束');
  });
  await step('设置事件只刷新一次，迟到快照不覆盖新设置或未保存草稿', async () => {
    const interval = page.getByRole('textbox', { name: /检测间隔/ });
    const original = state.userConfig.loop_time_seconds;
    const held = h.holdNextSettingsRead();
    try {
      state.userConfig.loop_time_seconds = '700'; h.emit('settings', {});
      await held.received;
      const before = requests('GET', '/api/settings').length;
      state.userConfig.loop_time_seconds = '800'; h.emit('settings', {});
      await waitState(() => interval.inputValue(), value => value === '800', '新设置先发布');
      assert.equal(requests('GET', '/api/settings').length, before + 1, '页面不能再为同一事件重复请求');
      held.release();
      await page.waitForTimeout(120);
      assert.equal(await interval.inputValue(), '800', '迟到的 700 秒不能覆盖 800 秒');
      await interval.fill('900');
      const received = page.waitForResponse(response => response.url().endsWith('/api/settings') && response.request().method() === 'GET');
      state.userConfig.loop_time_seconds = '810'; h.emit('settings', {});
      await (await received).finished();
      await page.waitForTimeout(80);
      assert.equal(await interval.inputValue(), '900', '刷新不能丢弃正在编辑的字段');
      await page.getByRole('button', { name: '放弃修改', exact: true }).click();
      await waitState(() => interval.inputValue(), value => value === '810', '放弃草稿后使用最新服务端值');
    } finally {
      held.release(); state.userConfig.loop_time_seconds = original; h.emit('settings', {});
    }
    await waitState(() => interval.inputValue(), value => value === original, '恢复原设置');
  });
  await step('录制结束转 MP4 开关按显式选择保存，不改变 TS 与检测间隔', async () => {
    await page.getByRole('button',{name:'录制与存储',exact:true}).click();
    const interval=state.userConfig.loop_time_seconds,format=state.userConfig.video_format;
    const toggle=page.getByRole('switch',{name:'录制结束转 MP4',exact:true});await setSwitch(toggle,true);
    await page.locator('.settings-savebar').getByRole('button',{name:'保存修改'}).click();
    await waitState(()=>state.userConfig.convert_to_mp4,v=>v===true,'启用转换设置');await hidden(page.locator('.settings-savebar'));
    assert.equal(state.userConfig.loop_time_seconds,interval);assert.equal(state.userConfig.video_format,format);
    await setSwitch(toggle,false);await page.locator('.settings-savebar').getByRole('button',{name:'保存修改'}).click();
    await waitState(()=>state.userConfig.convert_to_mp4,v=>v===false,'关闭转换设置');await hidden(page.locator('.settings-savebar'));
  });
  await step('仅有亮暗主题，系统变化不改用户选择且不保留多强调色', async () => {
    await page.getByRole('button', { name: '外观与窗口', exact: true }).click();
    assert.equal(await page.locator('.theme-option').count(), 2);
    assert.equal(await page.getByRole('button', { name: '跟随系统', exact: true }).count(), 0);
    assert.equal(await page.locator('.accent-options').count(), 0);
    await page.getByRole('button', { name: '浅色', exact: true }).click();
    await waitState(() => state.userConfig.theme_mode, (v) => v === 'light', '主题保存');
    await page.emulateMedia({ colorScheme: 'dark' });
    await waitTheme(page, 'light');
    await page.getByRole('button', { name: '深色', exact: true }).click();
    await waitTheme(page, 'dark');
    await visible(page.getByRole('button', { name: '切换到浅色' }));
    assert.equal(await page.evaluate(()=>typeof window.__TAURI__), 'undefined');
    assert.equal(await page.locator('html').getAttribute('data-accent'), null);
    await h.shot('08-settings-dark-glass');
    await page.emulateMedia({ colorScheme: 'light' });
    await waitTheme(page, 'dark');
    await page.getByRole('button', { name: '浅色', exact: true }).click();
    await waitState(() => state.userConfig.theme_mode, (v) => v === 'light', '浅色恢复');
  });
  await step('快速切换亮暗串行合并，最后选择不会被旧写入覆盖', async () => {
    const before = requests('PUT', '/api/settings').length;
    h.delayNext('PUT', '/api/settings', 900);
    await page.getByRole('button', { name: '深色', exact: true }).click();
    await waitState(() => requests('PUT', '/api/settings').length, count => count === before + 1, '首个外观请求在途');
    await page.getByRole('button', { name: '浅色', exact: true }).click();
    await page.getByRole('button', { name: '深色', exact: true }).click();
    await page.getByRole('button', { name: '浅色', exact: true }).click();
    await waitTheme(page, 'light');
    await page.waitForTimeout(100);
    assert.equal(requests('PUT', '/api/settings').length, before + 1, '旧写入完成前不能发出并行写入');
    await waitState(() => requests('PUT', '/api/settings').length, count => count === before + 2, '旧写入结束后才发送最终主题');
    await waitState(() => state.userConfig.theme_mode, value => value === 'light', '最后外观持久化');
    await page.waitForTimeout(120);
    assert.equal(requests('PUT', '/api/settings').length, before + 2, '中间外观应合并为最后一次选择');
    await waitTheme(page, 'light');
    assert.equal(await page.locator('html').getAttribute('data-accent'), null);
    assert.deepEqual(await page.evaluate(() => JSON.parse(localStorage.getItem('streamcap.appearance'))), { theme: 'light' });
    assert.ok(requests('PUT', '/api/settings').slice(before).every(request => !Object.hasOwn(request.body.userConfig, 'theme_color')), '外观保存不再写入强调色');
  });
  await step('默认视图保存失败回滚，两个入口都能直接重试同一选项', async () => {
    const original = state.userConfig.is_grid_view ?? state.defaultConfig.is_grid_view;
    const gridWrites = () => requests('PUT', '/api/settings').filter(r => Object.hasOwn(r.body.userConfig, 'is_grid_view'));
    for (const route of ['/settings', '/recordings']) {
      await go(page, h.base, route);
      if (route === '/settings') await page.getByRole('button', { name: '外观与窗口', exact: true }).click();
      const name = value => route === '/settings'
        ? (value ? '默认网格视图' : '默认列表视图') : (value ? '网格视图' : '列表视图');
      const target = page.getByRole('button', { name: name(!original), exact: true });
      const initial = page.getByRole('button', { name: name(original), exact: true });
      h.failNext('PUT', '/api/settings', '模拟视图保存失败', 500);
      await target.click();
      await visible(page.getByText('模拟视图保存失败', { exact: false }).first());
      await waitState(() => initial.getAttribute('aria-pressed'), value => value === 'true', '读回旧偏好并回滚');
      await waitState(() => page.locator('.view-switch').getAttribute('aria-busy'), value => value === 'false', '失败后队列结束');
      const count = gridWrites().length;
      await target.click();
      await waitState(() => state.userConfig.is_grid_view, value => value === !original, '再次点击必须发出保存');
      await waitState(() => page.locator('.view-switch').getAttribute('aria-busy'), value => value === 'false', '重试完成');
      assert.equal(gridWrites().length, count + 1);
      await initial.click();
      await waitState(() => state.userConfig.is_grid_view, value => value === original, '恢复原视图');
      await waitState(() => page.locator('.view-switch').getAttribute('aria-busy'), value => value === 'false', '恢复完成');
    }
  });
  await step('跨页视图选择串行合并，迟到设置快照不覆盖最后选择', async () => {
    await go(page, h.base, '/recordings');
    await page.getByRole('button', { name: '网格视图', exact: true }).click();
    await waitState(() => state.userConfig.is_grid_view, value => value === true, '初始网格');
    await waitState(() => page.locator('.view-switch').getAttribute('aria-busy'), value => value === 'false', '初始保存完成');
    const held = h.holdNextSettingsRead();
    try {
      h.emit('settings', {});
      await held.received;
      const before = requests('PUT', '/api/settings').length;
      h.delayNext('PUT', '/api/settings', 1200);
      await page.getByRole('button', { name: '列表视图', exact: true }).click();
      await waitState(() => requests('PUT', '/api/settings').length, count => count === before + 1, '首个视图请求在途');
      await navigate('偏好设置');
      await page.getByRole('button', { name: '外观与窗口', exact: true }).click();
      await page.getByRole('button', { name: '默认网格视图', exact: true }).click();
      await page.getByRole('button', { name: '默认列表视图', exact: true }).click();
      await page.getByRole('button', { name: '默认网格视图', exact: true }).click();
      assert.equal(requests('PUT', '/api/settings').length, before + 1, '跨页不能并行写入');
      held.release();
      await waitState(() => page.locator('.view-switch').getAttribute('aria-busy'), value => value === 'false', '合并队列结束');
      assert.equal(state.userConfig.is_grid_view, true);
      assert.equal(requests('PUT', '/api/settings').length, before + 2, '只写首个和最终选择');
      assert.equal(await page.getByRole('button', { name: '默认网格视图', exact: true }).getAttribute('aria-pressed'), 'true');
      await navigate('录制任务');
      assert.equal(await page.getByRole('button', { name: '网格视图', exact: true }).getAttribute('aria-pressed'), 'true');
    } finally { held.release(); }
  });
  await step('视图写入已提交但响应失败时读回确认，不伪报回滚或重复写入', async () => {
    await go(page, h.base, '/settings');
    await page.getByRole('button', { name: '外观与窗口', exact: true }).click();
    const pattern = '**/api/settings';
    let intercepted = false;
    const handler = async route => {
      if (!intercepted && route.request().method() === 'PUT'
        && Object.hasOwn(route.request().postDataJSON().userConfig, 'is_grid_view')) {
        intercepted = true;
        const response = await fetchFixture(route);
        assert.equal(response.status(), 200);
        await route.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ detail: '模拟已提交但确认响应丢失' }) });
      } else await route.fallback();
    };
    await page.route(pattern, handler);
    try {
      const before = requests('PUT', '/api/settings').length;
      await page.getByRole('button', { name: '默认列表视图', exact: true }).click();
      await waitState(() => intercepted, Boolean, '确认丢失夹具已触发');
      await waitState(() => page.locator('.view-switch').getAttribute('aria-busy'), value => value === 'false', '读回确认结束');
      assert.equal(state.userConfig.is_grid_view, false);
      assert.equal(await page.getByRole('button', { name: '默认列表视图', exact: true }).getAttribute('aria-pressed'), 'true');
      assert.equal(requests('PUT', '/api/settings').length, before + 1, '读回而非重发修改');
    } finally { await page.unroute(pattern, handler); }
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
    await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await visible(close);await setSwitch(close.getByRole('switch',{name:'记住我的选择'}),true);state.native.failRemember=true;
    await close.getByRole('button',{name:/最小化到托盘/}).click();await visible(close.getByRole('alert'));assert.equal(state.native.visible,true);assert.equal(state.userConfig.close_action,undefined);
    await close.getByRole('button',{name:/最小化到托盘/}).click();await hidden(close);assert.equal(state.native.visible,false);assert.equal(state.native.closing,false);assert.equal(state.userConfig.close_action,'tray');
    await h.restoreNative();await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await waitState(()=>state.native.visible,v=>!v,'记住托盘后直接隐藏');await hidden(close);await h.restoreNative();
    await preference.selectOption('ask');await waitState(()=>state.userConfig.close_action,v=>v==='ask','恢复每次询问');
    state.native.trayAvailable=false;await page.getByRole('button',{name:'关闭窗口',exact:true}).click();await visible(close);assert.equal(await close.getByRole('button',{name:/最小化到托盘/}).isDisabled(),true);
    await close.getByRole('button',{name:'取消',exact:true}).click();await hidden(close);state.native.trayAvailable=true;
    await h.shot('window-preferences');
  });
  await step('账号迟到读取保留草稿，保存只确认对应快照且不回显已存凭据', async () => {
    state.accounts.douyin = {
      username: 'stored-fixture', accountType: 'personal',
      password: 'synthetic-old-secret', accessToken: 'synthetic-old-token',
    };
    const heldRead = h.holdNextAccountRead();
    await page.getByRole('button', { name: '平台登录', exact: true }).click();
    await heldRead.received;
    const summary = page.locator('summary').filter({ hasText: '平台账号与访问令牌' });
    await summary.click();
    const editor = page.locator('details').filter({ has: summary });
    const username = editor.getByLabel('账号', { exact: true });
    const accountType = editor.getByLabel('账号类型', { exact: true });
    const password = editor.getByLabel('密码', { exact: true });
    const token = editor.getByLabel('访问令牌', { exact: true });
    const save = editor.getByRole('button', { name: '保存平台账号', exact: true });
    try {
      await username.fill('typed-during-read');
      heldRead.release();
      await waitState(() => accountType.inputValue(), value => value === 'personal', '读取账号摘要');
      assert.equal(await username.inputValue(), 'typed-during-read');
      assert.equal(await password.inputValue(), '');
      assert.equal(await token.inputValue(), '');
    } finally { heldRead.release(); }
    await password.fill('synthetic-new-secret');
    const heldWrite = h.holdNextAccountWrite();
    try {
      await save.click(); await heldWrite.received;
      await username.fill('typed-during-write');
      await accountType.fill('business');
      heldWrite.release();
      await waitState(() => save.isEnabled(), value => value, '保存期间新编辑仍可继续保存');
      await waitState(() => password.inputValue(), value => value === '', '仅清空已确认保存的凭据');
      assert.equal(await username.inputValue(), 'typed-during-write');
      assert.equal(await accountType.inputValue(), 'business');
      assert.equal(state.accounts.douyin.username, 'typed-during-read');
      assert.equal(state.accounts.douyin.accountType, 'personal');
    } finally { heldWrite.release(); }
    h.failNext('PUT', '/api/accounts', '模拟账号保存失败');
    await save.click(); await visible(page.getByText('模拟账号保存失败', { exact: true }));
    assert.equal(state.accounts.douyin.username, 'typed-during-read');
    assert.equal(await username.inputValue(), 'typed-during-write');
    await waitState(() => save.isEnabled(), value => value, '失败后保留重试入口');
    await save.click();
    await waitState(() => state.accounts.douyin.username, value => value === 'typed-during-write', '新账号草稿已保存');
    assert.equal(state.accounts.douyin.accountType, 'business');
    await waitState(() => save.isDisabled(), value => value, '已保存快照没有遗留脏标记');
    await summary.click();
  });
  await step('登录信息按平台局部保存，扫码仅在隔离服务模拟', async () => {
    await page.getByRole('button', { name: '平台登录', exact: true }).click();
    const input = page.getByLabel('平台 Cookie', { exact: true });
    await visible(input);
    const untouched = state.cookies.bilibili;
    await input.fill('fixture-edited-not-a-credential');
    await page.getByRole('button', { name: '保存登录信息' }).click();
    await waitState(() => state.cookies.douyin, (v) => v === 'fixture-edited-not-a-credential', 'Cookie 局部保存');
    assert.deepEqual(requests('PUT', '/api/cookies').at(-1).body.cookies, { douyin: 'fixture-edited-not-a-credential' });
    assert.equal(state.cookies.bilibili, untouched);
    await page.getByRole('textbox', { name: '搜索平台' }).fill('快手');
    await page.getByRole('combobox', { name: 'Cookie 平台列表', exact: true }).selectOption('kuaishou');
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
    await setSwitch(page.getByRole('switch',{name:'录制结束转 MP4',exact:true}),true);
    await setSwitch(page.getByRole('switch',{name:'转换成功后清理源 TS',exact:true}),true);
    await setSwitch(page.getByRole('switch',{name:'生成时间字幕',exact:true}),true);
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
    await setSwitch(card('fixture-5').getByRole('switch', { name: '自动检测', exact: true }),true);
    await waitState(() => record('fixture-5').monitorStatus, Boolean, '监控启用');
    await card('fixture-5').getByRole('button', { name: '单次录制', exact: true }).click();
    await waitState(() => record('fixture-5').isRecording, Boolean, '开始录制');
    const recordingMenu = await openCardMenu(card('fixture-5'));
    assert.equal(await recordingMenu.getByRole('button', { name: '编辑任务', exact: true }).isDisabled(), true);
    assert.equal(await recordingMenu.getByRole('button', { name: '移除任务', exact: true }).isDisabled(), true);
    await closeCardMenu(recordingMenu);
    await card('fixture-5').getByRole('button', { name: '停止录制', exact: true }).click();
    await waitState(() => record('fixture-5').isRecording, (v) => !v, '停止录制');
    h.failNext('POST', '/api/recordings/fixture-5/start', '模拟 FFmpeg 启动失败');
    await card('fixture-5').getByRole('button', { name: '单次录制', exact: true }).click();
    await visible(page.getByText('模拟 FFmpeg 启动失败', { exact: true }));
    assert.equal(record('fixture-5').isRecording, false);
  });
  await step('快手验证受限不冒充未开播或录制失败，且不再打开验证窗口', async () => {
    const original=structuredClone(record('fixture-5'));
    Object.assign(record('fixture-5'),{platformKey:'kuaishou',platform:'快手直播',checkError:'快手需要完成滑块验证（400002）',verificationRequired:true,isLive:true,isRecording:false,recordingError:null});h.emit('update',record('fixture-5'));
    await visible(card('fixture-5').getByText('验证受限',{exact:true}));
    await visible(card('fixture-5').getByText('检测失败：快手需要完成滑块验证（400002）',{exact:true}));
    const attentionCount=state.recordings.filter(r=>!r.isRecording&&(r.verificationRequired||r.checkError||r.recordingError)).length;
    const attentionTab=page.getByRole('button',{name:/^需关注/});
    assert.ok((await attentionTab.innerText()).includes(String(attentionCount)));
    await attentionTab.click();assert.equal(await page.locator('.recording-card').count(),attentionCount);
    await visible(card('fixture-5'));
    await navigate('总览');
    assert.equal(await page.locator('.overview-summary > div').filter({has:page.getByText('需关注',{exact:true})}).locator('dd').innerText(),String(attentionCount));
    await visible(page.locator('.overview-task-row[data-rec-id="fixture-5"] .overview-task-detail').getByText('快手需要完成滑块验证（400002）',{exact:true}));
    assert.equal(await page.locator('.overview-task-row').first().getAttribute('data-rec-id'),'fixture-5');
    await navigate('录制任务');
    assert.equal(await card('fixture-5').getByRole('button',{name:'单次录制',exact:true}).isDisabled(),true);
    const verificationMenu = await openCardMenu(card('fixture-5'));
    assert.equal(await verificationMenu.getByRole('button',{name:'检测直播状态',exact:true}).isDisabled(),true);
    await closeCardMenu(verificationMenu);
    assert.equal(await card('fixture-5').locator('.card-recording-error').count(),0);
    assert.equal(await card('fixture-5').getByRole('button',{name:/^(检查页面|重新验证)$/}).count(),0);
    assert.equal(state.native.calls.some(call=>call.command==='desktop_kuaishou_verification'),false,'访问错误不调用已移除的原生命令');
    await h.shot('kuaishou-access-restricted');
    Object.assign(record('fixture-5'),original);h.emit('update',record('fixture-5'));
    await hidden(card('fixture-5').getByRole('button',{name:'重新验证',exact:true}));
  });
  await step('限流、访问受限和登录提示不再冒充验证码', async () => {
    const original=structuredClone(record('fixture-5'));
    for(const [access,label] of [['cooldown','平台冷却中'],['unavailable','访问受限'],['loginRequired','需要登录'],['loginPrompt','登录提示']]){
      Object.assign(record('fixture-5'),{platformKey:'kuaishou',platform:'快手直播',accessState:access,checkError:'页面暂不可读',verificationRequired:false,isLive:true,isRecording:false,recordingError:null});h.emit('update',record('fixture-5'));
      await visible(card('fixture-5').getByText(label,{exact:true}));
      assert.equal(await card('fixture-5').getByRole('button',{name:'重新验证',exact:true}).count(),0);
      assert.equal(await card('fixture-5').getByText('请在快手窗口完成验证',{exact:true}).count(),0);
      assert.equal(await card('fixture-5').getByRole('button',{name:'检查页面',exact:true}).count(),0);
      assert.equal(record('fixture-5').isLive,true,'an unreadable page is not evidence of offline');
    }
    Object.assign(record('fixture-5'),original,{accessState:original.accessState??''});h.emit('update',record('fixture-5'));
  });
  await step('卡片操作区在列表和网格、宽窄窗口保持对齐', async () => {
    for(const [width,height] of [[1280,960],[800,600],[420,960]]) {
      await page.setViewportSize({width,height});
      for(const list of [true,false]) {
        await page.getByRole('button',{name:list?'列表视图':'网格视图',exact:true}).click();
        const target=card('fixture-5');await target.scrollIntoViewIfNeeded();
        const layout=await target.evaluate(card=>{
          const rect=element=>{const r=element.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,cy:(r.top+r.bottom)/2};};
          const primary=[...card.querySelectorAll('.card-actions > button.card-record')].map(rect);
          const tools=[...card.querySelectorAll('.card-menu > summary')].map(rect);
          return {card:rect(card),buttons:[...primary,...tools],primary,tools,floating:card.querySelectorAll('.card-monitor-row button').length,overflow:document.documentElement.scrollWidth>innerWidth+1};
        });
        assert.equal(layout.floating,0);assert.equal(layout.overflow,false);
        assert.equal(layout.primary.length,1,'每张卡片保持一个主要录制动作');
        assert.equal(layout.tools.length,1,'次要动作使用唯一菜单入口');
        for(const button of layout.buttons) assert.ok(button.left>=layout.card.left&&button.right<=layout.card.right+1,'操作按钮不越过卡片');
        assert.ok(Math.abs(layout.primary[0].cy-layout.tools[0].cy)<=1,'主操作与菜单入口中线对齐');
        assert.ok(layout.primary[0].right<=layout.tools[0].left+1,'主要操作与菜单入口不重叠');
        const menu=await openCardMenu(target);
        const popup=await menu.locator('.card-menu-panel').evaluate(node=>{
          const box=node.getBoundingClientRect();
          const trigger=node.closest('.card-menu').querySelector('summary').getBoundingClientRect();
          const viewport=node.closest('#main-content').getBoundingClientRect();
          return {left:box.left,right:box.right,top:box.top,bottom:box.bottom,viewport:innerWidth,
            trigger:{top:trigger.top,bottom:trigger.bottom},viewportTop:viewport.top,viewportBottom:viewport.bottom,
            rows:[...node.querySelectorAll('button')].map(button=>{const r=button.getBoundingClientRect(),hit=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,hit:hit===button||button.contains(hit)};})};
        });
        assert.equal(popup.rows.length,3,'预览前置后，菜单只保留检测、编辑和移除');
        assert.ok(popup.rows.every(row=>row.hit),'菜单操作不被相邻玻璃卡片遮挡');
        assert.ok(popup.left>=0&&popup.right<=popup.viewport+1,'菜单不得横向溢出窗口');
        assert.ok(popup.top>=popup.trigger.bottom||popup.bottom<=popup.trigger.top,'展开的次要动作与主操作分区、不重叠；同帧测量避免滚动前后坐标混用');
        assert.ok(popup.top>=popup.viewportTop-1&&popup.bottom<=popup.viewportBottom+1,'菜单每一项都在主内容可视区内');
        assert.ok(Math.max(...popup.rows.map(row=>row.left))-Math.min(...popup.rows.map(row=>row.left))<=1,'菜单项目左侧对齐');
        assert.ok(Math.max(...popup.rows.map(row=>row.right))-Math.min(...popup.rows.map(row=>row.right))<=1,'菜单项目右侧对齐');
        for(let i=1;i<popup.rows.length;i++)assert.ok(popup.rows[i].top>=popup.rows[i-1].bottom-1,'菜单项目垂直排列且不重叠');
        await h.shot('actions-'+width+'-'+(list?'list':'grid'));
        await closeCardMenu(menu);
      }
    }
    await page.setViewportSize({width:1280,height:900});
    await page.getByRole('button',{name:'网格视图',exact:true}).click();
  });
  await step('卡片监控与画质同一行，空状态不占行，宽窄卡片紧凑且不溢出', async () => {
    const item=record('fixture-5'), original=structuredClone(item);
    Object.assign(item,{isRecording:false,isLive:false,monitorStatus:true,liveTitle:null,recordingError:null,
      checkError:null,verificationRequired:false,accessState:'',checkState:'idle',checkWaitReason:'interval',
      queuePosition:null,checkReadyAt:null,recoveryAt:null,recoveryAttempt:0,nextCheckAt:Math.floor(Date.now()/1000)+4500});
    h.emit('update',item);
    await visible(card(item.recId).getByText('自动监控，开播即录',{exact:true}));
    for(const width of [1120,900,420]) {
      await page.setViewportSize({width,height:900});
      await page.getByRole('button',{name:'网格视图',exact:true}).click();
      await card(item.recId).scrollIntoViewIfNeeded();
      const layout=await card(item.recId).evaluate(card=>{
        const rect=e=>{const r=e.getBoundingClientRect();return {x:r.x,right:r.right,cy:r.y+r.height/2,height:r.height,width:r.width}};
        return {card:rect(card),monitor:rect(card.querySelector('.card-monitor-row')),tags:rect(card.querySelector('.card-tags')),
          controls:[...card.querySelectorAll('.card-actions > button.card-record,.card-menu > summary')].map(rect),subtitle:card.querySelectorAll('.card-subtitle').length,
          emptySpeed:card.querySelectorAll('.card-speed').length,overflow:document.documentElement.scrollWidth>innerWidth+1};
      });
      assert.ok(Math.abs(layout.monitor.cy-layout.tags.cy)<=1,'监控状态与原画/格式应处于同一行');
      assert.equal(layout.subtitle,0,'等待开播已在徽标展示，不应再占用副标题行');
      assert.equal(layout.emptySpeed,0,'未录制时不为破折号预留速度区域');
      assert.ok(layout.card.height<290,'普通监控卡片不应因空行变高');
      assert.equal(layout.overflow,false);
      for(const control of layout.controls)assert.ok(control.x>=layout.card.x&&control.right<=layout.card.right+1);
      if(layout.card.width>=450&&width>520)assert.ok(Math.max(...layout.controls.map(r=>r.cy))-Math.min(...layout.controls.map(r=>r.cy))<=1);
      await h.shot('compact-card-'+width);
    }
    Object.assign(item,original);h.emit('update',item);await page.setViewportSize({width:1280,height:900});
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
    assert.equal(await card('fixture-5').locator('.card-state .ui-badge').innerText(), '录制异常');
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
    assert.equal(await card('fixture-5').locator('.card-state .ui-badge').innerText(), '录制中');
    await card('fixture-5').getByRole('button', { name: '停止录制', exact: true }).click();
    await visible(card('fixture-5').getByRole('button', { name: '单次录制', exact: true }));
    assert.equal(await card('fixture-5').locator('.card-recording-error').count(), 0);
    assert.equal(await card('fixture-5').locator('.card-state .ui-badge').innerText(), '直播未录制');
    Object.assign(record('fixture-5'), original);
    h.emit('update', record('fixture-5'));
    await visible(card('fixture-5').getByRole('button', { name: original.streamerName, exact: true }));
  });
  await step('当前录制状态常驻显示，不被标题或空标题覆盖', async () => {
    const item=record('fixture-5'), original=structuredClone(item);
    try {
      Object.assign(item,{isRecording:true,isLive:true,monitorStatus:true,recordingError:null,
        checkError:null,verificationRequired:false,accessState:'',checkState:'idle'});
      for(const title of ['你好',null,'','   ']) {
        item.liveTitle=title;h.emit('update',item);
        await visible(card(item.recId).locator('.card-recording-status'));
        assert.equal(await card(item.recId).locator('.card-recording-status').innerText(),'录制中');
        if(title?.trim()) await waitState(()=>card(item.recId).locator('.card-subtitle').innerText(),v=>v===title,'独立直播标题');
        else await waitState(()=>card(item.recId).locator('.card-subtitle').count(),n=>n===0,'空标题不占位');
      }
      item.monitorStatus=false;h.emit('update',item);
      await visible(card(item.recId).getByText('监控已暂停',{exact:true}));
      assert.equal(await card(item.recId).locator('.card-recording-status').innerText(),'录制中');
      await navigate('总览');
      await visible(page.locator(`.overview-task-row[data-rec-id="${item.recId}"] .overview-task-status`).getByText('录制中',{exact:true}));
      await navigate('录制任务');
      Object.assign(item,{isRecording:false,isLive:false,monitorStatus:true,liveTitle:null});h.emit('update',item);
      await hidden(card(item.recId).locator('.card-recording-status'));
      assert.equal(await card(item.recId).locator('.card-subtitle').count(),0);
      assert.equal(await card(item.recId).locator('.card-state .ui-badge').innerText(),'等待开播');
    } finally {Object.assign(item,original);h.emit('update',item);}
  });
  await step('录制与录后处理分离，历史完成结果隐藏且处理来源明确', async () => {
    const item=record('fixture-5'), original=structuredClone(item), originalJobs=structuredClone(state.mediaJobs);
    const job={id:'status-history',taskId:item.recId,source:'历史目录/历史录像_2026-09-27.ts',
      output:'历史目录/历史录像_2026-09-27.mp4',state:'complete',message:'MP4 已生成并校验，源 TS 已清理',
      sourceRemoved:true,deleteOriginal:true,elapsedSeconds:0,startedAt:null,finishedAt:null};
    try {
      Object.assign(item,{isRecording:true,isLive:true,monitorStatus:true,liveTitle:'你好',recordingError:null,
        checkError:null,verificationRequired:false,accessState:'',checkState:'idle'});h.emit('update',item);
      await visible(card(item.recId).locator('.card-recording-status'));
      state.mediaJobs.push(job);h.emit('mediaJob',job);
      assert.equal(await card(item.recId).locator('.card-media-job').count(),0);
      Object.assign(job,{state:'running',sourceRemoved:false,startedAt:Math.floor(Date.now()/1000)-12,
        message:'正在读取源文件音视频与时长'});h.emit('mediaJob',job);
      await visible(card(item.recId).locator('.card-media-job'));
      assert.equal(await card(item.recId).locator('.card-media-job-source').textContent(),'历史目录/历史录像_2026-09-27.ts');
      assert.equal(await card(item.recId).locator('.card-media-job-source').getAttribute('title'),job.source);
      assert.equal(await card(item.recId).locator('.card-recording-status').innerText(),'录制中');
      Object.assign(job,{state:'failed',finishedAt:Math.floor(Date.now()/1000),message:'转换失败，原 TS 保留'});h.emit('mediaJob',job);
      await visible(card(item.recId).locator('.card-media-job[data-state=failed]'));
      assert.equal(await card(item.recId).locator('.card-state .ui-badge').innerText(),'录制中');
      assert.equal(await card(item.recId).locator('.card-recording-error').count(),0);
      for(const [width,list,dark] of [[1280,true,false],[1280,false,true],[420,true,true],[420,false,false]]) {
        await page.setViewportSize({width,height:900});
        if(await page.locator('html').getAttribute('data-theme')!==(dark?'dark':'light')) {
          await page.getByRole('button',{name:dark?'切换到深色':'切换到浅色',exact:true}).click();
          await waitTheme(page,dark?'dark':'light');
        }
        await page.getByRole('button',{name:list?'列表视图':'网格视图',exact:true}).click();
        await card(item.recId).scrollIntoViewIfNeeded();
        const layout=await card(item.recId).evaluate(el=>{const c=el.getBoundingClientRect();return {inside:[...el.querySelectorAll('.card-recording-status,.card-media-job')].every(n=>{const r=n.getBoundingClientRect();return r.x>=c.x&&r.right<=c.right+1;}),overflow:document.documentElement.scrollWidth>innerWidth+1};});
        assert.equal(layout.inside,true);assert.equal(layout.overflow,false);
        await h.shot('status-active-'+width+'-'+(list?'list':'grid'));
      }
      Object.assign(job,{state:'complete',startedAt:null,finishedAt:null,elapsedSeconds:0,
        message:'MP4 已生成并校验，源 TS 已清理'});h.emit('mediaJob',job);
      await hidden(card(item.recId).locator('.card-media-job'));
      Object.assign(item,{isRecording:false,isLive:false,liveTitle:null});h.emit('update',item);
      await hidden(card(item.recId).locator('.card-media-job'));
      assert.equal(await card(item.recId).locator('.card-state .ui-badge').innerText(),'等待开播');
      for(const width of [1280,420]) {
        await page.setViewportSize({width,height:900});
        for(const list of [true,false]) {
          await page.getByRole('button',{name:list?'列表视图':'网格视图',exact:true}).click();
          await card(item.recId).scrollIntoViewIfNeeded();
          assert.equal(await card(item.recId).locator('.card-media-job').count(),0);
          const layout=await inspectLayout(page);
          assert.equal(layout.contentOverflow,false);assert.deepEqual(layout.overflow,[]);
        }
      }
      await h.shot('status-history-narrow');
    } finally {
      Object.assign(item,original);state.mediaJobs=originalJobs;
      await page.setViewportSize({width:1280,height:900});await go(page,h.base,'/recordings');
    }
  });
  await step('已完成任务不论耗时数据是否完整都不显示完成描述，失败仍可查看', async () => {
    const originalJobs=structuredClone(state.mediaJobs);
    const ids=['fixture-2','fixture-3','fixture-5'], originalRecords=ids.map(id=>structuredClone(record(id)));
    try {
      for(const id of ids)Object.assign(record(id),{isRecording:false});
      const base={output:'history.mp4',state:'complete',message:'MP4 已生成并校验；原 TS 保留',
        sourceRemoved:false,deleteOriginal:false,elapsedSeconds:0,startedAt:null,finishedAt:null};
      state.mediaJobs=[{...base,id:'unknown-time',taskId:ids[0],source:'unknown.ts'},
        {...base,id:'zero-time',taskId:ids[1],source:'zero.ts',startedAt:100,finishedAt:100},
        {...base,id:'saved-time',taskId:ids[2],source:'saved.ts',startedAt:100,elapsedSeconds:7}];
      await go(page,h.base,'/recordings');
      for(const id of ids){
        const status=card(id).locator('.card-media-job');
        await hidden(status);
        assert.equal(await card(id).getByText('处理详情',{exact:true}).count(),0);
      }
      for(const [index,expected] of [[0,'耗时未知'],[1,'耗时 00:00:00'],[2,'耗时 00:00:07']]){
        const job=state.mediaJobs[index];
        Object.assign(job,{state:'failed',message:'转换失败，源 TS 保留'});h.emit('mediaJob',job);
        const status=card(ids[index]).locator('.card-media-job');
        await visible(status);
        assert.equal(await status.locator('.media-job-detail').innerText(),expected);
        await status.locator('summary').click();
        await visible(status.getByText('转换失败，源 TS 保留',{exact:true}));
      }
    } finally {state.mediaJobs=originalJobs;for(const original of originalRecords)Object.assign(record(original.recId),original);await go(page,h.base,'/recordings');}
  });
  await step('录制、直播未录制和下播复核各归一类，旧直播状态不重复计数', async () => {
    const r=record('fixture-5'), original=structuredClone(r);
    const filter=name=>page.getByRole('button',{name:new RegExp('^'+name)});
    const all=()=>filter('全部').click();
    Object.assign(r,{isLive:true,isRecording:true,monitorStatus:true,recordingError:null,checkError:null,verificationRequired:false,accessState:'',checkState:'idle'}); h.emit('update',r);
    await filter('录制中').click();await visible(card(r.recId));
    await filter('直播未录制').click();await hidden(card(r.recId));
    Object.assign(r,{isRecording:false});h.emit('update',r);await visible(card(r.recId));
    assert.equal(await card(r.recId).locator('.card-state .ui-badge').innerText(),'直播未录制');
    Object.assign(r,{checkState:'rechecking',recordedSeconds:2584.9});h.emit('update',r);await hidden(card(r.recId));
    await all();await visible(card(r.recId).getByText('录制结束，复核中',{exact:true}));
    assert.equal(await card(r.recId).locator('.card-check-error').count(),0);
    Object.assign(r,{checkState:'idle',checkError:'平台返回空响应（未验证直播状态）'});h.emit('update',r);
    await visible(card(r.recId).getByText('录制已结束，直播状态待确认',{exact:true}));
    await filter('直播未录制').click();await hidden(card(r.recId));
    Object.assign(r,{isLive:false,liveTitle:null,checkError:null});h.emit('update',r);
    await filter('等待开播').click();await visible(card(r.recId));
    assert.equal(await card(r.recId).locator('.card-state .ui-badge').innerText(),'等待开播');
    assert.equal(await card(r.recId).locator('.card-check-error').count(),0);
    const counts=await page.locator('.filter-tab .filter-count').allTextContents();
    assert.equal(counts.slice(1).reduce((a,n)=>a+Number(n),0),Number(counts[0]));
    Object.assign(r,original);h.emit('update',r);await all();
  });
  await step('删除确认、取消与 SSE 删除后无信号生命周期错误', async () => {
    const before = requests('DELETE', '/api/recordings/fixture-7').length;
    await cardMenuAction(card('fixture-7'), '移除任务');
    const modal = dialog('移除直播间'); await visible(modal);
    await modal.getByRole('button', { name: '取消', exact: true }).click(); await hidden(modal);
    assert.equal(requests('DELETE', '/api/recordings/fixture-7').length, before);
    await cardMenuAction(card('fixture-7'), '移除任务');
    await modal.getByRole('button', { name: '确认移除' }).click();
    await hidden(card('fixture-7'));
    await waitState(() => requests('DELETE', '/api/recordings/fixture-7').length, (n) => n === before + 1, '仅删除选定任务');
    await waitState(() => page.getByRole('textbox',{name:'搜索直播间',exact:true}).isEnabled(), Boolean, '卡片删除后父层操作计数仍须释放');
    assert.ok(record('fixture-8'));
  });
  await step('同名任务菜单互斥，Escape 关闭并恢复入口焦点，不触发业务操作', async () => {
    const mutations=()=>state.requests.filter(request=>/^(POST|PUT|PATCH|DELETE)$/.test(request.method)).length;
    const before=mutations();
    const first=await openCardMenu(card('fixture-1'));
    const second=await openCardMenu(card('fixture-5'));
    await waitState(()=>first.evaluate(node=>node.open),value=>!value,'打开第二个同名菜单关闭第一个');
    assert.equal(await page.locator('details.card-menu[open]').count(),1);
    assert.equal(await second.evaluate(node=>node.open),true);
    await closeCardMenu(second);
    assert.equal(await page.locator('details.card-menu[open]').count(),0);
    const editable=card('fixture-2');
    const trigger=editable.locator('details.card-menu > summary');
    for(const attempt of [1,2]) {
      await cardMenuAction(editable,'编辑任务');
      const modal=dialog('编辑直播间');
      await visible(modal);
      await page.keyboard.press('Escape');
      await hidden(modal);
      assert.equal(await trigger.evaluate(node=>node===document.activeElement),true,
        `第 ${attempt} 次编辑关闭后焦点必须回到该卡片的可见菜单入口，而不是 BODY`);
      assert.equal(await editable.locator('details.card-menu').evaluate(node=>node.open),false);
    }
    assert.equal(mutations(),before,'打开和关闭菜单不提交任务操作');
  });
  await step('任务菜单图标与文字齐全，悬停与键盘焦点显示禁用原因', async () => {
    const menu=await openCardMenu(card('fixture-1'));
    const tools=menu.locator('.card-menu-panel');
    assert.equal(await tools.getByRole('button').count(),3);
    const bounds=await tools.getByRole('button').evaluateAll(nodes=>nodes.map(node=>({width:node.getBoundingClientRect().width,height:node.getBoundingClientRect().height,text:node.textContent.trim(),icon:!!node.querySelector('svg')})));
    assert.ok(bounds.every(item=>item.width===bounds[0].width&&item.height===bounds[0].height&&item.height>=24&&item.text.length>0&&item.icon),'三个次要动作保留等宽尺寸、图标与可读标签');
    const edit=tools.getByRole('button',{name:'编辑任务'});assert.equal(await edit.isDisabled(),true);
    const hint=edit.locator('..');await hint.focus();
    assert.equal(await hint.evaluate(node=>getComputedStyle(node,'::after').visibility),'visible');
    assert.match(await hint.getAttribute('data-tooltip'),/请先停止录制/);
    await tools.getByRole('button',{name:'检测直播状态'}).hover();
    await h.shot('card-icon-toolbar');
    await closeCardMenu(menu);
  });
  await step('TS 录后与录中预览实际播放，关闭后释放播放器', async () => {
    for(const live of [false,true]) {
      state.previewLive=live;
      await openRecordingPreview(card('fixture-1'));const modal=dialog('录制预览');
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
    await openRecordingPreview(card('fixture-1'));const modal=dialog('录制预览');
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
  await step('无文件时也能一键打开小窗，键盘可展开关闭且不自动拉直播源', async () => {
    const pattern='**/api/recordings/fixture-1/files';
    const sourceRequests=requests('GET','/api/recordings/fixture-1/preview').length;
    const handler=route=>route.fulfill({contentType:'application/json',body:JSON.stringify({dir:null,files:[]})});
    await page.route(pattern,handler);
    const modal=dialog('录制预览');
    try {
      const trigger=card('fixture-1').getByRole('button',{name:'小窗预览',exact:true});
      await trigger.focus();await trigger.press('Enter');
      await visible(modal.getByText('还没有录制文件',{exact:true}));
      assert.equal(await modal.evaluate(el=>el.matches(':modal')),false);
      assert.equal(await modal.locator('video').count(),0);
      assert.equal(requests('GET','/api/recordings/fixture-1/preview').length,sourceRequests);
      await modal.getByRole('button',{name:'恢复预览',exact:true}).press('Enter');
      assert.equal(await modal.evaluate(el=>el.matches(':modal')),true);
      await page.keyboard.press('Escape');await hidden(modal);
      assert.equal(await page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),0);
    } finally {
      if(await modal.isVisible())await modal.getByRole('button',{name:'关闭对话框',exact:true}).click();
      await page.unroute(pattern,handler);
    }
  });
  await step('小窗直接切换直播源与录像，单实例播放且关闭不停止录制', async () => {
    const current=record('fixture-1'), wasLive=current.isLive;
    current.isLive=true;h.emit('update',current);
    const stops=requests('POST','/api/recordings/fixture-1/stop').length;
    await card('fixture-1').getByRole('button',{name:'小窗预览',exact:true}).click();
    const modal=dialog('录制预览');
    await page.waitForFunction(()=>document.querySelector('video')?.readyState>=2);
    const file=await modal.locator('.media-preview').getAttribute('data-path');
    for(const width of [1280,800,375]){
      await page.setViewportSize({width,height:900});
      const box=await modal.boundingBox();
      assert.ok(box.x>=0&&box.y>=0&&box.x+box.width<=width+1&&box.y+box.height<=900,'小窗始终位于窗口内');
      await visible(modal.getByRole('button',{name:'恢复预览',exact:true}));
      await visible(modal.getByRole('button',{name:'预览直播源',exact:true}));
      await visible(modal.getByRole('button',{name:'切换到录制文件',exact:true}));
    }
    await page.setViewportSize({width:1280,height:900});
    await modal.getByRole('button',{name:'预览直播源',exact:true}).click();
    await waitState(()=>modal.locator('.media-preview').getAttribute('data-path'),v=>v==='live:fixture-1','小窗内切换直播源');
    await waitState(()=>modal.locator('.preview-eyebrow').textContent(),v=>v==='直播源','来源标题跟随实际播放内容');
    await page.waitForFunction(()=>document.querySelector('video')?.readyState>=2);
    assert.equal(await modal.evaluate(el=>el.matches(':modal')),false);
    assert.equal(await page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),1);
    await modal.getByRole('button',{name:'切换到录制文件',exact:true}).click();
    await waitState(()=>modal.locator('.media-preview').getAttribute('data-path'),v=>v===file,'回到原录制文件');
    await waitState(()=>modal.locator('.preview-eyebrow').textContent(),v=>v==='录制文件','切回录像后恢复来源标题');
    await page.waitForFunction(()=>document.querySelector('video')?.readyState>=2);
    await modal.getByRole('button',{name:'暂停',exact:true}).click();
    await page.locator('video').evaluate(v=>{window.sameTargetPreview=v;});
    await openRecordingPreview(card('fixture-1'));
    assert.equal(await modal.evaluate(el=>el.matches(':modal')),true);
    assert.equal(await page.locator('video').evaluate(v=>v===window.sameTargetPreview&&v.paused),true,'同目标预览按钮只展开，不重载');
    await modal.getByRole('button',{name:'关闭对话框',exact:true}).click();await hidden(modal);
    assert.equal(await page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),0);
    await waitState(()=>state.activePreviews,n=>n===0,'关闭小窗释放媒体连接');
    assert.equal(requests('POST','/api/recordings/fixture-1/stop').length,stops);
    current.isLive=wasLive;h.emit('update',current);
  });
  await step('一键小窗跨页保留同一播放器，重复打开不重载，设置仍可操作', async () => {
    const mutationsBefore=state.requests.filter(r=>/^(POST|PUT|DELETE|PATCH)$/.test(r.method)).length;
    const miniButton=card('fixture-1').getByRole('button',{name:'小窗预览',exact:true});
    await visible(miniButton);await miniButton.click();const modal=dialog('录制预览');
    await page.waitForFunction(()=>document.querySelector('video')?.currentTime>.2);
    const original=await page.locator('video').evaluate(v=>{window.miniPreviewVideo=v;return {time:v.currentTime,loads:v.dataset.segmentLoads};});
    assert.equal(await miniButton.getAttribute('aria-pressed'),'true');
    await miniButton.click();
    assert.equal(await page.locator('video').evaluate(v=>v===window.miniPreviewVideo),true,'重复点击同一目标不重建播放器');
    assert.equal(await page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),1);
    assert.equal(state.requests.filter(r=>/^(POST|PUT|DELETE|PATCH)$/.test(r.method)).length,mutationsBefore,'预览不启动或修改录制任务');
    await visible(modal.getByRole('button',{name:'恢复预览',exact:true}));
    await page.waitForFunction(()=>document.querySelector('.preview-shell')?.getAttribute('aria-modal')==='false');
    assert.equal(await modal.evaluate(node=>node.matches(':modal')),false);
    await h.shot('one-click-mini-preview');
    await navigate('偏好设置');await page.getByRole('button',{name:'网络与检测',exact:true}).click();
    const interval=page.getByRole('textbox',{name:/检测间隔/});const saved=await interval.inputValue();
    await interval.fill('31');assert.equal(await interval.inputValue(),'31');await interval.fill(saved);
    await page.waitForFunction(time=>{const v=document.querySelector('video');return v===window.miniPreviewVideo&&!v.paused&&v.currentTime>time+.1;},original.time);
    await modal.getByRole('button',{name:'暂停',exact:true}).click();
    const paused=await page.locator('video').evaluate(v=>v.currentTime);
    for(const [width,height] of [[800,600],[620,760]]) {
      await page.setViewportSize({width,height});
      const box=await modal.boundingBox();assert.ok(box.x>=0&&box.y>=0&&box.x+box.width<=width&&box.y+box.height<=height);
      await h.shot('mini-preview-settings-right-'+width);
      await modal.getByRole('button',{name:'小窗移到左侧',exact:true}).press('Enter');
      await page.waitForFunction(()=>document.querySelector('.preview-shell')?.classList.contains('preview-docked-left'));
      const moved=await modal.boundingBox();assert.ok(moved.x>=0&&moved.x<box.x&&moved.y>=0&&moved.x+moved.width<=width&&moved.y+moved.height<=height);
      assert.equal(await page.locator('video').evaluate(v=>v===window.miniPreviewVideo&&v.paused),true,'停靠不重建播放器或恢复暂停的视频');
      assert.equal(await page.evaluate(async()=>(await import('/media-player.js')).activePlayerCount()),1);
      assert.ok(Math.abs(await page.locator('video').evaluate(v=>v.currentTime)-paused)<.1);
      await interval.click({timeout:5000});assert.equal(await interval.evaluate(el=>el===document.activeElement),true,'切换停靠后可以用鼠标点击原本被遮挡的设置字段');
      await h.shot('mini-preview-settings-left-'+width);
      await modal.getByRole('button',{name:'小窗移到右侧',exact:true}).click();
    }
    await page.setViewportSize({width:1280,height:900});
    await modal.getByRole('button',{name:'恢复预览',exact:true}).click();
    assert.equal(await modal.evaluate(node=>node.matches(':modal')),true);
    assert.equal(await page.locator('video').evaluate(v=>v===window.miniPreviewVideo&&v.paused),true);
    assert.ok(Math.abs(await page.locator('video').evaluate(v=>v.currentTime)-paused)<.1);
    assert.equal(await page.locator('video').getAttribute('data-segment-loads'),original.loads);
    await modal.getByRole('button',{name:'最小化预览',exact:true}).click();
    await navigate('媒体库');await page.getByRole('button',{name:'返回媒体库根目录',exact:true}).click();
    await page.getByRole('button',{name:/^清晨片段.wav/}).click();
    const audio=dialog('媒体预览');await visible(audio.getByLabel('录制音频预览'));
    assert.equal(await page.locator('video').count(),0);assert.equal(await page.locator('dialog[open].preview-shell').count(),1);
    await audio.getByRole('button',{name:'最小化预览',exact:true}).click();await navigate('总览');
    await visible(audio.getByLabel('录制音频预览'));
    await audio.getByRole('button',{name:'关闭对话框',exact:true}).click();
    await page.waitForFunction(async()=>(await import('/media-player.js')).activePlayerCount()===0);
    assert.equal(await page.locator('audio').count(),0);
  });
  await step('任务预览同步改名和下播，删除后取消轮询且迟到结果不复活', async () => {
    const rec={...seedRecordings()[0],recId:'preview-state-regression',streamerName:'预览同步回归'};
    state.recordings.push(rec);h.emit('update',rec);await navigate('录制任务');
    await visible(card(rec.recId));await openRecordingPreview(card(rec.recId));
    const modal=dialog('录制预览');await page.waitForFunction(()=>document.querySelector('video')?.currentTime>.1);
    await modal.getByRole('button',{name:'暂停',exact:true}).click();
    const before=await page.locator('video').evaluate(v=>{window.synchronizedPreview=v;return v.currentTime;});
    rec.streamerName='后台同步后的主播';rec.isLive=false;h.emit('update',rec);
    await waitState(()=>modal.locator('.preview-title').innerText(),text=>text===rec.streamerName,'预览同步最新名字');
    assert.equal(await modal.getByRole('button',{name:'预览直播源',exact:true}).count(),0);
    assert.equal(await page.locator('video').evaluate(v=>v===window.synchronizedPreview&&v.paused),true);
    assert.ok(Math.abs(await page.locator('video').evaluate(v=>v.currentTime)-before)<.1);
    await modal.getByRole('button',{name:'最小化预览',exact:true}).click();await navigate('偏好设置');
    const endpoint='/api/recordings/'+rec.recId+'/files';
    h.delayNext('GET',endpoint,1800);
    const previous=requests('GET',endpoint).length;
    await waitState(()=>requests('GET',endpoint).length,count=>count>previous,'慢轮询已发出');
    state.recordings=state.recordings.filter(r=>r.recId!==rec.recId);h.emit('delete',[rec.recId]);
    await hidden(modal);await page.waitForFunction(async()=>(await import('/media-player.js')).activePlayerCount()===0);
    const count=requests('GET',endpoint).length;await new Promise(resolve=>setTimeout(resolve,2300));
    assert.equal(requests('GET',endpoint).length,count,'删除后不新增文件请求');
    assert.equal(await page.locator('dialog[open].preview-shell').count(),0,'迟到响应不得恢复预览');
    assert.equal(await page.locator('video').count(),0);
  });
  await step('文件轮询超时显示错误且下一次请求恢复，不复用已取消控制器', async () => {
    const rec={...seedRecordings()[0],recId:'preview-timeout-regression',streamerName:'预览超时恢复'};
    state.recordings.push(rec);h.emit('update',rec);await navigate('录制任务');await visible(card(rec.recId));
    const endpoint='/api/recordings/'+rec.recId+'/files';h.delayNext('GET',endpoint,12500);
    await openRecordingPreview(card(rec.recId));const modal=dialog('录制预览');
    await eventually(()=>modal.locator('[role="alert"]').count(),count=>count===1,'单次超时必须显示错误',14500);
    assert.equal(await modal.locator('.loading-state').count(),0);
    await eventually(()=>modal.locator('video').count(),count=>count===1,'下一次轮询恢复播放',6000);
    assert.ok(requests('GET',endpoint).length>=2);assert.equal(await modal.locator('[role="alert"]').count(),0);
    state.recordings=state.recordings.filter(r=>r.recId!==rec.recId);h.emit('delete',[rec.recId]);await hidden(modal);
    await page.waitForFunction(async()=>(await import('/media-player.js')).activePlayerCount()===0);
  });
  await step('媒体排序支持三种字段、双方向和跨页记忆，目录始终优先', async () => {
    await navigate('媒体库');await page.getByRole('button',{name:'返回媒体库根目录',exact:true}).click();
    const rows=page.locator('.file-table tbody tr');await waitState(()=>rows.count(),n=>n===4,'媒体列表');
    const names=()=>rows.locator('.file-name strong').allTextContents();
    const expected=(key,desc)=>state.files.filter(file=>!file.path.includes('/')).sort((a,b)=>Number(b.isDir)-Number(a.isDir)||
      (key==='size'?(desc?b.size-a.size:a.size-b.size):key==='modified'?(desc?b.modified-a.modified:a.modified-b.modified):
       (a.name.toLowerCase()<b.name.toLowerCase()?-1:a.name.toLowerCase()>b.name.toLowerCase()?1:0)*(desc?-1:1))||
      (a.name<b.name?-1:a.name>b.name?1:0)).map(f=>f.name);
    const sorting=page.getByRole('combobox',{name:'媒体排序',exact:true});const direction=page.getByRole('button',{name:'切换排序方向',exact:true});
    for(const key of ['name','modified','size']){
      await sorting.selectOption(key);
      for(const desc of [false,true]){
        if((await direction.getAttribute('aria-pressed'))!==String(desc))await direction.click();
        await waitState(names,actual=>JSON.stringify(actual)===JSON.stringify(expected(key,desc)),key+' 排序');
      }
    }
    await navigate('总览');await navigate('媒体库');assert.equal(await sorting.inputValue(),'size');assert.equal(await direction.getAttribute('aria-pressed'),'true');
    await page.setViewportSize({width:375,height:820});const layout=await inspectLayout(page);assert.deepEqual(layout.overflow,[]);await h.shot('storage-sort-narrow');
    await page.setViewportSize({width:1280,height:900});await sorting.selectOption('name');await direction.click();
  });
  await step('媒体库筛选、目录导航与真正的内嵌音频加载', async () => {
    await navigate('媒体库');
    await waitState(() => page.locator('.file-table tbody tr').count(), (n) => n === 4, '根目录文件');
    await assertFullWidthPage('/storage');
    await h.shot('10-storage');
    await page.getByRole('button', { name: '音频', exact: true }).click();
    assert.equal(await page.locator('.file-table tbody tr').count(), 1);
    await page.locator('.file-table tr[data-path="清晨片段.wav"]').getByRole('button', { name: '清晨片段.wav', exact: true }).click();
    const modal = dialog('媒体预览');
    await visible(modal.getByLabel('录制音频预览'));
    await page.waitForFunction(() => document.querySelector('audio')?.readyState >= 1);
    await modal.getByRole('button', { name: '关闭对话框' }).click(); await hidden(modal);
    assert.equal(await page.locator('audio').count(), 0);
    await page.getByRole('button', { name: '全部文件', exact: true }).click();
    await page.locator('.file-table tr[data-path="云间电台"]').getByRole('button', { name: '云间电台', exact: true }).click();
    await visible(page.locator('.file-table tr[data-path="云间电台/周末音乐.ts"]').getByRole('button', { name: '周末音乐.ts', exact: true }));
    await page.getByRole('button', { name: '返回媒体库根目录' }).click();
    await visible(page.locator('.file-table tr[data-path="录制说明.txt"]').getByRole('button', { name: '录制说明.txt', exact: true }));
  });
  await step('媒体库先返回目录，体积异步查询且不阻塞切页', async () => {
    state.storageSizeDelay=1800;
    await navigate('媒体库');
    await page.getByRole('button',{name:'返回媒体库根目录',exact:true}).click();
    await page.getByRole('button',{name:'刷新文件',exact:true}).click();
    await visible(page.locator('.file-name').filter({hasText:'海边日落.ts'}));
    await visible(page.getByText('体积计算中…',{exact:true}));
    await navigate('录制任务');
    state.storageSizeDelay=25;
    await navigate('媒体库');
    await visible(page.locator('.file-name').filter({hasText:'海边日落.ts'}));
  });
  await step('目录体积超时仅取消当前请求，切页仍取消整批且清除监听', async () => {
    const originalFiles = structuredClone(state.files);
    await navigate('录制任务');
    state.files = [1, 2, 3, 4].map(n => ({ name: '超时回归' + n, path: '超时回归' + n, isDir: true, size: n * 1024, modified: 0 }));
    await page.evaluate(() => {
      const originalFetch = window.fetch, originalTimeout = window.setTimeout;
      const audit = window.__sizeTimeoutAudit = { requests: [], timers: [] };
      window.setTimeout = (callback, milliseconds, ...args) => {
        const id = originalTimeout.call(window, callback, milliseconds, ...args);
        if (milliseconds === 120000) audit.timers.push(() => callback(...args));
        return id;
      };
      window.fetch = (input, init) => {
        const url = new URL(input instanceof Request ? input.url : input, location.href);
        if (url.pathname !== '/api/storage/size') return originalFetch.call(window, input, init);
        const signal = init?.signal ?? input.signal;
        const request = { path: url.searchParams.get('path'), signal, settled: false };
        audit.requests.push(request);
        return new Promise((resolve, reject) => {
          const finish = (cancelled = false) => {
            if (request.settled) return;
            request.settled = true;
            signal.removeEventListener('abort', cancel);
            if (cancelled) reject(new DOMException('Synthetic request cancelled', 'AbortError'));
            else resolve(new Response(JSON.stringify({ size: Number(request.path.slice(-1)) * 1024 }), { headers: { 'Content-Type': 'application/json' } }));
          };
          const cancel = () => finish(true);
          request.complete = () => finish();
          request.cancel = cancel;
          signal.addEventListener('abort', cancel, { once: true });
          if (signal.aborted) cancel();
        });
      };
      audit.restore = () => {
        for (const request of audit.requests) request.cancel();
        window.fetch = originalFetch;
        window.setTimeout = originalTimeout;
        delete window.__sizeTimeoutAudit;
      };
    });
    try {
      await navigate('媒体库');
      await page.waitForFunction(() => window.__sizeTimeoutAudit.requests.length === 2);
      await page.evaluate(() => window.__sizeTimeoutAudit.timers[0]());
      await page.waitForFunction(() => window.__sizeTimeoutAudit.requests.length >= 3);
      assert.deepEqual(await page.evaluate(() => window.__sizeTimeoutAudit.requests.slice(0, 3).map(request => request.signal.aborted)), [true, false, false], '首项超时不能中止并发或后续请求');
      await page.evaluate(() => window.__sizeTimeoutAudit.requests.slice(1).forEach(request => request.complete()));
      await page.waitForFunction(() => window.__sizeTimeoutAudit.requests.length === 4);
      await page.evaluate(() => window.__sizeTimeoutAudit.requests[3].complete());
      await visible(page.locator('tr[data-path="超时回归1"] .file-size').getByText('体积不可用', { exact: true }));
      for (const n of [2, 3, 4]) {
        await waitState(() => page.locator(`tr[data-path="超时回归${n}"] .file-size`).innerText(), text => text === n.toFixed(1) + ' KB', '其他目录体积正常完成');
      }
      await page.getByRole('button', { name: '刷新文件', exact: true }).click();
      await page.waitForFunction(() => window.__sizeTimeoutAudit.requests.length === 6);
      await navigate('录制任务');
      await page.waitForFunction(() => window.__sizeTimeoutAudit.requests.slice(4).every(request => request.signal.aborted && request.settled));
      assert.equal(await page.evaluate(() => window.__sizeTimeoutAudit.requests.length), 6, '切页后不再执行排队目录');
    } finally {
      await navigate('录制任务');
      await page.evaluate(() => window.__sizeTimeoutAudit.restore());
      state.files = originalFiles;
      await navigate('媒体库');
      await visible(page.locator('.file-name').filter({ hasText: '海边日落.ts' }));
    }
  });
  await step('保存目录切换被拒绝时保留预览，成功切换后释放旧目录播放器', async () => {
    const originalRoot = state.userConfig.live_save_path;
    const nextRoot = 'X:/Fixture/NextRecordings';
    const blockedMessage = '请先停止录制并等待媒体处理完成，再更改保存目录';
    try {
      await navigate('媒体库');
      await page.getByRole('button', { name: '返回媒体库根目录', exact: true }).click();
      await page.locator('.file-name').filter({ hasText: '海边日落.ts' }).click();
      const modal = dialog('媒体预览'); await visible(modal);
      await page.waitForFunction(() => document.querySelector('video')?.videoWidth > 0);
      await page.evaluate(() => { window.rootChangePreviewVideo = document.querySelector('video'); });
      await modal.getByRole('button', { name: '最小化预览', exact: true }).click();
      await navigate('偏好设置');
      const path = page.getByRole('textbox', { name: /录制保存位置/ });
      await path.fill(nextRoot);
      assert.equal(await page.locator('video').evaluate(video => video === window.rootChangePreviewVideo), true);
      h.failNext('PUT', '/api/settings', blockedMessage, 409);
      await page.locator('.settings-savebar').getByRole('button', { name: '保存修改', exact: true }).click();
      await visible(page.locator('.form-feedback').getByText(blockedMessage, { exact: true }));
      assert.equal(state.userConfig.live_save_path, originalRoot);
      assert.equal(await path.inputValue(), nextRoot, '拒绝保存时保留用户草稿');
      await visible(modal);
      assert.equal(await page.locator('video').evaluate(video => video === window.rootChangePreviewVideo), true);
      await page.locator('.settings-savebar').getByRole('button', { name: '保存修改', exact: true }).click();
      await waitState(() => state.userConfig.live_save_path, value => value === nextRoot, '空闲时切换目录');
      await hidden(modal);
      await page.waitForFunction(async () => (await import('/media-player.js')).activePlayerCount() === 0);
      assert.equal(await page.evaluate(() => window.rootChangePreviewVideo.isConnected), false);
      await hidden(page.locator('.settings-savebar'));
    } finally {
      state.userConfig.live_save_path = originalRoot; h.emit('settings', {});
      await navigate('媒体库');
      await visible(page.locator('.file-name').filter({ hasText: '海边日落.ts' }));
    }
  });
  await step('媒体库删除只修改模拟目录并要求确认', async () => {
    await page.getByRole('button', { name: '回收录制说明.txt', exact: true }).click();
    const modal = dialog('移至回收站'); await visible(modal);
    await modal.getByRole('button', { name: '取消', exact: true }).click();
    assert.ok(state.files.some((f) => f.name === '录制说明.txt'));
    await page.getByRole('button', { name: '回收录制说明.txt', exact: true }).click();
    await modal.getByRole('button', { name: '移到回收站', exact: true }).click(); await hidden(modal);
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
    const row=page.locator('.file-table tr[data-path="海边日落.ts"]');
    const control=row.locator('.file-convert-control'), hint=control.getByRole('tooltip');
    await page.getByRole('heading',{name:'媒体库',exact:true}).hover();
    await hidden(hint);
    const height=(await row.boundingBox()).height;
    await control.hover();await visible(hint);
    assert.equal((await row.boundingBox()).height,height,'悬停提示不撑高文件行');
    await page.getByRole('heading',{name:'媒体库',exact:true}).hover();
    await control.focus();await visible(hint);
    assert.equal(await control.getAttribute('aria-label'),'同名 MP4 已存在');
    assert.equal(await row.locator('.file-action-hint').count(),0);
    await page.getByRole('button',{name:'刷新文件',exact:true}).focus();
    await hidden(hint);
    await h.shot('storage-compact-no-overwrite');
  });
  await step('媒体库不展示侧栏，后台仍保留超过八项队列且任务卡显示进度', async () => {
    const original=structuredClone(state.mediaJobs);
    try {
      state.mediaJobs=[];
      for(let i=0;i<12;i++){
        const job={id:'full-queue-'+i,taskId:i===0?'fixture-5':i===11?'fixture-6':null,source:'queue-fixture-'+i+'.ts',output:'queue-fixture-'+i+'.mp4',state:i===0?'running':'waiting',deleteOriginal:false,sourceRemoved:false,message:i===0?'正在无损转封装为 MP4':'等待转 MP4；原 TS 保留',stagePercent:i===0?37:null,stageEtaSeconds:i===0?5:null,startedAt:i===0?Math.floor(Date.now()/1000)-2:null,finishedAt:null,elapsedSeconds:2,queuePosition:i===0?null:i};
        state.mediaJobs.push(job);
      }
      await go(page,h.base,'/storage');
      await assertFullWidthPage('/storage');
      const stored=await readFixtureJobs();
      assert.deepEqual(stored.map(job=>job.id),state.mediaJobs.map(job=>job.id),'没有队列栏不截断后台快照');
      assert.equal(stored.length,12);
      await navigate('录制任务');
      const active=card('fixture-5').locator('.card-media-job'), tail=card('fixture-6').locator('.card-media-job');
      await waitState(()=>active.getAttribute('data-job-id'),id=>id==='full-queue-0','最早活动任务未丢失');
      await waitState(()=>tail.getAttribute('data-job-id'),id=>id==='full-queue-11','第十二项仍进入客户端任务状态');
      await visible(active.getByText('本阶段 37%',{exact:false}));
      assert.equal(await active.getByRole('progressbar',{name:'本阶段进度',exact:true}).getAttribute('value'),'37');
      assert.equal(await tail.locator('.media-job-detail').innerText(),'等待处理 · 第 11 位');
      await page.reload({waitUntil:'domcontentloaded'});
      await waitState(()=>active.getAttribute('data-job-id'),id=>id==='full-queue-0','重载后活动任务仍在');
      await waitState(()=>tail.getAttribute('data-job-id'),id=>id==='full-queue-11','重载后等待任务仍在');
      assert.equal((await readFixtureJobs()).length,12);
      await h.shot('full-media-queue');
    } finally {state.mediaJobs=original;h.emit('resync',{});}
  });
  await step('当前转换不被后续分段遮住，进度更新保留详情节点', async () => {
    const originalJobs=structuredClone(state.mediaJobs);
    try {
      const active=seedMediaJob({id:'active-segment',taskId:'fixture-5',source:'active.ts',state:'running',message:'正在无损转封装为 MP4',stagePercent:37,startedAt:Math.floor(Date.now()/1000)-10});
      const waiting=seedMediaJob({id:'next-segment',taskId:'fixture-5',source:'next.ts',queuePosition:1});
      state.mediaJobs=[...originalJobs,active,waiting];
      await go(page,h.base,'/recordings');
      const status=card('fixture-5').locator('.card-media-job');
      await waitState(()=>status.getAttribute('data-job-id'),id=>id===active.id,'显示活动分段');
      await status.locator('summary').click();
      await status.evaluate(el=>el.setAttribute('data-instance-check','retained'));
      active.stagePercent=61;h.emit('mediaJob',active);
      await waitState(()=>status.locator('progress').getAttribute('value'),v=>v==='61','帧进度更新');
      assert.equal(await status.getAttribute('data-instance-check'),'retained');
      assert.equal(await status.locator('details').getAttribute('open'),'');
      Object.assign(active,{state:'complete',finishedAt:Math.floor(Date.now()/1000)});h.emit('mediaJob',active);
      await waitState(()=>status.getAttribute('data-job-id'),id=>id===waiting.id,'显示队列首个分段');
      await h.shot('processing-segment-priority');
    } finally {state.mediaJobs=originalJobs;await go(page,h.base,'/storage');}
  });
  await step('侧栏移除后保留完整历史快照，任务卡活动状态与计时节点稳定', async () => {
    const original=structuredClone(state.mediaJobs);
    try {
      const active=seedMediaJob({id:'stable-active',taskId:'fixture-5',state:'verifying',message:'正在校验 MP4 完整性',source:'active.ts',startedAt:Math.floor(Date.now()/1000)-10,stagePercent:45});
      const history=Array.from({length:18},(_,i)=>seedMediaJob({id:'history-'+i,taskId:'fixture-5',source:`history-${i}.ts`,state:'complete',message:'MP4 已生成并校验；原 TS 保留',startedAt:100,finishedAt:105}));
      state.mediaJobs=[active,...history];
      await go(page,h.base,'/storage');
      await assertFullWidthPage('/storage');
      const fileRow=page.locator('.file-table tbody tr').first();
      await fileRow.evaluate(el=>el.setAttribute('data-instance-check','retained'));
      active.stagePercent=49;h.emit('mediaJob',active);
      await visible(page.locator('.file-table'));
      assert.equal(await fileRow.getAttribute('data-instance-check'),'retained','进度更新不重建文件行');
      await navigate('录制任务');
      const current=card('fixture-5').locator('.card-media-job');
      await visible(current);
      assert.equal(await current.getAttribute('data-job-id'),active.id);
      await current.locator('summary').click();
      await current.evaluate(el=>el.setAttribute('data-instance-check','retained'));
      const clock=await current.locator('.media-job-detail').innerText();
      await waitState(()=>current.locator('.media-job-detail').innerText(),text=>text!==clock,'处理计时刷新');
      assert.equal(await current.getAttribute('data-instance-check'),'retained');
      assert.equal(await current.locator('details').getAttribute('open'),'');
      active.stagePercent=58;h.emit('mediaJob',active);
      await waitState(()=>current.locator('progress').getAttribute('value'),v=>v==='58','校验进度刷新');
      assert.equal(await current.getAttribute('data-instance-check'),'retained');
      assert.deepEqual((await readFixtureJobs()).filter(job=>job.id.startsWith('history-')),history,'18条完成记录仍保留在后台快照中');
      await page.reload({waitUntil:'domcontentloaded'});
      await waitState(()=>current.getAttribute('data-job-id'),id=>id===active.id,'重载后完成历史不遮住活动任务');
      assert.equal((await readFixtureJobs()).length,19);
      await h.shot('processing-active-and-history');
    } finally {state.mediaJobs=original;await go(page,h.base,'/storage');}
  });
  await step('任务卡显示失败原因，文件行重试保留 TS 且不重复提交', async () => {
    const jobs=structuredClone(state.mediaJobs), files=structuredClone(state.files);
    try {
      state.files.push({name:'retry-fixture.ts',path:'retry-fixture.ts',isDir:false,size:4096});
      const failed=seedMediaJob({taskId:'fixture-5',source:'retry-fixture.ts',output:'retry-fixture.mp4',state:'failed',message:'MP4 完整解码校验失败：reference picture missing；原 TS 保留'});
      state.mediaJobs=[failed];
      await go(page,h.base,'/recordings');
      const status=card('fixture-5').locator('.card-media-job');
      await visible(status);
      assert.equal(await status.getAttribute('data-state'),'failed');
      await status.locator('summary').click();
      await visible(status.getByText(/reference picture missing/));
      await status.getByRole('link',{name:'在媒体库查看文件',exact:true}).click();
      await assertFullWidthPage('/storage');
      const convert=page.locator('.file-table tr[data-path="retry-fixture.ts"]').getByRole('button',{name:'转为MP4：retry-fixture.ts',exact:true});
      const before=requests('POST','/api/media/remux').length;
      h.delayNext('POST','/api/media/remux',700);
      await convert.click();
      await waitState(()=>convert.isDisabled(),Boolean,'文件行提交期间禁用重复转换');
      await convert.evaluate(button=>{button.click();button.click();});
      await waitState(()=>state.mediaJobs.at(-1)?.state,v=>v==='complete','保留源文件重试完成');
      const writes=requests('POST','/api/media/remux').slice(before);
      assert.equal(writes.length,1);
      assert.equal(writes[0].body.path,'retry-fixture.ts');
      assert.notEqual(writes[0].body.deleteOriginal,true);
      assert.ok(state.files.some(file=>file.path==='retry-fixture.ts'));
      await visible(page.getByRole('button',{name:/^retry-fixture.mp4/}));
      await assertFullWidthPage('/storage');
      assert.ok((await readFixtureJobs()).some(job=>job.id===failed.id&&job.state==='failed'),'成功后后台仍保留失败记录');
      await navigate('录制任务');
      await hidden(status);
      assert.ok((await readFixtureJobs()).some(job=>job.source==='retry-fixture.ts'&&job.state==='complete'),'成功结果保留在后台，不常驻任务行');
      await h.shot('processing-safe-retry');
    } finally {state.mediaJobs=jobs;state.files=files;await go(page,h.base,'/storage');}
  });
  await step('同源手动重试成功后任务不再展示旧失败，历史记录仍可追溯', async () => {
    const jobs=structuredClone(state.mediaJobs), files=structuredClone(state.files);
    const item=record('fixture-5'), original=structuredClone(item);
    try {
      Object.assign(item,{isRecording:false,isLive:false,monitorStatus:true,checkError:null,recordingError:null,verificationRequired:false});
      const source='task-retry.ts';
      state.files.push({name:source,path:source,isDir:false,size:4096});
      const failed=seedMediaJob({taskId:item.recId,source,output:'task-retry.mp4',state:'failed',deleteOriginal:true,message:'MP4 完整解码校验失败'});
      const other=seedMediaJob({taskId:'fixture-6',source:'other/task-retry.ts',state:'failed',message:'unrelated failure'});
      state.mediaJobs=[failed,other];
      await go(page,h.base,'/recordings');
      const compact=card(item.recId).locator('.card-media-job');
      await visible(compact);
      assert.equal(await compact.getAttribute('data-job-id'),failed.id);
      assert.equal(await compact.locator('.card-media-job-source').isVisible(),false,'路径不再占据卡片主体');
      assert.equal(await compact.locator('.media-job-heading').innerText(),'校验未通过 · 未清理源文件');
      await navigate('媒体库');
      await assertFullWidthPage('/storage');
      await page.locator('.file-table tr[data-path="task-retry.ts"]').getByRole('button',{name:'转为MP4：task-retry.ts',exact:true}).click();
      await waitState(()=>state.mediaJobs.at(-1)?.state,v=>v==='complete','手动重试成功');
      const latest=state.mediaJobs.at(-1);
      assert.equal(latest.taskId,item.recId);
      assert.equal(latest.deleteOriginal,false);
      await navigate('录制任务');
      await hidden(compact);
      assert.ok((await readFixtureJobs()).some(job=>job.id===latest.id&&job.state==='complete'),'完成记录保留，但不遮蔽当前录制状态');
      assert.equal(await card('fixture-6').locator('.card-media-job').getAttribute('data-job-id'),other.id);
      assert.equal((await readFixtureJobs()).find(job=>job.id===failed.id).state,'failed','不改写旧记录的结果');
      assert.ok(state.files.some(file=>file.path===source),'手动重试不删源');
      await h.shot('task-retry-reconciled');
    } finally {state.mediaJobs=jobs;state.files=files;Object.assign(item,original);await go(page,h.base,'/storage');}
  });
  await step('历史队列满额替换时仍刷新新 MP4 且保留活动任务', async () => {
    const jobs=structuredClone(state.mediaJobs), files=structuredClone(state.files);
    try {
      const active=seedMediaJob({id:'keep-running',taskId:'fixture-5',source:'running.ts',state:'running'});
      const history=Array.from({length:255},(_,i)=>seedMediaJob({id:'rollover-'+i,source:`rollover-${i}.ts`,state:'complete'}));
      state.mediaJobs=[active,...history];
      await go(page,h.base,'/storage');
      await assertFullWidthPage('/storage');
      const before=await readFixtureJobs();
      assert.equal(before.length,256);
      assert.equal(before.find(job=>job.id===active.id).state,'running');
      const fresh=seedMediaJob({id:'new-published-output',source:'new-source.ts',output:'new-output.mp4',state:'complete'});
      state.files.push({name:'new-output.mp4',path:'new-output.mp4',isDir:false,size:2048});
      state.mediaJobs.splice(1,1);state.mediaJobs.push(fresh);h.emit('mediaJob',fresh);
      await visible(page.getByRole('button',{name:/^new-output.mp4/}));
      const after=await readFixtureJobs();
      assert.equal(after.length,before.length,'满额替换保持队列快照长度');
      assert.equal(after.find(job=>job.id===active.id).state,'running','活动任务未随旧历史移除');
      assert.ok(after.some(job=>job.id===fresh.id));
      await assertFullWidthPage('/storage');
      await navigate('录制任务');
      const status=card('fixture-5').locator('.card-media-job');
      await waitState(()=>status.getAttribute('data-job-id'),id=>id===active.id,'客户端仍保留活动任务');
      assert.equal(await status.getAttribute('data-state'),'running');
    } finally {state.mediaJobs=jobs;state.files=files;await go(page,h.base,'/storage');}
  });
  await step('异步预览期间切页，不访问已销毁信号', async () => {
    await navigate('录制任务');
    h.delayNext('GET', '/api/recordings/fixture-1/files', 700);
    await openRecordingPreview(card('fixture-1'));
    await page.keyboard.press('Escape');
    await navigate('总览');
    await visible(page.getByRole('heading', { name: '录制工作台', exact: true }));
  });
  await step('区分解析不可用与后端断线，保留任务并自动恢复', async () => {
    state.resolverReady = false;
    await navigate('录制任务');
    await waitState(() => card('fixture-2').getByRole('button', { name: '单次录制', exact: true }).isDisabled(), Boolean, '解析未就绪时禁止启动录制');
    await hidden(page.locator('.connection-banner'));
    const menu = await openCardMenu(card('fixture-2'));
    const check = menu.getByRole('button', { name: '检测直播状态', exact: true });
    assert.equal(await check.isDisabled(), true);
    const hint = check.locator('..');
    await hint.focus();
    assert.equal(await hint.getAttribute('data-tooltip'), '直播解析服务尚未就绪');
    assert.equal(await hint.evaluate(node => getComputedStyle(node, '::after').visibility), 'visible');
    await closeCardMenu(menu);
    await navigate('总览');
    await assertFullWidthPage('/home');
    const count = state.recordings.length;
    h.setOffline(true);
    await visible(page.locator('.connection-banner'));
    assert.equal(await page.locator('.overview-task-row').count(), Math.min(count, 6));
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
          await hidden(page.locator('.overview-loading'));
          if (newestFirst) await finishOld();
          await waitState(() => count, n => n >= 3, '最新快照失败后自动重试');
          assert.ok(requests('GET', '/api/status').length > statusCount, '重试前已读取健康状态');
          await visible(page.locator('.connection-banner'));
          assert.equal(await page.locator('.overview-loading').count(), 0);
          assert.equal(await page.locator('.overview-task-row').count(), 0, '旧快照不得冒充初始化成功');
          await h.shot(`startup-error-${oldStatus}-${newestFirst ? 'latest-first' : 'old-first'}`);
          const jobsRestored = page.waitForResponse(response => new URL(response.url()).pathname === '/api/media/jobs' && response.status() === 200);
          recoveryGate.resolve();
          await jobsRestored;
          await waitState(() => page.locator('.overview-task-row').count(), n => n === Math.min(state.recordings.length, 6), '快照真实加载成功后恢复列表');
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
    const before = await page.locator('.overview-task-row').count();
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
      assert.equal(await page.locator('.overview-task-row').count(), before);
      assert.equal(await page.locator('.overview-loading').count(), 0);
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
    await navigate('录制任务');await openRecordingPreview(card('fixture-1'));const modal=dialog('录制预览');await visible(modal);
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
    await visible(page.locator('.card-recording-status').first());
    assert.equal(await page.locator('.card-recording-status').first().innerText(),'Recording');
    const languageJobs=structuredClone(state.mediaJobs);
    try {
      const job={id:'english-history',taskId:'fixture-2',source:'english-history.ts',output:'english-history.mp4',
        state:'complete',message:'MP4 已生成并校验；原 TS 保留',sourceRemoved:false,deleteOriginal:false,
        elapsedSeconds:0,startedAt:null,finishedAt:null};
      state.mediaJobs.push(job);h.emit('mediaJob',job);
      await hidden(card('fixture-2').locator('.card-media-job'));
      assert.equal(await card('fixture-2').getByText('Elapsed time unknown',{exact:true}).count(),0);
      job.state='running';h.emit('mediaJob',job);
      await visible(card('fixture-2').locator('.card-media-job[data-state=running]'));
      assert.equal(await card('fixture-2').locator('.card-media-job-source').textContent(),'english-history.ts');
      await h.shot('status-processing-english');
    } finally {state.mediaJobs=languageJobs;await go(page,h.base,'/recordings');}
    await page.keyboard.press('Control+5');await page.getByRole('button',{name:'Check for updates',exact:true}).click();
    const release = page.locator('a.about-release-link');
    await visible(release);
    assert.equal(await release.locator('strong').innerText(),apiContracts.updateCheck.release.tag_name);
    await visible(release.getByText('View release notes',{exact:true}));
    assert.equal(await release.getAttribute('href'),apiContracts.updateCheck.release.html_url);assert.equal(requests('GET','/api/tools/update').length,1);
    await page.keyboard.press('Control+,');await page.getByRole('button',{name:'Appearance and window',exact:true}).click();await page.getByRole('combobox',{name:'Interface language',exact:true}).selectOption('zh_CN');await waitState(()=>page.locator('html').getAttribute('lang'),value=>value==='zh-CN','恢复中文');
    assert.equal(state.requests.filter(request=>/\/(check|start)$/.test(request.path)).length,checks,'切页和重载不触发额外平台检查');
  });
  await step('工作台主体全宽，保留实时录制信息与失败原因且不再显示运行侧栏',async()=>{
    await navigate('总览');
    await assertFullWidthPage('/home');
    const live=page.locator('.overview-task-row[data-rec-id="fixture-1"]');
    await visible(live.getByRole('button',{name:'小窗预览',exact:true}));
    assert.match(await live.locator('.overview-task-detail strong').innerText(),/^\d{2}:\d{2}:\d{2}$/);
    const selected=record('fixture-2');const original=structuredClone(selected);
    Object.assign(selected,{isRecording:false,recordingError:'工作台异常夹具'});h.emit('update',selected);
    await waitState(()=>page.locator('.overview-task-row').first().getAttribute('data-rec-id'),id=>id==='fixture-2','工作台失败任务置顶');
    await visible(page.locator('.overview-task-row[data-rec-id="fixture-2"] .overview-task-detail').getByText('工作台异常夹具',{exact:true}));
    await h.shot('workbench-attention');
    await navigate('录制任务');await page.getByRole('button',{name:/^需关注/}).click();
    await visible(card('fixture-2'));assert.equal(await card('fixture-1').count(),0);
    await page.getByRole('button',{name:/^全部/}).click();
    Object.assign(selected,original);h.emit('update',selected);
    await navigate('总览');
    await hidden(page.locator('.overview-task-row').getByText('工作台异常夹具',{exact:true}));
    await assertFullWidthPage('/home');
    await navigate('录制任务');
  });
  await step('队列位置、等待原因、恢复倒计时和中英文展示不触发平台请求', async () => {
    await navigate('录制任务'); await page.getByRole('button',{name:/^全部/}).click();
    const selected = record('fixture-2'), recovering = record('fixture-6');
    const original = structuredClone(selected), other = structuredClone(recovering);
    const checkRequests = () => state.requests.filter(r=>['check','start'].some(action=>r.path.endsWith('/'+action))).length;
    const count = checkRequests();
    const now = Math.floor(Date.now()/1000);
    Object.assign(selected,{isRecording:false,monitorStatus:true,checkError:null,recordingError:null,verificationRequired:false,
      checkState:'queued',checkWaitReason:'platformSpacing',queuePosition:2,checkReadyAt:now+30,recoveryAt:null,recoveryAttempt:0,nextCheckAt:now});
    Object.assign(recovering,{isRecording:false,monitorStatus:true,checkError:null,recordingError:'FLV 拉流连接中断',verificationRequired:false,
      checkState:'recovering',checkWaitReason:'recovery',queuePosition:null,checkReadyAt:now+45,recoveryAt:now+45,recoveryAttempt:2,nextCheckAt:now+4500});
    h.emit('update',selected); h.emit('update',recovering);
    await visible(card('fixture-2').getByText('排队第 2 位 · 同平台请求间隔',{exact:true}));
    await visible(card('fixture-6').getByText('恢复第 2/3 次 · 断流后退避等待',{exact:true}));
    const initial = await card('fixture-6').locator('.card-queue-time').innerText();
    await page.evaluate(()=>window.__queueCard=document.querySelector('[data-rec-id="fixture-6"]'));
    await waitState(()=>card('fixture-6').locator('.card-queue-time').innerText(), value=>value!==initial,'共享时钟更新恢复倒计时');
    assert.equal(await page.evaluate(()=>window.__queueCard===document.querySelector('[data-rec-id="fixture-6"]')),true);
    await h.shot('queue-and-recovery');
    await card('fixture-2').getByRole('button',{name:selected.streamerName,exact:true}).click();
    await visible(dialog('直播间信息').getByText('排队第 2 位 · 同平台请求间隔',{exact:true}));
    await page.keyboard.press('Escape');
    Object.assign(selected,{checkWaitReason:'cooldown',checkReadyAt:now+600});h.emit('update',selected);
    await visible(card('fixture-2').locator('.card-state .ui-badge').getByText('平台冷却中',{exact:true}));
    await navigate('总览');
    await assertFullWidthPage('/home');
    await visible(page.locator('.overview-task-row[data-rec-id="fixture-6"] .overview-task-detail strong').getByText('恢复第 2/3 次 · 断流后退避等待',{exact:true}));
    await navigate('偏好设置');await page.getByRole('button',{name:'网络与检测',exact:true}).click();
    const interval=page.getByRole('textbox',{name:/检测间隔/});await interval.fill('4500');
    await visible(page.getByText('4500 秒（75 分钟）',{exact:true}));
    await page.getByRole('button',{name:'外观与窗口',exact:true}).click();
    await page.getByRole('combobox',{name:'界面语言',exact:true}).selectOption('en');
    await waitState(()=>page.locator('html').getAttribute('lang'),value=>value==='en','队列英文界面');
    await page.keyboard.press('Control+2');
    await visible(card('fixture-6').getByText('Recovery attempt 2/3 · Backoff after an interrupted recording',{exact:true}));
    await page.keyboard.press('Control+,');await page.getByRole('button',{name:'Appearance and window',exact:true}).click();
    await page.getByRole('combobox',{name:'Interface language',exact:true}).selectOption('zh_CN');
    await waitState(()=>page.locator('html').getAttribute('lang'),value=>value==='zh-CN','恢复中文');
    Object.assign(selected,original);Object.assign(recovering,other);h.emit('update',selected);h.emit('update',recovering);
    assert.equal(checkRequests(),count);
    await navigate('录制任务');
  });
  await step('媒体库千文件索引、搜索和刷新保留节点且不触发超一秒阻塞', async () => {
    const original = state.files;
    try {
      for (const count of [100, 500, 1000]) {
        state.files = Array.from({ length: count }, (_, index) => {
          const name = `Library-${String(index).padStart(5, '0')}.mp4`;
          return { name, path: name, isDir: false, size: 1234567, modified: 1789171200 };
        });
        const started = Date.now();
        await go(page, h.base, '/storage');
        await waitState(() => page.locator('.file-table tbody tr').count(), n => n === count, '全部文件可访问');
        const mountMs = Date.now() - started;
        const search = page.getByRole('textbox', { name: '搜索文件', exact: true });
        await page.evaluate(() => {
          window.__libraryLongTasks = [];
          window.__libraryObserver = new PerformanceObserver(list => window.__libraryLongTasks.push(...list.getEntries().map(entry => entry.duration)));
          window.__libraryObserver.observe({ type: 'longtask', buffered: false });
        });
        const searchStarted = Date.now();
        await search.fill('Library-00000');
        await waitState(() => page.locator('.file-table tbody tr').count(), n => n === 1, '索引后搜索');
        const searchMs = Date.now() - searchStarted;
        const clearStarted = Date.now();
        await search.fill('');
        await waitState(() => page.locator('.file-table tbody tr').count(), n => n === count, '清空后恢复');
        const clearMs = Date.now() - clearStarted;
        await page.evaluate(() => { window.__libraryFirstRow = document.querySelector('.file-table tbody tr'); });
        state.files[0].size = 7340032;
        await page.getByRole('button', { name: '刷新文件', exact: true }).click();
        await waitState(() => page.locator('tr[data-path="Library-00000.mp4"] .file-size').textContent(), text => text.includes('7.0'), '索引行数据更新');
        const measurement = await page.evaluate(() => {
          window.__libraryObserver.disconnect();
          return { stable: window.__libraryFirstRow === document.querySelector('.file-table tbody tr'),
            longTaskMaxMs: Math.max(0, ...window.__libraryLongTasks), nodes: document.querySelectorAll('*').length };
        });
        assert.equal(measurement.stable, true, '刷新不能重建已有文件行');
        assert.ok(measurement.longTaskMaxMs < 1000, `媒体库主线程长任务 ${measurement.longTaskMaxMs}ms`);
        assert.ok(searchMs < 1000 && clearMs < 1000, `媒体库搜索/恢复 ${searchMs}/${clearMs}ms`);
        storageLoadResults.push({ count, mountMs, searchMs, clearMs, ...measurement });
      }
    } finally { state.files = original; await go(page, h.base, '/recordings'); }
  });
  await step('100、250、500 个任务中的五路秒级进度不重建卡片且在一秒内完成', async () => {
    const original = state.recordings;
    const template = seedRecordings()[0];
    try {
      for (const count of [100, 250, 500]) {
        state.recordings = Array.from({ length: count }, (_, index) => ({ ...structuredClone(template),
          recId: 'load-' + index, streamerName: 'Load ' + String(index).padStart(4, '0'),
          url: 'https://fixture.example.invalid/' + index, isRecording: index < 5, isLive: index < 5,
          speed: index < 5 ? '0 MB/s' : null, recordedSeconds: 0,
        }));
        await go(page, h.base, '/recordings');
        await waitState(() => page.locator('article[data-rec-id]').count(), value => value === count, '完整负载列表渲染');
        const samples = [];
        for (let round = 0; round < 6; round++) {
          const speed = `${round + 1}.7 MB/s`;
          await page.evaluate(() => { window.__progressCard = document.querySelector('[data-rec-id="load-0"]'); window.__progressStart = performance.now(); });
          for (const record of state.recordings.slice(0, 5)) {
            record.speed = speed; record.recordedSeconds = (round + 1) * 60; h.emit('update', record);
          }
          const ready = await page.waitForFunction(expected => [0, 1, 2, 3, 4].every(index => document.querySelector(`[data-rec-id="load-${index}"] .card-speed`)?.textContent.includes(expected)), speed);
          await ready.dispose();
          const elapsed = await page.evaluate(() => performance.now() - window.__progressStart);
          assert.equal(await page.evaluate(() => window.__progressCard === document.querySelector('[data-rec-id="load-0"]')), true);
          if (round > 0) samples.push(Math.round(elapsed * 10) / 10);
        }
        loadResults.push({ recordings: count, active: 5, milliseconds: samples });
        assert.ok(Math.max(...samples) < 1000, `${count} tasks exceeded the one-second progress budget: ${samples}`);
      }
    } finally { state.recordings = original; await go(page, h.base, '/recordings'); }
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
  await h.report({ passed: !failure, steps, loadResults, storageLoadResults, failure: failure ? String(failure.stack ?? failure) : null });
  await h.close();
}
console.log(JSON.stringify({ passed: !failure, checks: steps.length, artifacts: h.runDir }));
if (failure) process.exitCode = 1;
