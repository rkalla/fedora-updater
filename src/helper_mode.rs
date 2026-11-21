//! Privileged helper mode — allowlisted CLI only.
//!
//! Invoked as: `fedora-updater --helper` (typically via `pkexec`).
//! Protocol: see `helper_protocol`.
//!
//! Child stdout/stderr are drained concurrently (avoids pipe deadlocks).
//! `stdbuf -o0 -e0` turns off block buffering. dnf5 redraws a download with
//! `\r` and only writes `\n` at 100%, so line buffering never flushes it.

use std::io::{self, BufRead, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::helper_protocol::{
    format_end_line, ClientRequest, HelperCommand, BYE_LINE, DRAW_PREFIX, HELLO_LINE,
};

const STDBUF_PATH: &str = "/usr/bin/stdbuf";
/// Fully unbuffered stdio. `-oL` flushes on `\n` only, which hides `\r` updates.
const STDBUF_ARGS: &[&str] = &["-o0", "-e0"];
const POST_EXIT_DRAIN_GRACE: Duration = Duration::from_millis(300);
const POST_EXIT_DRAIN_MAX: Duration = Duration::from_secs(1);

enum HelperPump {
    Piece(StreamPiece),
    StreamDone,
    ChildExited(i32),
}

/// One piece of a child stream. `\r` is live; `\n` and `\r\n` are committed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamPiece {
    Live(String),
    Line(String),
}

/// Split a byte stream on `\r` and `\n` without waiting for a full line.
/// A trailing `\r` is held so a following `\n` stays one committed line.
struct StreamSplitter {
    pending: Vec<u8>,
}

impl StreamSplitter {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Vec<StreamPiece> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(idx) = self.pending.iter().position(|b| *b == b'\n' || *b == b'\r') {
            let sep = self.pending[idx];
            if sep == b'\r' && idx + 1 == self.pending.len() {
                break;
            }
            let next_is_lf = sep == b'\r' && self.pending.get(idx + 1) == Some(&b'\n');
            let body: Vec<u8> = self.pending.drain(..idx).collect();
            self.pending.drain(..1);
            if next_is_lf {
                self.pending.drain(..1);
            }
            let text = String::from_utf8_lossy(&body).into_owned();
            if text.is_empty() {
                continue;
            }
            if sep == b'\r' && !next_is_lf {
                out.push(StreamPiece::Live(text));
            } else {
                out.push(StreamPiece::Line(text));
            }
        }
        out
    }

    fn finish(&mut self) -> Option<StreamPiece> {
        if self.pending.is_empty() {
            return None;
        }
        let mut bytes = std::mem::take(&mut self.pending);
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if text.is_empty() {
            None
        } else {
            Some(StreamPiece::Line(text))
        }
    }
}

/// Entry point for multi-call helper mode. Does not return (exits process).
pub fn run() -> ! {
    let code = run_inner();
    std::process::exit(code);
}

fn run_inner() -> i32 {
    // After `--helper`, no further user args are accepted.
    let extra: Vec<String> = std::env::args().skip(2).collect();
    if !extra.is_empty() {
        eprintln!("fedora-updater --helper takes no extra arguments (got {extra:?})");
        return 2;
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();

    let _ = writeln!(stdout, "{HELLO_LINE}");
    let _ = stdout.flush();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                let _ = writeln!(stderr, "read error: {e}");
                break;
            }
        };

        match ClientRequest::parse(&line) {
            Ok(ClientRequest::Quit) => {
                let _ = writeln!(stdout, "{BYE_LINE}");
                let _ = stdout.flush();
                break;
            }
            Ok(ClientRequest::Run(cmd)) => {
                let code = run_allowlisted(cmd, &mut stdout, &mut stderr);
                let _ = writeln!(stdout, "{}", format_end_line(code));
                let _ = stdout.flush();
            }
            Err(e) => {
                let _ = writeln!(stderr, "reject: {e}");
                let _ = writeln!(stdout, "{}", format_end_line(2));
                let _ = stdout.flush();
            }
        }
    }
    0
}

