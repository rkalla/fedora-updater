//! Pure state machine for the updater UI.
//!
//! All transitions are pure functions of `(AppState, Event) → Result<AppState, TransitionError>`.
//! Side effects (spawning processes) live outside this module.

use std::time::{Duration, SystemTime};

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
        /// Last per-item fraction for the now-playing card. Not the top-bar count.
        work_progress: Option<f64>,
        /// True after this source leaves the download pass and starts applying.
        applying: bool,
        /// `[cur/total]` for the active download or transaction. Drives the top bar.
        stage_current: Option<u32>,
        stage_total: Option<u32>,
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

/// A Ready update list may be applied for this long after it was checked.
/// Once the list is older than this, Update All must refresh before applying.
pub const UPDATE_LIST_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct AppState {
    pub phase: Phase,
    pub console: ConsoleBuffer,
    pub stopwatch: Option<Stopwatch>,
    /// When the current Ready list was produced by a check.
    /// `None` before a successful check, after an empty check, and for preview
    /// states that were not produced by [`Event::CheckComplete`].
    pub updates_listed_at: Option<SystemTime>,
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
            updates_listed_at: None,
        }
    }
}

impl AppState {
    /// Whether Update All should refresh before applying.
    ///
    /// Only a Ready list with a recorded check time can expire. A missing
    /// timestamp (preview states) stays applicable. A backwards clock step
    /// does not expire the list. The list is valid through [`UPDATE_LIST_MAX_AGE`]
    /// and needs a refresh once it is older than that.
    pub fn update_list_needs_refresh(&self, now: SystemTime) -> bool {
        let Some(listed_at) = self.updates_listed_at else {
            return false;
        };
        if !matches!(self.phase, Phase::Ready { .. }) {
            return false;
        }
        match now.duration_since(listed_at) {
            Ok(age) => age > UPDATE_LIST_MAX_AGE,
            Err(_) => false,
        }
    }

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
    /// `listed_at` is the clock time the check finished; the reducer stores it
    /// when the result is a Ready list.
    CheckComplete {
        packages: Vec<Package>,
        soft_errors: Vec<String>,
        listed_at: SystemTime,
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
        stage_current: Option<u32>,
        stage_total: Option<u32>,
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
            state.updates_listed_at = None;
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
                            applying: false,
                            stage_current: None,
                            stage_total: None,
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
            listed_at,
        } => match state.phase {
            Phase::Checking { .. } => {
                if packages.is_empty() {
                    let msg = if soft_errors.is_empty() {
                        "No updates available".into()
                    } else {
                        format!("No updates available ({})", soft_errors.join("; "))
                    };
                    state.updates_listed_at = None;
                    state.phase = Phase::Idle {
                        last_checked: Some(format_relative_now()),
                        message: Some(msg),
                    };
                } else {
                    let mut packages = packages;
                    sort_for_apply(&mut packages);
                    state.updates_listed_at = Some(listed_at);
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
                applying,
                stage_current,
                stage_total,
                overall_progress,
                ..
            } => {
                *phase_label = label;
                *current_source = Some(source);
                *current_name = None;
                *work_progress = None;
                *active_index = None;
                *applying = false;
                *stage_current = None;
                *stage_total = None;
                *overall_progress = 0.0;
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
            stage_current,
            stage_total,
        } => match &mut state.phase {
            Phase::Running {
                packages,
                active_index,
                overall_progress: op,
                phase_label: pl,
                current_source,
                current_name,
                work_progress,
                applying,
                stage_current: stage_cur,
                stage_total: stage_tot,
                ..
            } => {
                if *current_source != Some(source) {
                    *current_name = None;
                    *work_progress = None;
                    *active_index = None;
                    *applying = false;
                    *stage_cur = None;
                    *stage_tot = None;
                    *op = 0.0;
                }
                if !*applying && should_enter_apply(&phase_label, status_hint) {
                    reset_live_packages(packages);
                    *work_progress = None;
                    *active_index = None;
                    *current_name = None;
                    *stage_cur = None;
                    *stage_tot = None;
                    *op = 0.0;
                    *applying = true;
                }
                let mut status_hint = status_hint;
                if *applying && status_hint == Some(PackageStatus::Downloading) {
                    status_hint = Some(PackageStatus::Installing);
                }
                if !phase_label.trim().is_empty() && !(*applying && phase_is_download(&phase_label))
                {
                    *pl = phase_label;
                }
                *current_source = Some(source);
                if let (Some(cur), Some(total)) = (stage_current, stage_total) {
                    if total > 0 {
                        *stage_cur = Some(cur);
                        *stage_tot = Some(total);
                        *op = (f64::from(cur) / f64::from(total)).clamp(0.0, 1.0);
                    }
                }
                if progress.is_some() {
                    *work_progress = progress;
                }
                match apply_progress_hint(
                    packages,
                    source,
                    package_hint.as_deref(),
                    status_hint,
                    progress,
                ) {
                    Some(idx) => {
                        *current_name = Some(packages[idx].name.clone());
                        *active_index = if packages[idx].status.is_done() {
                            None
                        } else {
                            Some(idx)
                        };
                    }
                    None => {
                        if let Some(name) = package_hint
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            let named_done = packages.iter().any(|p| {
                                p.source == source
                                    && p.matches_progress_name(name)
                                    && p.status.is_done()
                            });
                            if !named_done {
                                *current_name = Some(name.to_string());
                                let still_current = active_index
                                    .and_then(|i| packages.get(i))
                                    .is_some_and(|p| p.matches_progress_name(name));
                                if !still_current {
                                    *active_index = None;
                                }
                            }
                        }
                    }
                }
                if stage_cur.is_none() && *applying {
                    *op = overall_progress(packages);
                }
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
                stage_current,
                stage_total,
                ..
            } => {
                finalize_source(packages, source, success);
                if needs_reboot {
                    *nr = true;
                }
                if !success && !failed_sources.contains(&source) {
                    failed_sources.push(source);
                }
                *stage_current = None;
                *stage_total = None;
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

fn phase_is_download(label: &str) -> bool {
    label.to_ascii_lowercase().contains("download")
}

fn should_enter_apply(label: &str, status: Option<PackageStatus>) -> bool {
    if phase_is_download(label) {
        return false;
    }
    let label = label.to_ascii_lowercase();
    label.contains("install")
        || label.contains("upgrad")
        || label.contains("verif")
        || label.contains("transaction")
        || label.contains("scriptlet")
        || label.contains("cleanup")
        || status == Some(PackageStatus::Installing)
        || status == Some(PackageStatus::Completed)
}

fn reset_live_packages(packages: &mut [Package]) {
    for package in packages.iter_mut() {
        if !package.status.is_done() {
            package.status = PackageStatus::Pending;
            package.progress = 0.0;
        }
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
                listed_at: SystemTime::UNIX_EPOCH,
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
                listed_at: SystemTime::UNIX_EPOCH,
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
    fn empty_phase_label_does_not_clobber_current_phase() {
        let mut s = AppState::default();
        s.phase = Phase::Ready {
            packages: vec![pkg("System Firmware", UpdateSource::Firmware)],
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
            Event::ApplyProgress {
                source: UpdateSource::Firmware,
                package_hint: None,
                status_hint: Some(PackageStatus::Installing),
                progress: Some(0.5),
                phase_label: "Installing firmware".into(),
                stage_current: None,
                stage_total: None,
            },
        )
        .unwrap();
        let s = reduce(
            s,
            Event::ApplyProgress {
                source: UpdateSource::Firmware,
                package_hint: None,
                status_hint: None,
                progress: Some(1.0),
                phase_label: String::new(),
                stage_current: None,
                stage_total: None,
            },
        )
        .unwrap();
        match &s.phase {
            Phase::Running { phase_label, .. } => {
                assert_eq!(phase_label, "Installing firmware");
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    fn dnf_progress(
        name: Option<&str>,
        status: Option<PackageStatus>,
        progress: Option<f64>,
        phase_label: &str,
        stage_current: Option<u32>,
        stage_total: Option<u32>,
    ) -> Event {
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: name.map(str::to_string),
            status_hint: status,
            progress,
            phase_label: phase_label.into(),
            stage_current,
            stage_total,
        }
    }

    #[test]
    fn download_pass_does_not_finish_packages_and_apply_restarts_the_bar() {
        let mut s = AppState::default();
        s.phase = Phase::Running {
            packages: vec![dnf_pkg("wireplumber"), dnf_pkg("glibc"), dnf_pkg("kernel")],
            active_index: None,
            overall_progress: 0.0,
            phase_label: "Starting…".into(),
            current_source: Some(UpdateSource::Dnf),
            current_name: None,
            work_progress: None,
            applying: false,
            stage_current: None,
            stage_total: None,
            needs_reboot: false,
            failed_sources: Vec::new(),
        };

        for (name, cur) in [("wireplumber", 1), ("glibc", 2), ("kernel", 3)] {
            s = reduce(
                s,
                dnf_progress(
                    Some(name),
                    Some(PackageStatus::Downloading),
                    Some(1.0),
                    "Downloading packages",
                    Some(cur),
                    Some(3),
                ),
            )
            .unwrap();
        }

        match &s.phase {
            Phase::Running {
                packages,
                overall_progress,
                applying,
                stage_current,
                stage_total,
                ..
            } => {
                assert!(
                    packages
                        .iter()
                        .all(|p| p.status != PackageStatus::Completed),
                    "downloads must not check packages off, got {packages:?}"
                );
                assert_eq!(packages[0].status, PackageStatus::Pending);
                assert_eq!(packages[1].status, PackageStatus::Pending);
                assert_eq!(packages[2].status, PackageStatus::Downloading);
                assert!(!*applying);
                assert_eq!((*stage_current, *stage_total), (Some(3), Some(3)));
                assert!(
                    (*overall_progress - 1.0).abs() < f64::EPSILON,
                    "top bar follows the download count, got {overall_progress}"
                );
            }
            other => panic!("expected Running, got {other:?}"),
        }

        s = reduce(
            s,
            dnf_progress(None, None, None, "Running transaction", None, None),
        )
        .unwrap();
        match &s.phase {
            Phase::Running {
                packages,
                overall_progress,
                applying,
                work_progress,
                ..
            } => {
                assert!(*applying);
                assert!(packages.iter().all(|p| p.status == PackageStatus::Pending));
                assert!(work_progress.is_none());
                assert!(
                    (*overall_progress - 0.0).abs() < f64::EPSILON,
                    "apply pass restarts the top bar, got {overall_progress}"
                );
            }
            other => panic!("expected Running, got {other:?}"),
        }

        s = reduce(
            s,
            dnf_progress(
                Some("glibc"),
                Some(PackageStatus::Installing),
                Some(1.0),
                "Installing",
                Some(16),
                Some(137),
            ),
        )
        .unwrap();
        match &s.phase {
            Phase::Running {
                packages,
                overall_progress,
                work_progress,
                applying,
                stage_current,
                ..
            } => {
                assert!(*applying);
                assert_eq!(status_of(packages, "glibc"), PackageStatus::Installing);
                assert_eq!(status_of(packages, "wireplumber"), PackageStatus::Pending);
                assert_eq!(status_of(packages, "kernel"), PackageStatus::Pending);
                assert_eq!(*work_progress, Some(1.0));
                assert_eq!(*stage_current, Some(16));
                assert!(
                    (*overall_progress - 16.0 / 137.0).abs() < 1e-9,
                    "item 100% must not replace the transaction count, got {overall_progress}"
                );
            }
            other => panic!("expected Running, got {other:?}"),
        }

        s = reduce(
            s,
            dnf_progress(
                Some("kernel"),
                Some(PackageStatus::Installing),
                Some(0.4),
                "Installing",
                Some(17),
                Some(137),
            ),
        )
        .unwrap();
        match &s.phase {
            Phase::Running {
                packages,
                overall_progress,
                work_progress,
                ..
            } => {
                assert_eq!(status_of(packages, "glibc"), PackageStatus::Completed);
                assert_eq!(status_of(packages, "kernel"), PackageStatus::Installing);
                assert_eq!(*work_progress, Some(0.4));
                assert!((*overall_progress - 17.0 / 137.0).abs() < 1e-9);
            }
            other => panic!("expected Running, got {other:?}"),
        }

        s = reduce(
            s,
            dnf_progress(
                Some("libswscale-free"),
                Some(PackageStatus::Installing),
                Some(1.0),
                "Installing",
                Some(18),
                Some(137),
            ),
        )
        .unwrap();
        match &s.phase {
            Phase::Running {
                packages,
                active_index,
                current_name,
                overall_progress,
                ..
            } => {
                assert_eq!(status_of(packages, "kernel"), PackageStatus::Installing);
                assert_eq!(*active_index, None);
                assert_eq!(current_name.as_deref(), Some("libswscale-free"));
                assert!((*overall_progress - 18.0 / 137.0).abs() < 1e-9);
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    fn dnf_pkg(name: &str) -> Package {
        Package::new_dnf(name, "x86_64", "1", "updates")
    }

    fn status_of(packages: &[Package], name: &str) -> PackageStatus {
        packages
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .status
    }

    #[test]
    fn firmware_success_progress_completes_the_live_item() {
        let mut s = AppState::default();
        s.phase = Phase::Ready {
            packages: vec![pkg("System Firmware", UpdateSource::Firmware)],
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
            Event::ApplyProgress {
                source: UpdateSource::Firmware,
                package_hint: None,
                status_hint: Some(PackageStatus::Completed),
                progress: Some(1.0),
                phase_label: "Firmware installed".into(),
                stage_current: None,
                stage_total: None,
            },
        )
        .unwrap();
        match &s.phase {
            Phase::Running {
                packages,
                overall_progress,
                ..
            } => {
                assert_eq!(packages[0].status, PackageStatus::Completed);
                assert!((*overall_progress - 1.0).abs() < f64::EPSILON);
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn ready_list_expires_after_24_hours() {
        let listed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
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
            Event::CheckComplete {
                packages: vec![pkg("kernel", UpdateSource::Dnf)],
                soft_errors: vec![],
                listed_at,
            },
        )
        .unwrap();
        assert_eq!(s.updates_listed_at, Some(listed_at));
        assert!(!s.update_list_needs_refresh(listed_at));
        assert!(!s.update_list_needs_refresh(listed_at + UPDATE_LIST_MAX_AGE));
        assert!(
            s.update_list_needs_refresh(listed_at + UPDATE_LIST_MAX_AGE + Duration::from_secs(1))
        );
        assert!(s.update_list_needs_refresh(listed_at + Duration::from_secs(4 * 24 * 60 * 60)));
        // Clock stepped backwards: the list is not older than the check.
        assert!(!s.update_list_needs_refresh(listed_at - Duration::from_secs(30)));
    }

    #[test]
    fn empty_check_clears_list_timestamp() {
        let mut s = AppState::default();
        s.updates_listed_at = Some(SystemTime::UNIX_EPOCH);
        s.phase = Phase::Checking {
            packages_so_far: vec![],
            soft_errors: vec![],
            checked_sources: vec![],
        };
        let s = reduce(
            s,
            Event::CheckComplete {
                packages: vec![],
                soft_errors: vec![],
                listed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(50),
            },
        )
        .unwrap();
        assert!(s.updates_listed_at.is_none());
        assert!(!s.update_list_needs_refresh(SystemTime::now()));
    }

    #[test]
    fn stale_timestamp_does_not_block_other_phases() {
        let mut s = AppState::default();
        s.updates_listed_at = Some(SystemTime::UNIX_EPOCH);
        s.phase = Phase::Idle {
            last_checked: None,
            message: None,
        };
        assert!(!s.update_list_needs_refresh(SystemTime::now()));

        s.phase = Phase::Ready {
            packages: vec![pkg("kernel", UpdateSource::Dnf)],
            soft_errors: vec![],
        };
        assert!(s.update_list_needs_refresh(SystemTime::now()));

        let s = reduce(s, Event::StartCheck).unwrap();
        assert!(s.updates_listed_at.is_none());
        assert!(!s.update_list_needs_refresh(SystemTime::now()));
    }

    #[test]
    fn preview_ready_without_timestamp_can_still_apply() {
        let mut s = AppState::default();
        s.phase = Phase::Ready {
            packages: vec![pkg("kernel", UpdateSource::Dnf)],
            soft_errors: vec![],
        };
        assert!(!s.update_list_needs_refresh(SystemTime::now()));
        assert!(reduce(s, Event::StartApply).is_ok());
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
