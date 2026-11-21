//! Parsers for `dnf check-update --refresh` and live `dnf update -y` output.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::model::{parse_size_bytes, Package, PackageStatus, UpdateSource};

use super::ProgressHint;

/// Read-only size lookup after `check-update --refresh` has warmed metadata.
pub const REPOQUERY_UPGRADES_ARGS: &[&str] = &[
    "-q",
    "repoquery",
    "--upgrades",
    "--queryformat",
    "%{name}\t%{arch}\t%{evr}\t%{downloadsize}\n",
];

pub fn parse_repoquery_sizes(output: &str) -> HashMap<(String, String, String), u64> {
    let mut out = HashMap::new();
    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = if line.contains('\t') {
            line.split('\t').collect()
        } else {
            line.split_whitespace().collect()
        };
        if parts.len() < 4 {
            continue;
        }
        let name = parts[0];
        let arch = parts[1];
        let evr = parts[2];
        if !is_plausible_arch(arch) {
            continue;
        }
        let Ok(bytes) = parts[3].trim().parse::<u64>() else {
            continue;
        };
        out.insert((name.to_string(), arch.to_string(), evr.to_string()), bytes);
    }
    out
}

/// Fill missing `Package.size_bytes` from a repoquery download-size map. Returns how many were set.
pub fn apply_download_sizes(
    packages: &mut [Package],
    sizes: &HashMap<(String, String, String), u64>,
) -> usize {
    let mut n = 0;
    for p in packages.iter_mut() {
        if p.size_bytes.is_some() {
            continue;
        }
        let key = (p.name.clone(), p.arch.clone(), p.version.clone());
        if let Some(&bytes) = sizes.get(&key) {
            p.size_bytes = Some(bytes);
            n += 1;
        }
    }
    n
}

pub fn parse_check_update(output: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("Last metadata") || is_noise_line(line) {
            continue;
        }
        if let Some(pkg) = parse_classic_line(line).or_else(|| parse_dnf5_table_line(line)) {
            if seen.insert(pkg.id.clone()) {
                packages.push(pkg);
            }
        }
    }
    packages
}

fn is_noise_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("updating and loading")
        || lower.starts_with("repositories loaded")
        || lower.starts_with("package ")
        || lower.starts_with("upgrading:")
        || lower.starts_with("installing:")
        || lower.starts_with("removing:")
        || lower.starts_with("obsoleting")
        || lower.starts_with("transaction summary")
        || lower.starts_with("total ")
        || lower.starts_with("skipping")
        || lower.contains("no match for argument")
        || line.starts_with("---")
        || line.starts_with("===")
        || (line.starts_with("Arch") && line.contains("Version"))
}

fn parse_classic_line(line: &str) -> Option<Package> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let (name, arch) = split_name_arch(parts[0])?;
    let version = parts[1].to_string();
    let repo = parts[2].to_string();
    if version.contains('/') {
        return None;
    }
    let size_bytes = (parts.len() > 3)
        .then(|| parse_size_bytes(&parts[3..].join(" ")))
        .flatten();
    let mut pkg = Package::new_dnf(name, arch, version, repo);
    pkg.size_bytes = size_bytes;
    Some(pkg)
}

fn parse_dnf5_table_line(line: &str) -> Option<Package> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    if parts[0].contains('.') && split_name_arch(parts[0]).is_some() {
        return None;
    }
    let name = parts[0].to_string();
    let arch = parts[1].to_string();
    if !is_plausible_arch(&arch) {
        return None;
    }
    let version = parts[2].to_string();
    let repo = parts[3].to_string();
    if !version.chars().any(|c| c.is_ascii_digit()) || name.eq_ignore_ascii_case("Package") {
        return None;
    }
    let size_bytes = (parts.len() > 4)
        .then(|| parse_size_bytes(&parts[4..].join(" ")))
        .flatten();
    let mut pkg = Package::new_dnf(name, arch, version, repo);
    pkg.size_bytes = size_bytes;
    Some(pkg)
}

fn split_name_arch(name_arch: &str) -> Option<(String, String)> {
    let idx = name_arch.rfind('.')?;
    if idx == 0 || idx + 1 >= name_arch.len() {
        return None;
    }
    let name = &name_arch[..idx];
    let arch = &name_arch[idx + 1..];
    if !is_plausible_arch(arch) || name.is_empty() {
        return None;
    }
    Some((name.to_string(), arch.to_string()))
}

fn is_plausible_arch(arch: &str) -> bool {
    matches!(
        arch,
        "x86_64"
            | "aarch64"
            | "i686"
            | "i386"
            | "noarch"
            | "src"
            | "armv7hl"
            | "ppc64le"
            | "s390x"
            | "riscv64"
    )
}

