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

        let mut child = if direct {
            let mut cmd = Command::new(&path);
            cmd.arg(HELPER_FLAG);
            configure_helper_command(&mut cmd).spawn()
        } else {
            let mut cmd = Command::new("pkexec");
            cmd.arg(&path).arg(HELPER_FLAG);
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
            let _ = child.try_wait();
            return Err(SessionError::AuthDenied);
        }
        if hello != HELLO_LINE && !hello.contains("fedora-updater-helper") {
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
    for line in BufReader::new(stream).lines().flatten() {
        if tx.send(line).is_err() {
            break;
        }
    }
}

/// Run an unprivileged host command (Flatpak user ops, etc.), streaming lines.
pub fn run_local_command(
    program: &str,
    args: &[&str],
    mut on_line: impl FnMut(String),
) -> Result<i32, SessionError> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| SessionError::Spawn(format!("{program}: {e}")))?;

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
            .recv_timeout(std::time::Duration::from_secs(5))
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
    fn helper_flag_is_stable() {
        assert_eq!(HELPER_FLAG, "--helper");
    }
}
