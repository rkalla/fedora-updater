//! Isolated Python CLI tool backends (`uv tool` and `pipx`) — discovery,
//! read-only update checks, selective upgrade helpers, and progress hints.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::path::PathBuf;

use serde::Deserialize;

use crate::model::{Package, PackageStatus, UpdateSource};

use super::{
    find_json_object_start, find_user_binary_with, home_dir_from, strip_ansi_escapes, ProgressHint,
};

/// Return user-local binary directories for `uv`.
pub fn uv_extra_search_dirs(home_env: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(home) = home_dir_from(home_env) else {
        return Vec::new();
    };
    vec![home.join(".local/bin"), home.join(".cargo/bin")]
}

/// Return user-local binary directories for `pipx`.
pub fn pipx_extra_search_dirs(home_env: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(home) = home_dir_from(home_env) else {
        return Vec::new();
    };
    vec![home.join(".local/bin")]
}

/// Auto-detect `uv` via `PATH`, `~/.local/bin/uv`, and `~/.cargo/bin/uv`.
pub fn find_uv_binary() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    let home_env = std::env::var_os("HOME");
    find_uv_binary_with(path_env.as_deref(), home_env.as_deref())
}

/// Injectable variant of [`find_uv_binary`] for offline tests.
pub fn find_uv_binary_with(
    path_env: Option<&OsStr>,
    home_env: Option<&OsStr>,
) -> Option<PathBuf> {
    let extras = uv_extra_search_dirs(home_env);
    find_user_binary_with("uv", path_env, &extras)
}

/// Auto-detect `pipx` via `PATH` and `~/.local/bin/pipx`.
pub fn find_pipx_binary() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    let home_env = std::env::var_os("HOME");
    find_pipx_binary_with(path_env.as_deref(), home_env.as_deref())
}

/// Injectable variant of [`find_pipx_binary`] for offline tests.
pub fn find_pipx_binary_with(
    path_env: Option<&OsStr>,
    home_env: Option<&OsStr>,
) -> Option<PathBuf> {
    let extras = pipx_extra_search_dirs(home_env);
    find_user_binary_with("pipx", path_env, &extras)
}

/// Normalize Python package names according to PEP 503 (`HTTPie_CLI` == `httpie-cli`).
pub fn normalize_python_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_sep = false;
    for ch in raw.trim().chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !prev_sep && !out.is_empty() {
                out.push('-');
            }
            prev_sep = true;
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_sep = false;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// True when CLI output indicates an unsupported flag (used to fall back cleanly
/// across older/newer `uv` and `pipx` releases without emitting false positives).
pub fn flag_is_unsupported(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("unrecognized arguments")
        || lower.contains("unexpected argument")
        || lower.contains("unknown flag")
        || lower.contains("no such option")
        || lower.contains("invalid option")
}

/// Installed isolated `uv` tool returned by `uv tool list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvInstalledTool {
    pub name: String,
    pub current_version: String,
    pub executables: Vec<String>,
}

/// Parse `uv tool list` output into installed tool records.
pub fn parse_uv_tool_list(output: &str) -> Vec<UvInstalledTool> {
    let cleaned = strip_ansi_escapes(output);
    let mut tools: Vec<UvInstalledTool> = Vec::new();

    for raw_line in cleaned.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("no tools installed") {
            continue;
        }
        if let Some(exe) = trimmed.strip_prefix("- ") {
            let exe = exe.trim();
            if !exe.is_empty() {
                if let Some(last) = tools.last_mut() {
                    last.executables.push(exe.to_string());
                }
            }
            continue;
        }
        if raw_line.starts_with(char::is_whitespace) {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let Some(name) = parts.next() else {
            continue;
        };
        let Some(ver_token) = parts.next() else {
            continue;
        };
        let Some(raw_ver) = ver_token.strip_prefix('v') else {
            continue;
        };
        let version = raw_ver.trim().trim_end_matches([',', ';']);
        if version.is_empty() || !version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        tools.push(UvInstalledTool {
            name: name.to_string(),
            current_version: version.to_string(),
            executables: Vec::new(),
        });
    }

    tools
}

fn format_uv_detail(version: &str, executables: &[String]) -> (String, Option<String>) {
    if executables.is_empty() {
        (format!("{version} · uv tool"), None)
    } else {
        let joined = executables.join(", ");
        (
            format!("{version} · uv tool · {joined}"),
            Some(format!("Provides executables: {joined}")),
        )
    }
}

