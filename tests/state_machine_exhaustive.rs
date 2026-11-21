//! Exhaustive transition tests for the updater state machine.
//! These are the safety net for UI flow integrity.

use std::time::{Duration, SystemTime};

use fedora_updater::backend::parse_progress;
use fedora_updater::model::{
    IdleScreen, Package, PackageStatus, RetryAction, RunPass, StageCount, UpdateSource,
};
use fedora_updater::orchestrator::REBOOT_FAILED_TITLE;
use fedora_updater::presenter::idle_chrome;
use fedora_updater::state::{
    phase_name, reduce, session_started_purpose, AppState, Event, Phase, RunningState,
    UPDATE_LIST_MAX_AGE,
};
use fedora_updater::{AuthPurpose, TransitionError};

fn apply(mut state: AppState, event: Event) -> Result<AppState, TransitionError> {
    reduce(&mut state, event)?;
    Ok(state)
}

fn pkg(name: &str, source: UpdateSource) -> Package {
    let mut p = Package::new_dnf(name, "x86_64", "1.0", "updates");
    p.source = source;
    if source != UpdateSource::Dnf {
        p.id = format!("{source:?}:{name}");
    }
    p
}

fn dnf(name: &str) -> Package {
    Package::new_dnf(name, "x86_64", "1.0-1.fc42", "updates")
}

fn flatpak(name: &str) -> Package {
    Package {
        id: format!("flatpak:user:{name}"),
        name: name.into(),
        arch: String::new(),
        version: "1.0".into(),
        repo: "flathub".into(),
        size_bytes: None,
        old_version: None,
        status: PackageStatus::Pending,
        source: UpdateSource::FlatpakUser,
        detail: "flathub · user".into(),
    }
}

fn firmware(name: &str) -> Package {
    Package {
        id: format!("fwupd:{name}"),
        name: name.into(),
        arch: String::new(),
        version: "2.0".into(),
        repo: "LVFS".into(),
        size_bytes: None,
        old_version: Some("1.0".into()),
        status: PackageStatus::Pending,
        source: UpdateSource::Firmware,
        detail: "1.0 → 2.0".into(),
    }
}

fn assert_phase(s: &AppState, name: &str) {
    assert_eq!(phase_name(&s.phase), name);
}

fn ready_state(packages: Vec<Package>) -> AppState {
    AppState {
        phase: Phase::Ready {
            packages,
            soft_errors: vec![],
        },
        ..Default::default()
    }
}

fn running_dnf(packages: Vec<Package>) -> AppState {
    let mut running = RunningState::new(packages);
    running.current_source = Some(UpdateSource::Dnf);
    AppState {
        phase: Phase::Running(running),
        ..Default::default()
    }
}

/// Ready → StartApply → SessionReady, exercising real transitions into `Phase::Running`.
fn started_apply(packages: Vec<Package>) -> AppState {
    let s = apply(ready_state(packages), Event::StartApply).unwrap();
    apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Apply,
        },
    )
    .unwrap()
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

fn status_of(packages: &[Package], name: &str) -> PackageStatus {
    packages
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .status
}

/// Same skip the orchestrator uses: a section header carries no progress.
fn apply_dnf_output(state: AppState, line: &str) -> AppState {
    let hint = parse_progress(UpdateSource::Dnf, line);
    if hint.package_name.is_none()
        && hint.status.is_none()
        && hint.progress.is_none()
        && hint.phase_label.is_none()
    {
        return state;
    }
    apply(
        state,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: hint.package_name,
            status_hint: hint.status,
            progress: hint.progress,
            phase_label: hint.phase_label.unwrap_or_default(),
            stage_current: hint.stage_current,
            stage_total: hint.stage_total,
        },
    )
    .unwrap_or_else(|e| panic!("progress line {line:?} rejected: {e}"))
}

fn assert_download_without_stage(s: &AppState) {
    match &s.phase {
        Phase::Running(running)
            if running.pass == RunPass::Download && running.stage.is_none() => {}
        other => panic!("expected Download with no stage, got {other:?}"),
    }
}

#[test]
fn happy_path_multi_source() {
    let s = AppState::default();
    assert_phase(&s, "Idle");

    let s = apply(s, Event::StartCheck { then_apply: false }).unwrap();
    assert_phase(&s, "Authenticating");

    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    assert_phase(&s, "Checking");

    let pkgs = vec![dnf("kernel"), flatpak("org.app.Foo"), firmware("UEFI")];
    let s = apply(
        s,
        Event::BackendCheckDone {
            source: UpdateSource::Dnf,
            packages: vec![dnf("kernel")],
            soft_error: None,
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Checking {
            checked_sources, ..
        } => assert_eq!(checked_sources, &[UpdateSource::Dnf]),
        _ => unreachable!(),
    }
    let s = apply(
        s,
        Event::BackendCheckDone {
            source: UpdateSource::FlatpakUser,
            packages: vec![flatpak("org.app.Foo")],
            soft_error: None,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::BackendCheckDone {
            source: UpdateSource::Firmware,
            packages: vec![firmware("UEFI")],
            soft_error: Some("minor warning".into()),
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Checking {
            checked_sources, ..
        } => assert_eq!(
            checked_sources,
            &[
                UpdateSource::Dnf,
                UpdateSource::FlatpakUser,
                UpdateSource::Firmware
            ]
        ),
        _ => unreachable!(),
    }
    assert_eq!(s.packages().len(), 3);

    let s = apply(
        s,
        Event::CheckComplete {
            packages: pkgs.clone(),
            soft_errors: vec!["minor warning".into()],
            listed_at: SystemTime::UNIX_EPOCH,
        },
    )
    .unwrap();
    assert_phase(&s, "Ready");
    match &s.phase {
        Phase::Ready { soft_errors, .. } => assert_eq!(soft_errors.len(), 1),
        _ => unreachable!(),
    }

    // Apply order: DNF before Flatpak before Firmware (sort_for_apply)
    let names: Vec<_> = s
        .packages()
        .iter()
        .map(|p| p.source.apply_order())
        .collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]));

    let s = apply(s, Event::StartApply).unwrap();
    assert_phase(&s, "Authenticating");

    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Apply,
        },
    )
    .unwrap();
    assert_phase(&s, "Running");

    let mut s = s;
    for (source, label) in [
        (UpdateSource::Dnf, "System"),
        (UpdateSource::FlatpakUser, "Apps"),
        (UpdateSource::Firmware, "Firmware"),
    ] {
        s = apply(
            s,
            Event::ApplyProgress {
                source,
                package_hint: None,
                status_hint: Some(PackageStatus::Installing),
                progress: Some(0.5),
                phase_label: format!("Updating {label}"),
                stage_current: None,
                stage_total: None,
            },
        )
        .unwrap();
        s = apply(
            s,
            Event::BackendApplyDone {
                source,
                success: true,
                needs_reboot: source == UpdateSource::Dnf || source == UpdateSource::Firmware,
                error: None,
            },
        )
        .unwrap();
    }

    let s = apply(
        s,
        Event::ApplyComplete {
            needs_reboot: false,
        },
    )
    .unwrap();
    assert_phase(&s, "Done");
    match s.phase {
        Phase::Done {
            needs_reboot,
            failed,
            packages,
            ..
        } => {
            assert!(needs_reboot);
            assert_eq!(failed, 0);
            assert_eq!(packages.len(), 3);
            assert!(packages
                .iter()
                .all(|p| p.status == PackageStatus::Completed));
        }
        _ => unreachable!(),
    }
}

