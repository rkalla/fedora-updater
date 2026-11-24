//! User-space `mise` runtime/tool discovery, outdated JSON parser, and progress hints.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::model::{Package, PackageStatus, UpdateSource};

use super::{
    find_json_object_start, find_user_binary_with, home_dir_from, strip_ansi_escapes, ProgressHint,
};

/// Return standard user-space directories where `mise` may be installed outside `PATH`.
pub fn extra_search_dirs(home_env: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(home) = home_dir_from(home_env) else {
        return Vec::new();
    };
    vec![
        home.join(".local/bin"),
        home.join(".local/share/mise/bin"),
        home.join(".local/share/mise/shims"),
        home.join(".cargo/bin"),
    ]
}

/// Auto-detect `mise` via `PATH`, `~/.local/bin/mise`, `~/.local/share/mise/{bin,shims}/mise`, and `~/.cargo/bin/mise`.
pub fn find_mise_binary() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    let home_env = std::env::var_os("HOME");
    find_mise_binary_with(path_env.as_deref(), home_env.as_deref())
}

/// Injectable variant of [`find_mise_binary`] for offline tests.
pub fn find_mise_binary_with(
    path_env: Option<&OsStr>,
    home_env: Option<&OsStr>,
) -> Option<PathBuf> {
    let extras = extra_search_dirs(home_env);
    find_user_binary_with("mise", path_env, &extras)
}

#[derive(Debug, Deserialize)]
struct MiseOutdatedSource {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MiseOutdatedEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    requested: Option<String>,
    #[serde(default)]
    current: Option<String>,
    #[serde(default)]
    latest: Option<String>,
    #[serde(default)]
    bump: Option<String>,
    #[serde(default)]
    source: Option<MiseOutdatedSource>,
}

fn find_json_array_start(trimmed: &str) -> Option<&str> {
    if trimmed.starts_with('[') {
        return Some(trimmed);
    }
    let mut offset = 0usize;
    for line in trimmed.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            return Some(trimmed[offset..].trim_start());
        }
        offset += line.len();
    }
    None
}

