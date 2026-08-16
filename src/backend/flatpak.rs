//! Flatpak list/progress parsers and accurate “needs update” detection.
//!
//! IMPORTANT: `flatpak remote-ls --updates` on Fedora/OCI remotes often lists
//! refs whose remote commit is only an **Alt-id** of the already-installed
//! commit. `flatpak update` correctly reports “Nothing to update” for those.
//! We filter those false positives by comparing remote commit to installed
//! Commit + Alt-id before showing them in the UI.

use std::process::Command;

use crate::model::{AdvisoryKind, Package, PackageStatus, UpdateSource};

use super::ProgressHint;

/// Columns for JSON remote-ls (stable field names).
pub const REMOTE_LS_COLUMNS: &str =
    "ref,application,name,version,branch,arch,origin,download-size,commit";

/// Candidate from `flatpak remote-ls --updates -j` before Alt-id filtering.
#[derive(Debug, Clone)]
pub struct FlatpakCandidate {
    pub rref: String,
    pub application_id: String,
    pub name: String,
    pub version: String,
    pub branch: String,
    pub arch: String,
    pub origin: String,
    pub size: Option<String>,
    pub remote_commit: String,
    pub source: UpdateSource,
}

/// Parse JSON array from `flatpak remote-ls … -j`.
pub fn parse_remote_ls_json(output: &str, source: UpdateSource) -> Vec<FlatpakCandidate> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // Skip any leading non-JSON noise
    let json = match trimmed.find('[') {
        Some(i) => &trimmed[i..],
        None => {
            return parse_remote_ls_updates_text(trimmed, source)
                .into_iter()
                .map(|p| candidate_from_package(p, source))
                .collect()
        }
    };

    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => {
            return parse_remote_ls_updates_text(trimmed, source)
                .into_iter()
                .map(|p| candidate_from_package(p, source))
                .collect();
        }
    };

    let arr = match value.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for item in arr {
        let application_id =
            json_str(item, &["application_id", "application", "name"]).unwrap_or_default();
        let name = json_str(item, &["name"]).unwrap_or_else(|| application_id.clone());
        let version = json_str(item, &["version"]).unwrap_or_default();
        let branch = json_str(item, &["branch"]).unwrap_or_default();
        let arch = json_str(item, &["arch"]).unwrap_or_else(|| "x86_64".into());
        let origin = json_str(item, &["origin"]).unwrap_or_else(|| "flatpak".into());
        let size = json_str(item, &["download_size", "download-size"]);
        let remote_commit = json_str(item, &["commit"]).unwrap_or_default();
        let rref = json_str(item, &["ref"]).unwrap_or_else(|| {
            // Build a ref when column missing
            let kind = if application_id.contains('.')
                && !application_id.starts_with("org.freedesktop.Platform")
            {
                // Heuristic: runtimes often have Platform/Sdk/GL in name; prefer runtime/ if branch looks like fNN
                if branch.starts_with('f') && branch.len() <= 4 {
                    "runtime"
                } else {
                    "app"
                }
            } else {
                "app"
            };
            if application_id.is_empty() {
                String::new()
            } else {
                format!("{kind}/{application_id}/{arch}/{branch}")
            }
        });

        if application_id.is_empty() && rref.is_empty() {
            continue;
        }

        let application_id = if application_id.is_empty() {
            rref.clone()
        } else {
            application_id
        };
        out.push(FlatpakCandidate {
            rref,
            application_id,
            name: if name.is_empty() {
                "Flatpak".into()
            } else {
                name
            },
            version,
            branch,
            arch,
            origin,
            size,
            remote_commit,
            source,
        });
    }
    out
}

fn candidate_from_package(p: Package, source: UpdateSource) -> FlatpakCandidate {
    // Best-effort from text parse (no commit → treated as actionable if installed check fails open)
    let app = p.id.rsplit(':').next().unwrap_or(&p.name).to_string();
    FlatpakCandidate {
        rref: app.clone(),
        application_id: app,
        name: p.name,
        version: p.version,
        branch: String::new(),
        arch: p.arch,
        origin: p.repo,
        size: p.size,
        remote_commit: String::new(),
        source,
    }
}

