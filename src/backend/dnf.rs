//! Parsers for `dnf check-update --refresh` and live `dnf update -y` output.

use crate::model::{Package, PackageStatus, UpdateSource};

use super::ProgressHint;

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
    let size = parts.get(3).and_then(|s| {
        if s.chars().any(|c| c.is_ascii_digit()) {
            Some(parts[3..].join(" "))
        } else {
            None
        }
    });
    let mut pkg = Package::new_dnf(name, arch, version, repo);
    pkg.size = size;
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
    let size = if parts.len() > 4 {
        Some(parts[4..].join(" "))
    } else {
        None
    };
    let mut pkg = Package::new_dnf(name, arch, version, repo);
    pkg.size = size;
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
    let lower = trimmed.to_ascii_lowercase();
    let mut hint = ProgressHint::default();

    if lower.contains("downloading") {
        hint.phase_label = Some("Downloading packages".into());
        hint.status = Some(PackageStatus::Downloading);
    } else if lower.contains("running transaction") {
        hint.phase_label = Some("Running transaction".into());
    } else if lower.contains("verifying") {
        hint.phase_label = Some("Verifying".into());
        hint.status = Some(PackageStatus::Installing);
    } else if lower.contains("installing") || lower.contains("upgrading") {
        hint.phase_label = Some("Installing".into());
        hint.status = Some(PackageStatus::Installing);
    } else if lower.contains("complete!") {
        hint.phase_label = Some("Complete".into());
        hint.progress = Some(1.0);
    }

    if let Some((cur, total, rest)) = parse_bracket_fraction(trimmed) {
        if total > 0 {
            hint.progress = Some((cur as f64 / total as f64).clamp(0.0, 1.0));
        }
        if let Some(name) = extract_package_from_action_rest(rest) {
            hint.package_name = Some(name);
        }
    } else if let Some(name) = extract_named_action(trimmed) {
        hint.package_name = Some(name);
    } else if let Some(name) = extract_dnf5_table_package(trimmed) {
        // dnf5 often prints bare "name arch version repo size" lines mid-transaction
        hint.package_name = Some(name);
        if hint.status.is_none() {
            hint.status = Some(PackageStatus::Installing);
        }
        if hint.phase_label.is_none() {
            hint.phase_label = Some("Updating packages".into());
        }
    }

    if let Some(pct) = extract_percent(trimmed) {
        hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
    }

    hint
}

/// First token that looks like a package name followed by a plausible arch.
fn extract_dnf5_table_package(line: &str) -> Option<String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let name = parts[0];
    let arch = parts[1];
    if !is_plausible_arch(arch) {
        // Maybe name.arch form
        if let Some((n, a)) = split_name_arch(name) {
            if is_plausible_arch(&a) {
                return Some(n);
            }
        }
        return None;
    }
    if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '+' || c == '.')
        && !name.eq_ignore_ascii_case("Package")
        && !name.starts_with('[')
    {
        Some(name.to_string())
    } else {
        None
    }
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
    if name.is_empty() {
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
            | "erasing"
            | "removing"
            | "preparing"
            | "downloading"
            | "running"
    )
}

fn strip_to_package_name(nevra: &str) -> String {
    let mut s = nevra.trim_matches(|c| c == ':' || c == ',').to_string();
    if let Some((epoch, rest)) = s.split_once(':') {
        if epoch.chars().all(|c| c.is_ascii_digit()) {
            s = rest.to_string();
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
    while parts.len() >= 2 {
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
            break;
        }
        break;
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("-"))
    }
}

fn extract_percent(line: &str) -> Option<f64> {
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

pub fn detect_reboot_needed(log: &str, packages: &[Package]) -> bool {
    let lower = log.to_ascii_lowercase();
    if lower.contains("reboot") || lower.contains("restart your system") {
        return true;
    }
    packages.iter().any(|p| {
        p.source == UpdateSource::Dnf
            && p.status == PackageStatus::Completed
            && (p.name == "kernel"
                || p.name.starts_with("kernel-core")
                || p.name.starts_with("kernel-")
                || p.name == "linux-firmware"
                || p.name == "glibc")
    })
}

/// dnf check-update: 0 = none, 100 = updates available; other non-zero is error unless packages parsed.
pub fn check_exit_is_ok(code: i32, packages: &[Package]) -> bool {
    code == 0 || code == 100 || !packages.is_empty()
}

pub fn check_exit_is_hard_error(code: i32, packages: &[Package]) -> bool {
    !check_exit_is_ok(code, packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_classic_check_update() {
        let out = r#"
Last metadata expiration check: 1:00:00 ago on Thu 01 Jan 2026.
kernel.x86_64                     6.14.11-300.fc42                updates
firefox.x86_64                    140.0-1.fc42                    updates
openssl.x86_64                    1:3.2.2-3.fc42                  updates
"#;
        let pkgs = parse_check_update(out);
        assert_eq!(pkgs.len(), 3);
        assert_eq!(pkgs[0].name, "kernel");
        assert_eq!(pkgs[0].source, UpdateSource::Dnf);
        assert_eq!(pkgs[2].version, "1:3.2.2-3.fc42");
    }

    #[test]
    fn parse_progress_fraction() {
        let h = parse_progress_line("[3/12] Installing gnome-shell-48.2-1.fc42.x86_64");
        assert_eq!(h.package_name.as_deref(), Some("gnome-shell"));
        assert!(h.progress.unwrap() > 0.2);
    }

    #[test]
    fn check_exit_codes() {
        assert!(check_exit_is_ok(0, &[]));
        assert!(check_exit_is_ok(100, &[]));
        assert!(check_exit_is_hard_error(1, &[]));
        let pkgs = vec![Package::new_dnf("a", "x86_64", "1", "updates")];
        assert!(!check_exit_is_hard_error(1, &pkgs));
    }
}
