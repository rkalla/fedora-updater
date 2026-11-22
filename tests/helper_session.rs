//! Integration tests for privileged helper mode (multi-call: --helper).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use fedora_updater::helper_protocol::{parse_end_line, HelperCommand, BYE_LINE, HELLO_LINE};
use fedora_updater::privilege::{
    close_session, session_is_alive, with_direct_session_for_test, SessionError, SessionEvent,
    HELPER_FLAG,
};

/// Prevents `Command::spawn` in sibling tests from inheriting a writable FD
/// to temporary fake-helper scripts while they are being written (ETXTBSY).
static FORK_WRITE_LOCK: RwLock<()> = RwLock::new(());
/// Serializes tests in this binary that share the process-wide `SESSION` pool.
static SESSION_TEST_LOCK: Mutex<()> = Mutex::new(());

struct CloseSessionGuard;

impl CloseSessionGuard {
    fn new() -> Self {
        close_session();
        Self
    }
}

impl Drop for CloseSessionGuard {
    fn drop(&mut self) {
        close_session();
    }
}

fn updater_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps
    path.pop(); // debug or release
    path.push("fedora-updater");
    if path.exists() {
        return path;
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let p = PathBuf::from(manifest).join("target/debug/fedora-updater");
    assert!(
        p.exists(),
        "build fedora-updater first (cargo test builds it)"
    );
    p
}

fn spawn_helper() -> std::process::Child {
    let _guard = FORK_WRITE_LOCK.read().unwrap();
    Command::new(updater_bin())
        .arg(HELPER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fedora-updater --helper")
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let _guard = FORK_WRITE_LOCK.write().unwrap();
    std::fs::write(&path, body).expect("write fake helper");
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake helper");
    path
}

fn read_until_end(stdout: &mut BufReader<std::process::ChildStdout>) -> i32 {
    let mut line = String::new();
    loop {
        line.clear();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "helper closed before __END__");
        if let Some(code) = parse_end_line(line.trim_end()) {
            return code;
        }
    }
}

#[test]
fn helper_hello_quit() {
    assert_eq!(HELPER_FLAG, "--helper");
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
    assert_eq!(read_until_end(&mut stdout), 2);

    writeln!(stdin, "QUIT").unwrap();
    let _ = child.wait();
}

#[test]
fn helper_rejects_extra_cli_args_with_exit_2() {
    for extra in [&["extra-arg"][..], &["--another", "arg"][..]] {
        let _guard = FORK_WRITE_LOCK.read().unwrap();
        let out = Command::new(updater_bin())
            .arg(HELPER_FLAG)
            .args(extra)
            .stdin(Stdio::null())
            .output()
            .expect("run fedora-updater --helper with extra args");
        assert_eq!(out.status.code(), Some(2), "extra args={extra:?}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.is_empty() && !stdout.contains(HELLO_LINE),
            "helper must not emit HELLO_LINE on bad args: {stdout:?}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("takes no extra arguments"),
            "expected usage error on stderr, got {stderr:?}"
        );
    }
}

#[test]
fn helper_handles_malformed_requests_and_recovers_to_quit() {
    let mut child = spawn_helper();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut hello = String::new();
    stdout.read_line(&mut hello).unwrap();
    assert_eq!(hello.trim_end(), HELLO_LINE);

    for bad in [
        "",
        "RUN ../evil",
        "RUN check-dnf; id",
        "RUN check-dnf extra",
        "EXEC check-dnf",
    ] {
        writeln!(stdin, "{bad}").unwrap();
        stdin.flush().unwrap();
        assert_eq!(
            read_until_end(&mut stdout),
            2,
            "expected __END__ 2 for malformed request {bad:?}"
        );
    }

    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    let mut bye = String::new();
    stdout.read_line(&mut bye).unwrap();
    assert_eq!(bye.trim_end(), BYE_LINE);

    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
#[ignore = "requires live dnf and network"]
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
    let _ = writeln!(stdin, "QUIT");
    let _ = child.kill();
    let _ = child.wait();
}

/// Returns true if `pid` no longer exists or is an unreaped zombie/dead task.
fn process_is_dead_or_zombie(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == -1 {
        return true;
    }
    // In containers whose PID 1 does not reap orphans (e.g. GitHub Actions
    // running `tail -f /dev/null`), a SIGKILLed grandchild remains a zombie
    // (`Z`) for which `kill(pid, 0)` still returns 0.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rfind(')')
            .and_then(|idx| stat[idx + 1..].split_whitespace().next())
            .is_some_and(|state| matches!(state, "Z" | "X" | "x")),
        Err(_) => unsafe { libc::kill(pid, 0) == -1 },
    }
}

