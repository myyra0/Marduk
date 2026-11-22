use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;

use marduk_workspace::{
    Code, Envelope, Op, Operation, Prepared, Preview, Purpose, Request, RestoreResult, SelectionId,
    Snapshot, WireError, Workspace, WorkspaceStatus,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

/// Shared desktop state. The catalog root is resolved once by Tauri from the
/// bundle identifier; no client input can change it.
pub struct DesktopState {
    app_data: PathBuf,
    workspace: Mutex<Option<Workspace>>,
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
        }
    }

    /// Lock the one workspace, opening it on first use. Resolving and consuming
    /// selections happens under this same lock, inside `marduk-workspace`.
    fn open_workspace(&self) -> Result<std::sync::MutexGuard<'_, Option<Workspace>>, WireError> {
        let mut guard = self
            .workspace
            .lock()
            .map_err(|_| WireError::new(Code::Storage))?;
        if guard.is_none() {
            *guard = Some(Workspace::open(&self.app_data)?);
        }
        Ok(guard)
    }

    /// Dispatch one request and decode its result into the frozen DTO. A
    /// serialization mismatch becomes `Storage`; it never reaches the UI as a
    /// success with invented fields.
    fn dispatch<T: DeserializeOwned>(&self, request: Request) -> Envelope<T> {
        let mut guard = match self.open_workspace() {
            Ok(guard) => guard,
            Err(error) => return Envelope::error(error),
        };
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

    fn remember(&self, purpose: Purpose, path: PathBuf) -> Result<SelectionId, WireError> {
        let mut guard = self.open_workspace()?;
        let Some(workspace) = guard.as_mut() else {
            return Err(WireError::new(Code::Storage));
        };
        workspace.remember_selection(purpose, path)
    }
}

// Workspace commands are async so the single workspace mutex and its disk I/O
// run on Tauri's async runtime, never on the main thread that owns the window.
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

#[tauri::command(async)]
pub fn setup(selection_id: String, state: State<'_, DesktopState>) -> Envelope<WorkspaceStatus> {
    state.dispatch(Request::new(
        Op::Setup,
        json!({ "selectionId": selection_id }),
    ))
}

#[tauri::command(async)]
pub fn status(state: State<'_, DesktopState>) -> Envelope<WorkspaceStatus> {
    state.dispatch(Request::new(Op::Status, json!({})))
}

#[tauri::command(async)]
pub fn import_preview(
    selection_id: String,
    name: String,
    at_revision: String,
    state: State<'_, DesktopState>,
) -> Envelope<Prepared> {
    state.dispatch(Request::new(
        Op::ImportPreview,
        json!({ "selectionId": selection_id, "name": name, "atRevision": at_revision }),
    ))
}

#[tauri::command(async)]
pub fn import_apply(import_id: String, state: State<'_, DesktopState>) -> Envelope<Snapshot> {
    state.dispatch(Request::new(
        Op::ImportApply,
        json!({ "importId": import_id }),
    ))
}

#[tauri::command(async)]
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

#[tauri::command(async)]
pub fn deploy_preview(at_revision: String, state: State<'_, DesktopState>) -> Envelope<Preview> {
    state.dispatch(Request::new(
        Op::DeployPreview,
        json!({ "atRevision": at_revision }),
    ))
}

#[tauri::command(async)]
pub fn deploy_apply(plan_id: String, state: State<'_, DesktopState>) -> Envelope<Operation> {
    state.dispatch(Request::new(Op::DeployApply, json!({ "planId": plan_id })))
}

#[tauri::command(async)]
pub fn restore(state: State<'_, DesktopState>) -> Envelope<RestoreResult> {
    state.dispatch(Request::new(Op::Restore, json!({})))
}

#[tauri::command(async)]
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
            selection_id: selection_id.to_string(),
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
    fn setup_refuses_a_renderer_supplied_path_as_a_handle() {
        let (_dir, state) = state();
        let envelope: Envelope<WorkspaceStatus> = state.dispatch(Request::new(
            Op::Setup,
            json!({ "selectionId": "/tmp/a-renderer-path" }),
        ));
        match envelope {
            Envelope::Error { error } => assert_eq!(error.code, Code::InvalidRequest),
            Envelope::Ok { .. } => panic!("a raw path must not be accepted as a handle"),
        }
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
