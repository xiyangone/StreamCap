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
    /// 默认录制基准；界面资源与默认配置已编译进 EXE。
    pub resource_dir: PathBuf,
    /// 可写的用户数据目录
    pub user_data_dir: PathBuf,
}

impl Workspace {
    /// 仓库或显式数据目录：默认与相对录像路径均以传入目录为基准。
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

    /// 默认输出随安装目录移动；显式数据目录保持隔离。
    pub fn default_recordings_dir(&self) -> PathBuf {
        self.recordings_base_dir().join("downloads")
    }

    fn recordings_base_dir(&self) -> &Path {
        if self.is_installed_profile() {
            &self.resource_dir
        } else {
            &self.user_data_dir
        }
    }

    /// 相对路径始终基于安装目录（或显式 profile），不依赖进程工作目录。
    pub fn resolve_recordings_path(&self, configured: &str) -> PathBuf {
        if configured.trim().is_empty() {
            return self.default_recordings_dir();
        }
        self.recordings_base_dir().join(configured)
    }

    pub(crate) fn is_installed_profile(&self) -> bool {
        dirs::config_dir().map(|dir| dir.join(APP_NAME)).as_ref() == Some(&self.user_data_dir)
    }

    /// 确保用户数据目录存在，并补齐缺失的默认配置（不覆盖用户已有文件）。
    ///
    /// 仅播种内嵌的录制默认设置；语言与版本由原生界面提供，不写入旧版元数据。
    /// `USER_OWNED_FILES` 里的文件是运行期产物，永不覆盖。
    pub fn ensure_ready(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.config_dir())?;
        self.seed_defaults()?;
        let defaults = crate::config::read_json_object(&self.default_settings_path())?;
        let user = crate::config::read_json_object(&self.user_settings_path())?;
        let custom_directory = user
            .get("live_save_path")
            .or_else(|| defaults.get("live_save_path"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| !path.trim().is_empty());
        if !custom_directory {
            std::fs::create_dir_all(self.default_recordings_dir())?;
        }
        Ok(())
    }

