//! Hidden `FEDORA_UPDATER_PREVIEW=<phase>` sample states for layout review.
//! Optional `FEDORA_UPDATER_SCREENSHOT=/path.png` writes the window and exits.

use gtk::gdk::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use fedora_updater::console::ConsoleBuffer;
use fedora_updater::model::{
    parse_size_bytes, AuthPurpose, IdleScreen, Package, PackageStatus, RetryAction, RunPass,
    Stopwatch, UpdateSource,
};
use fedora_updater::state::{AppState, Phase, RunningState};

pub fn initial_state() -> Option<AppState> {
    let phase = std::env::var("FEDORA_UPDATER_PREVIEW").ok()?;
    Some(state_for(&phase))
}

pub fn initial_console() -> ConsoleBuffer {
    let mut console = ConsoleBuffer::new(8_000);
    if std::env::var("FEDORA_UPDATER_PREVIEW").is_ok() {
        console.push("[dnf] Updating and loading repositories:");
        console.push("[dnf] Repositories loaded.");
        console.push("[3/5] Installing gnome-shell-48.2-1.fc42.x86_64");
    }
    console
}

pub fn maybe_schedule_screenshot(window: &impl IsA<gtk::Widget>) {
    let Ok(path) = std::env::var("FEDORA_UPDATER_SCREENSHOT") else {
        return;
    };
    let widget = window.as_ref().clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(450), move || {
        if let Err(e) = save_png(&widget, &path) {
            eprintln!("screenshot failed: {e}");
            std::process::exit(1);
        }
        std::process::exit(0);
    });
}

fn save_png(widget: &gtk::Widget, path: &str) -> Result<(), String> {
    let width = widget.width().max(1);
    let height = widget.height().max(1);
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node().ok_or("empty snapshot")?;
    let native = widget.native().ok_or("no native surface")?;
    let renderer = native.renderer().ok_or("no renderer")?;
    let texture = renderer.render_texture(&node, None);
    texture
        .save_to_png(path)
        .map_err(|e| format!("save {path}: {e}"))
}

pub fn state_for(phase: &str) -> AppState {
    let mut state = AppState {
        phase: Phase::Idle(IdleScreen::Fresh),
        stopwatch: Some(Stopwatch::start()),
        updates_listed_at: None,
        apply_when_ready: false,
        release_upgrade: None,
        release_age_days: None,
        retry_apply_packages: Vec::new(),
    };

    state.phase = match phase {
        "idle" => Phase::Idle(IdleScreen::Fresh),
        "idle-current" | "up-to-date" => Phase::Idle(IdleScreen::Current {
            checked_at: "just now".into(),
            detail: None,
        }),
        "idle-warning" => Phase::Idle(IdleScreen::Warning {
            title: "Could not check all sources",
            detail: "Some sources could not be checked: dnf check-update failed (exit 1)".into(),
            checked_at: Some("just now".into()),
        }),
        "auth" => Phase::Authenticating {
            purpose: AuthPurpose::Check,
            packages: vec![],
            last_checked: None,
        },
        "checking" => Phase::Checking {
            packages_so_far: sample_packages(false),
            soft_errors: vec![],
            checked_sources: vec![UpdateSource::Dnf],
        },
        "ready" => Phase::Ready {
            packages: sample_packages(false),
            soft_errors: vec![],
        },
        "ready-many" => Phase::Ready {
            packages: many_running_packages()
                .into_iter()
                .map(|mut p| {
                    p.status = PackageStatus::Pending;
                    p
                })
                .collect(),
            soft_errors: vec![],
        },
        "ready-warn" => Phase::Ready {
            packages: sample_packages(false),
            soft_errors: vec!["Firmware metadata refresh failed".into()],
        },
        "running" => Phase::Running(RunningState {
            packages: sample_packages(true),
            active_index: Some(3),
            overall_progress: 0.5,
            phase_label: "Installing".into(),
            current_source: Some(UpdateSource::Dnf),
            current_name: Some("gnome-shell".into()),
            work_progress: Some(0.72),
            pass: RunPass::Apply,
            stage: None,
            needs_reboot: false,
            failed_sources: vec![],
            failure_reasons: vec![],
        }),
        "running-many" | "running-expanded" => {
            let packages = many_running_packages();
            let active_index = packages
                .iter()
                .position(|p| p.status == PackageStatus::Installing);
            let done = packages
                .iter()
                .filter(|p| p.status == PackageStatus::Completed)
                .count();
            let overall = done as f64 / packages.len() as f64;
            Phase::Running(RunningState {
                packages,
                active_index,
                overall_progress: overall,
                phase_label: "Installing".into(),
                current_source: Some(UpdateSource::Dnf),
                current_name: Some("current-package".into()),
                work_progress: Some(0.42),
                pass: RunPass::Apply,
                stage: None,
                needs_reboot: false,
                failed_sources: vec![],
                failure_reasons: vec![],
            })
        }
        "done" => Phase::Done {
            packages: sample_packages(false)
                .into_iter()
                .map(|mut p| {
                    p.status = PackageStatus::Completed;
                    p
                })
                .collect(),
            duration: "6m 42s".into(),
            needs_reboot: true,
            failed: 0,
            failure_reasons: vec![],
        },
        "done-mixed" => Phase::Done {
            packages: {
                let mut pkgs = sample_packages(false);
                for p in &mut pkgs {
                    p.status = PackageStatus::Completed;
                }
                if let Some(last) = pkgs.last_mut() {
                    last.status = PackageStatus::Failed;
                }
                pkgs
            },
            duration: "6m 42s".into(),
            needs_reboot: true,
            failed: 1,
            failure_reasons: vec!["Firmware: Connect AC power and try again (battery too low).".into()],
        },
        "failed" => Phase::Failed {
            title: "All updates failed".into(),
            detail: "dnf update -y exited 1 · nothing was applied".into(),
            retry: RetryAction::CheckAgain,
        },
        other => {
            eprintln!("unknown FEDORA_UPDATER_PREVIEW={other}, using idle");
            Phase::Idle(IdleScreen::Fresh)
        }
    };
    state
}

