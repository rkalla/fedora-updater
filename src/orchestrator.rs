//! Sequential multi-backend check/apply using one privileged helper session.

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use async_channel::Sender;

use crate::backend::{self, dnf, flatpak, fwupd};
use crate::helper_protocol::HelperCommand;
use crate::model::{AuthPurpose, Package, UpdateSource, WorkerEvent};
use crate::privilege::{
    close_session, run_local_command, session_is_alive, with_session, SessionError, SessionEvent,
};
use crate::state::Event as StateEvent;

/// Incremented when a Check/Apply starts or the user cancels auth.
/// In-flight workers compare against the generation they captured at spawn.
static WORK_GEN: AtomicU64 = AtomicU64::new(0);

fn begin_background_work() -> u64 {
    WORK_GEN.fetch_add(1, Ordering::SeqCst) + 1
}

fn work_is_current(gen: u64) -> bool {
    WORK_GEN.load(Ordering::SeqCst) == gen
}

/// Invalidate in-flight Check/Apply after the user cancels the auth wait.
pub fn cancel_background_work() {
    WORK_GEN.fetch_add(1, Ordering::SeqCst);
}

/// Map orchestrator outcomes into pure state events for the UI reducer.
pub fn worker_to_state_events(ev: WorkerEvent) -> Vec<StateEvent> {
    match ev {
        WorkerEvent::SessionStarted => vec![],
        WorkerEvent::Line { source_tag, text } => vec![StateEvent::ConsoleLine {
            tag: source_tag,
            text,
        }],
        WorkerEvent::BackendCheckFinished {
            source,
            packages,
            soft_error,
            ..
        } => vec![StateEvent::BackendCheckDone {
            source,
            packages,
            soft_error,
        }],
        WorkerEvent::CheckAllFinished {
            packages,
            soft_errors,
        } => vec![StateEvent::CheckComplete {
            packages,
            soft_errors,
        }],
        WorkerEvent::ApplyProgress {
            source,
            package_hint,
            status_hint,
            progress,
            phase_label,
        } => vec![StateEvent::ApplyProgress {
            source,
            package_hint,
            status_hint,
            progress,
            phase_label,
        }],
        WorkerEvent::BackendApplyFinished {
            source,
            exit_code,
            needs_reboot,
            ..
        } => vec![StateEvent::BackendApplyDone {
            source,
            success: exit_code == 0,
            needs_reboot,
        }],
        WorkerEvent::ApplyAllFinished { needs_reboot, .. } => {
            vec![StateEvent::ApplyComplete { needs_reboot }]
        }
        WorkerEvent::Failed { message, detail } => vec![StateEvent::Fail {
            title: message,
            detail,
        }],
    }
}

fn send(tx: &Sender<WorkerEvent>, ev: WorkerEvent) {
    let _ = tx.send_blocking(ev);
}

fn line(tx: &Sender<WorkerEvent>, tag: &str, text: String) {
    send(
        tx,
        WorkerEvent::Line {
            source_tag: tag.into(),
            text,
        },
    );
}

fn map_session_start_err(tx: &Sender<WorkerEvent>, e: SessionError) {
    match e {
        SessionError::AuthDenied => send(
            tx,
            WorkerEvent::Failed {
                message: "Authentication cancelled".into(),
                detail: "polkit did not grant permission for the update helper.".into(),
            },
        ),
        other => send(
            tx,
            WorkerEvent::Failed {
                message: "Could not start privileged helper".into(),
                detail: other.to_string(),
            },
        ),
    }
}

