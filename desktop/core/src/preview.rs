//! Time-addressed local preview. File probing and playback never modify recorded media.
use crate::{
    storage::{self, Storage},
    Engine,
};
use axum::{
    body::Body,
    http::{header, Response},
};
use futures::stream;
use serde::Serialize;
use std::{io, path::PathBuf, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, sync::Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Preview {
    engine: Engine,
    stop: CancellationToken,
    slots: Arc<Semaphore>,
    probe_slots: Arc<Semaphore>,
    workers: tokio_util::task::TaskTracker,
    admission: Arc<std::sync::Mutex<()>>,
}
pub struct LiveInput {
    pub url: String,
    pub headers: Option<String>,
    pub proxy: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewInfo {
    pub format: String,
    pub is_recording: bool,
    pub size: u64,
    pub duration_seconds: Option<f64>,
    pub seekable: bool,
    pub seek_error: Option<String>,
}
impl Preview {
    pub fn new(engine: Engine, stop: CancellationToken) -> Self {
        Self {
            engine,
            stop,
            slots: Arc::new(Semaphore::new(4)),
            probe_slots: Arc::new(Semaphore::new(4)),
            workers: tokio_util::task::TaskTracker::new(),
            admission: Arc::new(std::sync::Mutex::new(())),
        }
    }
    pub fn active(&self) -> usize {
        4 - self.slots.available_permits()
    }
    /// On-demand compatibility preview; recording files are opened read-only and never changed.
    pub async fn transcode(
        &self,
        storage: &Storage,
        root: PathBuf,
        relative: String,
        ffmpeg: PathBuf,
        start_seconds: f64,
    ) -> io::Result<Response<Body>> {
        if !start_seconds.is_finite() || start_seconds < 0.0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "预览时间必须为有限的非负秒数",
            ));
        }
        if self.stop.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "应用正在退出"));
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "预览已达并发上限"))?;
        let (target, lease) = storage
            .blocking(move |stop| {
                storage::check_cancel(&stop)?;
                Ok((
                    storage::checked_target(&root, &relative, false)?,
                    storage::open_file(&root, &relative)?,
                ))
            })
            .await?;
        let ffprobe = crate::paths::adjacent_ffprobe(&ffmpeg).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "按时间预览需要 FFmpeg 和同目录 ffprobe",
            )
        })?;
        let duration = self.probe_duration(ffprobe, target.clone()).await?;
        if start_seconds >= duration {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "目标时间尚未录制，请选择已录范围",
            ));
        }
        let playback_seconds = (duration - start_seconds).min(120.0);
        let mut command = tokio::process::Command::new(ffmpeg);
        command
            .args([
                "-v",
                "error",
                "-nostdin",
                "-protocol_whitelist",
                "file",
                "-threads",
                "2",
                "-probesize", "2000000",
                "-analyzeduration", "2000000",
                "-duration_probesize", "2000000",
                "-ss",
            ])
            .arg(format!("{start_seconds:.3}"))
            .args(["-re", "-i"])
            .arg(&target)
            .args(["-t", &format!("{playback_seconds:.3}")])
            .args([
                "-map",
                "0:v:0?",
                "-map",
                "0:a:0?",
                "-vf",
                "scale=w='min(1280,iw)':h='min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-pix_fmt",
                "yuv420p",
                "-threads", "2",
                "-c:a",
                "aac",
                "-f",
                "mpegts",
                "-muxdelay",
                "0",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        self.pipe(command, Some(lease), permit)
    }
    fn pipe(
        &self,
        mut command: tokio::process::Command,
        lease: Option<std::fs::File>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> io::Result<Response<Body>> {
        let _admission = self.admission.lock().expect("preview admission");
        storage::check_cancel(&self.stop)?;
        let mut child = command.spawn()?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("预览管道不可用"))?;
        let cancelled = CancellationToken::new();
        let stop = self.stop.clone();
        let worker_cancel = cancelled.clone();
        self.workers.spawn(async move{tokio::select!{biased;_=stop.cancelled()=>{let _=child.kill().await;let _=child.wait().await;},_=worker_cancel.cancelled()=>{let _=child.kill().await;let _=child.wait().await;},_=child.wait()=>{}}drop(lease);});
        let chunks = stream::try_unfold(
            (
                output,
                PreviewGuard {
                    cancelled,
                    _permit: permit,
                },
            ),
            |(mut output, guard)| async move {
                let mut bytes = vec![0; 64 * 1024];
                let read = output.read(&mut bytes).await?;
                if read == 0 {
                    return Ok::<_, io::Error>(None);
                }
                bytes.truncate(read);
                Ok(Some((bytes, (output, guard))))
            },
        );
        Response::builder()
            .header(header::CONTENT_TYPE, "video/mp2t")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from_stream(chunks))
            .map_err(io::Error::other)
    }
    pub async fn live(&self, ffmpeg: PathBuf, input: LiveInput) -> io::Result<Response<Body>> {
        storage::check_cancel(&self.stop)?;
        crate::engine::validate_recording_proxy(&input.url, input.proxy.as_deref())
            .map_err(io::Error::other)?;
        crate::platforms::http::validate_stream_url(&input.url)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "直播源地址无效"))?;
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "预览已达并发上限"))?;
        let mut command = tokio::process::Command::new(ffmpeg);
        command.args([
            "-v",
            "error",
            "-nostdin",
            "-rw_timeout",
            "15000000",
            "-protocol_whitelist",
            "http,https,tcp,tls,crypto,rtmp,rtmps",
        ]);
        if let Some(proxy) = input.proxy {
            command.args(["-http_proxy", &proxy]);
        }
        if let Some(headers) = input.headers {
            command.args(["-headers", &headers]);
        }
        command
            .args([
                "-i",
                &input.url,
                "-map",
                "0:v:0?",
                "-map",
                "0:a:0?",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-pix_fmt",
                "yuv420p",
                "-threads",
                "2",
                "-c:a",
                "aac",
                "-f",
                "mpegts",
                "-muxdelay",
                "0",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        self.pipe(command, None, permit)
    }

    pub async fn shutdown(&self) {
        {
            let _admission = self.admission.lock().expect("preview admission");
            self.stop.cancel();
            self.workers.close();
        }
        self.workers.wait().await;
    }
    pub async fn info(
        &self,
        storage: &Storage,
        root: PathBuf,
        relative: String,
        ffmpeg: Option<PathBuf>,
    ) -> io::Result<PreviewInfo> {
        let (target, size) = storage
            .blocking(move |stop| {
                storage::check_cancel(&stop)?;
                let target = storage::checked_target(&root, &relative, false)?;
                let file = storage::open_file(&root, &relative)?;
                Ok((target, file.metadata()?.len()))
            })
            .await?;
        let format = target
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let is_recording = self.engine.is_recording_path(&target).await;
        let needs_timeline = matches!(
            format.as_str(),
            "ts" | "flv" | "mkv" | "mov" | "nut" | "wma"
        );
        let mut duration_seconds = None;
        let mut seek_error = None;
        if needs_timeline {
            match ffmpeg.and_then(|path| crate::paths::adjacent_ffprobe(&path)) {
                Some(ffprobe) => match self.probe_duration(ffprobe, target).await {
                    Ok(duration) => duration_seconds = Some(duration),
                    Err(error) => seek_error = Some(error.to_string()),
                },
                None => seek_error = Some("按时间预览需要 FFmpeg 和同目录 ffprobe".into()),
            }
        }
        Ok(PreviewInfo {
            format,
            is_recording,
            size,
            duration_seconds,
            seekable: duration_seconds.is_some(),
            seek_error,
        })
    }

    async fn probe_duration(&self, ffprobe: PathBuf, target: PathBuf) -> io::Result<f64> {
        let permit = self.probe_slots.clone().try_acquire_owned().map_err(|_| {
            io::Error::new(io::ErrorKind::WouldBlock, "媒体信息读取繁忙，请稍后重试")
        })?;
        let stop = self.stop.clone();
        let cancelled = CancellationToken::new();
        let worker_cancelled = cancelled.clone();
        let _cancel_on_drop = CancelOnDrop(cancelled);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let _admission = self.admission.lock().expect("preview admission");
            storage::check_cancel(&stop)?;
            self.workers.spawn(async move {
                let _permit = permit;
                let result = async {
                    let mut command = tokio::process::Command::new(ffprobe);
                    command.args(["-v","error","-protocol_whitelist","file","-probesize","2000000","-analyzeduration","2000000","-duration_probesize","2000000","-show_entries","format=duration","-of","json"])
                        .arg(target).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true);
                    #[cfg(windows)] command.creation_flags(0x08000000);
                    let mut child = command.spawn()?;
                    let output = child.stdout.take().ok_or_else(|| io::Error::other("时长读取管道不可用"))?;
                    let reader = tokio::spawn(async move {let mut bytes=Vec::new();output.take(64*1024+1).read_to_end(&mut bytes).await?;Ok::<_,io::Error>(bytes)});
                    let status = tokio::select! {biased;
                        _=stop.cancelled()=>None,
                        _=worker_cancelled.cancelled()=>None,
                        result=tokio::time::timeout(Duration::from_secs(8),child.wait())=>result.ok().and_then(Result::ok),
                    };
                    if status.is_none() { let _=child.kill().await;let _=child.wait().await; }
                    let bytes = reader.await.map_err(io::Error::other)??;
                    if !status.is_some_and(|code|code.success()) || bytes.len()>64*1024 { return Err(io::Error::other("暂时无法读取已录时长，请稍后重试")); }
                    let data: serde_json::Value=serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                    data["format"]["duration"].as_str().and_then(|value|value.parse::<f64>().ok()).filter(|duration|duration.is_finite()&&*duration>0.0)
                        .ok_or_else(||io::Error::other("文件尚未写入可播放的完整片段"))
                }.await;
                let _=sender.send(result);
            });
        }
        receiver.await.map_err(io::Error::other)?
    }
}

struct PreviewGuard {
    cancelled: CancellationToken,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl Drop for PreviewGuard {
    fn drop(&mut self) {
        self.cancelled.cancel();
    }
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
