//! Explicit update checks and verified project-data FFmpeg installation; never runs on startup.
use crate::{paths::Workspace, storage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub state: String,
    pub message: String,
    pub bytes: u64,
}
#[derive(Clone)]
pub struct Tools {
    workspace: Workspace,
    stop: CancellationToken,
    tasks: TaskTracker,
    status: Arc<std::sync::Mutex<ToolStatus>>,
    busy: Arc<std::sync::atomic::AtomicBool>,
    admission: Arc<std::sync::Mutex<()>>,
}
impl Tools {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            status: Arc::new(std::sync::Mutex::new(ToolStatus {
                state: "idle".into(),
                ..Default::default()
            })),
            busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            admission: Arc::new(std::sync::Mutex::new(())),
        }
    }
    pub fn status(&self) -> ToolStatus {
        self.status.lock().expect("tools status").clone()
    }
    fn report(&self, state: &str, message: &str, bytes: u64) {
        *self.status.lock().expect("tools status") = ToolStatus {
            state: state.into(),
            message: message.into(),
            bytes,
        };
    }
    pub fn install(&self) -> Result<(), String> {
        let _admission = self.admission.lock().map_err(|_| "工具任务锁异常")?;
        if self.stop.is_cancelled() {
            return Err("应用正在退出".into());
        }
        if self.workspace.user_data_dir.join("ffmpeg").exists() {
            return Err("用户数据目录已有 FFmpeg，未覆盖；可手动替换或使用 PATH 中的工具".into());
        }
        if self.busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err("FFmpeg 安装正在进行".into());
        }
        let manager = self.clone();
        self.report("downloading", "正在下载并验证 FFmpeg", 0);
        self.tasks.spawn(async move {
            let result = manager.download().await;
            if let Err(error) = result {
                manager.report("failed", &format!("安装未完成：{error}"), 0);
            } else {
                manager.report("complete", "FFmpeg 与 ffprobe 已就绪", 0);
            }
            manager
                .busy
                .store(false, std::sync::atomic::Ordering::SeqCst);
        });
        Ok(())
    }
    async fn download(&self) -> Result<(), String> {
        let base = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 3 {
                    return attempt.error("too many redirects");
                }
                let url = attempt.url();
                if url.scheme() == "https"
                    && url
                        .host_str()
                        .is_some_and(|host| matches!(host, "www.gyan.dev" | "gyan.dev"))
                {
                    attempt.follow()
                } else {
                    attempt.error("unexpected download host")
                }
            }))
            .build()
            .map_err(|_| "无法初始化下载")?;
        let work = async {
            let checksum = client
                .get(format!("{base}.sha256"))
                .send()
                .await
                .map_err(|_| "校验文件下载失败")?
                .error_for_status()
                .map_err(|_| "校验服务器响应失败")?
                .text()
                .await
                .map_err(|_| "无法读取校验文件")?;
            let expected = checksum
                .split_whitespace()
                .next()
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or("校验文件格式无效")?;
            let mut response = client
                .get(base)
                .send()
                .await
                .map_err(|_| "FFmpeg 下载失败")?
                .error_for_status()
                .map_err(|_| "下载服务器响应失败")?;
            if response
                .content_length()
                .is_some_and(|n| n > 300 * 1024 * 1024)
            {
                return Err("下载包超过大小限制".into());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "FFmpeg 下载中断")? {
                if bytes.len() + chunk.len() > 300 * 1024 * 1024 {
                    return Err("下载包超过大小限制".into());
                }
                bytes.extend_from_slice(&chunk);
                self.report("downloading", "正在下载并验证 FFmpeg", bytes.len() as u64);
            }
            let actual = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            if !actual.eq_ignore_ascii_case(expected) {
                return Err("下载校验不一致，未安装".into());
            }
            Ok::<_, String>(bytes)
        };
        let bytes = tokio::select! {biased;_=self.stop.cancelled()=>return Err("安装已取消".into()),result=work=>result}?;
        let directory = self.workspace.user_data_dir.clone();
        let stop = self.stop.clone();
        // Once extraction starts, always await its owner. Dropping a spawn_blocking
        // handle cannot stop its thread and would otherwise leave work after exit.
        tokio::task::spawn_blocking(move || extract_tools(bytes, directory, stop))
            .await
            .map_err(|_| "安装任务异常")?
            .map_err(|e| e.to_string())
    }
    pub async fn shutdown(&self) {
        {
            let _admission = self.admission.lock().expect("tool admission");
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
fn extract_tools(bytes: Vec<u8>, root: PathBuf, stop: CancellationToken) -> io::Result<()> {
    crate::media_safety::require_space(&root, 128 * 1024 * 1024, bytes.len() as u64)?;
    let mut zip = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(io::Error::other)?;
    let temporary = root.join(format!(".ffmpeg-install-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temporary)?;
    let result = (|| {
        for wanted in ["ffmpeg.exe", "ffprobe.exe"] {
            storage::check_cancel(&stop)?;
            let mut found = false;
            for index in 0..zip.len() {
                let mut entry = zip.by_index(index).map_err(io::Error::other)?;
                let name = entry
                    .enclosed_name()
                    .ok_or_else(|| io::Error::other("安装包包含越界路径"))?;
                if name.file_name().and_then(|s| s.to_str()) != Some(wanted) {
                    continue;
                }
                if found
                    || entry.is_dir()
                    || entry.size() > 256 * 1024 * 1024
                    || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
                {
                    return Err(io::Error::other("安装包内容无效"));
                }
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(temporary.join(wanted))?;
                copy_entry(&mut entry, &mut file, &stop)?;
                file.sync_all()?;
                found = true;
            }
            if !found {
                return Err(io::Error::other("安装包缺少 FFmpeg 或 ffprobe"));
            }
        }
        storage::check_cancel(&stop)?;
        let destination = root.join("ffmpeg");
        if destination.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "已有 FFmpeg，未覆盖",
            ));
        }
        std::fs::rename(&temporary, destination)
    })();
    if result.is_err() {
        for name in ["ffmpeg.exe", "ffprobe.exe"] {
            let p = temporary.join(name);
            if p.is_file() {
                let _ = std::fs::remove_file(p);
            }
        }
        let _ = std::fs::remove_dir(&temporary);
    }
    result
}
fn copy_entry(
    input: &mut impl io::Read,
    output: &mut impl io::Write,
    stop: &CancellationToken,
) -> io::Result<()> {
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        storage::check_cancel(stop)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        total += count as u64;
        if total > 256 * 1024 * 1024 {
            return Err(io::Error::other("安装内容超过大小限制"));
        }
        output.write_all(&buffer[..count])?;
    }
}
#[derive(Deserialize, Serialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub html_url: String,
    pub body: Option<String>,
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
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
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
    fn extracts_only_tools_and_never_overwrites_an_existing_install() {
        let root = tempfile::tempdir().unwrap();
        let bytes = archive(&[
            ("build/bin/ffmpeg.exe", b"fixture-ffmpeg"),
            ("build/bin/ffprobe.exe", b"fixture-ffprobe"),
            ("build/docs/readme.txt", b"not-extracted"),
        ]);
        extract_tools(
            bytes.clone(),
            root.path().to_owned(),
            CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("ffmpeg/ffmpeg.exe")).unwrap(),
            b"fixture-ffmpeg"
        );
        assert_eq!(
            std::fs::read_dir(root.path().join("ffmpeg"))
                .unwrap()
                .count(),
            2
        );
        assert!(extract_tools(bytes, root.path().to_owned(), CancellationToken::new()).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn traversal_duplicate_missing_and_cancelled_packages_leave_no_install() {
        for entries in [
            vec![
                ("../ffmpeg.exe", b"x".as_slice()),
                ("bin/ffprobe.exe", b"y".as_slice()),
            ],
            vec![
                ("a/ffmpeg.exe", b"x".as_slice()),
                ("b/ffmpeg.exe", b"y".as_slice()),
                ("bin/ffprobe.exe", b"z".as_slice()),
            ],
            vec![("a/ffmpeg.exe", b"x".as_slice())],
        ] {
            let root = tempfile::tempdir().unwrap();
            assert!(extract_tools(
                archive(&entries),
                root.path().to_owned(),
                CancellationToken::new()
            )
            .is_err());
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
        let root = tempfile::tempdir().unwrap();
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(extract_tools(
            archive(&[("ffmpeg.exe", b"x"), ("ffprobe.exe", b"x")]),
            root.path().to_owned(),
            stop
        )
        .is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn stopped_tools_reject_new_install_jobs() {
        let root = tempfile::tempdir().unwrap();
        let tools = Tools::new(Workspace::from_repo_root(root.path()));
        tools.shutdown().await;
        assert!(tools.install().is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
