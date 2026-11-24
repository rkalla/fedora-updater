//! Hidden `FEDORA_UPDATER_PREVIEW=<phase>` sample states for layout review.
//! Optional `FEDORA_UPDATER_SCREENSHOT=/path.png` writes the window and exits.

use gtk::gdk::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use fedora_updater::backend::dnf_dbus::{HistoryEntry, HistoryPackageChange, HistorySource};
use fedora_updater::console::ConsoleBuffer;
use fedora_updater::model::{
    parse_size_bytes, AdvisoryKind, ApplyMode, AuthPurpose, DetailView, IdleScreen, Package,
    PackageAdvisory, PackageStatus, RetryAction, RunPass, Stopwatch, UpdateSource,
};
use fedora_updater::state::{AppState, Phase, RunningState};

#[derive(Debug, Clone)]
pub struct PreviewConfig {
    pub state: AppState,
    pub sidebar_open: bool,
    pub detail_view: DetailView,
    pub expand_completed: bool,
    pub enable_native_capabilities: bool,
    pub ostree_booted: bool,
    pub sample_history: Vec<HistoryEntry>,
    pub sample_throughput: Vec<(f64, u64, u64)>,
}

pub fn initial_config() -> Option<PreviewConfig> {
    let phase = std::env::var("FEDORA_UPDATER_PREVIEW").ok()?;
    Some(config_for(&phase))
}

