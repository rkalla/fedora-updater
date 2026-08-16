//! Parsers and progress hints for each update backend (CLI output only).

pub mod dnf;
pub mod flatpak;
pub mod fwupd;

use crate::model::{Package, PackageStatus, UpdateSource};

/// Best-effort progress extracted from a single log line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressHint {
    pub package_name: Option<String>,
    pub status: Option<PackageStatus>,
    pub progress: Option<f64>,
    pub phase_label: Option<String>,
}

pub fn parse_progress(source: UpdateSource, line: &str) -> ProgressHint {
    match source {
        UpdateSource::Dnf => dnf::parse_progress_line(line),
        UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => {
            flatpak::parse_progress_line(line)
        }
        UpdateSource::Firmware => fwupd::parse_progress_line(line),
    }
}

pub fn detect_reboot(source: UpdateSource, log: &str, packages: &[Package]) -> bool {
    match source {
        UpdateSource::Dnf => dnf::detect_reboot_needed(log, packages),
        UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => false,
        UpdateSource::Firmware => fwupd::detect_reboot_needed(log),
    }
}