fn run_allowlisted(cmd: HelperCommand, stdout: &mut impl Write, stderr: &mut impl Write) -> i32 {
    let argv = cmd.argv();
    let program = argv[0];
    let args = &argv[1..];

    let _ = writeln!(stderr, "helper: running {} {:?}", program, args);

    let child = match spawn_unbuffered(program, args) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(stdout, "failed to spawn {program}: {e}");
            return 127;
        }
    };

    drain_child(child, stdout)
}

fn emit_piece(stdout: &mut impl Write, piece: StreamPiece) {
    match piece {
        StreamPiece::Live(text) => {
            let _ = writeln!(stdout, "{DRAW_PREFIX}{text}");
        }
        StreamPiece::Line(text) => {
            let _ = writeln!(stdout, "{text}");
        }
    }
    let _ = stdout.flush();
}

fn drain_child(mut child: std::process::Child, stdout: &mut impl Write) -> i32 {
    let child_out = child.stdout.take();
    let child_err = child.stderr.take();

    let (tx, rx) = mpsc::channel::<HelperPump>();
    let mut open_streams = 0usize;

    let t_out = child_out.map(|out| {
        open_streams += 1;
        let tx_out = tx.clone();
        thread::spawn(move || {
            drain_stream(out, &tx_out);
            let _ = tx_out.send(HelperPump::StreamDone);
        })
    });
    let t_err = child_err.map(|err| {
        open_streams += 1;
        let tx_err = tx.clone();
        thread::spawn(move || {
            drain_stream(err, &tx_err);
            let _ = tx_err.send(HelperPump::StreamDone);
        })
    });
    let tx_wait = tx.clone();
    let t_wait = thread::spawn(move || {
        let code = match child.wait() {
            Ok(status) => status.code().unwrap_or(-1),
            Err(_) => -1,
        };
        let _ = tx_wait.send(HelperPump::ChildExited(code));
    });
    drop(tx);

    let mut exit_code: Option<i32> = None;
    let mut max_deadline: Option<Instant> = None;

    while exit_code.is_none() {
        match rx.recv() {
            Ok(HelperPump::Piece(piece)) => emit_piece(stdout, piece),
            Ok(HelperPump::StreamDone) => {
                open_streams = open_streams.saturating_sub(1);
            }
            Ok(HelperPump::ChildExited(code)) => {
                exit_code = Some(code);
                max_deadline = Some(Instant::now() + POST_EXIT_DRAIN_MAX);
                if open_streams == 0 {
                    break;
                }
            }
            Err(_) => {
                open_streams = 0;
                break;
            }
        }
    }

    if exit_code.is_some() {
        while open_streams > 0 {
            let remaining_max = max_deadline
                .expect("max_deadline set when exit_code is Some")
                .saturating_duration_since(Instant::now());
            if remaining_max.is_zero() {
                break;
            }
            let wait = POST_EXIT_DRAIN_GRACE.min(remaining_max);
            match rx.recv_timeout(wait) {
                Ok(HelperPump::Piece(piece)) => emit_piece(stdout, piece),
                Ok(HelperPump::StreamDone) => {
                    open_streams = open_streams.saturating_sub(1);
                }
                Ok(HelperPump::ChildExited(_)) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    open_streams = 0;
                    break;
                }
            }
        }
    }

    let _ = stdout.flush();
    let _ = t_wait.join();
    if open_streams == 0 {
        if let Some(h) = t_out {
            let _ = h.join();
        }
        if let Some(h) = t_err {
            let _ = h.join();
        }
    }

    exit_code.unwrap_or(-1)
}

fn drain_stream(mut reader: impl Read, tx: &mpsc::Sender<HelperPump>) {
    let mut splitter = StreamSplitter::new();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                for piece in splitter.push(&buf[..n]) {
                    if tx.send(HelperPump::Piece(piece)).is_err() {
                        return;
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if let Some(piece) = splitter.finish() {
        let _ = tx.send(HelperPump::Piece(piece));
    }
}

fn spawn_unbuffered(program: &str, args: &[&str]) -> io::Result<std::process::Child> {
    let mut cmd = if stdbuf_available() {
        let mut cmd = Command::new(STDBUF_PATH);
        cmd.args(STDBUF_ARGS);
        cmd.arg(program);
        cmd.args(args);
        cmd
    } else {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    };
    // Never inherit the helper protocol pipe — interactive tools (fwupdmgr
    // "Restart now?") would steal stdin and deadlock the session.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PYTHONUNBUFFERED", "1");
    // If the helper is SIGKILL'd on window close, grandchildren must die too.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
            if libc::getppid() == 1 {
                libc::_exit(128 + libc::SIGKILL);
            }
            Ok(())
        });
    }
    cmd.spawn()
}

