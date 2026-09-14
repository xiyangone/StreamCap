//! Bounded, cancellable filesystem operations. Never follows reparse points or permanently deletes.
use serde::Serialize;
use std::{
    io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
#[derive(Clone)]
pub struct Storage {
    stop: CancellationToken,
    tasks: TaskTracker,
    registration: Arc<Mutex<()>>,
}
impl Storage {
    pub fn new(stop: CancellationToken) -> Self {
        Self {
            stop,
            tasks: TaskTracker::new(),
            registration: Arc::default(),
        }
    }
    pub async fn blocking<T, F>(&self, operation: F) -> io::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(CancellationToken) -> io::Result<T> + Send + 'static,
    {
        let task = {
            let _guard = self
                .registration
                .lock()
                .map_err(|_| io::Error::other("文件任务锁不可用"))?;
            check_cancel(&self.stop)?;
            if self.tasks.is_closed() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "文件服务已关闭"));
            }
            let stop = self.stop.clone();
            self.tasks.spawn_blocking(move || operation(stop))
        };
        task.await.map_err(io::Error::other)?
    }
    pub async fn shutdown(&self) {
        {
            let _guard = self.registration.lock().unwrap_or_else(|e| e.into_inner());
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
    pub async fn size(&self, path: PathBuf) -> io::Result<u64> {
        self.blocking(move |stop| directory_size(&path, &stop))
            .await
    }
}
pub fn check_cancel(stop: &CancellationToken) -> io::Result<()> {
    if stop.is_cancelled() {
        Err(io::Error::new(io::ErrorKind::Interrupted, "文件操作已取消"))
    } else {
        Ok(())
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
pub fn relative_path(relative: &str) -> io::Result<PathBuf> {
    if relative.contains(['\0', ':']) {
        return Err(invalid("无效路径"));
    }
    let relative = relative.replace('\\', "/");
    let path = Path::new(&relative);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid("路径必须位于录制目录内"));
    }
    Ok(path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect())
}
pub fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
pub fn checked_target(root: &Path, relative: &str, allow_root: bool) -> io::Result<PathBuf> {
    let relative = relative_path(relative)?;
    if relative.as_os_str().is_empty() && !allow_root {
        return Err(invalid("不能操作录制根目录"));
    }
    let root = root.canonicalize()?;
    let mut target = root.clone();
    for part in relative.components() {
        target.push(part);
        let metadata = std::fs::symlink_metadata(&target)?;
        if is_link(&metadata) {
            return Err(invalid("不能操作符号链接或目录联接"));
        }
    }
    let target = target.canonicalize()?;
    if !target.starts_with(&root) || (!allow_root && target == root) {
        return Err(invalid("路径超出录制目录"));
    }
    Ok(target)
}
pub fn directory_size(path: &Path, stop: &CancellationToken) -> io::Result<u64> {
    let mut pending = vec![path.to_path_buf()];
    let mut total = 0u64;
    while let Some(path) = pending.pop() {
        check_cancel(stop)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        if is_link(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                check_cancel(stop)?;
                pending.push(entry?.path());
            }
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

pub fn open_file(root: &Path, relative: &str) -> io::Result<std::fs::File> {
    open_media_file(root, relative, false)
}

/// Hold a read-only lease during conversion; on Windows writers and deleters cannot race it.
pub fn open_conversion_source(root: &Path, relative: &str) -> io::Result<std::fs::File> {
    open_media_file(root, relative, true)
}

fn open_media_file(root: &Path, relative: &str, locked: bool) -> io::Result<std::fs::File> {
    let target = checked_target(root, relative, false)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
        if locked {
            options.share_mode(1);
        }
    }
    #[cfg(not(windows))]
    let _ = locked;
    let file = options.open(&target)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(invalid("目标不是普通媒体文件"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
        let mut buffer = vec![0u16; 512];
        loop {
            let count = unsafe {
                windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW(
                    file.as_raw_handle() as _,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    0,
                )
            };
            if count == 0 {
                return Err(io::Error::last_os_error());
            }
            if count as usize >= buffer.len() {
                buffer.resize(count as usize + 1, 0);
                continue;
            }
            let actual = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..count as usize]));
            if !actual.starts_with(root.canonicalize()?) {
                return Err(invalid("文件实际位置超出录制目录"));
            }
            break;
        }
    }
    Ok(file)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub path: String,
    pub modified: Option<f64>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub root: String,
    pub items: Vec<Item>,
    pub total_size: u64,
}
pub fn list(root: &Path, relative: &str, stop: &CancellationToken) -> io::Result<Listing> {
    relative_path(relative)?;
    let empty = || Listing {
        root: root.to_string_lossy().into_owned(),
        items: vec![],
        total_size: 0,
    };
    let target = match checked_target(root, relative, true) {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(empty()),
        Err(e) => return Err(e),
    };
    let canonical_root = root.canonicalize()?;
    let mut result = empty();
    for entry in std::fs::read_dir(target)? {
        check_cancel(stop)?;
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".streamcap-remux-")
            || entry
                .file_name()
                .to_string_lossy()
                .starts_with(".streamcap-subtitle-")
        {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error)
                if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(303) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        if is_link(&metadata) {
            continue;
        }
        if !metadata.is_dir() && !metadata.is_file() {
            continue;
        }
        let size = if metadata.is_dir() {
            directory_size(&entry.path(), stop)?
        } else {
            metadata.len()
        };
        result.total_size = result.total_size.saturating_add(size);
        result.items.push(Item {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: metadata.is_dir(),
            size,
            path: entry
                .path()
                .strip_prefix(&canonical_root)
                .map_err(|_| invalid("路径超出录制目录"))?
                .to_string_lossy()
                .replace('\\', "/"),
            modified: metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64()),
        });
    }
    result
        .items
        .sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(result)
}
pub fn validate_recycle_tree(target: &Path, stop: &CancellationToken) -> io::Result<()> {
    let mut pending = vec![target.to_path_buf()];
    while let Some(path) = pending.pop() {
        check_cancel(stop)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        if is_link(&metadata) {
            return Err(invalid("目录含有链接，未执行回收"));
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(())
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecycleReceipt {
    pub recycled: bool,
    pub recycled_to: Option<String>,
}
pub fn recycle(
    root: &Path,
    relative: &str,
    stop: &CancellationToken,
) -> io::Result<RecycleReceipt> {
    check_cancel(stop)?;
    let target = checked_target(root, relative, false)?;
    validate_recycle_tree(&target, stop)?;
    #[cfg(windows)]
    {
        windows_recycle::recycle(target, stop.clone())
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "此系统尚未提供安全回收，原文件未修改",
        ))
    }
}

#[cfg(windows)]
mod windows_recycle {
    use super::*;
    use windows::{
        core::{implement, Error, Result as WinResult, PCWSTR},
        Win32::{
            Foundation::E_ABORT,
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
                COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
            },
            UI::Shell::*,
        },
    };
    #[derive(Default)]
    struct Outcome {
        completed: bool,
        destination: Option<String>,
    }
    #[implement(IFileOperationProgressSink)]
    struct Sink {
        stop: CancellationToken,
        outcome: Arc<Mutex<Outcome>>,
    }
    #[allow(non_snake_case)]
    impl IFileOperationProgressSink_Impl for Sink_Impl {
        fn StartOperations(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn FinishOperations(&self, hrresult: windows::core::HRESULT) -> windows::core::Result<()> {
            hrresult.ok()
        }
        fn PreRenameItem(
            &self,
            _dwflags: u32,
            _psiitem: windows::core::Ref<'_, IShellItem>,
            _psznewname: &windows::core::PCWSTR,
        ) -> windows::core::Result<()> {
            Err(Error::from_hresult(E_ABORT))
        }
        fn PostRenameItem(
            &self,
            _dwflags: u32,
            _psiitem: windows::core::Ref<'_, IShellItem>,
            _psznewname: &windows::core::PCWSTR,
            _hrrename: windows::core::HRESULT,
            _psinewlycreated: windows::core::Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Err(Error::from_hresult(E_ABORT))
        }
        fn PreMoveItem(
            &self,
            _dwflags: u32,
            _psiitem: windows::core::Ref<'_, IShellItem>,
            _psidestinationfolder: windows::core::Ref<'_, IShellItem>,
            _psznewname: &windows::core::PCWSTR,
        ) -> windows::core::Result<()> {
            Err(Error::from_hresult(E_ABORT))
        }
        fn PostMoveItem(
            &self,
            _dwflags: u32,
            _psiitem: windows::core::Ref<'_, IShellItem>,
            _psidestinationfolder: windows::core::Ref<'_, IShellItem>,
            _psznewname: &windows::core::PCWSTR,
            _hrmove: windows::core::HRESULT,
            _psinewlycreated: windows::core::Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Err(Error::from_hresult(E_ABORT))
        }
        fn PreCopyItem(
            &self,
            _dwflags: u32,
            _psiitem: windows::core::Ref<'_, IShellItem>,
            _psidestinationfolder: windows::core::Ref<'_, IShellItem>,
            _psznewname: &windows::core::PCWSTR,
        ) -> windows::core::Result<()> {
            Err(Error::from_hresult(E_ABORT))
        }
        fn PostCopyItem(
            &self,
            _dwflags: u32,
            _psiitem: windows::core::Ref<'_, IShellItem>,
            _psidestinationfolder: windows::core::Ref<'_, IShellItem>,
            _psznewname: &windows::core::PCWSTR,
            _hrcopy: windows::core::HRESULT,
            _psinewlycreated: windows::core::Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Err(Error::from_hresult(E_ABORT))
        }
        fn PreDeleteItem(
            &self,
            dwflags: u32,
            _psiitem: windows::core::Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            if self.stop.is_cancelled() || dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                return Err(Error::from_hresult(E_ABORT));
            }
            Ok(())
        }
        fn PostDeleteItem(
            &self,
            _dwflags: u32,
            _psiitem: windows::core::Ref<'_, IShellItem>,
            hrdelete: windows::core::HRESULT,
            psinewlycreated: windows::core::Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            hrdelete.ok()?;
            let mut result = self.outcome.lock().unwrap_or_else(|e| e.into_inner());
            result.completed = true;
            if let Some(item) = psinewlycreated.as_ref() {
                result.destination = unsafe { display_path(item) }.ok();
            }
            Ok(())
        }
        fn PreNewItem(
            &self,
            _dwflags: u32,
            _psidestinationfolder: windows::core::Ref<'_, IShellItem>,
            _psznewname: &windows::core::PCWSTR,
        ) -> windows::core::Result<()> {
            Err(Error::from_hresult(E_ABORT))
        }
        fn PostNewItem(
            &self,
            _dwflags: u32,
            _psidestinationfolder: windows::core::Ref<'_, IShellItem>,
            _psznewname: &windows::core::PCWSTR,
            _psztemplatename: &windows::core::PCWSTR,
            _dwfileattributes: u32,
            _hrnew: windows::core::HRESULT,
            _psinewitem: windows::core::Ref<'_, IShellItem>,
        ) -> windows::core::Result<()> {
            Err(Error::from_hresult(E_ABORT))
        }
        fn UpdateProgress(&self, _iworktotal: u32, _iworksofar: u32) -> windows::core::Result<()> {
            Ok(())
        }
        fn ResetTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn PauseTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn ResumeTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
    }
    unsafe fn display_path(item: &IShellItem) -> WinResult<String> {
        let text = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }?;
        let result = unsafe { text.to_string() };
        unsafe { CoTaskMemFree(Some(text.0.cast())) };
        Ok(result?)
    }
    pub(super) fn recycle(target: PathBuf, stop: CancellationToken) -> io::Result<RecycleReceipt> {
        std::thread::Builder::new()
            .name("streamcap-recycle".into())
            .spawn(move || perform(target, stop))?
            .join()
            .map_err(|_| io::Error::other("回收线程异常"))?
    }
    fn perform(target: PathBuf, stop: CancellationToken) -> io::Result<RecycleReceipt> {
        check_cancel(&stop)?;
        let operation = || -> WinResult<RecycleReceipt> {
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
                struct ComGuard;
                impl Drop for ComGuard {
                    fn drop(&mut self) {
                        unsafe { CoUninitialize() }
                    }
                }
                let _com = ComGuard;
                let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
                op.SetOperationFlags(
                    FOFX_RECYCLEONDELETE
                        | FOFX_EARLYFAILURE
                        | FOFX_ADDUNDORECORD
                        | FOF_NOCONFIRMATION
                        | FOF_NOERRORUI
                        | FOF_SILENT,
                )?;
                let original = target
                    .to_str()
                    .ok_or_else(|| Error::from_hresult(E_ABORT))?;
                let normal = if let Some(p) = original.strip_prefix(r"\\?\UNC\") {
                    format!(r"\\{p}")
                } else {
                    original
                        .strip_prefix(r"\\?\")
                        .unwrap_or(original)
                        .to_string()
                };
                let wide: Vec<u16> = normal.encode_utf16().chain(Some(0)).collect();
                let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)?;
                let outcome = Arc::new(Mutex::new(Outcome::default()));
                let sink: IFileOperationProgressSink = Sink {
                    stop: stop.clone(),
                    outcome: outcome.clone(),
                }
                .into();
                op.DeleteItem(&item, &sink)?;
                op.PerformOperations()?;
                if op.GetAnyOperationsAborted()?.as_bool() {
                    return Err(Error::from_hresult(E_ABORT));
                }
                let outcome = outcome.lock().unwrap_or_else(|e| e.into_inner());
                if !outcome.completed || target.exists() {
                    return Err(Error::from_hresult(E_ABORT));
                }
                Ok(RecycleReceipt {
                    recycled: true,
                    recycled_to: outcome.destination.clone(),
                })
            }
        };
        operation().map_err(|e| io::Error::other(format!("无法移入回收站，未启用永久删除: {e}")))
    }
}

