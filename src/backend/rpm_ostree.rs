//! Atomic Fedora (`rpm-ostree`) host detection, status/diff parsing, and progress hints.
//!
//! Zero-password discovery queries `rpm-ostree status --json` and `rpm-ostree upgrade --check`
//! unprivileged. Deployment staging executes `rpm-ostree upgrade`, which negotiates directly
//! with `rpm-ostreed`'s Polkit rules without modifying `--helper`.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use serde_json::Value as JsonValue;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::Connection;

use crate::model::{AdvisoryKind, Package, PackageAdvisory, PackageStatus};

use super::dnf::{is_plausible_arch, strip_to_package_name};
use super::{extract_percent, NativeOutcome, ProgressHint};

/// Boot marker file created on OSTree-booted systems (Silverblue, Kinoite, Sway Atomic, uBlue).
pub const OSTREE_BOOTED_PATH: &str = "/run/ostree-booted";

/// Well-known system D-Bus name for `rpm-ostreed`.
pub const RPM_OSTREE_DEST: &str = "org.projectatomic.rpmostree1";

/// Default binary location checked even when desktop `PATH` is minimal.
pub const RPM_OSTREE_DEFAULT_PATH: &str = "/usr/bin/rpm-ostree";

/// Non-interactive environment passed to `rpm-ostree` subprocesses.
pub const NONINTERACTIVE_ENV: &[(&str, &str)] = &[("NO_COLOR", "1")];

static APPLY_ACTIVE: AtomicBool = AtomicBool::new(false);

/// RAII guard marking whether an `rpm-ostree upgrade` transaction is currently running.
pub struct ApplyActiveGuard;

impl ApplyActiveGuard {
    pub fn enter() -> Self {
        APPLY_ACTIVE.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for ApplyActiveGuard {
    fn drop(&mut self) {
        APPLY_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// True when an `rpm-ostree upgrade` process is currently active.
pub fn is_apply_active() -> bool {
    APPLY_ACTIVE.load(Ordering::SeqCst)
}

/// Request cancellation of an active `rpm-ostree` daemon transaction without blocking the UI thread.
pub fn cancel_active_transaction() -> bool {
    if !APPLY_ACTIVE.swap(false, Ordering::SeqCst) {
        return false;
    }
    let Some(bin) = find_rpm_ostree_binary() else {
        return false;
    };
    std::thread::spawn(move || {
        let _ = Command::new(bin)
            .arg("cancel")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
    true
}

/// Detect whether the current host booted via OSTree (`/run/ostree-booted`).
pub fn is_ostree_booted() -> bool {
    is_ostree_booted_at(Path::new(OSTREE_BOOTED_PATH))
}

/// Injectable variant of [`is_ostree_booted`] for offline tests.
pub fn is_ostree_booted_at(path: &Path) -> bool {
    path.exists()
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_file() && (meta.permissions().mode() & 0o111) != 0
}

/// Locate the `rpm-ostree` executable on `PATH` or `/usr/bin/rpm-ostree`.
pub fn find_rpm_ostree_binary() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    find_rpm_ostree_binary_with(path_env.as_deref(), Path::new(RPM_OSTREE_DEFAULT_PATH))
}

/// Injectable variant of [`find_rpm_ostree_binary`] for offline tests.
pub fn find_rpm_ostree_binary_with(
    path_env: Option<&OsStr>,
    system_fallback: &Path,
) -> Option<PathBuf> {
    if let Some(paths) = path_env.filter(|p| !p.is_empty()) {
        for dir in std::env::split_paths(paths) {
            let candidate = dir.join("rpm-ostree");
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    if is_executable_file(system_fallback) {
        return Some(system_fallback.to_path_buf());
    }
    None
}

/// True when `org.projectatomic.rpmostree1` is running or activatable on the system D-Bus.
pub fn is_dbus_available() -> bool {
    let Ok(conn) = Connection::system() else {
        return false;
    };
    let Ok(dbus) = DBusProxy::new(&conn) else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(RPM_OSTREE_DEST) else {
        return false;
    };
    if dbus.name_has_owner(name).unwrap_or(false) {
        return true;
    }
    dbus.list_activatable_names()
        .map(|names| names.iter().any(|n| n.as_str() == RPM_OSTREE_DEST))
        .unwrap_or(false)
}

/// Parsed deployment entry from `rpm-ostree status --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OstreeDeployment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub osname: String,
    #[serde(default)]
    pub checksum: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub booted: bool,
    #[serde(default)]
    pub staged: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub container_image_reference: Option<String>,
    #[serde(default)]
    pub base_checksum: Option<String>,
    #[serde(default)]
    pub requested_packages: Vec<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub requested_local_packages: Vec<String>,
    #[serde(default)]
    pub requested_base_removals: Vec<String>,
}

impl OstreeDeployment {
    /// Human-readable origin or container image label for UI display.
    pub fn repo_label(&self) -> String {
        if let Some(container) = self
            .container_image_reference
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return clean_container_ref(container);
        }
        if let Some(origin) = self
            .origin
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Some((_, rest)) = origin.split_once(':') {
                if !rest.trim().is_empty() {
                    return rest.trim().to_string();
                }
            }
            return origin.to_string();
        }
        if !self.osname.trim().is_empty() {
            return format!("{}-ostree", self.osname.trim());
        }
        "fedora-ostree".to_string()
    }

    /// Friendly OS image title derived from the deployment origin/container reference.
    pub fn image_display_name(&self) -> String {
        let label = self.repo_label();
        let last_segment = label
            .rsplit('/')
            .next()
            .unwrap_or(&label)
            .split(':')
            .next()
            .unwrap_or(&label)
            .trim();
        if last_segment.is_empty() || last_segment == "fedora-ostree" {
            "Fedora Atomic System Image".to_string()
        } else {
            let mut chars = last_segment.chars();
            let titled = match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => last_segment.to_string(),
            };
            format!("{titled} System Image")
        }
    }

    /// True when `pkg_name` is a layered or local overlay package on this deployment.
    pub fn is_layered_package(&self, pkg_name: &str) -> bool {
        let needle = pkg_name.trim();
        if needle.is_empty() {
            return false;
        }
        self.requested_packages
            .iter()
            .chain(self.packages.iter())
            .chain(self.requested_local_packages.iter())
            .any(|entry| {
                let trimmed = entry.trim();
                trimmed == needle || strip_to_package_name(trimmed) == needle
            })
    }
}

fn clean_container_ref(raw: &str) -> String {
    let mut s = raw.trim();
    for prefix in [
        "ostree-image-signed:docker://",
        "ostree-unverified-image:docker://",
        "ostree-unverified-registry:",
        "ostree-remote-registry:",
        "docker://",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
            break;
        }
    }
    if s.is_empty() {
        "ostree-image".to_string()
    } else {
        s.to_string()
    }
}

/// Optional cached RPM diff inside `rpm-ostree status --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct OstreeRpmDiff {
    #[serde(default)]
    pub upgraded: Vec<JsonValue>,
    #[serde(default)]
    pub downgraded: Vec<JsonValue>,
    #[serde(default)]
    pub added: Vec<JsonValue>,
    #[serde(default)]
    pub removed: Vec<JsonValue>,
}

/// Optional `cached-update` section from `rpm-ostree status --json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OstreeCachedUpdate {
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub checksum: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub rpm_diff: Option<OstreeRpmDiff>,
}