fn strip_v(ver: &str) -> &str {
    ver.trim().strip_prefix('v').unwrap_or(ver.trim())
}

fn build_uv_package(
    name: &str,
    old_version: Option<String>,
    new_version: &str,
    installed_map: &HashMap<String, &UvInstalledTool>,
) -> Option<Package> {
    let norm = normalize_python_name(name);
    if !installed_map.is_empty() && !installed_map.contains_key(&norm) {
        return None;
    }
    let meta = installed_map.get(&norm).copied();
    let display_name = meta.map(|t| t.name.as_str()).unwrap_or(name);
    let old = old_version
        .map(|v| strip_v(&v).to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| meta.map(|t| t.current_version.clone()));
    let new_clean = strip_v(new_version);
    if new_clean.is_empty() || old.as_deref() == Some(new_clean) {
        return None;
    }
    let executables = meta.map(|t| t.executables.as_slice()).unwrap_or(&[]);
    let (detail, summary) = format_uv_detail(new_clean, executables);
    Some(Package::new_toolchain(
        UpdateSource::UvTools,
        format!("uv:{display_name}"),
        display_name,
        old,
        new_clean,
        "uv",
        detail,
        summary,
        None,
    ))
}

/// Parse `uv tool upgrade --all --dry-run` output into pending `UpdateSource::UvTools` items.
///
/// Supports both `Updated <tool> v<old> -> v<new>` summary lines and `- pkg==old` /
/// `+ pkg==new` diff lines. When `installed` is non-empty, only top-level tools
/// from `installed` are returned so internal venv dependency bumps do not pollute the queue.
pub fn parse_uv_upgrade_dry_run(output: &str, installed: &[UvInstalledTool]) -> Vec<Package> {
    let cleaned = strip_ansi_escapes(output);
    let installed_map: HashMap<String, &UvInstalledTool> = installed
        .iter()
        .map(|t| (normalize_python_name(&t.name), t))
        .collect();

    let mut packages = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut removed: HashMap<String, String> = HashMap::new();
    let mut added: Vec<(String, String)> = Vec::new();

    for raw_line in cleaned.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("nothing to upgrade") || lower.contains("no changes made") {
            continue;
        }

        // Diff lines: "- httpie==3.2.2" and "+ httpie==3.2.4"
        if let Some(rest) = line.strip_prefix("- ") {
            if let Some((pkg, ver)) = rest.split_once("==") {
                let pkg = pkg.trim();
                let ver = strip_v(ver);
                if !pkg.is_empty() && !ver.is_empty() {
                    removed.insert(normalize_python_name(pkg), ver.to_string());
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("+ ") {
            if let Some((pkg, ver)) = rest.split_once("==") {
                let pkg = pkg.trim();
                let ver = strip_v(ver);
                if !pkg.is_empty() && !ver.is_empty() {
                    added.push((pkg.to_string(), ver.to_string()));
                }
            }
            continue;
        }

        // Transition lines: "Updated httpie v3.2.2 -> v3.2.4" or "httpie v3.2.2 -> v3.2.4"
        if let Some((lhs, rhs)) = line.split_once(" -> ") {
            let lhs_body = lhs
                .trim()
                .strip_prefix("Updated ")
                .or_else(|| lhs.trim().strip_prefix("Upgraded "))
                .unwrap_or(lhs.trim());
            let mut tokens = lhs_body.split_whitespace();
            let Some(name) = tokens.next() else {
                continue;
            };
            let old_ver = tokens.next().map(str::to_string);
            let Some(new_ver) = rhs.split_whitespace().next() else {
                continue;
            };
            if let Some(pkg) = build_uv_package(name, old_ver, new_ver, &installed_map) {
                if seen_ids.insert(pkg.id.clone()) {
                    packages.push(pkg);
                }
            }
        }
    }

    for (name, new_ver) in added {
        let old_ver = removed.get(&normalize_python_name(&name)).cloned();
        if let Some(pkg) = build_uv_package(&name, old_ver, &new_ver, &installed_map) {
            if seen_ids.insert(pkg.id.clone()) {
                packages.push(pkg);
            }
        }
    }

    packages
}

/// Parse `uv tool list --outdated` output into `UpdateSource::UvTools` items.
pub fn parse_uv_tool_list_outdated(output: &str, installed: &[UvInstalledTool]) -> Vec<Package> {
    let cleaned = strip_ansi_escapes(output);
    let installed_map: HashMap<String, &UvInstalledTool> = installed
        .iter()
        .map(|t| (normalize_python_name(&t.name), t))
        .collect();
    let mut packages = parse_uv_upgrade_dry_run(&cleaned, installed);
    let mut seen_ids: HashSet<String> = packages.iter().map(|p| p.id.clone()).collect();

    for raw_line in cleaned.lines() {
        let line = raw_line.trim();
        if line.is_empty() || raw_line.starts_with(char::is_whitespace) {
            continue;
        }
        // e.g. "httpie 3.2.2 (latest: 3.2.4)" or "httpie v3.2.2 [latest: v3.2.4]"
        let lower = line.to_ascii_lowercase();
        let Some(marker_pos) = lower.find("latest:") else {
            continue;
        };
        let after = line[marker_pos + "latest:".len()..]
            .trim()
            .trim_end_matches([')', ']']);
        let Some(new_ver) = after.split_whitespace().next() else {
            continue;
        };
        let mut head = line[..marker_pos].split_whitespace();
        let Some(name) = head.next() else {
            continue;
        };
        let old_ver = head
            .next()
            .map(|v| v.trim_start_matches(['(', '[']).to_string());
        if let Some(pkg) = build_uv_package(name, old_ver, new_ver, &installed_map) {
            if seen_ids.insert(pkg.id.clone()) {
                packages.push(pkg);
            }
        }
    }

    packages
}

#[derive(Debug, Deserialize)]
struct UvPipOutdatedEntry {
    name: String,
    version: String,
    latest_version: String,
}

/// Parse `uv pip list --outdated --format json --python <venv>/bin/python` for one installed tool.
pub fn parse_uv_pip_outdated_json(
    raw: &str,
    tool: &UvInstalledTool,
) -> Result<Option<Package>, String> {
    let cleaned = strip_ansi_escapes(raw);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Some(start) = trimmed.find('[') else {
        return Err("uv pip list --outdated did not contain a JSON array".to_string());
    };
    let entries: Vec<UvPipOutdatedEntry> = serde_json::from_str(&trimmed[start..])
        .map_err(|e| format!("uv pip list --outdated returned invalid JSON: {e}"))?;
    let target_norm = normalize_python_name(&tool.name);
    for entry in entries {
        if normalize_python_name(&entry.name) != target_norm {
            continue;
        }
        let current = strip_v(&entry.version);
        let latest = strip_v(&entry.latest_version);
        if latest.is_empty() || latest == current {
            return Ok(None);
        }
        let old = if current.is_empty() {
            tool.current_version.clone()
        } else {
            current.to_string()
        };
        let (detail, summary) = format_uv_detail(latest, &tool.executables);
        return Ok(Some(Package::new_toolchain(
            UpdateSource::UvTools,
            format!("uv:{}", tool.name),
            &tool.name,
            Some(old),
            latest,
            "uv",
            detail,
            summary,
            None,
        )));
    }
    Ok(None)
}

/// Return selected `uv` tool names ready for `uv tool upgrade <tool>`.
pub fn uv_tool_names_from_packages(
    selected_ids: Option<&HashSet<String>>,
    discovered: &[Package],
) -> Vec<String> {
    selected_package_names(UpdateSource::UvTools, selected_ids, discovered)
}

/// Parse `uv tool upgrade` output lines into structured progress hints.
pub fn parse_uv_progress_line(line: &str) -> ProgressHint {
    let cleaned = strip_ansi_escapes(line);
    let trimmed = cleaned.rsplit('\r').next().unwrap_or(&cleaned).trim();
    if trimmed.is_empty() {
        return ProgressHint::default();
    }
    let progress = super::extract_percent(trimmed).map(|pct| pct / 100.0);
    let lower = trimmed.to_ascii_lowercase();

    if let Some(rest) = trimmed.strip_prefix("Updated ") {
        let name = rest.split_whitespace().next().map(str::to_string);
        return ProgressHint {
            package_name: name,
            status: Some(PackageStatus::Completed),
            progress: Some(1.0),
            phase_label: Some("Updated tool".to_string()),
            ..Default::default()
        };
    }

    if lower.starts_with("downloading ") {
        return ProgressHint {
            status: Some(PackageStatus::Downloading),
            progress,
            phase_label: Some("Downloading uv packages".to_string()),
            ..Default::default()
        };
    }

    if lower.starts_with("installed ") || lower.starts_with("installing ") {
        return ProgressHint {
            status: Some(PackageStatus::Installing),
            progress,
            phase_label: Some("Installing uv tool".to_string()),
            ..Default::default()
        };
    }

    if lower.starts_with("updating ") || lower.starts_with("upgrading ") {
        let name = trimmed.split_whitespace().nth(1).map(str::to_string);
        return ProgressHint {
            package_name: name,
            status: Some(PackageStatus::Installing),
            progress,
            phase_label: Some("Upgrading uv tool".to_string()),
            ..Default::default()
        };
    }

    ProgressHint::default()
}

/// Installed isolated `pipx` package parsed from `pipx list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipxInstalledPackage {
    pub name: String,
    pub version: String,
    pub apps: Vec<String>,
    pub pinned: bool,
}

#[derive(Debug, Deserialize)]
struct PipxListRoot {
    #[serde(default)]
    venvs: BTreeMap<String, PipxVenvEntry>,
}

#[derive(Debug, Deserialize)]
struct PipxVenvEntry {
    #[serde(default)]
    metadata: Option<PipxVenvMetadata>,
}

#[derive(Debug, Deserialize)]
struct PipxVenvMetadata {
    #[serde(default)]
    main_package: Option<PipxMainPackage>,
}

#[derive(Debug, Deserialize)]
struct PipxMainPackage {
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    package_version: Option<String>,
    #[serde(default)]
    latest_version: Option<String>,
    #[serde(default)]
    apps: Vec<String>,
    #[serde(default)]
    pinned: bool,
}

/// Parse `pipx list --json` output into installed venv records.
pub fn parse_pipx_list_json(raw: &str) -> Result<Vec<PipxInstalledPackage>, String> {
    let cleaned = strip_ansi_escapes(raw);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let Some(json_str) = find_json_object_start(trimmed) else {
        return Err("pipx list --json output did not contain a JSON object".to_string());
    };
    let root: PipxListRoot = serde_json::from_str(json_str)
        .map_err(|e| format!("pipx list --json returned invalid JSON: {e}"))?;

    let mut installed = Vec::new();
    for (venv_name, entry) in root.venvs {
        let Some(main) = entry.metadata.and_then(|m| m.main_package) else {
            continue;
        };
        let name = main
            .package
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&venv_name)
            .to_string();
        let version = main
            .package_version
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if name.is_empty() || version.is_empty() {
            continue;
        }
        installed.push(PipxInstalledPackage {
            name,
            version,
            apps: main.apps,
            pinned: main.pinned,
        });
    }

    Ok(installed)
}