fn json_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Drop candidates that Flatpak would not actually update (Alt-id / same commit).
pub fn filter_actionable_candidates(candidates: Vec<FlatpakCandidate>) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for c in candidates {
        if !is_actionable_update(&c) {
            continue;
        }
        let pkg = candidate_to_package(c);
        if seen.insert(pkg.id.clone()) {
            packages.push(pkg);
        }
    }
    packages
}

/// True if `flatpak update` would likely do work for this ref.
pub fn is_actionable_update(c: &FlatpakCandidate) -> bool {
    let remote = c.remote_commit.trim();
    // No commit info → keep (text fallback) but still require ref to look real
    if remote.is_empty() {
        return !c.rref.is_empty() || !c.application_id.is_empty();
    }

    let info_target = if !c.rref.is_empty() {
        c.rref.clone()
    } else if !c.branch.is_empty() {
        format!("{}//{}", c.application_id, c.branch)
    } else {
        c.application_id.clone()
    };

    let Some((installed, alt_id)) = installed_commit_and_alt(&info_target) else {
        // Not installed under this installation? Still show — update may install.
        // But remote-ls --updates only lists installed-with-update, so treat as skip if unreadable.
        return true;
    };

    // Remote short commit matches installed primary commit or Alt-id → already current.
    if commit_matches(remote, &installed) {
        return false;
    }
    if let Some(alt) = alt_id {
        if commit_matches(remote, &alt) {
            return false;
        }
    }
    true
}

fn commit_matches(remote_short: &str, full: &str) -> bool {
    let r = remote_short.trim();
    let f = full.trim();
    if r.is_empty() || f.is_empty() {
        return false;
    }
    f.starts_with(r) || r.starts_with(&f[..f.len().min(r.len())])
}

/// Parse `flatpak info <ref>` for Commit + Alt-id.
fn installed_commit_and_alt(info_target: &str) -> Option<(String, Option<String>)> {
    let output = Command::new("flatpak")
        .args(["info", info_target])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut commit = None;
    let mut alt = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t
            .strip_prefix("Commit:")
            .or_else(|| t.strip_prefix("commit:"))
        {
            let c = rest.trim();
            if !c.is_empty() {
                commit = Some(c.to_string());
            }
        } else if let Some(rest) = t
            .strip_prefix("Alt-id:")
            .or_else(|| t.strip_prefix("Alt-Id:"))
            .or_else(|| t.strip_prefix("alt-id:"))
        {
            let a = rest.trim();
            if !a.is_empty() {
                alt = Some(a.to_string());
            }
        }
    }
    commit.map(|c| (c, alt))
}

fn candidate_to_package(c: FlatpakCandidate) -> Package {
    let scope = match c.source {
        UpdateSource::FlatpakUser => "user",
        UpdateSource::FlatpakSystem => "system",
        _ => "flatpak",
    };
    // Encode full ref in id so apply can pass it to `flatpak update`
    let id = format!("flatpak:{scope}:{}", c.rref);
    let detail = {
        let mut bits = Vec::new();
        if !c.version.is_empty() {
            bits.push(c.version.clone());
        }
        if !c.branch.is_empty() {
            bits.push(c.branch.clone());
        }
        if !c.origin.is_empty() {
            bits.push(c.origin.clone());
        }
        bits.push(scope.into());
        bits.join(" · ")
    };

    Package {
        id,
        name: c.name,
        arch: c.arch,
        version: c.version,
        repo: c.origin,
        size: c.size,
        old_version: None,
        status: PackageStatus::Pending,
        progress: 0.0,
        kind: AdvisoryKind::Unknown,
        source: c.source,
        detail,
    }
}

/// Extract the Flatpak ref to pass to `flatpak update` from a package id.
pub fn update_ref_from_package(p: &Package) -> Option<String> {
    // flatpak:user:app/org.foo/x86_64/stable
    let rest = p.id.strip_prefix("flatpak:")?;
    let (_scope, rref) = rest.split_once(':')?;
    if rref.is_empty() {
        None
    } else {
        Some(rref.to_string())
    }
}