#[test]
fn illegal_transitions() {
    let s = AppState::default();
    assert!(apply(s.clone(), Event::StartApply).is_err());
    assert!(apply(
        s.clone(),
        Event::SessionReady {
            purpose: AuthPurpose::Check
        }
    )
    .is_err());
    assert!(apply(
        s.clone(),
        Event::CheckComplete {
            packages: vec![],
            soft_errors: vec![],
            listed_at: SystemTime::UNIX_EPOCH,
        }
    )
    .is_err());
    assert!(apply(
        s.clone(),
        Event::BackendApplyDone {
            source: UpdateSource::Dnf,
            success: true,
            needs_reboot: false,
            error: None,
        }
    )
    .is_err());
    assert!(apply(
        s,
        Event::ApplyComplete {
            needs_reboot: false
        }
    )
    .is_err());
}

#[test]
fn cannot_apply_empty_ready() {
    let s = ready_state(vec![]);
    assert!(apply(s, Event::StartApply).is_err());
}

#[test]
fn in_app_cancel_auth_returns_idle() {
    let s = apply(AppState::default(), Event::StartCheck { then_apply: false }).unwrap();
    assert_phase(&s, "Authenticating");
    let s = apply(s, Event::CancelAuth).unwrap();
    assert_phase(&s, "Idle");
    assert!(apply(AppState::default(), Event::CancelAuth).is_err());
}

#[test]
fn auth_cancel_style_fail() {
    let s = apply(AppState::default(), Event::StartCheck { then_apply: false }).unwrap();
    let s = apply(
        s,
        Event::Fail {
            title: "Authentication cancelled".into(),
            detail: "polkit denied".into(),
            retry: fedora_updater::RetryAction::CheckAgain,
        },
    )
    .unwrap();
    assert_phase(&s, "Failed");
    let s = apply(s, Event::ResetToIdle).unwrap();
    assert_phase(&s, "Idle");
}

#[test]
fn progress_marks_package_and_completes_previous() {
    let s = AppState {
        phase: Phase::Running(RunningState {
            phase_label: "x".into(),
            ..RunningState::new(vec![dnf("openssl"), dnf("kernel")])
        }),
        ..Default::default()
    };
    let s = apply(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("openssl".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.3),
            phase_label: "Installing".into(),
            stage_current: None,
            stage_total: None,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("kernel".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.1),
            phase_label: "Installing".into(),
            stage_current: None,
            stage_total: None,
        },
    )
    .unwrap();
    let pkgs = s.packages();
    assert_eq!(pkgs[0].status, PackageStatus::Completed);
    assert_eq!(pkgs[1].status, PackageStatus::Installing);
}