fn format_pipx_detail(version: &str, apps: &[String]) -> (String, Option<String>) {
    if apps.is_empty() {
        (format!("{version} · pipx"), None)
    } else {
        let joined = apps.join(", ");
        (
            format!("{version} · pipx · {joined}"),
            Some(format!("Provides apps: {joined}")),
        )
    }
}

fn build_pipx_package(
    name: &str,
    old_version: Option<String>,
    new_version: &str,
    installed_map: &HashMap<String, &PipxInstalledPackage>,
) -> Option<Package> {
    let norm = normalize_python_name(name);
    let meta = installed_map.get(&norm).copied();
    if meta.is_some_and(|p| p.pinned) {
        return None;
    }
    let display_name = meta.map(|p| p.name.as_str()).unwrap_or(name);
    let old = old_version
        .map(|v| strip_v(&v).to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| meta.map(|p| p.version.clone()));
    let new_clean = strip_v(new_version);
    if new_clean.is_empty() || old.as_deref() == Some(new_clean) {
        return None;
    }
    let apps = meta.map(|p| p.apps.as_slice()).unwrap_or(&[]);
    let (detail, summary) = format_pipx_detail(new_clean, apps);
    Some(Package::new_toolchain(
        UpdateSource::Pipx,
        format!("pipx:{display_name}"),
        display_name,
        old,
        new_clean,
        "pipx",
        detail,
        summary,
        None,
    ))
}

