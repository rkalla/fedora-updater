//! Read-only discovery and execution helpers for unused Flatpak runtimes and Homebrew leaves.
//!
//! All discovery probes are read-only and non-mutating. Execution is strictly
//! gated behind explicit user confirmation and never uses `--helper` / `pkexec`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::model::{format_bytes, parse_size_bytes};

use super::flatpak::FlatpakListedRef;
use super::strip_ansi_escapes;

/// Package ecosystem targeted by a post-update cleanup action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CleanupSource {
    FlatpakUser,
    FlatpakSystem,
    Homebrew,
}

impl CleanupSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::FlatpakUser => "Flatpak (User)",
            Self::FlatpakSystem => "Flatpak (System)",
            Self::Homebrew => "Homebrew",
        }
    }

    pub fn short_badge(self) -> &'static str {
        match self {
            Self::FlatpakUser => "FP USER",
            Self::FlatpakSystem => "FP SYS",
            Self::Homebrew => "BREW",
        }
    }

    pub fn console_tag(self) -> &'static str {
        match self {
            Self::FlatpakUser => "flatpak-user",
            Self::FlatpakSystem => "flatpak-system",
            Self::Homebrew => "brew",
        }
    }
}

/// One unused runtime, extension, or leaf dependency queued for user-confirmed removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupItem {
    pub source: CleanupSource,
    /// Stable identifier (formatted Flatpak ref or Homebrew formula name).
    pub id: String,
    /// Human-friendly display name.
    pub name: String,
    /// Reclaimable installed size in bytes when known.
    pub size_bytes: Option<u64>,
    /// True for user-owned artifacts (user Flatpak or Homebrew), false for system Flatpak.
    pub user_scope: bool,
}

/// Discovered post-update cleanup candidates across enabled ecosystems.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupPlan {
    pub items: Vec<CleanupItem>,
    pub warnings: Vec<String>,
}

impl CleanupPlan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn known_reclaimable_bytes(&self) -> u64 {
        self.items
            .iter()
            .filter_map(|item| item.size_bytes)
            .fold(0u64, u64::saturating_add)
    }

    pub fn formatted_size(&self) -> Option<String> {
        let total = self.known_reclaimable_bytes();
        if total > 0 {
            Some(format_bytes(total))
        } else {
            None
        }
    }

    pub fn summary_banner_title(&self) -> String {
        let count = self.items.len();
        let noun = if count == 1 { "item" } else { "items" };
        match self.formatted_size() {
            Some(size) => format!("Clean up {count} unused {noun} ({size})"),
            None => format!("Clean up {count} unused {noun}"),
        }
    }

    pub fn has_flatpak_user(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.source == CleanupSource::FlatpakUser)
    }

    pub fn has_flatpak_system(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.source == CleanupSource::FlatpakSystem)
    }

    pub fn has_homebrew(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.source == CleanupSource::Homebrew)
    }

    /// Deduplicate by `(source, id)` and sort deterministically for stable UI display.
    pub fn normalize(&mut self) {
        let mut seen = BTreeSet::new();
        self.items.retain(|item| seen.insert((item.source, item.id.clone())));
        self.items.sort_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        });
    }
}

/// Convert unused refs discovered via `libflatpak` into normalized [`CleanupItem`] entries.
pub fn cleanup_items_from_flatpak_refs(refs: &[FlatpakListedRef], user: bool) -> Vec<CleanupItem> {
    let source = if user {
        CleanupSource::FlatpakUser
    } else {
        CleanupSource::FlatpakSystem
    };
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for r in refs {
        let id = r.formatted_ref.trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let fallback_name = ref_display_id(id);
        let name = if r.name.trim().is_empty() {
            fallback_name.to_string()
        } else if !r.branch.trim().is_empty() && !r.name.contains(r.branch.trim()) {
            format!("{} ({})", r.name.trim(), r.branch.trim())
        } else {
            r.name.trim().to_string()
        };
        items.push(CleanupItem {
            source,
            id: id.to_string(),
            name,
            size_bytes: r.installed_size_bytes.filter(|&b| b > 0),
            user_scope: user,
        });
    }
    items.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    items
}

