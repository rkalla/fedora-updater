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
    /// A check finished with nothing to install, or the user left the Done page.
    /// `detail` is set only when a backend reported a soft error.
    Current {
        checked_at: String,
        detail: Option<String>,
    },
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
        IdleScreen::Current { checked_at, detail } => {
            up_to_date_chrome(checked_at, detail.clone())
        }
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
}

impl PackageStatus {
    pub fn is_done(self) -> bool {
        matches!(self, PackageStatus::Completed | PackageStatus::Failed)
    }

    pub fn label(self) -> &'static str {
        match self {
            PackageStatus::Pending => "queued",
            PackageStatus::Downloading => "downloading",
            PackageStatus::Installing => "installing",
            PackageStatus::Completed => "done",
            PackageStatus::Failed => "failed",
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
    pub size_bytes: Option<u64>,
    pub old_version: Option<String>,
    pub status: PackageStatus,
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
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source: UpdateSource::Dnf,
            detail: format!("{version} · {repo}"),
        }
    }

    pub fn size_label(&self) -> Option<String> {
        self.size_bytes.map(format_bytes)
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
        if let Some(n) = p.size_bytes {
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

    let status = match (sys, apps, fw) {
        (false, _, _) => "System in progress · Apps queued · Firmware queued",
        (true, false, _) => "System done · Apps in progress · Firmware queued",
        (true, true, false) => "System done · Apps done · Firmware in progress",
        (true, true, true) => "System · Apps · Firmware done",
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
        p.status = if ok {
            PackageStatus::Completed
        } else {
            PackageStatus::Failed
        };
    }
}

/// Apply a progress hint. Unnamed lines must not poke the first remaining
/// package — except an explicit Completed status, which firmware uses for
/// "Successfully installed firmware" (no device name on that line).
///
/// Download name-chasing does not complete packages (DNF names every RPM
/// once per download, then again per install/verify). Installing name-switch
/// completes the previous *installing* item only. A hint with no status leaves
/// the package status untouched and returns `Some(index)`.
/// Already-done packages are ignored so verify passes cannot resurrect them.
pub fn apply_progress_hint(
    packages: &mut [Package],
    source: UpdateSource,
    package_hint: Option<&str>,
    status_hint: Option<PackageStatus>,
) -> Option<usize> {
    let Some(hint) = package_hint.map(str::trim).filter(|s| !s.is_empty()) else {
        if status_hint != Some(PackageStatus::Completed) {
            return None;
        }
        let idx = packages
            .iter()
            .position(|p| p.source == source && !p.status.is_done())?;
        packages[idx].status = PackageStatus::Completed;
        return Some(idx);
    };
    // Same RPM name can be i686 and x86_64. The first row may already be done
    // from the other arch; keep looking so the twin still updates.
    let index = packages
        .iter()
        .position(|p| p.source == source && !p.status.is_done() && p.matches_progress_name(hint))?;
    match status_hint {
        Some(PackageStatus::Downloading) => {
            for (i, p) in packages.iter_mut().enumerate() {
                if i != index && p.source == source && p.status == PackageStatus::Downloading {
                    p.status = PackageStatus::Pending;
                }
            }
            packages[index].status = PackageStatus::Downloading;
        }
        Some(PackageStatus::Installing) => {
            for (i, p) in packages.iter_mut().enumerate() {
                if i == index || p.source != source {
                    continue;
                }
                if p.status == PackageStatus::Installing {
                    p.status = PackageStatus::Completed;
                } else if p.status == PackageStatus::Downloading {
                    p.status = PackageStatus::Pending;
                }
            }
            packages[index].status = PackageStatus::Installing;
        }
        None => {}
        Some(status) => {
            packages[index].status = status;
        }
    }
    Some(index)
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
    CacheGrowing,
    Stalled,
    ItemAtFull,
    Flowing,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QuietInput<'a> {
    pub quiet_secs: u64,
    pub at_full_item: bool,
    pub cache_note: Option<&'a str>,
    pub watching_dnf: bool,
    pub daemon_running: bool,
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
}

pub fn quiet_ui(input: QuietInput<'_>) -> QuietUi {
    let cache = input.cache_note.filter(|s| !s.is_empty());
    let show_eta = input.quiet_secs < 15 && cache.is_none();
    let stalled = input.quiet_secs >= 120 && cache.is_none();
    let at_full_spinner = input.quiet_secs >= 5 && input.at_full_item && cache.is_none();
    let show_spinner = cache.is_some() || stalled || at_full_spinner;
    let mode = if cache.is_some() {
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
    } else if stalled {
        Some(if input.watching_dnf {
            "Waiting on dnf".into()
        } else {
            "Waiting for output".into()
        })
    } else {
        None
    };
    let banner = if stalled {
        let mins = (input.quiet_secs / 60).max(2);
        Some(if input.daemon_running {
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
    fn check_step_names_the_cursor() {
        let (frac, groups, status) = check_progress(&[]);
        assert!((frac - 0.05).abs() < 1e-9);
        assert_eq!(groups, 0);
        assert_eq!(status, "System in progress · Apps queued · Firmware queued");

        let (frac, groups, status) = check_progress(&[UpdateSource::Dnf]);
        assert!((frac - 0.25).abs() < 1e-9);
        assert_eq!(groups, 1);
        assert_eq!(status, "System done · Apps in progress · Firmware queued");

        let (frac, groups, status) =
            check_progress(&[UpdateSource::Dnf, UpdateSource::FlatpakUser]);
        assert!((frac - 0.5).abs() < 1e-9);
        assert_eq!(groups, 1);
        assert_eq!(status, "System done · Apps in progress · Firmware queued");

        let (frac, groups, status) = check_progress(&[
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
        ]);
        assert!((frac - 0.75).abs() < 1e-9);
        assert_eq!(groups, 2);
        assert_eq!(status, "System done · Apps done · Firmware in progress");

        let (frac, groups, status) = check_progress(&[
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
            UpdateSource::Firmware,
        ]);
        assert!((frac - 1.0).abs() < 1e-9);
        assert_eq!(groups, 3);
        assert_eq!(status, "System · Apps · Firmware done");

        let (frac, groups, status) =
            check_progress(&[UpdateSource::Dnf, UpdateSource::Firmware]);
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
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
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
        let untouched = pkgs.clone();
        for status in [
            Some(PackageStatus::Pending),
            Some(PackageStatus::Downloading),
            Some(PackageStatus::Installing),
            Some(PackageStatus::Failed),
            None,
        ] {
            for hint in [None, Some(""), Some("   ")] {
                assert!(apply_progress_hint(&mut pkgs, UpdateSource::Dnf, hint, status).is_none());
            }
        }
        assert_eq!(pkgs, untouched);
    }

    fn firmware_pkg(name: &str) -> Package {
        Package {
            id: format!("fwupd:{name}"),
            name: name.into(),
            arch: String::new(),
            version: "1".into(),
            repo: "LVFS".into(),
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source: UpdateSource::Firmware,
            detail: String::new(),
        }
    }

    #[test]
    fn unnamed_completed_hint_finishes_first_open_row_for_source() {
        let dnf = Package::new_dnf("firefox", "x86_64", "1", "updates");
        let mut pkgs = vec![dnf, firmware_pkg("System Firmware"), firmware_pkg("UEFI CA")];
        pkgs[1].status = PackageStatus::Failed;

        let idx = apply_progress_hint(
            &mut pkgs,
            UpdateSource::Firmware,
            None,
            Some(PackageStatus::Completed),
        );
        assert_eq!(idx, Some(2));
        assert_eq!(pkgs[0].status, PackageStatus::Pending);
        assert_eq!(pkgs[1].status, PackageStatus::Failed);
        assert_eq!(pkgs[2].status, PackageStatus::Completed);

        assert!(apply_progress_hint(
            &mut pkgs,
            UpdateSource::Firmware,
            Some("   "),
            Some(PackageStatus::Completed),
        )
        .is_none());
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
        let idx = apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("mesa-dri-drivers"),
            Some(PackageStatus::Installing),
        );
        assert_eq!(idx, Some(1));
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Installing);
    }

    #[test]
    fn quiet_download_hides_eta_and_names_a_growing_cache_file() {
        let fresh = quiet_ui(QuietInput {
            quiet_secs: 2,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(fresh.mode, QuietMode::Flowing);
        assert!(fresh.show_eta);
        assert!(!fresh.show_spinner);
        assert!(fresh.banner.is_none());

        let stuck_full = quiet_ui(QuietInput {
            quiet_secs: 8,
            at_full_item: true,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(stuck_full.mode, QuietMode::ItemAtFull);
        assert!(stuck_full.show_spinner);
        assert!(stuck_full.show_eta);

        let eta_hidden = quiet_ui(QuietInput {
            quiet_secs: 15,
            at_full_item: false,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(eta_hidden.mode, QuietMode::Flowing);
        assert!(!eta_hidden.show_eta);
        assert!(!eta_hidden.show_spinner);

        let growing = quiet_ui(QuietInput {
            quiet_secs: 400,
            at_full_item: true,
            cache_note: Some("Downloading grok-bot — 85 MB and rising"),
            watching_dnf: true,
            daemon_running: false,
        });
        assert_eq!(growing.mode, QuietMode::CacheGrowing);
        assert!(!growing.show_eta);
        assert!(growing.show_spinner);
        assert_eq!(
            growing.footnote.as_deref(),
            Some("Downloading grok-bot — 85 MB and rising")
        );
        assert!(growing.banner.is_none());

        let stalled = quiet_ui(QuietInput {
            quiet_secs: 180,
            at_full_item: true,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(stalled.mode, QuietMode::Stalled);
        assert!(!stalled.show_eta);
        assert!(stalled.show_spinner);
        assert_eq!(stalled.footnote.as_deref(), Some("Waiting on dnf"));
        let stalled_banner = stalled.banner.expect("stalled banner");
        assert_eq!(stalled_banner, "No output for 3 minutes.");
        assert!(!stalled_banner.contains("Last line:"));

        let flatpak_stalled = quiet_ui(QuietInput {
            quiet_secs: 180,
            at_full_item: false,
            watching_dnf: false,
            ..Default::default()
        });
        assert_eq!(flatpak_stalled.mode, QuietMode::Stalled);
        assert_eq!(
            flatpak_stalled.footnote.as_deref(),
            Some("Waiting for output")
        );
        assert_eq!(
            flatpak_stalled.banner.as_deref(),
            Some("No output for 3 minutes.")
        );
    }

    #[test]
    fn stalled_banner_names_the_daemon_only_when_asked() {
        let on = quiet_ui(QuietInput {
            quiet_secs: 180,
            at_full_item: true,
            watching_dnf: true,
            daemon_running: true,
            ..Default::default()
        });
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

        let off = quiet_ui(QuietInput {
            quiet_secs: 180,
            at_full_item: true,
            watching_dnf: true,
            daemon_running: false,
            ..Default::default()
        });
        let off_banner = off.banner.expect("stalled banner");
        assert_eq!(off_banner, "No output for 3 minutes.");
        assert!(
            !off_banner.contains("Waiting on GNOME Software"),
            "{off_banner}"
        );
        assert!(!off_banner.contains("dnf daemon"), "{off_banner}");
        assert!(!off_banner.contains("Last line:"), "{off_banner}");
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
        );
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("kernel"),
            Some(PackageStatus::Installing),
        );
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Installing);
    }

    #[test]
    fn statusless_hint_returns_index_without_changing_rows() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
        );
        let idx = apply_progress_hint(&mut pkgs, UpdateSource::Dnf, Some("kernel"), None);
        assert_eq!(idx, Some(1));
        assert_eq!(pkgs[0].status, PackageStatus::Installing);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
    }

    #[test]
    fn download_and_install_hints_isolate_sources_and_reset_siblings() {
        let mut flatpak = Package::new_dnf("Thunderbird", "", "128", "flathub");
        flatpak.source = UpdateSource::FlatpakUser;
        flatpak.status = PackageStatus::Downloading;

        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("wget", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
            flatpak,
        ];
        assert_eq!(
            apply_progress_hint(
                &mut pkgs,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Downloading),
            ),
            Some(0)
        );
        assert_eq!(pkgs[0].status, PackageStatus::Downloading);
        assert_eq!(pkgs[3].status, PackageStatus::Downloading);

        assert_eq!(
            apply_progress_hint(
                &mut pkgs,
                UpdateSource::Dnf,
                Some("wget"),
                Some(PackageStatus::Downloading),
            ),
            Some(1)
        );
        assert_eq!(pkgs[0].status, PackageStatus::Pending);
        assert_eq!(pkgs[1].status, PackageStatus::Downloading);
        assert_eq!(pkgs[3].status, PackageStatus::Downloading);

        assert_eq!(
            apply_progress_hint(
                &mut pkgs,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Installing),
            ),
            Some(0)
        );
        assert_eq!(pkgs[0].status, PackageStatus::Installing);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
        assert_eq!(pkgs[3].status, PackageStatus::Downloading);

        assert_eq!(
            apply_progress_hint(
                &mut pkgs,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Failed),
            ),
            Some(2)
        );
        assert_eq!(pkgs[2].status, PackageStatus::Failed);
    }

    #[test]
    fn done_package_is_not_resurrected_as_current() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        pkgs[0].status = PackageStatus::Completed;
        assert!(apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
        )
        .is_none());
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
    }

    #[test]
    fn source_summary_line_appends_rolled_up_download_size() {
        let mut kernel = Package::new_dnf("kernel-core", "x86_64", "1", "updates");
        kernel.size_bytes = Some(21_727_761);
        assert_eq!(kernel.size_label().as_deref(), Some("21 MB"));
        let mut fp = Package::new_dnf("Thunderbird", "", "128", "flathub");
        fp.source = UpdateSource::FlatpakUser;
        fp.size_bytes = Some(164 * 1024 * 1024);
        assert_eq!(fp.size_label().as_deref(), Some("164 MB"));
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
        assert_eq!(total_download_bytes(std::slice::from_ref(&pkg)), None);
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
