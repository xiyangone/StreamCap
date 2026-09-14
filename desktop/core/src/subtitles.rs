//! Timestamp sidecars for exactly the files created by one recording attempt.
use chrono::{DateTime, Local};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
fn stamp(milliseconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        milliseconds / 3_600_000,
        (milliseconds / 60_000) % 60,
        (milliseconds / 1000) % 60,
        milliseconds % 1000
    )
}
fn cue(index: u64, end: u64, start: DateTime<Local>) -> String {
    let from = index * 1000;
    let wall = start + chrono::Duration::seconds(index as i64);
    format!(
        "{}\n{} --> {}\n{}\n\n",
        index + 1,
        stamp(from),
        stamp((from + 1000).min(end)),
        wall.format("%Y-%m-%d %H:%M:%S")
    )
}
pub async fn generate(
    root: &Path,
    pattern: &Path,
    mut wall: DateTime<Local>,
    ffprobe: &Path,
) -> io::Result<Vec<PathBuf>> {
    let root = root.canonicalize()?;
    let mut generated = Vec::new();
    for source in crate::media_safety::recording_outputs(pattern)? {
        let relative = source
            .strip_prefix(&root)
            .map_err(|_| io::Error::other("字幕源文件越界"))?
            .to_string_lossy()
            .replace('\\', "/");
        let _lease = crate::storage::open_conversion_source(&root, &relative)?;
        let mut command = crate::postprocess::command(ffprobe);
        command
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(&source);
        let stop = CancellationToken::new();
        let work = crate::postprocess::run(command, &stop);
        tokio::pin!(work);
        let bytes = tokio::select! {result=&mut work=>result,_=tokio::time::sleep(Duration::from_secs(15))=>{stop.cancel();work.await}}?;
        let seconds = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 604800.0)
            .ok_or_else(|| io::Error::other("字幕源文件时长无效或超过 7 天"))?;
        let duration = (seconds * 1000.0).round().max(1.0) as u64;
        let output = source.with_extension("srt");
        if std::fs::symlink_metadata(&output).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "同名时间字幕已存在，未覆盖",
            ));
        }
        let temporary =
            source.with_file_name(format!(".streamcap-subtitle-{}.srt", uuid::Uuid::new_v4()));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        let result = async {
            let mut buffer = String::with_capacity(65536);
            for index in 0..duration.div_ceil(1000) {
                buffer.push_str(&cue(index, duration, wall));
                if buffer.len() >= 60000 {
                    file.write_all(buffer.as_bytes()).await?;
                    buffer.clear();
                }
            }
            file.write_all(buffer.as_bytes()).await?;
            file.flush().await?;
            file.sync_all().await?;
            Ok::<_, io::Error>(())
        }
        .await;
        drop(file);
        if let Err(error) = result {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
        if let Err(error) = crate::storage::publish_new_file(&temporary, &output) {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
        generated.push(output);
        wall += chrono::Duration::milliseconds(duration as i64);
    }
    Ok(generated)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cues_keep_fractional_end_and_wall_clock() {
        let start = chrono::DateTime::parse_from_rfc3339("2026-09-13T10:00:00+08:00")
            .unwrap()
            .with_timezone(&Local);
        let line = cue(1, 1550, start);
        assert!(line.contains("00:00:01,000 --> 00:00:01,550"));
        assert!(line.contains(
            &(start + chrono::Duration::seconds(1))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        ));
    }
}
