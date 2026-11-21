//! Sequential multi-backend check/apply using one privileged helper session.

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::SystemTime;

use async_channel::Sender;

use crate::backend::{self, dnf, flatpak, fwupd};
use crate::helper_protocol::{split_helper_line, HelperCommand, HelperOutput};
use crate::model::{AuthPurpose, Package, RetryAction, UpdateSource};
use crate::privilege::{
    close_session, run_local_command, session_is_alive, with_session, SessionError, SessionEvent,
};
use crate::state::Event as StateEvent;

#[derive(Debug, Clone)]
pub enum WorkerMsg {
    SessionStarted,
    ConsoleLine { tag: String, text: String },
    ConsoleDraw { tag: String, text: String },
    Event(StateEvent),
}

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

fn send(tx: &Sender<WorkerMsg>, msg: WorkerMsg) {
    let _ = tx.send_blocking(msg);
}

fn line(tx: &Sender<WorkerMsg>, tag: &str, text: String) {
    send(
        tx,
        WorkerMsg::ConsoleLine {
            tag: tag.into(),
            text,
        },
    );
}

/// Generation-scoped sender so a cancelled worker stops emitting events as soon as `WORK_GEN` advances.
struct WorkChannel<'a> {
    tx: &'a Sender<WorkerMsg>,
    gen: u64,
}

impl<'a> WorkChannel<'a> {
    fn is_current(&self) -> bool {
        work_is_current(self.gen)
    }

    fn send(&self, msg: WorkerMsg) {
        if self.is_current() {
            send(self.tx, msg);
        }
    }

    fn event(&self, ev: StateEvent) {
        self.send(WorkerMsg::Event(ev));
    }

    fn line(&self, tag: &str, text: String) {
        self.send(WorkerMsg::ConsoleLine {
            tag: tag.into(),
            text,
        });
    }

    fn live_line(&self, tag: &str, text: String) {
        self.send(WorkerMsg::ConsoleDraw {
            tag: tag.into(),
            text,
        });
    }
}

/// Committed lines join `log`. Live `\r` updates update the console and the
/// progress parser, and stay out of the log so a redraw cannot be parsed as
/// a second package line.
fn accept_helper_line(ch: &WorkChannel, tag: &str, log: &mut String, raw: &str) -> String {
    match split_helper_line(raw) {
        HelperOutput::Draw(text) => {
            ch.live_line(tag, text.clone());
            text
        }
        HelperOutput::Line(text) => {
            log.push_str(&text);
            log.push('\n');
            ch.line(tag, text.clone());
            text
        }
    }
}

fn map_session_start_err(ch: &WorkChannel, e: SessionError) {
    match e {
        SessionError::AuthDenied => ch.event(StateEvent::Fail {
            title: "Authentication cancelled".into(),
            detail: "polkit did not grant permission for the update helper.".into(),
            retry: RetryAction::CheckAgain,
        }),
        other => ch.event(StateEvent::Fail {
            title: "Could not start privileged helper".into(),
            detail: other.to_string(),
            retry: RetryAction::CheckAgain,
        }),
    }
}

