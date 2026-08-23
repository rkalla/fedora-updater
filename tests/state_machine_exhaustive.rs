//! Exhaustive transition tests for the updater state machine.
//! These are the safety net for UI flow integrity.

use fedora_updater::model::{Package, PackageStatus, UpdateSource};
use fedora_updater::state::{phase_name, reduce, AppState, Event, Phase};
use fedora_updater::AuthPurpose;

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
        size: None,
        old_version: None,
        status: PackageStatus::Pending,
        progress: 0.0,
        kind: fedora_updater::AdvisoryKind::Unknown,
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
        size: None,
        old_version: Some("1.0".into()),
        status: PackageStatus::Pending,
        progress: 0.0,
        kind: fedora_updater::AdvisoryKind::Security,
        source: UpdateSource::Firmware,
        detail: "1.0 → 2.0".into(),
    }
}

fn assert_phase(s: &AppState, name: &str) {
    assert_eq!(phase_name(&s.phase), name);
}

#[test]
fn happy_path_multi_source() {
    let s = AppState::default();
    assert_phase(&s, "Idle");

    let s = reduce(s, Event::StartCheck).unwrap();
    assert_phase(&s, "Authenticating");

    let s = reduce(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Check,
        },
    )
    .unwrap();
    assert_phase(&s, "Checking");

    let s = reduce(
        s,
        Event::ConsoleLine {
            tag: "dnf".into(),
            text: "loading".into(),
        },
    )
    .unwrap();
    assert!(s.console.last_line().contains("[dnf]"));

    let pkgs = vec![dnf("kernel"), flatpak("org.app.Foo"), firmware("UEFI")];
    let s = reduce(
        s,
        Event::BackendCheckDone {
            source: UpdateSource::Dnf,
            packages: vec![dnf("kernel")],
            soft_error: None,
        },
    )
    .unwrap();
    let s = reduce(
        s,
        Event::BackendCheckDone {
            source: UpdateSource::FlatpakUser,
            packages: vec![flatpak("org.app.Foo")],
            soft_error: None,
        },
    )
    .unwrap();
    let s = reduce(
        s,
        Event::BackendCheckDone {
            source: UpdateSource::Firmware,
            packages: vec![firmware("UEFI")],
            soft_error: Some("minor warning".into()),
        },
    )
    .unwrap();
    assert_eq!(s.packages().len(), 3);

    let s = reduce(
        s,
        Event::CheckComplete {
            packages: pkgs.clone(),
            soft_errors: vec!["minor warning".into()],
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

    let s = reduce(s, Event::StartApply).unwrap();
    assert_phase(&s, "Authenticating");

    let s = reduce(
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
        s = reduce(
            s,
            Event::ApplySourceStarted {
                source,
                label: label.into(),
            },
        )
        .unwrap();
        s = reduce(
            s,
            Event::ApplyProgress {
                source,
                package_hint: None,
                status_hint: Some(PackageStatus::Installing),
                progress: Some(0.5),
                phase_label: format!("Updating {label}"),
            },
        )
        .unwrap();
        s = reduce(
            s,
            Event::BackendApplyDone {
                source,
                success: true,
                needs_reboot: source == UpdateSource::Dnf || source == UpdateSource::Firmware,
            },
        )
        .unwrap();
    }

    let s = reduce(
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
    assert!(reduce(s.clone(), Event::StartApply).is_err());
    assert!(reduce(
        s.clone(),
        Event::SessionReady {
            purpose: AuthPurpose::Check
        }
    )
    .is_err());
    assert!(reduce(
        s.clone(),
        Event::CheckComplete {
            packages: vec![],
            soft_errors: vec![]
        }
    )
    .is_err());
    assert!(reduce(
        s.clone(),
        Event::BackendApplyDone {
            source: UpdateSource::Dnf,
            success: true,
            needs_reboot: false
        }
    )
    .is_err());
    assert!(reduce(
        s,
        Event::ApplyComplete {
            needs_reboot: false
        }
    )
    .is_err());
}

#[test]
fn cannot_apply_empty_ready() {
    let mut s = AppState::default();
    s.phase = Phase::Ready {
        packages: vec![],
        soft_errors: vec![],
    };
    assert!(reduce(s, Event::StartApply).is_err());
}

#[test]
fn in_app_cancel_auth_returns_idle() {
    let s = reduce(AppState::default(), Event::StartCheck).unwrap();
    assert_phase(&s, "Authenticating");
    let s = reduce(s, Event::CancelAuth).unwrap();
    assert_phase(&s, "Idle");
    assert!(reduce(AppState::default(), Event::CancelAuth).is_err());
}

#[test]
fn auth_cancel_style_fail() {
    let s = reduce(AppState::default(), Event::StartCheck).unwrap();
    let s = reduce(
        s,
        Event::Fail {
            title: "Authentication cancelled".into(),
            detail: "polkit denied".into(),
        },
    )
    .unwrap();
    assert_phase(&s, "Failed");
    let s = reduce(s, Event::ResetToIdle { message: None }).unwrap();
    assert_phase(&s, "Idle");
}

#[test]
fn progress_marks_package_and_completes_previous() {
    let mut s = AppState::default();
    s.phase = Phase::Running {
        packages: vec![dnf("openssl"), dnf("kernel")],
        active_index: None,
        overall_progress: 0.0,
        phase_label: "x".into(),
        current_source: None,
        current_name: None,
        work_progress: None,
        needs_reboot: false,
        failed_sources: Vec::new(),
    };
    let s = reduce(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("openssl".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.3),
            phase_label: "Installing".into(),
        },
    )
    .unwrap();
    let s = reduce(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("kernel".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.1),
            phase_label: "Installing".into(),
        },
    )
    .unwrap();
    let pkgs = s.packages();
    assert_eq!(pkgs[0].status, PackageStatus::Completed);
    assert_eq!(pkgs[1].status, PackageStatus::Installing);
}