/// Window close runs `close_session` on the GTK thread. If a helper command is
/// stuck (fwupdmgr waiting on "Restart now?"), that must not freeze the UI and
/// must kill grandchildren in the helper's process group.
#[test]
fn close_session_does_not_block_when_helper_command_hangs() {
    let _lock = SESSION_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _cleanup = CloseSessionGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        dir.path(),
        "hung-helper",
        r#"#!/bin/sh
printf '%s\n' '__READY__ fedora-updater-helper'
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    QUIT)
      printf '%s\n' '__BYE__'
      exit 0
      ;;
    RUN*)
      sleep 3600 &
      child_pid=$!
      printf 'child-pid=%s\n' "$child_pid"
      wait
      ;;
  esac
done
"#,
    );

    let (started_tx, started_rx) = mpsc::channel::<Result<i32, String>>();
    let script_for_worker = script.clone();
    let worker = thread::spawn(move || {
        let mut notified = false;
        let res = with_direct_session_for_test(&script_for_worker, |session| {
            session.run_command(HelperCommand::CheckDnf, |ev| {
                if !notified {
                    if let SessionEvent::Line(ref line) = ev {
                        if let Some(pid_str) = line.strip_prefix("child-pid=") {
                            if let Ok(pid) = pid_str.parse::<i32>() {
                                notified = true;
                                let _ = started_tx.send(Ok(pid));
                            }
                        }
                    }
                }
            })
        });
        if !notified {
            let _ = started_tx.send(Err(format!("{res:?}")));
        }
    });

    let grandchild_pid = started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("hung-helper worker timed out or disconnected")
        .expect("hung-helper did not start running the command");

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

    let mut grandchild_dead = false;
    for _ in 0..100 {
        if process_is_dead_or_zombie(grandchild_pid) {
            grandchild_dead = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        grandchild_dead,
        "grandchild pid {grandchild_pid} survived kill_recorded_helper"
    );
}

#[test]
fn pool_public_api_reuses_and_recovers_with_direct_session() {
    let _lock = SESSION_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _cleanup = CloseSessionGuard::new();

    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        dir.path(),
        "pool-api-helper",
        r#"#!/bin/sh
printf '%s\n' '__READY__ fedora-updater-helper'
while IFS= read -r line; do
  case "$line" in
    'RUN check-dnf')
      printf 'pid=%s\n' "$$"
      printf '__DRAW__ progress 50%%\n'
      printf 'done\n'
      printf '__END__ 0\n'
      ;;
    'RUN apply-dnf')
      exit 1
      ;;
    QUIT)
      printf '__BYE__\n'
      exit 0
      ;;
  esac
done
"#,
    );

    assert!(!session_is_alive());

    let mut events1 = Vec::new();
    let code1 = with_direct_session_for_test(&script, |session| {
        session.run_command(HelperCommand::CheckDnf, |ev| events1.push(ev))
    })
    .expect("call 1");
    assert_eq!(code1, 0);
    assert!(session_is_alive());

    let pid1 = events1
        .iter()
        .find_map(|ev| match ev {
            SessionEvent::Line(l) => l.strip_prefix("pid=").map(str::to_string),
            _ => None,
        })
        .expect("pid1");
    assert!(events1
        .iter()
        .any(|ev| matches!(ev, SessionEvent::Line(l) if l == "__DRAW__ progress 50%")));
    assert!(events1
        .iter()
        .any(|ev| matches!(ev, SessionEvent::Line(l) if l == "done")));
    assert!(matches!(
        events1.last(),
        Some(SessionEvent::Finished { exit_code: 0 })
    ));

    let pid2 = with_direct_session_for_test(&script, |session| {
        let mut pid = None;
        let code = session.run_command(HelperCommand::CheckDnf, |ev| {
            if let SessionEvent::Line(l) = ev {
                if let Some(p) = l.strip_prefix("pid=") {
                    pid = Some(p.to_string());
                }
            }
        })?;
        assert_eq!(code, 0);
        Ok(pid.expect("pid2"))
    })
    .expect("call 2");
    assert_eq!(pid1, pid2, "public pool API must reuse helper process");

    let err = with_direct_session_for_test(&script, |session| {
        session.run_command(HelperCommand::ApplyDnf, |_| {})
    });
    assert!(matches!(err, Err(SessionError::Closed)));
    assert!(!session_is_alive());

    let pid3 = with_direct_session_for_test(&script, |session| {
        let mut pid = None;
        let code = session.run_command(HelperCommand::CheckDnf, |ev| {
            if let SessionEvent::Line(l) = ev {
                if let Some(p) = l.strip_prefix("pid=") {
                    pid = Some(p.to_string());
                }
            }
        })?;
        assert_eq!(code, 0);
        Ok(pid.expect("pid3"))
    })
    .expect("call 4 respawn");
    assert_ne!(pid3, pid1, "pool must respawn fresh helper after eviction");

    close_session();
    assert!(!session_is_alive());
}
