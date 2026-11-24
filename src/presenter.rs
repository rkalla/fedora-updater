//! Pure UI presentation helpers and view-model copy (no GTK).

use crate::backend::dnf::package_implies_reboot;
use crate::state::Phase;
use crate::model::{
    count_by_source, format_bytes, total_download_bytes, ApplyMode, AuthPurpose, IdleScreen,
    Package, PackageStatus, RetryAction, RunPass, SourceCategory, UpdateSource,
};

pub const ABOUT_NAME: &str = "Fedora Updater";
pub const ABOUT_COMMENTS: &str =
    "A desktop front-end for Fedora system, Flatpak, and firmware updates";
pub const AUTH_DESCRIPTION: &str =
    "A system dialog asks for an administrator password. The password is not stored in this window.";

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
    pub show_view_log: bool,
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
            show_view_log: false,
            description: Some(
                "System packages, apps, and firmware. Nothing changes until the list is reviewed."
                    .into(),
            ),
        },
        IdleScreen::Current { checked_at, detail } => {
            up_to_date_chrome(checked_at, detail.clone())
        }
        IdleScreen::Warning { title, detail, .. } => IdleChrome {
            title,
            icon: "dialog-warning-symbolic",
            button: "Check Again",
            suggested: true,
            pill: true,
            flat: false,
            show_view_log: true,
            description: Some(detail.clone()),
        },
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
        show_view_log: false,
        description: Some(detail.unwrap_or_else(|| format!("Last checked {checked_at}"))),
    }
}

/// Ready-view caption: `38 system · 0 apps · 0 firmware · 1.2 GB`.
pub fn source_summary_line(packages: &[Package]) -> String {
    let (dnf, fp, fw) = count_by_source(packages);
    let counts = crate::model::count_by_category(packages);
    let dev = counts
        .get(&crate::model::SourceCategory::DevAndContainers)
        .copied()
        .unwrap_or(0);
    let extensions = counts
        .get(&crate::model::SourceCategory::Extensions)
        .copied()
        .unwrap_or(0);
    let mut parts = vec![
        format!("{dnf} system"),
        format!("{fp} apps"),
        format!("{fw} firmware"),
    ];
    if dev > 0 {
        parts.push(format!("{dev} dev"));
    }
    if extensions > 0 {
        parts.push(format!("{extensions} extensions"));
    }
    if let Some(bytes) = total_download_bytes(packages).filter(|b| *b > 0) {
        parts.push(format_bytes(bytes));
    }
    parts.join(" · ")
}

