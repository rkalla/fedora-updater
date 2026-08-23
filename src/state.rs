//! Pure state machine for the updater UI.
//!
//! All transitions are pure functions of `(AppState, Event) → Result<AppState, TransitionError>`.
//! Side effects (spawning processes) live outside this module.

use crate::model::{
    apply_progress_hint, finalize_source, format_relative_now, overall_progress, sort_for_apply,
    AuthPurpose, ConsoleBuffer, Package, PackageStatus, Stopwatch, UpdateSource,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle {
        last_checked: Option<String>,
        message: Option<String>,
    },
    /// Waiting for polkit / helper session. For Apply, `packages` holds the Ready list.
    Authenticating {
        purpose: AuthPurpose,
        packages: Vec<Package>,
        /// Preserved so Cancel returns to the same Idle last-checked state.
        last_checked: Option<String>,
    },
    Checking {
        packages_so_far: Vec<Package>,
        soft_errors: Vec<String>,
        checked_sources: Vec<UpdateSource>,
    },
    Ready {
        packages: Vec<Package>,
        soft_errors: Vec<String>,
    },
    Running {
        packages: Vec<Package>,
        active_index: Option<usize>,
        overall_progress: f64,
        phase_label: String,
        current_source: Option<UpdateSource>,
        /// Last package name parsed from a backend line (may not be in `packages`).
        current_name: Option<String>,
        /// Last transaction/download fraction for the now-playing card.
        work_progress: Option<f64>,
        needs_reboot: bool,
        failed_sources: Vec<UpdateSource>,
    },
    Done {
        packages: Vec<Package>,
        duration: String,
        needs_reboot: bool,
        failed: usize,
    },
    Failed {
        title: String,
        detail: String,
    },
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub phase: Phase,
    pub console: ConsoleBuffer,
    pub stopwatch: Option<Stopwatch>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle {
                last_checked: None,
                message: None,
            },
            console: ConsoleBuffer::new(8_000),
            stopwatch: None,
        }
    }
}

impl AppState {
    pub fn packages(&self) -> &[Package] {
        match &self.phase {
            Phase::Authenticating { packages, .. } => packages,
            Phase::Checking {
                packages_so_far, ..
            } => packages_so_far,
            Phase::Ready { packages, .. } => packages,
            Phase::Running { packages, .. } => packages,
            Phase::Done { packages, .. } => packages,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    Invalid { from: String, event: String },
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { from, event } => {
                write!(f, "invalid transition: phase={from} event={event}")
            }
        }
    }
}

/// UI / orchestrator events (no I/O).
#[derive(Debug, Clone)]
pub enum Event {
    /// User clicked Check for Updates / Refresh.
    StartCheck,
    /// User clicked Update All.
    StartApply,
    /// polkit session is live.
    SessionReady {
        purpose: AuthPurpose,
    },
    /// Console line from a backend.
    ConsoleLine {
        tag: String,
        text: String,
    },
    /// One backend finished its check portion.
    BackendCheckDone {
        source: UpdateSource,
        packages: Vec<Package>,
        soft_error: Option<String>,
    },
    /// Entire check pipeline finished.
    CheckComplete {
        packages: Vec<Package>,
        soft_errors: Vec<String>,
    },
    /// Apply pipeline entered a backend.
    ApplySourceStarted {
        source: UpdateSource,
        label: String,
    },
    ApplyProgress {
        source: UpdateSource,
        package_hint: Option<String>,
        status_hint: Option<PackageStatus>,
        progress: Option<f64>,
        phase_label: String,
    },
    BackendApplyDone {
        source: UpdateSource,
        success: bool,
        needs_reboot: bool,
    },
    ApplyComplete {
        needs_reboot: bool,
    },
    Fail {
        title: String,
        detail: String,
    },
    /// User dismissed failure or finished Done → Idle.
    ResetToIdle {
        message: Option<String>,
    },
    /// User cancelled the in-app auth wait. Does not dismiss the system polkit dialog.
    CancelAuth,
}