#[test]
fn unnamed_progress_does_not_burn_down_or_poke_first_package() {
    let s = AppState {
        phase: Phase::Running(RunningState {
            phase_label: "x".into(),
            current_source: Some(UpdateSource::Dnf),
            ..RunningState::new(vec![dnf("abrt"), dnf("kernel")])
        }),
        ..Default::default()
    };
    let s = apply(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: None,
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.5),
            phase_label: "Downloading packages".into(),
            stage_current: None,
            stage_total: None,
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Running(running) => {
            assert!(running
                .packages
                .iter()
                .all(|p| p.status == PackageStatus::Pending));
            assert_eq!(running.active_index, None);
            assert_eq!(running.current_name.as_deref(), None);
            assert_eq!(running.work_progress, Some(0.5));
            assert!((running.overall_progress - 0.0).abs() < f64::EPSILON);
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn kernel_hint_does_not_complete_kernel_core() {
    let s = AppState {
        phase: Phase::Running(RunningState {
            phase_label: "x".into(),
            ..RunningState::new(vec![dnf("kernel"), dnf("kernel-core")])
        }),
        ..Default::default()
    };
    let s = apply(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("kernel".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.2),
            phase_label: "Installing".into(),
            stage_current: None,
            stage_total: None,
        },
    )
    .unwrap();
    let pkgs = s.packages();
    assert_eq!(pkgs[0].status, PackageStatus::Installing);
    assert_eq!(pkgs[1].status, PackageStatus::Pending);
}

#[test]
fn verify_pass_does_not_move_current_back_to_completed() {
    let mut firefox = dnf("firefox");
    firefox.status = PackageStatus::Completed;
    let mut kernel = dnf("kernel");
    kernel.status = PackageStatus::Installing;
    let s = AppState {
        phase: Phase::Running(RunningState {
            packages: vec![firefox, kernel],
            active_index: Some(1),
            overall_progress: 0.5,
            phase_label: "Installing".into(),
            current_source: Some(UpdateSource::Dnf),
            current_name: Some("kernel".into()),
            work_progress: Some(0.5),
            pass: fedora_updater::model::RunPass::Apply,
            stage: None,
            needs_reboot: false,
            failed_sources: Vec::new(),
            failure_reasons: Vec::new(),
        }),
        ..Default::default()
    };
    let s = apply(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("firefox".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.9),
            phase_label: "Verifying".into(),
            stage_current: None,
            stage_total: None,
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Running(running) => {
            assert_eq!(running.packages[0].status, PackageStatus::Completed);
            assert_eq!(running.packages[1].status, PackageStatus::Installing);
            assert_eq!(running.active_index, Some(1));
            assert_eq!(running.current_name.as_deref(), Some("kernel"));
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn source_switch_progress_does_not_mark_first_package() {
    let s = AppState {
        phase: Phase::Running(RunningState {
            phase_label: "x".into(),
            current_name: Some("stale".into()),
            work_progress: Some(0.3),
            ..RunningState::new(vec![dnf("abrt"), dnf("kernel")])
        }),
        ..Default::default()
    };
    let s = apply(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: None,
            status_hint: None,
            progress: None,
            phase_label: "Updating System".into(),
            stage_current: None,
            stage_total: None,
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Running(running) => {
            assert!(running
                .packages
                .iter()
                .all(|p| p.status == PackageStatus::Pending));
            assert_eq!(running.active_index, None);
            assert_eq!(running.current_name, None);
            assert_eq!(running.work_progress, None);
            assert_eq!(running.phase_label, "Updating System");
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn soft_errors_on_empty_check_surface_message() {
    let s = apply(AppState::default(), Event::StartCheck { then_apply: false }).unwrap();
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::CheckComplete {
            packages: vec![],
            soft_errors: vec!["fwupd not available".into()],
            listed_at: SystemTime::UNIX_EPOCH,
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Idle(screen @ fedora_updater::IdleScreen::Warning { title, detail, .. }) => {
            assert_eq!(*title, "Could not check all sources");
            assert!(detail.contains("fwupd not available"), "{detail}");
            let chrome = idle_chrome(screen);
            assert_ne!(chrome.title, "System is up to date");
            assert_eq!(chrome.icon, "dialog-warning-symbolic");
            assert!(chrome.show_view_log);
        }
        other => panic!("expected warning idle with soft-error detail, got {other:?}"),
    }
}

#[test]
fn dnf_failure_does_not_block_done_if_others_ok() {
    let s = ready_state(vec![dnf("kernel"), flatpak("app")]);
    let s = apply(s, Event::StartApply).unwrap();
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Apply,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::BackendApplyDone {
            source: UpdateSource::Dnf,
            success: false,
            needs_reboot: false,
            error: Some("System: RPM database is locked".into()),
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::BackendApplyDone {
            source: UpdateSource::FlatpakUser,
            success: true,
            needs_reboot: false,
            error: None,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::ApplyComplete {
            needs_reboot: false,
        },
    )
    .unwrap();
    match s.phase {
        Phase::Done {
            failed, packages, ..
        } => {
            assert_eq!(failed, 1);
            assert!(packages
                .iter()
                .any(|p| p.source == UpdateSource::Dnf && p.status == PackageStatus::Failed));
            assert!(packages.iter().any(|p| {
                p.source == UpdateSource::FlatpakUser && p.status == PackageStatus::Completed
            }));
        }
        other => panic!("expected Done, got {}", phase_name(&other)),
    }
}

#[test]
fn session_ready_wrong_purpose_rejected() {
    let s = apply(AppState::default(), Event::StartCheck { then_apply: false }).unwrap();
    assert!(apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Apply
        }
    )
    .is_err());
}

#[test]
fn a_release_note_stays_across_a_check_and_does_not_change_phase() {
    let s = AppState::default();
    let s = apply(
        s,
        Event::ReleaseNoted {
            version: Some(45),
            days_since_release: Some(12),
        },
    )
    .unwrap();
    assert_eq!(s.release_upgrade, Some(45));
    assert_eq!(s.release_age_days, Some(12));
    assert!(matches!(s.phase, Phase::Idle(_)));

    let s = apply(s, Event::StartCheck { then_apply: false }).unwrap();
    assert_eq!(s.release_upgrade, Some(45));
    assert_eq!(s.release_age_days, Some(12));
    assert!(matches!(s.phase, Phase::Authenticating { .. }));

    let s = apply(
        s,
        Event::ReleaseNoted {
            version: Some(45),
            days_since_release: None,
        },
    )
    .unwrap();
    assert_eq!(s.release_upgrade, Some(45));
    assert_eq!(s.release_age_days, None);

    let s = apply(
        s,
        Event::ReleaseNoted {
            version: None,
            days_since_release: Some(12),
        },
    )
    .unwrap();
    assert_eq!(s.release_upgrade, None);
    assert_eq!(s.release_age_days, None);
    assert!(matches!(s.phase, Phase::Authenticating { .. }));
}

#[test]
fn check_flow_no_updates() {
    let s = AppState::default();
    let s = apply(s, Event::StartCheck { then_apply: false }).unwrap();
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::CheckComplete {
            packages: vec![],
            soft_errors: vec![],
            listed_at: SystemTime::UNIX_EPOCH,
        },
    )
    .unwrap();
    match s.phase {
        Phase::Idle(IdleScreen::Current {
            detail: None,
            checked_at,
        }) => {
            assert_eq!(checked_at, "just now");
            assert_eq!(
                idle_chrome(&IdleScreen::Current {
                    checked_at,
                    detail: None,
                })
                .description
                .as_deref(),
                Some("Last checked just now")
            );
        }
        other => panic!("expected Current with no detail, got {other:?}"),
    }
}

#[test]
fn all_failed_goes_to_failed_phase() {
    let s = ready_state(vec![pkg("kernel", UpdateSource::Dnf)]);
    let s = apply(s, Event::StartApply).unwrap();
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Apply,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::BackendApplyDone {
            source: UpdateSource::Dnf,
            success: false,
            needs_reboot: false,
            error: None,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::ApplyComplete {
            needs_reboot: false,
        },
    )
    .unwrap();
    match s.phase {
        Phase::Failed {
            detail,
            retry: RetryAction::CheckAgain,
            ..
        } => {
            assert_eq!(detail, "All updates failed. Open View Log for details.");
        }
        other => panic!("expected Failed CheckAgain, got {other:?}"),
    }
}

#[test]
fn done_resets_to_current_idle() {
    let s = AppState {
        phase: Phase::Done {
            packages: vec![],
            duration: "1s".into(),
            needs_reboot: false,
            failed: 0,
            failure_reasons: vec![],
        },
        ..Default::default()
    };
    let s = apply(s, Event::ResetToIdle).unwrap();
    let Phase::Idle(IdleScreen::Current {
        checked_at,
        detail: None,
    }) = &s.phase
    else {
        panic!("expected Current, got {:?}", s.phase);
    };
    let description = idle_chrome(&IdleScreen::Current {
        checked_at: checked_at.clone(),
        detail: None,
    })
    .description;
    assert_eq!(
        description.as_deref(),
        Some(format!("Last checked {checked_at}").as_str())
    );
}

#[test]
fn empty_phase_label_does_not_clobber_current_phase() {
    let s = ready_state(vec![pkg("System Firmware", UpdateSource::Firmware)]);
    let s = apply(s, Event::StartApply).unwrap();
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Apply,
        },
    )
    .unwrap();
    let s = apply(
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
    let s = apply(
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
        Phase::Running(running) => {
            assert_eq!(running.phase_label, "Installing firmware");
            assert_eq!(
                running.pass,
                RunPass::Apply,
                "Installing firmware is a real apply step and must leave the download pass"
            );
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn download_pass_does_not_finish_packages_and_apply_restarts_the_bar() {
    let mut s = running_dnf(vec![dnf("wireplumber"), dnf("glibc"), dnf("kernel")]);

    for (name, cur) in [("wireplumber", 1), ("glibc", 2), ("kernel", 3)] {
        s = apply(
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
        Phase::Running(running) => {
            assert!(
                running
                    .packages
                    .iter()
                    .all(|p| p.status != PackageStatus::Completed),
                "downloads must not check packages off, got {:?}",
                running.packages
            );
            assert_eq!(running.packages[0].status, PackageStatus::Pending);
            assert_eq!(running.packages[1].status, PackageStatus::Pending);
            assert_eq!(running.packages[2].status, PackageStatus::Downloading);
            assert_eq!(running.pass, RunPass::Download);
            assert_eq!(
                running.stage,
                Some(StageCount {
                    current: 3,
                    total: 3
                })
            );
            assert!(
                (running.overall_progress - 1.0).abs() < f64::EPSILON,
                "top bar follows the download count, got {}",
                running.overall_progress
            );
        }
        other => panic!("expected Running, got {other:?}"),
    }

    s = apply(
        s,
        dnf_progress(None, None, None, "Running transaction", None, None),
    )
    .unwrap();
    match &s.phase {
        Phase::Running(running) => {
            assert_eq!(running.pass, RunPass::Apply);
            assert!(running
                .packages
                .iter()
                .all(|p| p.status == PackageStatus::Pending));
            assert!(running.work_progress.is_none());
            assert!(
                (running.overall_progress - 0.0).abs() < f64::EPSILON,
                "apply pass restarts the top bar, got {}",
                running.overall_progress
            );
        }
        other => panic!("expected Running, got {other:?}"),
    }

    s = apply(
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
        Phase::Running(running) => {
            assert_eq!(running.pass, RunPass::Apply);
            assert_eq!(
                status_of(&running.packages, "glibc"),
                PackageStatus::Installing
            );
            assert_eq!(
                status_of(&running.packages, "wireplumber"),
                PackageStatus::Pending
            );
            assert_eq!(status_of(&running.packages, "kernel"), PackageStatus::Pending);
            assert_eq!(running.work_progress, Some(1.0));
            assert_eq!(
                running.stage,
                Some(StageCount {
                    current: 16,
                    total: 137
                })
            );
            assert!(
                (running.overall_progress - 16.0 / 137.0).abs() < 1e-9,
                "item 100% must not replace the transaction count, got {}",
                running.overall_progress
            );
        }
        other => panic!("expected Running, got {other:?}"),
    }

    s = apply(
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
        Phase::Running(running) => {
            assert_eq!(
                status_of(&running.packages, "glibc"),
                PackageStatus::Completed
            );
            assert_eq!(
                status_of(&running.packages, "kernel"),
                PackageStatus::Installing
            );
            assert_eq!(running.work_progress, Some(0.4));
            assert!((running.overall_progress - 17.0 / 137.0).abs() < 1e-9);
        }
        other => panic!("expected Running, got {other:?}"),
    }

    s = apply(
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
        Phase::Running(running) => {
            assert_eq!(
                status_of(&running.packages, "kernel"),
                PackageStatus::Installing
            );
            assert_eq!(running.active_index, None);
            assert_eq!(running.current_name.as_deref(), Some("libswscale-free"));
            assert!((running.overall_progress - 18.0 / 137.0).abs() < 1e-9);
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn firmware_success_progress_completes_the_live_item() {
    let s = ready_state(vec![pkg("System Firmware", UpdateSource::Firmware)]);
    let s = apply(s, Event::StartApply).unwrap();
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Apply,
        },
    )
    .unwrap();
    let s = apply(
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
        Phase::Running(running) => {
            assert_eq!(running.packages[0].status, PackageStatus::Completed);
            assert!((running.overall_progress - 1.0).abs() < f64::EPSILON);
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn ready_list_expires_after_24_hours() {
    let listed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let s = apply(AppState::default(), Event::StartCheck { then_apply: false }).unwrap();
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    let s = apply(
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
    let s = AppState {
        updates_listed_at: Some(SystemTime::UNIX_EPOCH),
        phase: Phase::Checking {
            packages_so_far: vec![],
            soft_errors: vec![],
            checked_sources: vec![],
        },
        ..Default::default()
    };
    let s = apply(
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
    let idle = AppState {
        updates_listed_at: Some(SystemTime::UNIX_EPOCH),
        phase: Phase::Idle(IdleScreen::Fresh),
        ..Default::default()
    };
    assert!(!idle.update_list_needs_refresh(SystemTime::now()));

    let ready = AppState {
        updates_listed_at: Some(SystemTime::UNIX_EPOCH),
        phase: Phase::Ready {
            packages: vec![pkg("kernel", UpdateSource::Dnf)],
            soft_errors: vec![],
        },
        ..Default::default()
    };
    assert!(ready.update_list_needs_refresh(SystemTime::now()));

    let s = apply(ready, Event::StartCheck { then_apply: false }).unwrap();
    assert!(s.updates_listed_at.is_none());
    assert!(!s.update_list_needs_refresh(SystemTime::now()));
}

#[test]
fn preview_ready_without_timestamp_can_still_apply() {
    let s = ready_state(vec![pkg("kernel", UpdateSource::Dnf)]);
    assert!(!s.update_list_needs_refresh(SystemTime::now()));
    assert!(apply(s, Event::StartApply).is_ok());
}

#[test]
fn transaction_preview_does_not_mark_downloads_complete() {
    let packages = vec![
        Package::new_dnf("brave-browser", "x86_64", "1.99.55-1", "brave-browser"),
        Package::new_dnf("mesa-dri-drivers", "i686", "26.2.6-1.fc44", "updates"),
        Package::new_dnf("mesa-dri-drivers", "x86_64", "26.2.6-1.fc44", "updates"),
        Package::new_dnf("tzdata", "noarch", "2026f-1.fc44", "updates"),
        Package::new_dnf("tzdata-java", "noarch", "2026f-1.fc44", "updates"),
        Package::new_dnf("grok-bot", "x86_64", "0.79.2-1", "grok-bot"),
    ];
    let mut s = running_dnf(packages);
    let lines = [
        "Upgrading:",
        "Installing:",
        "[1/5] brave-browser-1.99.55-1.x86_64 100% | 2.0 MiB/s | 20.0 MiB | 00:00",
        "[2/5] mesa-dri-drivers-26.2.6-1.fc44.i686 100% | 2.0 MiB/s | 8.0 MiB | 00:00",
        "[3/5] mesa-dri-drivers-26.2.6-1.fc44.x86_64 100% | 2.0 MiB/s | 9.0 MiB | 00:00",
        "[4/5] tzdata-2026f-1.fc44.noarch 100% | 1.0 MiB/s | 1.0 MiB | 00:00",
        "[5/5] tzdata-java-2026f-1.fc44.noarch 100% | 1.0 MiB/s | 200.0 KiB | 00:00",
    ];
    for line in lines {
        s = apply_dnf_output(s, line);
    }
    match &s.phase {
        Phase::Running(running) => {
            let packages = &running.packages;
            let completed = packages.iter().filter(|p| p.status.is_done()).count();
            assert_eq!(completed, 0, "downloads must stay unchecked: {packages:?}");
            assert_eq!(running.pass, RunPass::Download);
            assert_eq!(running.phase_label, "Downloading packages");
            assert_eq!(
                running.stage,
                Some(StageCount {
                    current: 5,
                    total: 5
                })
            );
            assert_eq!(running.current_name.as_deref(), Some("tzdata-java"));
            assert_eq!(running.work_progress, Some(1.0));
            assert_eq!(
                status_of(packages, "tzdata-java"),
                PackageStatus::Downloading
            );
            assert_eq!(status_of(packages, "grok-bot"), PackageStatus::Pending);
            assert_eq!(
                packages
                    .iter()
                    .filter(|p| p.name == "mesa-dri-drivers")
                    .filter(|p| p.status == PackageStatus::Completed)
                    .count(),
                0
            );
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn bare_installing_status_does_not_start_the_apply_pass() {
    let mut s = running_dnf(vec![dnf("tzdata-java")]);
    s = apply(
        s,
        dnf_progress(
            None,
            Some(PackageStatus::Installing),
            None,
            "Installing",
            None,
            None,
        ),
    )
    .unwrap();
    s = apply(
        s,
        dnf_progress(
            None,
            Some(PackageStatus::Installing),
            None,
            "Upgrading",
            None,
            None,
        ),
    )
    .unwrap();
    match &s.phase {
        Phase::Running(running) if running.pass == RunPass::Download => {}
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn stage_count_requires_a_positive_total() {
    let s = started_apply(vec![dnf("glibc")]);
    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.2),
            "Downloading packages",
            Some(1),
            Some(0),
        ),
    )
    .unwrap();
    assert_download_without_stage(&s);

    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.2),
            "Downloading packages",
            Some(2),
            None,
        ),
    )
    .unwrap();
    assert_download_without_stage(&s);

    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.2),
            "Downloading packages",
            None,
            Some(4),
        ),
    )
    .unwrap();
    assert_download_without_stage(&s);

    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.5),
            "Downloading packages",
            Some(2),
            Some(4),
        ),
    )
    .unwrap();
    match &s.phase {
        Phase::Running(running) => {
            assert_eq!(
                running.stage,
                Some(StageCount {
                    current: 2,
                    total: 4
                })
            );
        }
        other => panic!("expected Running, got {other:?}"),
    }

    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.5),
            "Downloading packages",
            Some(2),
            Some(0),
        ),
    )
    .unwrap();
    assert_download_without_stage(&s);

    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.5),
            "Downloading packages",
            Some(1),
            Some(4),
        ),
    )
    .unwrap();
    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.5),
            "Downloading packages",
            Some(3),
            None,
        ),
    )
    .unwrap();
    assert_download_without_stage(&s);
}

