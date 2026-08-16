//! Shared application model (no GTK).

use std::fmt;
use std::time::Instant;

/// Where an update item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateSource {
    Dnf,
    FlatpakUser,
    FlatpakSystem,
    Firmware,
}

impl UpdateSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dnf => "System",
            Self::FlatpakUser => "Apps",
            Self::FlatpakSystem => "Apps",
            Self::Firmware => "Firmware",
        }
    }

    pub fn badge(self) -> &'static str {
        match self {
            Self::Dnf => "DNF",
            Self::FlatpakUser => "FP",
            Self::FlatpakSystem => "FP",
            Self::Firmware => "FW",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Self::Dnf => "src-dnf",
            Self::FlatpakUser | Self::FlatpakSystem => "src-flatpak",
            Self::Firmware => "src-firmware",
        }
    }

    pub fn console_tag(self) -> &'static str {
        match self {
            Self::Dnf => "dnf",
            Self::FlatpakUser => "flatpak-user",
            Self::FlatpakSystem => "flatpak-system",
            Self::Firmware => "fwupd",
        }
    }

    /// Apply order: DNF → Flatpak user → Flatpak system → Firmware.
    pub fn apply_order(self) -> u8 {
        match self {
            Self::Dnf => 0,
            Self::FlatpakUser => 1,
            Self::FlatpakSystem => 2,
            Self::Firmware => 3,
        }
    }
}

impl fmt::Display for UpdateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPurpose {
    Check,
    Apply,
}