/// Publish a completed temporary output without ever replacing an existing user file.
pub fn publish_new_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    if temporary.parent() != destination.parent() {
        return Err(invalid("输出必须在源文件目录"));
    }
    let meta = std::fs::symlink_metadata(temporary)?;
    if !meta.is_file() || is_link(&meta) {
        return Err(invalid("临时输出不是普通文件"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // MOVEFILE_WRITE_THROUGH, deliberately without MOVEFILE_REPLACE_EXISTING.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 8)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::hard_link(temporary, destination)?;
        std::fs::remove_file(temporary)?;
    }
    Ok(())
}

/// Permanently delete only the exact file verified while leased read-only.
/// Caller holds the engine filesystem guard and has published a validated MP4.
pub fn remove_verified_source(root: &Path, relative: &str, lease: std::fs::File) -> io::Result<()> {
    let expected = source_identity(&lease)?;
    drop(lease);
    let target = checked_target(root, relative, false)?;
    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
        };
        // Read + DELETE, only share reads; no external writer/deleter can race this check.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .access_mode(0x80010000)
            .share_mode(1)
            .custom_flags(0x00200000)
            .open(&target)?;
        if is_link(&file.metadata()?) || source_identity(&file)? != expected {
            return Err(invalid("源 TS 已发生变化，未删除"));
        }
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        let ok = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as _,
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        drop(file);
    }
    #[cfg(not(windows))]
    {
        let file = open_conversion_source(root, relative)?;
        if source_identity(&file)? != expected {
            return Err(invalid("源 TS 已发生变化，未删除"));
        }
        std::fs::remove_file(target)?;
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceIdentity {
    pub volume: u64,
    pub index: u64,
    pub size: u64,
    pub modified: u64,
}
pub fn source_identity(file: &std::fs::File) -> io::Result<SourceIdentity> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SourceIdentity {
            volume: info.dwVolumeSerialNumber as u64,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            size: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            modified: ((info.ftLastWriteTime.dwHighDateTime as u64) << 32)
                | info.ftLastWriteTime.dwLowDateTime as u64,
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let info = file.metadata()?;
        Ok(SourceIdentity {
            volume: info.dev(),
            index: info.ino(),
            size: info.len(),
            modified: info.mtime_nsec() as u64 ^ info.mtime() as u64,
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台不支持安全源文件清理",
        ))
    }
}
