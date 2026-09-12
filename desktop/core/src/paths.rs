//! 原生版沿用既有用户数据目录；默认配置内嵌，不依赖 Python 或旁置资源包。

use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "StreamCap";

/// 用户数据文件：由程序运行期写入，任何情况下不允许被包内同名文件覆盖。
pub const USER_OWNED_FILES: &[&str] = &[
    "recordings.json",
    "cookies.json",
    "accounts.json",
    "user_settings.json",
    "web_auth.json",
];

#[derive(Debug, Clone)]
pub struct Workspace {
    /// 程序资源根；默认配置已编译进 EXE。
    pub resource_dir: PathBuf,
    /// 可写的用户数据目录
    pub user_data_dir: PathBuf,
}

impl Workspace {
    /// 开发态：以当前工作目录（仓库根）为资源与数据目录，与 Python 非冻结模式一致。
    pub fn from_repo_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            resource_dir: root.clone(),
            user_data_dir: root,
        }
    }

    /// 打包态：资源在 exe 同目录，用户数据在 %APPDATA%/StreamCap。
    pub fn for_installed(exe_dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let resource_dir = exe_dir.as_ref().to_path_buf();
        let user_data_dir = dirs::config_dir()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "无法定位系统用户配置目录")
            })?
            .join(APP_NAME);
        Ok(Self {
            resource_dir,
            user_data_dir,
        })
    }

    pub fn config_dir(&self) -> PathBuf {
        self.user_data_dir.join("config")
    }

    pub fn user_settings_path(&self) -> PathBuf {
        self.config_dir().join("user_settings.json")
    }

    pub fn default_settings_path(&self) -> PathBuf {
        self.config_dir().join("default_settings.json")
    }

    pub fn recordings_path(&self) -> PathBuf {
        self.config_dir().join("recordings.json")
    }

    pub fn cookies_path(&self) -> PathBuf {
        self.config_dir().join("cookies.json")
    }

    /// 默认录制输出目录，与 Python 的 default_recordings_dir 一致。
    pub fn default_recordings_dir(&self) -> PathBuf {
        self.user_data_dir.join("downloads")
    }

    /// 确保用户数据目录存在，并补齐缺失的默认配置（不覆盖用户已有文件）。
    ///
    /// 打包态下 `default_settings.json` / `language.json` 等随安装包分发在资源目录，
    /// 首次运行时需要播种到 `%APPDATA%/StreamCap/config`，否则设置页在所有默认值上都是空的。
    /// `USER_OWNED_FILES` 里的文件是运行期产物，永不覆盖。
    pub fn ensure_ready(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.config_dir())?;
        std::fs::create_dir_all(self.default_recordings_dir())?;
        self.seed_defaults()
    }

    fn seed_defaults(&self) -> std::io::Result<()> {
        // Only create missing shipped defaults, never seed tasks or login state.
        let values = crate::config::native_defaults();
        for (name, contents) in [
            (
                "default_settings.json",
                serde_json::to_string_pretty(&values)?,
            ),
            (
                "language.json",
                include_str!("../../../config/language.json").to_string(),
            ),
            (
                "version.json",
                include_str!("../../../config/version.json").to_string(),
            ),
        ] {
            let target = self.config_dir().join(name);
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(target)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(contents.as_bytes())?;
                    file.sync_all()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// 定位 ffmpeg：优先用户数据目录内自带的，其次 PATH。
pub fn find_ffmpeg(workspace: &Workspace) -> Option<PathBuf> {
    let bundled = workspace.user_data_dir.join("ffmpeg").join("ffmpeg.exe");
    if bundled.is_file() {
        return Some(bundled);
    }
    which_in_path("ffmpeg.exe").or_else(|| which_in_path("ffmpeg"))
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_workspace_uses_root_for_both_dirs() {
        let ws = Workspace::from_repo_root("Z:/demo/streamcap");
        assert_eq!(ws.config_dir(), PathBuf::from("Z:/demo/streamcap/config"));
        assert_eq!(
            ws.default_recordings_dir(),
            PathBuf::from("Z:/demo/streamcap/downloads")
        );
    }

    #[test]
    fn user_owned_files_match_python_side() {
        assert!(USER_OWNED_FILES.contains(&"recordings.json"));
        assert!(USER_OWNED_FILES.contains(&"user_settings.json"));
        assert_eq!(USER_OWNED_FILES.len(), 5);
    }

    #[test]
    fn seeding_copies_defaults_but_never_user_owned_files() {
        let dir = tempfile::tempdir().unwrap();
        let resource = dir.path().join("resource");
        let data = dir.path().join("data");
        std::fs::create_dir_all(resource.join("config")).unwrap();

        std::fs::write(
            resource.join("config/default_settings.json"),
            r#"{"video_format":"TS"}"#,
        )
        .unwrap();
        std::fs::write(
            resource.join("config/language.json"),
            r#"{"Chinese":"zh_CN"}"#,
        )
        .unwrap();
        // 用户数据文件不应被播种
        std::fs::write(
            resource.join("config/recordings.json"),
            r#"{"recordings":[]}"#,
        )
        .unwrap();
        std::fs::write(
            resource.join("config/user_settings.json"),
            r#"{"video_format":"MP4"}"#,
        )
        .unwrap();

        let ws = Workspace {
            resource_dir: resource.clone(),
            user_data_dir: data.clone(),
        };
        ws.ensure_ready().unwrap();

        assert!(ws.config_dir().join("default_settings.json").is_file());
        assert!(ws.config_dir().join("language.json").is_file());
        assert!(!ws.config_dir().join("recordings.json").exists());
        assert!(!ws.config_dir().join("user_settings.json").exists());

        // 已存在的文件不被覆盖
        std::fs::write(
            ws.config_dir().join("default_settings.json"),
            r#"{"video_format":"MKV"}"#,
        )
        .unwrap();
        ws.ensure_ready().unwrap();
        let kept = std::fs::read_to_string(ws.config_dir().join("default_settings.json")).unwrap();
        assert!(kept.contains("MKV"), "已存在的配置不应被覆盖");
    }

    #[test]
    fn dev_workspace_skips_seeding() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config/default_settings.json"),
            r#"{"a":1}"#,
        )
        .unwrap();
        // 资源目录与数据目录相同时不应有任何拷贝动作
        ws.ensure_ready().unwrap();
        assert_eq!(ws.resource_dir, ws.user_data_dir);
    }
}
