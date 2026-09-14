// Local-only player bridge. mpegts.js is bundled with the app, not downloaded at runtime.
let activePlayers = 0;
export function activePlayerCount() { return activePlayers; }
export function attachMedia(video, apiOrigin, relativePath, onState) {
  let disposed = false, player = null;
  const abort = new AbortController();
  activePlayers++;
  const update = (phase, message = '') => { if (!disposed) onState(phase, message); };
  const onError = () => update('error', '无法解码此文件，请检查音视频编码；原始文件未改动。');
  const onPlaying = () => update('playing');
  video.addEventListener('error', onError);
  video.addEventListener('playing', onPlaying);
  video.muted = true; video.crossOrigin = "anonymous";
  const dispose = () => {
    if (disposed) return;
    disposed = true; abort.abort();
    video.removeEventListener('error', onError); video.removeEventListener('playing', onPlaying);
    if (player) { player.pause(); player.unload(); player.detachMediaElement(); player.destroy(); player = null; }
    video.pause(); video.removeAttribute('src'); video.load(); activePlayers--;
  };
  (async () => {
    try {
      update('loading', '正在读取媒体信息…');
      const liveSource = relativePath.startsWith('live:');
      let info;
      if (liveSource) { info = {format: 'live', isRecording: true}; }
      else {
        const response = await fetch(apiOrigin + '/api/media/info?path=' + encodeURIComponent(relativePath), { signal: abort.signal });
        if (!response.ok) throw new Error('无法读取媒体文件信息');
        info = await response.json();
      }
      if (disposed) return;
      if (liveSource || info.format === 'ts' || ['flv','mkv','mov','nut','wma'].includes(info.format)) {
        const mpegts = window.mpegts;
        if (!mpegts || !mpegts.isSupported()) throw new Error('当前系统 WebView2 不支持 TS 所需的媒体播放能力');
        mpegts.LoggingControl.enableAll = false;
        const compatibility = info.format !== 'ts';
        const url = liveSource ? apiOrigin + '/api/recordings/' + encodeURIComponent(relativePath.slice(5)) + '/preview' : apiOrigin + (compatibility ? '/api/media/transcode' : info.isRecording ? '/api/media/preview' : '/api/videos') + '?path=' + encodeURIComponent(relativePath);
        player = mpegts.createPlayer({ type: 'mpegts', isLive: info.isRecording || compatibility, url }, {
          enableWorker: false, enableWorkerForMSE: false,
          enableStashBuffer: !info.isRecording && !compatibility, stashInitialSize: 128 * 1024,
          lazyLoad: !info.isRecording && !compatibility, lazyLoadMaxDuration: 30, lazyLoadRecoverDuration: 10,
          autoCleanupSourceBuffer: true, autoCleanupMaxBackwardDuration: 30, autoCleanupMinBackwardDuration: 15,
        });
        player.on(mpegts.Events.ERROR, (type, detail) => {
          if (!disposed) update('error', type === 'NetworkError' ? '本地预览连接中断，请重新打开预览。' : '此 TS 的音视频编码暂不受当前播放器支持；原文件未改动。');
        });
        player.attachMediaElement(video); player.load();
        await player.play().catch(() => update('ready', '点击播放按钮开始预览'));
      } else {
        if (info.isRecording && ['mp4', 'm4v'].includes(info.format)) throw new Error('该 MP4 尚在录制，停止并完成封装后可预览；TS 支持录中预览。');
        video.src = apiOrigin + '/api/videos?path=' + encodeURIComponent(relativePath);
        video.load(); await video.play().catch(() => update('ready', '点击播放按钮开始预览'));
      }
    } catch (error) { if (!disposed && error.name !== 'AbortError') update('error', error.message || '无法加载本地媒体'); }
  })();
  return dispose;
}

export function captureFrame(path) {
 const frame=[...document.querySelectorAll('.media-preview')].find(node=>node.dataset.path===path)?.querySelector('video');
 if (!frame || !frame.videoWidth || frame.readyState < 2) throw new Error('请先播放视频再截图');
 const scale=Math.min(1,1280/frame.videoWidth,1280/frame.videoHeight); const canvas=document.createElement('canvas'); canvas.width=Math.round(frame.videoWidth*scale); canvas.height=Math.round(frame.videoHeight*scale);
 canvas.getContext('2d').drawImage(frame,0,0,canvas.width,canvas.height); return canvas.toDataURL('image/png').split(',')[1];
}