impl fmt::Display for AuthPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthPurpose::Check => write!(f, "check for updates"),
            AuthPurpose::Apply => write!(f, "apply updates"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageStatus {
    Pending,
    Downloading,
    Installing,
    Completed,
    Failed,
    /// Backend skipped because a prior backend failed hard (optional policy).
    Skipped,
}

impl PackageStatus {
    pub fn is_done(self) -> bool {
        matches!(
            self,
            PackageStatus::Completed | PackageStatus::Failed | PackageStatus::Skipped
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            PackageStatus::Pending => "queued",
            PackageStatus::Downloading => "downloading",
            PackageStatus::Installing => "installing",
            PackageStatus::Completed => "done",
            PackageStatus::Failed => "failed",
            PackageStatus::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvisoryKind {
    Security,
    Bugfix,
    Enhancement,
    Unknown,
}

impl AdvisoryKind {
    pub fn short(self) -> &'static str {
        match self {
            AdvisoryKind::Security => "S",
            AdvisoryKind::Bugfix => "B",
            AdvisoryKind::Enhancement => "E",
            AdvisoryKind::Unknown => "P",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            AdvisoryKind::Security => "security",
            AdvisoryKind::Bugfix => "bugfix",
            AdvisoryKind::Enhancement => "enhance",
            AdvisoryKind::Unknown => "unknown",
        }
    }
}

/// One updatable unit (RPM, Flatpak ref, or firmware device).
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub arch: String,
    pub version: String,
    pub repo: String,
    pub size: Option<String>,
    pub old_version: Option<String>,
    pub status: PackageStatus,
    pub progress: f64,
    pub kind: AdvisoryKind,
    pub source: UpdateSource,
    pub detail: String,
}

impl Package {
    pub fn new_dnf(
        name: impl Into<String>,
        arch: impl Into<String>,
        version: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        let name = name.into();
        let arch = arch.into();
        let version = version.into();
        let repo = repo.into();
        let id = format!("dnf:{name}.{arch}:{version}");
        Self {
            id,
            name: name.clone(),
            arch,
            version: version.clone(),
            repo: repo.clone(),
            size: None,
            old_version: None,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: AdvisoryKind::Unknown,
            source: UpdateSource::Dnf,
            detail: format!("{version} · {repo}"),
        }
    }

    pub fn version_line(&self) -> String {
        match &self.old_version {
            Some(old) => format!("{old} → {}", self.version),
            None => {
                if self.detail.is_empty() {
                    self.version.clone()
                } else {
                    self.detail.clone()
                }
            }
        }
    }

    pub fn matches_progress_name(&self, hint: &str) -> bool {
        self.name == hint
            || self.id == hint
            || self.name.starts_with(hint)
            || hint.starts_with(&self.name)
            || self.id.contains(hint)
    }
}

/// Events from background workers → UI / state machine.
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    /// Privileged session started (polkit accepted).
    SessionStarted,
    Line {
        source_tag: String,
        text: String,
    },
    BackendCheckFinished {
        source: UpdateSource,
        packages: Vec<Package>,
        exit_code: i32,
        log: String,
        /// Soft failure: backend missing or non-fatal error; continue others.
        soft_error: Option<String>,
    },
    /// All check backends finished (orchestrator complete).
    CheckAllFinished {
        packages: Vec<Package>,
        soft_errors: Vec<String>,
    },
    ApplyProgress {
        source: UpdateSource,
        package_hint: Option<String>,
        status_hint: Option<PackageStatus>,
        progress: Option<f64>,
        phase_label: String,
    },
    BackendApplyFinished {
        source: UpdateSource,
        exit_code: i32,
        log: String,
        needs_reboot: bool,
    },
    ApplyAllFinished {
        exit_code: i32,
        needs_reboot: bool,
        failed_sources: Vec<UpdateSource>,
    },
    Failed {
        message: String,
        detail: String,
    },
}

#[derive(Debug, Default, Clone)]
pub struct ConsoleBuffer {
    lines: Vec<String>,
    max_lines: usize,
}

impl ConsoleBuffer {
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: Vec::new(),
            max_lines,
        }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
        if self.lines.len() > self.max_lines {
            let excess = self.lines.len() - self.max_lines;
            self.lines.drain(0..excess);
        }
    }

    pub fn push_tagged(&mut self, tag: &str, line: &str) {
        self.push(format!("[{tag}] {line}"));
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    pub fn last_line(&self) -> &str {
        self.lines.last().map(String::as_str).unwrap_or("")
    }

    pub fn full_text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Stopwatch {
    start: Instant,
}

impl Stopwatch {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    pub fn elapsed_label(&self) -> String {
        let secs = self.elapsed_secs();
        let m = secs / 60;
        let s = secs % 60;
        if m == 0 {
            format!("{s}s")
        } else {
            format!("{m}m {s:02}s")
        }
    }
}

pub fn format_relative_now() -> String {
    "just now".into()
}

pub fn count_by_source(packages: &[Package]) -> (usize, usize, usize) {
    let mut dnf = 0;
    let mut flatpak = 0;
    let mut firmware = 0;
    for p in packages {
        match p.source {
            UpdateSource::Dnf => dnf += 1,
            UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => flatpak += 1,
            UpdateSource::Firmware => firmware += 1,
        }
    }
    (dnf, flatpak, firmware)
}

/// Counts of known advisory kinds. `Unknown` is omitted so empty chips stay hidden.
pub fn count_by_kind(packages: &[Package]) -> (usize, usize, usize) {
    let mut security = 0;
    let mut bugfix = 0;
    let mut enhancement = 0;
    for p in packages {
        match p.kind {
            AdvisoryKind::Security => security += 1,
            AdvisoryKind::Bugfix => bugfix += 1,
            AdvisoryKind::Enhancement => enhancement += 1,
            AdvisoryKind::Unknown => {}
        }
    }
    (security, bugfix, enhancement)
}

/// Check progress from finished backends. Three user-facing groups:
/// System (DNF), Apps (both Flatpak scopes), Firmware.
pub fn check_progress(checked: &[UpdateSource]) -> (f64, u32, &'static str) {
    let sys = checked.contains(&UpdateSource::Dnf);
    let fp_user = checked.contains(&UpdateSource::FlatpakUser);
    let fp_sys = checked.contains(&UpdateSource::FlatpakSystem);
    let fw = checked.contains(&UpdateSource::Firmware);
    let apps = fp_user && fp_sys;

    let groups_done = u32::from(sys) + u32::from(apps) + u32::from(fw);
    let backends = u32::from(sys) + u32::from(fp_user) + u32::from(fp_sys) + u32::from(fw);
    // Keep the bar off empty so the view never looks frozen at 0%.
    let fraction = (backends as f64 / 4.0).clamp(0.05, 1.0);

    let status = if !sys {
        "System in progress · Apps queued · Firmware queued"
    } else if !apps {
        "System done · Apps in progress · Firmware queued"
    } else if !fw {
        "System done · Apps done · Firmware in progress"
    } else {
        "System · Apps · Firmware done"
    };
    (fraction, groups_done, status)
}

/// HIG remaining-work text once progress is trustworthy.
pub fn about_time_left(elapsed_secs: u64, progress: f64) -> Option<String> {
    if progress < 0.08 || elapsed_secs < 5 {
        return None;
    }
    let remaining = elapsed_secs as f64 * (1.0 - progress) / progress;
    if remaining < 45.0 {
        Some("Less than a minute left".into())
    } else if remaining < 90.0 {
        Some("About 1 minute left".into())
    } else {
        let mins = (remaining / 60.0).round().max(2.0) as u64;
        Some(format!("About {mins} minutes left"))
    }
}

pub fn sort_for_apply(packages: &mut [Package]) {
    packages.sort_by_key(|p| (p.source.apply_order(), p.name.clone()));
}

pub fn sources_present(packages: &[Package]) -> Vec<UpdateSource> {
    let mut out = Vec::new();
    for src in [
        UpdateSource::Dnf,
        UpdateSource::FlatpakUser,
        UpdateSource::FlatpakSystem,
        UpdateSource::Firmware,
    ] {
        if packages.iter().any(|p| p.source == src) {
            out.push(src);
        }
    }
    out
}

/// Mark all items for a source as completed/failed based on exit code.
pub fn finalize_source(packages: &mut [Package], source: UpdateSource, ok: bool) {
    for p in packages.iter_mut().filter(|p| p.source == source) {
        if p.status.is_done() {
            continue;
        }
        if ok {
            p.status = PackageStatus::Completed;
            p.progress = 1.0;
        } else {
            p.status = PackageStatus::Failed;
        }
    }
}

/// Apply progress hint to matching package(s) within a source.
pub fn apply_progress_hint(
    packages: &mut [Package],
    source: UpdateSource,
    package_hint: Option<&str>,
    status_hint: Option<PackageStatus>,
    progress: Option<f64>,
) -> Option<usize> {
    // Complete previous active item in this source when switching names
    if let Some(hint) = package_hint {
        let mut prev_active: Option<usize> = None;
        for (i, p) in packages.iter().enumerate() {
            if p.source == source
                && matches!(
                    p.status,
                    PackageStatus::Downloading | PackageStatus::Installing
                )
                && !p.matches_progress_name(hint)
            {
                prev_active = Some(i);
            }
        }
        if let Some(i) = prev_active {
            packages[i].status = PackageStatus::Completed;
            packages[i].progress = 1.0;
        }

        if let Some(idx) = packages
            .iter()
            .position(|p| p.source == source && p.matches_progress_name(hint))
        {
            if let Some(st) = status_hint {
                packages[idx].status = st;
            } else if packages[idx].status == PackageStatus::Pending {
                packages[idx].status = PackageStatus::Installing;
            }
            if let Some(pr) = progress {
                packages[idx].progress = pr.clamp(0.0, 1.0);
            }
            return Some(idx);
        }
    } else if let Some(st) = status_hint {
        // Update first non-done of this source
        if let Some(idx) = packages
            .iter()
            .position(|p| p.source == source && !p.status.is_done())
        {
            packages[idx].status = st;
            if let Some(pr) = progress {
                packages[idx].progress = pr.clamp(0.0, 1.0);
            }
            return Some(idx);
        }
    }
    None
}

pub fn overall_progress(packages: &[Package]) -> f64 {
    if packages.is_empty() {
        return 0.0;
    }
    let done = packages.iter().filter(|p| p.status.is_done()).count();
    done as f64 / packages.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_by_kind_skips_unknown() {
        let mut sec = Package::new_dnf("openssl", "x86_64", "1", "updates");
        sec.kind = AdvisoryKind::Security;
        let mut bug = Package::new_dnf("mesa", "x86_64", "1", "updates");
        bug.kind = AdvisoryKind::Bugfix;
        let unk = Package::new_dnf("foo", "x86_64", "1", "updates");
        assert_eq!(count_by_kind(&[sec, bug, unk]), (1, 1, 0));
    }

    #[test]
    fn check_progress_groups_flatpak() {
        let (frac, groups, status) = check_progress(&[UpdateSource::Dnf]);
        assert!(frac > 0.0 && frac < 1.0);
        assert_eq!(groups, 1);
        assert!(status.contains("Apps in progress"));

        let (_, groups, status) = check_progress(&[
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
        ]);
        assert_eq!(groups, 2);
        assert!(status.contains("Firmware in progress"));
    }

    #[test]
    fn about_time_left_needs_enough_signal() {
        assert!(about_time_left(2, 0.5).is_none());
        assert!(about_time_left(30, 0.05).is_none());
        let label = about_time_left(60, 0.25).expect("estimate");
        assert!(label.contains("minute"));
    }
}