/// Full check pipeline. Reuses the shared helper session when already elevated
/// (no second password); otherwise starts it once via polkit.
pub fn run_check_all(tx: Sender<WorkerMsg>) {
    let gen = begin_background_work();
    thread::spawn(move || {
        let ch = WorkChannel { tx: &tx, gen };
        // Ensure session up front so SessionStarted fires before long work
        let started = with_session(|_s| Ok(()));
        if !ch.is_current() {
            return;
        }
        if let Err(e) = started {
            map_session_start_err(&ch, e);
            return;
        }
        ch.send(WorkerMsg::SessionStarted);

        let mut all = Vec::new();
        let mut soft_errors = Vec::new();

        if !ch.is_current() {
            return;
        }

        // 1) DNF (privileged)
        let dnf_res = run_helper_check(&ch, HelperCommand::CheckDnf, "dnf", |log, code| {
            let mut pkgs = dnf::parse_check_update(log);
            if dnf::check_exit_is_hard_error(code, &pkgs) {
                Err(format!("dnf check-update failed (exit {code})"))
            } else {
                attach_dnf_download_sizes(&ch, &mut pkgs);
                Ok(pkgs)
            }
        });
        if !ch.is_current() {
            return;
        }
        emit_backend_check(&ch, UpdateSource::Dnf, dnf_res, &mut all, &mut soft_errors);

        // 2) Flatpak appstream (unprivileged). Names and versions come from this
        // metadata; the update set itself comes from libflatpak below.
        if !ch.is_current() {
            return;
        }
        let _ = run_local_command(
            "flatpak",
            &["update", "--user", "--appstream", "--noninteractive"],
            |l| ch.line("flatpak-user", l),
        );
        if !ch.is_current() {
            return;
        }
        let _ = run_local_command(
            "flatpak",
            &["update", "--system", "--appstream", "--noninteractive"],
            |l| ch.line("flatpak-system", l),
        );

        // 3–4) Flatpak lists. Same decision as GNOME Software: a ref is included
        // when its commit changed or a related extension still needs installing.
        if !ch.is_current() {
            return;
        }
        let flatpak_user_res =
            run_flatpak_check(&ch, true).map_err(|e| format!("flatpak user: {e}"));
        if !ch.is_current() {
            return;
        }
        emit_backend_check(
            &ch,
            UpdateSource::FlatpakUser,
            flatpak_user_res,
            &mut all,
            &mut soft_errors,
        );
        if !ch.is_current() {
            return;
        }
        let flatpak_system_res =
            run_flatpak_check(&ch, false).map_err(|e| format!("flatpak system: {e}"));
        if !ch.is_current() {
            return;
        }
        emit_backend_check(
            &ch,
            UpdateSource::FlatpakSystem,
            flatpak_system_res,
            &mut all,
            &mut soft_errors,
        );

        // 5) fwupd
        if !ch.is_current() {
            return;
        }
        let _ = run_helper_check(
            &ch,
            HelperCommand::CheckFwupdRefresh,
            "fwupd",
            |_log, _code| Ok(Vec::new()),
        );
        if !ch.is_current() {
            return;
        }

        let fwupd_res = run_helper_check(&ch, HelperCommand::CheckFwupd, "fwupd", |log, code| {
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
        });
        if !ch.is_current() {
            return;
        }
        emit_backend_check(
            &ch,
            UpdateSource::Firmware,
            fwupd_res,
            &mut all,
            &mut soft_errors,
        );

        if !ch.is_current() {
            return;
        }

        // Leave session open for Apply and later Checks (app-lifetime elevation).
        ch.event(StateEvent::CheckComplete {
            packages: all,
            soft_errors,
            listed_at: SystemTime::now(),
        });
    });
}

fn emit_backend_check(
    ch: &WorkChannel,
    source: UpdateSource,
    result: Result<Vec<Package>, String>,
    all: &mut Vec<Package>,
    soft_errors: &mut Vec<String>,
) {
    if !ch.is_current() {
        return;
    }
    match result {
        Ok(pkgs) => {
            ch.event(StateEvent::BackendCheckDone {
                source,
                packages: pkgs.clone(),
                soft_error: None,
            });
            all.extend(pkgs);
        }
        Err(e) => {
            ch.event(StateEvent::BackendCheckDone {
                source,
                packages: Vec::new(),
                soft_error: Some(e.clone()),
            });
            soft_errors.push(e);
        }
    }
}

fn attach_dnf_download_sizes(ch: &WorkChannel, pkgs: &mut [Package]) {
    if pkgs.is_empty() || !ch.is_current() {
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
                ch.line("dnf", format!("Download sizes for {n} package(s)"));
            }
        }
        Err(e) => {
            ch.line("dnf", format!("Could not query download sizes: {e}"));
        }
    }
}

fn run_helper_check<F>(
    ch: &WorkChannel,
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
                accept_helper_line(ch, tag, &mut log, &l);
            }
            SessionEvent::Finished { .. } => {}
        })
    })
    .map_err(|e| e.to_string())?;
    if !ch.is_current() {
        return Err("cancelled".into());
    }
    parse(&log, code)
}

fn run_flatpak_check(ch: &WorkChannel, user: bool) -> Result<Vec<Package>, String> {
    let tag = if user {
        "flatpak-user"
    } else {
        "flatpak-system"
    };
    let scope = if user { "user" } else { "system" };
    let check = flatpak::query_updates(user)?;
    if let Some(warning) = check.warning {
        ch.line(tag, warning);
    }
    ch.line(
        tag,
        format!("{} update(s) ({scope})", check.packages.len()),
    );
    for package in &check.packages {
        if let Some((_, installs)) = package.detail.split_once("installs ") {
            ch.line(tag, format!("{} installs {installs}", package.name));
        }
    }
    Ok(check.packages)
}