/// Top-level `rpm-ostree status --json` payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OstreeStatus {
    #[serde(default)]
    pub deployments: Vec<OstreeDeployment>,
    #[serde(default)]
    pub cached_update: Option<OstreeCachedUpdate>,
}

impl OstreeStatus {
    pub fn booted_deployment(&self) -> Option<&OstreeDeployment> {
        self.deployments.iter().find(|d| d.booted)
    }

    pub fn staged_deployment(&self) -> Option<&OstreeDeployment> {
        self.deployments.iter().find(|d| d.staged)
    }
}

/// Parse `rpm-ostree status --json` output into structured deployment metadata.
pub fn parse_status_json(raw: &str) -> Result<OstreeStatus, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty rpm-ostree status JSON".into());
    }
    serde_json::from_str::<OstreeStatus>(trimmed)
        .map_err(|e| format!("invalid rpm-ostree status JSON: {e}"))
}

/// Query unprivileged `rpm-ostree status --json` on the host.
pub fn query_status() -> NativeOutcome<OstreeStatus> {
    let Some(bin) = find_rpm_ostree_binary() else {
        return NativeOutcome::Unavailable("rpm-ostree executable not found".into());
    };
    let output = match Command::new(&bin).args(["status", "--json"]).output() {
        Ok(out) => out,
        Err(e) => return NativeOutcome::Unavailable(format!("rpm-ostree status failed: {e}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("not booted")
            || lower.contains("not available")
            || lower.contains("org.freedesktop.dbus")
        {
            return NativeOutcome::Unavailable(stderr.trim().to_string());
        }
        return NativeOutcome::Handled(Err(format!(
            "rpm-ostree status failed: {}",
            stderr.trim()
        )));
    }
    NativeOutcome::Handled(parse_status_json(&stdout))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffSection {
    Upgraded,
    Downgraded,
    Added,
    Removed,
    SecAdvisories,
    Other,
}

#[derive(Debug, Default)]
struct ParsedUpdateMeta {
    saw_available_update: bool,
    version: Option<String>,
    timestamp: Option<String>,
    commit_or_digest: Option<String>,
    diff_summary: Option<String>,
}

/// True when `rpm-ostree upgrade --check` reported no available updates (exit 77 or explicit text).
pub fn check_exit_is_no_updates(code: i32, log: &str) -> bool {
    if code == 77 {
        return true;
    }
    if code != 0 {
        return false;
    }
    let lower = log.to_ascii_lowercase();
    lower.contains("no upgrade available")
        || lower.contains("no updates available")
        || !log.lines().any(|l| {
            let t = l.trim();
            t.starts_with("AvailableUpdate:")
                || t.starts_with("Upgraded:")
                || t.starts_with("Added:")
                || t.starts_with("Removed:")
                || t.starts_with("Downgraded:")
        })
}

/// True when `rpm-ostree upgrade --check` failed rather than finding 0 or N updates.
pub fn check_exit_is_hard_error(code: i32, log: &str, packages: &[Package]) -> bool {
    if !packages.is_empty() || check_exit_is_no_updates(code, log) {
        return false;
    }
    code != 0
        || log
            .lines()
            .any(|l| l.trim().to_ascii_lowercase().starts_with("error:"))
}

fn short_commit(commit: &str) -> String {
    let trimmed = commit.trim();
    if let Some(hex) = trimmed.strip_prefix("sha256:") {
        let short: String = hex.chars().take(12).collect();
        return format!("sha256:{short}");
    }
    trimmed.chars().take(12).collect()
}

fn parse_version_and_parens_timestamp(raw: &str) -> (String, Option<String>) {
    let trimmed = raw.trim();
    if let Some((ver, rest)) = trimmed.split_once('(') {
        let ts = rest.trim_end_matches(')').trim();
        let ver = ver.trim().to_string();
        let ts = (!ts.is_empty()).then(|| ts.to_string());
        (ver, ts)
    } else {
        (trimmed.to_string(), None)
    }
}

fn strip_trailing_arch(token: &str) -> (String, String) {
    let trimmed = token.trim();
    if let Some(idx) = trimmed.rfind('.') {
        let arch = &trimmed[idx + 1..];
        let head = &trimmed[..idx];
        if !head.is_empty() && is_plausible_arch(arch) {
            return (head.to_string(), arch.to_string());
        }
    }
    (trimmed.to_string(), String::new())
}

fn split_nvr(token: &str) -> (String, String, String) {
    let (without_arch, arch) = strip_trailing_arch(token);
    let parts: Vec<&str> = without_arch.split('-').collect();
    if parts.len() >= 3 {
        let ver_idx = parts.len() - 2;
        if parts[ver_idx]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
        {
            let name = parts[..ver_idx].join("-");
            let version = format!("{}-{}", parts[ver_idx], parts[ver_idx + 1]);
            if !name.is_empty() {
                return (name, arch, version);
            }
        }
    }
    if parts.len() >= 2 {
        let last = parts[parts.len() - 1];
        if last.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            let name = parts[..parts.len() - 1].join("-");
            if !name.is_empty() {
                return (name, arch, last.to_string());
            }
        }
    }
    (without_arch, arch, String::new())
}

fn parse_arrow_transition(line: &str) -> Option<(String, String, String, String)> {
    let (lhs, rhs) = line
        .split_once(" -> ")
        .or_else(|| line.split_once(" → "))
        .or_else(|| line.split_once("->"))
        .or_else(|| line.split_once('→'))?;
    let lhs_tokens: Vec<&str> = lhs.split_whitespace().collect();
    let rhs_token = rhs.split_whitespace().next()?.trim();
    if lhs_tokens.len() < 2 || rhs_token.is_empty() {
        return None;
    }
    let (name, mut arch) = strip_trailing_arch(lhs_tokens[0]);
    if name.is_empty() {
        return None;
    }
    let (old_ver, old_arch) = strip_trailing_arch(lhs_tokens[1]);
    let (new_ver, new_arch) = strip_trailing_arch(rhs_token);
    if arch.is_empty() {
        arch = if !new_arch.is_empty() {
            new_arch
        } else {
            old_arch
        };
    }
    if old_ver.is_empty() || new_ver.is_empty() {
        return None;
    }
    Some((name, arch, old_ver, new_ver))
}

fn parse_single_package_entry(line: &str) -> Option<(String, String, String)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    if tokens.len() >= 2 && tokens[1].chars().any(|c| c.is_ascii_digit()) {
        let (name, mut arch) = strip_trailing_arch(tokens[0]);
        let (ver, ver_arch) = strip_trailing_arch(tokens[1]);
        if arch.is_empty() {
            arch = ver_arch;
        }
        if !name.is_empty() && !ver.is_empty() {
            return Some((name, arch, ver));
        }
    }
    let (name, arch, ver) = split_nvr(tokens[0]);
    (!name.is_empty() && !ver.is_empty()).then_some((name, arch, ver))
}

fn parse_advisory_line(line: &str) -> Option<(String, PackageAdvisory)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }
    let id = tokens[0].trim();
    if !id.contains('-') || !id.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut severity = None;
    let mut kind = AdvisoryKind::Security;
    let mut pkg_name = None;
    let mut cves = Vec::new();
    let mut bugzillas = Vec::new();

    for &tok in &tokens[1..] {
        let upper = tok.to_ascii_uppercase();
        if upper.starts_with("CVE-") {
            cves.push(tok.trim_matches(',').to_string());
        } else if upper.starts_with("RHBZ#") || upper.starts_with("BZ#") {
            bugzillas.push(tok.trim_matches(',').to_string());
        } else if matches!(
            tok.to_ascii_lowercase().as_str(),
            "critical" | "important" | "moderate" | "low"
        ) {
            severity = Some(tok.to_string());
        } else if tok.eq_ignore_ascii_case("bugfix") {
            kind = AdvisoryKind::Bugfix;
        } else if tok.eq_ignore_ascii_case("enhancement") {
            kind = AdvisoryKind::Enhancement;
        } else if pkg_name.is_none() {
            let (name, _, ver) = split_nvr(tok);
            if !name.is_empty() && !ver.is_empty() {
                pkg_name = Some(name);
            }
        }
    }

    let pkg_name = pkg_name?;
    Some((
        pkg_name,
        PackageAdvisory {
            id: id.to_string(),
            kind,
            severity,
            cves,
            bugzillas,
            description: None,
        },
    ))
}

