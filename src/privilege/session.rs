//! Client for the multi-call helper mode over stdin/stdout after a single `pkexec`.
//!
//! One polkit authentication starts: `pkexec <this-binary> --helper`
//! Subsequent allowlisted commands reuse that process for the whole GUI lifetime
//! (no extra password prompts until the app closes).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::helper_protocol::{parse_end_line, HelperCommand, HELLO_LINE};

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
    if let Ok(p) = std::env::var("FEDORA_UPDATER_HELPER") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if exe.exists() {
            return exe;
        }
    }
    let usr = PathBuf::from("/usr/bin/fedora-updater");
    if usr.exists() {
        return usr;
    }
    PathBuf::from("fedora-updater")
}

/// Long-lived privileged session (`pkexec <exe> --helper` or direct for tests).
pub struct PrivilegedSession {
    child: Child,
    stdin: ChildStdin,
    reader: Arc<Mutex<BufReader<std::process::ChildStdout>>>,
    elevated: bool,
}

impl PrivilegedSession {
    /// Start helper via `pkexec <self> --helper`, or directly when
    /// `FEDORA_UPDATER_HELPER_DIRECT=1` / `FEDORA_UPDATER_TEST_MODE=1`.
    pub fn start() -> Result<Self, SessionError> {
        let path = helper_path();
        let direct = std::env::var_os("FEDORA_UPDATER_HELPER_DIRECT").is_some()
            || std::env::var_os("FEDORA_UPDATER_TEST_MODE").is_some();

        let mut child = if direct {
            Command::new(&path)
                .arg(HELPER_FLAG)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        } else {
            Command::new("pkexec")
                .arg(&path)
                .arg(HELPER_FLAG)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
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

        Ok(Self {
            child,
            stdin,
            reader: Arc::new(Mutex::new(reader)),
            elevated: !direct,
        })
    }

    pub fn is_elevated(&self) -> bool {
        self.elevated
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

        let mut reader = self
            .reader
            .lock()
            .map_err(|_| SessionError::Io("reader lock poisoned".into()))?;

        loop {
            let mut line = String::new();
            let n = reader
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

    pub fn quit(mut self) -> Result<(), SessionError> {
        let _ = writeln!(self.stdin, "QUIT");
        let _ = self.stdin.flush();
        if let Ok(mut reader) = self.reader.lock() {
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
        }
        let _ = self.child.wait();
        Ok(())
    }
}

impl Drop for PrivilegedSession {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "QUIT");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    if let Some(out) = stdout {
        for line in BufReader::new(out).lines().flatten() {
            on_line(line);
        }
    }
    if let Some(err) = stderr {
        for line in BufReader::new(err).lines().flatten() {
            on_line(line);
        }
    }
    let status = child.wait().map_err(|e| SessionError::Io(e.to_string()))?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_path_respects_env() {
        std::env::set_var("FEDORA_UPDATER_HELPER", "/tmp/fake-updater-xyz");
        assert_eq!(helper_path(), PathBuf::from("/tmp/fake-updater-xyz"));
        std::env::remove_var("FEDORA_UPDATER_HELPER");
    }

    #[test]
    fn helper_flag_is_stable() {
        assert_eq!(HELPER_FLAG, "--helper");
    }
}