/// Apply all pending sources sequentially. Reuses the shared helper session
/// when still open (no second password).
pub fn run_apply_all(tx: Sender<WorkerMsg>, packages: Vec<Package>) {
    let gen = begin_background_work();
    thread::spawn(move || {
        let ch = WorkChannel { tx: &tx, gen };
        let sources = crate::model::sources_present(&packages);
        if sources.is_empty() {
            ch.event(StateEvent::Fail {
                title: "Nothing to apply".into(),
                detail: "Package list was empty.".into(),
                retry: RetryAction::CheckAgain,
            });
            return;
        }

        // Flatpak runs as the current user (its own polkit). Only dnf/fwupd need helper.
        let needs_session = sources
            .iter()
            .any(|s| matches!(s, UpdateSource::Dnf | UpdateSource::Firmware));

        if needs_session {
            if let Err(e) = with_session(|_s| Ok(())) {
                map_session_start_err(&ch, e);
                return;
            }
        }
        if !ch.is_current() {
            return;
        }
        ch.send(WorkerMsg::SessionStarted);

        let mut overall_reboot = false;

        for source in &sources {
            if !ch.is_current() {
                return;
            }
            let source = *source;
            let tag = source.console_tag();

            ch.line(
                "orchestrator",
                format!("── Applying {} ──", source.label()),
            );

            ch.event(StateEvent::ApplyProgress {
                source,
                package_hint: None,
                status_hint: None,
                progress: None,
                phase_label: format!("Updating {}", source.label()),
                stage_current: None,
                stage_total: None,
            });

            let (code, log) = match source {
                UpdateSource::Dnf => run_helper_apply(&ch, HelperCommand::ApplyDnf, tag, source),
                UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => {
                    // Apply the exact refs we listed at Check time, as the user.
                    // (Running system flatpak as root via helper can disagree with
                    // the user-session view and report "Nothing to update".)
                    let user = source == UpdateSource::FlatpakUser;
                    let refs: Vec<String> = packages
                        .iter()
                        .filter(|p| p.source == source)
                        .filter_map(flatpak::update_ref_from_package)
                        .collect();
                    if refs.is_empty() {
                        ch.line(tag, "No flatpak refs to update".into());
                        (0, String::new())
                    } else {
                        ch.line(
                            tag,
                            format!("Updating {} ref(s): {}", refs.len(), refs.join(", ")),
                        );
                        let args = flatpak::update_args_for_refs(user, &refs);
                        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                        run_local_apply(&ch, "flatpak", &arg_refs, tag, source)
                    }
                }
                UpdateSource::Firmware => {
                    run_helper_apply(&ch, HelperCommand::ApplyFwupd, tag, source)
                }
            };

            if !ch.is_current() {
                return;
            }

            let reboot = backend::detect_reboot(source, &log, &packages);
            if reboot {
                overall_reboot = true;
            }

            ch.event(StateEvent::BackendApplyDone {
                source,
                success: code == 0,
                needs_reboot: reboot,
            });
        }

        if !ch.is_current() {
            return;
        }

        if sources.contains(&UpdateSource::Firmware) {
            ch.line("fwupd", "Checking whether a reboot is needed…".into());
            let mut log = String::new();
            if let Ok(code) = with_session(|session| {
                session.run_command(HelperCommand::CheckFwupdReboot, |ev| {
                    if let SessionEvent::Line(l) = ev {
                        accept_helper_line(&ch, "fwupd", &mut log, &l);
                    }
                })
            }) {
                if fwupd::parse_reboot_needed_output(&log, code) {
                    overall_reboot = true;
                }
            }
        }

        if !ch.is_current() {
            return;
        }

        // Keep elevated helper for further Checks / Applies in this app session.
        ch.event(StateEvent::ApplyComplete {
            needs_reboot: overall_reboot,
        });
    });
}

fn emit_progress_from_line(ch: &WorkChannel, source: UpdateSource, l: &str) {
    let hint = backend::parse_progress(source, l);
    if hint.package_name.is_none()
        && hint.status.is_none()
        && hint.progress.is_none()
        && hint.phase_label.is_none()
    {
        return;
    }
    ch.event(StateEvent::ApplyProgress {
        source,
        package_hint: hint.package_name,
        status_hint: hint.status,
        progress: hint.progress,
        phase_label: hint.phase_label.unwrap_or_default(),
        stage_current: hint.stage_current,
        stage_total: hint.stage_total,
    });
}

fn run_helper_apply(
    ch: &WorkChannel,
    cmd: HelperCommand,
    tag: &str,
    source: UpdateSource,
) -> (i32, String) {
    let mut log = String::new();
    match with_session(|session| {
        session.run_command(cmd, |ev| match ev {
            SessionEvent::Line(l) => {
                let text = accept_helper_line(ch, tag, &mut log, &l);
                emit_progress_from_line(ch, source, &text);
            }
            SessionEvent::Finished { .. } => {}
        })
    }) {
        Ok(code) => (code, log),
        Err(e) => {
            ch.line(tag, format!("error: {e}"));
            (-1, format!("{log}\n{e}"))
        }
    }
}