pub fn parse_progress_line(line: &str) -> ProgressHint {
    let trimmed = line.trim();
    // `Upgrading:` in the transaction preview is a section title, not the
    // install pass. Treating it as Installing made every later download look
    // applied and froze the card on the last finished download.
    if is_section_header(trimmed) {
        return ProgressHint::default();
    }
    let lower = trimmed.to_ascii_lowercase();
    let mut hint = ProgressHint::default();

    if lower.contains("downloading") {
        hint.phase_label = Some("Downloading packages".into());
        hint.status = Some(PackageStatus::Downloading);
    } else if is_verify_step(&lower) {
        hint.phase_label = Some("Verifying".into());
        hint.status = Some(PackageStatus::Installing);
    } else if lower.contains("installing")
        || lower.contains("upgrading")
        || lower.contains("erasing")
        || lower.contains("removing")
    {
        hint.phase_label = Some("Installing".into());
        hint.status = Some(PackageStatus::Installing);
    } else if is_transaction_step(&lower) {
        hint.phase_label = Some("Running transaction".into());
    } else if lower.contains("complete!") {
        hint.phase_label = Some("Complete".into());
        hint.progress = Some(1.0);
    }

    if let Some((cur, total, rest)) = parse_bracket_fraction(trimmed) {
        if total > 0 {
            hint.stage_current = Some(cur);
            hint.stage_total = Some(total);
        }
        if let Some(name) = extract_package_from_action_rest(rest) {
            hint.package_name = Some(name);
        }
    } else if let Some(name) = extract_named_action(trimmed) {
        hint.package_name = Some(name);
    }

    if let Some(pct) = super::extract_percent(trimmed) {
        hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
    }

    // dnf5 download rows are `[n/total] nevra  pp%` with no "downloading" word.
    // Those must not be treated as installs.
    if hint.status.is_none()
        && hint.phase_label.is_none()
        && hint.package_name.is_some()
        && hint.stage_current.is_some()
    {
        hint.status = Some(PackageStatus::Downloading);
        hint.phase_label = Some("Downloading packages".into());
    }

    hint
}

/// `Upgrading:` / `Installing:` at the top of the transaction table or inside
/// `Transaction Summary:` (e.g., `Installing: 1 package`).
fn is_section_header(line: &str) -> bool {
    if line.contains('[') {
        return false;
    }
    let is_section_verb = |verb: &str| {
        matches!(
            verb.trim().to_ascii_lowercase().as_str(),
            "upgrading"
                | "installing"
                | "removing"
                | "erasing"
                | "reinstalling"
                | "downgrading"
                | "replacing"
                | "obsoleting"
                | "skipping"
        )
    };
    if let Some((left, right)) = line.split_once(':') {
        if !is_section_verb(left) {
            return false;
        }
        let rest = right.trim();
        if rest.is_empty() {
            return true;
        }
        let mut tokens = rest.split_whitespace();
        return matches!(
            (tokens.next(), tokens.next()),
            (Some(count), Some(unit))
                if !count.is_empty()
                    && count.chars().all(|c| c.is_ascii_digit())
                    && matches!(unit.to_ascii_lowercase().as_str(), "package" | "packages")
        );
    }
    is_section_verb(line)
}

fn is_verify_step(lower: &str) -> bool {
    lower.contains("verifying") || lower.contains("verify ") || lower.contains("verify:")
}

fn is_transaction_step(lower: &str) -> bool {
    lower.contains("running transaction")
        || lower.contains("prepare transaction")
        || lower.contains("scriptlet")
        || lower.contains("cleanup")
}

fn parse_bracket_fraction(line: &str) -> Option<(u32, u32, &str)> {
    let start = line.find('[')?;
    let end = line.find(']')?;
    if end <= start {
        return None;
    }
    let inside = &line[start + 1..end];
    let (a, b) = inside.split_once('/')?;
    let cur: u32 = a.trim().parse().ok()?;
    let total: u32 = b.trim().parse().ok()?;
    Some((cur, total, line[end + 1..].trim()))
}

fn extract_named_action(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for key in [
        "installing:",
        "upgrading:",
        "verifying:",
        "erasing:",
        "removing:",
        "installing ",
        "upgrading ",
        "verifying ",
    ] {
        if let Some(idx) = lower.find(key) {
            let rest = line[idx + key.len()..].trim();
            return extract_package_from_action_rest(rest);
        }
    }
    None
}

fn extract_package_from_action_rest(s: &str) -> Option<String> {
    let mut parts = s.split_whitespace();
    let first = parts.next()?;
    if is_action_verb(first) {
        let second = parts.next()?;
        return extract_package_token(second);
    }
    extract_package_token(first)
}

