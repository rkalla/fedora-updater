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

/// Window close runs `close_session` on the GTK thread. If a helper command is
/// stuck (fwupdmgr waiting on "Restart now?"), that must not freeze the UI.
#[test]
fn close_session_does_not_block_when_helper_command_hangs() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::thread;

    use fedora_updater::privilege::{close_session, with_session};

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hung-helper");
    std::fs::write(
        &script,
        r#"#!/bin/sh
printf '%s\n' '__READY__ fedora-updater-helper'
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    QUIT)
      printf '%s\n' '__BYE__'
      exit 0
      ;;
    RUN*)
      printf '%s\n' 'hung-helper: idle'
      sleep 3600
      ;;
  esac
done
"#,
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    std::env::set_var("FEDORA_UPDATER_HELPER", &script);
    std::env::set_var("FEDORA_UPDATER_HELPER_DIRECT", "1");

    let worker = thread::spawn(|| {
        let _ = with_session(|session| session.run_command(HelperCommand::CheckDnf, |_| {}));
    });

    thread::sleep(Duration::from_millis(300));

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        close_session();
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(2)).expect(
        "close_session blocked while a helper command was hung — this freezes GTK on window close",
    );

    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = worker.join();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker stayed blocked after close_session");
}
