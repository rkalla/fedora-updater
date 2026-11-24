//! Rootless Distrobox (`distrobox`) and Toolbx (`toolbox`) container discovery,
//! deduplication, target extraction, and progress hints.
//!
//! Discovery runs strictly unprivileged (`distrobox list --no-color` and
//! `toolbox list --containers`) and never starts stopped containers during Check.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::model::{Package, PackageStatus, UpdateSource};

use super::{extract_percent, ProgressHint};

/// Default system path checked for `distrobox` even under minimal `.desktop` `PATH`.
pub const DISTROBOX_DEFAULT_PATH: &str = "/usr/bin/distrobox";
/// Default system path checked for `toolbox` even under minimal `.desktop` `PATH`.
pub const TOOLBOX_DEFAULT_PATH: &str = "/usr/bin/toolbox";

/// Non-interactive environment passed to container subprocesses.
pub const NONINTERACTIVE_ENV: &[(&str, &str)] = &[("NO_COLOR", "1")];

/// Parsed rootless container metadata returned by `distrobox list` or `toolbox list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub status: String,
    pub image: String,
    pub created: Option<String>,
}

impl ContainerInfo {
    /// Convert this container record into an Observatory [`Package`] row.
    pub fn into_package(self, source: UpdateSource) -> Package {
        Package::new_container(
            source,
            self.id,
            self.name,
            self.status,
            self.image,
            self.created,
        )
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_file() && (meta.permissions().mode() & 0o111) != 0
}

/// Auto-detect `distrobox` via `PATH`, `/usr/bin/distrobox`, and `~/.local/bin/distrobox`.
pub fn find_distrobox_binary() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    let home_env = std::env::var_os("HOME");
    find_binary_with(
        "distrobox",
        path_env.as_deref(),
        Path::new(DISTROBOX_DEFAULT_PATH),
        home_env.as_deref(),
    )
}

/// Auto-detect `toolbox` via `PATH`, `/usr/bin/toolbox`, and `~/.local/bin/toolbox`.
pub fn find_toolbox_binary() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    let home_env = std::env::var_os("HOME");
    find_binary_with(
        "toolbox",
        path_env.as_deref(),
        Path::new(TOOLBOX_DEFAULT_PATH),
        home_env.as_deref(),
    )
}

