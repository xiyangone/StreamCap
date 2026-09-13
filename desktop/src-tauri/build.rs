fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "desktop_ready",
            "desktop_window_action",
            "desktop_close_choice",
            "desktop_theme",
            "desktop_smoke_tray_quit",
        ]),
    ))
    .expect("Tauri build and command permission generation failed");
}
