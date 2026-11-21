//! Process-wide shared privileged helper session.
//!
//! Started once via `pkexec` (one password for the app process). Reused for
//! every Check and Apply until the GUI exits or the helper dies. Closed only
//! on window close / explicit `close_session` — not after each check/apply.

use std::path::Path;
use std::sync::Mutex;

use super::session::{kill_recorded_helper, PrivilegedSession, SessionError};

static SESSION: Mutex<Option<PrivilegedSession>> = Mutex::new(None);

/// True if a helper process is already elevated, running, and ready for more commands.
pub fn session_is_alive() -> bool {
    let Ok(mut guard) = SESSION.lock() else {
        return false;
    };
    let Some(session) = guard.as_mut() else {
        return false;
    };
    if session.is_running() {
        true
    } else {
        // Child exited; drop so the next with_session can re-auth.
        *guard = None;
        false
    }
}

/// Run `f` with the shared session, starting it (and prompting polkit) if needed.
pub fn with_session<F, R>(f: F) -> Result<R, SessionError>
where
    F: FnOnce(&mut PrivilegedSession) -> Result<R, SessionError>,
{
    with_session_using(PrivilegedSession::start, f)
}

/// Run `f` with the shared session, starting `path` directly without `pkexec` (for tests).
pub fn with_direct_session_for_test<F, R>(path: &Path, f: F) -> Result<R, SessionError>
where
    F: FnOnce(&mut PrivilegedSession) -> Result<R, SessionError>,
{
    with_session_using(|| PrivilegedSession::start_direct(path), f)
}

fn with_session_using<F, R>(
    start: impl FnOnce() -> Result<PrivilegedSession, SessionError>,
    f: F,
) -> Result<R, SessionError>
where
    F: FnOnce(&mut PrivilegedSession) -> Result<R, SessionError>,
{
    let mut guard = SESSION
        .lock()
        .map_err(|_| SessionError::Io("session lock poisoned".into()))?;

    // Drop a dead child before starting a new one.
    if let Some(session) = guard.as_mut() {
        if !session.is_running() {
            *guard = None;
        }
    }

    if guard.is_none() {
        *guard = Some(start()?);
    }

    // Temporarily take ownership so a panic inside `f` does not leave a half-used session.
    let mut session = guard.take().expect("just ensured Some");
    let result = f(&mut session);
    match &result {
        Err(SessionError::Closed) | Err(SessionError::Io(_)) | Err(SessionError::AuthDenied) => {
            // Drop broken session (kills helper if still alive)
            drop(session);
        }
        _ => {
            if session.is_running() {
                *guard = Some(session);
            } else {
                drop(session);
            }
        }
    }
    result
}

