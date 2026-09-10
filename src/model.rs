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

    /// True when a backend progress line named this package. Token-exact so
    /// `kernel` does not steal `kernel-core`.
    pub fn matches_progress_name(&self, hint: &str) -> bool {
        let hint = hint.trim();
        if hint.is_empty() {
            return false;
        }
        if self.name == hint || self.id == hint {
            return true;
        }
        if !self.arch.is_empty() && format!("{}.{}", self.name, self.arch) == hint {
            return true;
        }
        // Flatpak/fwupd ids: `flatpak:user:app/org.foo/x86_64/stable`, `fwupd:<device>`.
        self.id.split([':', '/']).any(|part| part == hint)
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

/// Parse a package size from DNF/Flatpak display text or a raw byte count.
pub fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let lower = s.to_ascii_lowercase().replace(',', "");
    let split_at = lower.rfind(|c: char| c.is_ascii_digit() || c == '.')?;
    let num = lower[..=split_at].trim();
    let unit = lower[split_at + 1..].trim();
    let n: f64 = num.parse().ok()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    let mul = match unit {
        "" | "b" | "byte" | "bytes" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mul).round() as u64)
}

pub fn format_bytes(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < K * K {
        format_size_unit(b / K, "KB")
    } else if b < K * K * K {
        format_size_unit(b / (K * K), "MB")
    } else {
        format_size_unit(b / (K * K * K), "GB")
    }
}

fn format_size_unit(n: f64, unit: &str) -> String {
    if n >= 10.0 {
        format!("{n:.0} {unit}")
    } else {
        format!("{n:.1} {unit}")
    }
}

pub fn total_download_bytes(packages: &[Package]) -> Option<u64> {
    let mut total = 0u64;
    let mut any = false;
    for p in packages {
        if let Some(n) = p.size.as_deref().and_then(parse_size_bytes) {
            total = total.saturating_add(n);
            any = true;
        }
    }
    any.then_some(total)
}

