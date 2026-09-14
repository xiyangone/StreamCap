//! Bounded local MPEG-TS preview. Dropping the HTTP body closes the reader and releases its slot.
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
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Preview {
    engine: Engine,
    stop: CancellationToken,
    slots: Arc<Semaphore>,
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
}
impl Preview {
    pub fn new(engine: Engine, stop: CancellationToken) -> Self {
        Self {
            engine,
            stop,
            slots: Arc::new(Semaphore::new(4)),
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
    ) -> io::Result<Response<Body>> {
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
        if self.engine.is_recording_path(&target).await {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "此格式请在停止录制后预览；TS 支持录中预览",
            ));
        }
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
                "-re",
                "-i",
            ])
            .arg(&target)
            .args([
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
    ) -> io::Result<PreviewInfo> {
        let (target, size) = storage
            .blocking(move |stop| {
                storage::check_cancel(&stop)?;
                let target = storage::checked_target(&root, &relative, false)?;
                let file = storage::open_file(&root, &relative)?;
                Ok((target, file.metadata()?.len()))
            })
            .await?;
        Ok(PreviewInfo {
            format: target
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase(),
            is_recording: self.engine.is_recording_path(&target).await,
            size,
        })
    }
    pub async fn stream(
        &self,
        storage: &Storage,
        root: PathBuf,
        relative: String,
    ) -> io::Result<Response<Body>> {
        let permit = self.slots.clone().try_acquire_owned().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "预览已达并发上限，请关闭其他预览",
            )
        })?;
        let (target, file) = storage
            .blocking(move |stop| {
                storage::check_cancel(&stop)?;
                let target = storage::checked_target(&root, &relative, false)?;
                if !target
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "增量预览仅支持 TS 文件",
                    ));
                }
                let file = storage::open_file(&root, &relative)?;
                Ok((target, file))
            })
            .await?;
        let mut file = tokio::fs::File::from_std(file);
        if self.engine.is_recording_path(&target).await {
            let offset = live_tail_offset(file.metadata().await?.len());
            file.seek(std::io::SeekFrom::Start(offset)).await?;
        }
        let state = (file, target, self.engine.clone(), self.stop.clone(), permit);
        let chunks = stream::try_unfold(
            state,
            |(mut file, target, engine, stop, permit)| async move {
                loop {
                    let mut bytes = vec![0_u8; 64 * 1024];
                    let read = tokio::select! { biased; _ = stop.cancelled() => return Ok::<_, io::Error>(None), result = file.read(&mut bytes) => result? };
                    if read > 0 {
                        bytes.truncate(read);
                        return Ok(Some((bytes, (file, target, engine, stop, permit))));
                    }
                    if !engine.is_recording_path(&target).await {
                        // A stopped writer may have flushed its last bytes between the first EOF and the status check.
                        let read = file.read(&mut bytes).await?;
                        if read == 0 {
                            return Ok(None);
                        }
                        bytes.truncate(read);
                        return Ok(Some((bytes, (file, target, engine, stop, permit))));
                    }
                    tokio::select! { biased; _ = stop.cancelled() => return Ok(None), _ = tokio::time::sleep(Duration::from_millis(150)) => {} }
                }
            },
        );
        Response::builder()
            .header(header::CONTENT_TYPE, "video/mp2t")
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::ACCEPT_RANGES, "none")
            .body(Body::from_stream(chunks))
            .map_err(io::Error::other)
    }
}

/// An active file can already be hours long. Preview only a bounded tail, aligned to TS packets.
fn live_tail_offset(size: u64) -> u64 {
    size.saturating_sub(4 * 1024 * 1024) / 188 * 188
}
#[cfg(test)]
#[test]
fn active_preview_does_not_queue_an_entire_large_recording() {
    assert_eq!(live_tail_offset(1024), 0);
    let size = 8_u64 * 1024 * 1024 * 1024;
    let offset = live_tail_offset(size);
    assert_eq!(offset % 188, 0);
    assert!(size - offset <= 4 * 1024 * 1024 + 188);
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
