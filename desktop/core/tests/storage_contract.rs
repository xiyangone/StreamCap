use std::{
    io::{Seek, SeekFrom, Write},
    path::Path,
};
use streamcap_core::{
    api::bootstrap,
    service::{Server, ServerOptions},
    storage::{self, Storage},
    Workspace,
};
use tokio_util::sync::CancellationToken;
async fn server() -> (Server, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    (
        Server::start(
            state,
            ServerOptions {
                port: 0,
                monitoring: false,
            },
        )
        .await
        .unwrap(),
        dir,
    )
}
#[test]
fn rejects_traversal_roots_devices_and_alternate_streams() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("video.mp4"), b"safe").unwrap();
    for value in [
        "../file",
        r"..\file",
        "/file",
        r"C:\file",
        r"\\server\file",
        "video.mp4:secret",
        ".",
        "",
    ] {
        assert!(
            storage::checked_target(dir.path(), value, false).is_err(),
            "{value}"
        );
    }
    assert!(storage::checked_target(dir.path(), "video.mp4", false).is_ok());
}
#[tokio::test]
async fn ranges_are_bounded_and_head_does_not_read_the_body() {
    let (server, dir) = server().await;
    let file = dir.path().join("downloads/large.mp4");
    let mut f = std::fs::File::create(&file).unwrap();
    let size = 512 * 1024 * 1024u64;
    f.set_len(size).unwrap();
    f.seek(SeekFrom::End(-4)).unwrap();
    f.write_all(b"TAIL").unwrap();
    drop(f);
    let url = format!("http://{}/api/videos?path=large.mp4", server.address());
    let client = reqwest::Client::new();
    let result = client
        .get(&url)
        .header("Range", "bytes=-4")
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 206);
    assert_eq!(result.content_length(), Some(4));
    assert_eq!(result.bytes().await.unwrap().as_ref(), b"TAIL");
    let result = client
        .head(&url)
        .header("Range", "bytes=2-5")
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 200);
    assert_eq!(result.headers()["content-length"], size.to_string());
    assert!(result.bytes().await.unwrap().is_empty());
    for range in ["bytes=536870912-", "bytes=-0", "bytes=5-2", "bytes=bad"] {
        let result = client
            .get(&url)
            .header("Range", range)
            .send()
            .await
            .unwrap();
        assert_eq!(result.status(), 416, "{range}");
        assert_eq!(result.headers()["content-range"], format!("bytes */{size}"));
    }
    std::fs::write(dir.path().join("downloads/empty.wav"), b"").unwrap();
    let empty = format!("http://{}/api/videos?path=empty.wav", server.address());
    let result = client.get(&empty).send().await.unwrap();
    assert_eq!(result.status(), 200);
    assert_eq!(result.headers()["content-type"], "audio/wav");
    assert!(result.bytes().await.unwrap().is_empty());
    assert_eq!(
        client
            .get(&empty)
            .header("Range", "bytes=0-")
            .send()
            .await
            .unwrap()
            .status(),
        416
    );
    assert_eq!(
        client
            .get(format!(
                "http://{}/api/storage?subfolder=..",
                server.address()
            ))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    server.shutdown().await.unwrap();
}
#[tokio::test]
async fn scans_are_cancellable_and_shutdown_closes_registration() {
    let dir = tempfile::tempdir().unwrap();
    let stop = CancellationToken::new();
    let storage = Storage::new(stop.clone());
    stop.cancel();
    assert_eq!(
        storage
            .size(dir.path().to_owned())
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    storage.shutdown().await;
    assert!(storage.blocking(|_| Ok(())).await.is_err());
}
#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    use std::os::windows::process::CommandExt;
    let result=std::process::Command::new("pwsh").args(["-NoProfile","-NonInteractive","-Command","$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:SC_TEST_LINK -Target $env:SC_TEST_TARGET | Out-Null"])
        .env("SC_TEST_LINK",link).env("SC_TEST_TARGET",target).creation_flags(0x08000000).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
#[cfg(windows)]
#[test]
fn junctions_cannot_escape_or_cycle() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("private.mp4"), b"outside").unwrap();
    junction(&root.path().join("escape"), outside.path());
    junction(&root.path().join("cycle"), root.path());
    assert!(storage::checked_target(root.path(), "escape/private.mp4", false).is_err());
    let listing = storage::list(root.path(), "", &CancellationToken::new()).unwrap();
    assert!(listing.items.is_empty());
    assert!(storage::recycle(root.path(), "escape", &CancellationToken::new()).is_err());
    assert!(outside.path().join("private.mp4").exists());
}
#[cfg(windows)]
#[test]
fn recycle_success_and_locked_failure_never_use_permanent_delete() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("recycle-proof.txt");
    std::fs::write(&file, b"recycle proof").unwrap();
    let result =
        storage::recycle(dir.path(), "recycle-proof.txt", &CancellationToken::new()).unwrap();
    assert!(result.recycled);
    assert!(!file.exists());
    if let Some(destination) = result.recycled_to {
        assert_eq!(std::fs::read(destination).unwrap(), b"recycle proof");
    }
    let file = dir.path().join("locked.txt");
    std::fs::write(&file, b"keep").unwrap();
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&file)
        .unwrap();
    assert!(storage::recycle(dir.path(), "locked.txt", &CancellationToken::new()).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    drop(handle);
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(storage::recycle(dir.path(), "locked.txt", &stop).is_err());
    assert!(file.exists());
}