fn build_deployment_summary(
    action_label: &str,
    layered: bool,
    meta: &ParsedUpdateMeta,
    repo: &str,
) -> Option<String> {
    let mut parts = Vec::new();
    if layered {
        parts.push(format!("Layered package · {action_label}"));
    } else {
        parts.push(action_label.to_string());
    }
    if let Some(ver) = meta.version.as_deref() {
        parts.push(format!("Target deployment {ver}"));
    }
    if let Some(commit) = meta.commit_or_digest.as_deref() {
        parts.push(format!("Commit {}", short_commit(commit)));
    }
    if let Some(ts) = meta.timestamp.as_deref() {
        parts.push(ts.to_string());
    }
    if !repo.is_empty() {
        parts.push(repo.to_string());
    }
    if let Some(diff) = meta.diff_summary.as_deref() {
        parts.push(diff.to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn format_diff_detail(kind_label: &str, layered: bool, target_ver: Option<&str>) -> String {
    let mut parts = Vec::new();
    if layered {
        parts.push("Layered");
    }
    parts.push(kind_label);
    if let Some(ver) = target_ver {
        return format!("{} · {ver}", parts.join(" · "));
    }
    parts.join(" · ")
}

fn parse_cached_diff_packages(
    rpm_diff: &OstreeRpmDiff,
    meta: &ParsedUpdateMeta,
    repo: &str,
    booted: Option<&OstreeDeployment>,
    advisories: &HashMap<String, Vec<PackageAdvisory>>,
) -> Vec<Package> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let target_ver = meta.version.as_deref();

    for (kind_key, kind_label, list) in [
        ("upgraded", "Upgraded", &rpm_diff.upgraded),
        ("downgraded", "Downgraded", &rpm_diff.downgraded),
    ] {
        for item in list {
            let Some((name, arch, old_ver, new_ver)) = extract_json_diff_transition(item) else {
                continue;
            };
            let layered = booted.is_some_and(|d| d.is_layered_package(&name));
            let detail = format_diff_detail(kind_label, layered, target_ver);
            let summary = build_deployment_summary(kind_label, layered, meta, repo);
            let mut pkg = Package::new_rpm_ostree(
                kind_key,
                &name,
                arch,
                Some(old_ver),
                new_ver,
                repo,
                detail,
                summary,
            );
            if let Some(advs) = advisories.get(&name) {
                pkg.advisories = advs.clone();
            }
            if seen.insert(pkg.id.clone()) {
                out.push(pkg);
            }
        }
    }

    for item in &rpm_diff.added {
        let Some((name, arch, ver)) = extract_json_diff_single(item) else {
            continue;
        };
        let layered = booted.is_some_and(|d| d.is_layered_package(&name));
        let detail = if let Some(target) = target_ver {
            format!("{ver} · Added · {target}")
        } else {
            format!("{ver} · Added")
        };
        let summary = build_deployment_summary("Added", layered, meta, repo);
        let mut pkg =
            Package::new_rpm_ostree("added", &name, arch, None, ver, repo, detail, summary);
        if let Some(advs) = advisories.get(&name) {
            pkg.advisories = advs.clone();
        }
        if seen.insert(pkg.id.clone()) {
            out.push(pkg);
        }
    }

    for item in &rpm_diff.removed {
        let Some((name, arch, ver)) = extract_json_diff_single(item) else {
            continue;
        };
        let layered = booted.is_some_and(|d| d.is_layered_package(&name));
        let detail = format_diff_detail("Removed", layered, target_ver);
        let summary = build_deployment_summary("Removed", layered, meta, repo);
        let pkg = Package::new_rpm_ostree(
            "removed",
            &name,
            arch,
            Some(ver),
            "removed",
            repo,
            detail,
            summary,
        );
        if seen.insert(pkg.id.clone()) {
            out.push(pkg);
        }
    }

    out
}

fn extract_json_diff_transition(val: &JsonValue) -> Option<(String, String, String, String)> {
    let arr = val.as_array()?;
    let strings: Vec<&str> = arr.iter().filter_map(JsonValue::as_str).collect();
    match strings.as_slice() {
        [name, old_raw, new_raw] => {
            let (clean_name, mut arch) = strip_trailing_arch(name);
            let (old_ver, old_arch) = strip_trailing_arch(old_raw);
            let (new_ver, new_arch) = strip_trailing_arch(new_raw);
            if arch.is_empty() {
                arch = if !new_arch.is_empty() {
                    new_arch
                } else {
                    old_arch
                };
            }
            Some((clean_name, arch, old_ver, new_ver))
        }
        [name, arch, old_raw, new_raw] => {
            let (old_ver, _) = strip_trailing_arch(old_raw);
            let (new_ver, _) = strip_trailing_arch(new_raw);
            Some((
                name.trim().to_string(),
                arch.trim().to_string(),
                old_ver,
                new_ver,
            ))
        }
        _ => None,
    }
}

fn extract_json_diff_single(val: &JsonValue) -> Option<(String, String, String)> {
    let arr = val.as_array()?;
    let strings: Vec<&str> = arr.iter().filter_map(JsonValue::as_str).collect();
    match strings.as_slice() {
        [single] => {
            let (name, arch, ver) = split_nvr(single);
            (!name.is_empty() && !ver.is_empty()).then_some((name, arch, ver))
        }
        [name, ver_raw] => {
            let (clean_name, mut arch) = strip_trailing_arch(name);
            let (ver, ver_arch) = strip_trailing_arch(ver_raw);
            if arch.is_empty() {
                arch = ver_arch;
            }
            Some((clean_name, arch, ver))
        }
        [name, arch, ver_raw] => {
            let (ver, _) = strip_trailing_arch(ver_raw);
            Some((name.trim().to_string(), arch.trim().to_string(), ver))
        }
        _ => None,
    }
}

/// Parse `rpm-ostree upgrade --check` output (optionally enriched with `status --json`) into
/// inspectable `UpdateSource::RpmOstree` [`Package`] rows.
pub fn parse_upgrade_check(
    output: &str,
    status: Option<&OstreeStatus>,
) -> Result<Vec<Package>, String> {
    let booted = status.and_then(OstreeStatus::booted_deployment);
    let repo = booted
        .map(OstreeDeployment::repo_label)
        .unwrap_or_else(|| "fedora-ostree".to_string());

    let mut meta = ParsedUpdateMeta::default();
    let mut section = DiffSection::Other;
    let mut upgraded_lines = Vec::new();
    let mut downgraded_lines = Vec::new();
    let mut added_lines = Vec::new();
    let mut removed_lines = Vec::new();
    let mut advisories: HashMap<String, Vec<PackageAdvisory>> = HashMap::new();
    let mut explicit_errors = Vec::new();
    let mut saw_no_updates = false;

    for raw in output.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("error:") {
            explicit_errors.push(trimmed.to_string());
            continue;
        }
        if lower.contains("no upgrade available") || lower.contains("no updates available") {
            saw_no_updates = true;
            continue;
        }
        if trimmed.starts_with("AvailableUpdate:") {
            meta.saw_available_update = true;
            section = DiffSection::Other;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Version:") {
            meta.saw_available_update = true;
            let (ver, ts) = parse_version_and_parens_timestamp(rest);
            if !ver.is_empty() {
                meta.version = Some(ver);
            }
            if meta.timestamp.is_none() {
                meta.timestamp = ts;
            }
            section = DiffSection::Other;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Timestamp:") {
            let ts = rest.trim();
            if !ts.is_empty() {
                meta.timestamp = Some(ts.to_string());
            }
            section = DiffSection::Other;
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("Commit:")
            .or_else(|| trimmed.strip_prefix("Digest:"))
        {
            meta.saw_available_update = true;
            let c = rest.trim();
            if !c.is_empty() {
                meta.commit_or_digest = Some(c.to_string());
            }
            section = DiffSection::Other;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Diff:") {
            meta.saw_available_update = true;
            let d = rest.trim();
            if !d.is_empty() {
                meta.diff_summary = Some(d.to_string());
            }
            section = DiffSection::Other;
            continue;
        }
        if trimmed.starts_with("GPGSignature:") || trimmed.starts_with("LayeredPackages:") {
            section = DiffSection::Other;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("Upgraded:") {
            meta.saw_available_update = true;
            section = DiffSection::Upgraded;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("Downgraded:") {
            meta.saw_available_update = true;
            section = DiffSection::Downgraded;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("Added:") {
            meta.saw_available_update = true;
            section = DiffSection::Added;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("Removed:") {
            meta.saw_available_update = true;
            section = DiffSection::Removed;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("SecAdvisories:") {
            section = DiffSection::SecAdvisories;
            continue;
        }

        match section {
            DiffSection::Upgraded => upgraded_lines.push(trimmed.to_string()),
            DiffSection::Downgraded => downgraded_lines.push(trimmed.to_string()),
            DiffSection::Added => added_lines.push(trimmed.to_string()),
            DiffSection::Removed => removed_lines.push(trimmed.to_string()),
            DiffSection::SecAdvisories => {
                if let Some((pkg_name, adv)) = parse_advisory_line(trimmed) {
                    advisories.entry(pkg_name).or_default().push(adv);
                }
            }
            DiffSection::Other => {}
        }
    }

    if !meta.saw_available_update && !explicit_errors.is_empty() {
        return Err(explicit_errors.join("; "));
    }

    let mut packages = Vec::new();
    let mut seen = HashSet::new();
    let target_ver = meta.version.as_deref();

    for (kind_key, kind_label, lines) in [
        ("upgraded", "Upgraded", &upgraded_lines),
        ("downgraded", "Downgraded", &downgraded_lines),
    ] {
        for line in lines {
            let Some((name, arch, old_ver, new_ver)) = parse_arrow_transition(line) else {
                continue;
            };
            let layered = booted.is_some_and(|d| d.is_layered_package(&name));
            let detail = format_diff_detail(kind_label, layered, target_ver);
            let summary = build_deployment_summary(kind_label, layered, &meta, &repo);
            let mut pkg = Package::new_rpm_ostree(
                kind_key,
                &name,
                arch,
                Some(old_ver),
                new_ver,
                &repo,
                detail,
                summary,
            );
            if let Some(advs) = advisories.get(&name) {
                pkg.advisories = advs.clone();
            }
            if seen.insert(pkg.id.clone()) {
                packages.push(pkg);
            }
        }
    }

    for line in &added_lines {
        let Some((name, arch, ver)) = parse_single_package_entry(line) else {
            continue;
        };
        let layered = booted.is_some_and(|d| d.is_layered_package(&name));
        let detail = if let Some(target) = target_ver {
            if layered {
                format!("{ver} · Layered · Added · {target}")
            } else {
                format!("{ver} · Added · {target}")
            }
        } else if layered {
            format!("{ver} · Layered · Added")
        } else {
            format!("{ver} · Added")
        };
        let summary = build_deployment_summary("Added", layered, &meta, &repo);
        let mut pkg =
            Package::new_rpm_ostree("added", &name, arch, None, ver, &repo, detail, summary);
        if let Some(advs) = advisories.get(&name) {
            pkg.advisories = advs.clone();
        }
        if seen.insert(pkg.id.clone()) {
            packages.push(pkg);
        }
    }

    for line in &removed_lines {
        let Some((name, arch, ver)) = parse_single_package_entry(line) else {
            continue;
        };
        let layered = booted.is_some_and(|d| d.is_layered_package(&name));
        let detail = format_diff_detail("Removed", layered, target_ver);
        let summary = build_deployment_summary("Removed", layered, &meta, &repo);
        let pkg = Package::new_rpm_ostree(
            "removed",
            &name,
            arch,
            Some(ver),
            "removed",
            &repo,
            detail,
            summary,
        );
        if seen.insert(pkg.id.clone()) {
            packages.push(pkg);
        }
    }

    if packages.is_empty() && !saw_no_updates && meta.saw_available_update {
        if let Some(cached_diff) = status
            .and_then(|s| s.cached_update.as_ref())
            .and_then(|c| c.rpm_diff.as_ref())
        {
            packages = parse_cached_diff_packages(cached_diff, &meta, &repo, booted, &advisories);
        }
    }

    if packages.is_empty()
        && !saw_no_updates
        && meta.saw_available_update
        && (meta.version.is_some() || meta.commit_or_digest.is_some())
    {
        let image_name = booted
            .map(OstreeDeployment::image_display_name)
            .unwrap_or_else(|| "Fedora Atomic System Image".to_string());
        let old_ver = booted.and_then(|d| d.version.clone());
        let target_display = meta
            .version
            .clone()
            .or_else(|| meta.commit_or_digest.as_deref().map(short_commit))
            .unwrap_or_else(|| "update available".to_string());
        let detail = meta
            .diff_summary
            .clone()
            .unwrap_or_else(|| "Atomic system image deployment".to_string());
        let summary = build_deployment_summary("Atomic system image deployment", false, &meta, &repo);
        packages.push(Package::new_rpm_ostree(
            "image",
            image_name,
            "",
            old_ver,
            target_display,
            &repo,
            detail,
            summary,
        ));
    }

    Ok(packages)
}

/// Extract best-effort [`ProgressHint`] from one `rpm-ostree upgrade` output line.
pub fn parse_progress_line(line: &str) -> ProgressHint {
    let segment = line
        .split('\r')
        .map(str::trim)
        .rfind(|s| !s.is_empty())
        .unwrap_or("");
    if segment.is_empty() {
        return ProgressHint::default();
    }
    let lower = segment.to_ascii_lowercase();
    if lower.starts_with("error:")
        || lower.starts_with("warning:")
        || lower.starts_with("no upgrade available")
        || lower.starts_with("no updates available")
        || matches!(
            lower.as_str(),
            "availableupdate:"
                | "upgraded:"
                | "downgraded:"
                | "added:"
                | "removed:"
                | "secadvisories:"
        )
    {
        return ProgressHint::default();
    }

    let mut hint = ProgressHint::default();

    if lower.contains("receiving metadata objects")
        || lower.contains("updating metadata")
        || lower.contains("pulling manifest")
        || lower.contains("downloading from")
        || lower.contains("importing rpm-md")
    {
        hint.status = Some(PackageStatus::Downloading);
        hint.phase_label = Some("Downloading metadata".into());
        hint.stage_current = Some(1);
        hint.stage_total = Some(5);
    } else if lower.contains("receiving objects") || lower.contains("fetching layer") {
        hint.status = Some(PackageStatus::Downloading);
        hint.phase_label = Some("Downloading OSTree objects".into());
        hint.stage_current = Some(2);
        hint.stage_total = Some(5);
    } else if lower.contains("resolving dependencies")
        || lower.contains("checking out tree")
        || lower.contains("checking out packages")
    {
        hint.phase_label = Some("Preparing OSTree deployment".into());
        hint.stage_current = Some(3);
        hint.stage_total = Some(5);
    } else if lower.contains("writing ostree commit") || lower.contains("overlaying") {
        hint.phase_label = Some("Writing OSTree commit".into());
        hint.stage_current = Some(4);
        hint.stage_total = Some(5);
    } else if lower.contains("staging deployment") {
        hint.phase_label = Some(if lower.contains("done") {
            "Deployment staged".into()
        } else {
            "Staging deployment".into()
        });
        hint.stage_current = Some(5);
        hint.stage_total = Some(5);
        if lower.contains("done") {
            hint.progress = Some(1.0);
        }
    } else if lower.contains("systemctl reboot") || lower.contains("changes queued for next boot") {
        hint.phase_label = Some("Deployment staged".into());
        hint.stage_current = Some(5);
        hint.stage_total = Some(5);
        hint.progress = Some(1.0);
    }

    if hint.progress.is_none() && (hint.status.is_some() || hint.phase_label.is_some()) {
        if let Some(pct) = extract_percent(segment) {
            hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
        }
    }

    hint
}

/// True when `rpm-ostree upgrade` staged a new deployment for the next boot.
pub fn detect_reboot_needed(log: &str, _packages: &[Package]) -> bool {
    let lower = log.to_ascii_lowercase();
    if lower.trim().is_empty()
        || lower.contains("no upgrade available")
        || lower.contains("no updates available")
        || lower.lines().any(|l| l.trim().to_ascii_lowercase().starts_with("error:"))
    {
        return false;
    }
    lower.contains("staging deployment")
        || lower.contains("deployment staged")
        || lower.contains("systemctl reboot")
        || lower.contains("changes queued for next boot")
        || lower.lines().any(|l| {
            matches!(
                l.trim().to_ascii_lowercase().as_str(),
                "upgraded:" | "added:" | "removed:" | "downgraded:"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UpdateSource;

    #[test]
    fn host_and_binary_probes_work_deterministically_offline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("ostree-booted");
        assert!(!is_ostree_booted_at(&marker));
        std::fs::write(&marker, "").expect("write ostree-booted marker");
        assert!(is_ostree_booted_at(&marker));

        let path_dir = dir.path().join("bin");
        let fallback_dir = dir.path().join("usr/bin");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::create_dir_all(&fallback_dir).unwrap();
        let fallback_bin = fallback_dir.join("rpm-ostree");

        // Non-executable file must be ignored.
        std::fs::write(&fallback_bin, "not executable").unwrap();
        assert_eq!(
            find_rpm_ostree_binary_with(Some(path_dir.as_os_str()), &fallback_bin),
            None
        );

        // Executable fallback resolves cleanly.
        let mut perms = std::fs::metadata(&fallback_bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fallback_bin, perms).unwrap();
        assert_eq!(
            find_rpm_ostree_binary_with(Some(path_dir.as_os_str()), &fallback_bin),
            Some(fallback_bin.clone())
        );

        // Explicit PATH binary wins over fallback.
        let path_bin = path_dir.join("rpm-ostree");
        std::fs::write(&path_bin, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&path_bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path_bin, perms).unwrap();
        assert_eq!(
            find_rpm_ostree_binary_with(Some(path_dir.as_os_str()), &fallback_bin),
            Some(path_bin)
        );
    }

    #[test]
    fn parse_upgrade_check_handles_diff_layered_advisories_oci_and_errors() {
        let status = parse_status_json(
            r#"{
  "deployments": [
    {
      "id": "fedora-silverblue-0",
      "osname": "fedora",
      "checksum": "1111222233334444",
      "version": "44.20261120.0",
      "timestamp": 1795132800,
      "booted": true,
      "staged": false,
      "origin": "fedora:fedora/44/x86_64/silverblue",
      "requested-packages": ["tailscale", "htop"]
    }
  ]
}"#,
        )
        .expect("parse status");

        let diff_out = r#"
note: automatic updates (stage) are enabled
AvailableUpdate:
  Version: 44.20261124.0 (2026-11-24T06:15:30Z)
  Commit: 9f2d4e8c7b1a0f3c5e6d8a2b4c9e1f0a
  GPGSignature: Valid signature by ABC123
  SecAdvisories:
    FEDORA-2026-84a1  Important  openssl-1:3.2.2-3.fc44.x86_64  CVE-2026-2401
  Diff: 4 upgraded, 1 downgraded, 1 removed, 1 added
  Upgraded:
    firefox 139.0-1.fc44 -> 140.0-1.fc44
    mesa-dri-drivers.x86_64 26.2.5-1.fc44 -> 26.2.6-1.fc44
    openssl 1:3.2.2-1.fc44 -> 1:3.2.2-3.fc44
    tailscale 1.76.0-1.fc44 -> 1.78.0-1.fc44
  Downgraded:
    libcompat 2.0-1.fc44 → 1.9-1.fc44
  Removed:
    old-helper-1.2.3-1.fc44.x86_64
  Added:
    new-helper-2.1.0-1.fc44.noarch
"#;
        let pkgs = parse_upgrade_check(diff_out, Some(&status)).expect("parse diff");
        assert_eq!(pkgs.len(), 7);
        assert!(pkgs.iter().all(|p| p.source == UpdateSource::RpmOstree));
        assert!(pkgs.iter().all(|p| !p.source.supports_selective_apply()));

        let firefox = pkgs.iter().find(|p| p.name == "firefox").unwrap();
        assert_eq!(firefox.old_version.as_deref(), Some("139.0-1.fc44"));
        assert_eq!(firefox.version, "140.0-1.fc44");
        assert_eq!(firefox.repo, "fedora/44/x86_64/silverblue");
        assert_eq!(firefox.detail, "Upgraded · 44.20261124.0");
        assert!(firefox
            .summary
            .as_deref()
            .unwrap()
            .contains("9f2d4e8c7b1a"));

        let mesa = pkgs.iter().find(|p| p.name == "mesa-dri-drivers").unwrap();
        assert_eq!(mesa.arch, "x86_64");

        let openssl = pkgs.iter().find(|p| p.name == "openssl").unwrap();
        assert!(openssl.has_security_advisory());
        assert_eq!(openssl.primary_security_tag().as_deref(), Some("CVE-2026-2401"));

        let tailscale = pkgs.iter().find(|p| p.name == "tailscale").unwrap();
        assert!(tailscale.detail.contains("Layered"));
        assert!(tailscale.summary.as_deref().unwrap().contains("Layered"));

        let downgraded = pkgs.iter().find(|p| p.name == "libcompat").unwrap();
        assert_eq!(downgraded.old_version.as_deref(), Some("2.0-1.fc44"));
        assert_eq!(downgraded.version, "1.9-1.fc44");
        assert!(downgraded.detail.contains("Downgraded"));

        let removed = pkgs.iter().find(|p| p.name == "old-helper").unwrap();
        assert_eq!(removed.arch, "x86_64");
        assert_eq!(removed.old_version.as_deref(), Some("1.2.3-1.fc44"));
        assert_eq!(removed.version, "removed");
        assert_eq!(removed.version_line(), "1.2.3-1.fc44 → removed");

        let added = pkgs.iter().find(|p| p.name == "new-helper").unwrap();
        assert_eq!(added.arch, "noarch");
        assert_eq!(added.old_version, None);
        assert_eq!(added.version, "2.1.0-1.fc44");
        assert!(added.detail.contains("Added"));

        // Exit 77 / No updates available -> 0 updates, not an error.
        let none_out = "note: automatic updates (stage) are enabled\nNo updates available.\n";
        assert!(check_exit_is_no_updates(77, none_out));
        assert!(check_exit_is_no_updates(77, ""));
        assert!(check_exit_is_no_updates(0, none_out));
        assert!(!check_exit_is_hard_error(77, none_out, &[]));
        assert!(parse_upgrade_check(none_out, Some(&status))
            .unwrap()
            .is_empty());

        // OCI / Universal Blue image update without per-package RPM diff emits 1 deployment item.
        let ublue_status = parse_status_json(
            r#"{
  "deployments": [
    {
      "id": "fedora-bluefin-0",
      "osname": "fedora",
      "version": "41.20261120.0",
      "booted": true,
      "container-image-reference": "ostree-image-signed:docker://ghcr.io/ublue-os/bluefin:stable"
    }
  ]
}"#,
        )
        .unwrap();
        let oci_out = r#"
AvailableUpdate:
  Version: 41.20261124.0 (2026-11-24T04:00:00Z)
  Digest: sha256:abcdef1234567890fedcba
"#;
        let oci_pkgs = parse_upgrade_check(oci_out, Some(&ublue_status)).unwrap();
        assert_eq!(oci_pkgs.len(), 1);
        assert_eq!(oci_pkgs[0].name, "Bluefin System Image");
        assert_eq!(oci_pkgs[0].repo, "ghcr.io/ublue-os/bluefin:stable");
        assert_eq!(oci_pkgs[0].old_version.as_deref(), Some("41.20261120.0"));
        assert_eq!(oci_pkgs[0].version, "41.20261124.0");

        // Hard error output yields Err.
        let err_out = "error: While pulling fedora/44/x86_64/silverblue: Socket I/O timed out\n";
        assert!(parse_upgrade_check(err_out, None).is_err());
        assert!(check_exit_is_hard_error(1, err_out, &[]));
    }

    #[test]
    fn parse_progress_and_detect_reboot_cover_staged_and_noop_runs() {
        let meta = parse_progress_line("Receiving metadata objects: 12/(approx) 10/s");
        assert_eq!(meta.status, Some(PackageStatus::Downloading));
        assert_eq!(meta.phase_label.as_deref(), Some("Downloading metadata"));
        assert_eq!((meta.stage_current, meta.stage_total), (Some(1), Some(5)));

        let dl = parse_progress_line("Receiving objects: 45% (120/270) 8.4 MB/s");
        assert_eq!(dl.status, Some(PackageStatus::Downloading));
        assert_eq!(dl.phase_label.as_deref(), Some("Downloading OSTree objects"));
        assert_eq!(dl.progress, Some(0.45));
        assert_eq!((dl.stage_current, dl.stage_total), (Some(2), Some(5)));

        let prep = parse_progress_line("Checking out tree 9f2d4e8... done");
        assert_eq!(prep.status, None);
        assert_eq!(
            prep.phase_label.as_deref(),
            Some("Preparing OSTree deployment")
        );
        assert_eq!((prep.stage_current, prep.stage_total), (Some(3), Some(5)));

        let commit = parse_progress_line("Writing OSTree commit... done");
        assert_eq!(commit.phase_label.as_deref(), Some("Writing OSTree commit"));
        assert_eq!((commit.stage_current, commit.stage_total), (Some(4), Some(5)));

        let staged = parse_progress_line("Staging deployment... done");
        assert_eq!(staged.phase_label.as_deref(), Some("Deployment staged"));
        assert_eq!(staged.progress, Some(1.0));
        assert_eq!((staged.stage_current, staged.stage_total), (Some(5), Some(5)));

        assert_eq!(parse_progress_line("Upgraded:"), ProgressHint::default());
        assert_eq!(
            parse_progress_line("error: Could not depsolve transaction"),
            ProgressHint::default()
        );

        assert!(detect_reboot_needed(
            "Staging deployment... done\nRun \"systemctl reboot\" to start a reboot\n",
            &[]
        ));
        assert!(!detect_reboot_needed("No upgrade available.\n", &[]));
        assert!(!detect_reboot_needed("", &[]));
        assert!(!detect_reboot_needed(
            "error: Could not depsolve transaction\n",
            &[]
        ));
    }
}