#[test]
fn stage_count_sets_both() {
    let mut s = started_apply(vec![dnf("glibc")]);
    s.apply_when_ready = true;
    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.5),
            "Downloading packages",
            Some(2),
            Some(4),
        ),
    )
    .unwrap();
    assert!(s.apply_when_ready);
    match &s.phase {
        Phase::Running(running) => {
            assert_eq!(running.pass, RunPass::Download);
            assert_eq!(
                running.stage,
                Some(StageCount {
                    current: 2,
                    total: 4
                })
            );
            assert!((running.overall_progress - 0.5).abs() < 1e-9);
        }
        other => panic!("expected Running, got {other:?}"),
    }

    let kept = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Downloading),
            Some(0.8),
            "Downloading packages",
            None,
            None,
        ),
    )
    .unwrap();
    match &kept.phase {
        Phase::Running(running) => {
            assert_eq!(running.pass, RunPass::Download);
            assert_eq!(
                running.stage,
                Some(StageCount {
                    current: 2,
                    total: 4
                })
            );
            assert!((running.overall_progress - 0.5).abs() < 1e-9);
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn source_switch_returns_to_download() {
    let mut s = started_apply(vec![dnf("glibc")]);
    s.apply_when_ready = true;
    let s = apply(
        s,
        dnf_progress(
            Some("glibc"),
            Some(PackageStatus::Installing),
            Some(0.4),
            "Installing",
            Some(2),
            Some(4),
        ),
    )
    .unwrap();
    assert!(s.apply_when_ready);
    match &s.phase {
        Phase::Running(running) => {
            assert_eq!(running.pass, RunPass::Apply);
            assert_eq!(
                running.stage,
                Some(StageCount {
                    current: 2,
                    total: 4
                })
            );
        }
        other => panic!("expected Running, got {other:?}"),
    }
    let s = apply(
        s,
        Event::ApplyProgress {
            source: UpdateSource::FlatpakUser,
            package_hint: None,
            status_hint: None,
            progress: None,
            phase_label: "Updating Apps".into(),
            stage_current: None,
            stage_total: None,
        },
    )
    .unwrap();
    assert!(s.apply_when_ready);
    match &s.phase {
        Phase::Running(running) => {
            assert_eq!(running.pass, RunPass::Download);
            assert!(running.stage.is_none());
            assert!(running.current_name.is_none());
            assert!(running.work_progress.is_none());
            assert!(running.active_index.is_none());
            assert!((running.overall_progress - 0.0).abs() < f64::EPSILON);
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn stop_before_install_returns_every_package_to_pending() {
    let mut s = running_dnf(vec![dnf("tzdata-java"), dnf("grok-bot")]);
    s = apply(
        s,
        dnf_progress(
            Some("tzdata-java"),
            Some(PackageStatus::Downloading),
            Some(1.0),
            "Downloading packages",
            Some(1),
            Some(2),
        ),
    )
    .unwrap();
    let s = apply(
        s,
        Event::StopApply {
            during_transaction: false,
        },
    )
    .unwrap();
    match s.phase {
        Phase::Ready {
            packages,
            soft_errors,
        } => {
            assert!(packages.iter().all(|p| p.status == PackageStatus::Pending));
            assert!(soft_errors
                .iter()
                .any(|e| e.contains("Stopped before install")));
        }
        other => panic!("expected Ready, got {other:?}"),
    }
}

#[test]
fn stop_during_install_warns_that_the_rpm_database_may_be_half_applied() {
    let mut s = running_dnf(vec![dnf("tzdata-java")]);
    s = apply(
        s,
        dnf_progress(None, None, None, "Running transaction", None, None),
    )
    .unwrap();
    let s = apply(
        s,
        Event::StopApply {
            during_transaction: true,
        },
    )
    .unwrap();
    match s.phase {
        Phase::Failed {
            title,
            detail,
            retry,
        } => {
            assert_eq!(title, "Update stopped during install");
            assert!(detail.contains("dnf history"));
            assert_eq!(retry, RetryAction::CheckAgain);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn stop_outside_a_run_is_rejected() {
    let err = apply(
        AppState::default(),
        Event::StopApply {
            during_transaction: false,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("StopApply"));
}

#[test]
fn apply_when_ready_survives_until_ready() {
    let s = apply(AppState::default(), Event::StartCheck { then_apply: true }).unwrap();
    assert!(s.apply_when_ready);
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    assert!(s.apply_when_ready);
    assert!(matches!(s.phase, Phase::Checking { .. }));
    let s = apply(
        s,
        Event::BackendCheckDone {
            source: UpdateSource::Dnf,
            packages: vec![],
            soft_error: None,
        },
    )
    .unwrap();
    assert!(s.apply_when_ready);
    let s = apply(
        s,
        Event::CheckComplete {
            packages: vec![pkg("kernel", UpdateSource::Dnf)],
            soft_errors: vec![],
            listed_at: SystemTime::UNIX_EPOCH,
        },
    )
    .unwrap();
    assert!(s.apply_when_ready);
    assert!(matches!(s.phase, Phase::Ready { .. }));
}

#[test]
fn empty_check_clears_apply_when_ready() {
    let s = apply(AppState::default(), Event::StartCheck { then_apply: true }).unwrap();
    let s = apply(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::CheckComplete {
            packages: vec![],
            soft_errors: vec![],
            listed_at: SystemTime::UNIX_EPOCH,
        },
    )
    .unwrap();
    assert!(!s.apply_when_ready);
    match &s.phase {
        Phase::Idle(IdleScreen::Current {
            checked_at,
            detail: None,
        }) => assert_eq!(checked_at, "just now"),
        other => panic!("expected Current with no detail, got {other:?}"),
    }
}

#[test]
fn cancel_clears_apply_when_ready_and_drops_detail() {
    let s = AppState {
        phase: Phase::Idle(IdleScreen::Current {
            checked_at: "yesterday".into(),
            detail: Some("No updates available (fwupd)".into()),
        }),
        ..Default::default()
    };
    let s = apply(s, Event::StartCheck { then_apply: true }).unwrap();
    assert!(s.apply_when_ready);
    assert!(matches!(
        s.phase,
        Phase::Authenticating {
            last_checked: Some(ref t),
            ..
        } if t == "yesterday"
    ));
    let s = apply(s, Event::CancelAuth).unwrap();
    assert!(!s.apply_when_ready);
    match s.phase {
        Phase::Idle(IdleScreen::Current {
            checked_at,
            detail: None,
        }) => assert_eq!(checked_at, "yesterday"),
        other => panic!("expected Current without detail, got {other:?}"),
    }
}

#[test]
fn fail_clears_apply_when_ready() {
    let s = apply(AppState::default(), Event::StartCheck { then_apply: true }).unwrap();
    assert!(s.apply_when_ready);
    let s = apply(
        s,
        Event::Fail {
            title: "Authentication cancelled".into(),
            detail: "polkit denied".into(),
            retry: RetryAction::CheckAgain,
        },
    )
    .unwrap();
    assert!(!s.apply_when_ready);
    assert!(matches!(
        s.phase,
        Phase::Failed {
            retry: RetryAction::CheckAgain,
            ..
        }
    ));
}

#[test]
fn apply_reset_and_stop_clear_the_flag_and_session_ready_leaves_it() {
    let ready = AppState {
        apply_when_ready: true,
        phase: Phase::Ready {
            packages: vec![pkg("kernel", UpdateSource::Dnf)],
            soft_errors: vec![],
        },
        ..Default::default()
    };
    let applied = apply(ready, Event::StartApply).unwrap();
    assert!(!applied.apply_when_ready);

    let idle = AppState {
        apply_when_ready: true,
        phase: Phase::Idle(IdleScreen::Current {
            checked_at: "just now".into(),
            detail: None,
        }),
        ..Default::default()
    };
    let reset = apply(idle, Event::ResetToIdle).unwrap();
    assert!(!reset.apply_when_ready);
    assert!(matches!(
        reset.phase,
        Phase::Idle(IdleScreen::Current { detail: None, .. })
    ));

    let mut running = running_dnf(vec![pkg("kernel", UpdateSource::Dnf)]);
    running.apply_when_ready = true;
    let stopped = apply(
        running.clone(),
        Event::StopApply {
            during_transaction: false,
        },
    )
    .unwrap();
    assert!(!stopped.apply_when_ready);

    running.apply_when_ready = true;
    let stopped_tx = apply(
        running,
        Event::StopApply {
            during_transaction: true,
        },
    )
    .unwrap();
    assert!(!stopped_tx.apply_when_ready);
    assert!(matches!(
        stopped_tx.phase,
        Phase::Failed {
            retry: RetryAction::CheckAgain,
            ..
        }
    ));

    let auth = AppState {
        apply_when_ready: true,
        phase: Phase::Authenticating {
            purpose: AuthPurpose::Check,
            packages: vec![],
            last_checked: None,
        },
        ..Default::default()
    };
    let session = apply(
        auth,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    assert!(session.apply_when_ready);
    assert!(matches!(session.phase, Phase::Checking { .. }));
}

#[test]
fn session_started_purpose_follows_the_phase() {
    assert_eq!(
        session_started_purpose(&Phase::Authenticating {
            purpose: AuthPurpose::Apply,
            packages: vec![],
            last_checked: Some("just now".into()),
        }),
        Some(AuthPurpose::Apply)
    );
    assert_eq!(
        session_started_purpose(&Phase::Checking {
            packages_so_far: vec![],
            soft_errors: vec![],
            checked_sources: vec![],
        }),
        Some(AuthPurpose::Check)
    );
    assert_eq!(
        session_started_purpose(&running_dnf(vec![]).phase),
        Some(AuthPurpose::Apply)
    );
    assert_eq!(
        session_started_purpose(&Phase::Idle(IdleScreen::Fresh)),
        None
    );
    assert_eq!(
        session_started_purpose(&Phase::Ready {
            packages: vec![],
            soft_errors: vec![],
        }),
        None
    );
    assert_eq!(
        session_started_purpose(&Phase::Done {
            packages: vec![],
            duration: "1s".into(),
            needs_reboot: false,
            failed: 0,
            failure_reasons: vec![],
        }),
        None
    );
    assert_eq!(
        session_started_purpose(&Phase::Failed {
            title: "x".into(),
            detail: "y".into(),
            retry: RetryAction::CheckAgain,
        }),
        None
    );
}

#[test]
fn reboot_fail_event_retries_reboot() {
    let reboot = apply(
        AppState::default(),
        Event::Fail {
            title: REBOOT_FAILED_TITLE.into(),
            detail: "blocked".into(),
            retry: RetryAction::Reboot,
        },
    )
    .unwrap();
    assert!(
        matches!(
            &reboot.phase,
            Phase::Failed {
                retry: RetryAction::Reboot,
                title,
                ..
            } if title == REBOOT_FAILED_TITLE
        ),
        "got {:?}",
        reboot.phase
    );
    let normal = apply(
        AppState::default(),
        Event::Fail {
            title: "All updates failed".into(),
            detail: "dnf exited 1".into(),
            retry: RetryAction::CheckAgain,
        },
    )
    .unwrap();
    assert!(
        matches!(
            &normal.phase,
            Phase::Failed {
                retry: RetryAction::CheckAgain,
                ..
            }
        ),
        "got {:?}",
        normal.phase
    );
}

#[test]
fn running_state_helpers() {
    let mut p1 = dnf("glibc");
    p1.status = PackageStatus::Completed;
    let mut p2 = dnf("kernel");
    p2.status = PackageStatus::Downloading;

    let mut running = RunningState::new(vec![p1, p2]);
    assert!(!running.is_applying());
    assert_eq!(running.phase_label, "Starting…");
    assert_eq!(running.done_and_total(), (1, 2));

    running.stage = Some(StageCount {
        current: 3,
        total: 10,
    });
    assert_eq!(running.done_and_total(), (3, 10));

    running.stage = Some(StageCount {
        current: 3,
        total: 0,
    });
    assert_eq!(running.done_and_total(), (1, 2));

    running.work_progress = Some(0.8);
    running.active_index = Some(1);
    running.current_name = Some("kernel".into());
    running.overall_progress = 0.75;
    running.enter_apply_pass();
    assert!(running.is_applying());
    assert_eq!(running.packages[0].status, PackageStatus::Completed);
    assert_eq!(running.packages[1].status, PackageStatus::Pending);
    assert!(running.work_progress.is_none());
    assert!(running.active_index.is_none());
    assert!(running.current_name.is_none());
    assert!(running.stage.is_none());
    assert!((running.overall_progress - 0.0).abs() < f64::EPSILON);

    running.work_progress = Some(0.5);
    running.active_index = Some(0);
    running.current_name = Some("glibc".into());
    running.overall_progress = 0.5;
    running.stage = Some(StageCount {
        current: 1,
        total: 2,
    });
    running.reset_for_source(UpdateSource::Firmware);
    assert_eq!(running.current_source, Some(UpdateSource::Firmware));
    assert!(!running.is_applying());
    assert!(running.work_progress.is_none());
    assert!(running.active_index.is_none());
    assert!(running.current_name.is_none());
    assert!(running.stage.is_none());
    assert!((running.overall_progress - 0.0).abs() < f64::EPSILON);
}

#[test]
fn failed_transition_leaves_state_unchanged() {
    let mut s = AppState {
        apply_when_ready: true,
        release_upgrade: Some(45),
        release_age_days: Some(7),
        ..Default::default()
    };
    let original_phase = s.phase.clone();

    let err = reduce(&mut s, Event::StartApply).unwrap_err();
    assert!(err.to_string().contains("StartApply"));
    assert_eq!(s.phase, original_phase);
    assert!(s.apply_when_ready);
    assert_eq!(s.release_upgrade, Some(45));
    assert_eq!(s.release_age_days, Some(7));
    assert!(s.updates_listed_at.is_none());
    assert!(s.stopwatch.is_none());

    s.phase = Phase::Ready {
        packages: vec![],
        soft_errors: vec!["err".into()],
    };
    s.updates_listed_at = Some(SystemTime::UNIX_EPOCH);
    let empty_ready = s.phase.clone();
    assert!(reduce(&mut s, Event::StartApply).is_err());
    assert_eq!(s.phase, empty_ready);
    assert!(s.apply_when_ready);
    assert_eq!(s.updates_listed_at, Some(SystemTime::UNIX_EPOCH));

    let mut auth = apply(AppState::default(), Event::StartCheck { then_apply: true }).unwrap();
    let auth_phase = auth.phase.clone();
    assert!(reduce(
        &mut auth,
        Event::SessionReady {
            purpose: AuthPurpose::Apply,
        },
    )
    .is_err());
    assert_eq!(auth.phase, auth_phase);
    assert!(auth.apply_when_ready);
    assert!(auth.stopwatch.is_some());

    let mut checking = apply(
        auth,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    let checking_phase = checking.phase.clone();
    assert!(reduce(
        &mut checking,
        Event::ApplyComplete { needs_reboot: true },
    )
    .is_err());
    assert_eq!(checking.phase, checking_phase);
    assert!(checking.apply_when_ready);
}

#[test]
fn all_failed_surfaces_backend_failure_reasons_inline() {
    let s = started_apply(vec![dnf("kernel")]);
    let s = apply(
        s,
        Event::BackendApplyDone {
            source: UpdateSource::Dnf,
            success: false,
            needs_reboot: false,
            error: Some("System: RPM database is locked by another process.".into()),
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::ApplyComplete {
            needs_reboot: false,
        },
    )
    .unwrap();
    match s.phase {
        Phase::Failed {
            title,
            detail,
            retry: RetryAction::CheckAgain,
        } => {
            assert_eq!(title, "All updates failed");
            assert!(detail.contains("RPM database is locked"), "{detail}");
            assert!(detail.contains("Open View Log"), "{detail}");
        }
        other => panic!("expected Failed with inline reason, got {other:?}"),
    }
}

#[test]
fn partial_failure_done_preserves_failure_reasons_and_resets_to_warning_idle() {
    let s = started_apply(vec![dnf("kernel"), flatpak("org.app.Foo")]);
    let s = apply(
        s,
        Event::BackendApplyDone {
            source: UpdateSource::Dnf,
            success: false,
            needs_reboot: false,
            error: Some("System: dependency conflict — Problem 1: foo".into()),
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::BackendApplyDone {
            source: UpdateSource::FlatpakUser,
            success: true,
            needs_reboot: false,
            error: None,
        },
    )
    .unwrap();
    let s = apply(
        s,
        Event::ApplyComplete {
            needs_reboot: false,
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Done {
            failed,
            failure_reasons,
            ..
        } => {
            assert_eq!(*failed, 1);
            assert_eq!(
                failure_reasons,
                &vec!["System: dependency conflict — Problem 1: foo".to_string()]
            );
        }
        other => panic!("expected Done, got {other:?}"),
    }

    let idle = apply(s, Event::ResetToIdle).unwrap();
    match &idle.phase {
        Phase::Idle(screen @ IdleScreen::Warning { title, detail, .. }) => {
            assert_eq!(*title, "Some updates failed");
            assert!(detail.contains("Problem 1: foo"), "{detail}");
            let chrome = idle_chrome(screen);
            assert_ne!(chrome.title, "System is up to date");
            assert_eq!(chrome.icon, "dialog-warning-symbolic");
            assert!(chrome.show_view_log);
        }
        other => panic!("expected Warning idle, got {other:?}"),
    }
}

#[test]
fn cancel_auth_during_apply_restores_ready_list() {
    let listed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(12_345);
    let mut s = ready_state(vec![dnf("firefox"), flatpak("org.app.Foo")]);
    s.updates_listed_at = Some(listed_at);

    let s = apply(s, Event::StartApply).unwrap();
    assert_phase(&s, "Authenticating");

    let s = apply(s, Event::CancelAuth).unwrap();
    assert_phase(&s, "Ready");
    assert_eq!(s.updates_listed_at, Some(listed_at));
    assert_eq!(s.packages().len(), 2);
}

#[test]
fn apply_auth_denied_preserves_packages_for_retry_apply() {
    let s = ready_state(vec![dnf("kernel"), flatpak("org.app.Foo")]);
    let s = apply(s, Event::StartApply).unwrap();
    let s = apply(
        s,
        Event::Fail {
            title: "Authentication cancelled".into(),
            detail: "polkit denied".into(),
            retry: RetryAction::RetryApply,
        },
    )
    .unwrap();
    assert_phase(&s, "Failed");
    assert_eq!(s.packages().len(), 2);

    let retried = apply(s, Event::StartApply).unwrap();
    assert_phase(&retried, "Authenticating");
    assert_eq!(retried.packages().len(), 2);
    assert!(retried.retry_apply_packages.is_empty());

    let running = apply(
        retried,
        Event::SessionReady {
            purpose: AuthPurpose::Apply,
        },
    )
    .unwrap();
    assert_phase(&running, "Running");
    assert_eq!(running.packages().len(), 2);
}

#[test]
fn cancel_auth_from_warning_idle_returns_fresh_not_up_to_date() {
    let s = AppState {
        phase: Phase::Idle(IdleScreen::Warning {
            title: "Could not check all sources",
            detail: "dnf failed".into(),
            checked_at: Some("just now".into()),
        }),
        ..Default::default()
    };
    let auth = apply(s, Event::StartCheck { then_apply: false }).unwrap();
    let cancelled = apply(auth, Event::CancelAuth).unwrap();
    assert_eq!(cancelled.phase, Phase::Idle(IdleScreen::Fresh));
}
