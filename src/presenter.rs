//! Pure UI presentation helpers and view-model copy (no GTK).

use crate::model::{
    count_by_source, format_bytes, total_download_bytes, AuthPurpose, IdleScreen, Package,
    RetryAction, RunPass, UpdateSource,
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
    };
    HeaderChrome {
        title,
        subtitle: if narrow { String::new() } else { subtitle },
    }
}

pub fn checking_footnote(groups_done: u32, eta: Option<&str>) -> String {
    match eta {
        Some(eta) if !eta.is_empty() => format!("{eta} · {groups_done} of 3 sources"),
        _ => format!("{groups_done} of 3 sources"),
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

pub fn soft_error_banner_title(count: usize) -> String {
    if count == 1 {
        "1 source could not be checked".into()
    } else {
        format!("{count} sources could not be checked")
    }
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
        ];
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
        assert_eq!(
            checking_footnote(1, Some("about 1 minute")),
            "about 1 minute · 1 of 3 sources"
        );
        assert_eq!(
            soft_error_banner_title(1),
            "1 source could not be checked"
        );
        assert_eq!(
            soft_error_banner_title(3),
            "3 sources could not be checked"
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
}