fn extract_package_token(s: &str) -> Option<String> {
    let token = s.split_whitespace().next()?;
    if is_action_verb(token) {
        return None;
    }
    let name = strip_to_package_name(token);
    if name.is_empty()
        || !name.chars().any(|c| c.is_ascii_alphabetic())
        || is_action_verb(&name)
        || is_noise_name(&name)
    {
        None
    } else {
        Some(name)
    }
}

fn is_action_verb(token: &str) -> bool {
    matches!(
        token.trim_end_matches(':').to_ascii_lowercase().as_str(),
        "installing"
            | "upgrading"
            | "verifying"
            | "verify"
            | "erasing"
            | "removing"
            | "preparing"
            | "prepare"
            | "downloading"
            | "running"
            | "cleanup"
            | "scriptlet"
    )
}

/// Transaction words that are not RPM names (`Prepare transaction`, `Verify package files`).
fn is_noise_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "prepare"
            | "transaction"
            | "scriptlet"
            | "cleanup"
            | "verify"
            | "verifying"
            | "package"
            | "files"
            | "running"
            | "complete"
    )
}

pub fn strip_to_package_name(nevra: &str) -> String {
    let mut s = nevra.trim_matches(|c| c == ':' || c == ',').to_string();
    if let Some((epoch, rest)) = s.split_once(':') {
        if epoch.chars().all(|c| c.is_ascii_digit()) {
            s = rest.to_string();
        } else if let Some((prefix, _)) = epoch.rsplit_once('-') {
            if epoch[prefix.len() + 1..].chars().all(|c| c.is_ascii_digit()) {
                s = format!("{prefix}-{rest}");
            }
        }
    }
    if let Some(idx) = s.rfind('.') {
        let arch = &s[idx + 1..];
        if is_plausible_arch(arch) {
            s = s[..idx].to_string();
        }
    }
    split_name_from_vr(&s).unwrap_or(s)
}

fn split_name_from_vr(s: &str) -> Option<String> {
    let mut parts: Vec<&str> = s.split('-').collect();
    if parts.len() < 2 {
        return None;
    }
    let last = parts[parts.len() - 1];
    if last.chars().next()?.is_ascii_digit() || last.contains('.') {
        parts.pop();
        if parts
            .last()
            .and_then(|p| p.chars().next())
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            parts.pop();
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("-"))
    }
}

pub fn detect_reboot_needed(log: &str, packages: &[Package]) -> bool {
    if packages
        .iter()
        .any(|p| p.source == UpdateSource::Dnf && package_implies_reboot(&p.name))
    {
        return true;
    }
    let lower = log.to_ascii_lowercase();
    if lower.contains("no reboot") {
        return false;
    }
    lower.contains("requires a reboot")
        || lower.contains("reboot to apply")
        || lower.contains("reboot to complete")
        || lower.contains("reboot required")
        || lower.contains("restart your system")
        || lower.contains("needs-restarting")
}

/// Running kernel/modules images and a small set of core userspace that cannot
/// be fully replaced without a reboot. Userspace kernel add-ons (`kernel-tools`,
/// `kernel-devel`, headers) do not count.
pub fn package_implies_reboot(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if matches!(
        n.as_str(),
        "kernel"
            | "kernel-rt"
            | "kernel-debug"
            | "kernel-smp"
            | "kernel-pae"
            | "glibc"
            | "glibc-common"
            | "linux-firmware"
            | "microcode_ctl"
            | "amd-ucode-firmware"
            | "intel-microcode"
            | "systemd"
            | "systemd-libs"
            | "systemd-udev"
            | "dbus"
            | "dbus-broker"
            | "dbus-daemon"
            | "dracut"
    ) {
        return true;
    }
    for prefix in [
        "kernel-core",
        "kernel-modules",
        "kernel-rt-core",
        "kernel-rt-modules",
        "kernel-debug-core",
        "kernel-debug-modules",
        "kernel-uki",
        "grub2",
        "shim",
    ] {
        if n == prefix
            || n.strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('-'))
        {
            return true;
        }
    }
    false
}

/// dnf check-update: 0 = none, 100 = updates available; other non-zero is error unless packages parsed.
pub fn check_exit_is_ok(code: i32, packages: &[Package]) -> bool {
    code == 0 || code == 100 || !packages.is_empty()
}

pub fn check_exit_is_hard_error(code: i32, packages: &[Package]) -> bool {
    !check_exit_is_ok(code, packages)
}

/// Where `dnf` writes in-progress RPMs. Readable without root on a normal install.
pub const DNF_CACHE_ROOT: &str = "/var/cache/libdnf5";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheGrowth {
    pub package: String,
    pub bytes: u64,
    /// True when this path is larger than the previous sample.
    pub rising: bool,
}

