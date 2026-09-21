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
use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    sync::{Arc, Weak},
    time::{Duration, Instant, SystemTime},
};
use tokio::{io::AsyncReadExt, sync::Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Preview {
    engine: Engine,
    stop: CancellationToken,
    slots: Arc<Semaphore>,
    probe_slots: Arc<Semaphore>,
    durations: Arc<std::sync::Mutex<DurationCache>>,
    probe_gates:
        Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>>,
    workers: tokio_util::task::TaskTracker,
    admission: Arc<std::sync::Mutex<()>>,
}
pub struct LiveInput {
    pub url: String,
    pub headers: Option<String>,
    pub proxy: Option<String>,
}

#[derive(Clone, PartialEq)]
struct FileStamp {
    identity: (u64, u64),
    size: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}
impl FileStamp {
    fn read(file: &std::fs::File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        let identity = storage::source_identity(file)?;
        Ok(Self {
            identity: (identity.volume, identity.index),
            size: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        })
    }
}

#[derive(Default)]
struct DurationCache(VecDeque<(PathBuf, FileStamp, MediaProbe, Instant)>);

#[derive(Clone, Debug, PartialEq)]
struct MediaProbe {
    duration: f64,
    copy: bool,
}

impl DurationCache {
    fn get(
        &mut self,
        path: &std::path::Path,
        stamp: &FileStamp,
        growing: bool,
    ) -> Option<MediaProbe> {
        let index = self.0.iter().position(|(p, s, _, at)| {
            p == path
                && (s == stamp
                    || (growing
                        && s.identity == stamp.identity
                        && s.created == stamp.created
                        && stamp.size > s.size
                        && at.elapsed() < Duration::from_secs(1)))
        })?;
        let entry = self.0.remove(index)?;
        let duration = entry.2.clone();
        self.0.push_back(entry);
        Some(duration)
    }
    fn insert(&mut self, path: PathBuf, stamp: FileStamp, duration: MediaProbe) {
        self.0.retain(|(p, _, _, _)| p != &path);
        self.0.push_back((path, stamp, duration, Instant::now()));
        while self.0.len() > 32 {
            self.0.pop_front();
        }
    }
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
            durations: Arc::new(std::sync::Mutex::new(DurationCache::default())),
            probe_gates: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
        let stamp = FileStamp::read(&lease)?;
        let growing = self.engine.is_recording_path(&target).await;
        let media = self
            .probe_duration(ffprobe, target.clone(), stamp, growing)
            .await?;
        let duration = media.duration;
        if start_seconds >= duration {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "目标时间尚未录制，请选择已录范围",
            ));
        }
        let playback_seconds = (duration - start_seconds).min(120.0);
        if media.copy {
            let mut command = preview_command(&ffmpeg);
            command
                .args(["-copyts", "-start_at_zero", "-ss"])
                .arg(format!("{:.3}", (start_seconds - 2.0).max(0.0)))
                .arg("-i")
                .arg(&target)
                .args([
                    "-to",
                    &format!("{:.3}", start_seconds + playback_seconds),
                    "-map",
                    "0:v:0",
                    "-map",
                    "0:a:0?",
                    "-c",
                    "copy",
                    "-avoid_negative_ts",
                    "disabled",
                    "-mpegts_copyts",
                    "1",
                    // Avoid negative DTS/B-frame wrap at the file start. The returned
                    // timestamp offset removes this mux-only shift from the UI timeline.
                    "-output_ts_offset",
                    "30",
                    "-f",
                    "mpegts",
                    "-muxdelay",
                    "0",
                    "pipe:1",
                ]);
            // Keep the original lease for the compatibility path if timestamps cannot be mapped.
            match self
                .pipe(
                    command,
                    Some(lease.try_clone()?),
                    permit,
                    Some((start_seconds, true)),
                )
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    log::debug!("预览时间戳无法直接定位，改用精确转码");
                }
                Err(error) => return Err(error),
            }
        } else {
            drop(permit);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "预览已达并发上限"))?;
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
            .arg("-i")
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
                "-g", "48",
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
        self.pipe(command, Some(lease), permit, Some((start_seconds, false)))
            .await
    }
    async fn pipe(
        &self,
        mut command: tokio::process::Command,
        lease: Option<std::fs::File>,
        permit: tokio::sync::OwnedSemaphorePermit,
        timeline: Option<(f64, bool)>,
    ) -> io::Result<Response<Body>> {
        let (mut output, cancelled) = {
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
            self.workers.spawn(async move { tokio::select! { biased;
                _ = stop.cancelled() => { let _ = child.kill().await; let _ = child.wait().await; },
                _ = worker_cancel.cancelled() => { let _ = child.kill().await; let _ = child.wait().await; },
                _ = child.wait() => {},
            } drop(lease); });
            (output, cancelled)
        };
        let guard = PreviewGuard {
            cancelled,
            _permit: permit,
        };
        let mut prefix = Vec::new();
        let mut offset = timeline.map_or(0.0, |(start, _)| start);
        if let Some((requested, true)) = timeline {
            let initial = async {
                loop {
                    let mut bytes = [0; 16 * 1024];
                    let count = output.read(&mut bytes).await?;
                    if count == 0 || prefix.len() >= 256 * 1024 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "预览片段缺少视频时间戳",
                        ));
                    }
                    prefix.extend_from_slice(&bytes[..count]);
                    if let Some(value) = first_video_timestamp(&prefix, requested + 30.0) {
                        return Ok(value - 30.0);
                    }
                }
            };
            offset = tokio::select! { biased;
                _ = self.stop.cancelled() => return Err(io::Error::new(io::ErrorKind::Interrupted, "应用正在退出")),
                result = tokio::time::timeout(Duration::from_secs(5), initial) => result
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "预览片段定位超时"))??,
            };
            if offset > requested + 0.12 || requested - offset > 15.0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "目标时间不在关键帧可定位范围",
                ));
            }
        }
        let chunks = stream::try_unfold(
            (output, guard, prefix),
            |(mut output, guard, prefix)| async move {
                if !prefix.is_empty() {
                    return Ok(Some((prefix, (output, guard, Vec::new()))));
                }
                let mut bytes = vec![0; 64 * 1024];
                let read = output.read(&mut bytes).await?;
                if read == 0 {
                    return Ok::<_, io::Error>(None);
                }
                bytes.truncate(read);
                Ok(Some((bytes, (output, guard, Vec::new()))))
            },
        );
        Response::builder()
            .header(header::CONTENT_TYPE, "video/mp2t")
            .header(header::CACHE_CONTROL, "no-store")
            .header("x-streamcap-offset", format!("{offset:.6}"))
            .header(
                "x-streamcap-preview-mode",
                if timeline.is_some_and(|(_, copy)| copy) {
                    "copy"
                } else {
                    "transcode"
                },
            )
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
        self.pipe(command, None, permit, None).await
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
        let (target, stamp) = storage
            .blocking(move |stop| {
                storage::check_cancel(&stop)?;
                let target = storage::checked_target(&root, &relative, false)?;
                let file = storage::open_file(&root, &relative)?;
                Ok((target, FileStamp::read(&file)?))
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
                Some(ffprobe) => match self
                    .probe_duration(ffprobe, target, stamp.clone(), is_recording)
                    .await
                {
                    Ok(media) => duration_seconds = Some(media.duration),
                    Err(error) => seek_error = Some(error.to_string()),
                },
                None => seek_error = Some("按时间预览需要 FFmpeg 和同目录 ffprobe".into()),
            }
        }
        Ok(PreviewInfo {
            format,
            is_recording,
            size: stamp.size,
            duration_seconds,
            seekable: duration_seconds.is_some(),
            seek_error,
        })
    }

    async fn probe_duration(
        &self,
        ffprobe: PathBuf,
        target: PathBuf,
        stamp: FileStamp,
        growing: bool,
    ) -> io::Result<MediaProbe> {
        storage::check_cancel(&self.stop)?;
        let gate = {
            let mut gates = self.probe_gates.lock().expect("probe gates");
            gates.retain(|_, gate| gate.strong_count() > 0);
            let gate = gates
                .get(&target)
                .and_then(Weak::upgrade)
                .unwrap_or_default();
            gates.insert(target.clone(), Arc::downgrade(&gate));
            gate
        };
        let _gate = tokio::select! { biased;
            _ = self.stop.cancelled() => return Err(io::Error::new(io::ErrorKind::Interrupted, "应用正在退出")),
            gate = gate.lock() => gate,
        };
        if let Some(duration) = self
            .durations
            .lock()
            .expect("preview durations")
            .get(&target, &stamp, growing)
        {
            return Ok(duration);
        }
        let cache_path = target.clone();
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
                    command.args(["-v","error","-protocol_whitelist","file","-probesize","2000000","-analyzeduration","2000000","-duration_probesize","2000000","-show_entries","format=duration:stream=codec_type,codec_name,pix_fmt","-of","json"])
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
                    let duration = data["format"]["duration"].as_str().and_then(|value|value.parse::<f64>().ok()).filter(|duration|duration.is_finite()&&*duration>0.0)
                        .ok_or_else(||io::Error::other("文件尚未写入可播放的完整片段"))?;
                    Ok(MediaProbe { duration, copy: can_copy(&data) })
                }.await;
                let _=sender.send(result);
            });
        }
        let duration = receiver.await.map_err(io::Error::other)??;
        self.durations.lock().expect("preview durations").insert(
            cache_path,
            stamp,
            duration.clone(),
        );
        Ok(duration)
    }
}