/// Check progress from finished backends. Three user-facing groups:
/// System (DNF), Apps (both Flatpak scopes), Firmware.
pub fn check_progress(checked: &[UpdateSource]) -> (f64, u32, &'static str) {
    let sys = checked.contains(&UpdateSource::Dnf) || checked.contains(&UpdateSource::RpmOstree);
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

/// Source-config-aware check progress grouped by active [`SourceCategory`].
pub fn check_progress_for_sources(
    checked: &[UpdateSource],
    enabled: &[UpdateSource],
) -> (f64, u32, u32, String) {
    if enabled.is_empty() {
        return (1.0, 0, 0, "No update sources enabled".into());
    }

    let mut active_categories = Vec::new();
    for &cat in SourceCategory::ALL {
        if enabled.iter().any(|s| s.category() == cat) {
            active_categories.push(cat);
        }
    }

    let groups_total = active_categories.len() as u32;
    let done_backends = enabled.iter().filter(|s| checked.contains(s)).count();
    let fraction = (done_backends as f64 / enabled.len() as f64).clamp(0.05, 1.0);

    let mut category_done = Vec::with_capacity(active_categories.len());
    let mut groups_done = 0u32;
    for &cat in &active_categories {
        let is_done = enabled
            .iter()
            .filter(|s| s.category() == cat)
            .all(|s| checked.contains(s));
        if is_done {
            groups_done += 1;
        }
        category_done.push((cat, is_done));
    }

    let status = if groups_done == groups_total {
        let labels: Vec<&str> = active_categories.iter().map(|c| c.label()).collect();
        format!("{} done", labels.join(" · "))
    } else {
        let mut saw_in_progress = false;
        let parts: Vec<String> = category_done
            .into_iter()
            .map(|(cat, done)| {
                if done {
                    format!("{} done", cat.label())
                } else if !saw_in_progress {
                    saw_in_progress = true;
                    format!("{} in progress", cat.label())
                } else {
                    format!("{} queued", cat.label())
                }
            })
            .collect();
        parts.join(" · ")
    };

    (fraction, groups_done, groups_total, status)
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
    pub watching_ostree: bool,
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
        Some(if input.watching_ostree {
            "Waiting on rpm-ostree".into()
        } else if input.watching_dnf {
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

/// Title and subtitle for the one shared header. `narrow` blanks the subtitle on every phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderChrome {
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Copy)]
pub enum HeaderView<'a> {
    Idle,
    Authenticating(AuthPurpose),
    Checking { footnote: &'a str },
    Ready { count: usize, summary: &'a str },
    Running { heading: &'a str, footnote: &'a str },
    Done { failed: bool, subtitle: &'a str },
    Failed,
    ActivityLog { subtitle: &'a str },
    History { subtitle: &'a str },
}

pub fn window_header(view: HeaderView<'_>, narrow: bool) -> HeaderChrome {
    let (title, subtitle): (String, String) = match view {
        HeaderView::Idle | HeaderView::Failed => ("Fedora Updater".into(), String::new()),
        HeaderView::Authenticating(AuthPurpose::Check) => {
            ("Fedora Updater".into(), "Checking for updates".into())
        }
        HeaderView::Authenticating(AuthPurpose::Apply) => {
            ("Fedora Updater".into(), "Applying updates".into())
        }
        HeaderView::Checking { footnote } => ("Checking for Updates".into(), footnote.to_string()),
        HeaderView::Ready { count, summary } => {
            let title = if count == 1 {
                "1 Update Available".into()
            } else {
                format!("{count} Updates Available")
            };
            (title, summary.to_string())
        }
        HeaderView::Running { heading, footnote } => (heading.to_string(), footnote.to_string()),
        HeaderView::Done { failed, subtitle } => {
            let title = if failed {
                "Updates Finished With Problems"
            } else {
                "All Updates Applied"
            };
            (title.into(), subtitle.to_string())
        }
        HeaderView::ActivityLog { subtitle } => ("Activity Log".into(), subtitle.to_string()),
        HeaderView::History { subtitle } => ("Update History".into(), subtitle.to_string()),
    };
    HeaderChrome {
        title,
        subtitle: if narrow { String::new() } else { subtitle },
    }
}

pub fn history_subtitle(matched: usize, total: usize) -> String {
    if total == 0 {
        "No recorded transactions".into()
    } else if matched == total {
        if total == 1 {
            "1 transaction".into()
        } else {
            format!("{total} transactions")
        }
    } else {
        format!("{matched} of {total} transactions")
    }
}

/// Primary apply CTA label incorporating execution mode, selected subset, and size.
pub fn apply_cta_label(
    mode: ApplyMode,
    selected: usize,
    total: usize,
    selected_bytes: Option<u64>,
) -> String {
    let all_selected = selected == total || total == 0;
    let base = match mode {
        ApplyMode::Live => {
            if all_selected {
                "Update All".to_string()
            } else {
                format!("Update {selected} of {total}")
            }
        }
        ApplyMode::LiveAndReboot => {
            if all_selected {
                "Update & Restart".to_string()
            } else {
                format!("Update {selected} & Restart")
            }
        }
        ApplyMode::OfflineOnReboot => {
            if all_selected {
                "Prepare Offline Update".to_string()
            } else {
                format!("Prepare {selected} Offline")
            }
        }
        ApplyMode::DownloadOnly => {
            if all_selected {
                "Download Only".to_string()
            } else {
                format!("Download {selected}")
            }
        }
    };
    if selected > 0 {
        if let Some(bytes) = selected_bytes.filter(|b| *b > 0) {
            return format!("{base} · {}", format_bytes(bytes));
        }
    }
    base
}

/// Concise selection summary rendered alongside the "Toggle All Selected" control.
pub fn selection_summary_text(
    selected: usize,
    total: usize,
    selected_bytes: Option<u64>,
) -> String {
    if total == 0 {
        return "0 selected".into();
    }
    let base = format!("{selected} of {total} selected");
    if selected > 0 {
        if let Some(bytes) = selected_bytes.filter(|b| *b > 0) {
            return format!("{base} · {}", format_bytes(bytes));
        }
    }
    base
}

/// Formats signed installed size delta (`+1.4 MB`, `-240 KB`, `0 B`).
pub fn format_size_delta(delta: i64) -> String {
    if delta == 0 {
        "0 B".into()
    } else if delta > 0 {
        format!("+{}", format_bytes(delta as u64))
    } else {
        format!("-{}", format_bytes(delta.unsigned_abs()))
    }
}

pub fn activity_log_subtitle(matched: usize, total: usize) -> String {
    if total == 0 {
        "No output yet".into()
    } else if matched == total {
        if total == 1 {
            "1 line".into()
        } else {
            format!("{total} lines")
        }
    } else {
        format!("{matched} of {total} lines")
    }
}

pub fn checking_footnote(groups_done: u32, eta: Option<&str>) -> String {
    checking_footnote_of(groups_done, 3, eta)
}

pub fn checking_footnote_of(groups_done: u32, groups_total: u32, eta: Option<&str>) -> String {
    let noun = if groups_total == 1 {
        "source"
    } else {
        "sources"
    };
    match eta {
        Some(eta) if !eta.is_empty() => format!("{eta} · {groups_done} of {groups_total} {noun}"),
        _ => format!("{groups_done} of {groups_total} {noun}"),
    }
}

pub fn done_header_subtitle(
    failed: usize,
    upgraded: usize,
    dnf: usize,
    flatpak: usize,
    firmware: usize,
    duration: &str,
) -> String {
    if failed > 0 {
        format!("{upgraded} applied · {failed} failed · {duration}")
    } else {
        format!("{upgraded} upgraded · {dnf} system / {flatpak} apps / {firmware} firmware · {duration}")
    }
}

pub fn done_header_subtitle_for_packages(
    failed: usize,
    upgraded: usize,
    packages: &[Package],
    duration: &str,
) -> String {
    if failed > 0 {
        return format!("{upgraded} applied · {failed} failed · {duration}");
    }
    let (dnf, flatpak, firmware) = count_by_source(packages);
    let counts = crate::model::count_by_category(packages);
    let dev = counts
        .get(&crate::model::SourceCategory::DevAndContainers)
        .copied()
        .unwrap_or(0);
    let extensions = counts
        .get(&crate::model::SourceCategory::Extensions)
        .copied()
        .unwrap_or(0);
    if dev == 0 && extensions == 0 {
        return done_header_subtitle(failed, upgraded, dnf, flatpak, firmware, duration);
    }
    let mut breakdown = vec![
        format!("{dnf} system"),
        format!("{flatpak} apps"),
        format!("{firmware} firmware"),
    ];
    if dev > 0 {
        breakdown.push(format!("{dev} dev"));
    }
    if extensions > 0 {
        breakdown.push(format!("{extensions} extensions"));
    }
    format!(
        "{upgraded} upgraded · {} · {duration}",
        breakdown.join(" / ")
    )
}

pub fn soft_error_banner_title(count: usize) -> String {
    if count == 1 {
        "1 source could not be checked".into()
    } else {
        format!("{count} sources could not be checked")
    }
}

/// Banner title shown on the Ready page when `soft_errors` is non-empty.
pub fn ready_banner_title(soft_errors: &[String]) -> Option<String> {
    if soft_errors.is_empty() {
        return None;
    }
    if soft_errors.len() == 1 && soft_errors[0].starts_with("Stopped before install") {
        return Some(soft_errors[0].clone());
    }
    Some(soft_error_banner_title(soft_errors.len()))
}

pub fn fail_retry_label(retry: RetryAction) -> &'static str {
    match retry {
        RetryAction::CheckAgain => "Check Again",
        RetryAction::RetryApply => "Retry Update",
        RetryAction::Reboot => "Restart",
    }
}

/// Copy the two alert dialogs pass to `Adw.AlertDialog`. Response ids stay stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogCopy {
    pub heading: &'static str,
    pub body: &'static str,
    pub close_id: &'static str,
    pub close_label: &'static str,
    pub confirm_id: &'static str,
    pub confirm_label: &'static str,
    pub confirm_destructive: bool,
    pub confirm_suggested: bool,
    pub default_id: &'static str,
}

pub fn stale_update_dialog() -> DialogCopy {
    DialogCopy {
        heading: "Refresh Update List?",
        body: "This list is more than a day old. Refresh it before applying these updates.",
        close_id: "no",
        close_label: "_Cancel",
        confirm_id: "yes",
        confirm_label: "_Refresh",
        confirm_destructive: false,
        confirm_suggested: true,
        default_id: "yes",
    }
}

pub fn stop_update_dialog() -> DialogCopy {
    DialogCopy {
        heading: "Stop This Update?",
        body: "Stopping while packages are being installed can leave the package database incomplete. Check package history before trying again.",
        close_id: "wait",
        close_label: "_Keep Waiting",
        confirm_id: "stop",
        confirm_label: "Stop _Anyway",
        confirm_destructive: true,
        confirm_suggested: false,
        default_id: "wait",
    }
}

/// Four-stage pipeline breadcrumb rendered on Observatory's `Running` telemetry deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PipelineStage {
    Download = 0,
    Prepare = 1,
    Install = 2,
    Verify = 3,
}

impl PipelineStage {
    pub const ALL: [Self; 4] = [Self::Download, Self::Prepare, Self::Install, Self::Verify];

    pub fn label(self) -> &'static str {
        match self {
            Self::Download => "Download",
            Self::Prepare => "Prepare",
            Self::Install => "Install",
            Self::Verify => "Verify",
        }
    }

    pub fn done_label(self) -> &'static str {
        match self {
            Self::Download => "Download ✓",
            Self::Prepare => "Prepare ✓",
            Self::Install => "Install ✓",
            Self::Verify => "Verify ✓",
        }
    }
}

/// Maps backend `phase_label` + [`RunPass`] onto the 4-stage pipeline breadcrumb.
pub fn pipeline_stage(phase_label: &str, pass: RunPass) -> PipelineStage {
    let label = phase_label.to_ascii_lowercase();
    if label.contains("verif")
        || label.contains("cleanup")
        || label.contains("complete")
        || label.contains("installed")
        || label.contains("restarting")
        || label.contains("staged")
    {
        PipelineStage::Verify
    } else if label.contains("prepar")
        || label.contains("decompress")
        || label.contains("running transaction")
        || label.contains("checking out")
        || label.contains("writing ostree")
        || label.contains("overlaying")
        || label.contains("waiting")
    {
        PipelineStage::Prepare
    } else if pass == RunPass::Apply
        || label.contains("install")
        || label.contains("upgrad")
        || label.contains("eras")
        || label.contains("remov")
        || label.contains("staging")
        || label.contains("updating flatpak")
    {
        PipelineStage::Install
    } else {
        PipelineStage::Download
    }
}

/// Formats an elapsed duration in seconds cleanly (`42s`, `3m 05s`).
pub fn format_elapsed(secs: u64) -> String {
    let mins = secs / 60;
    let rem = secs % 60;
    if mins == 0 {
        format!("{rem}s")
    } else {
        format!("{mins}m {rem:02}s")
    }
}

/// Returns deduplicated non-failed package names responsible for a reboot recommendation.
pub fn reboot_trigger_names(packages: &[Package]) -> Vec<String> {
    let mut names = Vec::new();
    for pkg in packages {
        if pkg.status != PackageStatus::Failed
            && pkg.source == UpdateSource::Dnf
            && package_implies_reboot(&pkg.name)
            && !names.contains(&pkg.name)
        {
            names.push(pkg.name.clone());
        }
    }
    if names.is_empty() {
        for pkg in packages {
            if pkg.status != PackageStatus::Failed
                && pkg.source == UpdateSource::Firmware
                && !names.contains(&pkg.name)
            {
                names.push(pkg.name.clone());
            }
        }
    }
    names
}

/// Human-readable description for the `Done` Restart Recommended card.
pub fn reboot_trigger_summary(packages: &[Package]) -> String {
    let names = reboot_trigger_names(packages);
    match names.as_slice() {
        [] => {
            if packages
                .iter()
                .any(|p| p.status != PackageStatus::Failed && p.source == UpdateSource::RpmOstree)
            {
                "Restart to boot into the staged atomic system deployment.".to_string()
            } else {
                "Restart to finish applying system updates.".to_string()
            }
        }
        [a] => format!("{a} requires a restart to take effect."),
        [a, b] => format!("{a} and {b} require a restart to take effect."),
        [a, b, c] => format!("{a}, {b}, and {c} require a restart to take effect."),
        [a, b, c, rest @ ..] => format!(
            "{a}, {b}, {c}, and {} more require a restart to take effect.",
            rest.len()
        ),
    }
}

/// Non-blocking session restart note shown when GNOME Shell extensions are updated.
pub const GNOME_EXT_RELOGIN_NOTE: &str =
    "Log out and back in to activate updated GNOME Shell extensions.";

/// True when at least one GNOME Shell extension update finished cleanly.
pub fn has_completed_gnome_extensions(packages: &[Package]) -> bool {
    packages
        .iter()
        .any(|p| p.source == UpdateSource::GnomeExtensions && p.status == PackageStatus::Completed)
}

/// Concise summary body for the clean `Done` closure card.
pub fn done_clean_summary(upgraded: usize, duration: &str) -> String {
    let noun = if upgraded == 1 { "update" } else { "updates" };
    format!("{upgraded} {noun} installed in {duration}. No restart needed.")
}

/// Concise summary body for the clean `Done` closure card, noting re-login when
/// GNOME Shell extensions were updated.
pub fn done_clean_summary_for_packages(
    upgraded: usize,
    packages: &[Package],
    duration: &str,
) -> String {
    if has_completed_gnome_extensions(packages) {
        let noun = if upgraded == 1 { "update" } else { "updates" };
        format!("{upgraded} {noun} installed in {duration}. {GNOME_EXT_RELOGIN_NOTE}")
    } else {
        done_clean_summary(upgraded, duration)
    }
}

/// Fixed desktop notification ID replaced/withdrawn across check and apply cycles.
pub const COMPLETION_NOTIFICATION_ID: &str = "fedora-updater-status";

/// Planned desktop completion notification emitted when the window is unfocused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopNotification {
    pub id: &'static str,
    pub title: String,
    pub body: String,
}