fn pkg(
    name: &str,
    source: UpdateSource,
    version: &str,
    old: &str,
    size: &str,
    status: PackageStatus,
) -> Package {
    let mut p = match source {
        UpdateSource::Dnf => Package::new_dnf(name, "x86_64", version, "updates"),
        UpdateSource::Firmware => {
            let mut p = Package::new_dnf(name, "", version, "fwupd");
            p.source = UpdateSource::Firmware;
            p
        }
        UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => {
            let mut p = Package::new_dnf(name, "", version, "flathub");
            p.source = UpdateSource::FlatpakUser;
            p
        }
    };
    p.old_version = Some(old.into());
    p.size_bytes = parse_size_bytes(size);
    p.status = status;
    p
}

fn sample_packages(running: bool) -> Vec<Package> {
    vec![
        pkg(
            "openssl",
            UpdateSource::Dnf,
            "1:3.2.2-3.fc42",
            "1:3.2.2-1.fc42",
            "2.1 MB",
            if running {
                PackageStatus::Completed
            } else {
                PackageStatus::Pending
            },
        ),
        pkg(
            "kernel",
            UpdateSource::Dnf,
            "6.14.11-300.fc42",
            "6.14.9-300.fc42",
            "98 MB",
            if running {
                PackageStatus::Completed
            } else {
                PackageStatus::Pending
            },
        ),
        pkg(
            "firefox",
            UpdateSource::Dnf,
            "140.0-1.fc42",
            "139.0-1.fc42",
            "112 MB",
            if running {
                PackageStatus::Completed
            } else {
                PackageStatus::Pending
            },
        ),
        pkg(
            "gnome-shell",
            UpdateSource::Dnf,
            "48.2-1.fc42",
            "48.1-1.fc42",
            "8.7 MB",
            if running {
                PackageStatus::Installing
            } else {
                PackageStatus::Pending
            },
        ),
        pkg(
            "Thunderbird",
            UpdateSource::FlatpakUser,
            "128.2.0",
            "128.0.1",
            "94 MB",
            PackageStatus::Pending,
        ),
        pkg(
            "UEFI dbx",
            UpdateSource::Firmware,
            "20241101",
            "20240101",
            "18 KB",
            PackageStatus::Pending,
        ),
    ]
}

fn many_running_packages() -> Vec<Package> {
    // Typical large dnf transaction — the apply window must stay at default size.
    const COMPLETED: usize = 45;
    const QUEUED: usize = 30;
    let mut packages = Vec::with_capacity(COMPLETED + 1 + QUEUED);
    for i in 0..COMPLETED {
        packages.push(pkg(
            &format!("package-{}", i + 1),
            UpdateSource::Dnf,
            "1.0-1.fc44",
            "0.9-1.fc44",
            "1 MB",
            PackageStatus::Completed,
        ));
    }
    packages.push(pkg(
        "current-package",
        UpdateSource::Dnf,
        "2.0-1.fc44",
        "1.9-1.fc44",
        "8 MB",
        PackageStatus::Installing,
    ));
    packages.push(pkg(
        "Thunderbird",
        UpdateSource::FlatpakUser,
        "2.0-1.fc44",
        "1.9-1.fc44",
        "4 MB",
        PackageStatus::Pending,
    ));
    packages.push(pkg(
        "UEFI dbx",
        UpdateSource::Firmware,
        "2.0-1.fc44",
        "1.9-1.fc44",
        "4 MB",
        PackageStatus::Pending,
    ));
    for i in 2..QUEUED {
        packages.push(pkg(
            &format!("queued-package-{}", i + 1),
            UpdateSource::Dnf,
            "2.0-1.fc44",
            "1.9-1.fc44",
            "4 MB",
            PackageStatus::Pending,
        ));
    }
    packages
}

#[cfg(test)]
mod tests {
    use fedora_updater::model::{AuthPurpose, IdleScreen, PackageStatus, RetryAction, RunPass, UpdateSource};
    use fedora_updater::state::Phase;

