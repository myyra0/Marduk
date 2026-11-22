//! Evidence probe for the Windows namespace barrier question.
//!
//! Writes a payload, flushes the file, flushes its parent directory, prints
//! `CHECKPOINT`, then blocks. The host power-cuts the machine when it sees the
//! marker. After reboot, whether the payload exists with the expected bytes is
//! the only thing under test.
//!
//! This does not prove durability in general; it measures one flush-then-cut
//! cycle on one filesystem. Run it several times before drawing a conclusion.
//!
//! usage: `barrier_probe <directory> <file-name> <contents>`
//!
//! Only built on Windows: it exists to answer a question about NTFS.

#[cfg(windows)]
fn main() {
    use std::io::Write;

    let mut args = std::env::args().skip(1);
    let (Some(directory), Some(name), Some(contents)) = (args.next(), args.next(), args.next())
    else {
        eprintln!("usage: barrier_probe <directory> <file-name> <contents>");
        std::process::exit(2);
    };

    let directory = std::path::PathBuf::from(directory);
    let target = directory.join(&name);
    std::fs::create_dir_all(&directory).expect("create probe directory");

    {
        let mut file = std::fs::File::create(&target).expect("create payload");
        file.write_all(contents.as_bytes()).expect("write payload");
        file.sync_all().expect("flush payload");
    }
    marduk_platform::effects::sync_directory(&directory).expect("flush directory");

    println!("CHECKPOINT");
    use std::io::Write as _;
    std::io::stdout().flush().ok();

    // Stay alive so the cut lands after the flush, not after exit.
    std::thread::sleep(std::time::Duration::from_secs(600));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("barrier_probe is a Windows/NTFS evidence probe");
    std::process::exit(2);
}