/// Newest RPM under `root/<repo>/packages/` touched in the last few minutes.
/// `cursor` is the previous `(path, size)` sample; a larger size sets `rising`.
pub fn poll_cache_growth(root: &Path, cursor: &mut Option<(PathBuf, u64)>) -> Option<CacheGrowth> {
    let mut newest: Option<(PathBuf, SystemTime, u64)> = None;
    let Ok(repos) = std::fs::read_dir(root) else {
        return None;
    };
    for repo in repos.flatten() {
        let repo_path = repo.path();
        if !repo_path.is_dir() {
            continue;
        }
        let packages = repo_path.join("packages");
        let dir = if packages.is_dir() {
            packages
        } else {
            repo_path
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rpm") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let fresh = modified
                .elapsed()
                .map(|age| age < Duration::from_secs(180))
                .unwrap_or(false);
            if !fresh {
                continue;
            }
            let bytes = meta.len();
            let replace = newest
                .as_ref()
                .map(|(_, prev, _)| modified > *prev)
                .unwrap_or(true);
            if replace {
                newest = Some((path, modified, bytes));
            }
        }
    }
    let (path, _, bytes) = newest?;
    let package = package_name_from_rpm_filename(&path);
    let rising = cursor
        .as_ref()
        .is_some_and(|(prev, prev_bytes)| prev == &path && bytes > *prev_bytes);
    *cursor = Some((path, bytes));
    Some(CacheGrowth {
        package,
        bytes,
        rising,
    })
}

