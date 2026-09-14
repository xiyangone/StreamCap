// Local-only playback. Time-based TS seeking never downloads or converts the whole recording.
let activePlayers = 0;
export function activePlayerCount() { return activePlayers; }
const timelineFormats = new Set(['ts','flv','mkv','mov','nut','wma']);
export function attachMedia(video, apiOrigin, relativePath, onState) {
  let disposed=false, player=null, generation=0, poll=null, continuation=null, controls=null;
  let info=null, total=0, offset=0, wantedPlay=true, scrubbing=false, changing=false, probing=false;
  const abort=new AbortController(), removers=[];
  const english=document.documentElement.lang.startsWith('en');
  const label=(zh,en)=>english?en:zh;
  const update=(phase,message='')=>{if(!disposed)onState(phase,message);};
  const listen=(element,event,handler)=>{element.addEventListener(event,handler);removers.push(()=>element.removeEventListener(event,handler));};
  const clock=seconds=>{const value=Math.max(0,Math.floor(Number.isFinite(seconds)?seconds:0));return [Math.floor(value/3600),Math.floor(value/60)%60,value%60].map(n=>String(n).padStart(2,'0')).join(':');};
  const position=()=>offset+(Number.isFinite(video.currentTime)?video.currentTime:0);
  const release=()=>{if(player){const old=player;player=null;for(const method of ['pause','unload','detachMediaElement','destroy']){try{old[method]();}catch{ /* Complete every cleanup operation after partial initialization. */ }}}};
  let range,time,play,latest,mute;
  const render=()=>{
    if(!controls||disposed)return;
    const current=Math.min(total,position());
    if(!scrubbing){range.value=String(current);time.textContent=clock(current)+' / '+clock(total);}
    range.max=String(total);range.disabled=!(total>0);play.disabled=!(total>0);
    range.setAttribute('aria-valuetext',clock(Number(range.value))+' / '+clock(total));
    play.textContent=wantedPlay?label('暂停','Pause'):label('播放','Play');
    mute.textContent=video.muted?label('开启声音','Unmute'):label('静音','Mute');
    latest.hidden=!info?.isRecording;
    video.dataset.timelinePosition=String(current);video.dataset.timelineDuration=String(total);
  };
  const refreshInfo=async()=>{
    if(probing||disposed)return false;
    probing=true;
    try{
      const response=await fetch(apiOrigin+'/api/media/info?path='+encodeURIComponent(relativePath),{signal:abort.signal});
      if(!response.ok)throw Error(label('无法读取媒体文件信息','Unable to read media information'));
      const fresh=await response.json();if(disposed)return false;info=fresh;
      if(!timelineFormats.has(fresh.format))return true;
      if(Number.isFinite(fresh.durationSeconds)&&fresh.durationSeconds>0){total=fresh.durationSeconds;render();return true;}
      update('error',fresh.seekError||label('正在等待可播放的录制片段…','Waiting for recorded media…'));return false;
    }finally{probing=false;}
  };
  const makePlayer=(url,live,version)=>{
    const mpegts=window.mpegts;
    if(!mpegts||!mpegts.isSupported())throw Error(label('当前系统 WebView2 不支持媒体预览','This WebView2 runtime cannot preview this media'));
    mpegts.LoggingControl.enableAll=false;
    const next=mpegts.createPlayer({type:'mpegts',isLive:live,url},{enableWorker:false,enableWorkerForMSE:false,enableStashBuffer:false,lazyLoad:false,autoCleanupSourceBuffer:true,autoCleanupMaxBackwardDuration:30,autoCleanupMinBackwardDuration:15});
    next.on(mpegts.Events.ERROR,(type)=>{if(!disposed&&generation===version)update('error',type==='NetworkError'?label('本地预览连接中断，请重新选择播放位置。','Preview interrupted. Select a playback position again.'):label('无法解码当前片段，原文件未改动。','Cannot decode this segment. The original file is unchanged.'));});
    player=next;next.attachMediaElement(video);next.load();return next;
  };
  const loadAt=async(seconds,shouldPlay=true)=>{
    if(disposed||!(total>0))return;
    clearTimeout(continuation);continuation=null;
    const version=++generation;changing=true;release();
    offset=Math.max(0,Math.min(seconds,Math.max(0,total-0.3)));wantedPlay=shouldPlay;
    video.dataset.seekOffset=String(offset);render();update('loading',label('正在跳转到 ','Seeking to ')+clock(offset));
    try{
      const url=apiOrigin+'/api/media/transcode?path='+encodeURIComponent(relativePath)+'&start='+offset.toFixed(3);
      const next=makePlayer(url,false,version);
      if(shouldPlay)await next.play().catch(()=>{if(version===generation&&!disposed){wantedPlay=false;render();update('ready',label('点击播放继续','Click Play to continue'));}});
    }catch(error){if(version===generation&&!disposed)update('error',error.message);}
  };
  const continueAtEnd=async()=>{
    if(disposed||!wantedPlay||changing)return;
    const at=position();
    try{
      if(info?.isRecording)await refreshInfo();
      if(disposed||!wantedPlay)return;
      if(total-at>0.35){await loadAt(at,true);}
      else if(info?.isRecording){continuation=setTimeout(continueAtEnd,800);}
      else{wantedPlay=false;render();}
    }catch(error){if(!disposed&&error.name!=='AbortError')update('error',error.message);}
  };
  const buildControls=()=>{
    controls=document.createElement('div');controls.className='seek-controls';controls.setAttribute('role','group');controls.setAttribute('aria-label',label('录制播放控制','Recording playback controls'));
    const button=(text,action)=>{const node=document.createElement('button');node.type='button';node.className='seek-button';node.textContent=text;listen(node,'click',action);controls.append(node);return node;};
    play=button(label('暂停','Pause'),()=>{wantedPlay=!wantedPlay;if(wantedPlay){if(video.ended)loadAt(0,true);else video.play().catch(()=>update('ready',label('点击播放继续','Click Play to continue')));}else video.pause();render();});
    time=document.createElement('span');time.className='seek-time';controls.append(time);
    range=document.createElement('input');range.type='range';range.min='0';range.max=String(total);range.step='0.1';range.value='0';range.className='seek-range';range.setAttribute('aria-label',label('播放进度','Playback position'));controls.append(range);
    listen(range,'input',()=>{scrubbing=true;time.textContent=clock(Number(range.value))+' / '+clock(total);range.setAttribute('aria-valuetext',time.textContent);});
    listen(range,'change',()=>{const target=Number(range.value);scrubbing=false;loadAt(target,wantedPlay);});
    latest=button(label('回到最新','Latest'),async()=>{try{await refreshInfo();await loadAt(Math.max(0,total-3),true);}catch(error){if(!disposed)update('error',error.message);}});
    mute=button(label('开启声音','Unmute'),()=>{video.muted=!video.muted;render();});
    button(label('全屏','Full screen'),()=>{const host=video.closest('.media-preview');if(document.fullscreenElement)document.exitFullscreen().catch(()=>{});else host?.requestFullscreen().catch(()=>update('ready',label('当前窗口无法全屏','Full screen unavailable')));});
    video.controls=false;video.after(controls);video.closest('.media-preview')?.classList.add('time-seek-preview');
    listen(video,'loadedmetadata',()=>{changing=false;render();});listen(video,'play',()=>{if(!changing){wantedPlay=true;render();}});listen(video,'pause',()=>{if(!changing&&!video.ended){wantedPlay=false;render();}});listen(video,'timeupdate',render);listen(video,'volumechange',render);listen(video,'ended',continueAtEnd);render();
  };
  activePlayers++;video.muted=true;video.crossOrigin='anonymous';
  listen(video,'error',()=>update('error',label('无法解码此文件，原始文件未改动。','Cannot decode this file. The original is unchanged.')));
  listen(video,'playing',()=>{changing=false;update('playing');render();});
  (async()=>{
    try{
      update('loading',label('正在读取媒体信息…','Reading media information…'));
      if(relativePath.startsWith('live:')){
        makePlayer(apiOrigin+'/api/recordings/'+encodeURIComponent(relativePath.slice(5))+'/preview',true,generation);
        await player.play().catch(()=>update('ready',label('点击播放按钮开始预览','Click Play to preview')));return;
      }
      await refreshInfo();if(disposed)return;
      if(timelineFormats.has(info.format)){
        buildControls();
        if(total>0)await loadAt(info.isRecording?Math.max(0,total-3):0,true);
        poll=setInterval(async()=>{if(!disposed&&(info?.isRecording||!(total>0))){try{const before=total;const ready=await refreshInfo();if(ready&&before===0)await loadAt(info.isRecording?Math.max(0,total-3):0,true);}catch(error){if(!disposed&&error.name!=='AbortError')update('error',error.message);}}},4000);
      }else{
        if(info.isRecording&&['mp4','m4v'].includes(info.format))throw Error(label('该 MP4 尚在录制，完成封装后可预览。','MP4 preview is available after recording finalizes.'));
        video.src=apiOrigin+'/api/videos?path='+encodeURIComponent(relativePath);video.load();await video.play().catch(()=>update('ready',label('点击播放按钮开始预览','Click Play to preview')));
      }
    }catch(error){if(!disposed&&error.name!=='AbortError')update('error',error.message||label('无法加载本地媒体','Cannot load local media'));}
  })();
  return ()=>{
    if(disposed)return;disposed=true;generation++;abort.abort();clearInterval(poll);clearTimeout(continuation);
    removers.forEach(remove=>remove());release();controls?.remove();video.closest('.media-preview')?.classList.remove('time-seek-preview');
    video.pause();video.removeAttribute('src');video.load();video.controls=true;activePlayers--;
  };
}

export function captureFrame(path) {
 const frame=[...document.querySelectorAll('.media-preview')].find(node=>node.dataset.path===path)?.querySelector('video');
 if (!frame || !frame.videoWidth || frame.readyState < 2) throw new Error('请先播放视频再截图');
 const scale=Math.min(1,1280/frame.videoWidth,1280/frame.videoHeight); const canvas=document.createElement('canvas'); canvas.width=Math.round(frame.videoWidth*scale); canvas.height=Math.round(frame.videoHeight*scale);
 canvas.getContext('2d').drawImage(frame,0,0,canvas.width,canvas.height); return canvas.toDataURL('image/png').split(',')[1];
}
