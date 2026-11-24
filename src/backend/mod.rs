//! Parsers and progress hints for each update backend.
//! Flatpak discovery and transaction apply call libflatpak natively, with flatpak CLI fallback.

pub mod brew;
pub mod cleanup;
pub mod containers;
pub mod dnf;
pub mod dnf_dbus;
pub mod flatpak;
pub mod fwupd;
pub mod fwupd_dbus;
pub mod gnome_extensions;
pub mod hooks;
pub mod logind;
pub mod mise;
pub mod python_tools;
pub mod rpm_ostree;
pub mod rustup;

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::model::{Package, PackageStatus, UpdateSource};

/// Locate a user-space binary by checking `PATH` and then fallback directories.
/// Returns `None` without side effects when missing.
pub fn find_user_binary(name: &str, extra_dirs: &[PathBuf]) -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    find_user_binary_with(name, path_env.as_deref(), extra_dirs)
}

/// Injectable variant of [`find_user_binary`] for deterministic offline tests.
pub fn find_user_binary_with(
    name: &str,
    path_env: Option<&OsStr>,
    extra_dirs: &[PathBuf],
) -> Option<PathBuf> {
    if name.trim().is_empty() {
        return None;
    }
    if let Some(paths) = path_env.filter(|p| !p.is_empty()) {
        for dir in std::env::split_paths(paths) {
            let candidate = dir.join(name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    for dir in extra_dirs {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub(super) fn home_dir_from(home_env: Option<&OsStr>) -> Option<PathBuf> {
    home_env.filter(|h| !h.is_empty()).map(PathBuf::from)
}

pub(super) fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_file() && (meta.permissions().mode() & 0o111) != 0
}

pub(super) fn strip_ansi_escapes(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub(super) fn find_json_object_start(trimmed: &str) -> Option<&str> {
    if trimmed.starts_with('{') {
        return Some(trimmed);
    }
    let mut offset = 0usize;
    for line in trimmed.split_inclusive('\n') {
        if line.trim_start().starts_with('{') {
            return Some(trimmed[offset..].trim_start());
        }
        offset += line.len();
    }
    None
}

/// Result of attempting a native Fedora D-Bus or library operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeOutcome<T> {
    /// Native daemon answered (either succeeded or returned an actionable domain error).
    Handled(Result<T, String>),
    /// Native daemon or system D-Bus is unavailable — fall back to CLI path.
    Unavailable(String),
}

/// Best-effort progress extracted from a single log line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressHint {
    pub package_name: Option<String>,
    pub status: Option<PackageStatus>,
    /// Per-item percent (the `100%` on a dnf progress line). Not the `[cur/total]` count.
    pub progress: Option<f64>,
    pub phase_label: Option<String>,
    /// `[cur/total]` step counter for the current download or transaction.
    pub stage_current: Option<u32>,
    pub stage_total: Option<u32>,
}

pub fn parse_progress(source: UpdateSource, line: &str) -> ProgressHint {
    match source {
        UpdateSource::Dnf => dnf::parse_progress_line(line),
        UpdateSource::RpmOstree => rpm_ostree::parse_progress_line(line),
        UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => {
            flatpak::parse_progress_line(line)
        }
        UpdateSource::GnomeExtensions => ProgressHint::default(),
        UpdateSource::Homebrew => brew::parse_progress_line(line),
        UpdateSource::Rustup => rustup::parse_progress_line(line),
        UpdateSource::Mise => mise::parse_progress_line(line),
        UpdateSource::UvTools => python_tools::parse_uv_progress_line(line),
        UpdateSource::Pipx => python_tools::parse_pipx_progress_line(line),
        UpdateSource::Distrobox | UpdateSource::Toolbx => containers::parse_progress_line(line),
        UpdateSource::CustomHook => hooks::parse_progress_line(line),
        UpdateSource::Firmware => fwupd::parse_progress_line(line),
    }
}

pub fn detect_reboot(source: UpdateSource, log: &str, packages: &[Package]) -> bool {
    match source {
        UpdateSource::Dnf => dnf::detect_reboot_needed(log, packages),
        UpdateSource::RpmOstree => rpm_ostree::detect_reboot_needed(log, packages),
        UpdateSource::FlatpakUser
        | UpdateSource::FlatpakSystem
        | UpdateSource::GnomeExtensions
        | UpdateSource::Homebrew
        | UpdateSource::Rustup
        | UpdateSource::Mise
        | UpdateSource::UvTools
        | UpdateSource::Pipx
        | UpdateSource::Distrobox
        | UpdateSource::Toolbx
        | UpdateSource::CustomHook => false,
        UpdateSource::Firmware => fwupd::detect_reboot_needed(log),
    }
}

pub(super) fn extract_percent(line: &str) -> Option<f64> {
    for token in line.split_whitespace() {
        if let Some(num) = token.strip_suffix('%') {
            if let Ok(v) = num.parse::<f64>() {
                if (0.0..=100.0).contains(&v) {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_progress_dispatches_to_each_backend() {
        let dnf_hint = parse_progress(UpdateSource::Dnf, "[1/4] Upgrading firefox-140.0-1.fc44.x86_64 50%");
        assert_eq!(dnf_hint.package_name.as_deref(), Some("firefox"));
        assert_eq!(dnf_hint.status, Some(PackageStatus::Installing));
        assert_eq!(dnf_hint.progress, Some(0.5));

        let fp_user = parse_progress(
            UpdateSource::FlatpakUser,
            "Downloading app/org.signal.Signal/x86_64/stable 45%",
        );
        assert_eq!(fp_user.package_name.as_deref(), Some("org.signal.Signal"));
        assert_eq!(fp_user.status, Some(PackageStatus::Downloading));

        let fp_sys = parse_progress(
            UpdateSource::FlatpakSystem,
            "Installing runtime/org.gnome.Platform/x86_64/48 100%",
        );
        assert_eq!(fp_sys.package_name.as_deref(), Some("org.gnome.Platform"));
        assert_eq!(fp_sys.status, Some(PackageStatus::Installing));

        let fw_hint = parse_progress(UpdateSource::Firmware, "Writing… Percentage=60");
        assert_eq!(fw_hint.phase_label.as_deref(), Some("Installing firmware"));
        assert_eq!(fw_hint.progress, Some(0.6));

        let ostree_hint = parse_progress(
            UpdateSource::RpmOstree,
            "Receiving objects: 65% (130/200) 10.2 MB/s",
        );
        assert_eq!(ostree_hint.status, Some(PackageStatus::Downloading));
        assert_eq!(ostree_hint.progress, Some(0.65));
        assert_eq!(ostree_hint.stage_current, Some(2));
        assert_eq!(ostree_hint.stage_total, Some(5));

        let brew_hint = parse_progress(UpdateSource::Homebrew, "==> Pouring ripgrep--14.1.1.x86_64_linux.bottle.tar.gz");
        assert_eq!(brew_hint.package_name.as_deref(), Some("ripgrep"));
        assert_eq!(brew_hint.status, Some(PackageStatus::Installing));

        let rust_hint = parse_progress(UpdateSource::Rustup, "info: downloading component 'rustc' 50%");
        assert_eq!(rust_hint.status, Some(PackageStatus::Downloading));
        assert_eq!(rust_hint.progress, Some(0.5));

        let mise_hint = parse_progress(UpdateSource::Mise, "mise node@22.15.0  install");
        assert_eq!(mise_hint.package_name.as_deref(), Some("node"));
        assert_eq!(mise_hint.status, Some(PackageStatus::Installing));

        let uv_hint = parse_progress(UpdateSource::UvTools, "Updated httpie v3.2.2 -> v3.2.4");
        assert_eq!(uv_hint.package_name.as_deref(), Some("httpie"));
        assert_eq!(uv_hint.status, Some(PackageStatus::Completed));

        let pipx_hint = parse_progress(UpdateSource::Pipx, "upgrading ansible-lint...");
        assert_eq!(pipx_hint.package_name.as_deref(), Some("ansible-lint"));
        assert_eq!(pipx_hint.status, Some(PackageStatus::Installing));

        let dbox_hint = parse_progress(UpdateSource::Distrobox, "Upgrading arch-dev...");
        assert_eq!(dbox_hint.package_name.as_deref(), Some("arch-dev"));
        assert_eq!(dbox_hint.status, Some(PackageStatus::Installing));

        let tbx_hint = parse_progress(UpdateSource::Toolbx, "[1/2] Downloading git-core 50%");
        assert_eq!(tbx_hint.status, Some(PackageStatus::Downloading));
        assert_eq!(tbx_hint.progress, Some(0.5));

        assert_eq!(
            parse_progress(
                UpdateSource::GnomeExtensions,
                "Updated GNOME Shell extension blur-my-shell@aunetx"
            ),
            ProgressHint::default()
        );
    }

    #[test]
    fn find_user_binary_with_resolves_path_then_extra_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let extra = dir.path().join("extra");
        std::fs::create_dir_all(&extra).unwrap();
        let tool = extra.join("mytool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();

        // Missing execute bit is ignored.
        assert_eq!(
            find_user_binary_with("mytool", None, std::slice::from_ref(&extra)),
            None
        );

        let mut perms = std::fs::metadata(&tool).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tool, perms).unwrap();
        assert_eq!(
            find_user_binary_with("mytool", None, std::slice::from_ref(&extra)).as_deref(),
            Some(tool.as_path())
        );

        let path_dir = dir.path().join("path");
        std::fs::create_dir_all(&path_dir).unwrap();
        let path_tool = path_dir.join("mytool");
        std::fs::write(&path_tool, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&path_tool).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path_tool, perms).unwrap();
        assert_eq!(
            find_user_binary_with(
                "mytool",
                Some(path_dir.as_os_str()),
                std::slice::from_ref(&extra),
            )
            .as_deref(),
            Some(path_tool.as_path())
        );
    }

    #[test]
    fn detect_reboot_dispatches_by_source_and_never_triggers_for_flatpak() {
        let kernel = vec![Package::new_dnf("kernel", "x86_64", "7.1.12-200.fc44", "updates")];
        assert!(detect_reboot(UpdateSource::Dnf, "", &kernel));
        assert!(!detect_reboot(UpdateSource::Dnf, "Complete!", &[]));

        // Flatpak, GNOME Extensions, Homebrew, user-space toolchains, and container backends never request a system reboot regardless of log text or package list.
        assert!(!detect_reboot(
            UpdateSource::FlatpakUser,
            "An update requires a reboot to complete.",
            &kernel
        ));
        assert!(!detect_reboot(
            UpdateSource::FlatpakSystem,
            "System restart required",
            &kernel
        ));
        assert!(!detect_reboot(
            UpdateSource::GnomeExtensions,
            "An update requires a reboot to complete.",
            &kernel
        ));
        assert!(!detect_reboot(
            UpdateSource::Homebrew,
            "An update requires a reboot to complete.",
            &kernel
        ));
        for toolchain in [
            UpdateSource::Rustup,
            UpdateSource::Mise,
            UpdateSource::UvTools,
            UpdateSource::Pipx,
        ] {
            assert!(!detect_reboot(
                toolchain,
                "An update requires a reboot to complete.",
                &kernel
            ));
        }
        assert!(!detect_reboot(
            UpdateSource::Distrobox,
            "An update requires a reboot to complete.",
            &kernel
        ));
        assert!(!detect_reboot(
            UpdateSource::Toolbx,
            "An update requires a reboot to complete.",
            &kernel
        ));
        assert!(!detect_reboot(
            UpdateSource::CustomHook,
            "An update requires a reboot to complete.",
            &kernel
        ));

        assert!(detect_reboot(
            UpdateSource::Firmware,
            "An update requires a reboot to complete.",
            &[]
        ));
        assert!(!detect_reboot(
            UpdateSource::Firmware,
            "Successfully installed firmware",
            &[]
        ));

        assert!(detect_reboot(
            UpdateSource::RpmOstree,
            "Staging deployment... done\nRun \"systemctl reboot\" to start a reboot",
            &[]
        ));
        assert!(!detect_reboot(
            UpdateSource::RpmOstree,
            "No upgrade available.",
            &[]
        ));
    }

    #[test]
    fn extract_percent_validates_range_and_format() {
        assert_eq!(extract_percent("0%"), Some(0.0));
        assert_eq!(extract_percent("Downloading 45.5% done"), Some(45.5));
        assert_eq!(extract_percent("100%"), Some(100.0));
        assert_eq!(extract_percent("-1%"), None);
        assert_eq!(extract_percent("101%"), None);
        assert_eq!(extract_percent("100.1%"), None);
        assert_eq!(extract_percent("abc%"), None);
        assert_eq!(extract_percent("45 percent"), None);
    }
}