#[cfg(windows)]
#[test]
fn recycles_nonempty_unicode_directory_as_a_directory() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("验收目录");
    std::fs::create_dir_all(folder.join("child")).unwrap();
    std::fs::write(folder.join("child/proof.txt"), b"directory proof").unwrap();
    let receipt = storage::recycle(root.path(), "验收目录", &CancellationToken::new()).unwrap();
    assert!(receipt.recycled);
    assert!(!folder.exists());
    let recycled = receipt
        .recycled_to
        .expect("Windows should return the recycled shell item");
    assert_eq!(
        std::fs::read(Path::new(&recycled).join("child/proof.txt")).unwrap(),
        b"directory proof"
    );
}

#[tokio::test]
async fn recording_preview_uses_the_same_root_boundary_and_relative_paths() {
    let (server, dir) = server().await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("downloads/root.mp4"), b"safe").unwrap();
    std::fs::write(outside.path().join("private.mp4"), b"outside").unwrap();
    let mut rec = streamcap_core::Recording::new(
        "preview".into(),
        "http://127.0.0.1:1/media.mp4".into(),
        "Fixture".into(),
    );
    rec.recording_dir = Some(dir.path().join("downloads").to_string_lossy().into_owned());
    server.state().store.insert(vec![rec]).await.unwrap();
    let url = format!("http://{}/api/recordings/preview/files", server.address());
    let result = reqwest::get(&url).await.unwrap();
    assert_eq!(result.status(), 200);
    let value: serde_json::Value = result.json().await.unwrap();
    assert_eq!(value["files"][0]["path"], "root.mp4");
    server
        .state()
        .store
        .update("preview", |r| {
            r.recording_dir = Some(outside.path().to_string_lossy().into_owned())
        })
        .await;
    assert_eq!(reqwest::get(&url).await.unwrap().status(), 400);
    assert_eq!(
        std::fs::read(outside.path().join("private.mp4")).unwrap(),
        b"outside"
    );
    server.shutdown().await.unwrap();
}

#[test]
fn conversion_output_publish_never_overwrites_and_temporary_files_are_not_listed() {
    let dir = tempfile::tempdir().unwrap();
    let temporary = dir.path().join(".streamcap-remux-fixture.mp4");
    let output = dir.path().join("kept.mp4");
    std::fs::write(&temporary, b"new media").unwrap();
    std::fs::write(&output, b"original media").unwrap();
    assert!(storage::publish_new_file(&temporary, &output).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"original media");
    assert_eq!(std::fs::read(&temporary).unwrap(), b"new media");
    let listing = storage::list(dir.path(), "", &CancellationToken::new()).unwrap();
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.items[0].name, "kept.mp4");
    let new_output = dir.path().join("new.mp4");
    storage::publish_new_file(&temporary, &new_output).unwrap();
    assert_eq!(std::fs::read(&new_output).unwrap(), b"new media");
    assert!(!temporary.exists());
}
