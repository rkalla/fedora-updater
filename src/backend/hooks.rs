//! Opt-in, strictly unprivileged user-defined update hooks.
//!
//! Custom hooks are persisted in `$XDG_CONFIG_HOME/fedora-updater/ui.json`,
//! discovered and executed solely as the current desktop user (`sh -c ...` via
//! `run_cancellable_local_env`), and never routed through `--helper` / `pkexec`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::model::{Package, PackageStatus, UpdateSource};

use super::{extract_percent, strip_ansi_escapes, ProgressHint};

fn default_true() -> bool {
    true
}

/// User-defined update command configured in `$XDG_CONFIG_HOME/fedora-updater/ui.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomHook {
    pub name: String,
    #[serde(default)]
    pub check_command: Option<String>,
    pub apply_command: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl CustomHook {
    /// Return a trimmed copy where whitespace-only `check_command` collapses to `None`.
    pub fn normalized(&self) -> Self {
        let check_command = self
            .check_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        Self {
            name: self.name.trim().to_string(),
            check_command,
            apply_command: self.apply_command.trim().to_string(),
            enabled: self.enabled,
        }
    }

    /// Valid hooks must specify both a non-empty display name and apply command.
    pub fn is_valid(&self) -> bool {
        !self.name.trim().is_empty() && !self.apply_command.trim().is_empty()
    }

    pub fn has_check_command(&self) -> bool {
        self.check_command
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    }
}

/// Static editable form preset offered in the Preferences "New Custom Hook" dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookTemplate {
    pub name: &'static str,
    pub check_command: Option<&'static str>,
    pub apply_command: &'static str,
}

impl HookTemplate {
    /// Materialize this template as an enabled, unprivileged [`CustomHook`].
    pub fn to_custom_hook(&self) -> CustomHook {
        CustomHook {
            name: self.name.to_string(),
            check_command: self.check_command.map(ToOwned::to_owned),
            apply_command: self.apply_command.to_string(),
            enabled: true,
        }
        .normalized()
    }
}

/// Optional user-owned presets for common unprivileged CLI managers.
/// Never auto-seeded or auto-enabled; users pick and customize them manually.
pub const HOOK_TEMPLATES: &[HookTemplate] = &[
    HookTemplate {
        name: "uv tools",
        check_command: None,
        apply_command: "uv tool upgrade --all",
    },
    HookTemplate {
        name: "pipx apps",
        check_command: None,
        apply_command: "pipx upgrade-all",
    },
    HookTemplate {
        name: "Cargo binaries",
        check_command: None,
        apply_command: "cargo install-update -a",
    },
    HookTemplate {
        name: "npm global",
        check_command: None,
        apply_command: "npm update -g",
    },
];

/// Outcome of evaluating a custom hook's optional `check_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookCheckResult {
    UpToDate,
    UpdateAvailable {
        summary: String,
        detail_lines: Vec<String>,
    },
    Failed(String),
}

/// Pair every enabled valid hook with a unique deterministic package `id`.
pub fn enabled_hooks_with_ids(hooks: &[CustomHook]) -> Vec<(String, CustomHook)> {
    let mut seen_ids = BTreeSet::new();
    let mut out = Vec::new();
    for (idx, raw) in hooks.iter().enumerate() {
        let hook = raw.normalized();
        if !hook.enabled || !hook.is_valid() {
            continue;
        }
        let base_slug = slugify_hook_name(&hook.name);
        let mut candidate = format!("hook:{idx}:{base_slug}");
        while !seen_ids.insert(candidate.clone()) {
            candidate.push_str("-dup");
        }
        out.push((candidate, hook));
    }
    out
}

/// Interpret a hook `check_command` exit code and captured output.
///
/// Convention:
/// - exit `0` + non-empty trimmed output -> update available (first line is concise summary)
/// - exit `0` + empty output -> up to date
/// - non-zero exit -> check failure detail
pub fn interpret_check_result(status_code: i32, raw_output: &str) -> HookCheckResult {
    let cleaned = strip_ansi_escapes(raw_output);
    let lines: Vec<String> = cleaned
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if status_code != 0 {
        let tail = lines
            .last()
            .cloned()
            .unwrap_or_else(|| format!("exit code {status_code}"));
        return HookCheckResult::Failed(tail);
    }

    if lines.is_empty() {
        return HookCheckResult::UpToDate;
    }

    let summary = summarize_check_lines(&lines);
    HookCheckResult::UpdateAvailable {
        summary,
        detail_lines: lines,
    }
}

