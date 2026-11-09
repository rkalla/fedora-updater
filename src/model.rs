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

/// Which idle page the window is showing. The view renders [`idle_chrome`];
/// it does not choose a screen by matching subtitle text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleScreen {
    /// Never checked in this session.
    Fresh,
    /// A check finished with nothing to install.
    /// `detail` is set only when a backend reported a soft error.
    Current {
        checked_at: String,
        detail: Option<String>,
    },
    /// The user left the Done page after an apply.
    Updated { checked_at: String },
}

/// What the Failed page's Retry button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAction {
    CheckAgain,
    Reboot,
}

/// Download vs apply inside one backend source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPass {
    Download,
    Apply,
}

/// `[current/total]` for the active download or transaction.
/// Built only when both numbers are present and `total > 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageCount {
    pub current: u32,
    pub total: u32,
}

/// Idle page copy and button style. One row per [`IdleScreen`].
/// The view paints these fields only. It does not paint a button icon, a hint, or a footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleChrome {
    pub title: &'static str,
    pub icon: &'static str,
    pub button: &'static str,
    pub suggested: bool,
    pub pill: bool,
    pub flat: bool,
    pub description: Option<String>,
}

pub fn idle_chrome(screen: &IdleScreen) -> IdleChrome {
    match screen {
        IdleScreen::Fresh => IdleChrome {
            title: "Check for Updates",
            icon: "software-update-available-symbolic",
            button: "Check for Updates",
            suggested: true,
            pill: true,
            flat: false,
            description: Some(
                "System packages, apps, and firmware. Nothing changes until the list is reviewed."
                    .into(),
            ),
        },
        IdleScreen::Current {
            checked_at,
            detail: Some(text),
        } => up_to_date_chrome(checked_at, Some(text.clone())),
        IdleScreen::Current {
            checked_at,
            detail: None,
        }
        | IdleScreen::Updated { checked_at } => up_to_date_chrome(checked_at, None),
    }
}

