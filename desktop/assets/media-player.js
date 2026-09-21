// One control surface for native files and bounded, time-addressed compatibility segments.
// Seeking within a buffered segment never starts another FFmpeg process.
let activePlayers = 0;
let nextPlayerId = 0;
export function activePlayerCount() { return activePlayers; }
const timelineFormats = new Set(['ts', 'flv', 'mkv', 'mov', 'nut', 'wma']);
const icons = {
  play: '<path d="m8 5 11 7-11 7Z"/>',
  pause: '<path d="M8 5v14M16 5v14"/>',
  volume: '<path d="m11 5-6 4H2v6h3l6 4Z"/><path d="M15 8a6 6 0 0 1 0 8m3-11a10 10 0 0 1 0 14"/>',
  muted: '<path d="m11 5-6 4H2v6h3l6 4Z"/><path d="m17 9 5 6m0-6-5 6"/>',
  back: '<path d="M3 10a9 9 0 1 1 2 8M3 4v6h6"/><text x="9" y="15" stroke="none" fill="currentColor" font-size="8" font-family="sans-serif">10</text>',
  forward: '<path d="M21 10a9 9 0 1 0-2 8m2-14v6h-6"/><text x="7" y="15" stroke="none" fill="currentColor" font-size="8" font-family="sans-serif">10</text>',
  full: '<path d="M8 3H3v5m13-5h5v5M3 16v5h5m13-5v5h-5"/>',
  exitFull: '<path d="M3 8h5V3m13 5h-5V3M8 21v-5H3m13 5v-5h5"/>',
  camera: '<path d="M4 7h4l2-3h4l2 3h4a1 1 0 0 1 1 1v11H3V8a1 1 0 0 1 1-1Z"/><circle cx="12" cy="13" r="4"/>',
};
const svg = name => '<svg viewBox="0 0 24 24" width="19" height="19" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + icons[name] + '</svg>';
const clock = seconds => {
  const value = Math.max(0, Math.floor(Number.isFinite(seconds) ? seconds : 0));
  const parts = [Math.floor(value / 3600), Math.floor(value / 60) % 60, value % 60];
  return parts.map(n => String(n).padStart(2, '0')).join(':');
};

