//! Marduk command-line client.
//!
//! Flags map to the same [`marduk_workspace::Request`] values the desktop
//! shell uses. `--json` prints exactly one envelope on stdout.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use marduk_workspace::{Code, Envelope, Op, Request, WireError, Workspace, app_data_root};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(
    name = "marduk",
    version,
    about = "Marduk mod manager workspace client"
)]
struct Cli {
    /// Print one result envelope as JSON on stdout.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bind the selected game installation. Grants no write authority.
    Setup {
        #[arg(long, value_name = "DIR")]
        game_dir: PathBuf,
    },
    /// Read the authoritative workspace status.
    Status,
    /// Prepare or publish an archive import.
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
    /// Reorder profile members low to high by membership ID.
    Order {
        #[arg(long, value_name = "REVISION")]
        at_revision: String,
        #[arg(value_name = "MEMBER-ID")]
        member_ids: Vec<String>,
    },
    /// Review or apply a deployment plan.
    Deploy {
        #[command(subcommand)]
        command: DeployCommand,
    },
    /// Restore the recorded deployed generation.
    Restore,
    /// Reconcile pending work recorded before an interrupted apply or restore.
    Recover,
    /// Read one operation's authoritative status.
    Operation { operation_id: String },
}

#[derive(Subcommand)]
enum ImportCommand {
    /// Validate a ZIP against the current profile revision.
    Preview {
        zip: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, value_name = "REVISION")]
        at_revision: String,
    },
    /// Publish a prepared import by its durable ID.
    Apply { import_id: String },
}

#[derive(Subcommand)]
enum DeployCommand {
    /// Review the exact deployment plan for a profile revision.
    Preview {
        #[arg(long, value_name = "REVISION")]
        at_revision: String,
    },
    /// Apply an approved plan by its durable ID.
    Apply { plan_id: String },
}

fn main() {
    let arguments: Vec<OsString> = std::env::args_os().collect();
    let wants_json = arguments
        .iter()
        .any(|argument| argument.to_string_lossy() == "--json");

    let cli = match Cli::try_parse_from(&arguments) {
        Ok(cli) => cli,
        Err(error) => {
            use clap::error::ErrorKind;
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                process::exit(0);
            }
            if wants_json {
                emit(&Envelope::<Value>::error(WireError::new(
                    Code::InvalidRequest,
                )));
            } else {
                let _ = error.print();
            }
            process::exit(2);
        }
    };

    process::exit(run(cli));
}

fn resolved_app_data() -> Result<std::path::PathBuf, WireError> {
    app_data_root()
}

fn run(cli: Cli) -> i32 {
    let request = match request(cli.command) {
        Ok(request) => request,
        Err(error) => return report(Envelope::error(error), cli.json),
    };
    let app_data = match resolved_app_data() {
        Ok(path) => path,
        Err(error) => return report(Envelope::error(error), cli.json),
    };
    let mut workspace = match Workspace::open(&app_data) {
        Ok(workspace) => workspace,
        Err(error) => return report(Envelope::error(error), cli.json),
    };
    report(workspace.dispatch(request), cli.json)
}

fn request(command: Command) -> Result<Request, WireError> {
    let (op, args) = match command {
        Command::Setup { game_dir } => (Op::Setup, json!({ "gameDir": path(&game_dir)? })),
        Command::Status => (Op::Status, json!({})),
        Command::Import { command } => match command {
            ImportCommand::Preview {
                zip,
                name,
                at_revision,
            } => (
                Op::ImportPreview,
                json!({
                    "zip": path(&zip)?,
                    "name": name,
                    "atRevision": at_revision,
                }),
            ),
            ImportCommand::Apply { import_id } => {
                (Op::ImportApply, json!({ "importId": import_id }))
            }
        },
        Command::Order {
            at_revision,
            member_ids,
        } => (
            Op::Order,
            json!({ "atRevision": at_revision, "memberIds": member_ids }),
        ),
        Command::Deploy { command } => match command {
            DeployCommand::Preview { at_revision } => {
                (Op::DeployPreview, json!({ "atRevision": at_revision }))
            }
            DeployCommand::Apply { plan_id } => (Op::DeployApply, json!({ "planId": plan_id })),
        },
        Command::Restore => (Op::Restore, json!({})),
        Command::Recover => (Op::Recover, json!({})),
        Command::Operation { operation_id } => {
            (Op::Operation, json!({ "operationId": operation_id }))
        }
    };
    Ok(Request::new(op, args))
}

