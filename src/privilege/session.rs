//! Client for the multi-call helper mode over stdin/stdout after a single `pkexec`.
//!
//! One polkit authentication starts: `pkexec <this-binary> --helper`
//! Subsequent allowlisted commands reuse that process for the whole GUI lifetime
//! (no extra password prompts until the app closes).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;

use crate::helper_protocol::{parse_end_line, HelperCommand, HELLO_LINE};

/// Helper (or pkexec) pid so window close can SIGKILL without the session lock.
static HELPER_PID: AtomicU32 = AtomicU32::new(0);
/// Active cancellable unprivileged command pid (e.g. `flatpak update`) so Stop / close can SIGKILL it.
static LOCAL_CMD_PID: AtomicU32 = AtomicU32::new(0);

/// Flag that selects helper mode on the multi-call binary.
pub const HELPER_FLAG: &str = "--helper";

#[derive(Debug, Clone)]
pub enum SessionError {
    HelperNotFound(String),
    Spawn(String),
    Io(String),
    Closed,
    AuthDenied,
    Protocol(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HelperNotFound(p) => write!(f, "helper binary not found ({p})"),
            Self::Spawn(e) => write!(f, "failed to start helper: {e}"),
            Self::Io(e) => write!(f, "helper I/O error: {e}"),
            Self::Closed => write!(f, "helper closed unexpectedly"),
            Self::AuthDenied => write!(f, "authentication cancelled or denied"),
            Self::Protocol(e) => write!(f, "helper protocol error: {e}"),
        }
    }
}

impl std::error::Error for SessionError {}

/// Events while a single helper command runs.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Line(String),
    Finished { exit_code: i32 },
}

/// Path of the multi-call binary used for helper mode.
///
/// Order:
/// 1. `FEDORA_UPDATER_HELPER` — explicit override (path to the same multi-call binary)
/// 2. `current_exe()` — normal case (GUI pkexec's itself with `--helper`)
/// 3. `/usr/bin/fedora-updater` — system install
pub fn helper_path() -> PathBuf {
    let override_path = std::env::var_os("FEDORA_UPDATER_HELPER").map(PathBuf::from);
    let current_exe = std::env::current_exe().ok().filter(|p| p.exists());
    let usr_exists = Path::new("/usr/bin/fedora-updater").exists();
    helper_path_with(override_path, current_exe, usr_exists)
}

fn helper_path_with(
    override_path: Option<PathBuf>,
    current_exe: Option<PathBuf>,
    usr_exists: bool,
) -> PathBuf {
    if let Some(p) = override_path {
        return p;
    }
    if let Some(exe) = current_exe {
        return exe;
    }
    if usr_exists {
        return PathBuf::from("/usr/bin/fedora-updater");
    }
    PathBuf::from("fedora-updater")
}

/// Long-lived privileged session (`pkexec <exe> --helper` or direct for tests).
pub struct PrivilegedSession {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl PrivilegedSession {
    /// Start helper via `pkexec <self> --helper`, or directly when
    /// `FEDORA_UPDATER_HELPER_DIRECT=1` / `FEDORA_UPDATER_TEST_MODE=1`.
    pub fn start() -> Result<Self, SessionError> {
        let path = helper_path();
        let direct = std::env::var_os("FEDORA_UPDATER_HELPER_DIRECT").is_some()
            || std::env::var_os("FEDORA_UPDATER_TEST_MODE").is_some();
        Self::start_with(&path, direct)
    }

    /// Start a helper binary or script directly without `pkexec` (for tests).
    pub fn start_direct(path: &Path) -> Result<Self, SessionError> {
        Self::start_with(path, true)
    }

