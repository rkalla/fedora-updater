//! Integration tests for privileged helper mode (multi-call: --helper).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use fedora_updater::helper_protocol::{parse_end_line, HelperCommand, BYE_LINE, HELLO_LINE};
use fedora_updater::privilege::HELPER_FLAG;

fn updater_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps
    path.pop(); // debug or release
    path.push("fedora-updater");
    if path.exists() {
        return path;
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let p = std::path::PathBuf::from(manifest).join("target/debug/fedora-updater");
    assert!(
        p.exists(),
        "build fedora-updater first (cargo test builds it)"
    );
    p
}

fn spawn_helper() -> std::process::Child {
    Command::new(updater_bin())
        .arg(HELPER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fedora-updater --helper")
}

#[test]
fn helper_hello_quit() {
    let mut child = spawn_helper();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut hello = String::new();
    stdout.read_line(&mut hello).unwrap();
    assert_eq!(hello.trim_end(), HELLO_LINE);

    writeln!(stdin, "QUIT").unwrap();
    let mut bye = String::new();
    stdout.read_line(&mut bye).unwrap();
    assert_eq!(bye.trim_end(), BYE_LINE);

    let status = child.wait().unwrap();
    assert!(status.success() || status.code() == Some(0));
}

#[test]
fn helper_rejects_unknown_command() {
    let mut child = spawn_helper();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();

    writeln!(stdin, "RUN rm-rf-root").unwrap();
    loop {
        line.clear();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "helper closed");
        if let Some(code) = parse_end_line(line.trim_end()) {
            assert_eq!(code, 2);
            break;
        }
    }

    writeln!(stdin, "QUIT").unwrap();
    let _ = child.wait();
}

#[test]
fn helper_runs_check_dnf_if_present() {
    if Command::new("dnf").arg("--version").output().is_err() {
        return;
    }

    let mut child = spawn_helper();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();

    writeln!(stdin, "{}", HelperCommand::CheckDnf.encode_run()).unwrap();

    let start = std::time::Instant::now();
    let mut saw_end = false;
    while start.elapsed() < Duration::from_secs(120) {
        line.clear();
        match stdout.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if parse_end_line(line.trim_end()).is_some() {
                    saw_end = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(saw_end, "expected __END__ from check-dnf");
    writeln!(stdin, "QUIT").unwrap();
    let _ = child.kill();
}
