//! Marduk desktop shell.
//!
//! Every command decodes a frozen request, calls the shared [`Workspace`]
//! service and returns the same envelope the CLI prints. The catalog root is
//! Tauri's application data directory for the frozen bundle identifier.

mod commands;

use std::path::PathBuf;

use tauri::Manager;

use commands::DesktopState;

/// The catalog root shared with the CLI. Tauri's own resolver is not used here:
/// one OS resolver in `marduk-workspace` keeps both clients on the same path.
pub fn app_data_root() -> Result<PathBuf, String> {
    marduk_workspace::app_data_root().map_err(|error| format!("{error:?}"))
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_data = app_data_root().map_err(std::io::Error::other)?;
            app.manage(DesktopState::new(app_data));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::pick_installation,
            commands::pick_zip,
            commands::setup,
            commands::status,
            commands::import_preview,
            commands::import_apply,
            commands::order,
            commands::deploy_preview,
            commands::deploy_apply,
            commands::restore,
            commands::recover,
            commands::operation,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the Marduk desktop shell");
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn desktop_resolves_the_shared_app_data_root() {
        let root = app_data_root().expect("an OS data directory is available");
        assert_eq!(
            root.file_name(),
            Some(OsStr::new(marduk_workspace::APP_IDENTIFIER))
        );
        assert_eq!(root, marduk_workspace::app_data_root().unwrap());
    }
}