/// Non-mutating dry-run argument list for CLI fallback when `libflatpak` symbol is unavailable.
pub fn flatpak_uninstall_unused_dry_run_args(user: bool) -> [&'static str; 4] {
    let scope = if user { "--user" } else { "--system" };
    ["uninstall", scope, "--unused", "--assumeno"]
}

/// Argument list used after explicit user confirmation to remove unused Flatpak runtimes.
pub fn flatpak_uninstall_unused_args(user: bool) -> [&'static str; 5] {
    let scope = if user { "--user" } else { "--system" };
    ["uninstall", scope, "--unused", "-y", "--noninteractive"]
}

/// Parse output from `flatpak uninstall --unused --assumeno` (dry-run output or fixture).
pub fn parse_flatpak_unused_dry_run(raw: &str, user: bool) -> Vec<CleanupItem> {
    let cleaned = strip_ansi_escapes(raw);
    let source = if user {
        CleanupSource::FlatpakUser
    } else {
        CleanupSource::FlatpakSystem
    };
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();

    for raw_line in cleaned.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || is_flatpak_non_row_line(line)
        {
            continue;
        }

        let body = strip_leading_row_number(line);
        let columns: Vec<&str> = if body.contains('\t') {
            body.split('\t')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .collect()
        } else {
            split_multi_space_columns(body)
        };
        if columns.is_empty() {
            continue;
        }

        let Some(ref_col) = columns.iter().copied().find(|c| looks_like_flatpak_ref(c)) else {
            continue;
        };

        let branch = columns
            .iter()
            .copied()
            .skip_while(|&c| c != ref_col)
            .nth(1)
            .filter(|c| is_flatpak_branch_col(c));

        let size_bytes = columns
            .iter()
            .rev()
            .find_map(|c| extract_size_bytes(c));

        let id = ref_col.to_string();
        if !seen.insert(id.clone()) {
            continue;
        }

        let base_name = ref_display_id(ref_col);
        let name = if let Some(b) = branch {
            format!("{base_name} ({b})")
        } else {
            base_name.to_string()
        };

        items.push(CleanupItem {
            source,
            id,
            name,
            size_bytes,
            user_scope: user,
        });
    }

    items
}

/// Parse output from `brew autoremove --dry-run` into unused Homebrew formula items.
pub fn parse_brew_autoremove_dry_run(raw: &str) -> Vec<CleanupItem> {
    let cleaned = strip_ansi_escapes(raw);
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();

    for raw_line in cleaned.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with("==>")
            || line.starts_with('#')
            || line.to_ascii_lowercase().starts_with("would autoremove")
            || line.to_ascii_lowercase().starts_with("no unneeded")
            || line.to_ascii_lowercase().starts_with("nothing to autoremove")
        {
            continue;
        }

        // Support either simple formula lines ("gdbm"), optional size suffixes
        // ("gdbm (4.2 MB)" / "gdbm\t4.2 MB"), or space-separated formulae.
        if line.contains('\t') {
            let parts: Vec<&str> = line
                .split('\t')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if let Some(formula) = parts.first().copied().filter(|f| looks_like_formula_name(f)) {
                let size_bytes = parts.get(1).and_then(|s| extract_size_bytes(s));
                if seen.insert(formula.to_string()) {
                    items.push(CleanupItem {
                        source: CleanupSource::Homebrew,
                        id: formula.to_string(),
                        name: formula.to_string(),
                        size_bytes,
                        user_scope: true,
                    });
                }
            }
            continue;
        }

        if let Some((before_paren, inside_paren)) = split_trailing_paren(line) {
            if let Some(size_bytes) = extract_size_bytes(inside_paren) {
                let formula = before_paren.trim();
                if looks_like_formula_name(formula) && seen.insert(formula.to_string()) {
                    items.push(CleanupItem {
                        source: CleanupSource::Homebrew,
                        id: formula.to_string(),
                        name: formula.to_string(),
                        size_bytes: Some(size_bytes),
                        user_scope: true,
                    });
                    continue;
                }
            }
        }

        for token in line.split_whitespace() {
            if looks_like_formula_name(token) && seen.insert(token.to_string()) {
                items.push(CleanupItem {
                    source: CleanupSource::Homebrew,
                    id: token.to_string(),
                    name: token.to_string(),
                    size_bytes: None,
                    user_scope: true,
                });
            }
        }
    }

    items
}