#[test]
fn unnamed_progress_does_not_burn_down_or_poke_first_package() {
    let mut s = AppState::default();
    s.phase = Phase::Running {
        packages: vec![dnf("abrt"), dnf("kernel")],
        active_index: None,
        overall_progress: 0.0,
        phase_label: "x".into(),
        current_source: Some(UpdateSource::Dnf),
        current_name: None,
        work_progress: None,
        needs_reboot: false,
        failed_sources: Vec::new(),
    };
    let s = reduce(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: None,
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.5),
            phase_label: "Downloading packages".into(),
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Running {
            packages,
            active_index,
            current_name,
            work_progress,
            overall_progress,
            ..
        } => {
            assert!(packages.iter().all(|p| p.status == PackageStatus::Pending));
            assert_eq!(*active_index, None);
            assert_eq!(current_name.as_deref(), None);
            assert_eq!(*work_progress, Some(0.5));
            assert!((*overall_progress - 0.0).abs() < f64::EPSILON);
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn kernel_hint_does_not_complete_kernel_core() {
    let mut s = AppState::default();
    s.phase = Phase::Running {
        packages: vec![dnf("kernel"), dnf("kernel-core")],
        active_index: None,
        overall_progress: 0.0,
        phase_label: "x".into(),
        current_source: None,
        current_name: None,
        work_progress: None,
        needs_reboot: false,
        failed_sources: Vec::new(),
    };
    let s = reduce(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("kernel".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.2),
            phase_label: "Installing".into(),
        },
    )
    .unwrap();
    let pkgs = s.packages();
    assert_eq!(pkgs[0].status, PackageStatus::Installing);
    assert_eq!(pkgs[1].status, PackageStatus::Pending);
}

#[test]
fn verify_pass_does_not_move_current_back_to_completed() {
    let mut s = AppState::default();
    s.phase = Phase::Running {
        packages: vec![dnf("firefox"), dnf("kernel")],
        active_index: Some(1),
        overall_progress: 0.5,
        phase_label: "Installing".into(),
        current_source: Some(UpdateSource::Dnf),
        current_name: Some("kernel".into()),
        work_progress: Some(0.5),
        needs_reboot: false,
        failed_sources: Vec::new(),
    };
    match &mut s.phase {
        Phase::Running { packages, .. } => {
            packages[0].status = PackageStatus::Completed;
            packages[0].progress = 1.0;
            packages[1].status = PackageStatus::Installing;
        }
        _ => unreachable!(),
    }
    let s = reduce(
        s,
        Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("firefox".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.9),
            phase_label: "Verifying".into(),
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Running {
            packages,
            active_index,
            current_name,
            ..
        } => {
            assert_eq!(packages[0].status, PackageStatus::Completed);
            assert_eq!(packages[1].status, PackageStatus::Installing);
            assert_eq!(*active_index, Some(1));
            assert_eq!(current_name.as_deref(), Some("kernel"));
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn apply_source_started_does_not_mark_first_package() {
    let mut s = AppState::default();
    s.phase = Phase::Running {
        packages: vec![dnf("abrt"), dnf("kernel")],
        active_index: None,
        overall_progress: 0.0,
        phase_label: "x".into(),
        current_source: None,
        current_name: Some("stale".into()),
        work_progress: Some(0.3),
        needs_reboot: false,
        failed_sources: Vec::new(),
    };
    let s = reduce(
        s,
        Event::ApplySourceStarted {
            source: UpdateSource::Dnf,
            label: "Updating System".into(),
        },
    )
    .unwrap();
    match &s.phase {
        Phase::Running {
            packages,
            active_index,
            current_name,
            work_progress,
            phase_label,
            ..
        } => {
            assert!(packages.iter().all(|p| p.status == PackageStatus::Pending));
            assert_eq!(*active_index, None);
            assert_eq!(*current_name, None);
            assert_eq!(*work_progress, None);
            assert_eq!(phase_label, "Updating System");
        }
        other => panic!("expected Running, got {other:?}"),
    }
}

#[test]
fn soft_errors_on_empty_check_surface_message() {
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
            packages: vec![],
            soft_errors: vec!["fwupd not available".into()],
        },
    )
    .unwrap();
    match s.phase {
        Phase::Idle {
            message: Some(m), ..
        } => assert!(m.contains("fwupd")),
        _ => panic!("expected idle with message"),
    }
}

#[test]
fn dnf_failure_does_not_block_done_if_others_ok() {
    let mut s = AppState::default();
    s.phase = Phase::Ready {
        packages: vec![dnf("kernel"), flatpak("app")],
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
    let s = reduce(AppState::default(), Event::StartCheck).unwrap();
    assert!(reduce(
        s,
        Event::SessionReady {
            purpose: AuthPurpose::Apply
        }
    )
    .is_err());
}

#[test]
fn console_buffer_caps() {
    let mut s = AppState::default();
    s.console = fedora_updater::ConsoleBuffer::new(10);
    for i in 0..50 {
        s = reduce(
            s,
            Event::ConsoleLine {
                tag: "t".into(),
                text: format!("{i}"),
            },
        )
        .unwrap();
    }
    assert!(s.console.len() <= 10);
}