/// End the shared session. Must never block: GTK calls this from
/// `close_request`, and a hung `fwupdmgr` can be holding the session lock
/// inside `with_session` / `run_command`.
///
/// Kill the helper process group first (so grandchildren die), then drop the
/// session if the lock is free. Do not wait for QUIT/`__BYE__`.
pub fn close_session() {
    kill_recorded_helper();
    if let Ok(mut guard) = SESSION.try_lock() {
        if let Some(session) = guard.take() {
            session.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper_protocol::{HelperCommand, HELLO_LINE};
    use crate::privilege::session::SessionEvent;
    use crate::privilege::test_support::{
        start_direct_retry, write_fake_helper, PRIVILEGE_TEST_LOCK,
    };
    use std::thread;
    use std::time::Duration;

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

    fn run_check_pid(script: &Path) -> Result<String, SessionError> {
        with_session_using(
            || start_direct_retry(script),
            |session| {
                let mut pid = None;
                let code = session.run_command(HelperCommand::CheckDnf, |ev| {
                    if let SessionEvent::Line(line) = ev {
                        if let Some(val) = line.strip_prefix("pid=") {
                            pid = Some(val.to_string());
                        }
                    }
                })?;
                assert_eq!(code, 0);
                Ok(pid.expect("helper must emit pid=<pid>"))
            },
        )
    }

    #[test]
    fn pool_reuses_single_helper_across_sequential_calls() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _cleanup = CloseSessionGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "#!/bin/sh\n\
printf '%s\\n' '{HELLO_LINE}'\n\
while IFS= read -r line; do\n\
  case \"$line\" in\n\
    'RUN '*)\n\
      printf 'pid=%s\\n' \"$$\"\n\
      printf '__END__ 0\\n'\n\
      ;;\n\
    QUIT)\n\
      printf '__BYE__\\n'\n\
      exit 0\n\
      ;;\n\
  esac\n\
done\n"
        );
        let script = write_fake_helper(dir.path(), "pool-reuse-helper", &body);

        assert!(!session_is_alive());
        let pid1 = run_check_pid(&script).expect("first pooled call");
        assert!(session_is_alive());

        let pid2 = run_check_pid(&script).expect("second pooled call");
        assert_eq!(pid1, pid2, "pool must reuse single elevated helper PID");
        assert!(session_is_alive());

        close_session();
        assert!(!session_is_alive());
    }

    #[test]
    fn pool_detects_dead_helper_between_calls_and_respawns() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _cleanup = CloseSessionGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "#!/bin/sh\n\
printf '%s\\n' '{HELLO_LINE}'\n\
IFS= read -r _line\n\
printf 'pid=%s\\n' \"$$\"\n\
printf '__END__ 0\\n'\n\
exit 0\n"
        );
        let script = write_fake_helper(dir.path(), "one-shot-helper", &body);

        let pid1 = run_check_pid(&script).expect("first one-shot call");

        let mut alive = true;
        for _ in 0..100 {
            if !session_is_alive() {
                alive = false;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !alive,
            "session_is_alive must turn false and clear dead helper"
        );

        let pid2 = run_check_pid(&script).expect("second call must cleanly respawn");
        assert_ne!(pid1, pid2, "respawned helper must have a new PID");
    }

    #[test]
    fn pool_evicts_session_on_closed_io_and_auth_denied() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _cleanup = CloseSessionGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "#!/bin/sh\n\
printf '%s\\n' '{HELLO_LINE}'\n\
while IFS= read -r line; do\n\
  case \"$line\" in\n\
    'RUN check-dnf')\n\
      printf 'pid=%s\\n' \"$$\"\n\
      printf '__END__ 0\\n'\n\
      ;;\n\
    'RUN apply-dnf')\n\
      exit 1\n\
      ;;\n\
    QUIT)\n\
      printf '__BYE__\\n'\n\
      exit 0\n\
      ;;\n\
  esac\n\
done\n"
        );
        let script = write_fake_helper(dir.path(), "evict-helper", &body);

        // Case A: mid-command EOF produces SessionError::Closed and evicts the session.
        let pid1 = run_check_pid(&script).expect("initial call");
        assert!(session_is_alive());

        let err_closed = with_session_using(
            || start_direct_retry(&script),
            |s| s.run_command(HelperCommand::ApplyDnf, |_| {}),
        );
        assert!(matches!(err_closed, Err(SessionError::Closed)));
        assert!(!session_is_alive(), "Closed must evict pooled session");

        let pid2 = run_check_pid(&script).expect("respawn after Closed");
        assert_ne!(pid2, pid1);

        // Case B: SessionError::Io evicts an otherwise running session.
        let err_io: Result<(), SessionError> = with_session_using(
            || start_direct_retry(&script),
            |_| Err(SessionError::Io("simulated io".into())),
        );
        assert!(matches!(err_io, Err(SessionError::Io(_))));
        assert!(!session_is_alive(), "Io must evict pooled session");

        let pid3 = run_check_pid(&script).expect("respawn after Io");
        assert_ne!(pid3, pid2);

        // Case C: SessionError::AuthDenied evicts an otherwise running session.
        let err_denied: Result<(), SessionError> =
            with_session_using(|| start_direct_retry(&script), |_| Err(SessionError::AuthDenied));
        assert!(matches!(err_denied, Err(SessionError::AuthDenied)));
        assert!(!session_is_alive(), "AuthDenied must evict pooled session");

        let pid4 = run_check_pid(&script).expect("respawn after AuthDenied");
        assert_ne!(pid4, pid3);
    }

    #[test]
    fn pool_start_auth_denied_leaves_pool_empty() {
        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _cleanup = CloseSessionGuard::new();

        let dir = tempfile::tempdir().unwrap();
        let script = write_fake_helper(dir.path(), "deny-start-helper", "#!/bin/sh\nexit 1\n");

        let res = with_session_using(|| start_direct_retry(&script), |_| Ok(()));
        assert!(matches!(res, Err(SessionError::AuthDenied)));
        assert!(!session_is_alive());
    }

    #[test]
    fn pool_handles_poisoned_session_mutex_without_panicking() {
        struct ClearPoisonOnDrop;
        impl Drop for ClearPoisonOnDrop {
            fn drop(&mut self) {
                SESSION.clear_poison();
                close_session();
            }
        }

        let _lock = PRIVILEGE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _cleanup = CloseSessionGuard::new();
        let _clear = ClearPoisonOnDrop;

        let _ = thread::spawn(|| {
            let mut guard = SESSION.lock().unwrap();
            *guard = None;
            panic!("intentional panic to poison SESSION mutex");
        })
        .join();

        assert!(SESSION.is_poisoned());
        assert!(!session_is_alive());

        let res = with_session_using(
            || start_direct_retry(Path::new("/nonexistent-helper")),
            |_| Ok(()),
        );
        assert!(
            matches!(res, Err(SessionError::Io(ref msg)) if msg.contains("poisoned")),
            "expected poisoned lock error, got {res:?}"
        );

        close_session();
        SESSION.clear_poison();
        assert!(!session_is_alive());
    }
}