/// Parse `pipx list --outdated --json` output if `latest_version` fields are present.
pub fn parse_pipx_outdated_json(
    raw: &str,
    installed: &[PipxInstalledPackage],
) -> Result<Vec<Package>, String> {
    let cleaned = strip_ansi_escapes(raw);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let Some(json_str) = find_json_object_start(trimmed) else {
        return Err("pipx list --outdated --json did not contain a JSON object".to_string());
    };
    let root: PipxListRoot = serde_json::from_str(json_str)
        .map_err(|e| format!("pipx outdated JSON invalid: {e}"))?;
    let installed_map: HashMap<String, &PipxInstalledPackage> = installed
        .iter()
        .map(|p| (normalize_python_name(&p.name), p))
        .collect();

    let mut packages = Vec::new();
    for (venv_name, entry) in root.venvs {
        let Some(main) = entry.metadata.and_then(|m| m.main_package) else {
            continue;
        };
        if main.pinned {
            continue;
        }
        let Some(latest) = main.latest_version.as_deref() else {
            continue;
        };
        let name = main
            .package
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&venv_name);
        if let Some(pkg) =
            build_pipx_package(name, main.package_version, latest, &installed_map)
        {
            packages.push(pkg);
        }
    }
    Ok(packages)
}

/// Parse `pipx list --outdated` text output cross-referenced with `pipx list --json`.
///
/// Handles:
/// - `package <name> has version <old>, newest version is <new>`
/// - `package <name> <old>, latest version available: <new>`
/// - `<name> <old> -> <new>` / `<name> <old> (latest: <new>)`
///
/// Never produces false positives when output says nothing is outdated or only lists
/// informational venv paths, and always skips `pinned` packages from `installed`.
pub fn parse_pipx_outdated_text(output: &str, installed: &[PipxInstalledPackage]) -> Vec<Package> {
    let cleaned = strip_ansi_escapes(output);
    let installed_map: HashMap<String, &PipxInstalledPackage> = installed
        .iter()
        .map(|p| (normalize_python_name(&p.name), p))
        .collect();

    let mut packages = Vec::new();
    let mut seen_ids = HashSet::new();

    for raw_line in cleaned.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed = parse_pipx_outdated_line(line);
        let Some((name, old_ver, new_ver)) = parsed else {
            continue;
        };
        if let Some(pkg) = build_pipx_package(&name, Some(old_ver), &new_ver, &installed_map) {
            if seen_ids.insert(pkg.id.clone()) {
                packages.push(pkg);
            }
        }
    }

    packages
}

