#[cfg(not(test))]
use std::process::Command;
use std::{fs, path::Path};

pub const BUILD_INPUTS: &[&str] = &[
    "core/src",
    "core/build.rs",
    "core/Cargo.toml",
    "core/Cargo.lock",
    "src",
    "assets",
    "styles.css",
    "index.html",
    "Trunk.toml",
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "package-lock.json",
    ".cargo",
    "core/.cargo",
    "src-tauri/.cargo",
    "scripts/build-frontend.ps1",
    "scripts/build-release.ps1",
    "src-tauri/src",
    "src-tauri/build.rs",
    "src-tauri/Cargo.toml",
    "src-tauri/Cargo.lock",
    "src-tauri/tauri.conf.json",
    "src-tauri/capabilities",
    "src-tauri/permissions",
    "src-tauri/icons",
    "../config/default_settings.json",
    "../config/language.json",
    "../config/version.json",
];

fn hash_bytes(bytes: impl IntoIterator<Item = u8>, hash: &mut u64) {
    for byte in bytes {
        *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
}
fn fingerprint(path: &Path, root: &Path, hash: &mut u64) {
    println!("cargo:rerun-if-changed={}", path.display());
    if path.is_dir() {
        let mut entries = fs::read_dir(path)
            .expect("build inputs")
            .map(|entry| entry.expect("build entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            fingerprint(&entry, root, hash);
        }
    } else if path.is_file() {
        let name = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let contents = fs::read(path).expect("build source");
        hash_bytes((name.len() as u64).to_le_bytes(), hash);
        hash_bytes(name.bytes(), hash);
        hash_bytes((contents.len() as u64).to_le_bytes(), hash);
        hash_bytes(contents, hash);
    }
}

pub fn source_fingerprint(root: &Path) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    for input in BUILD_INPUTS {
        fingerprint(&root.join(input), root, &mut hash);
    }
    hash
}

#[cfg(not(test))]
fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("desktop root");
    let mut hash = source_fingerprint(root);
    for key in [
        "TARGET",
        "PROFILE",
        "OPT_LEVEL",
        "DEBUG",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "TAURI_CONFIG",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        hash_bytes(key.bytes().chain(std::iter::once(0)), &mut hash);
        hash_bytes(
            std::env::var(key)
                .unwrap_or_default()
                .bytes()
                .chain(std::iter::once(0)),
            &mut hash,
        );
    }
    for input in ["../.git/HEAD", "../.git/refs/heads", "../.git/packed-refs"] {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }
    let revision = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|revision| {
            revision.len() == 12 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .unwrap_or_else(|| "source".into());
    println!("cargo:rustc-env=STREAMCAP_BUILD_ID={revision}-{hash:016x}");
}
