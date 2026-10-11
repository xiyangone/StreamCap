import assert from 'node:assert/strict';
import { assertDialogStyles, assertSingleLineActions, cardMenuAction, createHarness, eventually, go, hidden, inspectLayout, seedMediaJob, themeColors, visible, waitTheme } from './ui-fixtures.mjs';

const h = await createHarness('visual');
const { page, state } = h;
state.mediaJobs = [
  seedMediaJob({taskId:'fixture-2',source:'history/completed-recording.ts',state:'complete',stagePercent:100,elapsedSeconds:136,message:'MP4 已生成并校验；原 TS 保留'}),
  seedMediaJob({taskId:'fixture-3',source:'history/failed-recording.ts',state:'failed',elapsedSeconds:43,message:'MP4 完整解码校验失败：diagnostic fixture；原 TS 保留'}),
  seedMediaJob({taskId:'fixture-6',source:'history/current-recording.ts',state:'verifying',stagePercent:42,startedAt:Math.floor(Date.now()/1000)-35,message:'正在校验 MP4 完整性'}),
];
const cases = [];
const dialogCases = [];
const englishCases = [];
const accessibilityCases = [];
let failure;
try {
  for (const width of [1280, 950, 800, 375]) {
    await page.setViewportSize({ width, height: width === 375 ? 812 : width === 950 ? 646 : 900 });
    for (const theme of ['light', 'dark']) {
      state.userConfig.theme_mode = theme;
      for (const path of ['/home', '/recordings', '/storage', '/settings', '/about']) {
        await go(page, h.base, path);
        await waitTheme(page, theme);
        await eventually(() => page.locator('.content-area .loading-state:visible').count(), (n) => n === 0, '页面完成加载');
        const layout = await inspectLayout(page);
        assert.ok(layout.heading?.width > 0 && layout.heading.top >= 0 && layout.heading.top < 800, '主标题必须在可视区域');
        assert.ok(layout.sidebar?.height > 0, '导航可见');
        assert.equal(await page.getByRole('navigation', { name: '主导航' }).getByRole('link', { name: '录制任务', exact: true }).count(), 1, '图标导航仍须有可访问名称');
        assert.equal(layout.bodyOverflow, false, '页面不能横向溢出');
        assert.equal(layout.contentOverflow, false, '主内容不能横向溢出');
        assert.deepEqual(layout.overflow, [], '组件不能溢出窗口');
        assert.equal(layout.dialogs, 0, '闭合弹窗不能覆盖页面');
        assert.equal(await page.evaluate(async()=> (await import('/media-player.js')).activePlayerCount()),0,'未打开预览时不得持有播放器');
        if(path==='/settings') assert.equal(await page.getByRole('switch',{name:'录制结束转 MP4'}).count(),1,'转换开关必须真实可见');
        assert.equal(await page.evaluate(()=>typeof window.__TAURI__),'undefined','不公开整套 Tauri 全局 API');
        const shell=await page.locator('.app-shell').boundingBox();assert.equal(shell.x,0);assert.equal(shell.y,0);
        assert.equal(await page.locator('.device-avatar').count(),0);
        assert.equal(await page.getByRole('group',{name:'窗口控制'}).getByRole('button').count(),3);
        assert.ok(layout.surface?.width > 0 && layout.surface.height > 0, '当前页面必须有实际可测的主内容承载区');
        assert.equal(layout.surface.background, themeColors[theme].surface, '主内容使用同一套高可读玻璃面板');
        assert.ok(layout.surface.left >= 0 && layout.surface.left + layout.surface.width <= width + 2, '主内容承载区不能越过窗口');
        assert.match(layout.surface.backdrop, /blur\(18px\)/, '玻璃模糊仅分配到主承载层');
        assert.equal(layout.baseFontSize, 16, '根字号必须实际加载');
        assert.equal(layout.bodyFontSize, 14, '正文基础字号必须实际加载');
        assert.equal(layout.headingFontSize, width <= 600 ? 23 : 25, '页头字号必须符合当前窗口的响应式排版');
        assert.equal(await page.locator('.glass').count(),0,'使用现有统一组件，不恢复另一套旧玻璃组件');
        assert.equal(await page.locator('.page-header').count(),1,'页面共用唯一标题结构');
        if (path === '/home' || path === '/storage') {
          const container = page.locator(path === '/home' ? '.home-page' : '.storage-page');
          const removed = path === '/home'
            ? '.overview-aside, .overview-health, .overview-queue-section, .overview-policy'
            : '.file-jobs-sidebar, .media-jobs, .media-history, .media-history-attention';
          assert.equal(await container.locator(removed).count(), 0, '右侧栏及替代折叠/计数区域完全不存在');
          const pageBounds = await container.boundingBox();
          assert.ok(pageBounds && Math.abs(layout.surface.left - pageBounds.x) <= 1 && Math.abs(layout.surface.width - pageBounds.width) <= 1, '主体铺满页面，不保留侧栏空列');
          layout.fullWidth = true;
        }
        if (path === '/home') layout.shortActions = await assertSingleLineActions(page.locator('.overview-task-action button:visible'));
        if (path === '/storage') {
          layout.shortActions = await assertSingleLineActions(page.locator('.file-row-actions button:visible'));
          const table = page.locator('.file-table');
          await visible(table);
          const viewport = await page.locator('.table-container').boundingBox(), tableBounds = await table.boundingBox();
          assert.ok(viewport && Math.abs(viewport.width - (layout.surface.width - 2)) <= 1, '文件列表除面板边框外全宽');
          const listClientWidth=await page.locator('.table-container').evaluate(node=>node.clientWidth);
          if (layout.surface.width >= 620) assert.ok(tableBounds && Math.abs(tableBounds.width - listClientWidth) <= 1, '宽窗口文件表除系统滚动槽外占满主体');
          assert.equal(await page.getByRole('button',{name:'转为MP4：海边日落.ts',exact:true}).count(),1,'文件行手动转换保留');
          assert.equal(await page.locator('.file-action-hint').count(),0,'不保留撑高文件行的同名输出提示');
          const rows=await page.locator('.file-table tbody tr').evaluateAll(nodes=>nodes.map(row=>({height:row.getBoundingClientRect().height,blur:getComputedStyle(row).backdropFilter})));
          assert.ok(rows.every(row=>row.height<=60&&row.blur==='none'),'文件列表紧凑且每行不分配模糊层');
          assert.equal(await table.locator('thead th').first().evaluate(node=>getComputedStyle(node).position),'sticky');
        }
        if (path === '/recordings') {
          const actions = await page.locator('.card-record').evaluateAll(nodes => nodes.map(node => ({
            height: node.getBoundingClientRect().height,
            fontSize: parseFloat(getComputedStyle(node).fontSize),
            background: getComputedStyle(node).backgroundColor,
          })));
          assert.ok(actions.length > 0 && actions.every(action => action.height >= 40 && action.fontSize >= 13), '录制主操作必须可读且有足够点击面积');
          assert.ok(actions.every(action => action.background !== layout.surface.background), '录制主操作不能被旧的小白按钮样式覆盖');
          assert.equal(await page.getByRole('switch', {name:'自动检测',exact:true}).count(), state.recordings.length, '每张卡片独立显示自动检测开关');
          for (const [name, mode] of [['列表视图','list'],['网格视图','grid']]) {
            await page.getByRole('button',{name,exact:true}).click();
            const arrangement=await inspectLayout(page);
            assert.equal(arrangement.contentOverflow,false,`${mode} 不得横向溢出`);
            assert.deepEqual(arrangement.overflow,[],`${mode} 组件必须适配窗口`);
            assert.equal(await page.getByRole('button',{name:'预览录制文件',exact:true}).count(),state.recordings.length,'所有任务常驻预览入口');
            assert.equal(await page.getByRole('button',{name:'小窗预览',exact:true}).count(),state.recordings.length,'所有任务常驻小窗入口');
            assert.equal(await page.locator('.card-menu-panel').getByRole('button',{name:'预览录制文件',exact:true}).count(),0,'预览不在二级菜单');
            await assertSingleLineActions(page.locator('.card-preview-actions button:visible'));
            if(mode==='list' && width>=950) {
              const heights=await page.locator('.recording-card').evaluateAll(nodes=>nodes.map(node=>node.getBoundingClientRect().height));
              const limit = width >= 1200 ? 200 : 240;
              assert.ok(Math.max(...heights)<=limit,'分区操作与录后摘要仍应保持有限行高: '+JSON.stringify(heights));
            }
            if(mode==='list' && width>=1200) {
              const idle=page.locator('.recording-card[data-rec-id="fixture-2"]');
              assert.equal(await idle.locator('.card-media-job').count(),0,'完成信息不再占据普通行');
              const density=await idle.evaluate(node=>{
                const row=node.getBoundingClientRect();
                const controls=[...node.querySelectorAll('.card-monitor-setting,.card-preview-actions,.card-actions')].map(element=>{const r=element.getBoundingClientRect();return r.y+r.height/2;});
                return {height:row.height,spread:Math.max(...controls)-Math.min(...controls)};
              });
              assert.ok(density.height<=100&&density.spread<=2,'宽窗口普通任务行紧凑，检测/预览/录制横向对齐: '+JSON.stringify(density));
            }
            assert.equal(await page.locator('.card-media-job .card-media-job-source:visible').count(),0,'完整历史路径默认收进详情');
            await h.shot(`${theme}-${width}-recordings-${mode}`);
          }
        }
        if (width === 1280 || path === '/recordings') await h.shot(theme + '-' + width + path.replaceAll('/', '-'));
        cases.push({ width, theme, path, layout });
        console.log('PASS ' + width + ' ' + theme + ' ' + path);
      }
    }
  }
  assert.equal(cases.length, 40, '完整保留五页面、四窗口、双主题矩阵');
  const recordingsBeforeDialogs = structuredClone(state.recordings);
  const filesBeforeDialogs = structuredClone(state.files);
  const cookiesBeforeDialogs = structuredClone(state.cookies);
  const mutationCount = () => state.requests.filter(request =>
    /^(POST|PUT|PATCH|DELETE)$/.test(request.method)
    && /^\/api\/(?:recordings|media|cookies|accounts)(?:\/|$)/.test(request.path)).length;
  const mutationsBeforeDialogs = mutationCount();
  const inactiveCard = () => page.locator('article.recording-card[data-rec-id="fixture-3"]');
  const scenarios = [
    { name: '添加直播间', slug: 'add', route: '/recordings', controls: ['textarea', 'input'],
      open: () => page.getByRole('button', { name: '添加直播间', exact: true }).click() },
    { name: '编辑直播间', slug: 'edit', route: '/recordings', controls: ['input', 'select'],
      open: () => cardMenuAction(inactiveCard(), '编辑任务') },
    { name: '批量编辑任务', slug: 'batch', route: '/recordings', controls: ['input', 'select'],
      open: async () => {
        await inactiveCard().getByRole('checkbox').check();
        await page.locator('article.recording-card[data-rec-id="fixture-5"]').getByRole('checkbox').check();
        await page.getByRole('button', { name: '批量编辑', exact: true }).click();
      } },
    { name: '直播间信息', slug: 'info', route: '/recordings', controls: ['input'],
      open: () => inactiveCard().getByRole('button', { name: state.recordings.find(record => record.recId === 'fixture-3').streamerName, exact: true }).click() },
    { name: '快手扫码登录', slug: 'qr', route: '/settings', controls: [],
      open: async () => {
        state.qrPhase = 'waiting';
        await page.getByRole('button', { name: '平台登录', exact: true }).click();
        await page.getByRole('combobox', { name: 'Cookie 平台列表', exact: true }).selectOption('kuaishou');
        await page.getByRole('button', { name: '扫码登录', exact: true }).click();
        await visible(page.getByRole('dialog', { name: '快手扫码登录', exact: true }).getByText('二维码有效期：120 秒', { exact: true }));
      } },
    { name: '关闭 StreamCap', slug: 'close', route: '/home', controls: [],
      open: () => page.getByRole('button', { name: '关闭窗口', exact: true }).click() },
  ];
  for (const width of [1280, 600]) {
    await page.setViewportSize({ width, height: 900 });
    for (const theme of ['light', 'dark']) {
      state.userConfig.theme_mode = theme;
      for (const scenario of scenarios) {
        await go(page, h.base, scenario.route);
        await waitTheme(page, theme);
        await eventually(() => page.locator('.content-area .loading-state:visible').count(), count => count === 0, '弹窗所在页面完成加载');
        await scenario.open();
        const modal = page.getByRole('dialog', { name: scenario.name, exact: true });
        const styles = await assertDialogStyles(modal, theme, scenario.controls);
        await h.shot(`dialog-${theme}-${width}-${scenario.slug}`);
        await page.keyboard.press('Escape');
        await hidden(modal);
        assert.equal(state.native.closing, false, '视觉检查不得退出模拟应用');
        dialogCases.push({ width, theme, name: scenario.name, styles });
      }
    }
  }
  assert.equal(dialogCases.length, 24, '六弹窗必须覆盖明暗主题及桌面、600px 窗口');
  assert.deepEqual(state.recordings, recordingsBeforeDialogs, '弹窗视觉检查不更改任务');
  assert.deepEqual(state.files, filesBeforeDialogs, '弹窗视觉检查保留全部媒体源文件');
  assert.deepEqual(state.cookies, cookiesBeforeDialogs, '模拟二维码截图不保存或更改登录信息');
  assert.equal(mutationCount(), mutationsBeforeDialogs, '只打开、关闭弹窗，不提交业务写入');
  state.userConfig.language='en';
  await page.evaluate(()=>localStorage.setItem('streamcap.language','en'));
  for (const width of [1280,1100,800,600]) {
    await page.setViewportSize({width,height:800});
    for (const theme of ['light','dark']) {
      state.userConfig.theme_mode=theme;
      state.userConfig.is_grid_view=false;
      for (const path of ['/recordings','/storage','/settings']) {
        await go(page,h.base,path);await waitTheme(page,theme);
        await eventually(()=>page.locator('.content-area .loading-state:visible').count(),n=>n===0,'English route loaded');
        if(path==='/settings') {
          await page.getByRole('button',{name:'Appearance and window',exact:true}).click();
          assert.equal(await page.locator('.theme-option').count(),2);
          assert.equal(await page.locator('.accent-options').count(),0);
        }
        const layout=await inspectLayout(page);
        assert.equal(layout.contentOverflow,false);assert.deepEqual(layout.overflow,[]);
        if(path==='/recordings') {
          await assertSingleLineActions(page.locator('.card-controls button:visible'));
          await page.getByRole('button',{name:'Grid view',exact:true}).click();
          const grid=await inspectLayout(page);
          assert.equal(grid.contentOverflow,false);assert.deepEqual(grid.overflow,[]);
          await assertSingleLineActions(page.locator('.card-controls button:visible'));
          await page.getByRole('button',{name:'List view',exact:true}).click();
        }
        englishCases.push({width,theme,path,layout});
        if(width===1100||width===600)await h.shot(`english-${theme}-${width}${path.replaceAll('/','-')}`);
      }
    }
  }
  assert.equal(englishCases.length,24);
  state.userConfig.language='zh_CN';
  await page.evaluate(()=>localStorage.setItem('streamcap.language','zh_CN'));
  await page.setViewportSize({width:1280,height:900});
  for(const theme of ['light','dark']) {
    state.userConfig.theme_mode=theme;
    await go(page,h.base,'/storage');await waitTheme(page,theme);
    await page.emulateMedia({contrast:'more',reducedMotion:'reduce'});
    const material=await page.locator('.file-manager').evaluate(node=>{
      const style=getComputedStyle(node),color=style.backgroundColor;
      return {color,backdrop:style.backdropFilter,sheen:style.backgroundImage,
        motion:getComputedStyle(node.querySelector('button')).transitionDuration};
    });
    assert.equal(material.color,theme==='light'?'rgb(255, 255, 255)':'rgb(27, 31, 39)');
    assert.match(material.backdrop,/blur\(0px\)/);
    assert.ok(material.sheen.split(',').every(layer=>layer.trim()==='none'),'降低透明度时关闭所有高光图层');
    assert.equal(material.motion,'0s');
    accessibilityCases.push({theme,material});
    await h.shot(`accessible-${theme}`);
    await page.emulateMedia({contrast:'no-preference',reducedMotion:'no-preference'});
  }
  await page.setViewportSize({ width: 1280, height: 900 });
  state.recordings = [];
  state.userConfig.theme_mode = 'light';
  await go(page, h.base, '/home');
  await visible(page.getByText('还没有直播间', { exact: true }));
  await h.shot('empty-state');
  h.setOffline(true);
  await go(page, h.base, '/home');
  await visible(page.locator('.connection-banner'));
  await h.shot('offline-state');
  assert.deepEqual(h.blocked, []); assert.deepEqual(state.unexpected, []); assert.deepEqual(h.runtimeErrors, []);
  assert.deepEqual(h.consoleErrors.filter(message => !message.startsWith('Failed to load resource:')), [], '应用 console.error');
} catch (error) {
  failure = error;
  await h.shot('failure').catch(() => {});
  console.error(error.stack ?? error);
} finally {
  await h.report({ passed: !failure, cases, dialogCases, englishCases, accessibilityCases, failure: failure ? String(failure.stack ?? failure) : null });
  await h.close();
}
console.log(JSON.stringify({ passed: !failure, cases: cases.length, dialogs: dialogCases.length, english: englishCases.length, accessibility: accessibilityCases.length, artifacts: h.runDir }));
if (failure) process.exitCode = 1;
