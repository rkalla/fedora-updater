//! User-space `rustup` toolchain discovery, check parsing, and progress hints.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::model::{Package, PackageStatus, UpdateSource};

use super::{find_user_binary_with, home_dir_from, strip_ansi_escapes, ProgressHint};

/// Return standard user-space locations where `rustup` may live outside `PATH`.
pub fn extra_search_dirs(home_env: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(home) = home_dir_from(home_env) else {
        return Vec::new();
    };
    vec![home.join(".cargo/bin"), home.join(".rustup/bin")]
}

/// Auto-detect `rustup` via `PATH`, `~/.cargo/bin/rustup`, and `~/.rustup/bin/rustup`.
pub fn find_rustup_binary() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    let home_env = std::env::var_os("HOME");
    find_rustup_binary_with(path_env.as_deref(), home_env.as_deref())
}

/// Injectable variant of [`find_rustup_binary`] for offline tests.
pub fn find_rustup_binary_with(
    path_env: Option<&OsStr>,
    home_env: Option<&OsStr>,
) -> Option<PathBuf> {
    let extras = extra_search_dirs(home_env);
    find_user_binary_with("rustup", path_env, &extras)
}

fn path_is_rpm_managed(path: &Path) -> bool {
    path.starts_with("/usr") && !path.starts_with("/usr/local")
}

/// True when `rustup` is installed via Fedora system packages (e.g. `/usr/bin/rustup`),
/// in which case `rustup self update` must not be attempted.
pub fn is_rpm_managed_rustup(bin_path: &Path) -> bool {
    path_is_rpm_managed(bin_path)
        || std::fs::canonicalize(bin_path).is_ok_and(|resolved| path_is_rpm_managed(&resolved))
}

/// `rustup check` exits 0 when up-to-date and 100 when updates are available.
pub const fn check_exit_is_ok(code: i32) -> bool {
    matches!(code, 0 | 100)
}

/// Parse `rustup check` output into pending `UpdateSource::Rustup` packages.
///
/// Accepts both `Update available : <old> -> <new>` and lowercase
/// `update available: <old> -> <new>` lines (as emitted across rustup versions),
/// ignoring `Up to date` and `rustc ...` lines.
pub fn parse_rustup_check(output: &str) -> Vec<Package> {
    let cleaned = strip_ansi_escapes(output);
    let mut packages = Vec::new();
    let mut seen_ids = HashSet::new();

    for raw_line in cleaned.split(['\n', '\r']) {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((raw_target, raw_rest)) = line.split_once(" - ") else {
            continue;
        };
        let target = raw_target.trim();
        let rest = raw_rest.trim();
        let Some(prefix) = rest.get(..16) else {
            continue;
        };
        if !prefix.eq_ignore_ascii_case("update available") {
            continue;
        }
        let Some(after_colon) = rest[16..].trim_start().strip_prefix(':') else {
            continue;
        };
        let transition = after_colon.trim();
        let Some((old_raw, new_raw)) = transition.split_once(" -> ") else {
            continue;
        };
        let old = old_raw.trim();
        let new = new_raw.trim();
        if target.is_empty() || old.is_empty() || new.is_empty() || old == new {
            continue;
        }

        let id = format!("rustup:{target}");
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let (repo, detail) = if target == "rustup" {
            ("rustup", format!("{new} · rustup self-update"))
        } else {
            ("rustup", format!("{new} · rustup toolchain"))
        };
        packages.push(Package::new_toolchain(
            UpdateSource::Rustup,
            id,
            target,
            Some(old.to_string()),
            new,
            repo,
            detail,
            None,
            None,
        ));
    }

    packages
}

/// Parse `rustup check` output while filtering out `rustup` self-update when
/// the `rustup` binary is distro RPM-managed.
pub fn parse_rustup_check_for_binary(output: &str, bin_path: Option<&Path>) -> Vec<Package> {
    let mut pkgs = parse_rustup_check(output);
    if bin_path.is_some_and(is_rpm_managed_rustup) {
        pkgs.retain(|pkg| pkg.name != "rustup");
    }
    pkgs
}

/// Split selected/discovered `Rustup` items into toolchain arguments and an
/// optional user-owned self-update step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RustupApplyPlan {
    pub toolchains: Vec<String>,
    pub self_update: bool,
}

pub fn split_apply_targets(
    selected_ids: Option<&HashSet<String>>,
    discovered: &[Package],
    bin_path: Option<&Path>,
) -> RustupApplyPlan {
    let rpm_managed = bin_path.is_some_and(is_rpm_managed_rustup);
    let mut plan = RustupApplyPlan::default();
    let mut seen = HashSet::new();

    for pkg in discovered {
        if pkg.source != UpdateSource::Rustup {
            continue;
        }
        if !selected_ids.is_none_or(|ids| ids.contains(&pkg.id)) {
            continue;
        }
        if pkg.name == "rustup" {
            if !rpm_managed {
                plan.self_update = true;
            }
            continue;
        }
        if seen.insert(pkg.name.clone()) {
            plan.toolchains.push(pkg.name.clone());
        }
    }

    plan
}