pub fn reduce(mut state: AppState, event: Event) -> Result<AppState, TransitionError> {
    match event {
        Event::StartCheck => {
            let last_checked = match &state.phase {
                Phase::Idle { last_checked, .. } => last_checked.clone(),
                _ => None,
            };
            state.console.clear();
            state.stopwatch = Some(Stopwatch::start());
            state.phase = Phase::Authenticating {
                purpose: AuthPurpose::Check,
                packages: Vec::new(),
                last_checked,
            };
            Ok(state)
        }
        Event::StartApply => match state.phase {
            Phase::Ready { packages, .. } if !packages.is_empty() => {
                state.console.clear();
                state.stopwatch = Some(Stopwatch::start());
                state.phase = Phase::Authenticating {
                    purpose: AuthPurpose::Apply,
                    packages,
                    last_checked: None,
                };
                Ok(state)
            }
            _ => Err(invalid(&state, "StartApply")),
        },
        Event::SessionReady { purpose } => match state.phase {
            Phase::Authenticating {
                purpose: p,
                packages,
                ..
            } if p == purpose => {
                match purpose {
                    AuthPurpose::Check => {
                        state.phase = Phase::Checking {
                            packages_so_far: Vec::new(),
                            soft_errors: Vec::new(),
                            checked_sources: Vec::new(),
                        };
                    }
                    AuthPurpose::Apply => {
                        let mut packages = packages;
                        sort_for_apply(&mut packages);
                        state.phase = Phase::Running {
                            packages,
                            active_index: None,
                            overall_progress: 0.0,
                            phase_label: "Starting…".into(),
                            current_source: None,
                            current_name: None,
                            work_progress: None,
                            needs_reboot: false,
                            failed_sources: Vec::new(),
                        };
                    }
                }
                Ok(state)
            }
            // Idempotent if already advanced past auth
            Phase::Checking { .. } if purpose == AuthPurpose::Check => Ok(state),
            Phase::Running { .. } if purpose == AuthPurpose::Apply => Ok(state),
            other => {
                state.phase = other;
                Err(invalid(&state, "SessionReady"))
            }
        },
        Event::ConsoleLine { tag, text } => {
            state.console.push_tagged(&tag, &text);
            Ok(state)
        }
        Event::BackendCheckDone {
            source,
            packages,
            soft_error,
        } => match &mut state.phase {
            Phase::Checking {
                packages_so_far,
                soft_errors,
                checked_sources,
            } => {
                packages_so_far.extend(packages);
                if !checked_sources.contains(&source) {
                    checked_sources.push(source);
                }
                if let Some(e) = soft_error {
                    soft_errors.push(e);
                }
                Ok(state)
            }
            _ => Err(invalid(&state, "BackendCheckDone")),
        },
        Event::CheckComplete {
            packages,
            soft_errors,
        } => match state.phase {
            Phase::Checking { .. } => {
                if packages.is_empty() {
                    let msg = if soft_errors.is_empty() {
                        "No updates available".into()
                    } else {
                        format!("No updates available ({})", soft_errors.join("; "))
                    };
                    state.phase = Phase::Idle {
                        last_checked: Some(format_relative_now()),
                        message: Some(msg),
                    };
                } else {
                    let mut packages = packages;
                    sort_for_apply(&mut packages);
                    state.phase = Phase::Ready {
                        packages,
                        soft_errors,
                    };
                }
                Ok(state)
            }
            _ => Err(invalid(&state, "CheckComplete")),
        },
        Event::ApplySourceStarted { source, label } => match &mut state.phase {
            Phase::Running {
                phase_label,
                current_source,
                current_name,
                work_progress,
                active_index,
                ..
            } => {
                *phase_label = label;
                *current_source = Some(source);
                *current_name = None;
                *work_progress = None;
                *active_index = None;
                Ok(state)
            }
            _ => Err(invalid(&state, "ApplySourceStarted")),
        },
        Event::ApplyProgress {
            source,
            package_hint,
            status_hint,
            progress,
            phase_label,
        } => match &mut state.phase {
            Phase::Running {
                packages,
                active_index,
                overall_progress: op,
                phase_label: pl,
                current_source,
                current_name,
                work_progress,
                ..
            } => {
                if *current_source != Some(source) {
                    *current_name = None;
                    *work_progress = None;
                    *active_index = None;
                }
                *pl = phase_label;
                *current_source = Some(source);
                if progress.is_some() {
                    *work_progress = progress;
                }
                if let Some(name) = package_hint
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    match apply_progress_hint(packages, source, Some(name), status_hint, progress) {
                        Some(idx) => {
                            *current_name = Some(name.to_string());
                            *active_index = Some(idx);
                        }
                        None => {
                            let named_done = packages.iter().any(|p| {
                                p.source == source
                                    && p.matches_progress_name(name)
                                    && p.status.is_done()
                            });
                            if !named_done {
                                *current_name = Some(name.to_string());
                            }
                        }
                    }
                }
                *op = overall_progress(packages);
                Ok(state)
            }
            _ => Err(invalid(&state, "ApplyProgress")),
        },
        Event::BackendApplyDone {
            source,
            success,
            needs_reboot,
        } => match &mut state.phase {
            Phase::Running {
                packages,
                needs_reboot: nr,
                failed_sources,
                overall_progress: op,
                ..
            } => {
                finalize_source(packages, source, success);
                if needs_reboot {
                    *nr = true;
                }
                if !success && !failed_sources.contains(&source) {
                    failed_sources.push(source);
                }
                *op = overall_progress(packages);
                Ok(state)
            }
            _ => Err(invalid(&state, "BackendApplyDone")),
        },
        Event::ApplyComplete { needs_reboot } => match state.phase {
            Phase::Running {
                packages,
                needs_reboot: nr,
                failed_sources,
                ..
            } => {
                let duration = state
                    .stopwatch
                    .as_ref()
                    .map(|s| s.elapsed_label())
                    .unwrap_or_else(|| "?".into());
                let mut packages = packages;
                for p in packages.iter_mut() {
                    if !p.status.is_done() {
                        if failed_sources.contains(&p.source) {
                            p.status = PackageStatus::Failed;
                        } else {
                            p.status = PackageStatus::Completed;
                            p.progress = 1.0;
                        }
                    }
                }
                let failed = packages
                    .iter()
                    .filter(|p| p.status == PackageStatus::Failed)
                    .count();
                let hard_fail = failed == packages.len() && !packages.is_empty();
                if hard_fail {
                    state.phase = Phase::Failed {
                        title: "All updates failed".into(),
                        detail: state.console.full_text(),
                    };
                } else {
                    state.phase = Phase::Done {
                        packages,
                        duration,
                        needs_reboot: nr || needs_reboot,
                        failed,
                    };
                }
                Ok(state)
            }
            _ => Err(invalid(&state, "ApplyComplete")),
        },
        Event::Fail { title, detail } => {
            state.phase = Phase::Failed { title, detail };
            Ok(state)
        }
        Event::ResetToIdle { message } => {
            let last = match &state.phase {
                Phase::Done { .. } => Some(format_relative_now()),
                Phase::Idle { last_checked, .. } => last_checked.clone(),
                _ => None,
            };
            state.console.clear();
            state.stopwatch = None;
            state.phase = Phase::Idle {
                last_checked: last,
                message,
            };
            Ok(state)
        }
        Event::CancelAuth => match state.phase {
            Phase::Authenticating { last_checked, .. } => {
                state.console.clear();
                state.stopwatch = None;
                state.phase = Phase::Idle {
                    last_checked,
                    message: None,
                };
                Ok(state)
            }
            _ => Err(invalid(&state, "CancelAuth")),
        },
    }
}