    #[test]
    fn preview_state_for_idle_and_failed() {
        let idle = super::state_for("idle");
        assert!(!idle.apply_when_ready);
        assert!(matches!(idle.phase, Phase::Idle(IdleScreen::Fresh)));

        let unknown = super::state_for("not-a-real-phase");
        assert!(!unknown.apply_when_ready);
        assert!(matches!(unknown.phase, Phase::Idle(IdleScreen::Fresh)));

        for name in ["idle-current", "up-to-date"] {
            let state = super::state_for(name);
            assert!(!state.apply_when_ready);
            match state.phase {
                Phase::Idle(IdleScreen::Current {
                    checked_at,
                    detail: None,
                }) => {
                    assert_eq!(checked_at, "just now");
                }
                other => panic!("{name} should be Current, got {other:?}"),
            }
        }

        match super::state_for("idle-warning").phase {
            Phase::Idle(IdleScreen::Warning { title, detail, checked_at }) => {
                assert_eq!(title, "Could not check all sources");
                assert!(detail.contains("dnf check-update failed"));
                assert_eq!(checked_at.as_deref(), Some("just now"));
            }
            other => panic!("expected Warning idle, got {other:?}"),
        }

        assert!(matches!(
            super::state_for("auth").phase,
            Phase::Authenticating {
                purpose: AuthPurpose::Check,
                ..
            }
        ));

        match super::state_for("checking").phase {
            Phase::Checking {
                packages_so_far,
                checked_sources,
                soft_errors,
            } => {
                assert_eq!(packages_so_far.len(), 6);
                assert_eq!(checked_sources, vec![UpdateSource::Dnf]);
                assert!(soft_errors.is_empty());
            }
            other => panic!("expected Checking, got {other:?}"),
        }

        assert!(matches!(
            &super::state_for("ready").phase,
            Phase::Ready { packages, soft_errors } if packages.len() == 6 && soft_errors.is_empty()
        ));
        assert!(matches!(
            &super::state_for("ready-many").phase,
            Phase::Ready { packages, .. }
                if packages.len() >= 70 && packages.iter().all(|p| p.status == PackageStatus::Pending)
        ));
        assert!(matches!(
            &super::state_for("ready-warn").phase,
            Phase::Ready { packages, soft_errors }
                if packages.len() == 6 && soft_errors == &vec!["Firmware metadata refresh failed".to_string()]
        ));

        match super::state_for("running").phase {
            Phase::Running(r) => {
                assert_eq!(r.active_index, Some(3));
                assert_eq!(r.pass, RunPass::Apply);
                assert_eq!(r.current_name.as_deref(), Some("gnome-shell"));
            }
            other => panic!("expected Running, got {other:?}"),
        }

        for name in ["running-many", "running-expanded"] {
            match super::state_for(name).phase {
                Phase::Running(r) => {
                    assert!(r.packages.len() >= 70, "{name}");
                    assert!(r.active_index.is_some(), "{name}");
                    assert_eq!(r.current_name.as_deref(), Some("current-package"));
                }
                other => panic!("{name} should be Running, got {other:?}"),
            }
        }

        match super::state_for("done").phase {
            Phase::Done {
                packages,
                needs_reboot,
                failed,
                failure_reasons,
                ..
            } => {
                assert_eq!(failed, 0);
                assert!(needs_reboot);
                assert!(failure_reasons.is_empty());
                assert!(packages.iter().all(|p| p.status == PackageStatus::Completed));
            }
            other => panic!("expected Done, got {other:?}"),
        }

        match super::state_for("done-mixed").phase {
            Phase::Done {
                packages,
                needs_reboot,
                failed,
                failure_reasons,
                ..
            } => {
                assert_eq!(failed, 1);
                assert!(needs_reboot);
                assert_eq!(failure_reasons.len(), 1);
                assert_eq!(packages.last().unwrap().status, PackageStatus::Failed);
            }
            other => panic!("expected done-mixed, got {other:?}"),
        }

        match super::state_for("failed").phase {
            Phase::Failed {
                title,
                detail,
                retry,
            } => {
                assert_eq!(title, "All updates failed");
                assert_eq!(detail, "dnf update -y exited 1 · nothing was applied");
                assert_eq!(retry, RetryAction::CheckAgain);
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        if std::env::var_os("FEDORA_UPDATER_PREVIEW").is_none() {
            assert!(super::initial_state().is_none());
            assert!(super::initial_console().is_empty());
        }
    }

    #[test]
    fn many_running_packages_matches_a_large_transaction() {
        let packages = super::many_running_packages();
        assert!(
            packages.len() >= 70,
            "preview must cover a full-height-growth case, got {}",
            packages.len()
        );
        let completed = packages
            .iter()
            .filter(|p| p.status == super::PackageStatus::Completed)
            .count();
        assert!(
            completed >= 40,
            "need a long completed expander, got {completed}"
        );
    }
}