/// Full check pipeline. Reuses the shared helper session when already elevated
/// (no second password); otherwise starts it once via polkit.
pub fn run_check_all(tx: Sender<WorkerEvent>) {
    let gen = begin_background_work();
    thread::spawn(move || {
        // Ensure session up front so SessionStarted fires before long work
        let started = with_session(|_s| Ok(()));
        if !work_is_current(gen) {
            return;
        }
        if let Err(e) = started {
            map_session_start_err(&tx, e);
            return;
        }
        send(&tx, WorkerEvent::SessionStarted);

        let mut all = Vec::new();
        let mut soft_errors = Vec::new();

        if !work_is_current(gen) {
            return;
        }

        // 1) DNF (privileged)
        emit_backend_check(
            &tx,
            UpdateSource::Dnf,
            run_helper_check(&tx, HelperCommand::CheckDnf, "dnf", |log, code| {
                let mut pkgs = dnf::parse_check_update(log);
                if dnf::check_exit_is_hard_error(code, &pkgs) {
                    Err(format!("dnf check-update failed (exit {code})"))
                } else {
                    attach_dnf_download_sizes(&tx, &mut pkgs);
                    Ok(pkgs)
                }
            }),
            &mut all,
            &mut soft_errors,
        );

        // 2) Flatpak metadata (unprivileged)
        let _ = run_local_command(
            "flatpak",
            &["update", "--appstream", "--noninteractive"],
            |l| line(&tx, "flatpak", l),
        );

        // 3–4) Flatpak lists
        emit_backend_check(
            &tx,
            UpdateSource::FlatpakUser,
            run_flatpak_remote_ls(&tx, true).map_err(|e| format!("flatpak user: {e}")),
            &mut all,
            &mut soft_errors,
        );
        emit_backend_check(
            &tx,
            UpdateSource::FlatpakSystem,
            run_flatpak_remote_ls(&tx, false).map_err(|e| format!("flatpak system: {e}")),
            &mut all,
            &mut soft_errors,
        );

        // 5) fwupd
        let _ = run_helper_check(
            &tx,
            HelperCommand::CheckFwupdRefresh,
            "fwupd",
            |_log, _code| Ok(Vec::new()),
        );

        emit_backend_check(
            &tx,
            UpdateSource::Firmware,
            run_helper_check(&tx, HelperCommand::CheckFwupd, "fwupd", |log, code| {
                let pkgs = fwupd::parse_get_updates(log);
                if code != 0 && pkgs.is_empty() {
                    let lower = log.to_ascii_lowercase();
                    if lower.contains("no updates") || lower.contains("nothing") {
                        return Ok(vec![]);
                    }
                    if lower.contains("not found") || lower.contains("no such file") {
                        return Err("fwupd not available".into());
                    }
                    return Err(format!("fwupdmgr get-updates exit {code}"));
                }
                Ok(pkgs)
            }),
            &mut all,
            &mut soft_errors,
        );

        if !work_is_current(gen) {
            return;
        }

        // Leave session open for Apply and later Checks (app-lifetime elevation).
        send(
            &tx,
            WorkerEvent::CheckAllFinished {
                packages: all,
                soft_errors,
            },
        );
    });
}

fn emit_backend_check(
    tx: &Sender<WorkerEvent>,
    source: UpdateSource,
    result: Result<Vec<Package>, String>,
    all: &mut Vec<Package>,
    soft_errors: &mut Vec<String>,
) {
    match result {
        Ok(pkgs) => {
            send(
                tx,
                WorkerEvent::BackendCheckFinished {
                    source,
                    packages: pkgs.clone(),
                    exit_code: 0,
                    log: String::new(),
                    soft_error: None,
                },
            );
            all.extend(pkgs);
        }
        Err(e) => {
            send(
                tx,
                WorkerEvent::BackendCheckFinished {
                    source,
                    packages: Vec::new(),
                    exit_code: 1,
                    log: String::new(),
                    soft_error: Some(e.clone()),
                },
            );
            soft_errors.push(e);
        }
    }
}

fn attach_dnf_download_sizes(tx: &Sender<WorkerEvent>, pkgs: &mut [Package]) {
    if pkgs.is_empty() {
        return;
    }
    let mut log = String::new();
    match run_local_command("dnf", dnf::REPOQUERY_UPGRADES_ARGS, |l| {
        log.push_str(&l);
        log.push('\n');
    }) {
        Ok(_) => {
            let sizes = dnf::parse_repoquery_sizes(&log);
            let n = dnf::apply_download_sizes(pkgs, &sizes);
            if n > 0 {
                line(tx, "dnf", format!("Download sizes for {n} package(s)"));
            }
        }
        Err(e) => {
            line(tx, "dnf", format!("Could not query download sizes: {e}"));
        }
    }
}