fn package_name_from_rpm_filename(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("package");
    strip_to_package_name(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_classic_check_update() {
        let out = r#"
Updating and loading repositories:
Repositories loaded.
Last metadata expiration check: 1:00:00 ago on Thu 01 Jan 2026.
---
kernel.x86_64                     6.14.11-300.fc42                updates
firefox.x86_64                    140.0-1.fc42                    updates
firefox.x86_64                    140.0-1.fc42                    updates
openssl.x86_64                    1:3.2.2-3.fc42                  updates
mesa-dri-drivers.i686             26.2.6-1.fc42                   updates
mesa-dri-drivers.x86_64           26.2.6-1.fc42                   updates
tzdata.noarch                     2026f-1.fc42                    updates
notanarch.bogus                   1.0-1                           updates
broken.x86_64                     2026/01/01                      updates
No match for argument: foo
"#;
        let pkgs = parse_check_update(out);
        assert_eq!(pkgs.len(), 6);
        assert_eq!(pkgs[0].name, "kernel");
        assert_eq!(pkgs[0].source, UpdateSource::Dnf);
        assert_eq!(pkgs[1].name, "firefox");
        assert_eq!(pkgs[2].version, "1:3.2.2-3.fc42");
        assert_eq!(pkgs[3].arch, "i686");
        assert_eq!(pkgs[4].arch, "x86_64");
        assert_ne!(pkgs[3].id, pkgs[4].id);
        assert_eq!(pkgs[5].name, "tzdata");
        assert_eq!(pkgs[5].arch, "noarch");
    }

    #[test]
    fn parse_check_update_parses_optional_size_bytes() {
        let out = r#"
Package                   Arch   Version                Repository       Size
==============================================================================
Upgrading:
 kernel.x86_64                     6.14.11-300.fc42                updates     98.2 MB
 firefox                   x86_64  140.0-1.fc42                    updates     112 MiB
 openssl.x86_64                    1:3.2.2-3.fc42                  updates
Obsoleting packages:
 pipewire-pulseaudio       x86_64  1.4.2-1.fc44                    updates     310.0 KiB
 pkgwithslash              x86_64  nodigits                        updates     10 KiB
 badrow                    bogus   1.0-1.fc44                      updates     10 KiB
Transaction Summary:
Total size of inbound packages is 211 MiB.
"#;
        let pkgs = parse_check_update(out);
        assert_eq!(pkgs.len(), 4);
        assert_eq!(pkgs[0].size_bytes, parse_size_bytes("98.2 MB"));
        assert_eq!(pkgs[0].size_label().as_deref(), Some("98 MB"));
        assert_eq!(pkgs[1].size_bytes, Some(112 * 1024 * 1024));
        assert_eq!(pkgs[1].size_label().as_deref(), Some("112 MB"));
        assert_eq!(pkgs[2].size_bytes, None);
        assert_eq!(pkgs[3].name, "pipewire-pulseaudio");
        assert_eq!(pkgs[3].size_bytes, parse_size_bytes("310.0 KiB"));
    }

    #[test]
    fn parse_check_update_recognizes_all_supported_arches() {
        for arch in [
            "x86_64", "aarch64", "i686", "i386", "noarch", "src", "armv7hl", "ppc64le", "s390x",
            "riscv64",
        ] {
            let line = format!("pkg.{arch} 1.2-3.fc44 updates");
            let pkgs = parse_check_update(&line);
            assert_eq!(pkgs.len(), 1, "classic arch `{arch}` should parse");
            assert_eq!(pkgs[0].arch, arch);

            let table_line = format!("pkg {arch} 1.2-3.fc44 updates 10 KiB");
            let tpkgs = parse_check_update(&table_line);
            assert_eq!(tpkgs.len(), 1, "table arch `{arch}` should parse");
            assert_eq!(tpkgs[0].arch, arch);
        }
    }

    #[test]
    fn parse_progress_fraction() {
        let h = parse_progress_line("[3/12] Installing gnome-shell-48.2-1.fc42.x86_64");
        assert_eq!(h.package_name.as_deref(), Some("gnome-shell"));
        assert_eq!(h.status, Some(PackageStatus::Installing));
        assert_eq!(h.stage_current, Some(3));
        assert_eq!(h.stage_total, Some(12));
        // The bracket is the transaction count. There is no per-package percent.
        assert_eq!(h.progress, None);
    }

    #[test]
    fn download_line_is_not_an_install_and_keeps_both_fractions() {
        let h = parse_progress_line(
            "[12/68] wireplumber-0.5.18-1.fc44.x86_64 100% | 1.2 MiB/s | 450.0 KiB",
        );
        assert_eq!(h.package_name.as_deref(), Some("wireplumber"));
        assert_eq!(h.status, Some(PackageStatus::Downloading));
        assert_eq!(h.phase_label.as_deref(), Some("Downloading packages"));
        assert_eq!(h.stage_current, Some(12));
        assert_eq!(h.stage_total, Some(68));
        assert_eq!(h.progress, Some(1.0));
    }

    #[test]
    fn upgrade_line_keeps_transaction_count_apart_from_item_percent() {
        let h = parse_progress_line(
            "[ 16/137] Upgrading glibc-0:2.43-9.fc44.x86_64 100% | 166.5 MiB/s | 1.0 MiB",
        );
        assert_eq!(h.package_name.as_deref(), Some("glibc"));
        assert_eq!(h.status, Some(PackageStatus::Installing));
        assert_eq!(h.phase_label.as_deref(), Some("Installing"));
        assert_eq!(h.stage_current, Some(16));
        assert_eq!(h.stage_total, Some(137));
        assert_eq!(h.progress, Some(1.0));
    }

    #[test]
    fn epoch_nevra_keeps_the_package_name() {
        let glibc =
            parse_progress_line("[3/137] Upgrading glibc-gconv-extra-0:2.43-9.fc44.x86_64 100%");
        assert_eq!(glibc.package_name.as_deref(), Some("glibc-gconv-extra"));
        let libs = parse_progress_line("[4/137] Upgrading bind-libs-32:9.18.50-2.fc44.x86_64");
        assert_eq!(libs.package_name.as_deref(), Some("bind-libs"));
        let sw = parse_progress_line("[16/137] Upgrading libswscale-free-0:8.0-1.fc44.x86_64 100%");
        assert_eq!(sw.package_name.as_deref(), Some("libswscale-free"));
        let legacy = parse_progress_line("[1/2] Upgrading 0:glibc-2.43-9.fc44.x86_64");
        assert_eq!(legacy.package_name.as_deref(), Some("glibc"));
    }

    #[test]
    fn transaction_steps_advance_the_count_without_a_package_name() {
        let prepare = parse_progress_line("[2/137] Prepare transaction 100% | 1.0 B/s");
        assert!(prepare.package_name.is_none());
        assert_eq!(prepare.stage_current, Some(2));
        assert_eq!(prepare.stage_total, Some(137));
        assert_eq!(prepare.phase_label.as_deref(), Some("Running transaction"));

        let verify = parse_progress_line("[1/137] Verify package files 100%");
        assert!(verify.package_name.is_none());
        assert_eq!(verify.status, Some(PackageStatus::Installing));
        assert_eq!(verify.phase_label.as_deref(), Some("Verifying"));
        assert_eq!(verify.stage_current, Some(1));
        assert_eq!(verify.stage_total, Some(137));
    }

    #[test]
    fn transaction_table_dump_is_not_a_named_package() {
        let h = parse_progress_line(
            "kernel                    x86_64 6.14.11-300.fc42        updates     98.2 MB",
        );
        assert!(h.package_name.is_none());
        for header in [
            "Upgrading:",
            "Installing:",
            "Removing:",
            " Installing:        1 package",
            " Upgrading:         3 packages",
            " Removing:          2 packages",
            " Reinstalling:      1 package",
            " Downgrading:       1 package",
            " Replacing:         1 package",
            " Obsoleting:        1 package",
            " Skipping:          1 package",
            "Transaction Summary:",
        ] {
            let hint = parse_progress_line(header);
            assert_eq!(
                hint,
                ProgressHint::default(),
                "header `{header}` should yield default hint"
            );
        }
    }

    #[test]
    fn progress_line_handles_cleanup_scriptlets_download_and_malformed_brackets() {
        let cleanup = parse_progress_line("[8/8] Cleanup / Verifying 100%");
        assert_eq!(cleanup.package_name, None);
        assert_eq!(cleanup.phase_label.as_deref(), Some("Verifying"));
        assert_eq!(cleanup.status, Some(PackageStatus::Installing));
        assert_eq!(cleanup.stage_current, Some(8));
        assert_eq!(cleanup.stage_total, Some(8));
        assert_eq!(cleanup.progress, Some(1.0));

        let scriptlet =
            parse_progress_line("[7/8] Running scriptlet: kernel-core-0:7.1.12-200.fc44.x86_64 100%");
        assert_eq!(scriptlet.package_name, None);
        assert_eq!(scriptlet.phase_label.as_deref(), Some("Running transaction"));
        assert_eq!(scriptlet.status, None);
        assert_eq!(scriptlet.stage_current, Some(7));
        assert_eq!(scriptlet.stage_total, Some(8));
        assert_eq!(scriptlet.progress, Some(1.0));

        let complete = parse_progress_line("Complete!");
        assert_eq!(complete.package_name, None);
        assert_eq!(complete.phase_label.as_deref(), Some("Complete"));
        assert_eq!(complete.progress, Some(1.0));

        let erase = parse_progress_line("[4/4] Erasing old-pkg-1.0-1.fc44.x86_64 50%");
        assert_eq!(erase.package_name.as_deref(), Some("old-pkg"));
        assert_eq!(erase.status, Some(PackageStatus::Installing));
        assert_eq!(erase.phase_label.as_deref(), Some("Installing"));
        assert_eq!(erase.progress, Some(0.5));

        let downloading = parse_progress_line("Downloading Packages:");
        assert_eq!(downloading.status, Some(PackageStatus::Downloading));
        assert!(downloading.package_name.is_none());

        let zero_total = parse_progress_line("[1/0] Upgrading firefox-140.0-1.fc44.x86_64");
        assert_eq!(zero_total.stage_current, None);
        assert_eq!(zero_total.stage_total, None);
        assert_eq!(zero_total.package_name.as_deref(), Some("firefox"));

        let reversed = parse_progress_line("]1/2[ Upgrading firefox-140.0-1.fc44.x86_64");
        assert_eq!(reversed.stage_current, None);
        assert_eq!(reversed.package_name.as_deref(), Some("firefox"));

        for err_line in [
            "Error: Could not acquire lock on /var/lib/rpm/.rpm.lock",
            "Problem 1: package foo conflicts with bar",
            "Failed to download metadata for repo 'updates'",
        ] {
            let h = parse_progress_line(err_line);
            assert!(h.status.is_none(), "error `{err_line}` set status");
            assert!(h.package_name.is_none(), "error `{err_line}` set package");
        }
    }

    #[test]
    fn check_exit_codes() {
        for code in [0, 100] {
            assert!(check_exit_is_ok(code, &[]));
            assert!(!check_exit_is_hard_error(code, &[]));
        }
        for code in [1, 2, 130, -1] {
            assert!(!check_exit_is_ok(code, &[]));
            assert!(check_exit_is_hard_error(code, &[]));
        }
        let pkgs = vec![Package::new_dnf("a", "x86_64", "1", "updates")];
        for code in [0, 1, 2, 100, 130] {
            assert!(check_exit_is_ok(code, &pkgs));
            assert!(!check_exit_is_hard_error(code, &pkgs));
        }
    }

    #[test]
    fn kernel_core_in_pending_transaction_implies_reboot() {
        let pkg = Package::new_dnf("kernel-core", "x86_64", "7.1.12-200.fc44", "updates");
        assert_eq!(pkg.status, PackageStatus::Pending);
        assert!(detect_reboot_needed("", &[pkg]));
    }

    #[test]
    fn kernel_tools_and_devel_do_not_imply_reboot() {
        let tools = Package::new_dnf("kernel-tools", "x86_64", "7.1.12-200.fc44", "updates");
        let libs = Package::new_dnf("kernel-tools-libs", "x86_64", "7.1.12-200.fc44", "updates");
        let devel = Package::new_dnf("kernel-devel", "x86_64", "7.1.12-200.fc44", "updates");
        let matched = Package::new_dnf(
            "kernel-devel-matched",
            "x86_64",
            "7.1.12-200.fc44",
            "updates",
        );
        assert!(!detect_reboot_needed("", &[tools, libs, devel, matched]));
    }

    #[test]
    fn core_system_packages_imply_reboot() {
        for name in [
            "kernel",
            "KERNEL",
            "kernel-rt",
            "kernel-debug",
            "kernel-smp",
            "kernel-pae",
            "kernel-core",
            "kernel-core-extra",
            "kernel-modules",
            "kernel-modules-extra",
            "kernel-rt-core",
            "kernel-rt-modules-core",
            "kernel-debug-core",
            "kernel-debug-modules-extra",
            "kernel-uki-virt",
            "glibc",
            "glibc-common",
            "linux-firmware",
            "microcode_ctl",
            "amd-ucode-firmware",
            "intel-microcode",
            "systemd",
            "systemd-libs",
            "systemd-udev",
            "dbus",
            "dbus-broker",
            "dbus-daemon",
            "dracut",
            "grub2-common",
            "grub2-efi-x64",
            "shim-x64",
        ] {
            let pkg = Package::new_dnf(name, "x86_64", "1", "updates");
            assert!(
                detect_reboot_needed("", &[pkg]),
                "{name} should recommend a reboot"
            );
        }
    }

    #[test]
    fn ordinary_packages_do_not_imply_reboot() {
        for name in [
            "firefox",
            "openssl",
            "kernel-headers",
            "systemd-resolved",
            "glibc-langpack-en",
            "dracut-config-reboot",
            "shimmer",
            "grubby",
        ] {
            let pkg = Package::new_dnf(name, "x86_64", "1", "updates");
            assert!(
                !detect_reboot_needed("", &[pkg]),
                "{name} must not recommend a reboot"
            );
        }
        // Non-DNF package named kernel must not trigger DNF reboot check.
        let mut flatpak_kernel = Package::new_dnf("kernel", "", "1", "flathub");
        flatpak_kernel.source = UpdateSource::FlatpakUser;
        assert!(!detect_reboot_needed("", &[flatpak_kernel]));
    }

    #[test]
    fn dnf_log_all_reboot_phrases_trigger_reboot() {
        let firefox = Package::new_dnf("firefox", "x86_64", "1", "updates");
        for phrase in [
            "This update requires a reboot",
            "Reboot to apply changes.",
            "Reboot to complete installation.",
            "System reboot required",
            "Please restart your system soon",
            "needs-restarting: core libraries updated",
        ] {
            assert!(
                detect_reboot_needed(phrase, std::slice::from_ref(&firefox)),
                "phrase `{phrase}` should imply reboot"
            );
        }
    }

    #[test]
    fn dnf_log_reboot_phrase_still_counts() {
        let firefox = Package::new_dnf("firefox", "x86_64", "1", "updates");
        assert!(detect_reboot_needed(
            "Complete!\nReboot to apply changes.",
            &[firefox]
        ));
    }

    #[test]
    fn dnf_log_negative_and_package_name_reboot_phrases_do_not_false_positive() {
        let firefox = Package::new_dnf("firefox", "x86_64", "1", "updates");
        assert!(!detect_reboot_needed(
            "No reboot required",
            std::slice::from_ref(&firefox)
        ));
        assert!(!detect_reboot_needed(
            "Upgrading: dracut-config-reboot",
            &[firefox]
        ));

        let kernel = Package::new_dnf("kernel", "x86_64", "6.14.11-300.fc42", "updates");
        assert!(detect_reboot_needed("No reboot required", &[kernel]));
    }

    #[test]
    fn repoquery_format_separates_packages_with_newlines() {
        let qf = REPOQUERY_UPGRADES_ARGS
            .iter()
            .skip_while(|a| **a != "--queryformat")
            .nth(1)
            .expect("queryformat value");
        assert!(
            qf.ends_with('\n'),
            "dnf5 concatenates packages unless queryformat ends with a newline"
        );
    }

    #[test]
    fn parse_repoquery_sizes_skips_noise_and_matches_nevra() {
        let out = r#"
Updating and loading repositories:
Repositories loaded.
short line
badarch	bogus	1.0-1	999
nonnumeric	x86_64	1.0-1	NaN
kernel-core	x86_64	7.1.12-200.fc44	21727761
firefox	x86_64	140.0-1.fc42	117440512
bind-libs	x86_64	32:9.18.50-2.fc44	1388459
tzdata noarch 2026f-1.fc44 720896
"#;
        let sizes = parse_repoquery_sizes(out);
        assert_eq!(sizes.len(), 4);
        assert_eq!(
            sizes.get(&(
                "kernel-core".into(),
                "x86_64".into(),
                "7.1.12-200.fc44".into()
            )),
            Some(&21727761)
        );
        assert_eq!(
            sizes.get(&(
                "bind-libs".into(),
                "x86_64".into(),
                "32:9.18.50-2.fc44".into()
            )),
            Some(&1388459)
        );
        assert_eq!(
            sizes.get(&("tzdata".into(), "noarch".into(), "2026f-1.fc44".into())),
            Some(&720896)
        );

        let mut pre_sized = Package::new_dnf("firefox", "x86_64", "140.0-1.fc42", "updates");
        pre_sized.size_bytes = Some(42);
        let mut pkgs = vec![
            Package::new_dnf("kernel-core", "x86_64", "7.1.12-200.fc44", "updates"),
            pre_sized,
            Package::new_dnf("mystery", "x86_64", "1", "updates"),
        ];
        let n = apply_download_sizes(&mut pkgs, &sizes);
        assert_eq!(n, 1);
        assert_eq!(pkgs[0].size_bytes, Some(21_727_761));
        assert_eq!(pkgs[0].size_label().as_deref(), Some("21 MB"));
        // Pre-populated sizes must be preserved and excluded from the returned count.
        assert_eq!(pkgs[1].size_bytes, Some(42));
        assert!(pkgs[2].size_bytes.is_none());
    }

    #[test]
    fn cache_growth_reports_the_newest_rpm_and_whether_it_grew() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Missing root directory and empty directory both return None.
        let mut cursor = None;
        assert!(poll_cache_growth(&root.join("missing"), &mut cursor).is_none());
        assert!(cursor.is_none());

        let packages = root.join("grok-bot-deadbeef").join("packages");
        std::fs::create_dir_all(&packages).unwrap();
        assert!(poll_cache_growth(root, &mut cursor).is_none());

        // Non-.rpm files and stale (>180s) RPMs are ignored.
        std::fs::write(packages.join("ignore.part"), vec![0u8; 4096]).unwrap();
        let stale = packages.join("stale-1.0-1.fc44.x86_64.rpm");
        std::fs::write(&stale, vec![0u8; 500]).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(300))
            .unwrap();
        assert!(poll_cache_growth(root, &mut cursor).is_none());

        // First fresh RPM sampled: rising is false until size grows.
        let rpm = packages.join("grok-bot-0.79.2-1.x86_64.rpm");
        std::fs::write(&rpm, vec![0u8; 1000]).unwrap();
        let older_mtime = SystemTime::now() - Duration::from_secs(30);
        std::fs::File::options()
            .write(true)
            .open(&rpm)
            .unwrap()
            .set_modified(older_mtime)
            .unwrap();
        let first = poll_cache_growth(root, &mut cursor).expect("fresh rpm");
        assert_eq!(first.package, "grok-bot");
        assert!(!first.rising);
        assert_eq!(first.bytes, 1000);

        // Same size on steady poll keeps rising == false.
        let steady = poll_cache_growth(root, &mut cursor).expect("steady rpm");
        assert!(!steady.rising);
        assert_eq!(steady.bytes, 1000);

        // Growth on the tracked RPM flips rising == true.
        std::fs::write(&rpm, vec![0u8; 2500]).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&rpm)
            .unwrap()
            .set_modified(older_mtime + Duration::from_secs(5))
            .unwrap();
        let second = poll_cache_growth(root, &mut cursor).expect("grew");
        assert!(second.rising);
        assert_eq!(second.bytes, 2500);

        // Rotation to a newer RPM directly inside a repo dir switches the cursor.
        let direct_repo = root.join("updates-abc");
        std::fs::create_dir_all(&direct_repo).unwrap();
        let next_rpm = direct_repo.join("firefox-140.0-1.fc44.x86_64.rpm");
        std::fs::write(&next_rpm, vec![0u8; 4000]).unwrap();
        let rotated = poll_cache_growth(root, &mut cursor).expect("rotated rpm");
        assert_eq!(rotated.package, "firefox");
        assert!(!rotated.rising);
        assert_eq!(rotated.bytes, 4000);

        std::fs::write(&next_rpm, vec![0u8; 6000]).unwrap();
        let rotated_grew = poll_cache_growth(root, &mut cursor).expect("rotated rpm grew");
        assert_eq!(rotated_grew.package, "firefox");
        assert!(rotated_grew.rising);
        assert_eq!(rotated_grew.bytes, 6000);
    }
}