fn stdbuf_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new(STDBUF_PATH)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::{drain_child, StreamPiece, StreamSplitter, STDBUF_ARGS};
    use crate::helper_protocol::HelperCommand;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    struct ProcessGroupGuard(i32);

    impl Drop for ProcessGroupGuard {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }

    #[test]
    fn carriage_return_is_live_and_crlf_is_one_line() {
        let mut splitter = StreamSplitter::new();
        assert!(splitter.push(b"hello").is_empty());
        assert_eq!(
            splitter.push(b"\r"),
            Vec::<StreamPiece>::new(),
            "a trailing CR waits so CRLF is not announced twice"
        );
        assert_eq!(
            splitter.push(b"\n[1/2] grok-bot 40%\r"),
            vec![StreamPiece::Line("hello".into())]
        );
        assert_eq!(
            splitter.push(b"[1/2] grok-bot 100%\n"),
            vec![
                StreamPiece::Live("[1/2] grok-bot 40%".into()),
                StreamPiece::Line("[1/2] grok-bot 100%".into()),
            ]
        );
        assert!(splitter.finish().is_none());
    }

    #[test]
    fn finish_commits_a_held_carriage_return() {
        let mut splitter = StreamSplitter::new();
        assert!(splitter.push(b"[1/2] grok-bot 40%\r").is_empty());
        assert_eq!(
            splitter.finish(),
            Some(StreamPiece::Line("[1/2] grok-bot 40%".into()))
        );
    }

    #[test]
    fn empty_segments_are_skipped() {
        let mut splitter = StreamSplitter::new();
        assert!(splitter.push(b"\n\r\n").is_empty());
        assert_eq!(splitter.push(b"ok\n"), vec![StreamPiece::Line("ok".into())]);
    }

    #[test]
    fn stdbuf_disables_block_buffering() {
        assert_eq!(STDBUF_ARGS, ["-o0", "-e0"]);
        assert!(!STDBUF_ARGS.contains(&"-oL"));
    }

    #[test]
    fn only_allowlisted_argv() {
        let argv = HelperCommand::CheckDnf.argv();
        assert_eq!(argv, &["/usr/bin/dnf", "check-update", "--refresh"]);
        let argv = HelperCommand::ApplyDnf.argv();
        assert_eq!(argv, &["/usr/bin/dnf", "update", "-y"]);
        let argv = HelperCommand::ApplyFwupd.argv();
        assert!(argv.contains(&"--no-reboot-check"));
        assert!(argv.contains(&"-y"));
    }

    #[test]
    fn leaked_grandchild_fd_does_not_hang_drain_child() {
        let child = Command::new("/bin/sh")
            .args(["-c", "echo hi; sleep 60 &"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("spawn /bin/sh");
        let _guard = ProcessGroupGuard(child.id() as i32);

        let started = Instant::now();
        let mut out = Vec::new();
        let code = drain_child(child, &mut out);
        let elapsed = started.elapsed();

        assert_eq!(code, 0);
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.lines().any(|l| l == "hi"),
            "expected 'hi' in output, got {text:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "drain_child took too long ({elapsed:?}) with leaked grandchild FD"
        );
    }

    #[test]
    fn drain_child_returns_exit_code_and_both_streams_normally() {
        let child = Command::new("/bin/sh")
            .args(["-c", "echo out_line; echo err_line >&2; exit 7"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("spawn /bin/sh");
        let _guard = ProcessGroupGuard(child.id() as i32);

        let mut out = Vec::new();
        let code = drain_child(child, &mut out);

        assert_eq!(code, 7);
        let text = String::from_utf8_lossy(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.contains(&"out_line"), "missing out_line in {lines:?}");
        assert!(lines.contains(&"err_line"), "missing err_line in {lines:?}");
    }
}
