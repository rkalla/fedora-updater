//! Privilege boundary: long-lived helper session (one polkit prompt per app run).

mod pool;
mod session;

pub use pool::{close_session, session_is_alive, with_session, would_prompt};
pub use session::{
    helper_path, kill_recorded_helper, run_local_command, PrivilegedSession, SessionError,
    SessionEvent, HELPER_FLAG,
};