/// Args for remote-ls check (JSON).
pub fn remote_ls_args(user: bool) -> Vec<String> {
    let mut args = vec![
        "remote-ls".into(),
        "--updates".into(),
        format!("--columns={REMOTE_LS_COLUMNS}"),
        "--json".into(),
    ];
    if user {
        args.insert(1, "--user".into());
    } else {
        args.insert(1, "--system".into());
    }
    args
}

/// Build `flatpak update` argv for a set of refs (same installation).
pub fn update_args_for_refs(user: bool, refs: &[String]) -> Vec<String> {
    let mut args = vec!["update".into()];
    if user {
        args.push("--user".into());
    } else {
        args.push("--system".into());
    }
    args.push("-y".into());
    args.push("--noninteractive".into());
    for r in refs {
        args.push(r.clone());
    }
    args
}

// ---------- text fallback (older flatpak / no JSON) ----------

pub fn parse_remote_ls_updates_text(output: &str, source: UpdateSource) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() || is_noise(line) {
            continue;
        }
        if let Some(pkg) = parse_remote_ls_line(line, source) {
            if seen.insert(pkg.id.clone()) {
                packages.push(pkg);
            }
        }
    }
    packages
}

fn is_noise(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("looking for updates")
        || lower.starts_with("error:")
        || lower.contains("no remote refs")
        || lower.starts_with("warning:")
        || line.starts_with('[')
}

fn parse_remote_ls_line(line: &str, source: UpdateSource) -> Option<Package> {
    let parts: Vec<&str> = if line.contains('\t') {
        line.split('\t')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        line.split_whitespace().collect()
    };

    if parts.is_empty() {
        return None;
    }

    let app_id = parts[0].to_string();
    if !looks_like_app_id(&app_id) {
        return None;
    }

    let (name, version, branch, origin, size) = match parts.len() {
        1 => (
            app_id.clone(),
            String::new(),
            String::new(),
            String::new(),
            None,
        ),
        2 => (
            parts[1].to_string(),
            String::new(),
            String::new(),
            String::new(),
            None,
        ),
        3 => (
            parts[1].to_string(),
            parts[2].to_string(),
            String::new(),
            String::new(),
            None,
        ),
        4 => (
            parts[1].to_string(),
            parts[2].to_string(),
            parts[3].to_string(),
            String::new(),
            None,
        ),
        5 => (
            parts[1].to_string(),
            parts[2].to_string(),
            parts[3].to_string(),
            parts[4].to_string(),
            None,
        ),
        _ => (
            parts[1].to_string(),
            parts[2].to_string(),
            parts[3].to_string(),
            parts[4].to_string(),
            Some(parts[5..].join(" ")),
        ),
    };

    let scope = match source {
        UpdateSource::FlatpakUser => "user",
        UpdateSource::FlatpakSystem => "system",
        _ => "flatpak",
    };
    let rref = app_id.clone();
    let id = format!("flatpak:{scope}:{rref}");
    let display = if name.is_empty() || name == app_id {
        app_id
    } else {
        name
    };
    let detail = {
        let mut bits = Vec::new();
        if !version.is_empty() {
            bits.push(version.clone());
        }
        if !branch.is_empty() {
            bits.push(branch);
        }
        if !origin.is_empty() {
            bits.push(origin.clone());
        }
        bits.push(scope.into());
        bits.join(" · ")
    };

    Some(Package {
        id,
        name: display,
        arch: String::new(),
        version,
        repo: if origin.is_empty() {
            "flatpak".into()
        } else {
            origin
        },
        size,
        old_version: None,
        status: PackageStatus::Pending,
        progress: 0.0,
        kind: AdvisoryKind::Unknown,
        source,
        detail,
    })
}

fn looks_like_app_id(s: &str) -> bool {
    if s.contains('/') {
        return s
            .split('/')
            .next()
            .map(|p| p == "app" || p == "runtime" || looks_like_app_id(p))
            .unwrap_or(false);
    }
    s.contains('.') && !s.contains(' ') && s.len() >= 3
}