/// Ready-view caption: `38 system · 0 apps · 0 firmware · 1.2 GB`.
pub fn source_summary_line(packages: &[Package]) -> String {
    let (dnf, fp, fw) = count_by_source(packages);
    match total_download_bytes(packages) {
        Some(bytes) if bytes > 0 => {
            format!(
                "{dnf} system · {fp} apps · {fw} firmware · {}",
                format_bytes(bytes)
            )
        }
        _ => format!("{dnf} system · {fp} apps · {fw} firmware"),
    }
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

/// Apply a progress hint. Unnamed lines must not poke the first remaining
/// package — except an explicit Completed status, which firmware uses for
/// "Successfully installed firmware" (no device name on that line).
///
/// Download name-chasing does not complete packages (DNF names every RPM
/// once per download, then again per install/verify). Installing name-switch
/// completes the previous *installing* item only. Already-done packages are
/// ignored so verify passes cannot resurrect them as current.
pub fn apply_progress_hint(
    packages: &mut [Package],
    source: UpdateSource,
    package_hint: Option<&str>,
    status_hint: Option<PackageStatus>,
    progress: Option<f64>,
) -> Option<usize> {
    let hint = package_hint.filter(|s| !s.trim().is_empty());
    let hint = match hint {
        Some(h) => h,
        None => {
            if status_hint != Some(PackageStatus::Completed) {
                return None;
            }
            let idx = packages
                .iter()
                .position(|p| p.source == source && !p.status.is_done())?;
            packages[idx].status = PackageStatus::Completed;
            packages[idx].progress = progress.unwrap_or(1.0).clamp(0.0, 1.0);
            return Some(idx);
        }
    };
    let idx = packages
        .iter()
        .position(|p| p.source == source && p.matches_progress_name(hint))?;

    if packages[idx].status.is_done() {
        return None;
    }

    match status_hint {
        Some(PackageStatus::Downloading) => {
            for (i, p) in packages.iter_mut().enumerate() {
                if i != idx && p.source == source && p.status == PackageStatus::Downloading {
                    p.status = PackageStatus::Pending;
                    p.progress = 0.0;
                }
            }
            packages[idx].status = PackageStatus::Downloading;
        }
        Some(PackageStatus::Installing) | None => {
            for (i, p) in packages.iter_mut().enumerate() {
                if i == idx || p.source != source {
                    continue;
                }
                if p.status == PackageStatus::Installing {
                    p.status = PackageStatus::Completed;
                    p.progress = 1.0;
                } else if p.status == PackageStatus::Downloading {
                    p.status = PackageStatus::Pending;
                    p.progress = 0.0;
                }
            }
            packages[idx].status = PackageStatus::Installing;
        }
        Some(st) => {
            packages[idx].status = st;
        }
    }
    if let Some(pr) = progress {
        packages[idx].progress = pr.clamp(0.0, 1.0);
    }
    Some(idx)
}

/// Title for the sticky now-playing card: named package, else last hint, else phase.
pub fn now_playing_title(
    packages: &[Package],
    active_index: Option<usize>,
    current_name: Option<&str>,
    phase_label: &str,
) -> String {
    if let Some(p) = active_index.and_then(|i| packages.get(i)) {
        if !p.status.is_done() {
            return p.name.clone();
        }
    }
    if let Some(name) = current_name.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    if phase_label.trim().is_empty() {
        "Starting…".into()
    } else {
        phase_label.to_string()
    }
}

/// Caption under the now-playing title. Omits the phase when it duplicates the title.
pub fn now_playing_subtitle(
    title: &str,
    phase_label: &str,
    source: Option<UpdateSource>,
) -> String {
    let mut parts = Vec::new();
    let phase = phase_label.trim();
    if !phase.is_empty() && phase != title {
        parts.push(phase.to_string());
    }
    if let Some(s) = source {
        parts.push(s.label().to_string());
    }
    parts.join(" · ")
}

pub fn overall_progress(packages: &[Package]) -> f64 {
    if packages.is_empty() {
        return 0.0;
    }
    let done = packages.iter().filter(|p| p.status.is_done()).count();
    done as f64 / packages.len() as f64
}

/// Running-view footnote and progress-meta. Both lead with the completed count
/// so "0 of 1 remaining" never sits above "1 done".
pub fn running_progress_captions(
    source: Option<UpdateSource>,
    done: usize,
    total: usize,
) -> (String, String) {
    let remaining = total.saturating_sub(done);
    let src = source.map(|s| s.label()).unwrap_or("all sources");
    let footnote = format!("{src} · {done} of {total} done");
    let meta = format!("{done} done · {remaining} remaining");
    (footnote, meta)
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

    #[test]
    fn matches_progress_name_is_token_exact() {
        let kernel = Package::new_dnf("kernel", "x86_64", "1", "updates");
        let core = Package::new_dnf("kernel-core", "x86_64", "1", "updates");
        assert!(kernel.matches_progress_name("kernel"));
        assert!(kernel.matches_progress_name("kernel.x86_64"));
        assert!(!kernel.matches_progress_name("kernel-core"));
        assert!(core.matches_progress_name("kernel-core"));
        assert!(!core.matches_progress_name("kernel"));

        let fp = Package {
            id: "flatpak:user:app/org.mozilla.Thunderbird/x86_64/stable".into(),
            name: "Thunderbird".into(),
            arch: "x86_64".into(),
            version: "1".into(),
            repo: "flathub".into(),
            size: None,
            old_version: None,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: AdvisoryKind::Unknown,
            source: UpdateSource::FlatpakUser,
            detail: String::new(),
        };
        assert!(fp.matches_progress_name("Thunderbird"));
        assert!(fp.matches_progress_name("org.mozilla.Thunderbird"));
        assert!(!fp.matches_progress_name("Thunder"));
    }

    #[test]
    fn unnamed_progress_does_not_touch_packages() {
        let mut pkgs = vec![
            Package::new_dnf("abrt", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        assert!(apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            None,
            Some(PackageStatus::Installing),
            Some(0.4),
        )
        .is_none());
        assert!(pkgs
            .iter()
            .all(|p| p.status == PackageStatus::Pending && p.progress == 0.0));
    }

    fn firmware_pkg(name: &str) -> Package {
        Package {
            id: format!("fwupd:{name}"),
            name: name.into(),
            arch: String::new(),
            version: "1".into(),
            repo: "LVFS".into(),
            size: None,
            old_version: None,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: AdvisoryKind::Security,
            source: UpdateSource::Firmware,
            detail: String::new(),
        }
    }

    #[test]
    fn unnamed_completed_hint_finishes_current_firmware_item() {
        let mut pkgs = vec![firmware_pkg("System Firmware"), firmware_pkg("UEFI CA")];
        let idx = apply_progress_hint(
            &mut pkgs,
            UpdateSource::Firmware,
            None,
            Some(PackageStatus::Completed),
            Some(1.0),
        );
        assert_eq!(idx, Some(0));
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[0].progress, 1.0);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
    }

    #[test]
    fn running_progress_captions_lead_with_done_count() {
        let (foot, meta) = running_progress_captions(Some(UpdateSource::Firmware), 1, 1);
        assert_eq!(foot, "Firmware · 1 of 1 done");
        assert_eq!(meta, "1 done · 0 remaining");
        assert!(
            !foot.contains("remaining"),
            "footnote must not invert remaining/total against the meta line, got {foot:?}"
        );

        let (foot, meta) = running_progress_captions(Some(UpdateSource::Dnf), 0, 12);
        assert_eq!(foot, "System · 0 of 12 done");
        assert_eq!(meta, "0 done · 12 remaining");

        let (foot, meta) = running_progress_captions(Some(UpdateSource::Dnf), 7, 12);
        assert_eq!(foot, "System · 7 of 12 done");
        assert_eq!(meta, "7 done · 5 remaining");
    }

    #[test]
    fn named_progress_completes_previous_live_item() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
            Some(0.2),
        );
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("kernel"),
            Some(PackageStatus::Installing),
            Some(0.1),
        );
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Installing);
    }

    #[test]
    fn download_name_switch_does_not_complete_previous() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Downloading),
            Some(0.2),
        );
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("kernel"),
            Some(PackageStatus::Downloading),
            Some(0.4),
        );
        assert_eq!(pkgs[0].status, PackageStatus::Pending);
        assert_eq!(pkgs[1].status, PackageStatus::Downloading);
    }

    #[test]
    fn done_package_is_not_resurrected_as_current() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        pkgs[0].status = PackageStatus::Completed;
        pkgs[0].progress = 1.0;
        assert!(apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
            Some(0.9),
        )
        .is_none());
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
    }

    #[test]
    fn source_summary_line_appends_rolled_up_download_size() {
        let mut kernel = Package::new_dnf("kernel-core", "x86_64", "1", "updates");
        kernel.size = Some("21727761".into());
        let mut fp = Package::new_dnf("Thunderbird", "", "128", "flathub");
        fp.source = UpdateSource::FlatpakUser;
        fp.size = Some("164.0 MB".into());
        let line = source_summary_line(&[kernel, fp]);
        assert!(
            line.starts_with("1 system · 1 apps · 0 firmware · "),
            "got {line}"
        );
        assert!(
            line.ends_with(" MB") || line.ends_with(" GB"),
            "expected a size suffix, got {line}"
        );
    }

    #[test]
    fn source_summary_line_omits_size_when_unknown() {
        let pkg = Package::new_dnf("firefox", "x86_64", "1", "updates");
        assert_eq!(
            source_summary_line(&[pkg]),
            "1 system · 0 apps · 0 firmware"
        );
    }

    #[test]
    fn format_bytes_uses_compact_units() {
        assert_eq!(format_bytes(900), "900 B");
        assert_eq!(format_bytes(12 * 1024), "12 KB");
        assert_eq!(format_bytes(21727761), "21 MB");
        assert_eq!(format_bytes(1_288_490_189), "1.2 GB");
    }

    #[test]
    fn now_playing_prefers_named_package() {
        let pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        assert_eq!(
            now_playing_title(&pkgs, Some(1), Some("kernel"), "Installing"),
            "kernel"
        );
        assert_eq!(
            now_playing_title(&pkgs, None, Some("libfoo"), "Installing"),
            "libfoo"
        );
        assert_eq!(
            now_playing_title(&pkgs, None, None, "Downloading packages"),
            "Downloading packages"
        );
        assert_eq!(
            now_playing_subtitle("kernel", "Installing", Some(UpdateSource::Dnf)),
            "Installing · System"
        );
        assert_eq!(
            now_playing_subtitle(
                "Downloading packages",
                "Downloading packages",
                Some(UpdateSource::Dnf)
            ),
            "System"
        );
    }
}
