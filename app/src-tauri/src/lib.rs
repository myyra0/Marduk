//! Marduk desktop shell.
//!
//! Every command decodes a frozen request, calls the shared [`Workspace`]
//! service and returns the same envelope the CLI prints. The catalog root is
//! Tauri's application data directory for the frozen bundle identifier.

mod commands;
mod selection;

use tauri::Manager;

use commands::DesktopState;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_data = app.path().app_data_dir()?;
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
            commands::operation,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the Marduk desktop shell");
}
