//! Own explicitly launched helper trees. Closing the Windows job terminates descendants.
use std::io;
#[cfg(windows)]
pub struct ChildJob {
    _handle: std::os::windows::io::OwnedHandle,
}
#[cfg(not(windows))]
pub struct ChildJob;
impl ChildJob {
    pub fn attach(child: &tokio::process::Child) -> io::Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::io::{AsRawHandle, FromRawHandle};
            use windows_sys::Win32::System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            };
            // The handle is uniquely owned and closed even when assignment fails.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            let handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw.cast()) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = unsafe {
                SetInformationJobObject(
                    raw,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("helper already exited"))?;
            if unsafe { AssignProcessToJobObject(handle.as_raw_handle().cast(), process.cast()) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { _handle: handle })
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(Self)
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    #[tokio::test]
    #[ignore = "requires explicit STREAMCAP_TEST_PWSH; executes only a synthetic sleeping fixture"]
    async fn owned_script_tree_exits_with_its_job() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
        let executable = std::env::var("STREAMCAP_TEST_PWSH").expect("explicit test shell");
        let script = r#"$s=[Diagnostics.ProcessStartInfo]::new();$s.FileName=(Join-Path $PSHOME 'pwsh.exe');$s.UseShellExecute=$false;$s.CreateNoWindow=$true;$s.WindowStyle='Hidden';foreach($a in @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60')){$s.ArgumentList.Add($a)};$p=[Diagnostics.Process]::Start($s);[Console]::WriteLine($p.Id);Start-Sleep -Seconds 60"#;
        let mut command = tokio::process::Command::new(executable);
        command
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .creation_flags(0x08000000);
        let mut child = command.spawn().unwrap();
        let job = ChildJob::attach(&child).unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let pid = line.trim().parse::<u32>().unwrap();
        let raw = unsafe { OpenProcess(0x00100000, 0, pid) };
        assert!(!raw.is_null());
        let grandchild = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw.cast()) };
        drop(job);
        tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(grandchild.as_raw_handle().cast(), 5000) },
            0
        );
    }
}
