//! Exercise the exact build-script fingerprint, not a duplicated test implementation.
#[path = "../build.rs"]
mod build_script;

#[test]
fn styles_locks_and_all_declared_release_inputs_change_the_fingerprint() {
    let directory = tempfile::tempdir().unwrap();
    let desktop = directory.path().join("desktop");
    std::fs::create_dir(&desktop).unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    for required in [
        "styles.css",
        "Cargo.lock",
        "core/Cargo.lock",
        "src-tauri/Cargo.lock",
        "package-lock.json",
        "index.html",
        "Trunk.toml",
        "src-tauri/build.rs",
        "scripts/build-release.ps1",
    ] {
        assert!(
            build_script::BUILD_INPUTS.contains(&required),
            "missing release input {required}"
        );
    }
    for input in build_script::BUILD_INPUTS {
        let mut target = desktop.join(input);
        if source.join(input).is_dir() || input.ends_with(".cargo") {
            target = target.join("fingerprint-fixture");
        }
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let before = build_script::source_fingerprint(&desktop);
        std::fs::write(&target, b"first revision").unwrap();
        let after = build_script::source_fingerprint(&desktop);
        assert_ne!(before, after, "new release input ignored: {input}");
        assert_eq!(
            after,
            build_script::source_fingerprint(&desktop),
            "unstable fingerprint: {input}"
        );
        std::fs::write(&target, b"other revision").unwrap();
        assert_ne!(
            after,
            build_script::source_fingerprint(&desktop),
            "changed release input ignored: {input}"
        );
    }
}

#[test]
fn fingerprints_do_not_depend_on_checkout_location_or_generated_outputs() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for directory in [&first, &second] {
        std::fs::write(directory.path().join("styles.css"), b"body { color: red }").unwrap();
    }
    let hash = build_script::source_fingerprint(first.path());
    assert_eq!(hash, build_script::source_fingerprint(second.path()));
    std::fs::create_dir(first.path().join("dist")).unwrap();
    std::fs::write(first.path().join("dist/output.wasm"), b"generated").unwrap();
    assert_eq!(hash, build_script::source_fingerprint(first.path()));
}
