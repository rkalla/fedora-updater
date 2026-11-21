//! Fedora Updater core library — pure logic, parsers, privilege protocol, orchestration.
//!
//! Both modes of the multi-call binary — the GNOME GUI (`fedora-updater`) and the
//! polkit helper (`fedora-updater --helper`) — link against this crate. Host mutations
//! always go through CLI tools; privileged commands are allowlisted and executed only
//! by the helper.

pub mod backend;
pub mod console;
pub mod helper_mode;
pub mod helper_protocol;
pub mod model;
pub mod orchestrator;
pub mod presenter;
pub mod privilege;
pub mod release_upgrade;
pub mod state;

pub use console::{ConsoleBuffer, ConsoleDelta};
pub use model::*;
pub use state::{phase_name, reduce, AppState, Event, Phase, RunningState, TransitionError};