/// Plan a desktop notification right after Check or Apply completes.
pub fn phase_completion_notification(
    phase: &Phase,
    just_finished_check: bool,
    just_finished_apply: bool,
) -> Option<DesktopNotification> {
    if just_finished_check {
        if let Phase::Ready { packages, .. } = phase {
            if !packages.is_empty() {
                let count = if packages.len() == 1 {
                    "1 update available".to_string()
                } else {
                    format!("{} updates available", packages.len())
                };
                let summary = source_summary_line(packages);
                let body = if summary.is_empty() {
                    count
                } else {
                    format!("{count} ({summary})")
                };
                return Some(DesktopNotification {
                    id: COMPLETION_NOTIFICATION_ID,
                    title: "Updates Ready to Review".into(),
                    body,
                });
            }
        }
        return None;
    }

    if !just_finished_apply {
        return None;
    }

    match phase {
        Phase::Done {
            packages,
            needs_reboot,
            failed,
            ..
        } => {
            if *failed > 0 {
                let total = packages.len();
                let body = if *needs_reboot {
                    format!("{failed} of {total} updates failed. Restart recommended for applied updates.")
                } else {
                    format!("{failed} of {total} updates failed. Review the Activity Log for details.")
                };
                Some(DesktopNotification {
                    id: COMPLETION_NOTIFICATION_ID,
                    title: "Updates Finished With Problems".into(),
                    body,
                })
            } else if *needs_reboot {
                Some(DesktopNotification {
                    id: COMPLETION_NOTIFICATION_ID,
                    title: "Updates Installed — Restart Recommended".into(),
                    body: reboot_trigger_summary(packages),
                })
            } else {
                let upgraded = packages
                    .iter()
                    .filter(|p| p.status == PackageStatus::Completed)
                    .count();
                let noun = if upgraded == 1 { "update" } else { "updates" };
                let body = if has_completed_gnome_extensions(packages) {
                    format!("{upgraded} {noun} applied cleanly. {GNOME_EXT_RELOGIN_NOTE}")
                } else {
                    format!("{upgraded} {noun} applied cleanly.")
                };
                Some(DesktopNotification {
                    id: COMPLETION_NOTIFICATION_ID,
                    title: "Updates Installed".into(),
                    body,
                })
            }
        }
        Phase::Failed { detail, .. } => Some(DesktopNotification {
            id: COMPLETION_NOTIFICATION_ID,
            title: "Update Failed".into(),
            body: detail.clone(),
        }),
        _ => None,
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
        assert!(!chrome.show_view_log);
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
        assert!(!chrome.show_view_log);
        assert_eq!(
            chrome.description.as_deref(),
            Some(
                "System packages, apps, and firmware. Nothing changes until the list is reviewed."
            )
        );
    }

    #[test]
    fn idle_chrome_warning_surfaces_problems_and_view_log() {
        let chrome = idle_chrome(&IdleScreen::Warning {
            title: "Could not check all sources",
            detail: "Some sources could not be checked: dnf failed".into(),
            checked_at: Some("just now".into()),
        });
        assert_ne!(chrome.title, "System is up to date");
        assert_eq!(chrome.title, "Could not check all sources");
        assert_eq!(chrome.icon, "dialog-warning-symbolic");
        assert_eq!(chrome.button, "Check Again");
        assert!(chrome.suggested);
        assert!(chrome.pill);
        assert!(!chrome.flat);
        assert!(chrome.show_view_log);
        assert_eq!(
            chrome.description.as_deref(),
            Some("Some sources could not be checked: dnf failed")
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
    fn check_step_names_the_cursor() {
        let (frac, groups, status) = check_progress(&[]);
        assert!((frac - 0.05).abs() < 1e-9);
        assert_eq!(groups, 0);
        assert_eq!(status, "System in progress · Apps queued · Firmware queued");

        let (frac, groups, status) = check_progress(&[UpdateSource::FlatpakUser]);
        assert!((frac - 0.25).abs() < 1e-9);
        assert_eq!(groups, 0);
        assert_eq!(status, "System in progress · Apps queued · Firmware queued");

        let (frac, groups, status) =
            check_progress(&[UpdateSource::FlatpakUser, UpdateSource::FlatpakSystem]);
        assert!((frac - 0.5).abs() < 1e-9);
        assert_eq!(groups, 1);
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
        assert_eq!(about_time_left(4, 0.5), None);
        assert_eq!(about_time_left(5, 0.079), None);
        assert_eq!(
            about_time_left(5, 0.08).as_deref(),
            Some("About 1 minute left")
        );
        assert_eq!(
            about_time_left(44, 0.5).as_deref(),
            Some("Less than a minute left")
        );
        assert_eq!(
            about_time_left(45, 0.5).as_deref(),
            Some("About 1 minute left")
        );
        assert_eq!(
            about_time_left(89, 0.5).as_deref(),
            Some("About 1 minute left")
        );
        assert_eq!(
            about_time_left(90, 0.5).as_deref(),
            Some("About 2 minutes left")
        );
        assert_eq!(
            about_time_left(60, 0.25).as_deref(),
            Some("About 3 minutes left")
        );
        assert_eq!(
            about_time_left(30, 1.0).as_deref(),
            Some("Less than a minute left")
        );
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

        // None source fallback and done > total saturating remaining count.
        let (foot, meta) = running_progress_captions(None, 5, 3, RunPass::Download);
        assert_eq!(foot, "all sources · 5 of 3 downloaded");
        assert_eq!(meta, "5 downloaded · 0 remaining");
    }

    #[test]
    fn quiet_download_hides_eta_and_names_a_growing_cache_file() {
        let fresh = quiet_ui(QuietInput {
            quiet_secs: 4,
            at_full_item: true,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(fresh.mode, QuietMode::Flowing);
        assert!(fresh.show_eta);
        assert!(!fresh.show_spinner);
        assert!(fresh.banner.is_none());

        // Exact 5s boundary triggers ItemAtFull when at_full_item is true.
        let stuck_full = quiet_ui(QuietInput {
            quiet_secs: 5,
            at_full_item: true,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(stuck_full.mode, QuietMode::ItemAtFull);
        assert!(stuck_full.show_spinner);
        assert!(stuck_full.show_eta);

        // Exact 14s vs 15s ETA visibility cutoff.
        let eta_visible = quiet_ui(QuietInput {
            quiet_secs: 14,
            cache_note: Some(""),
            ..Default::default()
        });
        assert!(eta_visible.show_eta);
        assert_eq!(eta_visible.mode, QuietMode::Flowing);

        let eta_hidden = quiet_ui(QuietInput {
            quiet_secs: 15,
            at_full_item: false,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(eta_hidden.mode, QuietMode::Flowing);
        assert!(!eta_hidden.show_eta);
        assert!(!eta_hidden.show_spinner);

        // Exact 119s vs 120s Stalled cutoff.
        let almost_stalled = quiet_ui(QuietInput {
            quiet_secs: 119,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(almost_stalled.mode, QuietMode::Flowing);
        assert!(!almost_stalled.show_spinner);

        let just_stalled = quiet_ui(QuietInput {
            quiet_secs: 120,
            watching_dnf: true,
            ..Default::default()
        });
        assert_eq!(just_stalled.mode, QuietMode::Stalled);
        assert_eq!(just_stalled.banner.as_deref(), Some("No output for 2 minutes."));

        // CacheGrowing takes priority even at quiet_secs == 0 or >= 120.
        let growing_immediately = quiet_ui(QuietInput {
            quiet_secs: 0,
            cache_note: Some("Downloading rpm — 1 MB"),
            ..Default::default()
        });
        assert_eq!(growing_immediately.mode, QuietMode::CacheGrowing);
        assert!(!growing_immediately.show_eta);
        assert!(growing_immediately.show_spinner);

        let growing = quiet_ui(QuietInput {
            quiet_secs: 400,
            at_full_item: true,
            cache_note: Some("Downloading grok-bot — 85 MB and rising"),
            watching_dnf: true,
            watching_ostree: false,
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

        let ostree_stalled = quiet_ui(QuietInput {
            quiet_secs: 180,
            at_full_item: false,
            watching_ostree: true,
            ..Default::default()
        });
        assert_eq!(ostree_stalled.mode, QuietMode::Stalled);
        assert_eq!(
            ostree_stalled.footnote.as_deref(),
            Some("Waiting on rpm-ostree")
        );

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
    fn source_summary_line_appends_rolled_up_download_size() {
        let mut kernel = Package::new_dnf("kernel-core", "x86_64", "1", "updates");
        kernel.size_bytes = Some(21_727_761);
        assert_eq!(kernel.size_label().as_deref(), Some("21 MB"));
        let mut fp = Package::new_dnf("Thunderbird", "", "128", "flathub");
        fp.source = UpdateSource::FlatpakUser;
        fp.size_bytes = Some(164 * 1024 * 1024);
        assert_eq!(fp.size_label().as_deref(), Some("164 MB"));
        let line = source_summary_line(&[kernel, fp]);
        assert_eq!(line, "1 system · 1 apps · 0 firmware · 185 MB");

        // Zero known bytes should omit the size suffix rather than printing "0 B".
        let mut fp_sys = Package::new_dnf("Steam", "", "1", "flathub");
        fp_sys.source = UpdateSource::FlatpakSystem;
        fp_sys.size_bytes = Some(0);
        let mut fw = Package::new_dnf("UEFI", "", "1", "LVFS");
        fw.source = UpdateSource::Firmware;
        assert_eq!(
            source_summary_line(&[fp_sys, fw]),
            "0 system · 1 apps · 1 firmware"
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
    fn run_heading_switches_with_the_stage() {
        assert_eq!(
            run_heading("Downloading packages", RunPass::Download),
            "Downloading"
        );
        assert_eq!(
            run_heading("Downloading packages", RunPass::Apply),
            "Applying"
        );
        assert_eq!(run_heading("Upgrading", RunPass::Download), "Applying");
        assert_eq!(run_heading("Installing", RunPass::Apply), "Applying");
        assert_eq!(
            run_heading("Running transaction", RunPass::Apply),
            "Applying"
        );
        assert_eq!(run_heading("Verifying", RunPass::Download), "Verifying");
        assert_eq!(run_heading("Verifying firmware", RunPass::Apply), "Verifying");
        assert_eq!(run_heading("Starting…", RunPass::Download), "Updating");
    }

    #[test]
    fn now_playing_prefers_named_package() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        assert_eq!(
            now_playing_title(&pkgs, Some(1), Some("kernel"), "Installing"),
            "kernel"
        );
        // Completed or out-of-bounds active_index falls back cleanly.
        pkgs[0].status = crate::model::PackageStatus::Completed;
        assert_eq!(
            now_playing_title(&pkgs, Some(0), Some("fallback"), "Installing"),
            "fallback"
        );
        assert_eq!(
            now_playing_title(&pkgs, Some(99), Some("  "), "Downloading packages"),
            "Downloading packages"
        );
        assert_eq!(now_playing_title(&pkgs, None, None, "   "), "Starting…");

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
        assert_eq!(now_playing_subtitle("kernel", "Installing", None), "Installing");
        assert_eq!(now_playing_subtitle("kernel", "", None), "");
    }

    #[test]
    fn header_titles_follow_the_phase_table() {
        let cases = [
            (HeaderView::Idle, "Fedora Updater", ""),
            (
                HeaderView::Authenticating(AuthPurpose::Check),
                "Fedora Updater",
                "Checking for updates",
            ),
            (
                HeaderView::Authenticating(AuthPurpose::Apply),
                "Fedora Updater",
                "Applying updates",
            ),
            (
                HeaderView::Checking {
                    footnote: "2 of 3 sources",
                },
                "Checking for Updates",
                "2 of 3 sources",
            ),
            (
                HeaderView::Checking {
                    footnote: "about 1 minute · 1 of 3 sources",
                },
                "Checking for Updates",
                "about 1 minute · 1 of 3 sources",
            ),
            (
                HeaderView::Ready {
                    count: 0,
                    summary: "0 system · 0 apps · 0 firmware",
                },
                "0 Updates Available",
                "0 system · 0 apps · 0 firmware",
            ),
            (
                HeaderView::Ready {
                    count: 1,
                    summary: "1 system · 0 apps · 0 firmware",
                },
                "1 Update Available",
                "1 system · 0 apps · 0 firmware",
            ),
            (
                HeaderView::Ready {
                    count: 4,
                    summary: "2 system · 2 apps · 0 firmware",
                },
                "4 Updates Available",
                "2 system · 2 apps · 0 firmware",
            ),
            (
                HeaderView::Running {
                    heading: "Downloading",
                    footnote: "System · 4 of 19 downloaded",
                },
                "Downloading",
                "System · 4 of 19 downloaded",
            ),
            (
                HeaderView::Done {
                    failed: false,
                    subtitle: "12 upgraded · 10 system / 2 apps / 0 firmware · 4 minutes",
                },
                "All Updates Applied",
                "12 upgraded · 10 system / 2 apps / 0 firmware · 4 minutes",
            ),
            (
                HeaderView::Done {
                    failed: true,
                    subtitle: "8 applied · 1 failed · 4 minutes",
                },
                "Updates Finished With Problems",
                "8 applied · 1 failed · 4 minutes",
            ),
            (HeaderView::Failed, "Fedora Updater", ""),
            (
                HeaderView::ActivityLog {
                    subtitle: "4 of 12 lines",
                },
                "Activity Log",
                "4 of 12 lines",
            ),
        ];
        assert_eq!(activity_log_subtitle(0, 0), "No output yet");
        assert_eq!(activity_log_subtitle(1, 1), "1 line");
        assert_eq!(activity_log_subtitle(12, 12), "12 lines");
        assert_eq!(activity_log_subtitle(4, 12), "4 of 12 lines");
        for (view, title, subtitle) in cases {
            let wide = window_header(view, false);
            assert_eq!(wide.title, title);
            assert_eq!(wide.subtitle, subtitle);
            assert_ne!(wide.subtitle, "Waiting for authentication");
            let narrow = window_header(view, true);
            assert_eq!(narrow.title, title);
            assert_eq!(narrow.subtitle, "");
        }

        for pass in [RunPass::Download, RunPass::Apply] {
            for label in [
                "",
                "Downloading",
                "Installing packages",
                "Verifying",
                "transaction",
                "Updating",
            ] {
                let heading = run_heading(label, pass);
                let chrome = window_header(
                    HeaderView::Running {
                        heading,
                        footnote: "System · 1 of 2 downloaded",
                    },
                    false,
                );
                assert_ne!(chrome.title, "Installing");
                assert_eq!(chrome.title, heading);
            }
        }
        assert_eq!(
            done_header_subtitle(0, 12, 10, 2, 0, "4 minutes"),
            "12 upgraded · 10 system / 2 apps / 0 firmware · 4 minutes"
        );
        assert_eq!(
            done_header_subtitle(1, 8, 8, 0, 0, "4 minutes"),
            "8 applied · 1 failed · 4 minutes"
        );
        assert_eq!(checking_footnote(2, None), "2 of 3 sources");
        assert_eq!(checking_footnote(2, Some("")), "2 of 3 sources");
        assert_eq!(
            checking_footnote(1, Some("about 1 minute")),
            "about 1 minute · 1 of 3 sources"
        );
        assert_eq!(
            soft_error_banner_title(0),
            "0 sources could not be checked"
        );
        assert_eq!(
            soft_error_banner_title(1),
            "1 source could not be checked"
        );
        assert_eq!(
            soft_error_banner_title(3),
            "3 sources could not be checked"
        );
        assert_eq!(ready_banner_title(&[]), None);
        assert_eq!(
            ready_banner_title(&["dnf failed".into()]).as_deref(),
            Some("1 source could not be checked")
        );
        assert_eq!(
            ready_banner_title(&["dnf failed".into(), "fwupd failed".into()]).as_deref(),
            Some("2 sources could not be checked")
        );
        assert_eq!(
            ready_banner_title(&[
                "Stopped before install. Downloaded packages were not applied.".into()
            ])
            .as_deref(),
            Some("Stopped before install. Downloaded packages were not applied.")
        );
        assert_eq!(
            ABOUT_COMMENTS,
            "A desktop front-end for Fedora system, Flatpak, and firmware updates"
        );
        assert_eq!(ABOUT_NAME, "Fedora Updater");
        assert_eq!(
            AUTH_DESCRIPTION,
            "A system dialog asks for an administrator password. The password is not stored in this window."
        );
        assert_ne!(
            window_header(HeaderView::Authenticating(AuthPurpose::Check), false).subtitle,
            "Waiting for authentication"
        );
    }

    #[test]
    fn alert_dialogs_use_the_accepted_copy() {
        let stale = stale_update_dialog();
        assert_eq!(stale.heading, "Refresh Update List?");
        assert_eq!(
            stale.body,
            "This list is more than a day old. Refresh it before applying these updates."
        );
        assert_eq!(stale.close_id, "no");
        assert_eq!(stale.close_label, "_Cancel");
        assert_eq!(stale.confirm_id, "yes");
        assert_eq!(stale.confirm_label, "_Refresh");
        assert!(stale.confirm_suggested);
        assert!(!stale.confirm_destructive);
        assert_eq!(stale.default_id, "yes");
        assert_eq!(stale.default_id, stale.confirm_id);
        assert_ne!(stale.default_id, stale.close_id);

        let stop = stop_update_dialog();
        assert_eq!(stop.heading, "Stop This Update?");
        assert_eq!(
            stop.body,
            "Stopping while packages are being installed can leave the package database incomplete. Check package history before trying again."
        );
        assert_eq!(stop.close_id, "wait");
        assert_eq!(stop.close_label, "_Keep Waiting");
        assert_eq!(stop.confirm_id, "stop");
        assert_eq!(stop.confirm_label, "Stop _Anyway");
        assert!(stop.confirm_destructive);
        assert!(!stop.confirm_suggested);
        assert_eq!(stop.default_id, "wait");
        assert_eq!(stop.default_id, stop.close_id);
        assert_ne!(stop.default_id, stop.confirm_id);
    }

    #[test]
    fn fail_retry_label_follows_the_action() {
        assert_eq!(fail_retry_label(RetryAction::CheckAgain), "Check Again");
        assert_eq!(fail_retry_label(RetryAction::RetryApply), "Retry Update");
        assert_eq!(fail_retry_label(RetryAction::Reboot), "Restart");
    }

    #[test]
    fn pipeline_stage_maps_download_prepare_install_verify() {
        assert_eq!(
            PipelineStage::ALL.map(|s| s.label()),
            ["Download", "Prepare", "Install", "Verify"]
        );
        assert_eq!(
            PipelineStage::ALL.map(|s| s.done_label()),
            ["Download ✓", "Prepare ✓", "Install ✓", "Verify ✓"]
        );

        assert_eq!(
            pipeline_stage("Downloading packages", RunPass::Download),
            PipelineStage::Download
        );
        assert_eq!(
            pipeline_stage("Downloading Flatpak", RunPass::Download),
            PipelineStage::Download
        );
        assert_eq!(
            pipeline_stage("Downloading firmware", RunPass::Download),
            PipelineStage::Download
        );
        assert_eq!(
            pipeline_stage("Starting…", RunPass::Download),
            PipelineStage::Download
        );

        assert_eq!(
            pipeline_stage("Running transaction", RunPass::Download),
            PipelineStage::Prepare
        );
        assert_eq!(
            pipeline_stage("Prepare transaction", RunPass::Apply),
            PipelineStage::Prepare
        );
        assert_eq!(
            pipeline_stage("Decompressing firmware", RunPass::Download),
            PipelineStage::Prepare
        );
        assert_eq!(
            pipeline_stage("Waiting", RunPass::Apply),
            PipelineStage::Prepare
        );

        assert_eq!(
            pipeline_stage("Installing", RunPass::Apply),
            PipelineStage::Install
        );
        assert_eq!(
            pipeline_stage("Installing firmware", RunPass::Download),
            PipelineStage::Install
        );
        assert_eq!(
            pipeline_stage("Updating Flatpak", RunPass::Download),
            PipelineStage::Install
        );
        assert_eq!(
            pipeline_stage("", RunPass::Apply),
            PipelineStage::Install
        );

        assert_eq!(
            pipeline_stage("Verifying", RunPass::Apply),
            PipelineStage::Verify
        );
        assert_eq!(
            pipeline_stage("Verifying firmware", RunPass::Apply),
            PipelineStage::Verify
        );
        assert_eq!(
            pipeline_stage("Cleanup", RunPass::Apply),
            PipelineStage::Verify
        );
        assert_eq!(
            pipeline_stage("Complete", RunPass::Apply),
            PipelineStage::Verify
        );
        assert_eq!(
            pipeline_stage("Firmware installed", RunPass::Apply),
            PipelineStage::Verify
        );
        assert_eq!(
            pipeline_stage("Restarting device", RunPass::Apply),
            PipelineStage::Verify
        );
    }

    #[test]
    fn format_elapsed_and_done_clean_summary_format_cleanly() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(42), "42s");
        assert_eq!(format_elapsed(60), "1m 00s");
        assert_eq!(format_elapsed(185), "3m 05s");

        assert_eq!(
            done_clean_summary(1, "12s"),
            "1 update installed in 12s. No restart needed."
        );
        assert_eq!(
            done_clean_summary(6, "4m 18s"),
            "6 updates installed in 4m 18s. No restart needed."
        );
    }

    #[test]
    fn reboot_trigger_names_and_summary_handle_kernel_dedup_failures_and_firmware() {
        let mut openssl = Package::new_dnf("openssl", "x86_64", "3.2.2", "updates");
        openssl.status = PackageStatus::Completed;
        assert_eq!(
            reboot_trigger_summary(&[openssl.clone()]),
            "Restart to finish applying system updates."
        );

        let mut kernel = Package::new_dnf("kernel", "x86_64", "6.14.11", "updates");
        kernel.status = PackageStatus::Completed;
        let mut kernel_dup = kernel.clone();
        kernel_dup.arch = "noarch".into();
        kernel_dup.id = "dnf:kernel.noarch:6.14.11".into();
        let mut kernel_core = Package::new_dnf("kernel-core", "x86_64", "6.14.11", "updates");
        kernel_core.status = PackageStatus::Completed;
        let mut systemd = Package::new_dnf("systemd", "x86_64", "257", "updates");
        systemd.status = PackageStatus::Completed;
        let mut glibc = Package::new_dnf("glibc", "i686", "2.41", "updates");
        glibc.status = PackageStatus::Completed;
        let mut mesa = Package::new_dnf("mesa-dri-drivers", "x86_64", "25.0", "updates");
        mesa.status = PackageStatus::Completed;
        let mut fw = Package::new_dnf("UEFI dbx", "", "20241101", "fwupd");
        fw.source = UpdateSource::Firmware;
        fw.status = PackageStatus::Completed;

        // 1 trigger (with duplicate arch filtered).
        assert_eq!(
            reboot_trigger_summary(&[kernel.clone(), kernel_dup, openssl.clone()]),
            "kernel requires a restart to take effect."
        );
        // 2 triggers.
        assert_eq!(
            reboot_trigger_summary(&[kernel.clone(), systemd.clone()]),
            "kernel and systemd require a restart to take effect."
        );
        // 3 triggers.
        assert_eq!(
            reboot_trigger_summary(&[kernel.clone(), kernel_core.clone(), systemd.clone()]),
            "kernel, kernel-core, and systemd require a restart to take effect."
        );
        // 5 triggers -> capped at 3 + "and 2 more".
        let mut dbus = Package::new_dnf("dbus", "x86_64", "1.16", "updates");
        dbus.status = PackageStatus::Completed;
        assert_eq!(
            reboot_trigger_summary(&[kernel.clone(), kernel_core, systemd, glibc, dbus]),
            "kernel, kernel-core, systemd, and 2 more require a restart to take effect."
        );

        // Failed DNF trigger is ignored, falling back cleanly to completed Firmware trigger.
        let mut failed_kernel = kernel;
        failed_kernel.status = PackageStatus::Failed;
        assert_eq!(
            reboot_trigger_summary(&[failed_kernel.clone(), fw]),
            "UEFI dbx requires a restart to take effect."
        );
        let mut failed_fw = Package::new_dnf("UEFI dbx", "", "20241101", "fwupd");
        failed_fw.source = UpdateSource::Firmware;
        failed_fw.status = PackageStatus::Failed;
        assert_eq!(
            reboot_trigger_summary(&[failed_kernel, failed_fw, mesa]),
            "Restart to finish applying system updates."
        );

        let mut ostree_pkg = Package::new_rpm_ostree(
            "upgraded",
            "mesa-dri-drivers",
            "x86_64",
            Some("25.0".into()),
            "25.1",
            "fedora-ostree",
            "Upgraded",
            None,
        );
        ostree_pkg.status = PackageStatus::Completed;
        assert_eq!(
            reboot_trigger_summary(std::slice::from_ref(&ostree_pkg)),
            "Restart to boot into the staged atomic system deployment."
        );
        ostree_pkg.status = PackageStatus::Failed;
        assert_eq!(
            reboot_trigger_summary(&[ostree_pkg]),
            "Restart to finish applying system updates."
        );
    }

    #[test]
    fn phase_completion_notification_covers_check_and_apply_outcomes() {
        let dnf = Package::new_dnf("firefox", "x86_64", "140.0", "updates");
        let ready = Phase::Ready {
            packages: vec![dnf.clone()],
            soft_errors: vec![],
        };
        let check_notif =
            phase_completion_notification(&ready, true, false).expect("ready notification");
        assert_eq!(check_notif.id, COMPLETION_NOTIFICATION_ID);
        assert_eq!(check_notif.title, "Updates Ready to Review");
        assert!(check_notif.body.contains("1 update available"), "{}", check_notif.body);

        // No notification when check produced nothing or flags are unset.
        assert_eq!(phase_completion_notification(&ready, false, false), None);
        let empty_ready = Phase::Ready {
            packages: vec![],
            soft_errors: vec![],
        };
        assert_eq!(
            phase_completion_notification(&empty_ready, true, false),
            None
        );

        let mut completed_pkg = dnf;
        completed_pkg.status = PackageStatus::Completed;
        let done_clean = Phase::Done {
            packages: vec![completed_pkg.clone()],
            duration: "8s".into(),
            needs_reboot: false,
            failed: 0,
            failure_reasons: vec![],
        };
        let clean_notif =
            phase_completion_notification(&done_clean, false, true).expect("clean done");
        assert_eq!(clean_notif.title, "Updates Installed");
        assert_eq!(clean_notif.body, "1 update applied cleanly.");

        let mut kernel = Package::new_dnf("kernel", "x86_64", "7.1.12", "updates");
        kernel.status = PackageStatus::Completed;
        let done_reboot = Phase::Done {
            packages: vec![kernel],
            duration: "24s".into(),
            needs_reboot: true,
            failed: 0,
            failure_reasons: vec![],
        };
        let reboot_notif =
            phase_completion_notification(&done_reboot, false, true).expect("reboot done");
        assert_eq!(reboot_notif.title, "Updates Installed — Restart Recommended");
        assert!(reboot_notif.body.contains("kernel"), "{}", reboot_notif.body);

        let mut failed_pkg = completed_pkg;
        failed_pkg.status = PackageStatus::Failed;
        let done_partial = Phase::Done {
            packages: vec![failed_pkg],
            duration: "5s".into(),
            needs_reboot: true,
            failed: 1,
            failure_reasons: vec!["Firmware: Connect AC power".into()],
        };
        let partial_notif =
            phase_completion_notification(&done_partial, false, true).expect("partial done");
        assert_eq!(partial_notif.title, "Updates Finished With Problems");
        assert!(partial_notif.body.contains("1 of 1"), "{}", partial_notif.body);
        assert!(partial_notif.body.contains("Restart recommended"), "{}", partial_notif.body);

        let failed = Phase::Failed {
            title: "All updates failed".into(),
            detail: "System: dependency conflict".into(),
            retry: RetryAction::CheckAgain,
        };
        let fail_notif =
            phase_completion_notification(&failed, false, true).expect("apply failed");
        assert_eq!(fail_notif.title, "Update Failed");
        assert_eq!(fail_notif.body, "System: dependency conflict");
    }

    #[test]
    fn phase2_cta_selection_history_and_size_delta_helpers() {
        assert_eq!(
            window_header(HeaderView::History { subtitle: "12 transactions" }, false),
            HeaderChrome {
                title: "Update History".into(),
                subtitle: "12 transactions".into(),
            }
        );
        assert_eq!(history_subtitle(0, 0), "No recorded transactions");
        assert_eq!(history_subtitle(1, 1), "1 transaction");
        assert_eq!(history_subtitle(12, 12), "12 transactions");
        assert_eq!(history_subtitle(3, 12), "3 of 12 transactions");

        assert_eq!(apply_cta_label(ApplyMode::Live, 5, 5, None), "Update All");
        assert_eq!(
            apply_cta_label(ApplyMode::Live, 5, 5, Some(15 * 1024 * 1024)),
            "Update All · 15 MB"
        );
        assert_eq!(
            apply_cta_label(ApplyMode::Live, 2, 5, Some(4 * 1024 * 1024)),
            "Update 2 of 5 · 4.0 MB"
        );
        assert_eq!(
            apply_cta_label(ApplyMode::LiveAndReboot, 5, 5, None),
            "Update & Restart"
        );
        assert_eq!(
            apply_cta_label(ApplyMode::LiveAndReboot, 3, 5, Some(2 * 1024 * 1024)),
            "Update 3 & Restart · 2.0 MB"
        );
        assert_eq!(
            apply_cta_label(ApplyMode::OfflineOnReboot, 5, 5, None),
            "Prepare Offline Update"
        );
        assert_eq!(
            apply_cta_label(ApplyMode::OfflineOnReboot, 1, 4, None),
            "Prepare 1 Offline"
        );
        assert_eq!(
            apply_cta_label(ApplyMode::DownloadOnly, 5, 5, None),
            "Download Only"
        );
        assert_eq!(
            apply_cta_label(ApplyMode::DownloadOnly, 0, 5, Some(1024)),
            "Download 0"
        );

        assert_eq!(selection_summary_text(0, 0, None), "0 selected");
        assert_eq!(
            selection_summary_text(3, 5, Some(2 * 1024 * 1024)),
            "3 of 5 selected · 2.0 MB"
        );
        assert_eq!(selection_summary_text(0, 5, Some(2 * 1024 * 1024)), "0 of 5 selected");

        assert_eq!(format_size_delta(0), "0 B");
        assert_eq!(format_size_delta(2 * 1024 * 1024), "+2.0 MB");
        assert_eq!(format_size_delta(-512 * 1024), "-512 KB");
    }

    #[test]
    fn check_progress_for_sources_matches_legacy_and_handles_subsets() {
        let core_sources = &[
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
            UpdateSource::Firmware,
        ];
        for checked in [
            vec![],
            vec![UpdateSource::Dnf],
            vec![UpdateSource::Dnf, UpdateSource::FlatpakUser],
            vec![
                UpdateSource::Dnf,
                UpdateSource::FlatpakUser,
                UpdateSource::FlatpakSystem,
            ],
            vec![
                UpdateSource::Dnf,
                UpdateSource::FlatpakUser,
                UpdateSource::FlatpakSystem,
                UpdateSource::Firmware,
            ],
        ] {
            let (l_frac, l_done, l_status) = check_progress(&checked);
            let (g_frac, g_done, g_total, g_status) =
                check_progress_for_sources(&checked, core_sources);
            assert!((l_frac - g_frac).abs() < 1e-9);
            assert_eq!(l_done, g_done);
            assert_eq!(g_total, 3);
            assert_eq!(l_status, g_status);
        }

        // With Firmware disabled, only System and Apps run and completion reaches 100%.
        let no_fw = [
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
        ];
        let (frac, done, total, status) = check_progress_for_sources(&[UpdateSource::Dnf], &no_fw);
        assert!((frac - (1.0 / 3.0)).abs() < 1e-9);
        assert_eq!(done, 1);
        assert_eq!(total, 2);
        assert_eq!(status, "System done · Apps in progress");

        let (frac_done, done2, total2, status2) = check_progress_for_sources(&no_fw, &no_fw);
        assert!((frac_done - 1.0).abs() < 1e-9);
        assert_eq!(done2, 2);
        assert_eq!(total2, 2);
        assert_eq!(status2, "System · Apps done");
        assert_eq!(checking_footnote_of(1, 1, None), "1 of 1 source");
        assert_eq!(
            checking_footnote_of(1, 2, Some("About 1 minute left")),
            "About 1 minute left · 1 of 2 sources"
        );
    }

    #[test]
    fn source_summary_and_done_subtitle_include_homebrew_dev_counts_when_present() {
        let dnf = Package::new_dnf("kernel", "x86_64", "6.14.11", "updates");
        let mut brew = Package::new_homebrew(
            "ripgrep",
            Some("14.1.0".into()),
            "14.1.1",
            "homebrew/core",
            false,
        );
        brew.status = PackageStatus::Completed;

        assert_eq!(
            source_summary_line(&[dnf.clone(), brew.clone()]),
            "1 system · 0 apps · 0 firmware · 1 dev"
        );
        assert_eq!(
            done_header_subtitle_for_packages(0, 2, &[dnf, brew], "45 seconds"),
            "2 upgraded · 1 system / 0 apps / 0 firmware / 1 dev · 45 seconds"
        );

        let with_brew = [
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
            UpdateSource::Homebrew,
            UpdateSource::Firmware,
        ];
        let (frac, done, total, status) =
            check_progress_for_sources(&[UpdateSource::Dnf], &with_brew);
        assert!((frac - 0.2).abs() < 1e-9);
        assert_eq!(done, 1);
        assert_eq!(total, 4);
        assert_eq!(
            status,
            "System done · Apps in progress · Dev & Containers queued · Firmware queued"
        );
    }

    #[test]
    fn gnome_extensions_relogin_summary_counts_and_notification_work_without_reboot() {
        let mut ext = Package::new_gnome_extension(
            "blur-my-shell@aunetx",
            "Blur my Shell",
            Some("v66".into()),
            Some("Adds blur to GNOME Shell".into()),
            Some("https://github.com/aunetx/blur-my-shell".into()),
        );
        assert_eq!(source_summary_line(std::slice::from_ref(&ext)), "0 system · 0 apps · 0 firmware · 1 extensions");

        ext.status = PackageStatus::Completed;
        assert!(has_completed_gnome_extensions(std::slice::from_ref(&ext)));
        assert_eq!(
            done_header_subtitle_for_packages(0, 1, std::slice::from_ref(&ext), "6s"),
            "1 upgraded · 0 system / 0 apps / 0 firmware / 1 extensions · 6s"
        );
        let summary = done_clean_summary_for_packages(1, std::slice::from_ref(&ext), "6s");
        assert!(summary.contains("Log out and back in"), "{summary}");
        assert!(!summary.contains("No restart needed"), "{summary}");

        let done_ext = Phase::Done {
            packages: vec![ext],
            duration: "6s".into(),
            needs_reboot: false,
            failed: 0,
            failure_reasons: vec![],
        };
        let notif = phase_completion_notification(&done_ext, false, true).expect("notif");
        assert_eq!(notif.title, "Updates Installed");
        assert!(notif.body.contains("Log out and back in"), "{}", notif.body);

        let with_ext = [
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
            UpdateSource::GnomeExtensions,
            UpdateSource::Firmware,
        ];
        let (frac, done, total, status) =
            check_progress_for_sources(&[UpdateSource::Dnf], &with_ext);
        assert!((frac - 0.2).abs() < 1e-9);
        assert_eq!(done, 1);
        assert_eq!(total, 4);
        assert_eq!(
            status,
            "System done · Apps in progress · Extensions queued · Firmware queued"
        );
    }

    #[test]
    fn user_space_toolchains_aggregate_under_dev_and_never_trigger_reboot() {
        let mut rust = Package::new_toolchain(
            UpdateSource::Rustup,
            "rustup:stable",
            "stable",
            Some("1.86.0".into()),
            "1.87.0",
            "rustup",
            "",
            None,
            None,
        );
        let mut mise = Package::new_toolchain(
            UpdateSource::Mise,
            "mise:node",
            "node",
            Some("22.14.0".into()),
            "22.15.0",
            "mise",
            "",
            None,
            None,
        );
        let mut uv = Package::new_toolchain(
            UpdateSource::UvTools,
            "uv:httpie",
            "httpie",
            Some("3.2.2".into()),
            "3.2.4",
            "uv",
            "",
            None,
            None,
        );
        let mut pipx = Package::new_toolchain(
            UpdateSource::Pipx,
            "pipx:ansible-lint",
            "ansible-lint",
            Some("24.9.0".into()),
            "24.10.0",
            "pipx",
            "",
            None,
            None,
        );
        let pkgs = vec![rust.clone(), mise.clone(), uv.clone(), pipx.clone()];
        assert_eq!(
            source_summary_line(&pkgs),
            "0 system · 0 apps · 0 firmware · 4 dev"
        );

        for p in [&mut rust, &mut mise, &mut uv, &mut pipx] {
            p.status = PackageStatus::Completed;
        }
        let done_pkgs = vec![rust, mise, uv, pipx];
        assert_eq!(
            done_header_subtitle_for_packages(0, 4, &done_pkgs, "12s"),
            "4 upgraded · 0 system / 0 apps / 0 firmware / 4 dev · 12s"
        );
        let clean = done_clean_summary_for_packages(4, &done_pkgs, "12s");
        assert!(clean.contains("No restart needed"), "{clean}");

        let enabled = [
            UpdateSource::Dnf,
            UpdateSource::Rustup,
            UpdateSource::Mise,
            UpdateSource::UvTools,
            UpdateSource::Pipx,
        ];
        let (_frac, done, total, status) =
            check_progress_for_sources(&[UpdateSource::Dnf, UpdateSource::Rustup], &enabled);
        assert_eq!(done, 1);
        assert_eq!(total, 2);
        assert_eq!(status, "System done · Dev & Containers in progress");
    }
}
