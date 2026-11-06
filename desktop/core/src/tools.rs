//! Verified offline runtime tools and explicit application update checks.
use crate::{paths::Workspace, storage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleManifest {
    schema_version: u32,
    version: String,
    archive_sha256: String,
    files: Vec<BundleFile>,
}
#[derive(Clone, Deserialize)]
struct BundleFile {
    name: String,
    size: u64,
    sha256: String,
}
fn manifest() -> &'static BundleManifest {
    static MANIFEST: OnceLock<BundleManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let manifest: BundleManifest =
            serde_json::from_str(include_str!("../../ffmpeg-bundle.json"))
                .expect("valid pinned FFmpeg manifest");
        validate_manifest(&manifest).expect("safe pinned FFmpeg manifest");
        manifest
    })
}
pub fn bundle_version() -> &'static str {
    &manifest().version
}
pub fn bundled_directory(workspace: &Workspace) -> PathBuf {
    workspace
        .user_data_dir
        .join("runtime/ffmpeg")
        .join(&manifest().archive_sha256)
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn validate_manifest(manifest: &BundleManifest) -> io::Result<()> {
    let valid_hash = |hash: &str| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
    let names = ["ffmpeg.exe", "ffprobe.exe", "LICENSE", "README.txt"];
    if manifest.schema_version != 1
        || manifest.version.is_empty()
        || !valid_hash(&manifest.archive_sha256)
        || manifest.files.len() != names.len()
        || names.iter().any(|name| {
            manifest
                .files
                .iter()
                .filter(|file| file.name == *name)
                .count()
                != 1
        })
        || manifest.files.iter().any(|file| {
            file.size == 0 || file.size > 256 * 1024 * 1024 || !valid_hash(&file.sha256)
        })
    {
        return Err(invalid("内置录制工具清单无效"));
    }
    Ok(())
}
fn ensure_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !storage::is_link(&metadata) => Ok(()),
        Ok(_) => Err(invalid(format!(
            "工具缓存目录不是普通目录：{}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => ensure_directory(path),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}
fn open_regular(path: &Path, create_lock: bool) -> io::Result<File> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_file() || storage::is_link(&metadata) {
            return Err(invalid(format!("工具缓存不是普通文件：{}", path.display())));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    if create_lock {
        options.write(true).create(true).truncate(false);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
        if !create_lock {
            options.share_mode(1); // verify while denying writes and deletion
        }
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || storage::is_link(&metadata) {
        return Err(invalid("工具缓存包含链接或非普通文件"));
    }
    Ok(file)
}
fn checked_copy(
    input: &mut impl Read,
    output: &mut impl Write,
    spec: &BundleFile,
) -> io::Result<()> {
    let mut hash = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > spec.size {
            return Err(invalid("内置工具超过清单大小"));
        }
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if total != spec.size || format!("{:x}", hash.finalize()) != spec.sha256 {
        return Err(invalid(format!("内置工具校验失败：{}", spec.name)));
    }
    Ok(())
}
fn verify_directory(directory: &Path, manifest: &BundleManifest) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || storage::is_link(&metadata) {
        return Err(invalid("工具缓存目录包含链接"));
    }
    for spec in &manifest.files {
        let mut file = open_regular(&directory.join(&spec.name), false)?;
        if file.metadata()?.len() != spec.size {
            return Err(invalid(format!("工具缓存大小不符：{}", spec.name)));
        }
        checked_copy(&mut file, &mut io::sink(), spec)?;
    }
    Ok(())
}

/// Desktop startup calls this before creating any recorder. No network or PATH lookup.
/// Only this version's dedicated cache is used; existing user-managed tools are untouched.
pub fn ensure_bundled_tools(workspace: &Workspace, bytes: &[u8]) -> io::Result<()> {
    ensure_bundle(&workspace.user_data_dir, bytes, manifest())
}
fn ensure_bundle(root: &Path, bytes: &[u8], manifest: &BundleManifest) -> io::Result<()> {
    validate_manifest(manifest)?;
    let runtime = root.join("runtime");
    ensure_directory(&runtime)?;
    let parent = runtime.join("ffmpeg");
    ensure_directory(&parent)?;
    let lock = open_regular(
        &parent.join(format!("{}.lock", manifest.archive_sha256)),
        true,
    )?;
    let started = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::WouldBlock) if started.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(io::Error::other(format!("等待录制工具准备失败：{error}"))),
        }
    }
    let destination = parent.join(&manifest.archive_sha256);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return verify_directory(&destination, manifest).map_err(|error| {
                invalid(format!(
                    "录制工具缓存校验失败：{error}；请移走缓存目录后重新启动：{}",
                    destination.display()
                ))
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut archive = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(io::Error::other)?;
    if archive.len() != manifest.files.len() {
        return Err(invalid("内置工具包文件数量无效"));
    }
    let mut seen = std::collections::HashSet::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(io::Error::other)?;
        let spec = manifest
            .files
            .iter()
            .find(|file| file.name == entry.name())
            .ok_or_else(|| invalid("内置工具包包含未声明文件"))?;
        if entry.is_dir()
            || entry.enclosed_name().as_deref() != Some(Path::new(&spec.name))
            || entry.size() != spec.size
            || !seen.insert(spec.name.clone())
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(invalid("内置工具包包含重复、链接或越界文件"));
        }
    }
    crate::media_safety::require_space(
        &parent,
        128 * 1024 * 1024,
        manifest.files.iter().map(|file| file.size).sum(),
    )?;
    let temporary = parent.join(format!(".prepare-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&temporary)?;
    let result = (|| {
        for spec in &manifest.files {
            let mut entry = archive.by_name(&spec.name).map_err(io::Error::other)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(temporary.join(&spec.name))?;
            checked_copy(&mut entry, &mut output, spec)?;
            output.sync_all()?;
        }
        verify_directory(&temporary, manifest)?;
        fs::rename(&temporary, &destination)
    })();
    if result.is_err()
        && fs::symlink_metadata(&temporary).is_ok_and(|m| m.is_dir() && !storage::is_link(&m))
    {
        for spec in &manifest.files {
            let _ = fs::remove_file(temporary.join(&spec.name));
        }
        let _ = fs::remove_dir(&temporary);
    }
    result
}

#[derive(Deserialize, Serialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub html_url: String,
    pub body: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub release: ReleaseInfo,
    pub current_version: String,
}
pub async fn check_update() -> Result<ReleaseInfo, String> {
    let client = reqwest::Client::builder()
        .user_agent("StreamCap-native-update")
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| "更新客户端初始化失败")?;
    let result = client
        .get("https://api.github.com/repos/xiyangone/StreamCap/releases/latest")
        .send()
        .await
        .map_err(|_| "更新检查连接失败")?;
    if result.status() == 404 {
        return Err("当前仓库尚未发布发行版".into());
    }
    let info: ReleaseInfo = result
        .error_for_status()
        .map_err(|_| "更新服务返回错误")?
        .json()
        .await
        .map_err(|_| "更新响应无效")?;
    if !info
        .html_url
        .starts_with("https://github.com/xiyangone/StreamCap/releases/")
    {
        return Err("更新地址不属于当前项目".into());
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (BundleManifest, Vec<u8>) {
        let entries = [
            ("ffmpeg.exe", b"fixture-ffmpeg".as_slice()),
            ("ffprobe.exe", b"fixture-ffprobe".as_slice()),
            ("LICENSE", b"fixture-license".as_slice()),
            ("README.txt", b"fixture-source".as_slice()),
        ];
        let manifest = BundleManifest {
            schema_version: 1,
            version: "fixture".into(),
            archive_sha256: "a".repeat(64),
            files: entries
                .iter()
                .map(|(name, bytes)| BundleFile {
                    name: (*name).into(),
                    size: bytes.len() as u64,
                    sha256: format!("{:x}", Sha256::digest(bytes)),
                })
                .collect(),
        };
        (manifest, archive(&entries))
    }
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(
                    *name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    #[test]
    fn pinned_manifest_is_valid_and_includes_the_license_and_source_notice() {
        validate_manifest(manifest()).unwrap();
        assert!(manifest().files.iter().any(|file| file.name == "LICENSE"));
        assert!(manifest()
            .files
            .iter()
            .any(|file| file.name == "README.txt"));
    }
    #[test]
    fn offline_extraction_is_repeatable_and_preserves_user_tools() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("ffmpeg")).unwrap();
        fs::write(root.path().join("ffmpeg/ffmpeg.exe"), b"user-managed").unwrap();
        let (manifest, bytes) = fixture();
        ensure_bundle(root.path(), &bytes, &manifest).unwrap();
        ensure_bundle(root.path(), &[], &manifest).unwrap();
        let installed = root
            .path()
            .join("runtime/ffmpeg")
            .join(&manifest.archive_sha256);
        verify_directory(&installed, &manifest).unwrap();
        assert_eq!(
            fs::read(root.path().join("ffmpeg/ffmpeg.exe")).unwrap(),
            b"user-managed"
        );
        fs::write(installed.join("ffmpeg.exe"), b"tampered-tool").unwrap();
        assert!(ensure_bundle(root.path(), &bytes, &manifest).is_err());
        assert_eq!(
            fs::read(installed.join("ffmpeg.exe")).unwrap(),
            b"tampered-tool"
        );
    }
    #[test]
    fn damaged_missing_and_undeclared_entries_never_publish_tools() {
        let (manifest, valid) = fixture();
        let bad_hash = {
            let mut spec = manifest.clone();
            spec.files[0].sha256 = "0".repeat(64);
            spec
        };
        for (spec, bytes) in [
            (bad_hash, valid),
            (manifest.clone(), b"invalid zip".to_vec()),
            (
                manifest.clone(),
                archive(&[("ffmpeg.exe", b"fixture-ffmpeg")]),
            ),
            (
                manifest.clone(),
                archive(&[
                    ("../ffmpeg.exe", b"fixture-ffmpeg"),
                    ("ffprobe.exe", b"fixture-ffprobe"),
                    ("LICENSE", b"fixture-license"),
                    ("README.txt", b"fixture-source"),
                ]),
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            assert!(ensure_bundle(root.path(), &bytes, &spec).is_err());
            assert!(!root
                .path()
                .join("runtime/ffmpeg")
                .join(&spec.archive_sha256)
                .exists());
        }
    }
    #[test]
    fn concurrent_startups_publish_one_complete_bundle() {
        let root = tempfile::tempdir().unwrap();
        let (manifest, bytes) = fixture();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| ensure_bundle(root.path(), &bytes, &manifest)))
                .collect();
            for handle in handles {
                handle.join().unwrap().unwrap();
            }
        });
        verify_directory(
            &root
                .path()
                .join("runtime/ffmpeg")
                .join(&manifest.archive_sha256),
            &manifest,
        )
        .unwrap();
    }
    #[test]
    fn unexpected_files_in_cache_directory_positions_are_never_replaced() {
        let (manifest, bytes) = fixture();
        for relative in [
            "runtime".to_string(),
            "runtime/ffmpeg".into(),
            format!("runtime/ffmpeg/{}", manifest.archive_sha256),
        ] {
            let root = tempfile::tempdir().unwrap();
            let occupied = root.path().join(relative);
            fs::create_dir_all(occupied.parent().unwrap()).unwrap();
            fs::write(&occupied, b"existing unrelated file").unwrap();
            assert!(ensure_bundle(root.path(), &bytes, &manifest).is_err());
            assert_eq!(fs::read(occupied).unwrap(), b"existing unrelated file");
        }
    }
    #[test]
    fn relocation_reuses_the_profile_without_reading_the_executable_directory() {
        let root = tempfile::tempdir().unwrap();
        let (manifest, bytes) = fixture();
        ensure_bundle(root.path(), &bytes, &manifest).unwrap();
        let moved = root.path().join("中文 空格 #另一安装目录");
        fs::create_dir(&moved).unwrap();
        let workspace = Workspace {
            resource_dir: moved,
            user_data_dir: root.path().into(),
        };
        ensure_bundle(&workspace.user_data_dir, &[], &manifest).unwrap();
        assert_eq!(fs::read_dir(&workspace.resource_dir).unwrap().count(), 0);
    }
}