/// Injectable binary lookup helper for offline tests.
pub fn find_binary_with(
    program: &str,
    path_env: Option<&OsStr>,
    system_fallback: &Path,
    home_env: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(paths) = path_env.filter(|p| !p.is_empty()) {
        for dir in std::env::split_paths(paths) {
            let candidate = dir.join(program);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    if is_executable_file(system_fallback) {
        return Some(system_fallback.to_path_buf());
    }
    if let Some(home) = home_env.filter(|h| !h.is_empty()) {
        let home_candidate = PathBuf::from(home).join(".local/bin").join(program);
        if is_executable_file(&home_candidate) {
            return Some(home_candidate);
        }
    }
    None
}

fn strip_ansi_escapes(raw: &str) -> String {
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

fn is_separator_line(trimmed: &str) -> bool {
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| matches!(c, '-' | '=' | '+' | '|' | '─' | '┼') || c.is_whitespace())
}

fn ids_overlap(a: &str, b: &str) -> bool {
    let a = a.trim().to_ascii_lowercase();
    let b = b.trim().to_ascii_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    a.len() >= 12
        && b.len() >= 12
        && a.bytes().all(|byte| byte.is_ascii_hexdigit())
        && b.bytes().all(|byte| byte.is_ascii_hexdigit())
        && (a.starts_with(&b) || b.starts_with(&a))
}

fn push_unique_container(
    out: &mut Vec<ContainerInfo>,
    seen_names: &mut HashSet<String>,
    info: ContainerInfo,
) {
    if info.name.is_empty() {
        return;
    }
    if !seen_names.insert(info.name.clone()) {
        return;
    }
    if !info.id.is_empty() && out.iter().any(|existing| ids_overlap(&existing.id, &info.id)) {
        return;
    }
    out.push(info);
}

/// Parse pipe-delimited `distrobox list --no-color` output (`ID | NAME | STATUS | IMAGE`).
pub fn parse_distrobox_list(output: &str) -> Vec<ContainerInfo> {
    let mut out = Vec::new();
    let mut seen_names = HashSet::new();

    for raw in output.lines() {
        let cleaned = strip_ansi_escapes(raw);
        let trimmed = cleaned.trim();
        if trimmed.is_empty() || is_separator_line(trimmed) {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        if cols.len() < 4 {
            continue;
        }
        if cols[0].eq_ignore_ascii_case("id") && cols[1].eq_ignore_ascii_case("name") {
            continue;
        }
        let id = cols[0].to_string();
        let name = cols[1].to_string();
        let status = if cols[2].is_empty() {
            "unknown".to_string()
        } else {
            cols[2].to_string()
        };
        let image = if cols[3].is_empty() {
            "unknown".to_string()
        } else {
            cols[3].to_string()
        };
        if name.is_empty() {
            continue;
        }
        push_unique_container(
            &mut out,
            &mut seen_names,
            ContainerInfo {
                id,
                name,
                status,
                image,
                created: None,
            },
        );
    }

    out
}

#[derive(Debug, Clone, Copy)]
struct ToolboxHeaderLayout {
    id_char_start: usize,
    name_char_start: usize,
    created_char_start: usize,
    status_char_start: Option<usize>,
    image_char_start: usize,
}

fn find_header_char_start(upper_line: &str, needle: &str) -> Option<usize> {
    let byte_idx = upper_line.find(needle)?;
    Some(upper_line[..byte_idx].chars().count())
}

fn parse_toolbox_header(line: &str) -> Option<ToolboxHeaderLayout> {
    let upper = line.to_ascii_uppercase();
    let id_start = find_header_char_start(&upper, "CONTAINER ID")?;
    let name_start = find_header_char_start(&upper, "CONTAINER NAME")?;
    let created_start = find_header_char_start(&upper, "CREATED")?;
    let status_start = find_header_char_start(&upper, "STATUS");
    let image_start = find_header_char_start(&upper, "IMAGE NAME")
        .or_else(|| find_header_char_start(&upper, "IMAGE"))?;

    if !(id_start < name_start && name_start < created_start && created_start < image_start) {
        return None;
    }
    if let Some(status) = status_start {
        if !(created_start < status && status < image_start) {
            return None;
        }
    }

    Some(ToolboxHeaderLayout {
        id_char_start: id_start,
        name_char_start: name_start,
        created_char_start: created_start,
        status_char_start: status_start,
        image_char_start: image_start,
    })
}

fn slice_char_range(chars: &[char], start: usize, end: Option<usize>) -> String {
    if start >= chars.len() {
        return String::new();
    }
    let stop = end.unwrap_or(chars.len()).min(chars.len());
    if start >= stop {
        return String::new();
    }
    chars[start..stop].iter().collect::<String>().trim().to_string()
}

fn parse_toolbox_row_by_layout(line: &str, layout: ToolboxHeaderLayout) -> Option<Vec<String>> {
    let chars: Vec<char> = line.chars().collect();
    let mut starts = vec![
        layout.id_char_start,
        layout.name_char_start,
        layout.created_char_start,
    ];
    if let Some(status) = layout.status_char_start {
        starts.push(status);
    }
    starts.push(layout.image_char_start);

    if chars.len() <= *starts.last().unwrap_or(&0) {
        return None;
    }
    for &start in &starts {
        if start > 0 && !chars[start - 1].is_whitespace() {
            return None;
        }
    }

    let mut cols = Vec::with_capacity(starts.len());
    for i in 0..starts.len() {
        let start = starts[i];
        let end = starts.get(i + 1).copied();
        cols.push(slice_char_range(&chars, start, end));
    }
    Some(cols)
}

fn split_multi_space(line: &str) -> Vec<String> {
    if line.contains('\t') {
        return line
            .split('\t')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect();
    }
    let mut cols = Vec::new();
    let mut current = String::new();
    let mut space_run = 0usize;
    for ch in line.chars() {
        if ch == ' ' {
            space_run += 1;
            if space_run == 2 {
                let cell = current.trim_end().to_string();
                if !cell.is_empty() {
                    cols.push(cell);
                }
                current.clear();
            } else if space_run == 1 && !current.is_empty() {
                current.push(' ');
            }
        } else {
            space_run = 0;
            current.push(ch);
        }
    }
    let tail = current.trim().to_string();
    if !tail.is_empty() {
        cols.push(tail);
    }
    cols
}

/// Parse `toolbox list --containers` table output
/// (`CONTAINER ID  CONTAINER NAME  CREATED  STATUS  IMAGE NAME`).
pub fn parse_toolbox_list(output: &str) -> Vec<ContainerInfo> {
    let mut out = Vec::new();
    let mut seen_names = HashSet::new();
    let mut layout: Option<ToolboxHeaderLayout> = None;

    for raw in output.lines() {
        let cleaned = strip_ansi_escapes(raw);
        let line = cleaned.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || is_separator_line(trimmed) {
            continue;
        }

        if let Some(hdr) = parse_toolbox_header(line) {
            layout = Some(hdr);
            continue;
        }

        let Some(hdr) = layout else {
            continue;
        };

        let cols = parse_toolbox_row_by_layout(line, hdr).unwrap_or_else(|| split_multi_space(trimmed));
        let has_status_col = hdr.status_char_start.is_some();

        let (id, name, created, status, image) = if has_status_col && cols.len() >= 5 {
            (
                cols[0].clone(),
                cols[1].clone(),
                (!cols[2].is_empty()).then(|| cols[2].clone()),
                cols[3].clone(),
                cols[4].clone(),
            )
        } else if !has_status_col && cols.len() >= 4 {
            (
                cols[0].clone(),
                cols[1].clone(),
                (!cols[2].is_empty()).then(|| cols[2].clone()),
                "unknown".to_string(),
                cols[3].clone(),
            )
        } else if cols.len() == 4 {
            (
                cols[0].clone(),
                cols[1].clone(),
                (!cols[2].is_empty()).then(|| cols[2].clone()),
                "unknown".to_string(),
                cols[3].clone(),
            )
        } else {
            continue;
        };

        if id.is_empty() || name.is_empty() {
            continue;
        }

        push_unique_container(
            &mut out,
            &mut seen_names,
            ContainerInfo {
                id,
                name,
                status: if status.is_empty() {
                    "unknown".to_string()
                } else {
                    status
                },
                image: if image.is_empty() {
                    "unknown".to_string()
                } else {
                    image
                },
                created,
            },
        );
    }

    out
}

/// True when a container list command exit status & output cleanly indicate zero containers.
pub fn list_exit_is_empty(exit_code: i32, output: &str) -> bool {
    if exit_code == 0 {
        return true;
    }
    let lower = output.to_ascii_lowercase();
    !lower.contains("error:")
        && !lower.contains("permission denied")
        && (lower.contains("no distrobox")
            || lower.contains("no containers")
            || lower.contains("nothing found"))
}

/// Exclude any Toolbx container already managed by Distrobox (matching ID or name).
/// Distrobox takes precedence when both tools report the same Podman container.
pub fn filter_toolbox_duplicates(
    toolbox: Vec<ContainerInfo>,
    distrobox: &[ContainerInfo],
) -> Vec<ContainerInfo> {
    toolbox
        .into_iter()
        .filter(|tbx| {
            !distrobox
                .iter()
                .any(|dbox| dbox.name == tbx.name || ids_overlap(&dbox.id, &tbx.id))
        })
        .collect()
}

/// Extract deduplicated `(package_id, container_name)` upgrade targets for `source`.
pub fn container_names_from_packages(
    packages: &[Package],
    source: UpdateSource,
) -> Vec<(String, String)> {
    let prefix = format!("{}:", source.console_tag());
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for pkg in packages.iter().filter(|p| p.source == source) {
        let name = if !pkg.name.trim().is_empty() {
            pkg.name.trim()
        } else {
            pkg.id.strip_prefix(&prefix).unwrap_or(&pkg.id).trim()
        };
        if !name.is_empty() && seen.insert(name.to_string()) {
            out.push((pkg.id.clone(), name.to_string()));
        }
    }
    out
}

/// Extract coarse progress hints from container upgrade output lines.
pub fn parse_progress_line(line: &str) -> ProgressHint {
    let cleaned = strip_ansi_escapes(line);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return ProgressHint::default();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("error:")
        || lower.starts_with("warning:")
        || lower.starts_with("e: ")
    {
        return ProgressHint::default();
    }

    let mut hint = ProgressHint::default();

    if let Some(rest) = trimmed
        .strip_prefix("Container ")
        .and_then(|s| s.strip_suffix(" upgraded.").or_else(|| s.strip_suffix(" upgraded successfully.")))
    {
        let name = rest.trim();
        if !name.is_empty() {
            hint.package_name = Some(name.to_string());
        }
        hint.status = Some(PackageStatus::Completed);
        hint.phase_label = Some("Complete".into());
        hint.progress = Some(1.0);
        return hint;
    }

    if let Some(inner) = trimmed
        .strip_prefix("── Upgrading ")
        .and_then(|s| s.strip_suffix(" ──"))
    {
        let name = inner.trim();
        if !name.is_empty() {
            hint.package_name = Some(name.to_string());
            hint.status = Some(PackageStatus::Installing);
            hint.phase_label = Some(format!("Upgrading {name}"));
            return hint;
        }
    }

    if let Some(rest) = trimmed.strip_prefix("Upgrading container ") {
        let name = rest.trim_end_matches(['.', '…']).trim();
        if !name.is_empty() {
            hint.package_name = Some(name.to_string());
            hint.status = Some(PackageStatus::Installing);
            hint.phase_label = Some(format!("Upgrading {name}"));
            return hint;
        }
    }

    if lower.starts_with("upgrading ") && lower.ends_with("...") {
        let name = trimmed["Upgrading ".len()..trimmed.len() - 3].trim();
        if !name.is_empty() && !name.contains(' ') {
            hint.package_name = Some(name.to_string());
            hint.status = Some(PackageStatus::Installing);
            hint.phase_label = Some(format!("Upgrading {name}"));
            return hint;
        }
    }

    if lower.contains("downloading")
        || lower.contains("synchronizing")
        || lower.contains("fetching")
        || lower.starts_with("get:")
        || lower.starts_with("updating and loading repositories")
    {
        hint.status = Some(PackageStatus::Downloading);
        hint.phase_label = Some("Downloading packages".into());
    } else if lower.contains("running transaction") {
        hint.status = Some(PackageStatus::Installing);
        hint.phase_label = Some("Running transaction".into());
    } else if lower.contains("verifying") {
        hint.status = Some(PackageStatus::Installing);
        hint.phase_label = Some("Verifying".into());
    } else if lower == "complete!" || lower.contains("upgraded successfully") {
        hint.phase_label = Some("Complete".into());
        hint.progress = Some(1.0);
    } else if (lower.contains("upgrading")
        || lower.contains("installing")
        || lower.contains("setting up")
        || lower.contains("unpacking"))
        && trimmed != "Upgrading:"
        && trimmed != "Installing:"
    {
        hint.status = Some(PackageStatus::Installing);
        hint.phase_label = Some("Upgrading".into());
    }

    if hint.progress.is_none() && (hint.status.is_some() || hint.phase_label.is_some()) {
        if let Some(pct) = extract_percent(trimmed) {
            hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
        }
    }

    hint
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_exec_script(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fake binary");
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    #[test]
    fn find_binary_with_checks_path_system_and_home_fallbacks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path_dir = dir.path().join("bin");
        let sys_bin = dir.path().join("usr/bin/distrobox");
        let home_dir = dir.path().join("home");
        let home_bin_dir = home_dir.join(".local/bin");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::create_dir_all(sys_bin.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&home_bin_dir).unwrap();

        // Non-executable file must be ignored.
        std::fs::write(&sys_bin, "not executable").unwrap();
        assert_eq!(
            find_binary_with(
                "distrobox",
                Some(path_dir.as_os_str()),
                &sys_bin,
                Some(home_dir.as_os_str()),
            ),
            None
        );

        let home_exec = write_exec_script(&home_bin_dir, "distrobox");
        assert_eq!(
            find_binary_with(
                "distrobox",
                Some(path_dir.as_os_str()),
                &sys_bin,
                Some(home_dir.as_os_str()),
            ),
            Some(home_exec)
        );

        let sys_exec = write_exec_script(sys_bin.parent().unwrap(), "distrobox");
        assert_eq!(
            find_binary_with(
                "distrobox",
                Some(path_dir.as_os_str()),
                &sys_exec,
                Some(home_dir.as_os_str()),
            ),
            Some(sys_exec.clone())
        );

        let path_exec = write_exec_script(&path_dir, "distrobox");
        assert_eq!(
            find_binary_with(
                "distrobox",
                Some(path_dir.as_os_str()),
                &sys_exec,
                Some(home_dir.as_os_str()),
            ),
            Some(path_exec)
        );
    }

    #[test]
    fn parse_distrobox_list_handles_pipes_ansi_headers_and_empty() {
        let raw = "\
\x1b[1mID           | NAME                 | STATUS                         | IMAGE\x1b[0m
-------------+----------------------+--------------------------------+----------------------------------------------
a1b2c3d4e5f6 | arch-dev             | Up 2 hours                     | docker.io/library/archlinux:latest
f6e5d4c3b2a1 | ubuntu-24            | Exited (0) 3 days ago          | quay.io/toolbx-images/ubuntu-toolbox:24.04
a1b2c3d4e5f6 | arch-dev             | Up 2 hours                     | docker.io/library/archlinux:latest
112233445566 | fedora-44-box        | Created                        | registry.fedoraproject.org/fedora-toolbox:44
";
        let parsed = parse_distrobox_list(raw);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].id, "a1b2c3d4e5f6");
        assert_eq!(parsed[0].name, "arch-dev");
        assert_eq!(parsed[0].status, "Up 2 hours");
        assert_eq!(parsed[0].image, "docker.io/library/archlinux:latest");
        assert_eq!(parsed[1].status, "Exited (0) 3 days ago");
        assert_eq!(parsed[2].status, "Created");

        assert!(parse_distrobox_list("").is_empty());
        assert!(parse_distrobox_list("ID | NAME | STATUS | IMAGE\n").is_empty());
        assert!(list_exit_is_empty(0, ""));
        assert!(list_exit_is_empty(1, "No distrobox containers found"));
        assert!(!list_exit_is_empty(1, "Error: podman socket failure"));
    }

    #[test]
    fn parse_toolbox_list_handles_tabwriter_long_names_spaces_and_empty() {
        let raw = "\
CONTAINER ID  CONTAINER NAME                   CREATED       STATUS             IMAGE NAME
0123456789ab  fedora-toolbox-44                2 weeks ago   exited             registry.fedoraproject.org/fedora-toolbox:44
fedcba987654  work-rust-very-long-name-check   3 days ago    running            quay.io/fedora/fedora-toolbox:44
";
        let parsed = parse_toolbox_list(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "0123456789ab");
        assert_eq!(parsed[0].name, "fedora-toolbox-44");
        assert_eq!(parsed[0].created.as_deref(), Some("2 weeks ago"));
        assert_eq!(parsed[0].status, "exited");
        assert_eq!(
            parsed[0].image,
            "registry.fedoraproject.org/fedora-toolbox:44"
        );

        assert_eq!(parsed[1].name, "work-rust-very-long-name-check");
        assert_eq!(parsed[1].status, "running");

        // Four-column variant without STATUS falls back cleanly.
        let legacy = "\
CONTAINER ID  CONTAINER NAME     CREATED      IMAGE NAME
abcdef123456  fedora-toolbox-43  1 month ago  registry.fedoraproject.org/fedora-toolbox:43
";
        let parsed_legacy = parse_toolbox_list(legacy);
        assert_eq!(parsed_legacy.len(), 1);
        assert_eq!(parsed_legacy[0].status, "unknown");
        assert_eq!(
            parsed_legacy[0].image,
            "registry.fedoraproject.org/fedora-toolbox:43"
        );

        assert!(parse_toolbox_list("").is_empty());
        assert!(parse_toolbox_list("Error: failed to connect to podman").is_empty());
    }

    #[test]
    fn filter_toolbox_duplicates_deduplicates_by_name_exact_id_and_prefix_id() {
        let dbox = vec![
            ContainerInfo {
                id: "a1b2c3d4e5f6".into(),
                name: "arch-dev".into(),
                status: "Up 2 hours".into(),
                image: "docker.io/library/archlinux:latest".into(),
                created: None,
            },
            ContainerInfo {
                id: "f6e5d4c3b2a1".into(),
                name: "ubuntu-24".into(),
                status: "Exited (0) 3 days ago".into(),
                image: "quay.io/toolbx-images/ubuntu-toolbox:24.04".into(),
                created: None,
            },
        ];
        let tbx = vec![
            ContainerInfo {
                id: "a1b2c3d4e5f6".into(),
                name: "arch-dev".into(),
                status: "running".into(),
                image: "docker.io/library/archlinux:latest".into(),
                created: Some("4 days ago".into()),
            },
            ContainerInfo {
                // Full 64-char Podman ID matching Distrobox 12-char short ID
                id: "f6e5d4c3b2a19999888877776666555544443333222211110000aaaabbbbcccc".into(),
                name: "ubuntu-alias".into(),
                status: "exited".into(),
                image: "quay.io/toolbx-images/ubuntu-toolbox:24.04".into(),
                created: Some("5 days ago".into()),
            },
            ContainerInfo {
                // Different ID, same name as Distrobox
                id: "deadbeefcafe".into(),
                name: "ubuntu-24".into(),
                status: "exited".into(),
                image: "ubuntu:24.04".into(),
                created: Some("6 days ago".into()),
            },
            ContainerInfo {
                id: "0123456789ab".into(),
                name: "fedora-toolbox-44".into(),
                status: "exited".into(),
                image: "registry.fedoraproject.org/fedora-toolbox:44".into(),
                created: Some("2 weeks ago".into()),
            },
        ];

        let filtered = filter_toolbox_duplicates(tbx, &dbox);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "fedora-toolbox-44");
    }

    #[test]
    fn container_names_from_packages_and_progress_hints_work_cleanly() {
        let dbox = Package::new_container(
            UpdateSource::Distrobox,
            "a1b2c3d4e5f6",
            "arch-dev",
            "Up 2 hours",
            "docker.io/library/archlinux:latest",
            None,
        );
        let tbx = Package::new_container(
            UpdateSource::Toolbx,
            "0123456789ab",
            "fedora-toolbox-44",
            "exited",
            "registry.fedoraproject.org/fedora-toolbox:44",
            Some("2 weeks ago".into()),
        );
        let combined = vec![dbox.clone(), dbox, tbx];
        assert_eq!(
            container_names_from_packages(&combined, UpdateSource::Distrobox),
            vec![("distrobox:arch-dev".into(), "arch-dev".into())]
        );
        assert_eq!(
            container_names_from_packages(&combined, UpdateSource::Toolbx),
            vec![(
                "toolbx:fedora-toolbox-44".into(),
                "fedora-toolbox-44".into()
            )]
        );

        assert_eq!(
            parse_progress_line("Error: container failed"),
            ProgressHint::default()
        );
        let start = parse_progress_line("Upgrading arch-dev...");
        assert_eq!(start.package_name.as_deref(), Some("arch-dev"));
        assert_eq!(start.status, Some(PackageStatus::Installing));

        let dl = parse_progress_line("[1/2] Downloading git-core 65%");
        assert_eq!(dl.status, Some(PackageStatus::Downloading));
        assert_eq!(dl.progress, Some(0.65));

        let complete_inner = parse_progress_line("Complete!");
        assert_eq!(complete_inner.phase_label.as_deref(), Some("Complete"));
        assert_eq!(complete_inner.status, None);

        let complete_box = parse_progress_line("Container arch-dev upgraded successfully.");
        assert_eq!(complete_box.package_name.as_deref(), Some("arch-dev"));
        assert_eq!(complete_box.status, Some(PackageStatus::Completed));
        assert_eq!(complete_box.progress, Some(1.0));
    }
}