fn ref_display_id(formatted: &str) -> &str {
    formatted
        .strip_prefix("runtime/")
        .or_else(|| formatted.strip_prefix("app/"))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(formatted)
}

fn is_flatpak_non_row_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("these runtimes")
        || lower.starts_with("nothing unused")
        || lower.starts_with("proceed with these changes")
        || lower.starts_with("id ")
        || lower.starts_with("id\t")
        || lower.starts_with("---")
        || lower.starts_with("error:")
}

fn strip_leading_row_number(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx > 0 && idx < bytes.len() && bytes[idx] == b'.' {
        return line[idx + 1..].trim_start();
    }
    line
}

fn split_multi_space_columns(line: &str) -> Vec<&str> {
    let mut cols = Vec::new();
    let mut col_start = 0usize;
    let mut space_run = 0usize;
    for (idx, ch) in line.char_indices() {
        if ch == ' ' {
            if space_run == 0 && idx > col_start {
                let candidate = &line[col_start..idx];
                // Wait for 2 consecutive spaces to split columns cleanly while
                // preserving single spaces like "< 1.2 GB".
                let next_is_space = line[idx + 1..].starts_with(' ');
                if next_is_space {
                    cols.push(candidate.trim());
                }
            }
            space_run += 1;
        } else {
            if space_run >= 2 {
                col_start = idx;
            }
            space_run = 0;
        }
    }
    let tail = line[col_start..].trim();
    if !tail.is_empty() && cols.last().is_none_or(|prev| *prev != tail) {
        cols.push(tail);
    }
    if cols.is_empty() && !line.trim().is_empty() {
        cols.push(line.trim());
    }
    cols
}

fn looks_like_flatpak_ref(col: &str) -> bool {
    let trimmed = col.trim();
    !trimmed.is_empty()
        && !trimmed.contains(' ')
        && trimmed.contains('.')
        && !trimmed.ends_with(':')
}

fn is_flatpak_branch_col(col: &str) -> bool {
    let trimmed = col.trim();
    !trimmed.is_empty()
        && !matches!(trimmed, "r" | "u" | "i")
        && extract_size_bytes(trimmed).is_none()
        && !trimmed.contains(' ')
}

fn looks_like_formula_name(token: &str) -> bool {
    !token.is_empty()
        && !token.ends_with(':')
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '+' | '/'))
        && token.chars().any(|c| c.is_ascii_alphanumeric())
}

fn split_trailing_paren(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if !trimmed.ends_with(')') {
        return None;
    }
    let open = trimmed.rfind('(')?;
    let before = trimmed[..open].trim();
    let inside = trimmed[open + 1..trimmed.len() - 1].trim();
    (!before.is_empty() && !inside.is_empty()).then_some((before, inside))
}

