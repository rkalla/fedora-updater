//! Privilege boundary: long-lived helper session (one polkit prompt per app run).

mod pool;
mod session;

pub use pool::{close_session, session_is_alive, with_direct_session_for_test, with_session};
pub use session::{
    dnf_daemon_is_running, helper_path, kill_recorded_helper, kill_recorded_local_command,
    run_cancellable_local_command, run_cancellable_local_command_env, run_local_command,
    PrivilegedSession, SessionError, SessionEvent, HELPER_FLAG,
};

#[cfg(test)]
pub(super) mod test_support {
    use super::{PrivilegedSession, SessionError};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    /// Serializes lib unit tests that mutate process-wide `SESSION`, `HELPER_PID`,
    /// or `LOCAL_CMD_PID`.
    pub(super) static PRIVILEGE_TEST_LOCK: Mutex<()> = Mutex::new(());

    pub(super) fn write_fake_helper(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write fake helper script");
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake helper");
        path
    }

    pub(super) fn start_direct_retry(path: &Path) -> Result<PrivilegedSession, SessionError> {
        for attempt in 0..10 {
            match PrivilegedSession::start_direct(path) {
                Err(SessionError::Spawn(ref msg))
                    if attempt < 9 && msg.contains("Text file busy") =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                other => return other,
            }
        }
        unreachable!("loop returns on final attempt")
    }
}
