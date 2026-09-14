//! Filesystem preflights and file identities for recording and post-processing.
use std::{
    io,
    path::{Path, PathBuf},
};

pub fn available_bytes(path: &Path) -> io::Result<u64> {
    let existing = path
        .ancestors()
        .find(|p| p.exists())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "保存目录所在磁盘不可用"))?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0;
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(available)
    }
    #[cfg(not(windows))]
    {
        let _ = existing;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台没有磁盘空间检测实现",
        ))
    }
}
pub fn require_space(path: &Path, minimum: u64, extra: u64) -> io::Result<()> {
    let available = available_bytes(path)?;
    if available < minimum.saturating_add(extra) {
        return Err(io::Error::other("磁盘可用空间不足，已停止以保护现有录像"));
    }
    Ok(())
}
pub fn minimum_bytes(value: &str) -> u64 {
    (value.parse::<f64>().unwrap_or(2.0).clamp(0.1, 1_000_000.0) * 1024.0 * 1024.0 * 1024.0) as u64
}

/// Match the exact output or its numeric FFmpeg segments, never unrelated directory contents.
pub fn recording_outputs(pattern: &Path) -> io::Result<Vec<PathBuf>> {
    let parent = pattern
        .parent()
        .ok_or_else(|| io::Error::other("录制目录无效"))?;
    let name = pattern
        .file_name()
        .ok_or_else(|| io::Error::other("录制文件名无效"))?
        .to_string_lossy();
    let Some((prefix, suffix)) = name.split_once("%03d") else {
        return Ok(if pattern.is_file() {
            vec![pattern.to_path_buf()]
        } else {
            vec![]
        });
    };
    if !parent.exists() {
        return Ok(vec![]);
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        if filename
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(suffix))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if !metadata.is_file() || crate::storage::is_link(&metadata) {
                return Err(io::Error::other("录制输出包含不安全的路径"));
            }
            let sequence = filename
                .strip_prefix(prefix)
                .and_then(|name| name.strip_suffix(suffix))
                .and_then(|digits| digits.parse::<u64>().ok())
                .ok_or_else(|| io::Error::other("录制分段序号无效"))?;
            paths.push((sequence, entry.path()));
        }
    }
    paths.sort_unstable_by_key(|(sequence, _)| *sequence);
    Ok(paths.into_iter().map(|(_, path)| path).collect())
}
pub fn output_bytes(pattern: &Path) -> io::Result<u64> {
    recording_outputs(pattern)?
        .into_iter()
        .try_fold(0_u64, |total, p| {
            Ok(total.saturating_add(std::fs::metadata(p)?.len()))
        })
}
pub fn unique_output(root: &Path, relative: &Path) -> io::Result<PathBuf> {
    let root = root.canonicalize()?;
    for attempt in 0..100 {
        let candidate = if attempt == 0 {
            root.join(relative)
        } else {
            let name = relative
                .file_name()
                .ok_or_else(|| io::Error::other("录制文件名无效"))?
                .to_string_lossy();
            let (stem, ext) = name
                .rsplit_once('.')
                .ok_or_else(|| io::Error::other("录制格式无效"))?;
            root.join(relative.with_file_name(format!("{stem}_{}.{ext}", uuid::Uuid::new_v4())))
        };
        let rel = candidate
            .strip_prefix(&root)
            .map_err(io::Error::other)?
            .to_string_lossy();
        let parent = candidate
            .parent()
            .ok_or_else(|| io::Error::other("输出目录无效"))?;
        let parent_relative = parent
            .strip_prefix(&root)
            .map_err(io::Error::other)?
            .to_string_lossy();
        crate::storage::checked_target(&root, &parent_relative, true)?;
        crate::storage::relative_path(&rel)?;
        if std::fs::symlink_metadata(&candidate).is_err()
            && recording_outputs(&candidate)?.is_empty()
        {
            return Ok(candidate);
        }
    }
    Err(io::Error::other("无法分配未使用的录像文件名"))
}

pub fn matches_output(pattern: &Path, target: &Path) -> bool {
    if pattern.parent() != target.parent() {
        return false;
    }
    let pattern = pattern.file_name().unwrap_or_default().to_string_lossy();
    let target = target.file_name().unwrap_or_default().to_string_lossy();
    if let Some((before, after)) = pattern.split_once("%03d") {
        target
            .strip_prefix(before)
            .and_then(|s| s.strip_suffix(after))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    } else {
        pattern == target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn space_preflight_rejects_unreasonable_requirement() {
        let dir = tempfile::tempdir().unwrap();
        assert!(require_space(dir.path(), u64::MAX, 0).is_err());
    }
    #[test]
    fn counts_only_matching_segments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("live_000.ts"), [1; 5]).unwrap();
        std::fs::write(dir.path().join("live_001.ts"), [1; 7]).unwrap();
        std::fs::write(dir.path().join("other.mp4"), [1; 100]).unwrap();
        assert_eq!(output_bytes(&dir.path().join("live_%03d.ts")).unwrap(), 12);
    }
}