/// Construct a [`Package`] entry when a hook with `check_command` reported available updates.
pub fn package_from_checked_hook(
    hook_id: &str,
    hook: &CustomHook,
    check: &HookCheckResult,
) -> Option<Package> {
    let HookCheckResult::UpdateAvailable {
        summary,
        detail_lines,
    } = check
    else {
        return None;
    };

    let mut detail_parts = Vec::new();
    detail_parts.push(format!("Check summary: {}", detail_lines.join("\n")));
    detail_parts.push(format!("Apply command: {}", hook.apply_command));
    detail_parts.push("Privilege: current user account only (never root)".into());

    Some(Package {
        id: hook_id.to_string(),
        name: hook.name.clone(),
        arch: "user".into(),
        version: "available".into(),
        repo: "custom hook (user)".into(),
        size_bytes: None,
        old_version: Some("checked".into()),
        status: PackageStatus::Pending,
        source: UpdateSource::CustomHook,
        detail: detail_parts.join("\n"),
        summary: Some(format!("Custom user hook: {summary}")),
        url: None,
        install_size_delta: None,
        advisories: Vec::new(),
        preflight_warning: None,
    })
}

/// Construct a [`Package`] entry for an enabled hook that defines no `check_command`.
pub fn package_from_unverified_hook(hook_id: &str, hook: &CustomHook) -> Package {
    Package {
        id: hook_id.to_string(),
        name: hook.name.clone(),
        arch: "user".into(),
        version: "custom".into(),
        repo: "custom hook (user)".into(),
        size_bytes: None,
        old_version: None,
        status: PackageStatus::Pending,
        source: UpdateSource::CustomHook,
        detail: "Custom user command".into(),
        summary: Some(format!(
            "Custom user command (no check command configured; runs `{}`)",
            hook.apply_command
        )),
        url: None,
        install_size_delta: None,
        advisories: Vec::new(),
        preflight_warning: None,
    }
}

/// Resolve selected `UpdateSource::CustomHook` packages back to their enabled [`CustomHook`] definitions.
pub fn resolve_hooks_for_packages(
    hooks: &[CustomHook],
    packages: &[&Package],
) -> Vec<(String, CustomHook, String)> {
    let catalog = enabled_hooks_with_ids(hooks);
    let mut resolved = Vec::new();

    for pkg in packages {
        if pkg.source != UpdateSource::CustomHook {
            continue;
        }
        let matched = catalog
            .iter()
            .find(|(id, _)| id == &pkg.id)
            .or_else(|| {
                catalog
                    .iter()
                    .find(|(_, hook)| hook.name.eq_ignore_ascii_case(pkg.name.trim()))
            });
        if let Some((_, hook)) = matched {
            resolved.push((pkg.id.clone(), hook.clone(), pkg.name.clone()));
        }
    }

    resolved
}

/// Extract optional percentage hints emitted by user hook scripts.
pub fn parse_progress_line(line: &str) -> ProgressHint {
    ProgressHint {
        progress: extract_percent(line).map(|pct| (pct / 100.0).clamp(0.0, 1.0)),
        ..ProgressHint::default()
    }
}

fn summarize_check_lines(lines: &[String]) -> String {
    let joined = lines.join("; ");
    const MAX_SUMMARY_CHARS: usize = 120;
    if joined.chars().count() <= MAX_SUMMARY_CHARS {
        joined
    } else {
        let prefix: String = joined.chars().take(MAX_SUMMARY_CHARS - 1).collect();
        format!("{prefix}…")
    }
}

