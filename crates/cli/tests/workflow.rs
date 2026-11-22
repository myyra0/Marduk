//! CLI workflow witnesses against the real shared workspace service.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

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

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_marduk"))
            .args(arguments)
            .env("XDG_DATA_HOME", &self.data)
            .env("LOCALAPPDATA", &self.data)
            .output()
            .unwrap()
    }

    fn app_root(&self) -> PathBuf {
        self.data
            .join(if cfg!(windows) { "Marduk" } else { "marduk" })
    }

    fn database(&self) -> PathBuf {
        self.app_root().join("catalog.sqlite3")
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
    assert_eq!(
        value["result"]["availability"]["import"]["blocker"]["code"],
        json!("StageUnavailable")
    );

    let output = fixture.run(&["status", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let value = envelope(&output);
    assert_eq!(value["result"]["profile"]["revision"], json!("0"));
    assert_eq!(
        value["result"]["availability"]["deploy"]["blocker"]["detail"]["reason"],
        json!("Configuration")
    );

    let output = fixture.run(&["order", "--at-revision", "0", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(envelope(&output)["result"]["revision"], json!("1"));

    let output = fixture.run(&["order", "--at-revision", "0", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&envelope(&output)), "StalePlan");
}

#[test]
fn blocked_routes_are_typed_unavailable() {
    let fixture = Fixture::new();
    let zip = fixture.temp.path().join("import.zip");
    fs::write(&zip, b"not a zip").unwrap();

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
    assert_eq!(code(&envelope(&output)), "StageUnavailable");

    let output = fixture.run(&["import", "apply", "1", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&envelope(&output)), "StageUnavailable");

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
fn large_revision_round_trips_as_a_string() {
    let fixture = Fixture::new();

    // Create the catalog through the service, then arrange one large revision.
    let output = fixture.run(&["status", "--json"]);
    assert_eq!(output.status.code(), Some(0));

    let revision = "9007199254740993"; // 2^53 + 1
    let member = "9223372036854775806"; // i64::MAX - 1
    let connection = rusqlite::Connection::open(fixture.database()).unwrap();
    connection
        .execute_batch(&format!(
            "INSERT INTO imports
                 (id, profile_id, expected_revision, target, display_name, state,
                  owner, archive_sha, result_revision)
             VALUES (1, 1, 0, 'append', 'Big', 'committed', x'00', zeroblob(32), 0);
             INSERT INTO installed_revisions (id) VALUES (1);
             INSERT INTO members
                 (id, profile_id, ordinal, installed_id, enabled, display_name)
             VALUES ({member}, 1, 0, 1, 1, 'Big');
             UPDATE profiles SET revision = {revision} WHERE id = 1;"
        ))
        .unwrap();
    drop(connection);

    let output = fixture.run(&["order", "--at-revision", revision, member, "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.matches('\n').count(), 1);
    assert!(
        text.contains("\"revision\":\"9007199254740994\""),
        "revision must survive as an exact string: {text}"
    );
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["result"]["revision"], json!("9007199254740994"));
    assert_eq!(value["result"]["members"][0]["id"], json!(member));
    assert!(value["result"]["revision"].is_string());
    assert!(value["result"]["members"][0]["id"].is_string());
}