fn parse_pipx_outdated_line(line: &str) -> Option<(String, String, String)> {
    let lower = line.to_ascii_lowercase();

    // Form 1: "package black has version 24.10.0, newest version is 25.1.0"
    if let Some(after_pkg) = lower
        .strip_prefix("package ")
        .map(|_| line["package ".len()..].trim())
    {
        if let Some(has_idx) = after_pkg.to_ascii_lowercase().find(" has version ") {
            let name = after_pkg[..has_idx].trim();
            let rest = &after_pkg[has_idx + " has version ".len()..];
            let (old_part, after_old) = rest.split_once(',')?;
            let old_ver = strip_v(old_part).trim();
            let after_old_lower = after_old.to_ascii_lowercase();
            let marker = ["newest version is ", "latest version is ", "latest: "]
                .into_iter()
                .find_map(|m| after_old_lower.find(m).map(|idx| &after_old[idx + m.len()..]))?;
            let new_ver = marker.split_whitespace().next().map(strip_v)?;
            return Some((name.to_string(), old_ver.to_string(), new_ver.to_string()));
        }
        // Form 2: "package ansible-lint 24.9.0, latest version available: 24.10.0"
        if let Some((left, right)) = after_pkg.split_once(',') {
            let mut left_parts = left.split_whitespace();
            let name = left_parts.next()?;
            let old_ver = left_parts.next().map(strip_v)?;
            let right_lower = right.to_ascii_lowercase();
            let marker_pos = right_lower.find(':')?;
            let new_ver = right[marker_pos + 1..].split_whitespace().next().map(strip_v)?;
            return Some((name.to_string(), old_ver.to_string(), new_ver.to_string()));
        }
    }

    // Form 3: "black 24.10.0 -> 25.1.0" or "Upgrade available: black 24.10.0 -> 25.1.0"
    if let Some((lhs, rhs)) = line.split_once(" -> ") {
        let lhs_clean = lhs
            .trim()
            .strip_prefix("Upgrade available:")
            .or_else(|| lhs.trim().strip_prefix("upgrade available:"))
            .unwrap_or(lhs.trim())
            .trim();
        let mut parts = lhs_clean.split_whitespace();
        let name = parts.next()?;
        let old_ver = parts.next().map(strip_v)?;
        let new_ver = rhs.split_whitespace().next().map(strip_v)?;
        if parts.next().is_none()
            && old_ver.chars().next().is_some_and(|c| c.is_ascii_digit())
            && new_ver.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            return Some((name.to_string(), old_ver.to_string(), new_ver.to_string()));
        }
    }

    // Form 4: "black 24.10.0 (latest: 25.1.0)"
    if let Some(pos) = lower.find("(latest:") {
        let mut head = line[..pos].split_whitespace();
        let name = head.next()?;
        let old_ver = head.next().map(strip_v)?;
        let new_ver = line[pos + "(latest:".len()..]
            .trim()
            .trim_end_matches(')')
            .split_whitespace()
            .next()
            .map(strip_v)?;
        return Some((name.to_string(), old_ver.to_string(), new_ver.to_string()));
    }

    None
}

