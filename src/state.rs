//! Pure state machine for the updater UI.
//!
//! All transitions are pure functions of `(&mut AppState, Event) → Result<(), TransitionError>`.
//! Side effects (spawning processes) live outside this module.

use std::time::{Duration, SystemTime};

use crate::model::{
    apply_progress_hint, finalize_source, overall_progress, sort_for_apply, ApplyMode,
    AuthPurpose, IdleScreen, Package, PackageStatus, RetryAction, RunPass, StageCount, Stopwatch,
    UpdateSource,
};

#[derive(Debug, Clone, PartialEq)]
pub struct RunningState {
    pub packages: Vec<Package>,
    pub active_index: Option<usize>,
    pub overall_progress: f64,
    pub phase_label: String,
    pub current_source: Option<UpdateSource>,
    /// Last package name parsed from a backend line (may not be in `packages`).
    pub current_name: Option<String>,
    /// Last per-item fraction for the now-playing card. Not the top-bar count.
    pub work_progress: Option<f64>,
    /// Download or apply pass for the current source.
    pub pass: RunPass,
    /// `[current/total]` when both numbers arrived and `total > 0`.
    pub stage: Option<StageCount>,
    pub needs_reboot: bool,
    pub failed_sources: Vec<UpdateSource>,
    pub failure_reasons: Vec<String>,
}

impl RunningState {
    pub fn new(packages: Vec<Package>) -> Self {
        Self {
            packages,
            active_index: None,
            overall_progress: 0.0,
            phase_label: "Starting…".into(),
            current_source: None,
            current_name: None,
            work_progress: None,
            pass: RunPass::Download,
            stage: None,
            needs_reboot: false,
            failed_sources: Vec::new(),
            failure_reasons: Vec::new(),
        }
    }

    pub fn reset_for_source(&mut self, source: UpdateSource) {
        self.current_source = Some(source);
        self.current_name = None;
        self.work_progress = None;
        self.active_index = None;
        self.pass = RunPass::Download;
        self.stage = None;
        self.overall_progress = 0.0;
    }

    pub fn enter_apply_pass(&mut self) {
        reset_live_packages(&mut self.packages);
        self.work_progress = None;
        self.active_index = None;
        self.current_name = None;
        self.stage = None;
        self.overall_progress = 0.0;
        self.pass = RunPass::Apply;
    }

    pub fn done_and_total(&self) -> (usize, usize) {
        match self.stage {
            Some(StageCount { current, total }) if total > 0 => (current as usize, total as usize),
            _ => {
                let done = self.packages.iter().filter(|p| p.status.is_done()).count();
                (done, self.packages.len())
            }
        }
    }

