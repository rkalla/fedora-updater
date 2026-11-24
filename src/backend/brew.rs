//! Homebrew (`brew`) user-space discovery, JSON v2 parsing, and progress hints.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::Deserialize;

use crate::model::{Package, PackageStatus, UpdateSource};

use super::ProgressHint;

/// Standard Linuxbrew installation path checked even under minimal `.desktop` `PATH`.
pub const LINUXBREW_SYSTEM_PATH: &str = "/home/linuxbrew/.linuxbrew/bin/brew";

/// Non-interactive environment for `brew update --quiet`.
pub const CHECK_REFRESH_ENV: &[(&str, &str)] = &[
    ("NONINTERACTIVE", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("HOMEBREW_NO_ENV_HINTS", "1"),
    ("HOMEBREW_NO_COLOR", "1"),
];

/// Non-interactive environment disabling implicit `brew` auto-updates during check/apply.
pub const NONINTERACTIVE_NO_AUTO_UPDATE_ENV: &[(&str, &str)] = &[
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
    ("HOMEBREW_NO_ENV_HINTS", "1"),
    ("HOMEBREW_NO_COLOR", "1"),
    ("NONINTERACTIVE", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_file() && (meta.permissions().mode() & 0o111) != 0
}

/// Auto-detect `brew` via `PATH`, `/home/linuxbrew/.linuxbrew/bin/brew`, and `~/.linuxbrew/bin/brew`.
pub fn find_brew_binary() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    let home_env = std::env::var_os("HOME");
    find_brew_binary_with(
        path_env.as_deref(),
        home_env.as_deref(),
        Path::new(LINUXBREW_SYSTEM_PATH),
    )
}

/// Injectable version of [`find_brew_binary`] for offline unit tests.
pub fn find_brew_binary_with(
    path_env: Option<&OsStr>,
    home_env: Option<&OsStr>,
    system_fallback: &Path,
) -> Option<PathBuf> {
    if let Some(paths) = path_env.filter(|p| !p.is_empty()) {
        for dir in std::env::split_paths(paths) {
            let candidate = dir.join("brew");
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    if is_executable_file(system_fallback) {
        return Some(system_fallback.to_path_buf());
    }
    if let Some(home) = home_env.filter(|h| !h.is_empty()) {
        let home_candidate = PathBuf::from(home).join(".linuxbrew/bin/brew");
        if is_executable_file(&home_candidate) {
            return Some(home_candidate);
        }
    }
    None
}

fn deserialize_versions<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct VersionsVisitor;

    impl<'de> Visitor<'de> for VersionsVisitor {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a version string, array of version strings, or null")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![v.to_string()])
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![v])
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                out.push(item);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_any(VersionsVisitor)
}

#[derive(Debug, Deserialize)]
struct OutdatedPayload {
    formulae: Option<Vec<OutdatedFormula>>,
    casks: Option<Vec<OutdatedCask>>,
}

#[derive(Debug, Deserialize)]
struct OutdatedFormula {
    name: String,
    #[serde(default, deserialize_with = "deserialize_versions")]
    installed_versions: Vec<String>,
    current_version: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pinned_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutdatedCask {
    name: String,
    #[serde(default, deserialize_with = "deserialize_versions")]
    installed_versions: Vec<String>,
    current_version: String,
    #[serde(default)]
    pinned: bool,
}

fn find_json_object_start(trimmed: &str) -> Option<&str> {
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

fn extract_tap(full_name: &str, default_tap: &str) -> String {
    match full_name.rsplit_once('/') {
        Some((tap, _)) if !tap.trim().is_empty() => tap.trim().to_string(),
        _ => default_tap.to_string(),
    }
}

fn latest_installed_version(versions: &[String]) -> Option<String> {
    versions
        .iter()
        .rev()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .map(ToString::to_string)
}

/// Parse `brew outdated --json=v2` output into pending Homebrew [`Package`] items,
/// skipping pinned items and capturing `old_version`, `version`, and tap metadata.
pub fn parse_outdated_json(output: &str) -> Result<Vec<Package>, String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Err("empty brew outdated output".into());
    }
    let json_slice = find_json_object_start(trimmed)
        .ok_or_else(|| "brew outdated output did not contain a JSON object".to_string())?;

    let payload: OutdatedPayload = serde_json::from_str(json_slice)
        .map_err(|e| format!("invalid brew outdated JSON: {e}"))?;

    if payload.formulae.is_none() && payload.casks.is_none() {
        return Err("brew outdated JSON missing `formulae` and `casks` fields".into());
    }

    let mut packages = Vec::new();
    let mut seen = HashSet::new();

    for formula in payload.formulae.unwrap_or_default() {
        if formula.pinned {
            continue;
        }
        let name = formula.name.trim();
        let current = formula.current_version.trim();
        if name.is_empty() || current.is_empty() {
            continue;
        }
        let old = latest_installed_version(&formula.installed_versions);
        let tap = extract_tap(name, "homebrew/core");
        let pkg = Package::new_homebrew(name, old, current, tap, false);
        if seen.insert(pkg.id.clone()) {
            packages.push(pkg);
        }
    }

    for cask in payload.casks.unwrap_or_default() {
        if cask.pinned {
            continue;
        }
        let name = cask.name.trim();
        let current = cask.current_version.trim();
        if name.is_empty() || current.is_empty() {
            continue;
        }
        let old = latest_installed_version(&cask.installed_versions);
        let tap = extract_tap(name, "homebrew/cask");
        let pkg = Package::new_homebrew(name, old, current, tap, true);
        if seen.insert(pkg.id.clone()) {
            packages.push(pkg);
        }
    }

    Ok(packages)
}

/// Extract deduplicated Homebrew formula/cask names for `brew upgrade` or `brew fetch`.
pub fn upgrade_names_from_packages(packages: &[Package]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for pkg in packages
        .iter()
        .filter(|p| p.source == UpdateSource::Homebrew)
    {
        let raw_name = pkg
            .id
            .strip_prefix("brew:")
            .and_then(|rest| rest.rsplit_once(':').map(|(n, _)| n))
            .unwrap_or(pkg.name.as_str());
        let target = raw_name.trim();
        if !target.is_empty() && seen.insert(target.to_string()) {
            out.push(target.to_string());
        }
    }
    out
}

/// Extract best-effort [`ProgressHint`] from one `brew upgrade` or `brew fetch` output line.
pub fn parse_progress_line(line: &str) -> ProgressHint {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ProgressHint::default();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("error:") || lower.starts_with("warning:") {
        return ProgressHint::default();
    }

    let mut hint = ProgressHint::default();

    if let Some(rest) = trimmed.strip_prefix("🍺") {
        if let Some(pkg) = extract_completion_package(rest.trim()) {
            hint.package_name = Some(pkg);
            hint.status = Some(PackageStatus::Completed);
            hint.phase_label = Some("Complete".into());
            hint.progress = Some(1.0);
        }
        return hint;
    }

    if let Some(body) = trimmed.strip_prefix("==>") {
        let body = body.trim();
        let body_lower = body.to_ascii_lowercase();
        if let Some(rest) = strip_prefix_ci(body, "Downloading ") {
            hint.status = Some(PackageStatus::Downloading);
            hint.phase_label = Some("Downloading packages".into());
            hint.package_name = extract_download_package(rest);
        } else if let Some(rest) = strip_prefix_ci(body, "Fetching ") {
            hint.status = Some(PackageStatus::Downloading);
            hint.phase_label = Some("Downloading packages".into());
            let rest_lower = rest.trim_start().to_ascii_lowercase();
            if !rest_lower.starts_with("dependencies") && !rest_lower.starts_with("downloads") {
                hint.package_name = extract_action_package(rest);
            }
        } else if let Some(rest) = strip_prefix_ci(body, "Pouring ") {
            hint.status = Some(PackageStatus::Installing);
            hint.phase_label = Some("Installing".into());
            hint.package_name = extract_pouring_package(rest);
        } else if let Some(rest) = strip_prefix_ci(body, "Upgrading ") {
            if !is_outdated_summary_header(rest) {
                hint.status = Some(PackageStatus::Installing);
                hint.phase_label = Some("Upgrading".into());
                hint.package_name = extract_action_package(rest);
            }
        } else if let Some(rest) = strip_prefix_ci(body, "Installing ") {
            let rest_lower = rest.trim_start().to_ascii_lowercase();
            if !rest_lower.starts_with("dependencies") && !is_outdated_summary_header(rest) {
                hint.status = Some(PackageStatus::Installing);
                hint.phase_label = Some("Installing".into());
                hint.package_name = extract_action_package(rest);
            }
        } else if body_lower.starts_with("running `brew cleanup") {
            hint.phase_label = Some("Cleanup".into());
        }
    }

    if hint.progress.is_none() && (hint.status.is_some() || hint.phase_label.is_some()) {
        if let Some(pct) = super::extract_percent(trimmed) {
            hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
        }
    }

    hint
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn is_outdated_summary_header(rest: &str) -> bool {
    let mut parts = rest.split_whitespace();
    let Some(first) = parts.next() else {
        return true;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    first.chars().all(|c| c.is_ascii_digit())
        && matches!(
            second.trim_end_matches(':').to_ascii_lowercase().as_str(),
            "outdated" | "package" | "packages" | "formula" | "formulae" | "cask" | "casks"
        )
}

fn extract_action_package(rest: &str) -> Option<String> {
    let mut token = rest.split_whitespace().next()?;
    if token.eq_ignore_ascii_case("cask") || token.eq_ignore_ascii_case("formula") {
        token = rest.split_whitespace().nth(1)?;
    }
    let cleaned = token.trim_end_matches(':').trim();
    if cleaned.is_empty() || cleaned.chars().all(|c| c.is_ascii_digit()) {
        None
    } else {
        Some(cleaned.to_string())
    }
}

fn extract_pouring_package(rest: &str) -> Option<String> {
    let token = rest.split_whitespace().next()?;
    let filename = token.rsplit('/').next().unwrap_or(token);
    let name = filename
        .split_once("--")
        .map(|(pkg, _)| pkg)
        .unwrap_or(filename)
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name.replace("%40", "@"))
    }
}

fn extract_download_package(rest: &str) -> Option<String> {
    let token = rest.split_whitespace().next()?;
    let without_query = token.split(['?', '#']).next().unwrap_or(token);
    let filename = without_query.rsplit('/').next().unwrap_or(without_query);
    if let Some((pkg, _)) = filename.split_once("--") {
        let clean = pkg.trim();
        if !clean.is_empty() {
            return Some(clean.replace("%40", "@"));
        }
    }
    let segments: Vec<&str> = without_query.split('/').filter(|s| !s.is_empty()).collect();
    for (idx, seg) in segments.iter().enumerate() {
        if (*seg == "manifests" || *seg == "blobs") && idx >= 1 {
            let prev = segments[idx - 1];
            // Versioned GHCR formulas look like `.../homebrew/core/python/3.12/manifests/...`
            if idx >= 2
                && prev
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
            {
                let base = segments[idx - 2];
                if !matches!(base, "core" | "cask" | "v2") {
                    return Some(format!("{base}@{prev}"));
                }
            }
            if !matches!(prev, "core" | "cask" | "api" | "formula") {
                return Some(prev.replace("%40", "@"));
            }
        }
    }
    None
}

fn extract_completion_package(rest: &str) -> Option<String> {
    for marker in ["/Cellar/", "/Caskroom/"] {
        if let Some((_, tail)) = rest.split_once(marker) {
            let pkg = tail.split(['/', ':']).next()?.trim();
            if !pkg.is_empty() {
                return Some(pkg.to_string());
            }
        }
    }
    let lower = rest.to_ascii_lowercase();
    if let Some(idx) = lower.find(" was successfully ") {
        let candidate = rest[..idx].trim();
        if !candidate.is_empty() && !candidate.contains(' ') {
            return Some(candidate.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_exec_script(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fake brew");
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    #[test]
    fn find_brew_binary_checks_path_linuxbrew_and_home_fallbacks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path_dir = dir.path().join("bin");
        let sys_dir = dir.path().join("linuxbrew/bin");
        let home_dir = dir.path().join("home");
        let home_brew_dir = home_dir.join(".linuxbrew/bin");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::create_dir_all(&sys_dir).unwrap();
        std::fs::create_dir_all(&home_brew_dir).unwrap();

        let sys_brew = sys_dir.join("brew");
        // Non-executable file must be ignored.
        std::fs::write(&sys_brew, "not executable").unwrap();
        assert_eq!(
            find_brew_binary_with(Some(path_dir.as_os_str()), Some(home_dir.as_os_str()), &sys_brew),
            None
        );

        // Home fallback resolves when PATH and system fallback are missing/non-exec.
        let home_brew = write_exec_script(&home_brew_dir, "brew");
        assert_eq!(
            find_brew_binary_with(Some(path_dir.as_os_str()), Some(home_dir.as_os_str()), &sys_brew),
            Some(home_brew)
        );

        // System /home/linuxbrew/.linuxbrew/bin/brew beats ~/.linuxbrew/bin/brew when PATH lacks brew.
        let sys_exec = write_exec_script(&sys_dir, "brew");
        assert_eq!(
            find_brew_binary_with(Some(path_dir.as_os_str()), Some(home_dir.as_os_str()), &sys_exec),
            Some(sys_exec.clone())
        );

        // Explicit PATH entry wins over fallbacks.
        let path_brew = write_exec_script(&path_dir, "brew");
        assert_eq!(
            find_brew_binary_with(Some(path_dir.as_os_str()), Some(home_dir.as_os_str()), &sys_exec),
            Some(path_brew)
        );
    }

    #[test]
    fn parse_outdated_json_handles_formulae_casks_pinned_multi_versions_and_errors() {
        let json = r#"
Warning: Some tap failed to refresh
{
  "formulae": [
    {
      "name": "ripgrep",
      "installed_versions": ["14.1.0"],
      "current_version": "14.1.1",
      "pinned": false,
      "pinned_version": null
    },
    {
      "name": "neovim",
      "installed_versions": ["0.10.2", "0.10.3"],
      "current_version": "0.10.4",
      "pinned": false,
      "pinned_version": null
    },
    {
      "name": "oven-sh/bun/bun",
      "installed_versions": ["1.1.30"],
      "current_version": "1.1.38",
      "pinned": false,
      "pinned_version": null
    },
    {
      "name": "python@3.12",
      "installed_versions": ["3.12.6"],
      "current_version": "3.12.7",
      "pinned": true,
      "pinned_version": "3.12.6"
    }
  ],
  "casks": [
    {
      "name": "font-jetbrains-mono-nerd-font",
      "installed_versions": ["3.2.1"],
      "current_version": "3.3.0"
    }
  ]
}
"#;
        let pkgs = parse_outdated_json(json).expect("parse outdated json");
        assert_eq!(pkgs.len(), 4);

        assert_eq!(pkgs[0].id, "brew:ripgrep:14.1.1");
        assert_eq!(pkgs[0].name, "ripgrep");
        assert_eq!(pkgs[0].old_version.as_deref(), Some("14.1.0"));
        assert_eq!(pkgs[0].version, "14.1.1");
        assert_eq!(pkgs[0].repo, "homebrew/core");
        assert_eq!(pkgs[0].detail, "14.1.1 · homebrew/core · formula");
        assert_eq!(pkgs[0].source, UpdateSource::Homebrew);

        // Multi-version installed_versions picks latest installed version.
        assert_eq!(pkgs[1].name, "neovim");
        assert_eq!(pkgs[1].old_version.as_deref(), Some("0.10.3"));
        assert_eq!(pkgs[1].version, "0.10.4");

        // Third-party tap preserves tap in repo/detail and full name in id/name.
        assert_eq!(pkgs[2].id, "brew:oven-sh/bun/bun:1.1.38");
        assert_eq!(pkgs[2].name, "oven-sh/bun/bun");
        assert_eq!(pkgs[2].repo, "oven-sh/bun");
        assert_eq!(pkgs[2].detail, "1.1.38 · oven-sh/bun · formula");

        // Cask included cleanly.
        assert_eq!(pkgs[3].id, "brew:font-jetbrains-mono-nerd-font:3.3.0");
        assert_eq!(pkgs[3].repo, "homebrew/cask");
        assert_eq!(pkgs[3].detail, "3.3.0 · homebrew/cask · cask");

        // Empty outdated response yields Ok(empty).
        assert!(parse_outdated_json(r#"{"formulae": [], "casks": []}"#)
            .unwrap()
            .is_empty());

        // Cask single-string installed_versions legacy shape works too.
        let str_cask = parse_outdated_json(
            r#"{"formulae":[],"casks":[{"name":"raycast","installed_versions":"1.0","current_version":"1.1"}]}"#,
        )
        .unwrap();
        assert_eq!(str_cask[0].old_version.as_deref(), Some("1.0"));

        // Invalid / malformed / wrong-schema JSON returns Err.
        assert!(parse_outdated_json("").is_err());
        assert!(parse_outdated_json("not json").is_err());
        assert!(parse_outdated_json(r#"{"formulae": [{"name": "a""#).is_err());
        assert!(parse_outdated_json(r#"{"unrelated": []}"#).is_err());

        // upgrade_names_from_packages isolates Homebrew items and deduplicates.
        let dnf = Package::new_dnf("bash", "x86_64", "5.2", "updates");
        let mut combined = vec![dnf];
        combined.extend(pkgs);
        assert_eq!(
            upgrade_names_from_packages(&combined),
            vec![
                "ripgrep",
                "neovim",
                "oven-sh/bun/bun",
                "font-jetbrains-mono-nerd-font"
            ]
        );
    }

    #[test]
    fn parse_progress_line_recognizes_brew_phases_and_ignores_headers_errors() {
        assert_eq!(
            parse_progress_line("==> Upgrading 2 outdated packages:"),
            ProgressHint::default()
        );
        assert_eq!(
            parse_progress_line("Error: Failed to download resource \"bun\""),
            ProgressHint::default()
        );
        assert_eq!(
            parse_progress_line("Warning: No available formula"),
            ProgressHint::default()
        );

        let fetch = parse_progress_line("==> Fetching ripgrep");
        assert_eq!(fetch.package_name.as_deref(), Some("ripgrep"));
        assert_eq!(fetch.status, Some(PackageStatus::Downloading));

        let dl_ghcr = parse_progress_line(
            "==> Downloading https://ghcr.io/v2/homebrew/core/ripgrep/manifests/14.1.1",
        );
        assert_eq!(dl_ghcr.package_name.as_deref(), Some("ripgrep"));
        assert_eq!(dl_ghcr.status, Some(PackageStatus::Downloading));
        assert_eq!(
            dl_ghcr.phase_label.as_deref(),
            Some("Downloading packages")
        );

        let dl_ver = parse_progress_line(
            "==> Downloading https://ghcr.io/v2/homebrew/core/python/3.12/manifests/3.12.7",
        );
        assert_eq!(dl_ver.package_name.as_deref(), Some("python@3.12"));
        assert_eq!(dl_ver.status, Some(PackageStatus::Downloading));

        let dl_bottle = parse_progress_line(
            "==> Downloading https://ghcr.io/v2/homebrew/core/jq/blobs/sha256:abc123/jq--1.7.1.x86_64_linux.bottle.tar.gz 45%",
        );
        assert_eq!(dl_bottle.package_name.as_deref(), Some("jq"));
        assert_eq!(dl_bottle.progress, Some(0.45));

        let upg = parse_progress_line("==> Upgrading oven-sh/bun/bun");
        assert_eq!(upg.package_name.as_deref(), Some("oven-sh/bun/bun"));
        assert_eq!(upg.status, Some(PackageStatus::Installing));
        assert_eq!(upg.phase_label.as_deref(), Some("Upgrading"));

        let pour = parse_progress_line("==> Pouring python%403.12--3.12.7.x86_64_linux.bottle.tar.gz");
        assert_eq!(pour.package_name.as_deref(), Some("python@3.12"));
        assert_eq!(pour.status, Some(PackageStatus::Installing));
        assert_eq!(pour.phase_label.as_deref(), Some("Installing"));

        let done = parse_progress_line(
            "🍺  /home/linuxbrew/.linuxbrew/Cellar/ripgrep/14.1.1: 12 files, 5.2MB",
        );
        assert_eq!(done.package_name.as_deref(), Some("ripgrep"));
        assert_eq!(done.status, Some(PackageStatus::Completed));
        assert_eq!(done.progress, Some(1.0));

        let done_cask = parse_progress_line("🍺  font-jetbrains-mono-nerd-font was successfully upgraded!");
        assert_eq!(
            done_cask.package_name.as_deref(),
            Some("font-jetbrains-mono-nerd-font")
        );
        assert_eq!(done_cask.status, Some(PackageStatus::Completed));
    }
}