/// Return selected `pipx` package names ready for `pipx upgrade <pkg>`.
pub fn pipx_package_names_from_packages(
    selected_ids: Option<&HashSet<String>>,
    discovered: &[Package],
) -> Vec<String> {
    selected_package_names(UpdateSource::Pipx, selected_ids, discovered)
}

/// Parse `pipx upgrade` output lines into structured progress hints.
pub fn parse_pipx_progress_line(line: &str) -> ProgressHint {
    let cleaned = strip_ansi_escapes(line);
    let trimmed = cleaned.rsplit('\r').next().unwrap_or(&cleaned).trim();
    if trimmed.is_empty() {
        return ProgressHint::default();
    }
    let lower = trimmed.to_ascii_lowercase();
    let progress = super::extract_percent(trimmed).map(|pct| pct / 100.0);

    if lower.starts_with("upgrading ") || lower.starts_with("updating ") {
        let name = trimmed
            .split_whitespace()
            .nth(1)
            .map(|s| s.trim_end_matches("...").to_string());
        return ProgressHint {
            package_name: name,
            status: Some(PackageStatus::Installing),
            progress,
            phase_label: Some("Upgrading pipx package".to_string()),
            ..Default::default()
        };
    }

    if lower.starts_with("upgraded package ") {
        let name = trimmed.split_whitespace().nth(2).map(str::to_string);
        return ProgressHint {
            package_name: name,
            status: Some(PackageStatus::Completed),
            progress: Some(1.0),
            phase_label: Some("Upgraded pipx package".to_string()),
            ..Default::default()
        };
    }

    ProgressHint::default()
}

