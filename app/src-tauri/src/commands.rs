use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;

use marduk_workspace::{
    Code, Envelope, Op, Operation, Prepared, Preview, Request, RestoreResult, Snapshot, WireError,
    Workspace, WorkspaceStatus,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::selection::{Purpose, SelectionStore};

/// Shared desktop state. The catalog root is resolved once by Tauri from the
/// bundle identifier; no client input can change it.
pub struct DesktopState {
    app_data: PathBuf,
    workspace: Mutex<Option<Workspace>>,
    selections: Mutex<SelectionStore>,
}

/// The renderer-visible selection. The path stays in Rust.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionDto {
    pub selection_id: String,
    pub label: String,
}

impl DesktopState {
    pub fn new(app_data: PathBuf) -> Self {
        Self {
            app_data,
            workspace: Mutex::new(None),
            selections: Mutex::new(SelectionStore::new()),
        }
    }

    /// Dispatch one request and decode its result into the frozen DTO. A
    /// serialization mismatch becomes `Storage`; it never reaches the UI as a
    /// success with invented fields.
    fn dispatch<T: DeserializeOwned>(&self, request: Request) -> Envelope<T> {
        let mut guard = match self.workspace.lock() {
            Ok(guard) => guard,
            Err(_) => return Envelope::error(WireError::new(Code::Storage)),
        };
        if guard.is_none() {
            match Workspace::open(&self.app_data) {
                Ok(workspace) => *guard = Some(workspace),
                Err(error) => return Envelope::error(error),
            }
        }
        let Some(workspace) = guard.as_mut() else {
            return Envelope::error(WireError::new(Code::Storage));
        };
        match workspace.dispatch(request) {
            Envelope::Ok { result } => match serde_json::from_value::<T>(result) {
                Ok(dto) => Envelope::ok(dto),
                Err(_) => Envelope::error(WireError::new(Code::Storage)),
            },
            Envelope::Error { error } => Envelope::error(error),
        }
    }

    fn remember(&self, purpose: Purpose, path: PathBuf) -> Result<String, WireError> {
        let mut store = self
            .selections
            .lock()
            .map_err(|_| WireError::new(Code::Storage))?;
        Ok(store.insert(purpose, path))
    }

    fn consume(&self, selection_id: &str, purpose: Purpose) -> Result<PathBuf, WireError> {
        let mut store = self
            .selections
            .lock()
            .map_err(|_| WireError::new(Code::Storage))?;
        store.take(selection_id, purpose)
    }
}

#[tauri::command]
pub async fn pick_installation(app: AppHandle) -> Envelope<Option<SelectionDto>> {
    let picked = pick(&app, Purpose::Installation).await;
    finish_pick(&app, Purpose::Installation, picked)
}

#[tauri::command]
pub async fn pick_zip(app: AppHandle) -> Envelope<Option<SelectionDto>> {
    let picked = pick(&app, Purpose::Archive).await;
    finish_pick(&app, Purpose::Archive, picked)
}

#[tauri::command]
pub fn setup(selection_id: String, state: State<'_, DesktopState>) -> Envelope<WorkspaceStatus> {
    match state.consume(&selection_id, Purpose::Installation) {
        Ok(path) => match path.to_str() {
            Some(game_dir) => {
                state.dispatch(Request::new(Op::Setup, json!({ "gameDir": game_dir })))
            }
            None => Envelope::error(WireError::new(Code::InvalidRequest)),
        },
        Err(error) => Envelope::error(error),
    }
}

#[tauri::command]
pub fn status(state: State<'_, DesktopState>) -> Envelope<WorkspaceStatus> {
    state.dispatch(Request::new(Op::Status, json!({})))
}

#[tauri::command]
pub fn import_preview(
    selection_id: String,
    name: String,
    at_revision: String,
    state: State<'_, DesktopState>,
) -> Envelope<Prepared> {
    match state.consume(&selection_id, Purpose::Archive) {
        Ok(path) => match path.to_str() {
            Some(zip) => state.dispatch(Request::new(
                Op::ImportPreview,
                json!({ "zip": zip, "name": name, "atRevision": at_revision }),
            )),
            None => Envelope::error(WireError::new(Code::InvalidRequest)),
        },
        Err(error) => Envelope::error(error),
    }
}

#[tauri::command]
pub fn import_apply(import_id: String, state: State<'_, DesktopState>) -> Envelope<Snapshot> {
    state.dispatch(Request::new(
        Op::ImportApply,
        json!({ "importId": import_id }),
    ))
}

