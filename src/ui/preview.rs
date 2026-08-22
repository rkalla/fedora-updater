//! Hidden `FEDORA_UPDATER_PREVIEW=<phase>` sample states for layout review.
//! Optional `FEDORA_UPDATER_SCREENSHOT=/path.png` writes the window and exits.

use gtk::gdk::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use fedora_updater::model::{
    AdvisoryKind, AuthPurpose, ConsoleBuffer, Package, PackageStatus, Stopwatch, UpdateSource,
};
use fedora_updater::state::{AppState, Phase};

pub fn initial_state() -> Option<AppState> {
    let phase = std::env::var("FEDORA_UPDATER_PREVIEW").ok()?;
    Some(state_for(&phase))
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
    let mut console = ConsoleBuffer::new(200);
    console.push("[dnf] Updating and loading repositories:");
    console.push("[dnf] Repositories loaded.");
    console.push("[3/5] Installing gnome-shell-48.2-1.fc42.x86_64");

    let mut state = AppState {
        phase: Phase::Idle {
            last_checked: None,
            message: None,
        },
        console,
        stopwatch: Some(Stopwatch::start()),
    };

    state.phase = match phase {
        "idle" => Phase::Idle {
            last_checked: None,
            message: None,
        },
        "idle-current" | "up-to-date" => Phase::Idle {
            last_checked: Some("just now".into()),
            message: Some("System updated".into()),
        },
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
                    p.progress = 0.0;
                    p
                })
                .collect(),
            soft_errors: vec![],
        },
        "ready-warn" => Phase::Ready {
            packages: sample_packages(false),
            soft_errors: vec!["Firmware metadata refresh failed".into()],
        },
        "running" => Phase::Running {
            packages: sample_packages(true),
            active_index: Some(3),
            overall_progress: 0.58,
            phase_label: "Installing gnome-shell".into(),
            current_source: Some(UpdateSource::Dnf),
            needs_reboot: false,
            failed_sources: vec![],
        },
        "running-many" | "running-expanded" => Phase::Running {
            packages: many_running_packages(),
            active_index: Some(24),
            overall_progress: 0.64,
            phase_label: "Installing current-package".into(),
            current_source: Some(UpdateSource::Dnf),
            needs_reboot: false,
            failed_sources: vec![],
        },
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
        },
        "failed" => Phase::Failed {
            title: "All updates failed".into(),
            detail: "dnf update -y exited 1 · nothing was applied".into(),
        },
        other => {
            eprintln!("unknown FEDORA_UPDATER_PREVIEW={other}, using idle");
            Phase::Idle {
                last_checked: None,
                message: None,
            }
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
    kind: AdvisoryKind,
    status: PackageStatus,
    progress: f64,
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
    p.size = Some(size.into());
    p.kind = kind;
    p.status = status;
    p.progress = progress;
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
            AdvisoryKind::Security,
            if running {
                PackageStatus::Completed
            } else {
                PackageStatus::Pending
            },
            if running { 1.0 } else { 0.0 },
        ),
        pkg(
            "kernel",
            UpdateSource::Dnf,
            "6.14.11-300.fc42",
            "6.14.9-300.fc42",
            "98 MB",
            AdvisoryKind::Security,
            if running {
                PackageStatus::Completed
            } else {
                PackageStatus::Pending
            },
            if running { 1.0 } else { 0.0 },
        ),
        pkg(
            "firefox",
            UpdateSource::Dnf,
            "140.0-1.fc42",
            "139.0-1.fc42",
            "112 MB",
            AdvisoryKind::Security,
            if running {
                PackageStatus::Completed
            } else {
                PackageStatus::Pending
            },
            if running { 1.0 } else { 0.0 },
        ),
        pkg(
            "gnome-shell",
            UpdateSource::Dnf,
            "48.2-1.fc42",
            "48.1-1.fc42",
            "8.7 MB",
            AdvisoryKind::Enhancement,
            if running {
                PackageStatus::Installing
            } else {
                PackageStatus::Pending
            },
            if running { 0.72 } else { 0.0 },
        ),
        pkg(
            "Thunderbird",
            UpdateSource::FlatpakUser,
            "128.2.0",
            "128.0.1",
            "94 MB",
            AdvisoryKind::Bugfix,
            PackageStatus::Pending,
            0.0,
        ),
        pkg(
            "UEFI dbx",
            UpdateSource::Firmware,
            "20241101",
            "20240101",
            "18 KB",
            AdvisoryKind::Bugfix,
            PackageStatus::Pending,
            0.0,
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
            AdvisoryKind::Unknown,
            PackageStatus::Completed,
            1.0,
        ));
    }
    packages.push(pkg(
        "current-package",
        UpdateSource::Dnf,
        "2.0-1.fc44",
        "1.9-1.fc44",
        "8 MB",
        AdvisoryKind::Enhancement,
        PackageStatus::Installing,
        0.42,
    ));
    packages.push(pkg(
        "Thunderbird",
        UpdateSource::FlatpakUser,
        "2.0-1.fc44",
        "1.9-1.fc44",
        "4 MB",
        AdvisoryKind::Unknown,
        PackageStatus::Pending,
        0.0,
    ));
    packages.push(pkg(
        "UEFI dbx",
        UpdateSource::Firmware,
        "2.0-1.fc44",
        "1.9-1.fc44",
        "4 MB",
        AdvisoryKind::Unknown,
        PackageStatus::Pending,
        0.0,
    ));
    for i in 2..QUEUED {
        packages.push(pkg(
            &format!("queued-package-{}", i + 1),
            UpdateSource::Dnf,
            "2.0-1.fc44",
            "1.9-1.fc44",
            "4 MB",
            AdvisoryKind::Unknown,
            PackageStatus::Pending,
            0.0,
        ));
    }
    packages
}

#[cfg(test)]
mod tests {
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