    fn seed_defaults(&self) -> std::io::Result<()> {
        let target = self.default_settings_path();
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(target)
        {
            Ok(mut file) => {
                use std::io::Write;
                let contents = serde_json::to_vec_pretty(&crate::config::native_defaults())?;
                file.write_all(&contents)?;
                file.sync_all()
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// 桌面启动先校验释放内置工具；运行期不搜索当前目录或 PATH。
pub fn find_ffmpeg(workspace: &Workspace) -> Option<PathBuf> {
    configured_ffmpeg(workspace, None)
}

/// 优先使用本版本工具；核心调用者可显式注入验证工具。
pub fn configured_ffmpeg(workspace: &Workspace, configured: Option<&Path>) -> Option<PathBuf> {
    let bundled = crate::tools::bundled_directory(workspace).join("ffmpeg.exe");
    if bundled.is_file() && adjacent_ffprobe(&bundled).is_some() {
        return Some(bundled);
    }
    configured.map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_tools_require_a_pair_and_preserve_explicit_test_injection() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(directory.path());
        assert_eq!(configured_ffmpeg(&workspace, None), None);
        let configured = directory.path().join("startup-ffmpeg.exe");
        assert_eq!(
            configured_ffmpeg(&workspace, Some(&configured)),
            Some(configured.clone())
        );
        let installed = crate::tools::bundled_directory(&workspace).join("ffmpeg.exe");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"fixture only").unwrap();
        assert_eq!(find_ffmpeg(&workspace), None);
        std::fs::write(installed.with_file_name("ffprobe.exe"), b"fixture only").unwrap();
        assert_eq!(
            configured_ffmpeg(&workspace, Some(&configured)),
            Some(installed.clone())
        );
        assert_eq!(configured_ffmpeg(&workspace, None), Some(installed));
    }

    #[test]
    fn relative_recording_paths_follow_the_workspace_and_absolute_paths_stay_explicit() {
        let first = Workspace::for_installed("H:/apps/StreamCap").unwrap();
        let moved = Workspace::for_installed("E:/中文 目录/StreamCap").unwrap();
        assert_eq!(
            first.resolve_recordings_path("captures"),
            first.resource_dir.join("captures")
        );
        assert_eq!(
            moved.resolve_recordings_path("captures"),
            moved.resource_dir.join("captures")
        );
        let selected = std::env::temp_dir().join("explicit-recordings");
        assert_eq!(
            moved.resolve_recordings_path(selected.to_str().unwrap()),
            selected
        );
        let profile = Workspace::from_repo_root(std::env::temp_dir().join("isolated-profile"));
        assert_eq!(
            profile.resolve_recordings_path("相对 目录"),
            profile.user_data_dir.join("相对 目录")
        );
        assert_eq!(
            profile.resolve_recordings_path(""),
            profile.default_recordings_dir()
        );
    }

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
    fn installed_workspace_keeps_default_recordings_beside_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::for_installed(dir.path()).unwrap();
        assert_eq!(ws.default_recordings_dir(), dir.path().join("downloads"));
    }

    #[test]
    fn explicit_profiles_keep_default_recordings_inside_the_profile() {
        let ws = Workspace {
            resource_dir: PathBuf::from("E:/StreamCap"),
            user_data_dir: PathBuf::from("E:/isolated-profile"),
        };
        assert_eq!(
            ws.default_recordings_dir(),
            PathBuf::from("E:/isolated-profile/downloads")
        );
    }

    #[test]
    fn explicitly_selecting_the_normal_profile_still_uses_it_as_the_recording_base() {
        let directory = dirs::config_dir().unwrap().join(APP_NAME);
        let workspace = Workspace::from_repo_root(&directory);
        assert_eq!(
            workspace.default_recordings_dir(),
            directory.join("downloads")
        );
        assert_eq!(
            workspace.resolve_recordings_path("captures"),
            directory.join("captures")
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
        assert!(!ws.config_dir().join("language.json").exists());
        assert!(!ws.config_dir().join("version.json").exists());
        assert!(!ws.config_dir().join("recordings.json").exists());
        assert!(!ws.config_dir().join("user_settings.json").exists());

        // 已存在的文件不被覆盖
        for name in ["language.json", "version.json"] {
            std::fs::write(ws.config_dir().join(name), b"existing user metadata").unwrap();
        }
        std::fs::write(
            ws.config_dir().join("default_settings.json"),
            r#"{"video_format":"MKV"}"#,
        )
        .unwrap();
        ws.ensure_ready().unwrap();
        let kept = std::fs::read_to_string(ws.config_dir().join("default_settings.json")).unwrap();
        assert!(kept.contains("MKV"), "已存在的配置不应被覆盖");
        for name in ["language.json", "version.json"] {
            assert_eq!(
                std::fs::read(ws.config_dir().join(name)).unwrap(),
                b"existing user metadata"
            );
        }
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

/// Post-processing verifies completed outputs with the ffprobe from the same FFmpeg distribution.
pub fn adjacent_ffprobe(ffmpeg: &Path) -> Option<PathBuf> {
    let probe = ffmpeg.with_file_name(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    });
    probe.is_file().then_some(probe)
}

#[cfg(test)]
#[test]
fn media_probe_discovery_never_silently_uses_an_unrelated_binary() {
    let root = tempfile::tempdir().unwrap();
    let ffmpeg = root.path().join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    assert!(adjacent_ffprobe(&ffmpeg).is_none());
    let probe = root.path().join(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    });
    std::fs::write(&probe, b"fixture").unwrap();
    assert_eq!(adjacent_ffprobe(&ffmpeg), Some(probe));
}

#[cfg(test)]
#[test]
fn configured_recording_directory_does_not_require_the_unused_default_directory() {
    for from_defaults in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(directory.path());
        std::fs::create_dir_all(workspace.config_dir()).unwrap();
        // A file at the fallback location makes creating it deterministically fail.
        let unused = workspace.default_recordings_dir();
        std::fs::write(&unused, b"keep unused fallback").unwrap();
        let selected = directory.path().join("selected-recordings");
        std::fs::create_dir(&selected).unwrap();
        let settings_path = if from_defaults {
            workspace.default_settings_path()
        } else {
            workspace.user_settings_path()
        };
        let settings = serde_json::to_vec(&serde_json::json!({"live_save_path":selected})).unwrap();
        std::fs::write(&settings_path, &settings).unwrap();
        workspace.ensure_ready().unwrap();
        let config = crate::config::ConfigStore::load(workspace.clone()).unwrap();
        assert_eq!(config.recordings_root(), selected);
        assert_eq!(std::fs::read(&unused).unwrap(), b"keep unused fallback");
        assert_eq!(std::fs::read(&settings_path).unwrap(), settings);

        // Explicitly clearing the preference restores the fallback semantics.
        std::fs::write(workspace.user_settings_path(), br#"{"live_save_path":""}"#).unwrap();
        assert!(workspace.ensure_ready().is_err());
        assert_eq!(std::fs::read(&unused).unwrap(), b"keep unused fallback");
    }
}