#[tauri::command]
pub fn order(
    at_revision: String,
    member_ids: Vec<String>,
    state: State<'_, DesktopState>,
) -> Envelope<Snapshot> {
    state.dispatch(Request::new(
        Op::Order,
        json!({ "atRevision": at_revision, "memberIds": member_ids }),
    ))
}

#[tauri::command]
pub fn deploy_preview(at_revision: String, state: State<'_, DesktopState>) -> Envelope<Preview> {
    state.dispatch(Request::new(
        Op::DeployPreview,
        json!({ "atRevision": at_revision }),
    ))
}

#[tauri::command]
pub fn deploy_apply(plan_id: String, state: State<'_, DesktopState>) -> Envelope<Operation> {
    state.dispatch(Request::new(Op::DeployApply, json!({ "planId": plan_id })))
}

#[tauri::command]
pub fn restore(state: State<'_, DesktopState>) -> Envelope<RestoreResult> {
    state.dispatch(Request::new(Op::Restore, json!({})))
}

#[tauri::command]
pub fn operation(operation_id: String, state: State<'_, DesktopState>) -> Envelope<Operation> {
    state.dispatch(Request::new(
        Op::Operation,
        json!({ "operationId": operation_id }),
    ))
}

enum Picked {
    Path(PathBuf),
    Cancelled,
    Failed(Code),
}

async fn pick(app: &AppHandle, purpose: Purpose) -> Picked {
    let (sender, receiver) = mpsc::channel();
    match purpose {
        Purpose::Installation => {
            app.dialog()
                .file()
                .set_title("Choose the Skyrim SE installation folder")
                .pick_folder(move |selected| {
                    let _ = sender.send(selected);
                });
        }
        Purpose::Archive => {
            app.dialog()
                .file()
                .set_title("Choose a mod archive")
                .add_filter("ZIP archive", &["zip"])
                .pick_file(move |selected| {
                    let _ = sender.send(selected);
                });
        }
    }

    // The dialog callback runs on the main thread; waiting on a blocking pool
    // thread keeps the async command off the UI thread without a busy loop.
    let selected =
        tauri::async_runtime::spawn_blocking(move || receiver.recv().ok().flatten()).await;
    match selected {
        Ok(Some(file)) => match file.into_path() {
            Ok(path) => Picked::Path(path),
            Err(_) => Picked::Failed(Code::InvalidPath),
        },
        Ok(None) => Picked::Cancelled,
        Err(_) => Picked::Failed(Code::InvalidPath),
    }
}

fn finish_pick(
    app: &AppHandle,
    purpose: Purpose,
    picked: Picked,
) -> Envelope<Option<SelectionDto>> {
    let path = match picked {
        Picked::Path(path) => path,
        Picked::Cancelled => return Envelope::ok(None),
        Picked::Failed(code) => return Envelope::error(WireError::new(code)),
    };
    let label = label(&path);
    let state = app.state::<DesktopState>();
    match state.remember(purpose, path) {
        Ok(selection_id) => Envelope::ok(Some(SelectionDto {
            selection_id,
            label,
        })),
        Err(error) => Envelope::error(error),
    }
}

fn label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("selection")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (tempfile::TempDir, DesktopState) {
        let dir = tempfile::tempdir().unwrap();
        let state = DesktopState::new(dir.path().to_path_buf());
        (dir, state)
    }

    #[test]
    fn status_result_decodes_into_the_frozen_dto() {
        let (_dir, state) = state();
        let envelope: Envelope<WorkspaceStatus> =
            state.dispatch(Request::new(Op::Status, json!({})));
        let status = match envelope {
            Envelope::Ok { result } => result,
            Envelope::Error { error } => panic!("status failed: {}", error.code.as_str()),
        };
        assert_eq!(status.profile.id, "1");
        assert_eq!(status.profile.revision, "0");
        assert!(status.installation.is_none());
    }

    #[test]
    fn unavailable_restore_keeps_its_typed_blocker() {
        let (_dir, state) = state();
        let envelope: Envelope<RestoreResult> =
            state.dispatch(Request::new(Op::Restore, json!({})));
        match envelope {
            Envelope::Error { error } => {
                assert_eq!(error.code, Code::UnsupportedTarget);
                assert_eq!(
                    error.detail,
                    Some(marduk_workspace::Detail::Reason {
                        reason: marduk_workspace::ReasonDto::Configuration
                    })
                );
            }
            Envelope::Ok { .. } => panic!("restore must not simulate success"),
        }
    }
}