pub fn initial_console() -> ConsoleBuffer {
    let mut console = ConsoleBuffer::new(8_000);
    if std::env::var("FEDORA_UPDATER_PREVIEW").is_ok() {
        console.push_committed_tagged("dnf", "Updating and loading repositories:");
        console.push_committed_tagged("dnf", "Repositories loaded.");
        console.push_committed_tagged("dnf", "[3/5] Installing gnome-shell-48.2-1.fc44.x86_64");
        console.push_committed_tagged("flatpak-user", "Syncing flathub metadata");
        console.push_committed_tagged(
            "gnome-ext",
            "Update available: Blur my Shell (v47 -> v48)",
        );
        console.push_committed_tagged("brew", "==> Upgrading ripgrep 14.1.0 -> 14.1.1");
        console.push_committed_tagged("fwupd", "Loaded LVFS metadata for UEFI dbx");
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

pub fn config_for(phase: &str) -> PreviewConfig {
    let mut state = AppState {
        phase: Phase::Idle(IdleScreen::Fresh),
        stopwatch: Some(Stopwatch::start()),
        updates_listed_at: None,
        apply_when_ready: false,
        release_upgrade: None,
        release_age_days: None,
        retry_apply_packages: Vec::new(),
        apply_mode: ApplyMode::Live,
    };
    let mut sidebar_open = true;
    let mut detail_view = DetailView::Updates;
    let mut expand_completed = false;
    let mut enable_native_capabilities = false;
    let mut ostree_booted = false;
    let mut sample_history = Vec::new();
    let mut sample_throughput = Vec::new();

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
        "ready-lumen" => {
            sidebar_open = false;
            Phase::Ready {
                packages: sample_packages(false),
                soft_errors: vec![],
            }
        }
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
        "ready-native" => {
            enable_native_capabilities = true;
            Phase::Ready {
                packages: sample_native_packages(),
                soft_errors: vec![],
            }
        }
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
        "running-sparkline" => {
            enable_native_capabilities = true;
            sample_throughput = vec![
                (1_200_000.0, 1_200_000, 16_000_000),
                (3_400_000.0, 4_600_000, 16_000_000),
                (5_800_000.0, 10_400_000, 16_000_000),
                (4_600_000.0, 15_000_000, 16_000_000),
            ];
            let mut dl_pkgs = sample_native_packages();
            dl_pkgs[0].status = PackageStatus::Downloading;
            Phase::Running(RunningState {
                packages: dl_pkgs,
                active_index: Some(0),
                overall_progress: 0.25,
                phase_label: "Downloading packages".into(),
                current_source: Some(UpdateSource::Dnf),
                current_name: Some("openssl".into()),
                work_progress: Some(0.85),
                pass: RunPass::Download,
                stage: Some(fedora_updater::model::StageCount {
                    current: 1,
                    total: 6,
                }),
                needs_reboot: false,
                failed_sources: vec![],
                failure_reasons: vec![],
            })
        }
        "running-many" | "running-expanded" => {
            expand_completed = phase == "running-expanded";
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
                .filter(|p| p.name != "kernel" && p.source != UpdateSource::Firmware)
                .map(|mut p| {
                    p.status = PackageStatus::Completed;
                    p
                })
                .collect(),
            duration: "2m 14s".into(),
            needs_reboot: false,
            failed: 0,
            failure_reasons: vec![],
        },
        "done-reboot" => Phase::Done {
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
        "done-partial" | "done-mixed" => Phase::Done {
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
        "log" => {
            detail_view = DetailView::ActivityLog;
            Phase::Ready {
                packages: sample_packages(false),
                soft_errors: vec![],
            }
        }
        "history" => {
            enable_native_capabilities = true;
            detail_view = DetailView::History;
            sample_history = sample_history_entries();
            Phase::Ready {
                packages: sample_native_packages(),
                soft_errors: vec![],
            }
        }
        "ready-ostree" => {
            ostree_booted = true;
            Phase::Ready {
                packages: sample_ostree_packages(false),
                soft_errors: vec![],
            }
        }
        "ready-ostree-mixed" => {
            ostree_booted = true;
            enable_native_capabilities = true;
            Phase::Ready {
                packages: sample_ostree_mixed_packages(),
                soft_errors: vec![],
            }
        }
        "running-ostree" => {
            ostree_booted = true;
            let mut running = RunningState::new(sample_ostree_packages(true));
            running.current_source = Some(UpdateSource::RpmOstree);
            running.phase_label = "Downloading OSTree objects".into();
            running.pass = RunPass::Download;
            running.overall_progress = 0.45;
            running.stage = Some(fedora_updater::model::StageCount {
                current: 2,
                total: 5,
            });
            Phase::Running(running)
        }
        "done-ostree" => {
            ostree_booted = true;
            Phase::Done {
                packages: sample_ostree_packages(false)
                    .into_iter()
                    .map(|mut p| {
                        p.status = PackageStatus::Completed;
                        p
                    })
                    .collect(),
                duration: "1m 52s".into(),
                needs_reboot: true,
                failed: 0,
                failure_reasons: vec![],
            }
        }
        other => {
            eprintln!("unknown FEDORA_UPDATER_PREVIEW={other}, using idle");
            Phase::Idle(IdleScreen::Fresh)
        }
    };

    PreviewConfig {
        state,
        sidebar_open,
        detail_view,
        expand_completed,
        enable_native_capabilities,
        ostree_booted,
        sample_history,
        sample_throughput,
    }
}

#[cfg(test)]
pub fn state_for(phase: &str) -> AppState {
    config_for(phase).state
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
        UpdateSource::RpmOstree => Package::new_rpm_ostree(
            "upgraded",
            name,
            "x86_64",
            Some(old.into()),
            version,
            "fedora:fedora/44/x86_64/silverblue",
            "Upgraded",
            Some("Target deployment: 44.20261124.0".into()),
        ),
        UpdateSource::Firmware => {
            let mut p = Package::new_dnf(name, "", version, "fwupd");
            p.source = UpdateSource::Firmware;
            p
        }
        UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => {
            let mut p = Package::new_dnf(name, "", version, "flathub");
            p.source = UpdateSource::FlatpakUser;
            p.detail =
                format!("{version} · flathub · installs org.freedesktop.Platform.GL.nvidia-570-133-07");
            p
        }
        UpdateSource::GnomeExtensions => {
            let mut p = Package::new_gnome_extension(
                format!("{}@example.org", name.to_lowercase().replace(' ', "-")),
                name,
                Some(old.into()),
                Some("Adds a blur look to different parts of the GNOME Shell".into()),
                Some("https://extensions.gnome.org/extension/3193/blur-my-shell/".into()),
            );
            p.version = version.into();
            p.detail = format!("{old} → {version}");
            p
        }
        UpdateSource::Homebrew => {
            Package::new_homebrew(name, Some(old.into()), version, "homebrew/core", false)
        }
    };
    p.old_version = Some(old.into());
    p.size_bytes = parse_size_bytes(size);
    p.status = status;
    p
}

fn sample_ostree_packages(running: bool) -> Vec<Package> {
    vec![
        pkg(
            "kernel",
            UpdateSource::RpmOstree,
            "6.14.11-300.fc44",
            "6.14.10-300.fc44",
            "68 MB",
            if running {
                PackageStatus::Installing
            } else {
                PackageStatus::Pending
            },
        ),
        pkg(
            "mesa-dri-drivers",
            UpdateSource::RpmOstree,
            "25.0.4-1.fc44",
            "25.0.2-1.fc44",
            "24 MB",
            if running {
                PackageStatus::Completed
            } else {
                PackageStatus::Pending
            },
        ),
    ]
}

fn sample_ostree_mixed_packages() -> Vec<Package> {
    let mut pkgs = sample_ostree_packages(false);
    pkgs.push(pkg(
        "Thunderbird",
        UpdateSource::FlatpakUser,
        "128.4.0",
        "128.3.0",
        "84 MB",
        PackageStatus::Pending,
    ));
    pkgs.push(pkg(
        "ripgrep",
        UpdateSource::Homebrew,
        "14.1.1",
        "14.1.0",
        "3.2 MB",
        PackageStatus::Pending,
    ));
    pkgs
}

fn sample_packages(running: bool) -> Vec<Package> {
    vec![
        pkg(
            "openssl",
            UpdateSource::Dnf,
            "1:3.2.2-3.fc44",
            "1:3.2.2-1.fc44",
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
            "6.14.11-300.fc44",
            "6.14.9-300.fc44",
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
            "140.0-1.fc44",
            "139.0-1.fc44",
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
            "48.2-1.fc44",
            "48.1-1.fc44",
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
            "ripgrep",
            UpdateSource::Homebrew,
            "14.1.1",
            "14.1.0",
            "2.8 MB",
            PackageStatus::Pending,
        ),
        pkg(
            "Blur my Shell",
            UpdateSource::GnomeExtensions,
            "v48",
            "v47",
            "180 KB",
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

fn sample_native_packages() -> Vec<Package> {
    let mut pkgs = sample_packages(false);
    if let Some(openssl) = pkgs.first_mut() {
        openssl.install_size_delta = Some(320 * 1024);
        openssl.summary =
            Some("Utilities from the general purpose cryptography library with TLS support".into());
        openssl.advisories.push(PackageAdvisory {
            id: "FEDORA-2026-84a1".into(),
            kind: AdvisoryKind::Security,
            severity: Some("Important".into()),
            cves: vec!["CVE-2026-2401".into()],
            bugzillas: vec!["RHBZ#2458001".into()],
            description: Some("Fixes TLS 1.3 buffer over-read when decoding session tickets.".into()),
        });
    }
    pkgs
}

fn sample_history_entries() -> Vec<HistoryEntry> {
    vec![
        HistoryEntry {
            id: "dnf-txn-203".into(),
            source: HistorySource::Dnf,
            timestamp: 1_795_224_552,
            title: "openssl + 1 more".into(),
            description: "dnf update -y".into(),
            status: "Ok".into(),
            succeeded: true,
            changes: vec![
                HistoryPackageChange {
                    name: "openssl".into(),
                    arch: "x86_64".into(),
                    evr: "3.2.2-3.fc44".into(),
                    original_evr: Some("3.2.2-1.fc44".into()),
                    action: "Upgraded".into(),
                },
                HistoryPackageChange {
                    name: "exfatprogs".into(),
                    arch: "x86_64".into(),
                    evr: "1.4.4-1.fc44".into(),
                    original_evr: Some("1.4.3-1.fc44".into()),
                    action: "Upgraded".into(),
                },
            ],
        },
        HistoryEntry {
            id: "fwupd-uefi-2026".into(),
            source: HistorySource::Firmware,
            timestamp: 1_795_150_000,
            title: "System Firmware".into(),
            description: "uefi_capsule".into(),
            status: "Restart Required".into(),
            succeeded: true,
            changes: vec![HistoryPackageChange {
                name: "System Firmware".into(),
                arch: String::new(),
                evr: "0.1.33".into(),
                original_evr: Some("0.1.32".into()),
                action: "Firmware".into(),
            }],
        },
    ]
}

#[cfg(test)]
mod tests {
    use fedora_updater::model::{
        AuthPurpose, DetailView, IdleScreen, PackageStatus, RetryAction, RunPass, UpdateSource,
    };
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
                assert_eq!(packages_so_far.len(), 8);
                assert!(packages_so_far
                    .iter()
                    .any(|p| p.source == UpdateSource::GnomeExtensions));
                assert!(packages_so_far.iter().any(|p| p.source == UpdateSource::Homebrew));
                assert_eq!(checked_sources, vec![UpdateSource::Dnf]);
                assert!(soft_errors.is_empty());
            }
            other => panic!("expected Checking, got {other:?}"),
        }

        let ready_cfg = super::config_for("ready");
        assert!(ready_cfg.sidebar_open);
        assert_eq!(ready_cfg.detail_view, DetailView::Updates);
        assert!(matches!(
            &ready_cfg.state.phase,
            Phase::Ready { packages, soft_errors } if packages.len() == 8 && soft_errors.is_empty()
        ));

        let lumen_cfg = super::config_for("ready-lumen");
        assert!(!lumen_cfg.sidebar_open);
        assert_eq!(lumen_cfg.detail_view, DetailView::Updates);
        assert!(matches!(
            &lumen_cfg.state.phase,
            Phase::Ready { packages, soft_errors } if packages.len() == 8 && soft_errors.is_empty()
        ));

        assert!(matches!(
            &super::state_for("ready-many").phase,
            Phase::Ready { packages, .. }
                if packages.len() >= 70 && packages.iter().all(|p| p.status == PackageStatus::Pending)
        ));
        assert!(matches!(
            &super::state_for("ready-warn").phase,
            Phase::Ready { packages, soft_errors }
                if packages.len() == 8 && soft_errors == &vec!["Firmware metadata refresh failed".to_string()]
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
            let cfg = super::config_for(name);
            assert_eq!(cfg.expand_completed, name == "running-expanded");
            match cfg.state.phase {
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
                assert!(!needs_reboot);
                assert!(failure_reasons.is_empty());
                assert!(!packages.is_empty());
                assert!(packages.iter().all(|p| p.status == PackageStatus::Completed));
            }
            other => panic!("expected clean Done, got {other:?}"),
        }

        match super::state_for("done-reboot").phase {
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
                assert_eq!(packages.len(), 8);
                assert!(packages.iter().all(|p| p.status == PackageStatus::Completed));
            }
            other => panic!("expected Done (reboot), got {other:?}"),
        }

        for name in ["done-partial", "done-mixed"] {
            match super::state_for(name).phase {
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
                other => panic!("expected {name}, got {other:?}"),
            }
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

        let log_cfg = super::config_for("log");
        assert!(log_cfg.sidebar_open);
        assert_eq!(log_cfg.detail_view, DetailView::ActivityLog);
        assert!(matches!(log_cfg.state.phase, Phase::Ready { .. }));

        let native_cfg = super::config_for("ready-native");
        assert!(native_cfg.enable_native_capabilities);
        assert!(matches!(&native_cfg.state.phase, Phase::Ready { packages, .. } if packages[0].has_security_advisory()));

        let spark_cfg = super::config_for("running-sparkline");
        assert!(spark_cfg.enable_native_capabilities);
        assert!(!spark_cfg.sample_throughput.is_empty());

        let hist_cfg = super::config_for("history");
        assert!(hist_cfg.enable_native_capabilities);
        assert_eq!(hist_cfg.detail_view, DetailView::History);
        assert_eq!(hist_cfg.sample_history.len(), 2);

        let ready_ostree = super::config_for("ready-ostree");
        assert!(ready_ostree.ostree_booted);
        assert!(matches!(
            &ready_ostree.state.phase,
            Phase::Ready { packages, .. }
                if packages.len() == 2 && packages.iter().all(|p| p.source == UpdateSource::RpmOstree)
        ));
        let ready_ostree_mixed = super::config_for("ready-ostree-mixed");
        assert!(ready_ostree_mixed.ostree_booted);
        assert!(matches!(
            &ready_ostree_mixed.state.phase,
            Phase::Ready { packages, .. } if packages.len() == 4
        ));
        let running_ostree = super::config_for("running-ostree");
        assert!(running_ostree.ostree_booted);
        assert!(matches!(
            &running_ostree.state.phase,
            Phase::Running(r) if r.current_source == Some(UpdateSource::RpmOstree)
        ));
        let done_ostree = super::config_for("done-ostree");
        assert!(done_ostree.ostree_booted);
        assert!(matches!(
            &done_ostree.state.phase,
            Phase::Done { needs_reboot: true, failed: 0, .. }
        ));

        if std::env::var_os("FEDORA_UPDATER_PREVIEW").is_none() {
            assert!(super::initial_config().is_none());
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
