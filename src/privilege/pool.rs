//! Process-wide shared privileged helper session.
//!
//! Started once via `pkexec` (one password for the app process). Reused for
//! every Check and Apply until the GUI exits or the helper dies. Closed only
//! on window close / explicit `close_session` — not after each check/apply.

use std::sync::Mutex;

use super::session::{PrivilegedSession, SessionError};

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
        *guard = Some(PrivilegedSession::start()?);
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

/// Whether `with_session` would need a new polkit prompt.
pub fn would_prompt() -> bool {
    !session_is_alive()
}

/// End the shared session (QUIT helper / kill). Safe to call if none.
/// Prefer calling only when the app is shutting down.
pub fn close_session() {
    if let Ok(mut guard) = SESSION.lock() {
        if let Some(session) = guard.take() {
            let _ = session.quit();
        }
    }
}