fn entry_to_package(fallback_key: Option<&str>, entry: MiseOutdatedEntry) -> Option<Package> {
    let name = entry
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(fallback_key.map(str::trim).filter(|s| !s.is_empty()))?
        .to_string();

    let latest = entry
        .latest
        .as_deref()
        .or(entry.bump.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();

    let old_version = entry
        .current
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if old_version.as_deref() == Some(latest.as_str()) {
        return None;
    }

    let requested = entry
        .requested
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let src_path = entry
        .source
        .as_ref()
        .and_then(|s| s.path.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let src_kind = entry
        .source
        .as_ref()
        .and_then(|s| s.kind.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut detail_parts = vec![latest.clone()];
    if let Some(req) = requested {
        detail_parts.push(format!("requested {req}"));
    }
    if let Some(path) = src_path {
        let short = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or(path);
        detail_parts.push(short.to_string());
    } else if let Some(kind) = src_kind {
        detail_parts.push(kind.to_string());
    } else {
        detail_parts.push("mise".to_string());
    }

    let summary = src_path
        .map(|p| format!("Configured in {p}"))
        .or_else(|| src_kind.map(|k| format!("Configured via {k}")));

    Some(Package::new_toolchain(
        UpdateSource::Mise,
        format!("mise:{name}"),
        name,
        old_version,
        latest,
        "mise",
        detail_parts.join(" · "),
        summary,
        None,
    ))
}

/// Parse `mise outdated --json` output into `UpdateSource::Mise` package items.
///
/// Accepts both the standard JSON object map (`{"node": {...}}`) and legacy
/// JSON arrays, returning `Ok(vec![])` for `{}` / `[]` / empty output.
pub fn parse_mise_outdated_json(raw: &str) -> Result<Vec<Package>, String> {
    let cleaned = strip_ansi_escapes(raw);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut packages = Vec::new();
    let mut seen_ids = HashSet::new();

    if let Some(obj_json) = find_json_object_start(trimmed) {
        let map: BTreeMap<String, MiseOutdatedEntry> = serde_json::from_str(obj_json)
            .map_err(|e| format!("mise outdated returned invalid JSON: {e}"))?;
        for (key, entry) in map {
            if let Some(pkg) = entry_to_package(Some(&key), entry) {
                if seen_ids.insert(pkg.id.clone()) {
                    packages.push(pkg);
                }
            }
        }
        return Ok(packages);
    }

    if let Some(arr_json) = find_json_array_start(trimmed) {
        let list: Vec<MiseOutdatedEntry> = serde_json::from_str(arr_json)
            .map_err(|e| format!("mise outdated returned invalid JSON array: {e}"))?;
        for entry in list {
            if let Some(pkg) = entry_to_package(None, entry) {
                if seen_ids.insert(pkg.id.clone()) {
                    packages.push(pkg);
                }
            }
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(packages);
    }

    Err("mise outdated output did not contain valid JSON".to_string())
}

/// Return selected `mise` tool names ready for `mise upgrade <tools...>`.
pub fn upgrade_names_from_packages(
    selected_ids: Option<&HashSet<String>>,
    discovered: &[Package],
) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for pkg in discovered {
        if pkg.source != UpdateSource::Mise {
            continue;
        }
        if selected_ids.is_none_or(|ids| ids.contains(&pkg.id)) && seen.insert(pkg.name.clone()) {
            names.push(pkg.name.clone());
        }
    }
    names
}

fn tool_name_from_spec(token: &str) -> Option<String> {
    let trimmed = token.trim().trim_end_matches(':');
    let tool = trimmed.split_once('@').map(|(t, _)| t).unwrap_or(trimmed);
    let clean = tool.trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

/// Parse `mise upgrade` output lines into structured progress hints.
pub fn parse_progress_line(line: &str) -> ProgressHint {
    let cleaned = strip_ansi_escapes(line);
    let trimmed = cleaned.rsplit('\r').next().unwrap_or(&cleaned).trim();
    if trimmed.is_empty() {
        return ProgressHint::default();
    }

    let lower = trimmed.to_ascii_lowercase();
    let progress = super::extract_percent(trimmed).map(|pct| pct / 100.0);

    // Common format: "mise node@22.15.0  download" / "mise node@22.15.0  install" / "mise node@22.15.0 ✓ installed"
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() >= 2 && tokens[0].eq_ignore_ascii_case("mise") {
        if let Some(spec) = tokens.get(1).filter(|s| s.contains('@')) {
            let pkg = tool_name_from_spec(spec);
            let tail = tokens[2..].join(" ").to_ascii_lowercase();
            if tail.contains("installed") || trimmed.contains('✓') {
                return ProgressHint {
                    package_name: pkg,
                    status: Some(PackageStatus::Completed),
                    progress: Some(1.0),
                    phase_label: Some("Installed tool".to_string()),
                    ..Default::default()
                };
            }
            if tail.contains("download") || tail.contains("fetch") {
                return ProgressHint {
                    package_name: pkg,
                    status: Some(PackageStatus::Downloading),
                    progress,
                    phase_label: Some("Downloading mise tool".to_string()),
                    ..Default::default()
                };
            }
            if tail.contains("install") || tail.contains("build") || tail.contains("extract") {
                return ProgressHint {
                    package_name: pkg,
                    status: Some(PackageStatus::Installing),
                    progress,
                    phase_label: Some("Installing mise tool".to_string()),
                    ..Default::default()
                };
            }
        }
    }

    if lower.starts_with("downloading ") {
        let pkg = tokens.get(1).and_then(|t| tool_name_from_spec(t));
        return ProgressHint {
            package_name: pkg,
            status: Some(PackageStatus::Downloading),
            progress,
            phase_label: Some("Downloading mise tool".to_string()),
            ..Default::default()
        };
    }

    if lower.starts_with("installing ") {
        let pkg = tokens.get(1).and_then(|t| tool_name_from_spec(t));
        return ProgressHint {
            package_name: pkg,
            status: Some(PackageStatus::Installing),
            progress,
            phase_label: Some("Installing mise tool".to_string()),
            ..Default::default()
        };
    }

    ProgressHint::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn find_mise_binary_with_checks_path_local_bin_and_shims() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shims = dir.path().join(".local/share/mise/shims");
        std::fs::create_dir_all(&shims).unwrap();
        let shim_mise = shims.join("mise");
        std::fs::write(&shim_mise, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&shim_mise).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shim_mise, perms).unwrap();

        assert_eq!(
            find_mise_binary_with(Some(OsStr::new("")), Some(dir.path().as_os_str())).as_deref(),
            Some(shim_mise.as_path())
        );

        let local_bin = dir.path().join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let local_mise = local_bin.join("mise");
        std::fs::write(&local_mise, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&local_mise).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&local_mise, perms).unwrap();

        // ~/.local/bin takes precedence over shims when both exist outside PATH.
        assert_eq!(
            find_mise_binary_with(Some(OsStr::new("")), Some(dir.path().as_os_str())).as_deref(),
            Some(local_mise.as_path())
        );
    }

    #[test]
    fn parse_mise_outdated_json_handles_map_null_current_empty_and_errors() {
        let raw = r#"
WARN config file experimental notice
{
  "node": {
    "name": "node",
    "requested": "22",
    "current": "22.14.0",
    "latest": "22.15.0",
    "source": { "type": "mise.toml", "path": "/home/alice/.config/mise/config.toml" }
  },
  "ubi:jdx/usage": {
    "name": "ubi:jdx/usage",
    "requested": "latest",
    "current": null,
    "latest": "2.1.0",
    "source": { "type": "mise.toml", "path": "/home/alice/.config/mise/config.toml" }
  },
  "unchanged": {
    "name": "unchanged",
    "current": "1.0.0",
    "latest": "1.0.0"
  }
}
"#;
        let pkgs = parse_mise_outdated_json(raw).expect("valid mise json");
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].id, "mise:node");
        assert_eq!(pkgs[0].old_version.as_deref(), Some("22.14.0"));
        assert_eq!(pkgs[0].version, "22.15.0");
        assert_eq!(pkgs[0].detail, "22.15.0 · requested 22 · config.toml");
        assert_eq!(
            pkgs[0].summary.as_deref(),
            Some("Configured in /home/alice/.config/mise/config.toml")
        );

        assert_eq!(pkgs[1].id, "mise:ubi:jdx/usage");
        assert_eq!(pkgs[1].old_version, None);
        assert_eq!(pkgs[1].version, "2.1.0");

        assert!(parse_mise_outdated_json("{}").unwrap().is_empty());
        assert!(parse_mise_outdated_json("[]").unwrap().is_empty());
        assert!(parse_mise_outdated_json("").unwrap().is_empty());
        assert!(parse_mise_outdated_json("not json").is_err());
        assert!(parse_mise_outdated_json("{\"bad\":").is_err());
    }

    #[test]
    fn upgrade_names_from_packages_respects_selection_and_order() {
        let pkgs = parse_mise_outdated_json(
            r#"{
              "node": {"name": "node", "current": "22.14.0", "latest": "22.15.0"},
              "ripgrep": {"name": "ripgrep", "current": "14.1.0", "latest": "14.1.1"}
            }"#,
        )
        .unwrap();
        assert_eq!(
            upgrade_names_from_packages(None, &pkgs),
            vec!["node", "ripgrep"]
        );
        let selected: HashSet<String> = ["mise:ripgrep".to_string()].into_iter().collect();
        assert_eq!(
            upgrade_names_from_packages(Some(&selected), &pkgs),
            vec!["ripgrep"]
        );
    }

    #[test]
    fn parse_progress_line_tracks_mise_upgrade_stages() {
        let dl = parse_progress_line("mise node@22.15.0  download 40%");
        assert_eq!(dl.package_name.as_deref(), Some("node"));
        assert_eq!(dl.status, Some(PackageStatus::Downloading));
        assert_eq!(dl.progress, Some(0.4));

        let inst = parse_progress_line("mise ubi:jdx/usage@2.1.0  install");
        assert_eq!(inst.package_name.as_deref(), Some("ubi:jdx/usage"));
        assert_eq!(inst.status, Some(PackageStatus::Installing));

        let done = parse_progress_line("mise node@22.15.0  ✓ installed");
        assert_eq!(done.package_name.as_deref(), Some("node"));
        assert_eq!(done.status, Some(PackageStatus::Completed));
        assert_eq!(done.progress, Some(1.0));
    }
}
