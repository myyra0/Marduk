use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio, exit};

use marduk_platform::{GateError, InstanceGuard, acquire_instance_guard};

#[test]
fn catalog_guard_is_nonblocking_and_independent_of_other_catalogs() {
    let _serial = PROCESS_SPAWN.lock().unwrap();
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<InstanceGuard>();
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    let guard = acquire_instance_guard(&a).unwrap();
    assert!(matches!(acquire_instance_guard(&a), Err(GateError::Busy)));
    let _other = acquire_instance_guard(&b).unwrap();
    assert!(a.join("instance.lock").is_file());
    drop(guard);
    let _reacquired = acquire_instance_guard(&a).unwrap();
}

// A concurrently forked child can keep a CLOEXEC lock descriptor until exec.
static PROCESS_SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn same_catalog_refuses_two_processes_with_different_environments() {
    const CATALOG: &str = "MARDUK_INSTANCE_WITNESS_CATALOG";
    const HOLD: &str = "MARDUK_INSTANCE_WITNESS_HOLD";
    if let Some(root) = std::env::var_os(CATALOG) {
        match acquire_instance_guard(Path::new(&root)) {
            Ok(_guard) => {
                if std::env::var_os(HOLD).is_some() {
                    std::io::stdout().write_all(b"\nheld!\n").unwrap();
                    std::io::stdout().flush().unwrap();
                    let _ = std::io::stdin().read(&mut [0]);
                }
                exit(0);
            }
            Err(GateError::Busy) => exit(71),
            Err(error) => panic!("{error:?}"),
        }
    }

    let _serial = PROCESS_SPAWN.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let catalog = temp.path().join("catalog");
    let command = |env_name| {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "same_catalog_refuses_two_processes_with_different_environments",
                "--nocapture",
            ])
            .env(CATALOG, &catalog)
            .env("HOME", temp.path().join(env_name).join("home"))
            .env(
                "XDG_RUNTIME_DIR",
                temp.path().join(env_name).join("runtime"),
            )
            .env("XDG_DATA_HOME", temp.path().join(env_name).join("data"))
            .env("LOCALAPPDATA", temp.path().join(env_name).join("local"));
        child
    };
    let mut first = command("one")
        .env(HOLD, "1")
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = first.stdout.take().unwrap();
    let (ready, seen) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let found = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .any(|line| line == "held!");
        let _ = ready.send(found);
    });
    if !seen
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or(false)
    {
        let _ = first.kill();
        let _ = first.wait();
        panic!("first process did not acquire catalog");
    }
    let mut second = command("two").stdout(Stdio::null()).spawn().unwrap();
    let (status, observed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = status.send(second.wait());
    });
    let second = observed.recv_timeout(std::time::Duration::from_secs(5));
    drop(first.stdin.take());
    assert!(first.wait().unwrap().success());
    assert_eq!(
        second.expect("second acquisition queued").unwrap().code(),
        Some(71)
    );
    assert!(command("two").output().unwrap().status.success());
}
