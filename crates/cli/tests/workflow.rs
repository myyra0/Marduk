//! CLI workflow witnesses against the real shared workspace service.
//!
//! The shared resolver reads `XDG_DATA_HOME`/`HOME` on Unix. Windows resolves
//! the roaming known folder with no environment input, so a subprocess cannot
//! be isolated there without touching the runner's real catalog; these
//! witnesses are Unix-only and the pure resolver test covers the layout.

#![cfg(unix)]

#[allow(dead_code)]
#[path = "../../archive/tests/fixtures/mod.rs"]
mod zip_fixture;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;
use zip_fixture::{ClassicZip, Record};

struct Fixture {
    temp: TempDir,
    data: PathBuf,
    game: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let game = temp.path().join("Skyrim Special Edition");
        fs::create_dir_all(game.join("Data")).unwrap();
        fs::write(game.join("SkyrimSE.exe"), b"").unwrap();
        Self { temp, data, game }
    }

    fn command(&self, cwd: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_marduk"));
        command
            .args(arguments)
            .current_dir(cwd)
            .env("XDG_DATA_HOME", &self.data)
            .env("HOME", self.temp.path())
            .env_remove("APPDATA")
            .env_remove("LOCALAPPDATA");
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command(self.temp.path(), arguments).output().unwrap()
    }

    fn run_at(&self, cwd: &Path, arguments: &[&str]) -> Output {
        self.command(cwd, arguments).output().unwrap()
    }

    fn app_root(&self) -> PathBuf {
        self.data.join("com.marduk.desktop")
    }

    fn game_dir(&self) -> &str {
        self.game.to_str().unwrap()
    }
}

fn envelope(output: &Output) -> Value {
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1,
        "expected one JSON line, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap()
}

fn tree(root: &Path) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        entries.push(path.clone());
        if path.is_dir() {
            entries.extend(tree(&path));
        }
    }
    entries.sort();
    entries
}