fn invalid(state: &AppState, event: &str) -> TransitionError {
    let from = match &state.phase {
        Phase::Idle { .. } => "Idle",
        Phase::Authenticating { .. } => "Authenticating",
        Phase::Checking { .. } => "Checking",
        Phase::Ready { .. } => "Ready",
        Phase::Running { .. } => "Running",
        Phase::Done { .. } => "Done",
        Phase::Failed { .. } => "Failed",
    };
    TransitionError::Invalid {
        from: from.into(),
        event: event.into(),
    }
}

/// Phase name for tests / logging.
pub fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle { .. } => "Idle",
        Phase::Authenticating { .. } => "Authenticating",
        Phase::Checking { .. } => "Checking",
        Phase::Ready { .. } => "Ready",
        Phase::Running { .. } => "Running",
        Phase::Done { .. } => "Done",
        Phase::Failed { .. } => "Failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, source: UpdateSource) -> Package {
        let mut p = Package::new_dnf(name, "x86_64", "1.0", "updates");
        p.source = source;
        if source != UpdateSource::Dnf {
            p.id = format!("{source:?}:{name}");
        }
        p
    }

    #[test]
    fn check_flow_to_ready() {
        let s = AppState::default();
        let s = reduce(s, Event::StartCheck).unwrap();
        assert!(matches!(
            s.phase,
            Phase::Authenticating {
                purpose: AuthPurpose::Check,
                ..
            }
        ));
        let s = reduce(
            s,
            Event::SessionReady {
                purpose: AuthPurpose::Check,
            },
        )
        .unwrap();
        assert!(matches!(s.phase, Phase::Checking { .. }));

        let s = reduce(
            s,
            Event::BackendCheckDone {
                source: UpdateSource::Dnf,
                packages: vec![pkg("kernel", UpdateSource::Dnf)],
                soft_error: None,
            },
        )
        .unwrap();
        let packages = s.packages().to_vec();
        let s = reduce(
            s,
            Event::CheckComplete {
                packages,
                soft_errors: vec![],
            },
        )
        .unwrap();
        assert!(matches!(s.phase, Phase::Ready { .. }));
        assert_eq!(s.packages().len(), 1);
    }

    #[test]
    fn check_flow_no_updates() {
        let s = AppState::default();
        let s = reduce(s, Event::StartCheck).unwrap();
        let s = reduce(
            s,
            Event::SessionReady {
                purpose: AuthPurpose::Check,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::CheckComplete {
                packages: vec![],
                soft_errors: vec![],
            },
        )
        .unwrap();
        match s.phase {
            Phase::Idle {
                message: Some(m), ..
            } => assert!(m.contains("No updates")),
            _ => panic!("expected Idle"),
        }
    }

    #[test]
    fn apply_flow_success() {
        let mut s = AppState::default();
        s.phase = Phase::Ready {
            packages: vec![
                pkg("kernel", UpdateSource::Dnf),
                pkg("Signal", UpdateSource::FlatpakUser),
            ],
            soft_errors: vec![],
        };
        let s = reduce(s, Event::StartApply).unwrap();
        let s = reduce(
            s,
            Event::SessionReady {
                purpose: AuthPurpose::Apply,
            },
        )
        .unwrap();
        assert!(matches!(s.phase, Phase::Running { .. }));

        let s = reduce(
            s,
            Event::ApplySourceStarted {
                source: UpdateSource::Dnf,
                label: "System".into(),
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::BackendApplyDone {
                source: UpdateSource::Dnf,
                success: true,
                needs_reboot: true,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::BackendApplyDone {
                source: UpdateSource::FlatpakUser,
                success: true,
                needs_reboot: false,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::ApplyComplete {
                needs_reboot: false,
            },
        )
        .unwrap();
        match s.phase {
            Phase::Done {
                needs_reboot,
                failed,
                packages,
                ..
            } => {
                assert!(needs_reboot);
                assert_eq!(failed, 0);
                assert!(packages
                    .iter()
                    .all(|p| p.status == PackageStatus::Completed));
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn apply_rejects_from_idle() {
        let s = AppState::default();
        assert!(reduce(s, Event::StartApply).is_err());
    }

    #[test]
    fn partial_failure_still_done() {
        let mut s = AppState::default();
        s.phase = Phase::Ready {
            packages: vec![
                pkg("kernel", UpdateSource::Dnf),
                pkg("fw", UpdateSource::Firmware),
            ],
            soft_errors: vec![],
        };
        let s = reduce(s, Event::StartApply).unwrap();
        let s = reduce(
            s,
            Event::SessionReady {
                purpose: AuthPurpose::Apply,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::BackendApplyDone {
                source: UpdateSource::Dnf,
                success: true,
                needs_reboot: false,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::BackendApplyDone {
                source: UpdateSource::Firmware,
                success: false,
                needs_reboot: false,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::ApplyComplete {
                needs_reboot: false,
            },
        )
        .unwrap();
        match s.phase {
            Phase::Done { failed, .. } => assert_eq!(failed, 1),
            _ => panic!("expected Done with partial failure"),
        }
    }

    #[test]
    fn all_failed_goes_to_failed_phase() {
        let mut s = AppState::default();
        s.phase = Phase::Ready {
            packages: vec![pkg("kernel", UpdateSource::Dnf)],
            soft_errors: vec![],
        };
        let s = reduce(s, Event::StartApply).unwrap();
        let s = reduce(
            s,
            Event::SessionReady {
                purpose: AuthPurpose::Apply,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::BackendApplyDone {
                source: UpdateSource::Dnf,
                success: false,
                needs_reboot: false,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::ApplyComplete {
                needs_reboot: false,
            },
        )
        .unwrap();
        assert!(matches!(s.phase, Phase::Failed { .. }));
    }

    #[test]
    fn console_lines_accumulate() {
        let s = AppState::default();
        let s = reduce(s, Event::StartCheck).unwrap();
        let s = reduce(
            s,
            Event::ConsoleLine {
                tag: "dnf".into(),
                text: "hello".into(),
            },
        )
        .unwrap();
        assert!(s.console.last_line().contains("[dnf] hello"));
    }

    #[test]
    fn fail_from_anywhere() {
        let s = AppState::default();
        let s = reduce(
            s,
            Event::Fail {
                title: "x".into(),
                detail: "y".into(),
            },
        )
        .unwrap();
        assert!(matches!(s.phase, Phase::Failed { .. }));
    }

    #[test]
    fn reset_to_idle_from_done() {
        let mut s = AppState::default();
        s.phase = Phase::Done {
            packages: vec![],
            duration: "1s".into(),
            needs_reboot: false,
            failed: 0,
        };
        let s = reduce(
            s,
            Event::ResetToIdle {
                message: Some("ok".into()),
            },
        )
        .unwrap();
        match s.phase {
            Phase::Idle {
                last_checked: Some(_),
                message: Some(m),
            } => assert_eq!(m, "ok"),
            _ => panic!("expected Idle with last_checked"),
        }
    }

    #[test]
    fn cancel_auth_restores_last_checked() {
        let mut s = AppState::default();
        s.phase = Phase::Idle {
            last_checked: Some("just now".into()),
            message: Some("No updates available".into()),
        };
        let s = reduce(s, Event::StartCheck).unwrap();
        assert!(matches!(s.phase, Phase::Authenticating { .. }));
        let s = reduce(s, Event::CancelAuth).unwrap();
        match s.phase {
            Phase::Idle {
                last_checked: Some(t),
                message: None,
            } => assert_eq!(t, "just now"),
            other => panic!("expected Idle with last_checked, got {other:?}"),
        }
    }

    #[test]
    fn backend_check_records_sources() {
        let s = reduce(AppState::default(), Event::StartCheck).unwrap();
        let s = reduce(
            s,
            Event::SessionReady {
                purpose: AuthPurpose::Check,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::BackendCheckDone {
                source: UpdateSource::Dnf,
                packages: vec![pkg("kernel", UpdateSource::Dnf)],
                soft_error: None,
            },
        )
        .unwrap();
        match s.phase {
            Phase::Checking {
                checked_sources, ..
            } => {
                assert_eq!(checked_sources, vec![UpdateSource::Dnf]);
            }
            _ => panic!("expected Checking"),
        }
    }
}
