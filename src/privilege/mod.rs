//! Privilege boundary: long-lived helper session (one polkit prompt per app run).

mod pool;
mod session;

pub use pool::{close_session, session_is_alive, with_direct_session_for_test, with_session};
pub use session::{
    dnf_daemon_is_running, helper_path, kill_recorded_helper, kill_recorded_local_command,
    run_cancellable_local_command, run_local_command, PrivilegedSession, SessionError, SessionEvent,
    HELPER_FLAG,
};