    pub fn is_applying(&self) -> bool {
        self.pass == RunPass::Apply
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle(IdleScreen),
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
    Running(RunningState),
    Done {
        packages: Vec<Package>,
        duration: String,
        needs_reboot: bool,
        failed: usize,
        failure_reasons: Vec<String>,
    },
    Failed {
        title: String,
        detail: String,
        retry: RetryAction,
    },
}

/// A Ready update list may be applied for this long after it was checked.
/// Once the list is older than this, Update All must refresh before applying.
pub const UPDATE_LIST_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct AppState {
    pub phase: Phase,
    pub stopwatch: Option<Stopwatch>,
    /// When the current Ready list was produced by a check.
    /// `None` before a successful check, after an empty check, and for preview
    /// states that were not produced by [`Event::CheckComplete`].
    pub updates_listed_at: Option<SystemTime>,
    /// After a check, start apply when the list is non-empty.
    /// Set only by the stale-list dialog's Yes (`StartCheck { then_apply: true }`).
    pub apply_when_ready: bool,
    /// Newer Active Fedora release, when the collections list names one.
    /// Independent of [`Phase`]. A check does not clear it.
    pub release_upgrade: Option<u32>,
    /// Calendar days since that release was marked Active.
    /// `None` leaves the reminder up and hides the age line.
    pub release_age_days: Option<u32>,
    /// Preserved Ready packages when Apply authentication fails so Retry Update can resume.
    pub retry_apply_packages: Vec<Package>,
    /// Execution mode selected for the active or next apply transaction.
    pub apply_mode: ApplyMode,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle(IdleScreen::Fresh),
            stopwatch: None,
            updates_listed_at: None,
            apply_when_ready: false,
            release_upgrade: None,
            release_age_days: None,
            retry_apply_packages: Vec::new(),
            apply_mode: ApplyMode::Live,
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
            Phase::Running(running) => &running.packages,
            Phase::Done { packages, .. } => packages,
            Phase::Failed {
                retry: RetryAction::RetryApply,
                ..
            } => &self.retry_apply_packages,
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
    /// `then_apply` is true only when they agreed to refresh a stale list and
    /// then apply whatever that check finds.
    StartCheck {
        then_apply: bool,
    },
    /// User clicked Update All.
    StartApply,
    /// User initiated Apply with explicit selection and execution mode.
    StartSelectiveApply {
        selected_ids: Vec<String>,
        mode: ApplyMode,
    },
    /// polkit session is live.
    SessionReady {
        purpose: AuthPurpose,
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
        error: Option<String>,
    },
    ApplyComplete {
        needs_reboot: bool,
    },
    Fail {
        title: String,
        detail: String,
        retry: RetryAction,
    },
    /// User dismissed failure or finished Done → Idle.
    ResetToIdle,
    /// User cancelled the in-app auth wait. Does not dismiss the system polkit dialog.
    CancelAuth,
    /// User stopped the running update. `during_transaction` is the apply pass.
    StopApply {
        during_transaction: bool,
    },
    /// A newer Fedora release was looked up. `None` hides the reminder.
    /// Valid in every phase. Does not start a check or an apply.
    /// A missing age hides only the "(N days since release)" line.
    ReleaseNoted {
        version: Option<u32>,
        days_since_release: Option<u32>,
    },
}

pub fn reduce(state: &mut AppState, event: Event) -> Result<(), TransitionError> {
    match event {
        Event::ReleaseNoted {
            version,
            days_since_release,
        } => {
            state.release_upgrade = version;
            state.release_age_days = version.and(days_since_release);
            Ok(())
        }
        Event::StartCheck { then_apply } => {
            let last_checked = match &state.phase {
                Phase::Idle(screen) => idle_checked_at(screen),
                _ => None,
            };
            state.apply_when_ready = then_apply;
            state.updates_listed_at = None;
            state.retry_apply_packages.clear();
            state.stopwatch = Some(Stopwatch::start());
            state.phase = Phase::Authenticating {
                purpose: AuthPurpose::Check,
                packages: Vec::new(),
                last_checked,
            };
            Ok(())
        }
        Event::StartApply => {
            let packages = match &mut state.phase {
                Phase::Ready { packages, .. } if !packages.is_empty() => {
                    state.retry_apply_packages.clear();
                    std::mem::take(packages)
                }
                Phase::Failed {
                    retry: RetryAction::RetryApply,
                    ..
                } if !state.retry_apply_packages.is_empty() => {
                    std::mem::take(&mut state.retry_apply_packages)
                }
                _ => return Err(invalid(state, "StartApply")),
            };
            state.apply_mode = ApplyMode::Live;
            state.apply_when_ready = false;
            state.stopwatch = Some(Stopwatch::start());
            state.phase = Phase::Authenticating {
                purpose: AuthPurpose::Apply,
                packages,
                last_checked: None,
            };
            Ok(())
        }
        Event::StartSelectiveApply { selected_ids, mode } => {
            let packages = match &mut state.phase {
                Phase::Ready { packages, .. } => {
                    let chosen: Vec<Package> = packages
                        .iter()
                        .filter(|p| selected_ids.iter().any(|id| id == &p.id))
                        .cloned()
                        .collect();
                    if chosen.is_empty() {
                        return Err(invalid(state, "StartSelectiveApply"));
                    }
                    state.retry_apply_packages.clear();
                    chosen
                }
                Phase::Failed {
                    retry: RetryAction::RetryApply,
                    ..
                } if !state.retry_apply_packages.is_empty() => {
                    if selected_ids.is_empty() {
                        std::mem::take(&mut state.retry_apply_packages)
                    } else {
                        let chosen: Vec<Package> = state
                            .retry_apply_packages
                            .iter()
                            .filter(|p| selected_ids.iter().any(|id| id == &p.id))
                            .cloned()
                            .collect();
                        if chosen.is_empty() {
                            return Err(invalid(state, "StartSelectiveApply"));
                        }
                        state.retry_apply_packages.clear();
                        chosen
                    }
                }
                _ => return Err(invalid(state, "StartSelectiveApply")),
            };
            state.apply_mode = mode;
            state.apply_when_ready = false;
            state.stopwatch = Some(Stopwatch::start());
            state.phase = Phase::Authenticating {
                purpose: AuthPurpose::Apply,
                packages,
                last_checked: None,
            };
            Ok(())
        }
        Event::SessionReady { purpose } => match &mut state.phase {
            Phase::Authenticating {
                purpose: p,
                packages,
                ..
            } if *p == purpose => {
                let mut packages = std::mem::take(packages);
                match purpose {
                    AuthPurpose::Check => {
                        state.phase = Phase::Checking {
                            packages_so_far: Vec::new(),
                            soft_errors: Vec::new(),
                            checked_sources: Vec::new(),
                        };
                    }
                    AuthPurpose::Apply => {
                        sort_for_apply(&mut packages);
                        state.phase = Phase::Running(RunningState::new(packages));
                    }
                }
                Ok(())
            }
            // Idempotent if already advanced past auth
            Phase::Checking { .. } if purpose == AuthPurpose::Check => Ok(()),
            Phase::Running(_) if purpose == AuthPurpose::Apply => Ok(()),
            _ => Err(invalid(state, "SessionReady")),
        },
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
                Ok(())
            }
            _ => Err(invalid(state, "BackendCheckDone")),
        },
        Event::CheckComplete {
            packages,
            soft_errors,
            listed_at,
        } => {
            if !matches!(&state.phase, Phase::Checking { .. }) {
                return Err(invalid(state, "CheckComplete"));
            }
            if packages.is_empty() {
                state.apply_when_ready = false;
                state.updates_listed_at = None;
                state.retry_apply_packages.clear();
                if soft_errors.is_empty() {
                    state.phase = Phase::Idle(IdleScreen::Current {
                        checked_at: "just now".into(),
                        detail: None,
                    });
                } else {
                    state.phase = Phase::Idle(IdleScreen::Warning {
                        title: "Could not check all sources",
                        detail: format!(
                            "Some sources could not be checked: {}",
                            soft_errors.join("; ")
                        ),
                        checked_at: Some("just now".into()),
                    });
                }
            } else {
                let mut packages = packages;
                sort_for_apply(&mut packages);
                state.updates_listed_at = Some(listed_at);
                state.phase = Phase::Ready {
                    packages,
                    soft_errors,
                };
            }
            Ok(())
        }
        Event::ApplyProgress {
            source,
            package_hint,
            status_hint,
            progress,
            phase_label,
            stage_current,
            stage_total,
        } => match &mut state.phase {
            Phase::Running(running) => {
                if running.current_source != Some(source) {
                    running.reset_for_source(source);
                }
                if !running.is_applying()
                    && should_enter_apply(&phase_label, status_hint, package_hint.as_deref())
                {
                    running.enter_apply_pass();
                }
                let mut status_hint = status_hint;
                if running.is_applying() && status_hint == Some(PackageStatus::Downloading) {
                    status_hint = Some(PackageStatus::Installing);
                }
                if !phase_label.trim().is_empty()
                    && (!running.is_applying() || !phase_is_download(&phase_label))
                {
                    running.phase_label = phase_label;
                }
                running.current_source = Some(source);
                match (stage_current, stage_total) {
                    (Some(current), Some(total)) if total > 0 => {
                        running.stage = Some(StageCount { current, total });
                        running.overall_progress =
                            (f64::from(current) / f64::from(total)).clamp(0.0, 1.0);
                    }
                    (None, None) => {}
                    _ => {
                        running.stage = None;
                    }
                }
                if progress.is_some() {
                    running.work_progress = progress;
                }
                match apply_progress_hint(
                    &mut running.packages,
                    source,
                    package_hint.as_deref(),
                    status_hint,
                ) {
                    Some(idx) => {
                        running.current_name = Some(running.packages[idx].name.clone());
                        running.active_index = if running.packages[idx].status.is_done() {
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
                            let named_done = running.packages.iter().any(|p| {
                                p.source == source
                                    && p.matches_progress_name(name)
                                    && p.status.is_done()
                            });
                            if !named_done {
                                running.current_name = Some(name.to_string());
                                let still_current = running
                                    .active_index
                                    .and_then(|i| running.packages.get(i))
                                    .is_some_and(|p| p.matches_progress_name(name));
                                if !still_current {
                                    running.active_index = None;
                                }
                            }
                        }
                    }
                }
                if running.stage.is_none() && running.is_applying() {
                    running.overall_progress = overall_progress(&running.packages);
                }
                Ok(())
            }
            _ => Err(invalid(state, "ApplyProgress")),
        },
        Event::BackendApplyDone {
            source,
            success,
            needs_reboot,
            error,
        } => match &mut state.phase {
            Phase::Running(running) => {
                finalize_source(&mut running.packages, source, success);
                if needs_reboot {
                    running.needs_reboot = true;
                }
                if !success {
                    if !running.failed_sources.contains(&source) {
                        running.failed_sources.push(source);
                    }
                    if let Some(msg) = error.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                        if !running.failure_reasons.contains(&msg) {
                            running.failure_reasons.push(msg);
                        }
                    }
                }
                running.stage = None;
                running.overall_progress = overall_progress(&running.packages);
                Ok(())
            }
            _ => Err(invalid(state, "BackendApplyDone")),
        },
        Event::ApplyComplete { needs_reboot } => {
            let Phase::Running(running) = &mut state.phase else {
                return Err(invalid(state, "ApplyComplete"));
            };
            state.retry_apply_packages.clear();
            let mut packages = std::mem::take(&mut running.packages);
            let nr = running.needs_reboot;
            let failed_sources = std::mem::take(&mut running.failed_sources);
            let failure_reasons = std::mem::take(&mut running.failure_reasons);
            let duration = state
                .stopwatch
                .as_ref()
                .map(|s| s.elapsed_label())
                .unwrap_or_else(|| "?".into());
            for p in packages.iter_mut() {
                if !p.status.is_done() {
                    if failed_sources.contains(&p.source) {
                        p.status = PackageStatus::Failed;
                    } else {
                        p.status = PackageStatus::Completed;
                    }
                }
            }
            let failed = packages
                .iter()
                .filter(|p| p.status == PackageStatus::Failed)
                .count();
            let hard_fail = failed == packages.len() && !packages.is_empty();
            if hard_fail {
                state.apply_when_ready = false;
                let detail = if failure_reasons.is_empty() {
                    "All updates failed. Open View Log for details.".into()
                } else {
                    format!(
                        "{}\n\nOpen View Log for details.",
                        failure_reasons.join("\n")
                    )
                };
                state.phase = Phase::Failed {
                    title: "All updates failed".into(),
                    detail,
                    retry: RetryAction::CheckAgain,
                };
            } else {
                state.phase = Phase::Done {
                    packages,
                    duration,
                    needs_reboot: nr || needs_reboot,
                    failed,
                    failure_reasons,
                };
            }
            Ok(())
        }
        Event::Fail {
            title,
            detail,
            retry,
        } => {
            let preserved = if retry == RetryAction::RetryApply {
                match &mut state.phase {
                    Phase::Authenticating {
                        purpose: AuthPurpose::Apply,
                        packages,
                        ..
                    } if !packages.is_empty() => std::mem::take(packages),
                    Phase::Failed {
                        retry: RetryAction::RetryApply,
                        ..
                    } => std::mem::take(&mut state.retry_apply_packages),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            state.retry_apply_packages = preserved;
            state.apply_when_ready = false;
            state.phase = Phase::Failed {
                title,
                detail,
                retry,
            };
            Ok(())
        }
        Event::ResetToIdle => {
            state.retry_apply_packages.clear();
            let screen = match &state.phase {
                Phase::Done {
                    failed,
                    failure_reasons,
                    ..
                } => {
                    if *failed == 0 {
                        IdleScreen::Current {
                            checked_at: "just now".into(),
                            detail: None,
                        }
                    } else {
                        let detail = if !failure_reasons.is_empty() {
                            let summary = failure_reasons.join("; ");
                            if *failed == 1 {
                                format!("1 update did not finish: {summary}")
                            } else {
                                format!("{failed} updates did not finish: {summary}")
                            }
                        } else if *failed == 1 {
                            "1 update did not finish. Open View Log for details or check again."
                                .into()
                        } else {
                            format!(
                                "{failed} updates did not finish. Open View Log for details or check again."
                            )
                        };
                        IdleScreen::Warning {
                            title: "Some updates failed",
                            detail,
                            checked_at: Some("just now".into()),
                        }
                    }
                }
                Phase::Idle(screen) => screen.clone(),
                _ => IdleScreen::Fresh,
            };
            state.apply_when_ready = false;
            state.stopwatch = None;
            state.phase = Phase::Idle(screen);
            Ok(())
        }
        Event::StopApply { during_transaction } => {
            let Phase::Running(running) = &mut state.phase else {
                return Err(invalid(state, "StopApply"));
            };
            let mut packages = std::mem::take(&mut running.packages);
            state.apply_when_ready = false;
            state.retry_apply_packages.clear();
            if during_transaction {
                state.phase = Phase::Failed {
                    title: "Update stopped during install".into(),
                    detail: "Stopping dnf while it is applying packages can leave the rpm database half-applied. Check `dnf history` before running Update All again.".into(),
                    retry: RetryAction::CheckAgain,
                };
            } else {
                for package in packages.iter_mut() {
                    package.status = PackageStatus::Pending;
                }
                state.phase = Phase::Ready {
                    packages,
                    soft_errors: vec![
                        "Stopped before install. Downloaded packages were not applied.".into(),
                    ],
                };
            }
            Ok(())
        }
        Event::CancelAuth => {
            let Phase::Authenticating {
                purpose,
                packages,
                last_checked,
            } = &mut state.phase
            else {
                return Err(invalid(state, "CancelAuth"));
            };
            let purpose = *purpose;
            let packages = std::mem::take(packages);
            let last_checked = last_checked.take();
            state.apply_when_ready = false;
            state.stopwatch = None;
            state.retry_apply_packages.clear();
            if purpose == AuthPurpose::Apply && !packages.is_empty() {
                state.phase = Phase::Ready {
                    packages,
                    soft_errors: Vec::new(),
                };
            } else {
                state.phase = Phase::Idle(match last_checked {
                    Some(checked_at) => IdleScreen::Current {
                        checked_at,
                        detail: None,
                    },
                    None => IdleScreen::Fresh,
                });
            }
            Ok(())
        }
    }
}

