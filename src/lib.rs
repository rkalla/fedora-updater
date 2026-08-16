//! Fedora Updater core library — pure logic, parsers, privilege protocol, orchestration.
//!
//! The GUI (`fedora-updater`) and the polkit helper (`fedora-updater-helper`) both
//! link against this crate. Host mutations always go through CLI tools; privileged
//! commands are allowlisted and executed only by the helper.

pub mod backend;
pub mod helper_mode;
pub mod helper_protocol;
pub mod model;
pub mod orchestrator;
pub mod privilege;
pub mod state;

pub use model::*;
pub use state::{phase_name, reduce, AppState, Event, Phase, TransitionError};