export function attachMedia(video, apiOrigin, relativePath, onState, onCapture) {
  const id = ++nextPlayerId;
  const host = video.closest('.media-preview');
  const english = document.documentElement.lang.startsWith('en');
  const label = (zh, en) => english ? en : zh;
  const abort = new AbortController();
  const removers = [];
  let disposed = false, player = null, generation = 0, mode = 'native', info = null;
  let total = 0, offset = 0, wantedPlay = true, scrubbing = false, changing = false, probing = false;
  let dragTarget = 0, pendingTarget = null, lastDispatch = 0, seekStarted = 0, acceptedTarget = null;
  let poll = null, continuation = null, seekTimer = null, loadTimeout = null, animation = null;
  let controls, range, time, play, mute, latest, buffered, transport, hint, rate, volume, fullscreen, freeze;
  const update = (phase, message = '') => { if (!disposed) onState(phase, message); };
  const listen = (element, event, handler) => {
    element.addEventListener(event, handler);
    removers.push(() => element.removeEventListener(event, handler));
  };
  const create = (tag, className, parent) => {
    const node = document.createElement(tag);
    node.className = className;
    if (parent) parent.append(node);
    return node;
  };
  const position = () => offset + (Number.isFinite(video.currentTime) ? video.currentTime : 0);
  const clamp = value => Math.max(0, Math.min(Number.isFinite(value) ? value : 0, Math.max(0, total - 0.12)));
  const release = () => {
    if (!player) return;
    const old = player; player = null;
    for (const method of ['pause', 'unload', 'detachMediaElement', 'destroy']) {
      try { old[method](); } catch { /* Finish cleanup after partial player initialization. */ }
    }
  };
  const nameButton = (node, text, icon) => {
    node.setAttribute('aria-label', text);
    node.parentElement.dataset.tooltip = text;
    if (node.dataset.icon !== icon) { node.innerHTML = svg(icon); node.dataset.icon = icon; }
  };
  const button = (text, icon, action) => {
    const wrapper = create('span', 'player-control-tip', transport);
    const node = create('button', 'seek-button', wrapper);
    node.type = 'button'; nameButton(node, text, icon); listen(node, 'click', action);
    return node;
  };
  const renderBuffer = () => {
    if (!buffered || !(total > 0)) return;
    const spans = [];
    for (let index = 0; index < video.buffered.length; index++) {
      const start = Math.max(0, offset + video.buffered.start(index));
      const end = Math.min(total, offset + video.buffered.end(index));
      if (end <= start) continue;
      const span = create('i', 'seek-buffer');
      span.style.left = (start / total * 100) + '%';
      span.style.width = ((end - start) / total * 100) + '%'; spans.push(span);
    }
    buffered.replaceChildren(...spans);
  };
  const render = () => {
    if (!controls || disposed) return;
    if (mode === 'native' && Number.isFinite(video.duration) && video.duration > 0) total = video.duration;
    const actual = Math.min(total || Infinity, position());
    const current = scrubbing ? dragTarget : changing && acceptedTarget != null ? acceptedTarget : actual;
    range.max = String(total || 1); range.value = String(current);
    range.disabled = !(total > 0); range.setAttribute('aria-valuetext', clock(current) + ' / ' + clock(total));
    controls.style.setProperty('--played', (total > 0 ? Math.min(100, current / total * 100) : 0) + '%');
    time.textContent = clock(current) + (total > 0 ? ' / ' + clock(total) : mode === 'live' ? ' · LIVE' : ' / —');
    nameButton(play, wantedPlay ? label('暂停', 'Pause') : label('播放', 'Play'), wantedPlay ? 'pause' : 'play');
    nameButton(mute, video.muted ? label('开启声音', 'Unmute') : label('静音', 'Mute'), video.muted ? 'muted' : 'volume');
    nameButton(fullscreen, document.fullscreenElement === host ? label('退出全屏', 'Exit full screen') : label('全屏', 'Full screen'), document.fullscreenElement === host ? 'exitFull' : 'full');
    volume.value = String(video.muted ? 0 : video.volume);
    latest.hidden = mode !== 'timeline' || !info?.isRecording;
    host?.classList.toggle('is-scrubbing', scrubbing);
    host?.classList.toggle('is-seeking', changing);
    video.dataset.timelinePosition = String(actual);
    video.dataset.timelineDuration = String(total);
    video.dataset.seekTarget = String(scrubbing ? dragTarget : acceptedTarget ?? actual);
    video.dataset.scrubbing = String(scrubbing);
  };
  const animate = () => {
    animation = null;
    if (disposed) return;
    render();
    if (!video.paused || scrubbing) animation = requestAnimationFrame(animate);
  };
  const startAnimation = () => { if (animation == null) animation = requestAnimationFrame(animate); };
  const playSafely = () => {
    const version = generation;
    video.play().catch(error => {
      if (!disposed && version === generation && error.name !== 'AbortError' && !scrubbing) {
        wantedPlay = false; render(); update('ready', label('点击播放继续', 'Click Play to continue'));
      }
    });
  };
  const clearLoadTimeout = () => { clearTimeout(loadTimeout); loadTimeout = null; };
  const watchLoad = () => {
    clearLoadTimeout();
    loadTimeout = setTimeout(() => {
      if (disposed || !changing) return;
      changing = false; render();
      update('error', label('预览加载超时，请重新选择播放位置。', 'Preview timed out. Select a playback position again.'));
      if (pendingTarget != null) flushSeek(true);
    }, 15000);
  };
  const retainFrame = () => {
    if (!video.videoWidth || video.readyState < 2) return;
    const scale = Math.min(1, 1280 / video.videoWidth);
    freeze.width = Math.round(video.videoWidth * scale); freeze.height = Math.round(video.videoHeight * scale);
    freeze.style.height = video.getBoundingClientRect().height + 'px';
    try { freeze.getContext('2d').drawImage(video, 0, 0, freeze.width, freeze.height); freeze.hidden = false; } catch { freeze.hidden = true; }
  };
  const settle = () => {
    if (disposed || video.readyState < 2 || video.seeking) return;
    if (changing) {
      if (mode === 'timeline' && acceptedTarget != null && position() < acceptedTarget - 0.08) {
        const target = acceptedTarget - offset;
        if (!hasBuffered(acceptedTarget)) return;
        video.currentTime = Math.max(0, target);
        return;
      }
      changing = false; clearLoadTimeout(); freeze.hidden = true;
      if (seekStarted) video.dataset.seekLatencyMs = String(performance.now() - seekStarted);
      video.dataset.framePosition = String(position());
      if (wantedPlay && !scrubbing) playSafely();
      update(video.paused ? 'ready' : 'playing');
    }
    render(); renderBuffer();
    if (pendingTarget != null) {
      if (Math.abs(pendingTarget - position()) < 0.07) pendingTarget = null;
      else scheduleFlush();
    }
  };
  const refreshInfo = async () => {
    if (probing || disposed) return false;
    probing = true;
    try {
      const response = await fetch(apiOrigin + '/api/media/info?path=' + encodeURIComponent(relativePath), { signal: abort.signal });
      if (!response.ok) throw Error(label('无法读取媒体文件信息', 'Unable to read media information'));
      const fresh = await response.json(); if (disposed) return false; info = fresh;
      if (!timelineFormats.has(fresh.format)) return true;
      if (Number.isFinite(fresh.durationSeconds) && fresh.durationSeconds > 0) { total = fresh.durationSeconds; render(); return true; }
      update('ready', fresh.seekError || label('正在等待可播放的录制片段…', 'Waiting for recorded media…')); return false;
    } finally { probing = false; }
  };
  const makePlayer = (url, live, version) => {
    const mpegts = window.mpegts;
    if (!mpegts || !mpegts.isSupported()) throw Error(label('当前系统 WebView2 不支持媒体预览', 'This WebView2 runtime cannot preview this media'));
    mpegts.LoggingControl.enableAll = false;
    // Read the backend's actual first-DTS offset before giving any bytes to MSE.
    // Every disposed seek aborts its own response; stale completions cannot move the timeline.
    class TimelineLoader extends mpegts.BaseLoader {
      constructor() { super('streamcap-timeline'); this._needStash = true; this.controller = null; }
      abort() { this.controller?.abort(); this.controller = null; this._status = mpegts.LoaderStatus.kIdle; }
      destroy() { this.abort(); super.destroy(); }
      open(source, range) {
        this.abort(); const controller = this.controller = new AbortController();
        this._status = mpegts.LoaderStatus.kConnecting;
        (async () => {
          let reader;
          try {
            const response = await fetch(source.url, { signal: controller.signal });
            if (!response.ok) throw Error('Preview HTTP ' + response.status);
            const header = response.headers.get('x-streamcap-offset');
            if (!disposed && version === generation && header != null) {
              const actual = Number(header);
              if (!Number.isFinite(actual) || Math.abs(actual - acceptedTarget) > 15.5) throw Error('Invalid preview timeline offset');
              offset = actual; video.dataset.seekOffset = String(offset);
              video.dataset.previewMode = response.headers.get('x-streamcap-preview-mode') || 'transcode';
            }
            if (disposed || version !== generation || controller.signal.aborted) { await response.body.cancel(); return; }
            this._status = mpegts.LoaderStatus.kBuffering;
            reader = response.body.getReader(); let received = 0;
            while (!controller.signal.aborted && !disposed && version === generation) {
              const { done, value } = await reader.read();
              if (done) { this._status = mpegts.LoaderStatus.kComplete; this._onComplete?.(range.from, range.from + received - 1); return; }
              const start = received; received += value.byteLength;
              this._onDataArrival?.(value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength), start, received);
            }
          } catch (error) {
            if (!controller.signal.aborted && !disposed && version === generation) {
              this._status = mpegts.LoaderStatus.kError;
              this._onError?.(mpegts.LoaderErrors.EXCEPTION, { code: -1, msg: error.message });
            }
          } finally { if (controller.signal.aborted) await reader?.cancel().catch(() => {}); }
        })();
      }
    }
    const next = mpegts.createPlayer({ type: 'mpegts', isLive: live, url }, {
      enableWorker: false, enableWorkerForMSE: false, enableStashBuffer: false,
      lazyLoad: false, accurateSeek: true, autoCleanupSourceBuffer: live,
      autoCleanupMaxBackwardDuration: 30, autoCleanupMinBackwardDuration: 15,
      ...(live ? {} : { customLoader: TimelineLoader }),
    });
    next.on(mpegts.Events.ERROR, (type) => {
      if (!disposed && generation === version) {
        changing = false; clearLoadTimeout(); render();
        update('error', type === 'NetworkError' ? label('本地预览连接中断，请重新选择播放位置。', 'Preview interrupted. Select a playback position again.') : label('无法解码当前片段，原文件未改动。', 'Cannot decode this segment. The original file is unchanged.'));
      }
    });
    player = next; next.attachMediaElement(video); next.load(); return next;
  };
  const loadAt = seconds => {
    if (disposed || !(total > 0)) return;
    clearTimeout(continuation); continuation = null;
    retainFrame(); changing = true; const version = ++generation; release();
    offset = clamp(seconds); acceptedTarget = offset; seekStarted = performance.now();
    video.dataset.seekOffset = String(offset);
    video.dataset.segmentLoads = String(Number(video.dataset.segmentLoads || 0) + 1);
    render(); update('loading', label('正在定位画面…', 'Seeking…')); watchLoad();
    try {
      makePlayer(apiOrigin + '/api/media/transcode?path=' + encodeURIComponent(relativePath) + '&start=' + offset.toFixed(3), false, version);
      if (wantedPlay && !scrubbing) playSafely();
    } catch (error) { changing = false; clearLoadTimeout(); render(); update('error', error.message); }
  };
  const hasBuffered = target => {
    const local = target - offset;
    for (let i = 0; i < video.buffered.length; i++) {
      if (local >= video.buffered.start(i) && local < video.buffered.end(i) - 0.05) return true;
    }
    return false;
  };
  function flushSeek(force = false) {
    clearTimeout(seekTimer); seekTimer = null;
    if (disposed || pendingTarget == null || !(total > 0)) return;
    if (changing && acceptedTarget != null && Math.abs(pendingTarget - acceptedTarget) < 0.07) { pendingTarget = null; return; }
    const target = clamp(pendingTarget); pendingTarget = null; lastDispatch = performance.now();
    const bufferedTarget = mode === 'timeline' && hasBuffered(target);
    if (mode === 'native' || bufferedTarget) {
      acceptedTarget = target; seekStarted = performance.now(); changing = true; watchLoad();
      video.dataset.seekKind = bufferedTarget ? 'buffered' : 'native';
      video.currentTime = mode === 'native' ? target : target - offset;
      if (!video.seeking) settle();
    } else { video.dataset.seekKind = 'segment'; loadAt(target); }
    render();
  }
  function scheduleFlush() {
    if (disposed) return;
    clearTimeout(seekTimer);
    const interval = mode === 'native' || (pendingTarget != null && hasBuffered(pendingTarget)) ? 40 : 120;
    seekTimer = setTimeout(() => flushSeek(), scrubbing ? interval : Math.max(0, interval - (performance.now() - lastDispatch)));
  }
  const requestSeek = (target, force = false) => {
    if (!(total > 0) || mode === 'live') return;
    pendingTarget = clamp(target); dragTarget = pendingTarget;
    if (force) flushSeek(true); else scheduleFlush();
    render();
  };
  const beginScrub = () => {
    if (scrubbing) return;
    scrubbing = true; dragTarget = Number(range.value); video.pause(); startAnimation(); render();
  };
  const finishScrub = () => {
    if (!scrubbing) return;
    const target = dragTarget; scrubbing = false;
    if (Math.abs(position() - target) > 0.06 || pendingTarget != null) requestSeek(target, true);
    else { pendingTarget = null; if (wantedPlay) playSafely(); }
    hint.hidden = true; render();
  };
  const togglePlay = () => {
    wantedPlay = !wantedPlay;
    if (wantedPlay) { if (video.ended && total > 0) requestSeek(0, true); else playSafely(); }
    else video.pause();
    render();
  };
  const continueAtEnd = async () => {
    if (disposed || !wantedPlay || changing || scrubbing) return;
    const at = position();
    try {
      if (info?.isRecording) await refreshInfo();
      if (disposed || !wantedPlay || scrubbing) return;
      if (mode === 'timeline' && total - at > 0.35) loadAt(at);
      else if (info?.isRecording) continuation = setTimeout(continueAtEnd, 800);
      else { wantedPlay = false; render(); }
    } catch (error) { if (!disposed && error.name !== 'AbortError') update('error', error.message); }
  };
  const buildControls = () => {
    controls = create('div', 'seek-controls'); controls.setAttribute('role', 'group');
    controls.setAttribute('aria-label', label('录制播放控制', 'Recording playback controls'));
    const timeline = create('div', 'seek-timeline', controls);
    create('div', 'seek-track', timeline); buffered = create('div', 'seek-buffered', timeline);
    create('div', 'seek-played', timeline);
    hint = create('output', 'seek-hover-time', timeline); hint.hidden = true;
    range = create('input', 'seek-range', timeline); range.type = 'range'; range.min = '0'; range.max = '1'; range.step = '0.01'; range.value = '0';
    range.setAttribute('aria-label', label('播放进度', 'Playback position'));
    listen(range, 'pointerdown', beginScrub);
    listen(range, 'input', () => { const target = Number(range.value); beginScrub(); dragTarget = target; requestSeek(target); });
    listen(range, 'change', finishScrub); listen(range, 'pointerup', finishScrub); listen(range, 'pointercancel', finishScrub);
    listen(range, 'blur', finishScrub);
    listen(range, 'pointermove', event => {
      if (!(total > 0)) return;
      const rect = range.getBoundingClientRect(); const fraction = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
      hint.textContent = clock(total * fraction); hint.style.left = (fraction * 100) + '%'; hint.hidden = false;
    });
    listen(range, 'pointerleave', () => { if (!scrubbing) hint.hidden = true; });
    transport = create('div', 'seek-transport', controls);
    play = button(label('暂停', 'Pause'), 'pause', togglePlay); play.classList.add('seek-play');
    button(label('后退 10 秒', 'Back 10 seconds'), 'back', () => requestSeek(position() - 10, true));
    button(label('前进 10 秒', 'Forward 10 seconds'), 'forward', () => requestSeek(position() + 10, true));
    time = create('span', 'seek-time', transport); time.setAttribute('aria-live', 'off');
    latest = create('button', 'seek-latest', transport); latest.type = 'button'; latest.textContent = label('回到最新', 'Latest');
    listen(latest, 'click', async () => { try { await refreshInfo(); wantedPlay = true; requestSeek(Math.max(0, total - 3), true); } catch (error) { if (!disposed) update('error', error.message); } });
    create('span', 'seek-spacer', transport);
    mute = button(label('开启声音', 'Unmute'), 'muted', () => { video.muted = !video.muted; render(); });
    volume = create('input', 'seek-volume', transport); volume.type = 'range'; volume.min = '0'; volume.max = '1'; volume.step = '0.05';
    volume.setAttribute('aria-label', label('音量', 'Volume'));
    listen(volume, 'input', () => { video.volume = Number(volume.value); video.muted = video.volume === 0; render(); });
    const speed = create('span', 'player-control-tip', transport); speed.dataset.tooltip = label('播放速度', 'Playback speed');
    rate = create('select', 'seek-rate', speed); rate.setAttribute('aria-label', label('播放速度', 'Playback speed'));
    for (const value of [0.5, 0.75, 1, 1.25, 1.5, 2]) { const option = document.createElement('option'); option.value = String(value); option.textContent = value + '×'; rate.append(option); }
    rate.value = '1'; listen(rate, 'change', () => { video.defaultPlaybackRate = video.playbackRate = Number(rate.value); });
    if (onCapture && !relativePath.startsWith('live:')) button(label('保存截图', 'Save screenshot'), 'camera', () => onCapture());
    fullscreen = button(label('全屏', 'Full screen'), 'full', () => {
      if (document.fullscreenElement === host) document.exitFullscreen().catch(() => {});
      else host?.requestFullscreen().catch(() => update('ready', label('当前窗口无法全屏', 'Full screen unavailable')));
    });
    freeze = create('canvas', 'player-freeze'); freeze.hidden = true; freeze.setAttribute('aria-hidden', 'true');
    video.before(freeze); video.after(controls); video.controls = false;
    host?.classList.add('enhanced-preview');
    // Fullscreen children extend beyond the dialog rectangle; their clicks are not backdrop clicks.
    if (host) listen(host, 'click', event => event.stopPropagation());
    const dialog = host?.closest('dialog');
    if (dialog) {
      const cancelFullscreen = event => {
        if (document.fullscreenElement !== host) return;
        event.preventDefault(); event.stopImmediatePropagation();
        document.exitFullscreen().catch(() => {});
      };
      dialog.addEventListener('cancel', cancelFullscreen, true);
      removers.push(() => dialog.removeEventListener('cancel', cancelFullscreen, true));
    }
    listen(video, 'click', togglePlay);
    listen(controls, 'keydown', event => {
      if (event.target.matches('input,select,button')) return;
      if (event.key === ' ') { event.preventDefault(); togglePlay(); }
      if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); requestSeek(position() + (event.key === 'ArrowLeft' ? -5 : 5), true); }
    });
    controls.tabIndex = 0; controls.id = 'player-controls-' + id;
    listen(document, 'fullscreenchange', render);
  };

  activePlayers++; video.muted = true; video.crossOrigin = 'anonymous';
  video.dataset.segmentLoads = '0'; video.dataset.seekOffset = '0';
  buildControls();
  listen(video, 'loadedmetadata', () => { video.playbackRate = Number(rate.value); render(); renderBuffer(); });
  listen(video, 'durationchange', () => { render(); renderBuffer(); });
  listen(video, 'progress', () => { renderBuffer(); settle(); });
  listen(video, 'loadeddata', settle); listen(video, 'seeked', settle); listen(video, 'canplay', settle);
  listen(video, 'play', () => { if (!changing && !scrubbing) wantedPlay = true; startAnimation(); render(); });
  listen(video, 'pause', () => { if (!changing && !scrubbing && !video.ended) wantedPlay = false; render(); });
  listen(video, 'playing', () => { settle(); update('playing'); startAnimation(); });
  listen(video, 'timeupdate', render); listen(video, 'volumechange', render); listen(video, 'ended', continueAtEnd);
  listen(video, 'error', () => { if (!disposed) { changing = false; clearLoadTimeout(); render(); update('error', label('无法解码此文件，原始文件未改动。', 'Cannot decode this file. The original is unchanged.')); } });
  render();
  (async () => {
    try {
      update('loading', label('正在读取媒体信息…', 'Reading media information…'));
      if (relativePath.startsWith('live:')) {
        mode = 'live'; video.dataset.playerMode = mode;
        makePlayer(apiOrigin + '/api/recordings/' + encodeURIComponent(relativePath.slice(5)) + '/preview', true, generation); playSafely(); render(); return;
      }
      await refreshInfo(); if (disposed) return;
      mode = timelineFormats.has(info.format) ? 'timeline' : 'native'; video.dataset.playerMode = mode;
      if (mode === 'timeline') {
        if (total > 0) loadAt(info.isRecording ? Math.max(0, total - 3) : 0);
        poll = setInterval(async () => {
          if (!disposed && (info?.isRecording || !(total > 0))) {
            try { const before = total; const ready = await refreshInfo(); if (ready && before === 0) loadAt(info.isRecording ? Math.max(0, total - 3) : 0); }
            catch (error) { if (!disposed && error.name !== 'AbortError') update('error', error.message); }
          }
        }, 1000);
      } else {
        if (info.isRecording && ['mp4', 'm4v'].includes(info.format)) throw Error(label('该 MP4 尚在录制，完成封装后可预览。', 'MP4 preview is available after recording finalizes.'));
        video.src = apiOrigin + '/api/videos?path=' + encodeURIComponent(relativePath); video.load(); playSafely();
      }
      render();
    } catch (error) { if (!disposed && error.name !== 'AbortError') update('error', error.message); }
  })();
  return () => {
    if (disposed) return;
    disposed = true; generation++; abort.abort(); clearInterval(poll); clearTimeout(continuation); clearTimeout(seekTimer); clearLoadTimeout(); cancelAnimationFrame(animation);
    removers.forEach(remove => remove()); release(); controls.remove(); freeze.remove();
    host?.classList.remove('enhanced-preview', 'is-scrubbing', 'is-seeking');
    video.pause(); video.removeAttribute('src'); video.load(); video.controls = true; activePlayers--;
  };
}

export function captureFrame(path) {
  const frame = [...document.querySelectorAll('.media-preview')].find(node => node.dataset.path === path)?.querySelector('video');
  if (!frame || !frame.videoWidth || frame.readyState < 2) throw new Error('请先播放视频再截图');
  const scale = Math.min(1, 1280 / frame.videoWidth, 1280 / frame.videoHeight);
  const canvas = document.createElement('canvas'); canvas.width = Math.round(frame.videoWidth * scale); canvas.height = Math.round(frame.videoHeight * scale);
  canvas.getContext('2d').drawImage(frame, 0, 0, canvas.width, canvas.height);
  return canvas.toDataURL('image/png').split(',')[1];
}