fn selected_package_names(
    source: UpdateSource,
    selected_ids: Option<&HashSet<String>>,
    discovered: &[Package],
) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for pkg in discovered {
        if pkg.source != source {
            continue;
        }
        if selected_ids.is_none_or(|ids| ids.contains(&pkg.id)) && seen.insert(pkg.name.clone()) {
            names.push(pkg.name.clone());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn find_uv_and_pipx_binaries_check_path_and_local_bin() {
        let dir = tempfile::tempdir().unwrap();
        let local_bin = dir.path().join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();

        let uv = local_bin.join("uv");
        std::fs::write(&uv, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&uv).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&uv, perms).unwrap();

        assert_eq!(
            find_uv_binary_with(Some(OsStr::new("")), Some(dir.path().as_os_str())).as_deref(),
            Some(uv.as_path())
        );
        assert_eq!(
            find_pipx_binary_with(Some(OsStr::new("")), Some(dir.path().as_os_str())),
            None
        );

        let pipx = local_bin.join("pipx");
        std::fs::write(&pipx, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&pipx).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&pipx, perms).unwrap();
        assert_eq!(
            find_pipx_binary_with(Some(OsStr::new("")), Some(dir.path().as_os_str())).as_deref(),
            Some(pipx.as_path())
        );
    }

    #[test]
    fn uv_tool_list_and_dry_run_parsers_filter_transitive_deps_and_avoid_false_positives() {
        let list_out = "\
httpie v3.2.2
- http
- https
- httpie
ruff v0.9.6 [required: >=0.9.0]
- ruff
";
        let installed = parse_uv_tool_list(list_out);
        assert_eq!(installed.len(), 2);
        assert_eq!(installed[0].name, "httpie");
        assert_eq!(installed[0].current_version, "3.2.2");
        assert_eq!(installed[0].executables, vec!["http", "https", "httpie"]);

        let dry_run = "\
Resolved 12 packages in 140ms
 - httpie==3.2.2
 + httpie==3.2.4
 - urllib3==2.2.0
 + urllib3==2.3.0
Updated ruff v0.9.6 -> v0.9.8
";
        let pkgs = parse_uv_upgrade_dry_run(dry_run, &installed);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].id, "uv:ruff");
        assert_eq!(pkgs[0].old_version.as_deref(), Some("0.9.6"));
        assert_eq!(pkgs[0].version, "0.9.8");
        assert_eq!(pkgs[1].id, "uv:httpie");
        assert_eq!(pkgs[1].old_version.as_deref(), Some("3.2.2"));
        assert_eq!(pkgs[1].version, "3.2.4");
        assert!(pkgs[1].summary.as_deref().unwrap().contains("http"));

        assert!(parse_uv_upgrade_dry_run("Nothing to upgrade\n", &installed).is_empty());
        assert!(parse_uv_tool_list("No tools installed\n").is_empty());
    }

    #[test]
    fn uv_pip_outdated_json_fallback_matches_pep503_name() {
        let tool = UvInstalledTool {
            name: "Httpie_Cli".into(),
            current_version: "3.2.2".into(),
            executables: vec!["http".into()],
        };
        let json = r#"[
            {"name": "urllib3", "version": "2.2.0", "latest_version": "2.3.0"},
            {"name": "httpie-cli", "version": "3.2.2", "latest_version": "3.2.4"}
        ]"#;
        let pkg = parse_uv_pip_outdated_json(json, &tool)
            .unwrap()
            .expect("matching tool update");
        assert_eq!(pkg.id, "uv:Httpie_Cli");
        assert_eq!(pkg.version, "3.2.4");

        let up_to_date = parse_uv_pip_outdated_json("[]", &tool).unwrap();
        assert!(up_to_date.is_none());
    }

    #[test]
    fn pipx_list_json_and_outdated_text_skip_pinned_and_avoid_false_positives() {
        let list_json = r#"{
  "pipx_spec_version": "0.1",
  "venvs": {
    "ansible-lint": {
      "metadata": {
        "main_package": {
          "package": "ansible-lint",
          "package_version": "24.9.0",
          "pinned": false,
          "apps": ["ansible-lint"]
        }
      }
    },
    "black": {
      "metadata": {
        "main_package": {
          "package": "black",
          "package_version": "24.10.0",
          "pinned": true,
          "apps": ["black"]
        }
      }
    }
  }
}"#;
        let installed = parse_pipx_list_json(list_json).expect("valid pipx json");
        assert_eq!(installed.len(), 2);

        let outdated_text = "\
venvs are in /home/alice/.local/share/pipx/venvs
package ansible-lint has version 24.9.0, newest version is 24.10.0
package black has version 24.10.0, newest version is 25.1.0
";
        let pkgs = parse_pipx_outdated_text(outdated_text, &installed);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].id, "pipx:ansible-lint");
        assert_eq!(pkgs[0].old_version.as_deref(), Some("24.9.0"));
        assert_eq!(pkgs[0].version, "24.10.0");
        assert_eq!(pkgs[0].summary.as_deref(), Some("Provides apps: ansible-lint"));

        // Non-outdated informational output never triggers false positives.
        assert!(parse_pipx_outdated_text("venvs are in /tmp\nnothing to upgrade\n", &installed).is_empty());
        assert!(flag_is_unsupported("pipx list: error: unrecognized arguments: --outdated"));
        assert!(flag_is_unsupported("error: unexpected argument '--dry-run' found"));
    }

    #[test]
    fn parse_progress_lines_track_uv_and_pipx_stages() {
        let uv_done = parse_uv_progress_line("Updated httpie v3.2.2 -> v3.2.4");
        assert_eq!(uv_done.package_name.as_deref(), Some("httpie"));
        assert_eq!(uv_done.status, Some(PackageStatus::Completed));

        let pipx_up = parse_pipx_progress_line("upgrading ansible-lint...");
        assert_eq!(pipx_up.package_name.as_deref(), Some("ansible-lint"));
        assert_eq!(pipx_up.status, Some(PackageStatus::Installing));
    }
}