fn path(path: &PathBuf) -> Result<&str, WireError> {
    path.to_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WireError::new(Code::InvalidRequest))
}

fn report(envelope: Envelope<Value>, wants_json: bool) -> i32 {
    if wants_json {
        emit(&envelope);
    } else {
        match &envelope {
            Envelope::Ok { result } => match serde_json::to_string_pretty(result) {
                Ok(text) => println!("{text}"),
                Err(_) => eprintln!("Storage"),
            },
            Envelope::Error { error } => eprintln!("{}", human_error(error)),
        }
    }
    exit_code(&envelope)
}

/// Human mode keeps the stable code and the same serialized detail the wire
/// envelope carries, so wording never replaces the typed reason.
fn human_error(error: &WireError) -> String {
    let mut text = match &error.detail {
        None => error.code.as_str().to_owned(),
        Some(detail) => match serde_json::to_string(detail) {
            Ok(text) => format!("{} {text}", error.code.as_str()),
            Err(_) => error.code.as_str().to_owned(),
        },
    };
    if let Some(message) = &error.message {
        text.push_str(": ");
        text.push_str(message);
    }
    text
}

fn emit(envelope: &Envelope<Value>) {
    match serde_json::to_string(envelope) {
        Ok(text) => println!("{text}"),
        Err(_) => println!(r#"{{"ok":false,"error":{{"code":"Storage"}}}}"#),
    }
}

/// A blocked operation is an `Ok` envelope whose recorded operation carries a
/// non-null `blocker`; it exits 1 like a typed refusal because the work did not
/// settle. `DeployApply` returns the operation directly; `Restore` nests it
/// under `operation`. `null` and a blocker-free operation exit 0.
fn exit_code(envelope: &Envelope<Value>) -> i32 {
    match envelope {
        Envelope::Ok { result, .. } => {
            if is_blocked(result) {
                1
            } else {
                0
            }
        }
        Envelope::Error { error, .. } => {
            if error.code == Code::InvalidRequest {
                2
            } else {
                1
            }
        }
    }
}

fn is_blocked(value: &Value) -> bool {
    let has_blocker = |operation: &Value| {
        operation
            .get("blocker")
            .is_some_and(|blocker| !blocker.is_null())
    };
    has_blocker(value) || value.get("operation").is_some_and(has_blocker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_resolves_the_shared_app_data_root() {
        let root = resolved_app_data().expect("an OS data directory is available");
        assert_eq!(
            root.file_name(),
            Some(std::ffi::OsStr::new(marduk_workspace::APP_IDENTIFIER))
        );
        assert_eq!(root, app_data_root().unwrap());
    }

    #[test]
    fn blocked_recovery_exits_nonzero() {
        let blocked: Envelope<Value> = Envelope::Ok {
            result: json!({
                "id": "9223372036854775803",
                "kind": "deploy",
                "phase": "rolling_back",
                "blocker": { "code": "RecoveryConflict" },
            }),
        };
        assert_eq!(exit_code(&blocked), 1);

        let resolved: Envelope<Value> = Envelope::Ok {
            result: json!({
                "id": "9223372036854775803",
                "kind": "deploy",
                "phase": "rolled_back",
                "blocker": null,
            }),
        };
        assert_eq!(exit_code(&resolved), 0);

        let nothing: Envelope<Value> = Envelope::Ok {
            result: Value::Null,
        };
        assert_eq!(exit_code(&nothing), 0);

        let blocked_restore: Envelope<Value> = Envelope::Ok {
            result: json!({
                "outcome": "operation",
                "operation": {
                    "id": "9223372036854775803",
                    "kind": "restore",
                    "phase": "rolling_back",
                    "blocker": { "code": "RecoveryConflict" },
                },
            }),
        };
        assert_eq!(exit_code(&blocked_restore), 1);

        let settled_restore: Envelope<Value> = Envelope::Ok {
            result: json!({
                "outcome": "operation",
                "operation": {
                    "id": "9223372036854775803",
                    "kind": "restore",
                    "phase": "completed",
                    "blocker": null,
                },
            }),
        };
        assert_eq!(exit_code(&settled_restore), 0);
    }
}
