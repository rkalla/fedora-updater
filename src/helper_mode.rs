//! Privileged helper mode — allowlisted CLI only.
//!
//! Invoked as: `fedora-updater --helper` (typically via `pkexec`).
//! Protocol: see `helper_protocol`.
//!
//! Child stdout/stderr are drained concurrently (avoids pipe deadlocks) and
//! preferably line-buffered via `stdbuf` so the GUI sees live progress.

use std::io::{self, BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use crate::helper_protocol::{format_end_line, ClientRequest, HelperCommand, BYE_LINE, HELLO_LINE};

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

    let mut child = match spawn_line_buffered(program, args) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(stdout, "failed to spawn {program}: {e}");
            return 127;
        }
    };

    let child_out = child.stdout.take();
    let child_err = child.stderr.take();

    let (tx, rx) = mpsc::channel::<String>();
    let tx_err = tx.clone();

    let t_out = thread::spawn(move || {
        if let Some(out) = child_out {
            for line in io::BufReader::new(out).lines().flatten() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        }
    });
    let t_err = thread::spawn(move || {
        if let Some(err) = child_err {
            for line in io::BufReader::new(err).lines().flatten() {
                if tx_err.send(line).is_err() {
                    break;
                }
            }
        }
    });

    while let Ok(line) = rx.recv() {
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }

    let _ = t_out.join();
    let _ = t_err.join();

    match child.wait() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(_) => -1,
    }
}

fn spawn_line_buffered(program: &str, args: &[&str]) -> io::Result<std::process::Child> {
    let mut cmd = if stdbuf_available() {
        let mut cmd = Command::new("stdbuf");
        cmd.args(["-oL", "-eL"]);
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
    Command::new("stdbuf")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use crate::helper_protocol::HelperCommand;

    #[test]
    fn only_allowlisted_argv() {
        let argv = HelperCommand::CheckDnf.argv();
        assert_eq!(argv, &["dnf", "check-update", "--refresh"]);
        let argv = HelperCommand::ApplyDnf.argv();
        assert_eq!(argv, &["dnf", "update", "-y"]);
        let argv = HelperCommand::ApplyFwupd.argv();
        assert!(argv.contains(&"--no-reboot-check"));
        assert!(argv.contains(&"-y"));
    }
}