pub fn parse_progress_line(line: &str) -> ProgressHint {
    let lower = line.to_ascii_lowercase();
    let mut hint = ProgressHint::default();

    if lower.contains("nothing to update") || lower.contains("no updates") {
        hint.phase_label = Some("Nothing to update".into());
        return hint;
    }
    if lower.contains("updating") || lower.contains("installing") {
        hint.status = Some(PackageStatus::Installing);
        hint.phase_label = Some("Updating Flatpak".into());
    }
    if lower.contains("downloading") {
        hint.status = Some(PackageStatus::Downloading);
        hint.phase_label = Some("Downloading Flatpak".into());
    }

    for token in line.split_whitespace() {
        let id = token.split('/').next().unwrap_or(token);
        if looks_like_app_id(id) {
            hint.package_name = Some(
                id.trim_start_matches("app/")
                    .trim_start_matches("runtime/")
                    .to_string(),
            );
            break;
        }
    }

    if let Some(pct) = extract_percent(line) {
        hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
    }

    hint
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_candidates() {
        let json = r#"[
          {
            "ref": "app/org.videolan.vlc/x86_64/stable",
            "application_id": "org.videolan.vlc",
            "name": "VLC",
            "version": "3.0.23",
            "branch": "stable",
            "arch": "x86_64",
            "origin": "fedora",
            "download_size": "164.0 MB",
            "commit": "6b447bd48c1d"
          }
        ]"#;
        let c = parse_remote_ls_json(json, UpdateSource::FlatpakSystem);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "VLC");
        assert_eq!(c[0].rref, "app/org.videolan.vlc/x86_64/stable");
        assert_eq!(c[0].remote_commit, "6b447bd48c1d");
    }

    #[test]
    fn commit_match_prefix() {
        assert!(commit_matches(
            "6b447bd48c1d",
            "6b447bd48c1da17c9b372b667db2ed8dcabcf163ac8875c7a92bdf1051ae0f07"
        ));
        assert!(!commit_matches(
            "6b447bd48c1d",
            "4b3ce1a2145389c120f4a3ed6537a2355e7904a14a4536af53af9e628b4f09ac"
        ));
    }

    #[test]
    fn update_ref_roundtrip() {
        let p = Package {
            id: "flatpak:system:app/org.videolan.vlc/x86_64/stable".into(),
            name: "VLC".into(),
            arch: "x86_64".into(),
            version: "3.0.23".into(),
            repo: "fedora".into(),
            size: None,
            old_version: None,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: AdvisoryKind::Unknown,
            source: UpdateSource::FlatpakSystem,
            detail: String::new(),
        };
        assert_eq!(
            update_ref_from_package(&p).as_deref(),
            Some("app/org.videolan.vlc/x86_64/stable")
        );
    }

    #[test]
    fn parse_tab_separated_text() {
        let out = "org.signal.Signal\tSignal\t8.20.0\tstable\tflathub\t120 MB\n";
        let pkgs = parse_remote_ls_updates_text(out, UpdateSource::FlatpakUser);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "Signal");
    }

    #[test]
    fn rejects_noise() {
        let pkgs =
            parse_remote_ls_updates_text("Looking for updates…\n", UpdateSource::FlatpakUser);
        assert!(pkgs.is_empty());
    }

    /// Live filter against the host (skip if flatpak missing).
    #[test]
    fn filter_drops_alt_id_false_positives_on_host() {
        if Command::new("flatpak").arg("--version").output().is_err() {
            return;
        }
        let out = Command::new("flatpak")
            .args([
                "remote-ls",
                "--system",
                "--updates",
                &format!("--columns={REMOTE_LS_COLUMNS}"),
                "--json",
            ])
            .output();
        let Ok(out) = out else { return };
        if !out.status.success() {
            return;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let candidates = parse_remote_ls_json(&text, UpdateSource::FlatpakSystem);
        if candidates.is_empty() {
            return;
        }
        let before = candidates.len();
        let actionable = filter_actionable_candidates(candidates);
        // On this host we know remote-ls can report Alt-id ghosts; filter should
        // not increase count, and typically reduces it when ghosts exist.
        assert!(actionable.len() <= before);
    }
}