fn up_to_date_chrome(checked_at: &str, detail: Option<String>) -> IdleChrome {
    IdleChrome {
        title: "System is up to date",
        icon: "object-select-symbolic",
        button: "Check Again",
        suggested: false,
        pill: false,
        flat: true,
        description: Some(detail.unwrap_or_else(|| format!("Last checked {checked_at}"))),
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
    /// Newline-terminated backend output.
    Line {
        source_tag: String,
        text: String,
    },
    /// Carriage-return progress. Replaces the open console line.
    Draw {
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
        stage_current: Option<u32>,
        stage_total: Option<u32>,
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
    /// Last entry is an in-progress `\r` update and the next one replaces it.
    live_tail: bool,
}

impl ConsoleBuffer {
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: Vec::new(),
            max_lines,
            live_tail: false,
        }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
        self.live_tail = false;
        if self.lines.len() > self.max_lines {
            let excess = self.lines.len() - self.max_lines;
            self.lines.drain(0..excess);
        }
    }

    pub fn push_tagged(&mut self, tag: &str, line: &str) {
        self.push_committed_tagged(tag, line);
    }

    /// Carriage-return progress. Replaces the open line instead of appending.
    pub fn push_live_tagged(&mut self, tag: &str, line: &str) {
        let formatted = format!("[{tag}] {line}");
        if self.live_tail {
            if let Some(last) = self.lines.last_mut() {
                *last = formatted;
                return;
            }
        }
        self.lines.push(formatted);
        self.live_tail = true;
        self.trim();
    }

    /// Newline-terminated output. A following commit replaces the open `\r` line.
    pub fn push_committed_tagged(&mut self, tag: &str, line: &str) {
        let formatted = format!("[{tag}] {line}");
        if self.live_tail {
            if let Some(last) = self.lines.last_mut() {
                *last = formatted;
                self.live_tail = false;
                return;
            }
        }
        self.lines.push(formatted);
        self.live_tail = false;
        self.trim();
    }

    fn trim(&mut self) {
        if self.lines.len() > self.max_lines {
            let excess = self.lines.len() - self.max_lines;
            self.lines.drain(0..excess);
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.live_tail = false;
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

/// Check-page cursor. Chosen from which sources have finished, then matched
/// to the status sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStep {
    /// Dnf is not in the checked list yet.
    System,
    /// Dnf is in. Flatpak user and Flatpak system are not both in.
    Apps,
    /// Both Flatpak sides are in. Firmware is not.
    Firmware,
    /// All three groups are in.
    Done,
}

/// Check progress from finished backends. Three user-facing groups:
/// System (DNF), Apps (both Flatpak scopes), Firmware.
pub fn check_progress(checked: &[UpdateSource]) -> (CheckStep, f64, u32, &'static str) {
    let sys = checked.contains(&UpdateSource::Dnf);
    let fp_user = checked.contains(&UpdateSource::FlatpakUser);
    let fp_sys = checked.contains(&UpdateSource::FlatpakSystem);
    let fw = checked.contains(&UpdateSource::Firmware);
    let apps = fp_user && fp_sys;

    let groups_done = u32::from(sys) + u32::from(apps) + u32::from(fw);
    let backends = u32::from(sys) + u32::from(fp_user) + u32::from(fp_sys) + u32::from(fw);
    // Keep the bar off empty so the view never looks frozen at 0%.
    let fraction = (backends as f64 / 4.0).clamp(0.05, 1.0);

    let step = match (sys, apps, fw) {
        (false, _, _) => CheckStep::System,
        (true, false, _) => CheckStep::Apps,
        (true, true, false) => CheckStep::Firmware,
        (true, true, true) => CheckStep::Done,
    };
    let status = match step {
        CheckStep::System => "System in progress · Apps queued · Firmware queued",
        CheckStep::Apps => "System done · Apps in progress · Firmware queued",
        CheckStep::Firmware => "System done · Apps done · Firmware in progress",
        CheckStep::Done => "System · Apps · Firmware done",
    };
    (step, fraction, groups_done, status)
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

/// What one progress hint does to the package rows. Decided before any write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProgressEffect {
    /// No row changes. Return None.
    Ignore,
    /// Unnamed Completed. First not-done row of this source becomes Completed.
    CompleteFirstOpen { progress: f64 },
    /// Named Downloading. Other Downloading rows of this source go back to Pending.
    FocusDownloading { index: usize },
    /// Named Installing. Other Installing rows of this source become Completed.
    /// Other Downloading rows of this source go back to Pending.
    FocusInstalling { index: usize },
    /// Named hint with no status and no fraction. Return Some(index). Write nothing.
    Touch { index: usize },
    /// Named hint with no status, with a fraction. Status stays.
    FractionOnly { index: usize, progress: f64 },
    /// Named Completed, Failed, Skipped, or Pending.
    SetStatus {
        index: usize,
        status: PackageStatus,
        progress: Option<f64>,
    },
}

fn clamp_unit(progress: f64) -> f64 {
    progress.clamp(0.0, 1.0)
}

/// Choose the row effect from the current packages and the hint. Does not write.
///
/// The Running reducer coerces Downloading to Installing when the pass is
/// already Apply, and it does that before `apply_progress_hint`. A Downloading
/// hint that reaches this function is a download-pass hint.
pub fn progress_effect(
    packages: &[Package],
    source: UpdateSource,
    package_hint: Option<&str>,
    status_hint: Option<PackageStatus>,
    progress: Option<f64>,
) -> ProgressEffect {
    let hint = package_hint.map(str::trim).filter(|s| !s.is_empty());
    let Some(hint) = hint else {
        if status_hint != Some(PackageStatus::Completed) {
            return ProgressEffect::Ignore;
        }
        if !packages
            .iter()
            .any(|p| p.source == source && !p.status.is_done())
        {
            return ProgressEffect::Ignore;
        }
        return ProgressEffect::CompleteFirstOpen {
            progress: progress.map(clamp_unit).unwrap_or(1.0),
        };
    };
    // Same RPM name can be i686 and x86_64. The first row may already be done
    // from the other arch; keep looking so the twin still updates.
    let Some(index) = packages
        .iter()
        .position(|p| p.source == source && !p.status.is_done() && p.matches_progress_name(hint))
    else {
        return ProgressEffect::Ignore;
    };
    match status_hint {
        Some(PackageStatus::Downloading) => ProgressEffect::FocusDownloading { index },
        Some(PackageStatus::Installing) => ProgressEffect::FocusInstalling { index },
        None => match progress {
            Some(progress) => ProgressEffect::FractionOnly {
                index,
                progress: clamp_unit(progress),
            },
            None => ProgressEffect::Touch { index },
        },
        Some(status) => ProgressEffect::SetStatus {
            index,
            status,
            progress: progress.map(clamp_unit),
        },
    }
}

/// Apply a progress hint. Unnamed lines must not poke the first remaining
/// package — except an explicit Completed status, which firmware uses for
/// "Successfully installed firmware" (no device name on that line).
///
/// Download name-chasing does not complete packages (DNF names every RPM
/// once per download, then again per install/verify). Installing name-switch
/// completes the previous *installing* item only. A hint with no status only
/// updates that package's fraction — it must not invent an install.
/// Already-done packages are ignored so verify passes cannot resurrect them.
pub fn apply_progress_hint(
    packages: &mut [Package],
    source: UpdateSource,
    package_hint: Option<&str>,
    status_hint: Option<PackageStatus>,
    progress: Option<f64>,
) -> Option<usize> {
    match progress_effect(packages, source, package_hint, status_hint, progress) {
        ProgressEffect::Ignore => None,
        ProgressEffect::CompleteFirstOpen { progress } => {
            let idx = packages
                .iter()
                .position(|p| p.source == source && !p.status.is_done())?;
            packages[idx].status = PackageStatus::Completed;
            packages[idx].progress = progress;
            Some(idx)
        }
        ProgressEffect::FocusDownloading { index } => {
            for (i, p) in packages.iter_mut().enumerate() {
                if i != index && p.source == source && p.status == PackageStatus::Downloading {
                    p.status = PackageStatus::Pending;
                    p.progress = 0.0;
                }
            }
            packages[index].status = PackageStatus::Downloading;
            if let Some(pr) = progress {
                packages[index].progress = clamp_unit(pr);
            }
            Some(index)
        }
        ProgressEffect::FocusInstalling { index } => {
            for (i, p) in packages.iter_mut().enumerate() {
                if i == index || p.source != source {
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
            packages[index].status = PackageStatus::Installing;
            if let Some(pr) = progress {
                packages[index].progress = clamp_unit(pr);
            }
            Some(index)
        }
        ProgressEffect::Touch { index } => Some(index),
        ProgressEffect::FractionOnly { index, progress } => {
            packages[index].progress = progress;
            Some(index)
        }
        ProgressEffect::SetStatus {
            index,
            status,
            progress: fraction,
        } => {
            packages[index].status = status;
            if let Some(fraction) = fraction {
                packages[index].progress = fraction;
            }
            Some(index)
        }
    }
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

/// Heading for the running page. Verifying stays its own label inside the apply pass.
pub fn run_heading(phase_label: &str, pass: RunPass) -> &'static str {
    let label = phase_label.to_ascii_lowercase();
    if label.contains("verif") {
        "Verifying"
    } else if pass == RunPass::Apply
        || label.contains("install")
        || label.contains("upgrad")
        || label.contains("transaction")
    {
        "Applying"
    } else if label.contains("download") {
        "Downloading"
    } else {
        "Updating"
    }
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
    pass: RunPass,
) -> (String, String) {
    let remaining = total.saturating_sub(done);
    let src = source.map(|s| s.label()).unwrap_or("all sources");
    let verb = if pass == RunPass::Apply {
        "done"
    } else {
        "downloaded"
    };
    let footnote = format!("{src} · {done} of {total} {verb}");
    let meta = format!("{done} {verb} · {remaining} remaining");
    (footnote, meta)
}

/// Why the running page looks quiet. First match wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietMode {
    Exited,
    CacheGrowing,
    Stalled,
    ItemAtFull,
    Flowing,
}

/// How the running page should look when backend output has gone quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuietUi {
    pub mode: QuietMode,
    pub show_eta: bool,
    /// Put the spinner back over a stale 100% bar or a silent stretch.
    pub show_spinner: bool,
    /// Replaces the ETA prefix on the footnote. `None` keeps the normal caption.
    pub footnote: Option<String>,
    pub banner: Option<String>,
    /// Ask rpm whether the planned DNF versions are already installed.
    pub reconcile: bool,
}

pub fn quiet_ui(
    quiet_secs: u64,
    at_full_item: bool,
    cache_note: Option<&str>,
    tool_alive: bool,
    watching_dnf: bool,
    last_line: &str,
    daemon_running: bool,
) -> QuietUi {
    let cache = cache_note.filter(|s| !s.is_empty());
    let show_eta = quiet_secs < 15 && cache.is_none();
    let exited = watching_dnf && !tool_alive && quiet_secs >= 20;
    let stalled = quiet_secs >= 120 && cache.is_none() && !exited;
    let at_full_spinner = quiet_secs >= 5 && at_full_item && cache.is_none();
    let show_spinner = cache.is_some() || exited || stalled || at_full_spinner;
    let mode = if exited {
        QuietMode::Exited
    } else if cache.is_some() {
        QuietMode::CacheGrowing
    } else if stalled {
        QuietMode::Stalled
    } else if at_full_spinner {
        QuietMode::ItemAtFull
    } else {
        QuietMode::Flowing
    };
    let footnote = if let Some(note) = cache {
        Some(note.to_string())
    } else if exited {
        Some("dnf has exited".into())
    } else if stalled {
        Some(if watching_dnf {
            "Waiting on dnf".into()
        } else {
            "Waiting for output".into()
        })
    } else {
        None
    };
    // `last_line` stays in the signature. The stall banner does not quote it.
    // The running page shows that line in the console bar instead.
    let _ = last_line;
    let banner = if exited {
        Some("dnf has exited. Checking whether the packages were installed.".into())
    } else if stalled {
        let mins = (quiet_secs / 60).max(2);
        Some(if daemon_running {
            format!("No output for {mins} minutes. Waiting on GNOME Software.")
        } else {
            format!("No output for {mins} minutes.")
        })
    } else {
        None
    };
    QuietUi {
        mode,
        show_eta,
        show_spinner,
        footnote,
        banner,
        reconcile: exited,
    }
}

pub fn cache_activity_note(name: &str, bytes: u64, rising: bool) -> String {
    let size = format_bytes(bytes);
    if rising {
        format!("Downloading {name} — {size} and rising")
    } else {
        format!("Downloading {name} — {size}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_up_to_date(chrome: &IdleChrome, _checked_at: &str, description: &str) {
        assert_eq!(chrome.title, "System is up to date");
        assert_eq!(chrome.icon, "object-select-symbolic");
        assert_eq!(chrome.button, "Check Again");
        assert!(!chrome.suggested);
        assert!(!chrome.pill);
        assert!(chrome.flat);
        assert_eq!(chrome.description.as_deref(), Some(description));
    }

    #[test]
    fn idle_chrome_fresh() {
        let chrome = idle_chrome(&IdleScreen::Fresh);
        assert_eq!(chrome.title, "Check for Updates");
        assert_eq!(chrome.icon, "software-update-available-symbolic");
        assert_eq!(chrome.button, "Check for Updates");
        assert!(chrome.suggested);
        assert!(chrome.pill);
        assert!(!chrome.flat);
        assert_eq!(
            chrome.description.as_deref(),
            Some(
                "System packages, apps, and firmware. Nothing changes until the list is reviewed."
            )
        );
    }

    #[test]
    fn idle_chrome_current_without_detail() {
        let checked_at = "3 hours ago";
        let chrome = idle_chrome(&IdleScreen::Current {
            checked_at: checked_at.into(),
            detail: None,
        });
        assert_up_to_date(&chrome, checked_at, "Last checked 3 hours ago");
    }

    #[test]
    fn idle_chrome_current_with_detail() {
        let checked_at = "yesterday";
        let chrome = idle_chrome(&IdleScreen::Current {
            checked_at: checked_at.into(),
            detail: Some("No updates available (fwupd offline)".into()),
        });
        assert_up_to_date(&chrome, checked_at, "No updates available (fwupd offline)");
    }

    #[test]
    fn idle_chrome_updated() {
        let checked_at = "just now";
        let chrome = idle_chrome(&IdleScreen::Updated {
            checked_at: checked_at.into(),
        });
        assert_up_to_date(&chrome, checked_at, "Last checked just now");
    }

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
        let (_step, frac, groups, status) = check_progress(&[UpdateSource::Dnf]);
        assert!(frac > 0.0 && frac < 1.0);
        assert_eq!(groups, 1);
        assert!(status.contains("Apps in progress"));

        let (_, _, groups, status) = check_progress(&[
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
        ]);
        assert_eq!(groups, 2);
        assert!(status.contains("Firmware in progress"));
    }

    #[test]
    fn check_step_names_the_cursor() {
        let (step, frac, groups, status) = check_progress(&[]);
        assert_eq!(step, CheckStep::System);
        assert!((frac - 0.05).abs() < 1e-9);
        assert_eq!(groups, 0);
        assert_eq!(status, "System in progress · Apps queued · Firmware queued");

        let (step, frac, groups, status) = check_progress(&[UpdateSource::Dnf]);
        assert_eq!(step, CheckStep::Apps);
        assert!((frac - 0.25).abs() < 1e-9);
        assert_eq!(groups, 1);
        assert_eq!(status, "System done · Apps in progress · Firmware queued");

        let (step, frac, groups, status) =
            check_progress(&[UpdateSource::Dnf, UpdateSource::FlatpakUser]);
        assert_eq!(step, CheckStep::Apps);
        assert!((frac - 0.5).abs() < 1e-9);
        assert_eq!(groups, 1);
        assert_eq!(status, "System done · Apps in progress · Firmware queued");

        let (step, frac, groups, status) = check_progress(&[
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
        ]);
        assert_eq!(step, CheckStep::Firmware);
        assert!((frac - 0.75).abs() < 1e-9);
        assert_eq!(groups, 2);
        assert_eq!(status, "System done · Apps done · Firmware in progress");

        let (step, frac, groups, status) = check_progress(&[
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
            UpdateSource::Firmware,
        ]);
        assert_eq!(step, CheckStep::Done);
        assert!((frac - 1.0).abs() < 1e-9);
        assert_eq!(groups, 3);
        assert_eq!(status, "System · Apps · Firmware done");

        let (step, frac, groups, status) =
            check_progress(&[UpdateSource::Dnf, UpdateSource::Firmware]);
        assert_eq!(step, CheckStep::Apps);
        assert!((frac - 0.5).abs() < 1e-9);
        assert_eq!(groups, 2);
        assert_eq!(status, "System done · Apps in progress · Firmware queued");
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
        let (foot, meta) =
            running_progress_captions(Some(UpdateSource::Firmware), 1, 1, RunPass::Apply);
        assert_eq!(foot, "Firmware · 1 of 1 done");
        assert_eq!(meta, "1 done · 0 remaining");
        assert!(
            !foot.contains("remaining"),
            "footnote must not invert remaining/total against the meta line, got {foot:?}"
        );

        let (foot, meta) =
            running_progress_captions(Some(UpdateSource::Dnf), 0, 12, RunPass::Apply);
        assert_eq!(foot, "System · 0 of 12 done");
        assert_eq!(meta, "0 done · 12 remaining");

        let (foot, meta) =
            running_progress_captions(Some(UpdateSource::Dnf), 7, 12, RunPass::Apply);
        assert_eq!(foot, "System · 7 of 12 done");
        assert_eq!(meta, "7 done · 5 remaining");

        let (foot, meta) =
            running_progress_captions(Some(UpdateSource::Dnf), 25, 26, RunPass::Download);
        assert_eq!(foot, "System · 25 of 26 downloaded");
        assert_eq!(meta, "25 downloaded · 1 remaining");
    }

    #[test]
    fn second_arch_receives_progress_after_the_first_is_done() {
        let mut pkgs = vec![
            Package::new_dnf("mesa-dri-drivers", "i686", "1", "updates"),
            Package::new_dnf("mesa-dri-drivers", "x86_64", "1", "updates"),
        ];
        pkgs[0].status = PackageStatus::Completed;
        pkgs[0].progress = 1.0;
        let idx = apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("mesa-dri-drivers"),
            Some(PackageStatus::Installing),
            Some(0.4),
        );
        assert_eq!(idx, Some(1));
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Installing);
        assert!((pkgs[1].progress - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn carriage_return_replaces_the_open_console_line() {
        let mut console = ConsoleBuffer::new(20);
        console.push_live_tagged("dnf", "[26/26] grok-bot  10%");
        console.push_live_tagged("dnf", "[26/26] grok-bot  40%");
        assert_eq!(console.len(), 1);
        assert!(console.last_line().contains("40%"));
        console.push_committed_tagged("dnf", "[26/26] grok-bot 100%");
        assert_eq!(console.len(), 1);
        assert!(console.last_line().contains("100%"));
        console.push_committed_tagged("dnf", "Complete!");
        assert_eq!(console.len(), 2);
        assert!(console.last_line().contains("Complete!"));
    }

    #[test]
    fn quiet_download_hides_eta_and_names_a_growing_cache_file() {
        let fresh = quiet_ui(2, false, None, true, true, "", false);
        assert_eq!(fresh.mode, QuietMode::Flowing);
        assert!(fresh.show_eta);
        assert!(!fresh.show_spinner);
        assert!(fresh.banner.is_none());
        assert!(!fresh.reconcile);

        let stuck_full = quiet_ui(8, true, None, true, true, "[25/26] tzdata-java 100%", false);
        assert_eq!(stuck_full.mode, QuietMode::ItemAtFull);
        assert!(stuck_full.show_spinner);
        assert!(stuck_full.show_eta);

        let growing = quiet_ui(
            400,
            true,
            Some("Downloading grok-bot — 85 MB and rising"),
            true,
            true,
            "[25/26] tzdata-java 100%",
            false,
        );
        assert_eq!(growing.mode, QuietMode::CacheGrowing);
        assert!(!growing.show_eta);
        assert!(growing.show_spinner);
        assert_eq!(
            growing.footnote.as_deref(),
            Some("Downloading grok-bot — 85 MB and rising")
        );
        assert!(growing.banner.is_none());
        assert!(!growing.reconcile);

        let stalled = quiet_ui(
            180,
            true,
            None,
            true,
            true,
            "[25/26] tzdata-java 100%",
            false,
        );
        assert_eq!(stalled.mode, QuietMode::Stalled);
        assert!(!stalled.show_eta);
        let stalled_banner = stalled.banner.expect("stalled banner");
        assert_eq!(stalled_banner, "No output for 3 minutes.");
        assert!(!stalled_banner.contains("tzdata-java"));
        assert!(!stalled_banner.contains("Last line:"));
        assert!(!stalled.reconcile);

        let exited = quiet_ui(
            25,
            true,
            None,
            false,
            true,
            "[25/26] tzdata-java 100%",
            false,
        );
        assert_eq!(exited.mode, QuietMode::Exited);
        assert!(exited.reconcile);
        assert!(exited.banner.unwrap().contains("Checking whether"));

        let overlap = quiet_ui(
            25,
            true,
            Some("Downloading grok-bot — 85 MB and rising"),
            false,
            true,
            "[25/26] tzdata-java 100%",
            false,
        );
        assert_eq!(overlap.mode, QuietMode::Exited);
        assert_eq!(
            overlap.footnote.as_deref(),
            Some("Downloading grok-bot — 85 MB and rising")
        );
        assert!(overlap.reconcile);
        assert_eq!(
            overlap.banner.as_deref(),
            Some("dnf has exited. Checking whether the packages were installed.")
        );

        let flatpak = quiet_ui(30, false, None, false, false, "", false);
        assert_eq!(flatpak.mode, QuietMode::Flowing);
        assert!(!flatpak.reconcile);
    }

    #[test]
    fn stalled_banner_names_the_daemon_only_when_asked() {
        let on = quiet_ui(
            180,
            true,
            None,
            true,
            true,
            "[25/26] tzdata-java 100%",
            true,
        );
        let on_banner = on.banner.expect("stalled banner");
        assert_eq!(
            on_banner,
            "No output for 3 minutes. Waiting on GNOME Software."
        );
        assert!(
            on_banner.contains("Waiting on GNOME Software"),
            "{on_banner}"
        );
        assert!(!on_banner.contains("dnf daemon"), "{on_banner}");
        assert!(!on_banner.contains("Last line:"), "{on_banner}");
        assert!(!on_banner.contains("tzdata-java"), "{on_banner}");

        let off = quiet_ui(
            180,
            true,
            None,
            true,
            true,
            "[25/26] tzdata-java 100%",
            false,
        );
        let off_banner = off.banner.expect("stalled banner");
        assert_eq!(off_banner, "No output for 3 minutes.");
        assert!(
            !off_banner.contains("Waiting on GNOME Software"),
            "{off_banner}"
        );
        assert!(!off_banner.contains("dnf daemon"), "{off_banner}");
        assert!(!off_banner.contains("Last line:"), "{off_banner}");
        assert!(!off_banner.contains("tzdata-java"), "{off_banner}");
    }

    #[test]
    fn absent_helper_does_not_look_like_exited() {
        let quiet = quiet_ui(25, true, None, true, true, "", false);
        assert!(!quiet.reconcile);
    }

    #[test]
    fn cache_activity_note_mentions_growth() {
        assert_eq!(
            cache_activity_note("grok-bot", 85 * 1024 * 1024, true),
            "Downloading grok-bot — 85 MB and rising"
        );
        assert_eq!(
            cache_activity_note("grok-bot", 85 * 1024 * 1024, false),
            "Downloading grok-bot — 85 MB"
        );
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
    fn progress_only_hint_does_not_complete_previous() {
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
            None,
            Some(0.4),
        );
        assert_eq!(pkgs[0].status, PackageStatus::Installing);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
        assert!((pkgs[1].progress - 0.4).abs() < f64::EPSILON);
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
    fn progress_effect_table_covers_each_row() {
        let dnf = |name: &str| Package::new_dnf(name, "x86_64", "1", "updates");
        let flatpak_downloading = |name: &str, progress: f64| {
            let mut pkg = dnf(name);
            pkg.source = UpdateSource::FlatpakUser;
            pkg.status = PackageStatus::Downloading;
            pkg.progress = progress;
            pkg
        };

        // Empty name, anything except Completed: Ignore, and no row changes.
        let mut open = vec![dnf("firefox"), dnf("kernel")];
        let untouched = open.clone();
        for status in [
            Some(PackageStatus::Pending),
            Some(PackageStatus::Downloading),
            Some(PackageStatus::Installing),
            Some(PackageStatus::Failed),
            Some(PackageStatus::Skipped),
            None,
        ] {
            for hint in [None, Some(""), Some("   ")] {
                assert_eq!(
                    progress_effect(&open, UpdateSource::Dnf, hint, status, Some(0.4)),
                    ProgressEffect::Ignore,
                );
                assert!(
                    apply_progress_hint(&mut open, UpdateSource::Dnf, hint, status, Some(0.4),)
                        .is_none()
                );
            }
        }
        assert_eq!(open, untouched);

        // Empty Completed: first not-done row of that source. Clamp, or 1.0.
        let mut mixed = vec![dnf("firefox"), firmware_pkg("System Firmware")];
        mixed[1].status = PackageStatus::Failed;
        mixed.push(firmware_pkg("UEFI CA"));
        assert_eq!(
            progress_effect(
                &mixed,
                UpdateSource::Firmware,
                None,
                Some(PackageStatus::Completed),
                Some(1.5),
            ),
            ProgressEffect::CompleteFirstOpen { progress: 1.0 },
        );
        assert_eq!(
            progress_effect(
                &mixed,
                UpdateSource::Firmware,
                Some("  "),
                Some(PackageStatus::Completed),
                Some(-0.2),
            ),
            ProgressEffect::CompleteFirstOpen { progress: 0.0 },
        );
        assert_eq!(
            progress_effect(
                &mixed,
                UpdateSource::Firmware,
                None,
                Some(PackageStatus::Completed),
                None,
            ),
            ProgressEffect::CompleteFirstOpen { progress: 1.0 },
        );
        assert_eq!(
            apply_progress_hint(
                &mut mixed,
                UpdateSource::Firmware,
                None,
                Some(PackageStatus::Completed),
                Some(0.25),
            ),
            Some(2),
        );
        assert_eq!(mixed[0].status, PackageStatus::Pending);
        assert_eq!(mixed[1].status, PackageStatus::Failed);
        assert_eq!(mixed[2].status, PackageStatus::Completed);
        assert!((mixed[2].progress - 0.25).abs() < f64::EPSILON);

        let mut all_done = vec![dnf("firefox")];
        all_done[0].status = PackageStatus::Completed;
        all_done[0].progress = 1.0;
        let done_snap = all_done.clone();
        assert_eq!(
            progress_effect(
                &all_done,
                UpdateSource::Dnf,
                None,
                Some(PackageStatus::Completed),
                Some(1.0),
            ),
            ProgressEffect::Ignore,
        );
        assert!(apply_progress_hint(
            &mut all_done,
            UpdateSource::Dnf,
            None,
            Some(PackageStatus::Completed),
            Some(1.0),
        )
        .is_none());
        assert_eq!(all_done, done_snap);

        // Named hint with no open target leaves every row alone.
        let mut no_target = vec![dnf("firefox"), dnf("kernel-core")];
        no_target[0].status = PackageStatus::Completed;
        no_target[0].progress = 1.0;
        let no_target_snap = no_target.clone();
        for hint in ["firefox", "kernel", "not-a-package"] {
            assert_eq!(
                progress_effect(
                    &no_target,
                    UpdateSource::Dnf,
                    Some(hint),
                    Some(PackageStatus::Installing),
                    Some(0.9),
                ),
                ProgressEffect::Ignore,
            );
            assert!(apply_progress_hint(
                &mut no_target,
                UpdateSource::Dnf,
                Some(hint),
                Some(PackageStatus::Installing),
                Some(0.9),
            )
            .is_none());
        }
        assert_eq!(no_target, no_target_snap);

        // The second arch is the target once the first copy is done.
        let mut arches = vec![
            Package::new_dnf("mesa-dri-drivers", "i686", "1", "updates"),
            Package::new_dnf("mesa-dri-drivers", "x86_64", "1", "updates"),
        ];
        arches[0].status = PackageStatus::Completed;
        arches[0].progress = 1.0;
        assert_eq!(
            progress_effect(
                &arches,
                UpdateSource::Dnf,
                Some("mesa-dri-drivers"),
                Some(PackageStatus::Installing),
                Some(0.4),
            ),
            ProgressEffect::FocusInstalling { index: 1 },
        );

        // Download name switch is FocusDownloading. The previous row goes
        // Pending, and another source's Downloading row stays.
        let mut downloads = vec![
            dnf("firefox"),
            dnf("kernel"),
            flatpak_downloading("Thunderbird", 0.8),
        ];
        assert_eq!(
            progress_effect(
                &downloads,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Downloading),
                Some(0.2),
            ),
            ProgressEffect::FocusDownloading { index: 0 },
        );
        assert_eq!(
            apply_progress_hint(
                &mut downloads,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Downloading),
                Some(0.2),
            ),
            Some(0),
        );
        assert_eq!(downloads[0].status, PackageStatus::Downloading);
        assert!((downloads[0].progress - 0.2).abs() < f64::EPSILON);
        assert_eq!(downloads[2].status, PackageStatus::Downloading);
        assert!((downloads[2].progress - 0.8).abs() < f64::EPSILON);

        assert_eq!(
            progress_effect(
                &downloads,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Downloading),
                Some(0.4),
            ),
            ProgressEffect::FocusDownloading { index: 1 },
        );
        assert_eq!(
            apply_progress_hint(
                &mut downloads,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Downloading),
                Some(0.4),
            ),
            Some(1),
        );
        assert_eq!(downloads[0].status, PackageStatus::Pending);
        assert_eq!(downloads[0].progress, 0.0);
        assert_eq!(downloads[1].status, PackageStatus::Downloading);
        assert!((downloads[1].progress - 0.4).abs() < f64::EPSILON);
        assert_eq!(downloads[2].status, PackageStatus::Downloading);
        assert!((downloads[2].progress - 0.8).abs() < f64::EPSILON);

        // A Downloading hint with no fraction still leaves the target Downloading.
        let mut bare = vec![dnf("firefox")];
        bare[0].progress = 0.33;
        assert_eq!(
            progress_effect(
                &bare,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Downloading),
                None,
            ),
            ProgressEffect::FocusDownloading { index: 0 },
        );
        assert_eq!(
            apply_progress_hint(
                &mut bare,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Downloading),
                None,
            ),
            Some(0),
        );
        assert_eq!(bare[0].status, PackageStatus::Downloading);
        assert!((bare[0].progress - 0.33).abs() < f64::EPSILON);

        // Install name switch is FocusInstalling. Same-source Downloading
        // siblings reset; another source's Downloading row stays.
        let mut installs = vec![
            dnf("firefox"),
            dnf("wget"),
            dnf("kernel"),
            flatpak_downloading("App", 0.3),
        ];
        installs[1].status = PackageStatus::Downloading;
        installs[1].progress = 0.7;
        assert_eq!(
            progress_effect(
                &installs,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Installing),
                Some(0.2),
            ),
            ProgressEffect::FocusInstalling { index: 0 },
        );
        apply_progress_hint(
            &mut installs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
            Some(0.2),
        );
        installs[1].status = PackageStatus::Downloading;
        installs[1].progress = 0.7;
        assert_eq!(
            progress_effect(
                &installs,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Installing),
                Some(0.1),
            ),
            ProgressEffect::FocusInstalling { index: 2 },
        );
        assert_eq!(
            apply_progress_hint(
                &mut installs,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Installing),
                Some(0.1),
            ),
            Some(2),
        );
        assert_eq!(installs[0].status, PackageStatus::Completed);
        assert_eq!(installs[0].progress, 1.0);
        assert_eq!(installs[1].status, PackageStatus::Pending);
        assert_eq!(installs[1].progress, 0.0);
        assert_eq!(installs[2].status, PackageStatus::Installing);
        assert!((installs[2].progress - 0.1).abs() < f64::EPSILON);
        assert_eq!(installs[3].status, PackageStatus::Downloading);
        assert!((installs[3].progress - 0.3).abs() < f64::EPSILON);

        // Status-less fraction keeps status, including a previous Installing row.
        let mut fractions = vec![dnf("firefox"), dnf("kernel")];
        apply_progress_hint(
            &mut fractions,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
            Some(0.2),
        );
        assert_eq!(
            progress_effect(
                &fractions,
                UpdateSource::Dnf,
                Some("kernel"),
                None,
                Some(1.4),
            ),
            ProgressEffect::FractionOnly {
                index: 1,
                progress: 1.0,
            },
        );
        assert_eq!(
            progress_effect(
                &fractions,
                UpdateSource::Dnf,
                Some("kernel"),
                None,
                Some(0.4),
            ),
            ProgressEffect::FractionOnly {
                index: 1,
                progress: 0.4,
            },
        );
        assert_eq!(
            apply_progress_hint(
                &mut fractions,
                UpdateSource::Dnf,
                Some("kernel"),
                None,
                Some(0.4),
            ),
            Some(1),
        );
        assert_eq!(fractions[0].status, PackageStatus::Installing);
        assert!((fractions[0].progress - 0.2).abs() < f64::EPSILON);
        assert_eq!(fractions[1].status, PackageStatus::Pending);
        assert!((fractions[1].progress - 0.4).abs() < f64::EPSILON);

        // Status-less hint with no fraction writes nothing.
        let touch_snap = fractions.clone();
        assert_eq!(
            progress_effect(&fractions, UpdateSource::Dnf, Some("kernel"), None, None),
            ProgressEffect::Touch { index: 1 },
        );
        assert_eq!(
            apply_progress_hint(
                &mut fractions,
                UpdateSource::Dnf,
                Some("kernel"),
                None,
                None
            ),
            Some(1),
        );
        assert_eq!(fractions, touch_snap);

        // Named Completed, Failed, Skipped, and Pending are SetStatus.
        let mut named = vec![dnf("firefox"), dnf("kernel")];
        named[0].status = PackageStatus::Installing;
        named[0].progress = 0.2;
        named[1].progress = 0.55;
        assert_eq!(
            progress_effect(
                &named,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Failed),
                Some(0.2),
            ),
            ProgressEffect::SetStatus {
                index: 1,
                status: PackageStatus::Failed,
                progress: Some(0.2),
            },
        );
        assert_eq!(
            apply_progress_hint(
                &mut named,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Failed),
                Some(0.2),
            ),
            Some(1),
        );
        assert_eq!(named[0].status, PackageStatus::Installing);
        assert_eq!(named[1].status, PackageStatus::Failed);
        assert!((named[1].progress - 0.2).abs() < f64::EPSILON);

        assert_eq!(
            progress_effect(
                &named,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Completed),
                Some(1.0),
            ),
            ProgressEffect::SetStatus {
                index: 0,
                status: PackageStatus::Completed,
                progress: Some(1.0),
            },
        );
        apply_progress_hint(
            &mut named,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Completed),
            Some(1.0),
        );
        assert_eq!(named[0].status, PackageStatus::Completed);
        assert_eq!(named[0].progress, 1.0);

        let mut skipped = vec![dnf("kernel")];
        skipped[0].progress = 0.55;
        assert_eq!(
            progress_effect(
                &skipped,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Skipped),
                None,
            ),
            ProgressEffect::SetStatus {
                index: 0,
                status: PackageStatus::Skipped,
                progress: None,
            },
        );
        assert_eq!(
            apply_progress_hint(
                &mut skipped,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Skipped),
                None,
            ),
            Some(0),
        );
        assert_eq!(skipped[0].status, PackageStatus::Skipped);
        assert!((skipped[0].progress - 0.55).abs() < f64::EPSILON);

        skipped[0].status = PackageStatus::Pending;
        assert_eq!(
            progress_effect(
                &skipped,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Pending),
                Some(-0.4),
            ),
            ProgressEffect::SetStatus {
                index: 0,
                status: PackageStatus::Pending,
                progress: Some(0.0),
            },
        );
        apply_progress_hint(
            &mut skipped,
            UpdateSource::Dnf,
            Some("kernel"),
            Some(PackageStatus::Pending),
            Some(-0.4),
        );
        assert_eq!(skipped[0].status, PackageStatus::Pending);
        assert_eq!(skipped[0].progress, 0.0);
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
    fn run_heading_switches_with_the_stage() {
        assert_eq!(
            run_heading("Downloading packages", RunPass::Download),
            "Downloading"
        );
        assert_eq!(run_heading("Installing", RunPass::Apply), "Applying");
        assert_eq!(
            run_heading("Running transaction", RunPass::Apply),
            "Applying"
        );
        assert_eq!(run_heading("Verifying", RunPass::Apply), "Verifying");
        assert_eq!(run_heading("Starting…", RunPass::Download), "Updating");
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
