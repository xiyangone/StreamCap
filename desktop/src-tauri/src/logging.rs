//! Size-bounded application logs. Rotation never follows symlinks or overwrites unrelated names.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
pub struct RollingLog {
    root: PathBuf,
    file: Option<File>,
    bytes: u64,
    limit: u64,
    keep: usize,
}
impl RollingLog {
    pub fn open(root: &Path) -> io::Result<Self> {
        Self::with_limits(root, 4 * 1024 * 1024, 5)
    }
    fn with_limits(root: &Path, limit: u64, keep: usize) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let path = root.join("native.log");
        Self::regular(&path)?;
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            root: root.to_owned(),
            file: Some(file),
            bytes,
            limit,
            keep,
        })
    }
    fn regular(path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_file() || streamcap_core::storage::is_link(&metadata) => {
                Err(io::Error::other("日志目标不是普通文件"))
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn rotate(&mut self) -> io::Result<()> {
        for index in 0..self.keep {
            Self::regular(&self.path(index))?;
        }
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }
        let last = self.path(self.keep - 1);
        if last.exists() {
            fs::remove_file(last)?;
        }
        for index in (1..self.keep).rev() {
            let source = self.path(index - 1);
            if source.exists() {
                fs::rename(source, self.path(index))?;
            }
        }
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path(0))?,
        );
        self.bytes = 0;
        Ok(())
    }
    fn path(&self, index: usize) -> PathBuf {
        self.root.join(if index == 0 {
            "native.log".into()
        } else {
            format!("native.log.{index}")
        })
    }
}
impl Write for RollingLog {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(input);
        let lower = text.to_ascii_lowercase();
        let sensitive = [
            "authorization:",
            "cookie:",
            "access_token=",
            "refresh_token=",
            "password=",
            "token=",
        ]
        .iter()
        .any(|marker| lower.contains(marker));
        let data = if sensitive {
            b"[redacted sensitive diagnostic]\n".as_slice()
        } else {
            &input[..input.len().min(65536)]
        };
        if self.bytes > 0 && self.bytes.saturating_add(data.len() as u64) > self.limit {
            self.rotate()?;
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("日志文件不可用"))?
            .write_all(data)?;
        self.bytes = self.bytes.saturating_add(data.len() as u64);
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("日志文件不可用"))?
            .flush()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotation_is_bounded_preserves_existing_and_redacts_credentials() {
        let root = std::env::temp_dir().join(format!(
            "streamcap-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("keep.txt"), "keep").unwrap();
        let mut logger = RollingLog::with_limits(&root, 64, 3).unwrap();
        for _ in 0..20 {
            logger
                .write_all(b"a bounded ordinary diagnostic entry\n")
                .unwrap();
        }
        logger.write_all(b"Cookie: secret-value\n").unwrap();
        logger.flush().unwrap();
        drop(logger);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 4);
        for entry in fs::read_dir(&root).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains("secret-value"));
        }
        assert_eq!(fs::read_to_string(root.join("keep.txt")).unwrap(), "keep");
        fs::remove_dir_all(root).unwrap();
    }
}