fn run_helper_check<F>(
    tx: &Sender<WorkerEvent>,
    cmd: HelperCommand,
    tag: &str,
    parse: F,
) -> Result<Vec<Package>, String>
where
    F: Fn(&str, i32) -> Result<Vec<Package>, String>,
{
    let mut log = String::new();
    let code = with_session(|session| {
        session.run_command(cmd, |ev| match ev {
            SessionEvent::Line(l) => {
                log.push_str(&l);
                log.push('\n');
                line(tx, tag, l);
            }
            SessionEvent::Finished { .. } => {}
        })
    })
    .map_err(|e| e.to_string())?;
    parse(&log, code)
}

fn run_flatpak_remote_ls(tx: &Sender<WorkerEvent>, user: bool) -> Result<Vec<Package>, String> {
    let args = flatpak::remote_ls_args(user);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut log = String::new();
    let tag = if user {
        "flatpak-user"
    } else {
        "flatpak-system"
    };
    let code = run_local_command("flatpak", &arg_refs, |l| {
        log.push_str(&l);
        log.push('\n');
        // Don't flood console with raw JSON; keep a short status line.
        if !l.trim_start().starts_with('[') && !l.trim_start().starts_with('{') {
            line(tx, tag, l);
        }
    })
    .map_err(|e| e.to_string())?;

    if code != 0 {
        let lower = log.to_ascii_lowercase();
        if lower.contains("not found") || lower.contains("no such file") {
            return Err("flatpak not installed".into());
        }
        if log.trim().is_empty() {
            return Ok(vec![]);
        }
    }

    let source = if user {
        UpdateSource::FlatpakUser
    } else {
        UpdateSource::FlatpakSystem
    };

    // remote-ls --updates can false-positive on Fedora/OCI remotes when the
    // remote commit is only an Alt-id of the installed commit. Filter those out
    // so Check matches what `flatpak update` will actually do.
    let candidates = flatpak::parse_remote_ls_json(&log, source);
    let before = candidates.len();
    let packages = flatpak::filter_actionable_candidates(candidates);
    if before > packages.len() {
        line(
            tx,
            tag,
            format!(
                "Filtered {} phantom update(s) (already current / Alt-id match)",
                before - packages.len()
            ),
        );
    }
    line(
        tx,
        tag,
        format!(
            "{} actionable update(s) ({})",
            packages.len(),
            if user { "user" } else { "system" }
        ),
    );
    Ok(packages)
}

