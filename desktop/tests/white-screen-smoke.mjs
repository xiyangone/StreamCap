import assert from 'node:assert/strict';
import { createHarness, eventually, go, inspectLayout, seedMediaJob, visible, waitTheme } from './ui-fixtures.mjs';

const h = await createHarness('visual');
const { page, state } = h;
state.mediaJobs = [
  seedMediaJob({taskId:'fixture-2',source:'history/completed-recording.ts',state:'complete',stagePercent:100,elapsedSeconds:136,message:'MP4 已生成并校验；原 TS 保留'}),
  seedMediaJob({taskId:'fixture-3',source:'history/failed-recording.ts',state:'failed',elapsedSeconds:43,message:'MP4 完整解码校验失败：diagnostic fixture；原 TS 保留'}),
  seedMediaJob({taskId:'fixture-6',source:'history/current-recording.ts',state:'verifying',stagePercent:42,startedAt:Math.floor(Date.now()/1000)-35,message:'正在校验 MP4 完整性'}),
];
const cases = [];
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
        assert.equal(layout.surface.background, theme === 'dark' ? 'rgb(26, 30, 38)' : 'rgb(255, 255, 255)', '新版面板底色必须实际加载');
        assert.equal(layout.surface.border, 'solid', '面板边界必须存在');
        assert.ok(layout.surface.borderWidth >= 1 && layout.surface.radius >= 6 && layout.surface.radius <= 12, '面板尺寸样式必须实际加载');
        assert.equal(layout.surface.backdrop, 'none', '实色面板不叠加无效模糊');
        assert.equal(layout.baseFontSize, 14, '基础字号必须实际加载');
        assert.equal(await page.locator('.glass').count(),0,'旧玻璃组件路径已移除');
        assert.equal(await page.locator('.page-header').count(),1,'页面共用唯一标题结构');
        if (path === '/recordings') {
          for (const [name, mode] of [['列表视图','list'],['网格视图','grid']]) {
            await page.getByRole('button',{name,exact:true}).click();
            const arrangement=await inspectLayout(page);
            assert.equal(arrangement.contentOverflow,false,`${mode} 不得横向溢出`);
            assert.deepEqual(arrangement.overflow,[],`${mode} 组件必须适配窗口`);
            if(mode==='list' && width>=950) {
              const heights=await page.locator('.recording-card').evaluateAll(nodes=>nodes.map(node=>node.getBoundingClientRect().height));
              assert.ok(Math.max(...heights)<=150,'桌面任务行包含录后摘要时也应保持紧凑: '+JSON.stringify(heights));
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
} catch (error) {
  failure = error;
  await h.shot('failure').catch(() => {});
  console.error(error.stack ?? error);
} finally {
  await h.report({ passed: !failure, cases, failure: failure ? String(failure.stack ?? failure) : null });
  await h.close();
}
console.log(JSON.stringify({ passed: !failure, cases: cases.length, artifacts: h.runDir }));
if (failure) process.exitCode = 1;