fn idle_checked_at(screen: &IdleScreen) -> Option<String> {
    match screen {
        IdleScreen::Fresh | IdleScreen::Warning { .. } => None,
        IdleScreen::Current { checked_at, .. } => Some(checked_at.clone()),
    }
}

/// Purpose for a helper `SessionStarted` event.
/// Authenticating uses the purpose stored on the phase. A check or apply that
/// has already moved on stays on that purpose so a late SessionStarted is the
/// idempotent `SessionReady` arm. Every other phase is ignored.
pub fn session_started_purpose(phase: &Phase) -> Option<AuthPurpose> {
    match phase {
        Phase::Authenticating { purpose, .. } => Some(*purpose),
        Phase::Checking { .. } => Some(AuthPurpose::Check),
        Phase::Running(_) => Some(AuthPurpose::Apply),
        Phase::Idle(_) | Phase::Ready { .. } | Phase::Done { .. } | Phase::Failed { .. } => None,
    }
}

fn phase_is_download(label: &str) -> bool {
    label.to_ascii_lowercase().contains("download")
}

fn should_enter_apply(label: &str, status: Option<PackageStatus>, package: Option<&str>) -> bool {
    if phase_is_download(label) {
        return false;
    }
    let named = package.is_some_and(|s| !s.trim().is_empty());
    let label_l = label.to_ascii_lowercase();
    if label_l.contains("transaction")
        || label_l.contains("scriptlet")
        || label_l.contains("cleanup")
        || label_l.contains("verif")
        || label_l.contains("staging")
        || label_l.contains("writing ostree")
    {
        return true;
    }
    // The transaction preview prints a bare "Upgrading:" / "Installing:" header.
    // That is not the apply pass. A real step names a package or a backend phase
    // such as "Installing firmware".
    if !named && (label_l == "installing" || label_l == "upgrading") {
        return false;
    }
    if label_l.contains("install")
        || label_l.contains("upgrad")
        || label_l.contains("eras")
        || label_l.contains("remov")
    {
        return true;
    }
    named
        && matches!(
            status,
            Some(PackageStatus::Installing) | Some(PackageStatus::Completed)
        )
}

