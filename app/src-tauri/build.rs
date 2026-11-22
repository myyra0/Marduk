fn main() {
    // Restrict the registered commands to the frozen desktop set. Each entry
    // generates an `allow-<command>` permission that the main-window
    // capability must grant explicitly.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "pick_installation",
            "pick_zip",
            "setup",
            "status",
            "import_preview",
            "import_apply",
            "order",
            "deploy_preview",
            "deploy_apply",
            "restore",
            "operation",
        ]),
    ))
    .expect("failed to run the Tauri build script");
}