fn preview_command(ffmpeg: &std::path::Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(ffmpeg);
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-protocol_whitelist",
            "file",
            "-probesize",
            "2000000",
            "-analyzeduration",
            "2000000",
            "-duration_probesize",
            "2000000",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}

fn can_copy(data: &serde_json::Value) -> bool {
    let Some(streams) = data["streams"].as_array() else {
        return false;
    };
    let video = streams.iter().find(|s| s["codec_type"] == "video");
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");
    video.is_some_and(|s| {
        s["codec_name"] == "h264" && matches!(s["pix_fmt"].as_str(), Some("yuv420p" | "yuvj420p"))
    }) && audio.is_none_or(|s| s["codec_name"] == "aac")
}

/// The browser's TS remuxer normalizes its first video DTS to zero. Preserve that
/// original timeline offset so a copied keyframe is never presented as the requested frame.
fn first_video_timestamp(bytes: &[u8], requested: f64) -> Option<f64> {
    for packet in bytes.chunks_exact(188) {
        if packet[0] != 0x47 || packet[1] & 0x40 == 0 || packet[1] & 0x80 != 0 {
            continue;
        }
        let control = (packet[3] >> 4) & 3;
        if control != 1 && control != 3 {
            continue;
        }
        let at = 4 + if control == 3 {
            1 + packet[4] as usize
        } else {
            0
        };
        let Some(pes) = packet.get(at..) else {
            continue;
        };
        if pes.len() < 14 || pes[..3] != [0, 0, 1] || !(0xe0..=0xef).contains(&pes[3]) {
            continue;
        }
        let flags = pes[7] >> 6;
        let at = match flags {
            2 => 9,
            3 => 14,
            _ => continue,
        };
        let Some(timestamp) = pes.get(at..at + 5) else {
            continue;
        };
        if timestamp[0] & 1 == 0 || timestamp[2] & 1 == 0 || timestamp[4] & 1 == 0 {
            continue;
        }
        let ticks = ((u64::from(timestamp[0]) >> 1 & 7) << 30)
            | (u64::from(timestamp[1]) << 22)
            | ((u64::from(timestamp[2]) >> 1) << 15)
            | (u64::from(timestamp[3]) << 7)
            | (u64::from(timestamp[4]) >> 1);
        let seconds = (ticks as f64 / 90.0).floor() / 1000.0;
        let wrap = (1_u64 << 33) as f64 / 90_000.0;
        return Some(seconds + ((requested - seconds) / wrap).round() * wrap);
    }
    None
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

#[cfg(test)]
mod preview_cache_tests {
    use super::*;
    fn stamp() -> FileStamp {
        FileStamp {
            identity: (1, 1),
            size: 10,
            modified: Some(SystemTime::UNIX_EPOCH),
            created: None,
        }
    }
    fn media(duration: f64) -> MediaProbe {
        MediaProbe {
            duration,
            copy: true,
        }
    }
    #[test]
    fn duration_cache_is_bounded_and_invalidates_changed_recordings() {
        let stamp = stamp();
        let mut cache = DurationCache::default();
        for index in 0..40 {
            cache.insert(
                PathBuf::from(format!("{index}.ts")),
                stamp.clone(),
                media(index as f64 + 1.0),
            );
        }
        let path = std::path::Path::new("39.ts");
        assert_eq!(cache.0.len(), 32);
        assert!(cache
            .get(std::path::Path::new("0.ts"), &stamp, false)
            .is_none());
        assert_eq!(cache.get(path, &stamp, false), Some(media(40.0)));
        for changed in [
            FileStamp {
                size: 11,
                ..stamp.clone()
            },
            FileStamp {
                modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
                ..stamp.clone()
            },
            FileStamp {
                created: Some(SystemTime::UNIX_EPOCH),
                ..stamp.clone()
            },
            FileStamp {
                identity: (1, 2),
                ..stamp.clone()
            },
        ] {
            assert!(cache.get(path, &changed, false).is_none());
        }
    }
    #[test]
    fn growing_file_reuses_only_recent_same_identity_probe_and_does_not_extend_ttl() {
        let path = std::path::Path::new("growing.ts");
        let stamp = stamp();
        let mut cache = DurationCache::default();
        cache.insert(path.into(), stamp.clone(), media(40.0));
        let grown = FileStamp {
            size: 11,
            ..stamp.clone()
        };
        assert_eq!(cache.get(path, &grown, true), Some(media(40.0)));
        assert!(cache.get(path, &grown, false).is_none());
        assert!(cache
            .get(
                path,
                &FileStamp {
                    size: 9,
                    ..stamp.clone()
                },
                true
            )
            .is_none());
        assert!(cache
            .get(
                path,
                &FileStamp {
                    identity: (1, 2),
                    ..grown.clone()
                },
                true
            )
            .is_none());
        assert!(cache
            .get(
                path,
                &FileStamp {
                    modified: Some(SystemTime::now()),
                    ..stamp
                },
                true
            )
            .is_none());
        cache.0.front_mut().unwrap().3 = Instant::now() - Duration::from_secs(2);
        assert!(cache.get(path, &grown, true).is_none());
    }
    #[test]
    fn only_browser_compatible_first_video_and_audio_are_copied() {
        use serde_json::json;
        let mut data = json!({"streams":[{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p"},{"codec_type":"audio","codec_name":"aac"}]});
        assert!(can_copy(&data));
        data["streams"][0]["pix_fmt"] = json!("yuv420p10le");
        assert!(!can_copy(&data));
        data["streams"][0]["pix_fmt"] = json!("yuv420p");
        data["streams"][1]["codec_name"] = json!("ac3");
        assert!(!can_copy(&data));
        data["streams"][1]["codec_name"] = json!("aac");
        data["streams"][0]["codec_name"] = json!("hevc");
        assert!(!can_copy(&data));
        assert!(!can_copy(&json!({})));
    }
    #[test]
    fn parses_first_video_dts_and_wrap_without_trusting_audio_or_truncated_packets() {
        fn packet(seconds: f64) -> Vec<u8> {
            let wrap = 1_i64 << 33;
            let value = ((seconds * 90000.0) as i64).rem_euclid(wrap) as u64;
            let mut p = vec![0xff; 188];
            p[..4].copy_from_slice(&[0x47, 0x41, 0, 0x10]);
            p[4..13].copy_from_slice(&[0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5]);
            p[13..18].copy_from_slice(&[
                0x21 | ((value >> 29) as u8 & 14),
                (value >> 22) as u8,
                ((value >> 14) as u8 & 254) | 1,
                (value >> 7) as u8,
                ((value << 1) as u8 & 254) | 1,
            ]);
            p
        }
        assert_eq!(first_video_timestamp(&packet(591.25), 594.0), Some(591.25));
        assert!((first_video_timestamp(&packet(-0.1), 0.0).unwrap() + 0.1).abs() < 0.002);
        let mut audio = packet(1.0);
        audio[7] = 0xc0;
        assert!(first_video_timestamp(&audio, 1.0).is_none());
        assert!(first_video_timestamp(&[0; 10], 0.0).is_none());
    }
}