fn reset_live_packages(packages: &mut [Package]) {
    for package in packages.iter_mut() {
        if !package.status.is_done() {
            package.status = PackageStatus::Pending;
        }
    }
}

fn invalid(state: &AppState, event: &str) -> TransitionError {
    TransitionError::Invalid {
        from: phase_name(&state.phase).into(),
        event: event.into(),
    }
}

/// Phase name for tests / logging.
pub fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle(_) => "Idle",
        Phase::Authenticating { .. } => "Authenticating",
        Phase::Checking { .. } => "Checking",
        Phase::Ready { .. } => "Ready",
        Phase::Running(_) => "Running",
        Phase::Done { .. } => "Done",
        Phase::Failed { .. } => "Failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_enter_apply_distinguishes_download_preview_and_apply_steps() {
        assert!(phase_is_download("Downloading packages"));
        assert!(!phase_is_download("Installing"));

        // Download phase labels stay in the download pass even with a package/status hint.
        assert!(!should_enter_apply(
            "Downloading packages",
            Some(PackageStatus::Downloading),
            Some("kernel"),
        ));
        assert!(!should_enter_apply(
            "Downloading updates",
            Some(PackageStatus::Installing),
            Some("kernel"),
        ));

        // Bare "Installing"/"Upgrading" section headers in the transaction preview must not enter apply.
        assert!(!should_enter_apply(
            "Installing",
            Some(PackageStatus::Installing),
            None,
        ));
        assert!(!should_enter_apply("Upgrading", None, Some("   ")));

        // Real transaction phases enter apply even without a package name.
        assert!(should_enter_apply("Running transaction", None, None));
        assert!(should_enter_apply("Running scriptlet", None, None));
        assert!(should_enter_apply("Cleanup", None, None));
        assert!(should_enter_apply("Verifying", None, None));
        assert!(should_enter_apply("Installing firmware", None, None));
        assert!(should_enter_apply("Staging deployment", None, None));
        assert!(should_enter_apply("Writing OSTree commit", None, None));

        // Named packages enter apply on install/upgrade labels or apply statuses.
        assert!(should_enter_apply(
            "Installing",
            Some(PackageStatus::Downloading),
            Some("kernel"),
        ));
        assert!(should_enter_apply("Upgrading", None, Some("glibc")));
        assert!(should_enter_apply("Removing", None, Some("old-pkg")));
        assert!(should_enter_apply(
            "Applying",
            Some(PackageStatus::Completed),
            Some("org.app.Foo"),
        ));
        assert!(!should_enter_apply(
            "Fetching",
            Some(PackageStatus::Downloading),
            Some("org.app.Foo"),
        ));
    }

    #[test]
    fn reset_live_packages_preserves_done_statuses() {
        let mut p_done = Package::new_dnf("glibc", "x86_64", "1.0", "updates");
        p_done.status = PackageStatus::Completed;
        let mut p_failed = Package::new_dnf("fw", "x86_64", "1.0", "updates");
        p_failed.status = PackageStatus::Failed;
        let mut p_dl = Package::new_dnf("kernel", "x86_64", "1.0", "updates");
        p_dl.status = PackageStatus::Downloading;
        let mut p_inst = Package::new_dnf("mesa", "x86_64", "1.0", "updates");
        p_inst.status = PackageStatus::Installing;

        let mut packages = vec![p_done, p_failed, p_dl, p_inst];
        reset_live_packages(&mut packages);

        assert_eq!(packages[0].status, PackageStatus::Completed);
        assert_eq!(packages[1].status, PackageStatus::Failed);
        assert_eq!(packages[2].status, PackageStatus::Pending);
        assert_eq!(packages[3].status, PackageStatus::Pending);
    }

    #[test]
    fn selective_apply_filters_ready_subset_tracks_mode_and_rejects_empty() {
        let openssl = Package::new_dnf("openssl", "x86_64", "3.2.2-3", "updates");
        let vim = Package::new_dnf("vim-enhanced", "x86_64", "9.1.0-1", "updates");
        let mut state = AppState {
            phase: Phase::Ready {
                packages: vec![openssl.clone(), vim.clone()],
                soft_errors: vec![],
            },
            ..Default::default()
        };

        // Empty selection must be rejected cleanly.
        assert!(reduce(
            &mut state,
            Event::StartSelectiveApply {
                selected_ids: vec![],
                mode: ApplyMode::OfflineOnReboot,
            }
        )
        .is_err());

        reduce(
            &mut state,
            Event::StartSelectiveApply {
                selected_ids: vec![openssl.id.clone()],
                mode: ApplyMode::OfflineOnReboot,
            },
        )
        .expect("selective apply transitions");
        assert_eq!(state.apply_mode, ApplyMode::OfflineOnReboot);
        match &state.phase {
            Phase::Authenticating {
                purpose: AuthPurpose::Apply,
                packages,
                ..
            } => {
                assert_eq!(packages.len(), 1);
                assert_eq!(packages[0].id, openssl.id);
            }
            other => panic!("unexpected phase: {other:?}"),
        }

        // Recovery via CancelAuth keeps only the previously selected subset if cancelled in Auth?
        // Wait: StartSelectiveApply preserves only selected items into Authenticating.
        // Advancing into Running and cancelling before transaction restores Ready safely.
        reduce(
            &mut state,
            Event::SessionReady {
                purpose: AuthPurpose::Apply,
            },
        )
        .expect("session ready");
        reduce(
            &mut state,
            Event::StopApply {
                during_transaction: false,
            },
        )
        .expect("stop before install");
        match &state.phase {
            Phase::Ready {
                packages,
                soft_errors,
            } => {
                assert_eq!(packages.len(), 1);
                assert_eq!(packages[0].id, openssl.id);
                assert!(soft_errors[0].starts_with("Stopped before install"));
            }
            other => panic!("unexpected phase: {other:?}"),
        }
    }
}