fn extract_size_bytes(raw: &str) -> Option<u64> {
    let cleaned = raw
        .trim()
        .trim_start_matches(['(', '<', '~', '≈'])
        .trim_end_matches(')')
        .replace('\u{a0}', " ");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject bare numbers (such as Flatpak branches like "46" or "23.08") unless
    // accompanied by a byte/size unit so branches are never mistaken for byte counts.
    if trimmed.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    parse_size_bytes(trimmed).filter(|&b| b > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ref(formatted_ref: &str, name: &str, branch: &str, size: Option<u64>) -> FlatpakListedRef {
        FlatpakListedRef {
            formatted_ref: formatted_ref.to_string(),
            name: name.to_string(),
            arch: "x86_64".into(),
            branch: branch.into(),
            version: branch.into(),
            origin: "flathub".into(),
            summary: None,
            installed_size_bytes: size,
        }
    }

    #[test]
    fn converts_libflatpak_unused_refs_and_calculates_plan_totals() {
        let refs = vec![
            sample_ref(
                "runtime/org.gnome.Platform/x86_64/45",
                "GNOME Application Platform",
                "45",
                Some(950_000_000),
            ),
            sample_ref(
                "runtime/org.gnome.Platform/x86_64/45",
                "GNOME Application Platform",
                "45",
                Some(950_000_000),
            ),
            sample_ref(
                "runtime/org.freedesktop.Platform.GL.default/x86_64/23.08",
                "Mesa",
                "23.08",
                Some(420_000_000),
            ),
        ];
        let items = cleanup_items_from_flatpak_refs(&refs, true);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].source, CleanupSource::FlatpakUser);
        assert!(items.iter().all(|item| item.user_scope));

        let mut plan = CleanupPlan {
            items,
            warnings: Vec::new(),
        };
        plan.normalize();
        assert_eq!(plan.known_reclaimable_bytes(), 1_370_000_000);
        assert!(plan.summary_banner_title().starts_with("Clean up 2 unused items ("));
        assert!(plan.has_flatpak_user());
        assert!(!plan.has_flatpak_system());
        assert!(!plan.has_homebrew());
    }

    #[test]
    fn parses_flatpak_unused_dry_run_tables_with_and_without_sizes() {
        let sample = "\
These runtimes in installation 'user' are no longer used and will be uninstalled:
        ID                                      Branch     Op     Remote     Download / Size
 1.     org.freedesktop.Platform.GL.default     23.08      r      flathub    412.5 MB
 2.     runtime/org.gnome.Platform/x86_64/45    45         r      flathub    < 1.2 GB

Proceed with these changes to the user installation? [Y/n]: n
";
        let items = parse_flatpak_unused_dry_run(sample, true);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "org.freedesktop.Platform.GL.default");
        assert_eq!(items[0].name, "org.freedesktop.Platform.GL.default (23.08)");
        assert!(items[0].size_bytes.is_some());
        assert_eq!(items[1].id, "runtime/org.gnome.Platform/x86_64/45");
        assert_eq!(items[1].name, "org.gnome.Platform (45)");
        assert!(items[1].size_bytes.unwrap() > 1_000_000_000);

        let empty = parse_flatpak_unused_dry_run("Nothing unused to uninstall\n", false);
        assert!(empty.is_empty());
    }

    #[test]
    fn parses_brew_autoremove_dry_run_output_and_sizes() {
        let sample = "\
\x1b[34m==>\x1b[0m Would autoremove 3 unneeded formulae:
gdbm
mpdecimal (1.5 MB)
readline\t2.0 MB
";
        let items = parse_brew_autoremove_dry_run(sample);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, "gdbm");
        assert_eq!(items[0].size_bytes, None);
        assert_eq!(items[1].id, "mpdecimal");
        assert!(items[1].size_bytes.is_some());
        assert_eq!(items[2].id, "readline");
        assert!(items[2].size_bytes.is_some());
        assert!(items.iter().all(|item| item.source == CleanupSource::Homebrew));
        assert!(items.iter().all(|item| item.user_scope));

        assert!(parse_brew_autoremove_dry_run("==> No unneeded formulae to autoremove.\n").is_empty());
    }

    #[test]
    fn builds_expected_flatpak_unused_args() {
        assert_eq!(
            flatpak_uninstall_unused_dry_run_args(true),
            ["uninstall", "--user", "--unused", "--assumeno"]
        );
        assert_eq!(
            flatpak_uninstall_unused_args(false),
            ["uninstall", "--system", "--unused", "-y", "--noninteractive"]
        );
    }
}