/// Apply all pending sources sequentially. Reuses the shared helper session
/// when still open (no second password).
pub fn run_apply_all(tx: Sender<WorkerEvent>, packages: Vec<Package>) {
    let gen = begin_background_work();
    thread::spawn(move || {
        let sources = crate::model::sources_present(&packages);
        if sources.is_empty() {
            send(
                &tx,
                WorkerEvent::Failed {
                    message: "Nothing to apply".into(),
                    detail: "Package list was empty.".into(),
                },
            );
            return;
        }

        // Flatpak runs as the current user (its own polkit). Only dnf/fwupd need helper.
        let needs_session = sources
            .iter()
            .any(|s| matches!(s, UpdateSource::Dnf | UpdateSource::Firmware));

        if needs_session {
            if let Err(e) = with_session(|_s| Ok(())) {
                if work_is_current(gen) {
                    map_session_start_err(&tx, e);
                }
                return;
            }
        }
        if !work_is_current(gen) {
            return;
        }
        send(&tx, WorkerEvent::SessionStarted);

        if !work_is_current(gen) {
            return;
        }

        let mut overall_reboot = false;
        let mut failed_sources = Vec::new();
        let mut last_code = 0;

        for source in &sources {
            if !work_is_current(gen) {
                return;
            }
            let source = *source;
            let tag = source.console_tag();
            let source_pkgs: Vec<&Package> =
                packages.iter().filter(|p| p.source == source).collect();
            let known_names: Vec<String> = source_pkgs.iter().map(|p| p.name.clone()).collect();

            line(
                &tx,
                "orchestrator",
                format!("── Applying {} ──", source.label()),
            );

            send(
                &tx,
                WorkerEvent::ApplyProgress {
                    source,
                    package_hint: None,
                    status_hint: None,
                    progress: None,
                    phase_label: format!("Updating {}", source.label()),
                },
            );

            let (code, log) = match source {
                UpdateSource::Dnf => {
                    run_helper_apply(&tx, HelperCommand::ApplyDnf, tag, source, &known_names)
                }
                UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => {
                    // Apply the exact refs we listed at Check time, as the user.
                    // (Running system flatpak as root via helper can disagree with
                    // the user-session view and report "Nothing to update".)
                    let user = source == UpdateSource::FlatpakUser;
                    let refs: Vec<String> = source_pkgs
                        .iter()
                        .filter_map(|p| flatpak::update_ref_from_package(p))
                        .collect();
                    if refs.is_empty() {
                        line(&tx, tag, "No flatpak refs to update".into());
                        (0, String::new())
                    } else {
                        line(
                            &tx,
                            tag,
                            format!("Updating {} ref(s): {}", refs.len(), refs.join(", ")),
                        );
                        let args = flatpak::update_args_for_refs(user, &refs);
                        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                        run_local_apply(&tx, "flatpak", &arg_refs, tag, source, &known_names)
                    }
                }
                UpdateSource::Firmware => {
                    run_helper_apply(&tx, HelperCommand::ApplyFwupd, tag, source, &known_names)
                }
            };

            let reboot = backend::detect_reboot(source, &log, &packages);
            if reboot {
                overall_reboot = true;
            }
            if code != 0 {
                failed_sources.push(source);
                last_code = code;
            }

            send(
                &tx,
                WorkerEvent::BackendApplyFinished {
                    source,
                    exit_code: code,
                    log,
                    needs_reboot: reboot,
                },
            );
        }

        if sources.contains(&UpdateSource::Firmware) {
            let mut log = String::new();
            if let Ok(code) = with_session(|session| {
                session.run_command(HelperCommand::CheckFwupdReboot, |ev| {
                    if let SessionEvent::Line(l) = ev {
                        log.push_str(&l);
                        log.push('\n');
                    }
                })
            }) {
                if fwupd::parse_reboot_needed_output(&log, code) {
                    overall_reboot = true;
                }
            }
        }

        // Keep elevated helper for further Checks / Applies in this app session.
        send(
            &tx,
            WorkerEvent::ApplyAllFinished {
                exit_code: last_code,
                needs_reboot: overall_reboot,
                failed_sources,
            },
        );
    });
}

fn emit_progress_from_line(
    tx: &Sender<WorkerEvent>,
    source: UpdateSource,
    l: &str,
    _known_names: &[String],
) {
    let hint = backend::parse_progress(source, l);
    if hint.package_name.is_none()
        && hint.status.is_none()
        && hint.progress.is_none()
        && hint.phase_label.is_none()
    {
        return;
    }
    send(
        tx,
        WorkerEvent::ApplyProgress {
            source,
            package_hint: hint.package_name,
            status_hint: hint.status,
            progress: hint.progress,
            phase_label: hint
                .phase_label
                .unwrap_or_else(|| format!("Updating {}", source.label())),
        },
    );
}

fn run_helper_apply(
    tx: &Sender<WorkerEvent>,
    cmd: HelperCommand,
    tag: &str,
    source: UpdateSource,
    known_names: &[String],
) -> (i32, String) {
    let mut log = String::new();
    match with_session(|session| {
        session.run_command(cmd, |ev| match ev {
            SessionEvent::Line(l) => {
                log.push_str(&l);
                log.push('\n');
                line(tx, tag, l.clone());
                emit_progress_from_line(tx, source, &l, known_names);
            }
            SessionEvent::Finished { .. } => {}
        })
    }) {
        Ok(code) => (code, log),
        Err(e) => {
            line(tx, tag, format!("error: {e}"));
            (-1, format!("{log}\n{e}"))
        }
    }
}