    fn start_with(path: &Path, direct: bool) -> Result<Self, SessionError> {
        let mut child = if direct {
            let mut cmd = Command::new(path);
            cmd.arg(HELPER_FLAG);
            configure_helper_command(&mut cmd).spawn()
        } else {
            let mut cmd = Command::new("pkexec");
            cmd.arg(path).arg(HELPER_FLAG);
            configure_helper_command(&mut cmd).spawn()
        }
        .map_err(|e| {
            if !path.exists() && path.is_relative() {
                SessionError::HelperNotFound(path.display().to_string())
            } else {
                SessionError::Spawn(format!("{e} ({})", path.display()))
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SessionError::Spawn("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SessionError::Spawn("no stdout".into()))?;

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for _ in reader.lines() {}
            });
        }

        let mut reader = BufReader::new(stdout);
        let mut hello = String::new();
        reader
            .read_line(&mut hello)
            .map_err(|e| SessionError::Io(e.to_string()))?;
        let hello = hello.trim_end();
        if hello.is_empty() {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(SessionError::AuthDenied);
        }
        if hello != HELLO_LINE && !hello.contains("fedora-updater-helper") {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(SessionError::Protocol(format!("unexpected hello: {hello}")));
        }

        HELPER_PID.store(child.id(), Ordering::SeqCst);

        Ok(Self {
            child,
            stdin,
            reader,
        })
    }

    /// True while the helper child is still running (not exited/reaped).
    pub fn is_running(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => false,
        }
    }

    pub fn run_command<F>(
        &mut self,
        cmd: HelperCommand,
        mut on_event: F,
    ) -> Result<i32, SessionError>
    where
        F: FnMut(SessionEvent),
    {
        let req = cmd.encode_run();
        writeln!(self.stdin, "{req}").map_err(|e| SessionError::Io(e.to_string()))?;
        self.stdin
            .flush()
            .map_err(|e| SessionError::Io(e.to_string()))?;

        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| SessionError::Io(e.to_string()))?;
            if n == 0 {
                return Err(SessionError::Closed);
            }
            let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
            if let Some(code) = parse_end_line(&trimmed) {
                on_event(SessionEvent::Finished { exit_code: code });
                return Ok(code);
            }
            on_event(SessionEvent::Line(trimmed));
        }
    }

    /// SIGKILL the helper. Does not wait for protocol BYE or a stuck `read_line`.
    pub fn abort(self) {
        drop(self);
    }

    fn kill_and_clear(&mut self) {
        let pid = self.child.id();
        let _ = self.child.kill();
        let _ = self.child.try_wait();
        HELPER_PID
            .compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst)
            .ok();
    }
}

/// GNOME Software's dnf daemon. A silent dnf can be waiting on its rpm database.
/// This only looks. It never signals the daemon.
pub fn dnf_daemon_is_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        if comm.trim() == "dnf5daemon-server" {
            return true;
        }
    }
    false
}

/// SIGKILL the helper process group. Safe from the GTK thread: no locks, no wait.
pub fn kill_recorded_helper() {
    let pid = HELPER_PID.swap(0, Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let pid = pid as i32;
    unsafe {
        // process_group(0) at spawn makes this pid the group leader, so
        // grandchildren (fwupdmgr) die too.
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
}

/// SIGKILL the active cancellable local command's process group (e.g. `flatpak update`).
/// Safe from the GTK thread: no locks, no wait.
pub fn kill_recorded_local_command() {
    let pid = LOCAL_CMD_PID.swap(0, Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let pid = pid as i32;
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
}

struct LocalPidGuard(Option<u32>);

impl Drop for LocalPidGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            LOCAL_CMD_PID
                .compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst)
                .ok();
        }
    }
}

fn configure_helper_command(cmd: &mut Command) -> &mut Command {
    // Own process group so close can kill helper + fwupdmgr together.
    cmd.process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
}

impl Drop for PrivilegedSession {
    fn drop(&mut self) {
        self.kill_and_clear();
    }
}

fn drain_lines(stream: impl std::io::Read, tx: mpsc::Sender<String>) {
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        if tx.send(line).is_err() {
            break;
        }
    }
}

/// Run an unprivileged host command (`systemctl reboot`, etc.), streaming lines.
pub fn run_local_command(
    program: &str,
    args: &[&str],
    on_line: impl FnMut(String),
) -> Result<i32, SessionError> {
    run_local_command_inner(program, args, false, on_line)
}