#[test]
fn setup_status_and_empty_order_workflow() {
    let fixture = Fixture::new();

    let output = fixture.run(&["setup", "--game-dir", fixture.game_dir(), "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let value = envelope(&output);
    assert_eq!(value["ok"], json!(true));
    assert_eq!(
        value["result"]["installation"]["label"],
        json!("Skyrim Special Edition")
    );
    assert_eq!(value["result"]["profile"]["revision"], json!("0"));
    assert_eq!(
        value["result"]["availability"]["target"]["available"],
        json!(true)
    );
    let import = &value["result"]["availability"]["import"];
    if import["available"] == json!(true) {
        assert!(import["blocker"].is_null());
    } else {
        assert_eq!(import["blocker"]["code"], json!("StageUnavailable"));
    }

    let output = fixture.run(&["status", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let value = envelope(&output);
    assert_eq!(value["result"]["profile"]["revision"], json!("0"));
    assert_eq!(
        value["result"]["availability"]["deploy"]["blocker"]["detail"]["reason"],
        json!("Configuration")
    );

    // A clean root has no pending work; reconciliation answers null without a
    // native effect.
    let output = fixture.run(&["recover", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(envelope(&output)["result"], Value::Null);

    let output = fixture.run(&["order", "--at-revision", "0", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(envelope(&output)["result"]["revision"], json!("1"));

    let output = fixture.run(&["order", "--at-revision", "0", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&envelope(&output)), "StalePlan");
}

#[test]
fn setup_leaves_the_game_tree_unchanged_and_writes_no_revision() {
    let fixture = Fixture::new();
    let before = tree(&fixture.game);

    let output = fixture.run(&["setup", "--game-dir", fixture.game_dir(), "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(tree(&fixture.game), before);
    assert_eq!(
        fs::read_dir(fixture.app_root().join("revisions"))
            .unwrap()
            .count(),
        0
    );

    let output = fixture.run(&["status", "--json"]);
    let value = envelope(&output);
    assert_eq!(value["result"]["profile"]["revision"], json!("0"));
    assert_eq!(value["result"]["pending"], Value::Null);
    assert_eq!(value["result"]["activeGenerationId"], Value::Null);
}

#[test]
fn relative_setup_persists_an_absolute_locator() {
    let fixture = Fixture::new();
    let relative = fixture.game.file_name().unwrap().to_str().unwrap();

    // Bind through a relative argument from the parent directory.
    let output = fixture.run_at(
        fixture.temp.path(),
        &["setup", "--game-dir", relative, "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A different working directory proves the durable locator is absolute.
    let elsewhere = fixture.temp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let output = fixture.run_at(&elsewhere, &["status", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let value = envelope(&output);
    assert_eq!(
        value["result"]["installation"]["label"],
        json!("Skyrim Special Edition")
    );
    assert_eq!(
        value["result"]["availability"]["target"]["available"],
        json!(true)
    );
}

#[test]
fn human_mode_keeps_the_stable_code_and_serialized_detail() {
    let fixture = Fixture::new();

    // No bound installation is a typed refusal, never a simulated clean root.
    let output = fixture.run(&["recover"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "TargetUnavailable"
    );

    let output = fixture.run(&["setup", "--game-dir", fixture.game_dir()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = fixture.run(&["recover"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "null");

    let output = fixture.run(&["deploy", "preview", "--at-revision", "0"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("UnsupportedTarget"), "stderr: {stderr}");
    assert!(stderr.contains("Configuration"), "stderr: {stderr}");

    let output = fixture.run(&["order", "--at-revision", "0", "1"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.trim(), "InvalidOrder");
}

#[test]
fn import_availability_matches_the_stage_route() {
    let fixture = Fixture::new();
    let output = fixture.run(&["setup", "--game-dir", fixture.game_dir(), "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let available = envelope(&output)["result"]["availability"]["import"]["available"]
        .as_bool()
        .unwrap();

    let zip = fixture.temp.path().join("qualified.zip");
    let bytes = ClassicZip::new(&[Record::file(b"synthetic.txt", b"fixture bytes")]).bytes;
    fs::write(&zip, bytes).unwrap();
    let output = fixture.run(&[
        "import",
        "preview",
        zip.to_str().unwrap(),
        "--name",
        "Example",
        "--at-revision",
        "0",
        "--json",
    ]);

    if available {
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(
            envelope(&output)["result"]["files"][0]["path"],
            json!("synthetic.txt")
        );
    } else {
        assert_eq!(code(&envelope(&output)), "StageUnavailable");
        assert_eq!(
            fs::read_dir(fixture.app_root().join("revisions"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn blocked_routes_are_typed_unavailable() {
    let fixture = Fixture::new();
    let zip = fixture.temp.path().join("import.zip");
    fs::write(&zip, b"not a zip").unwrap();

    // Reconciliation without a bound installation refuses; it never answers
    // `null` as though the root were clean.
    let output = fixture.run(&["recover", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&envelope(&output)), "TargetUnavailable");

    let output = fixture.run(&[
        "import",
        "preview",
        zip.to_str().unwrap(),
        "--name",
        "Example",
        "--at-revision",
        "0",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    // An unqualified volume refuses before any write; a qualified one reaches
    // admission and rejects the container. Both are typed refusals.
    let preview_envelope = envelope(&output);
    let preview_code = code(&preview_envelope);
    assert!(
        preview_code == "StageUnavailable" || preview_code == "InvalidContainer",
        "unexpected preview refusal: {preview_code}"
    );

    let output = fixture.run(&["import", "apply", "1", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    // Unqualified volume: preview StageUnavailable, then apply -> UnknownImport.
    // Qualified volume: preview InvalidContainer, then apply -> ImportNotPrepared.
    if preview_code == "StageUnavailable" {
        assert_eq!(code(&envelope(&output)), "UnknownImport");
    } else {
        assert_eq!(code(&envelope(&output)), "ImportNotPrepared");
    }

    for arguments in [
        vec!["deploy", "preview", "--at-revision", "0", "--json"],
        vec!["deploy", "apply", "1", "--json"],
        vec!["restore", "--json"],
    ] {
        let output = fixture.run(&arguments);
        assert_eq!(output.status.code(), Some(1));
        let value = envelope(&output);
        assert_eq!(code(&value), "UnsupportedTarget");
        assert_eq!(value["error"]["detail"]["reason"], json!("Configuration"));
    }

    let output = fixture.run(&["operation", "1", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&envelope(&output)), "UnknownOperation");
}

#[test]
fn usage_and_id_syntax_exit_two() {
    let fixture = Fixture::new();

    for arguments in [
        vec!["shell", "--json"],
        vec!["write-file", "--json"],
        vec!["order", "--at-revision", "nope", "--json"],
        vec!["order", "--at-revision", "0", "0", "--json"],
        vec!["setup", "--json"],
    ] {
        let output = fixture.run(&arguments);
        assert_eq!(
            output.status.code(),
            Some(2),
            "arguments: {arguments:?}, stdout: {}, stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(code(&envelope(&output)), "InvalidRequest");
    }

    let output = fixture.run(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    assert!(!help.contains("shell"));
    assert!(!help.contains("write file"));
    assert!(!help.contains("write-file"));
}

#[test]
fn id_and_revision_syntax_stops_at_the_signed_boundary() {
    let fixture = Fixture::new();
    let output = fixture.run(&["setup", "--game-dir", fixture.game_dir(), "--json"]);
    assert_eq!(output.status.code(), Some(0));

    // i64::MAX is a valid revision string but stale, so it is parsed exactly.
    let output = fixture.run(&["order", "--at-revision", "9223372036854775807", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&envelope(&output)), "StalePlan");

    for arguments in [
        vec!["order", "--at-revision", "9223372036854775808", "--json"],
        vec![
            "order",
            "--at-revision",
            "0",
            "9223372036854775808",
            "--json",
        ],
    ] {
        let output = fixture.run(&arguments);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(code(&envelope(&output)), "InvalidRequest");
    }
}