fn run_local_apply(
    tx: &Sender<WorkerEvent>,
    program: &str,
    args: &[&str],
    tag: &str,
    source: UpdateSource,
    known_names: &[String],
) -> (i32, String) {
    let mut log = String::new();
    match run_local_command(program, args, |l| {
        log.push_str(&l);
        log.push('\n');
        line(tx, tag, l.clone());
        emit_progress_from_line(tx, source, &l, known_names);
    }) {
        Ok(code) => {
            // "Nothing to update" is success (exit 0) — treat as ok even if unexpected.
            (code, log)
        }
        Err(e) => {
            line(tx, tag, format!("error: {e}"));
            (-1, e.to_string())
        }
    }
}

pub const REBOOT_FAILED_TITLE: &str = "Reboot failed";
pub const REBOOT_SPAWN_FAILED_TITLE: &str = "Could not run systemctl reboot";

/// Map `systemctl reboot` output into a Failed-page detail string.
pub fn reboot_failure_detail(log: &str, code: i32) -> String {
    let trimmed = log.trim();
    if trimmed.to_ascii_lowercase().contains("block inhibitor") {
        "A process is still blocking shutdown — often an update finishing. Try again in a few seconds.".into()
    } else if !trimmed.is_empty() {
        trimmed.to_string()
    } else {
        format!("systemctl reboot exited with {code}")
    }
}

/// Retry on the Failed page re-runs reboot when the last failure was a reboot.
pub fn fail_retry_is_reboot(title: &str) -> bool {
    title == REBOOT_FAILED_TITLE || title == REBOOT_SPAWN_FAILED_TITLE
}

pub fn run_systemctl_reboot(tx: Sender<WorkerEvent>) {
    thread::spawn(move || {
        let mut log = String::new();
        match run_local_command("systemctl", &["reboot"], |l| {
            if !log.is_empty() {
                log.push('\n');
            }
            log.push_str(&l);
            line(&tx, "systemctl", l);
        }) {
            Ok(0) => {}
            Ok(code) => send(
                &tx,
                WorkerEvent::Failed {
                    message: REBOOT_FAILED_TITLE.into(),
                    detail: reboot_failure_detail(&log, code),
                },
            ),
            Err(e) => send(
                &tx,
                WorkerEvent::Failed {
                    message: REBOOT_SPAWN_FAILED_TITLE.into(),
                    detail: e.to_string(),
                },
            ),
        }
    });
}

pub fn session_started_event(purpose: AuthPurpose) -> StateEvent {
    StateEvent::SessionReady { purpose }
}

/// Close elevated helper. Call when the app window is closing — not after
/// individual Check/Apply flows, so the user is not re-prompted mid-session.
pub fn release_privileges() {
    close_session();
}

/// Whether Check/Apply can skip the polkit wait UI (helper already elevated).
pub fn can_skip_auth_ui() -> bool {
    session_is_alive()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_inhibitor_explains_try_again() {
        let detail = reboot_failure_detail(
            "Call to Reboot failed: Operation denied due to active block inhibitor",
            1,
        );
        assert!(
            detail.to_ascii_lowercase().contains("try again"),
            "expected a try-again hint, got {detail:?}"
        );
        assert!(
            !detail.contains("exited with"),
            "should not fall back to the exit-code line, got {detail:?}"
        );
    }

    #[test]
    fn other_systemctl_output_is_shown() {
        let detail = reboot_failure_detail("Failed to talk to logind", 1);
        assert_eq!(detail, "Failed to talk to logind");
    }

    #[test]
    fn empty_output_falls_back_to_exit_code() {
        let detail = reboot_failure_detail("  \n", 1);
        assert_eq!(detail, "systemctl reboot exited with 1");
    }

    #[test]
    fn retry_on_reboot_failed_retries_reboot() {
        assert!(fail_retry_is_reboot("Reboot failed"));
        assert!(fail_retry_is_reboot("Could not run systemctl reboot"));
        assert!(!fail_retry_is_reboot("All updates failed"));
        assert!(!fail_retry_is_reboot("Authentication cancelled"));
    }
}