fn extract_quoted_after(text: &str, marker: &str) -> Option<String> {
    let pos = text.find(marker)?;
    let rest = &text[pos + marker.len()..];
    let after_open = rest.split_once('\'')?.1;
    let quoted = after_open.split_once('\'')?.0.trim();
    if quoted.is_empty() {
        None
    } else {
        Some(quoted.to_string())
    }
}

fn strip_rustup_log_prefix(line: &str) -> &str {
    for prefix in ["info:", "warning:", "error:"] {
        if let Some(head) = line.get(..prefix.len()) {
            if head.eq_ignore_ascii_case(prefix) {
                return line[prefix.len()..].trim();
            }
        }
    }
    line
}

/// Extract structured progress hints from `rustup update` / `rustup self update` output.
pub fn parse_progress_line(line: &str) -> ProgressHint {
    let cleaned = strip_ansi_escapes(line);
    let trimmed = cleaned.rsplit('\r').next().unwrap_or(&cleaned).trim();
    if trimmed.is_empty() {
        return ProgressHint::default();
    }
    let clean = strip_rustup_log_prefix(trimmed);
    let lower = clean.to_ascii_lowercase();
    let progress = super::extract_percent(clean).map(|pct| pct / 100.0);

    if lower.contains("self-update installed") || lower.starts_with("rustup updated") {
        return ProgressHint {
            package_name: Some("rustup".to_string()),
            status: Some(PackageStatus::Completed),
            progress: Some(1.0),
            phase_label: Some("Updated rustup".to_string()),
            ..Default::default()
        };
    }

    if lower.contains("downloading self-update") || lower.contains("checking for self-update") {
        return ProgressHint {
            package_name: Some("rustup".to_string()),
            status: Some(PackageStatus::Downloading),
            progress,
            phase_label: Some("Updating rustup".to_string()),
            ..Default::default()
        };
    }

    if lower.contains("syncing channel updates for") {
        return ProgressHint {
            package_name: extract_quoted_after(clean, "for "),
            phase_label: Some("Syncing rustup channels".to_string()),
            ..Default::default()
        };
    }

    if lower.contains("downloading component") {
        return ProgressHint {
            package_name: extract_quoted_after(clean, "for "),
            status: Some(PackageStatus::Downloading),
            progress,
            phase_label: Some("Downloading components".to_string()),
            ..Default::default()
        };
    }

    if lower.contains("installing component") || lower.contains("removing previous version") {
        return ProgressHint {
            package_name: extract_quoted_after(clean, "for "),
            status: Some(PackageStatus::Installing),
            progress,
            phase_label: Some("Installing components".to_string()),
            ..Default::default()
        };
    }

    if let Some((prefix, _)) = clean
        .split_once(" updated")
        .or_else(|| clean.split_once(" unchanged"))
    {
        let target = prefix.trim();
        if !target.is_empty() && !target.contains(char::is_whitespace) {
            return ProgressHint {
                package_name: Some(target.to_string()),
                status: Some(PackageStatus::Completed),
                progress: Some(1.0),
                ..Default::default()
            };
        }
    }

    ProgressHint::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn find_rustup_binary_with_resolves_path_and_cargo_bin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cargo_bin = dir.path().join(".cargo/bin");
        std::fs::create_dir_all(&cargo_bin).expect("mkdir");
        let rustup_bin = cargo_bin.join("rustup");
        std::fs::write(&rustup_bin, "#!/bin/sh\n").expect("write");

        // Non-executable files must be ignored.
        assert_eq!(
            find_rustup_binary_with(Some(OsStr::new("")), Some(dir.path().as_os_str())),
            None
        );

        let mut perms = std::fs::metadata(&rustup_bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&rustup_bin, perms).unwrap();

        assert_eq!(
            find_rustup_binary_with(Some(OsStr::new("")), Some(dir.path().as_os_str())).as_deref(),
            Some(rustup_bin.as_path())
        );

        let path_dir = dir.path().join("bin");
        std::fs::create_dir_all(&path_dir).unwrap();
        let path_rustup = path_dir.join("rustup");
        std::fs::write(&path_rustup, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&path_rustup).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path_rustup, perms).unwrap();

        assert_eq!(
            find_rustup_binary_with(
                Some(path_dir.as_os_str()),
                Some(dir.path().as_os_str()),
            )
            .as_deref(),
            Some(path_rustup.as_path())
        );
    }

    #[test]
    fn is_rpm_managed_rustup_detects_usr_bin_paths() {
        assert!(is_rpm_managed_rustup(Path::new("/usr/bin/rustup")));
        assert!(!is_rpm_managed_rustup(Path::new("/usr/local/bin/rustup")));
        assert!(!is_rpm_managed_rustup(Path::new(
            "/home/alice/.cargo/bin/rustup"
        )));
    }

    #[test]
    fn check_exit_codes_match_rustup_semantics() {
        assert!(check_exit_is_ok(0));
        assert!(check_exit_is_ok(100));
        assert!(!check_exit_is_ok(1));
    }

    #[test]
    fn parse_rustup_check_handles_both_casings_spinners_and_rpm_filter() {
        let sample = "\x1b[1mchecking for self-update\x1b[0m\r\
            stable-x86_64-unknown-linux-gnu - Update available : 1.86.0 (05f9846f8 2025-03-31) -> 1.87.0 (17067e9ac 2025-05-09)\n\
            beta-x86_64-unknown-linux-gnu - Up to date : 1.87.0-beta.2\n\
            nightly-x86_64-unknown-linux-gnu - update available: 1.88.0-nightly (1111aaaa 2025-05-01) -> 1.88.0-nightly (2222bbbb 2025-05-10)\n\
            rustc 1.86.0 (05f9846f8 2025-03-31)\n\
            rustup - update available: 1.28.1 -> 1.29.0\n";

        let all = parse_rustup_check(sample);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "rustup:stable-x86_64-unknown-linux-gnu");
        assert_eq!(
            all[0].old_version.as_deref(),
            Some("1.86.0 (05f9846f8 2025-03-31)")
        );
        assert_eq!(all[0].version, "1.87.0 (17067e9ac 2025-05-09)");
        assert_eq!(all[1].id, "rustup:nightly-x86_64-unknown-linux-gnu");
        assert_eq!(all[2].id, "rustup:rustup");
        assert_eq!(all[2].version, "1.29.0");

        let rpm_filtered =
            parse_rustup_check_for_binary(sample, Some(Path::new("/usr/bin/rustup")));
        assert_eq!(rpm_filtered.len(), 2);
        assert!(!rpm_filtered.iter().any(|p| p.name == "rustup"));
    }

    #[test]
    fn split_apply_targets_separates_toolchains_and_self_update() {
        let sample = "\
            stable-x86_64-unknown-linux-gnu - Update available : 1.86.0 -> 1.87.0\n\
            nightly-x86_64-unknown-linux-gnu - Update available : 1.88.0-nightly (a) -> 1.88.0-nightly (b)\n\
            rustup - Update available : 1.28.1 -> 1.29.0\n";
        let pkgs = parse_rustup_check(sample);

        let full = split_apply_targets(None, &pkgs, Some(Path::new("/home/u/.cargo/bin/rustup")));
        assert_eq!(
            full.toolchains,
            vec![
                "stable-x86_64-unknown-linux-gnu",
                "nightly-x86_64-unknown-linux-gnu"
            ]
        );
        assert!(full.self_update);

        let selected: HashSet<String> = ["rustup:nightly-x86_64-unknown-linux-gnu".to_string()]
            .into_iter()
            .collect();
        let partial = split_apply_targets(Some(&selected), &pkgs, None);
        assert_eq!(partial.toolchains, vec!["nightly-x86_64-unknown-linux-gnu"]);
        assert!(!partial.self_update);

        let rpm_blocked = split_apply_targets(None, &pkgs, Some(Path::new("/usr/bin/rustup")));
        assert!(!rpm_blocked.self_update);
        assert_eq!(rpm_blocked.toolchains.len(), 2);
    }

    #[test]
    fn parse_progress_line_tracks_rustup_update_phases() {
        let sync = parse_progress_line(
            "info: syncing channel updates for 'stable-x86_64-unknown-linux-gnu'",
        );
        assert_eq!(
            sync.package_name.as_deref(),
            Some("stable-x86_64-unknown-linux-gnu")
        );
        assert_eq!(sync.phase_label.as_deref(), Some("Syncing rustup channels"));

        let dl = parse_progress_line("info: downloading component 'rustc' 45%");
        assert_eq!(dl.status, Some(PackageStatus::Downloading));
        assert_eq!(dl.progress, Some(0.45));
        assert_eq!(dl.phase_label.as_deref(), Some("Downloading components"));

        let inst = parse_progress_line(
            "info: installing component 'cargo' for 'stable-x86_64-unknown-linux-gnu'",
        );
        assert_eq!(inst.status, Some(PackageStatus::Installing));
        assert_eq!(
            inst.package_name.as_deref(),
            Some("stable-x86_64-unknown-linux-gnu")
        );

        let done =
            parse_progress_line("stable-x86_64-unknown-linux-gnu updated - rustc 1.87.0 (17067e9ac 2025-05-09)");
        assert_eq!(
            done.package_name.as_deref(),
            Some("stable-x86_64-unknown-linux-gnu")
        );
        assert_eq!(done.status, Some(PackageStatus::Completed));
        assert_eq!(done.progress, Some(1.0));
    }
}