fn run_local_apply(
    ch: &WorkChannel,
    program: &str,
    args: &[&str],
    tag: &str,
    source: UpdateSource,
) -> (i32, String) {
    let mut log = String::new();
    match run_local_command(program, args, |l| {
        log.push_str(&l);
        log.push('\n');
        ch.line(tag, l.clone());
        emit_progress_from_line(ch, source, &l);
    }) {
        Ok(code) => {
            // "Nothing to update" is success (exit 0) — treat as ok even if unexpected.
            (code, log)
        }
        Err(e) => {
            ch.line(tag, format!("error: {e}"));
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

fn reboot_fail_event(title: &'static str, detail: String) -> StateEvent {
    StateEvent::Fail {
        title: title.into(),
        detail,
        retry: RetryAction::Reboot,
    }
}

pub fn run_systemctl_reboot(tx: Sender<WorkerMsg>) {
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
                WorkerMsg::Event(reboot_fail_event(
                    REBOOT_FAILED_TITLE,
                    reboot_failure_detail(&log, code),
                )),
            ),
            Err(e) => send(
                &tx,
                WorkerMsg::Event(reboot_fail_event(
                    REBOOT_SPAWN_FAILED_TITLE,
                    e.to_string(),
                )),
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
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    static WORK_GEN_LOCK: Mutex<()> = Mutex::new(());

    fn lock_work_gen() -> MutexGuard<'static, ()> {
        WORK_GEN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn live_draw_is_not_a_committed_console_line() {
        use crate::helper_protocol::DRAW_PREFIX;

        let _guard = lock_work_gen();
        let (tx, rx) = async_channel::unbounded();
        let gen = begin_background_work();
        let ch = WorkChannel { tx: &tx, gen };
        let mut log = String::new();

        accept_helper_line(
            &ch,
            "dnf",
            &mut log,
            &format!("{DRAW_PREFIX}[26/26] grok-bot 40%"),
        );
        let draw = rx.try_recv().expect("draw event");
        assert!(
            matches!(
                &draw,
                WorkerMsg::ConsoleDraw { tag, text }
                    if tag == "dnf" && text == "[26/26] grok-bot 40%"
            ),
            "live draw must stay a console replacement, got {draw:?}"
        );
        assert!(log.is_empty(), "live draw must stay out of log: {log:?}");

        accept_helper_line(&ch, "dnf", &mut log, "[26/26] grok-bot 100%");
        let committed = rx.try_recv().expect("line event");
        assert!(matches!(committed, WorkerMsg::ConsoleLine { .. }));
        assert!(log.contains("[26/26] grok-bot 100%"));
    }

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
        assert!(matches!(
            reboot_fail_event(REBOOT_FAILED_TITLE, "blocked".into()),
            StateEvent::Fail {
                retry: RetryAction::Reboot,
                ..
            }
        ));
        assert!(matches!(
            reboot_fail_event(REBOOT_SPAWN_FAILED_TITLE, "missing".into()),
            StateEvent::Fail {
                retry: RetryAction::Reboot,
                ..
            }
        ));

        let _guard = lock_work_gen();
        let (tx, rx) = async_channel::unbounded();
        let gen = begin_background_work();
        let ch = WorkChannel { tx: &tx, gen };
        map_session_start_err(&ch, SessionError::AuthDenied);
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::Event(StateEvent::Fail {
                retry: RetryAction::CheckAgain,
                ..
            }))
        ));
    }

    #[test]
    fn work_channel_drops_events_after_cancel() {
        let _guard = lock_work_gen();
        let (tx, rx) = async_channel::unbounded();
        let gen = begin_background_work();
        let ch = WorkChannel { tx: &tx, gen };

        ch.line("dnf", "line before cancel".into());
        ch.live_line("dnf", "draw before cancel".into());
        emit_progress_from_line(
            &ch,
            UpdateSource::Dnf,
            "[1/2] Upgrading firefox-140.0-1.fc44.x86_64 50%",
        );

        cancel_background_work();

        ch.line("dnf", "line after cancel".into());
        ch.live_line("dnf", "draw after cancel".into());
        emit_progress_from_line(
            &ch,
            UpdateSource::Dnf,
            "[2/2] Upgrading kernel-6.14.11-300.fc44.x86_64 100%",
        );

        let mut drained = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            drained.push(msg);
        }
        assert_eq!(drained.len(), 3);
        assert!(matches!(
            &drained[0],
            WorkerMsg::ConsoleLine { text, .. } if text == "line before cancel"
        ));
        assert!(matches!(
            &drained[1],
            WorkerMsg::ConsoleDraw { text, .. } if text == "draw before cancel"
        ));
        assert!(matches!(
            &drained[2],
            WorkerMsg::Event(StateEvent::ApplyProgress {
                package_hint: Some(name),
                ..
            }) if name == "firefox"
        ));
    }
}