/// Run an unprivileged host command whose process group is recorded so `Stop` or
/// window close can terminate it immediately via [`kill_recorded_local_command`].
pub fn run_cancellable_local_command(
    program: &str,
    args: &[&str],
    on_line: impl FnMut(String),
) -> Result<i32, SessionError> {
    run_local_command_inner(program, args, true, on_line)
}

fn run_local_command_inner(
    program: &str,
    args: &[&str],
    track_for_cancel: bool,
    mut on_line: impl FnMut(String),
) -> Result<i32, SessionError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if track_for_cancel {
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| SessionError::Spawn(format!("{program}: {e}")))?;

    let _pid_guard = if track_for_cancel {
        let pid = child.id();
        LOCAL_CMD_PID.store(pid, Ordering::SeqCst);
        Some(LocalPidGuard(Some(pid)))
    } else {
        None
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel::<String>();
    let tx_err = tx.clone();

    let t_out = std::thread::spawn(move || {
        if let Some(out) = stdout {
            drain_lines(out, tx);
        }
    });
    let t_err = std::thread::spawn(move || {
        if let Some(err) = stderr {
            drain_lines(err, tx_err);
        }
    });

    for line in rx {
        on_line(line);
    }

    let _ = t_out.join();
    let _ = t_err.join();

    let status = child.wait().map_err(|e| SessionError::Io(e.to_string()))?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper_protocol::DRAW_PREFIX;
    use crate::privilege::test_support::{
        start_direct_retry, write_fake_helper, PRIVILEGE_TEST_LOCK,
    };
    use std::time::Duration;

    #[test]
    fn helper_path_respects_env() {
        assert_eq!(
            helper_path_with(
                Some(PathBuf::from("/tmp/fake-updater-xyz")),
                Some(PathBuf::from("/some/exe")),
                true,
            ),
            PathBuf::from("/tmp/fake-updater-xyz")
        );
        assert_eq!(
            helper_path_with(None, Some(PathBuf::from("/some/exe")), true),
            PathBuf::from("/some/exe")
        );
        assert_eq!(
            helper_path_with(None, None, true),
            PathBuf::from("/usr/bin/fedora-updater")
        );
        assert_eq!(
            helper_path_with(None, None, false),
            PathBuf::from("fedora-updater")
        );
    }

    #[test]
    fn run_local_command_drains_large_stderr_without_deadlock() {
        let script = r#"
s="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
s="$s$s$s$s"
s="$s$s$s$s"
i=0
while [ "$i" -lt 160 ]; do
  printf 'stderr-%03d-%s\n' "$i" "$s" >&2
  i=$((i + 1))
done
printf 'stdout-done\n'
"#;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut lines = Vec::new();
            let res = run_local_command("/bin/sh", &["-c", script], |line| lines.push(line));
            let _ = tx.send((res, lines));
        });

        let (res, lines) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("run_local_command deadlocked with >128 KiB on stderr");
        let code = res.expect("run_local_command should succeed");
        assert_eq!(code, 0);
        assert!(lines.iter().any(|l| l == "stdout-done"));
        let stderr_lines: Vec<&String> = lines.iter().filter(|l| l.starts_with("stderr-")).collect();
        assert_eq!(stderr_lines.len(), 160);
        let stderr_bytes: usize = stderr_lines.iter().map(|l| l.len() + 1).sum();
        assert!(
            stderr_bytes > 128 * 1024,
            "expected >128 KiB of stderr, got {stderr_bytes}"
        );
    }

    #[test]
    fn run_local_command_handles_nonzero_missing_empty_and_large_both_streams() {
        // Non-zero exit preserves both streams and exact status code.
        let mut lines = Vec::new();
        let code = run_local_command(
            "/bin/sh",
            &["-c", "echo local-out; echo local-err >&2; exit 23"],
            |line| lines.push(line),
        )
        .expect("run_local_command /bin/sh exit 23");
        assert_eq!(code, 23);
        assert!(lines.contains(&"local-out".to_string()));
        assert!(lines.contains(&"local-err".to_string()));

        // Missing executable returns Spawn error.
        let missing = run_local_command(
            "/nonexistent-fedora-updater-test-bin-xyz",
            &[],
            |_| panic!("should not emit lines"),
        );
        assert!(matches!(missing, Err(SessionError::Spawn(_))));

        // Empty output with exit 0 emits zero lines.
        let mut empty_lines = Vec::new();
        let code = run_local_command("/bin/sh", &["-c", "exit 0"], |l| empty_lines.push(l))
            .expect("empty exit 0");
        assert_eq!(code, 0);
        assert!(empty_lines.is_empty());

        // Concurrent >128 KiB on BOTH stdout and stderr completes without deadlock.
        let script = r#"
s="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
s="$s$s$s$s"
s="$s$s$s$s"
(
  i=0
  while [ "$i" -lt 160 ]; do
    printf 'out-%03d-%s\n' "$i" "$s"
    i=$((i + 1))
  done
) &
(
  j=0
  while [ "$j" -lt 160 ]; do
    printf 'err-%03d-%s\n' "$j" "$s" >&2
    j=$((j + 1))
  done
) &
wait
"#;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut both = Vec::new();
            let res = run_local_command("/bin/sh", &["-c", script], |l| both.push(l));
            let _ = tx.send((res, both));
        });
        let (res, both) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("run_local_command deadlocked with >128 KiB on stdout+stderr");
        assert_eq!(res.expect("ok"), 0);
        let out_lines: Vec<&String> = both.iter().filter(|l| l.starts_with("out-")).collect();
        let err_lines: Vec<&String> = both.iter().filter(|l| l.starts_with("err-")).collect();
        assert_eq!(out_lines.len(), 160);
        assert_eq!(err_lines.len(), 160);
        let out_bytes: usize = out_lines.iter().map(|l| l.len() + 1).sum();
        let err_bytes: usize = err_lines.iter().map(|l| l.len() + 1).sum();
        assert!(out_bytes > 128 * 1024, "stdout {out_bytes} <= 128 KiB");
        assert!(err_bytes > 128 * 1024, "stderr {err_bytes} <= 128 KiB");
    }

    #[test]
    fn start_direct_empty_stdout_returns_auth_denied() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = write_fake_helper(dir.path(), "deny-helper", "#!/bin/sh\nexit 1\n");
        let res = start_direct_retry(&script);
        assert!(
            matches!(res, Err(SessionError::AuthDenied)),
            "expected AuthDenied on empty stdout, got {:?}",
            res.err()
        );
    }

    #[test]
    fn start_direct_unexpected_hello_returns_protocol_error() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = write_fake_helper(
            dir.path(),
            "bad-hello-helper",
            "#!/bin/sh\nprintf 'WRONG HELLO\\n'\nsleep 30\n",
        );
        let res = start_direct_retry(&script);
        assert!(
            matches!(res, Err(SessionError::Protocol(ref msg)) if msg.contains("WRONG HELLO")),
            "expected Protocol error mentioning WRONG HELLO, got {:?}",
            res.err()
        );
    }

    #[test]
    fn start_direct_missing_paths_report_helper_not_found_or_spawn() {
        let rel = PrivilegedSession::start_direct(Path::new("nonexistent-fedora-updater-helper-xyz"));
        assert!(
            matches!(rel, Err(SessionError::HelperNotFound(_))),
            "expected HelperNotFound for relative path, got {:?}",
            rel.err()
        );
        let abs = PrivilegedSession::start_direct(Path::new(
            "/nonexistent-fedora-updater-helper-dir/bin",
        ));
        assert!(
            matches!(abs, Err(SessionError::Spawn(_))),
            "expected Spawn for missing absolute path, got {:?}",
            abs.err()
        );
    }

    #[test]
    fn session_reuses_single_helper_and_streams_draw_and_committed_lines() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "#!/bin/sh\n\
printf '%s\\n' '{HELLO_LINE}'\n\
while IFS= read -r line; do\n\
  case \"$line\" in\n\
    'RUN check-dnf')\n\
      printf 'pid=%s\\n' \"$$\"\n\
      printf '{DRAW_PREFIX}dl 50%%\\n'\n\
      printf 'committed-line\\n'\n\
      printf '__END__ 100\\n'\n\
      ;;\n\
    'RUN check-fwupd')\n\
      printf 'pid=%s\\n' \"$$\"\n\
      printf 'fwupd-ok\\n'\n\
      printf '__END__ 0\\n'\n\
      ;;\n\
    QUIT)\n\
      printf '__BYE__\\n'\n\
      exit 0\n\
      ;;\n\
  esac\n\
done\n"
        );
        let script = write_fake_helper(dir.path(), "reuse-helper", &body);
        let mut session = start_direct_retry(&script).expect("start_direct");
        assert!(session.is_running());

        let mut events1 = Vec::new();
        let code1 = session
            .run_command(HelperCommand::CheckDnf, |ev| events1.push(ev))
            .expect("first run_command");
        assert_eq!(code1, 100);
        assert!(session.is_running());

        let lines1: Vec<&str> = events1
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Line(l) => Some(l.as_str()),
                _ => None,
            })
            .collect();
        let pid1 = lines1
            .iter()
            .find_map(|l| l.strip_prefix("pid="))
            .expect("pid= line in first call")
            .to_string();
        assert_eq!(
            lines1,
            vec![
                format!("pid={pid1}").as_str(),
                "__DRAW__ dl 50%",
                "committed-line"
            ]
        );
        assert!(matches!(
            events1.last(),
            Some(SessionEvent::Finished { exit_code: 100 })
        ));

        let mut events2 = Vec::new();
        let code2 = session
            .run_command(HelperCommand::CheckFwupd, |ev| events2.push(ev))
            .expect("second run_command");
        assert_eq!(code2, 0);
        assert!(session.is_running());

        let pid2 = events2
            .iter()
            .find_map(|e| match e {
                SessionEvent::Line(l) => l.strip_prefix("pid="),
                _ => None,
            })
            .expect("pid= line in second call");
        assert_eq!(pid1, pid2, "second command must reuse the same helper PID");
        assert!(events2
            .iter()
            .any(|e| matches!(e, SessionEvent::Line(l) if l == "fwupd-ok")));
        assert!(matches!(
            events2.last(),
            Some(SessionEvent::Finished { exit_code: 0 })
        ));
    }

    #[test]
    fn session_mid_command_eof_returns_closed_and_marks_not_running() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "#!/bin/sh\n\
printf '%s\\n' '{HELLO_LINE}'\n\
IFS= read -r _line\n\
printf 'partial-line\\n'\n\
exit 1\n"
        );
        let script = write_fake_helper(dir.path(), "eof-helper", &body);
        let mut session = start_direct_retry(&script).expect("start_direct");

        let mut lines = Vec::new();
        let res = session.run_command(HelperCommand::CheckDnf, |ev| {
            if let SessionEvent::Line(l) = ev {
                lines.push(l);
            }
        });
        assert!(
            matches!(res, Err(SessionError::Closed)),
            "expected SessionError::Closed, got {res:?}"
        );
        assert_eq!(lines, vec!["partial-line".to_string()]);

        for _ in 0..50 {
            if !session.is_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !session.is_running(),
            "session should report not running after unexpected EOF"
        );
    }

    #[test]
    fn kill_recorded_local_command_terminates_running_local_process() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            let res = run_cancellable_local_command(
                "/bin/sh",
                &["-c", "printf 'ready\\n'; sleep 60"],
                |line| {
                    if line == "ready" {
                        let _ = started_tx.send(());
                    }
                },
            );
            let _ = done_tx.send(res);
        });

        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("local process did not emit ready");

        kill_recorded_local_command();

        let res = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("kill_recorded_local_command did not stop the local process promptly");
        assert!(res.is_ok(), "unexpected error: {res:?}");
        assert_ne!(res.unwrap(), 0);
        worker.join().expect("worker join");
        assert_eq!(LOCAL_CMD_PID.load(Ordering::SeqCst), 0);
    }
}