fn slugify_hook_name(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "hook".into()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_validates_custom_hooks() {
        let hook = CustomHook {
            name: "  Cargo tools  ".into(),
            check_command: Some("   ".into()),
            apply_command: "  cargo install-update -a  ".into(),
            enabled: true,
        }
        .normalized();
        assert_eq!(hook.name, "Cargo tools");
        assert_eq!(hook.check_command, None);
        assert_eq!(hook.apply_command, "cargo install-update -a");
        assert!(hook.is_valid());
        assert!(!hook.has_check_command());

        let blank = CustomHook {
            name: "".into(),
            check_command: None,
            apply_command: "echo hi".into(),
            enabled: true,
        };
        assert!(!blank.is_valid());
    }

    #[test]
    fn deserializes_hook_defaults_when_optional_fields_omitted() {
        let json = r#"{"name":"Sync dotfiles","apply_command":"chezmoi update"}"#;
        let hook: CustomHook = serde_json::from_str(json).expect("hook json");
        assert_eq!(hook.check_command, None);
        assert!(hook.enabled);
    }

    #[test]
    fn interprets_check_command_output_and_exit_status() {
        assert_eq!(interpret_check_result(0, "\n  \n"), HookCheckResult::UpToDate);
        assert_eq!(
            interpret_check_result(2, "network down\nfatal: timeout\n"),
            HookCheckResult::Failed("fatal: timeout".into())
        );

        let update = interpret_check_result(0, "\x1b[32mripgrep 14.0 -> 14.1\x1b[0m\nfd 9.0 -> 10.0");
        match update {
            HookCheckResult::UpdateAvailable {
                summary,
                detail_lines,
            } => {
                assert_eq!(summary, "ripgrep 14.0 -> 14.1; fd 9.0 -> 10.0");
                assert_eq!(detail_lines.len(), 2);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn constructs_and_resolves_custom_hook_packages() {
        let hooks = vec![
            CustomHook {
                name: "Cargo install-update".into(),
                check_command: Some("cargo install-update --list".into()),
                apply_command: "cargo install-update -a".into(),
                enabled: true,
            },
            CustomHook {
                name: "Disabled hook".into(),
                check_command: None,
                apply_command: "echo skip".into(),
                enabled: false,
            },
            CustomHook {
                name: "Sync chezmoi".into(),
                check_command: None,
                apply_command: "chezmoi update".into(),
                enabled: true,
            },
        ];

        let active = enabled_hooks_with_ids(&hooks);
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].0, "hook:0:cargo-install-update");
        assert_eq!(active[1].0, "hook:2:sync-chezmoi");

        let checked_res = interpret_check_result(0, "bat 0.24 -> 0.25");
        let checked_pkg = package_from_checked_hook(&active[0].0, &active[0].1, &checked_res)
            .expect("checked package");
        assert_eq!(checked_pkg.source, UpdateSource::CustomHook);
        assert_eq!(checked_pkg.version, "available");
        assert!(checked_pkg
            .summary
            .as_deref()
            .unwrap()
            .contains("bat 0.24 -> 0.25"));

        let unverified_pkg = package_from_unverified_hook(&active[1].0, &active[1].1);
        assert_eq!(unverified_pkg.source, UpdateSource::CustomHook);
        assert_eq!(unverified_pkg.version, "custom");
        assert_eq!(unverified_pkg.detail, "Custom user command");

        let refs = [&checked_pkg, &unverified_pkg];
        let resolved = resolve_hooks_for_packages(&hooks, &refs);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].1.apply_command, "cargo install-update -a");
        assert_eq!(resolved[1].1.apply_command, "chezmoi update");
    }

    #[test]
    fn extracts_percentage_progress_from_hook_output() {
        let hint = parse_progress_line("Updating plugins: 65%");
        assert_eq!(hint.progress, Some(0.65));
        assert_eq!(hint.package_name, None);
    }

    #[test]
    fn hook_templates_are_valid_and_strictly_unprivileged() {
        assert!(!HOOK_TEMPLATES.is_empty());
        assert_eq!(HOOK_TEMPLATES.len(), 4);

        let names: BTreeSet<&str> = HOOK_TEMPLATES.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), HOOK_TEMPLATES.len());

        for tpl in HOOK_TEMPLATES {
            let hook = tpl.to_custom_hook();
            assert!(hook.is_valid(), "template `{}` should be valid", tpl.name);
            assert!(hook.enabled);
            assert_eq!(hook.check_command, None);

            let apply_lower = hook.apply_command.to_ascii_lowercase();
            for forbidden in ["sudo", "pkexec", "doas", "--helper"] {
                assert!(
                    !apply_lower.contains(forbidden),
                    "template `{}` must not reference `{forbidden}`",
                    tpl.name
                );
                if let Some(chk) = hook.check_command.as_deref() {
                    assert!(
                        !chk.to_ascii_lowercase().contains(forbidden),
                        "template `{}` check_command must not reference `{forbidden}`",
                        tpl.name
                    );
                }
            }
        }

        assert_eq!(HOOK_TEMPLATES[0].name, "uv tools");
        assert_eq!(HOOK_TEMPLATES[0].apply_command, "uv tool upgrade --all");
        assert_eq!(HOOK_TEMPLATES[1].name, "pipx apps");
        assert_eq!(HOOK_TEMPLATES[1].apply_command, "pipx upgrade-all");
        assert_eq!(HOOK_TEMPLATES[2].name, "Cargo binaries");
        assert_eq!(HOOK_TEMPLATES[2].apply_command, "cargo install-update -a");
        assert_eq!(HOOK_TEMPLATES[3].name, "npm global");
        assert_eq!(HOOK_TEMPLATES[3].apply_command, "npm update -g");
    }
}
