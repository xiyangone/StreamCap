import assert from 'node:assert/strict';
import { createHarness, eventually, go, inspectLayout, visible, waitTheme } from './ui-fixtures.mjs';

const h = await createHarness('visual');
const { page, state } = h;
const cases = [];
let failure;
try {
  for (const width of [1280, 800, 375]) {
    await page.setViewportSize({ width, height: width === 375 ? 812 : 900 });
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
        assert.ok(layout.glass.includes('blur'), '玻璃态样式必须实际加载');
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
  await visible(page.getByText('你的直播间，从这里开始', { exact: true }));
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
