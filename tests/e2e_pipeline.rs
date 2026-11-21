//! End-to-End pipeline tests (Layer A): real wire-protocol helper subprocess
//! over OS pipes (`PrivilegedSession::start_direct`) + `orchestrator` +
//! `state::reduce` + `ConsoleBuffer` + `presenter`.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use async_channel::Receiver;
use common::fixtures::read_fixture;
use common::helper_e2e::PipeHelperRuntime;
use common::runtime::flatpak_check_from_fixture;
use fedora_updater::backend::dnf;
use fedora_updater::console::{ConsoleBuffer, ConsoleDelta};
use fedora_updater::helper_protocol::{HelperCommand, BYE_LINE, HELLO_LINE};
use fedora_updater::model::{
    total_download_bytes, IdleScreen, PackageStatus, RetryAction, RunPass, UpdateSource,
};
use fedora_updater::orchestrator::{
    execute_apply_all, execute_check_all, execute_systemctl_reboot, run_apply_all_with,
    session_started_event, WorkerMsg, WorkCancel, REBOOT_FAILED_TITLE,
};
use fedora_updater::presenter::{
    done_header_subtitle, fail_retry_label, idle_chrome, run_heading, soft_error_banner_title,
    source_summary_line, stale_update_dialog, stop_update_dialog, window_header, HeaderView,
};
use fedora_updater::privilege::HELPER_FLAG;
use fedora_updater::state::{
    phase_name, reduce, session_started_purpose, AppState, Event, Phase, UPDATE_LIST_MAX_AGE,
};

struct PipelineHarness {
    state: AppState,
    console: ConsoleBuffer,
    deltas: Vec<ConsoleDelta>,
    phases: Vec<&'static str>,
    draw_count: usize,
    saw_download_pass: bool,
    saw_apply_pass: bool,
    saw_apply_progress_reset: bool,
    saw_verifying_heading: bool,
}

impl Default for PipelineHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineHarness {
    fn new() -> Self {
        Self {
            state: AppState::default(),
            console: ConsoleBuffer::new(2_000),
            deltas: Vec::new(),
            phases: vec![phase_name(&AppState::default().phase)],
            draw_count: 0,
            saw_download_pass: false,
            saw_apply_pass: false,
            saw_apply_progress_reset: false,
            saw_verifying_heading: false,
        }
    }

    fn dispatch(&mut self, event: Event) {
        reduce(&mut self.state, event).expect("valid state transition");
        let name = phase_name(&self.state.phase);
        if self.phases.last().copied() != Some(name) {
            self.phases.push(name);
        }
        if let Phase::Running(running) = &self.state.phase {
            if running.pass == RunPass::Download {
                self.saw_download_pass = true;
            }
            if running.pass == RunPass::Apply {
                if !self.saw_apply_pass && (running.overall_progress - 0.0).abs() < f64::EPSILON {
                    self.saw_apply_progress_reset = true;
                }
                self.saw_apply_pass = true;
            }
            if run_heading(&running.phase_label, running.pass) == "Verifying" {
                self.saw_verifying_heading = true;
            }
        }
    }

    fn accept_msg(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::SessionStarted => {
                if let Some(purpose) = session_started_purpose(&self.state.phase) {
                    self.dispatch(session_started_event(purpose));
                }
            }
            WorkerMsg::ConsoleLine { tag, text } => {
                let d = self.console.push_committed_tagged(&tag, &text);
                self.deltas.push(d);
            }
            WorkerMsg::ConsoleDraw { tag, text } => {
                let d = self.console.push_live_tagged(&tag, &text);
                self.deltas.push(d);
                self.draw_count += 1;
            }
            WorkerMsg::Throughput { .. } => {}
            WorkerMsg::Event(ev) => self.dispatch(ev),
        }
    }

    fn drain(&mut self, rx: &Receiver<WorkerMsg>) {
        while let Ok(msg) = rx.try_recv() {
            self.accept_msg(msg);
        }
    }

    fn run_check(&mut self, rt: &PipeHelperRuntime, then_apply: bool) {
        self.console.clear();
        self.dispatch(Event::StartCheck { then_apply });
        let (tx, rx) = async_channel::unbounded();
        let cancel = WorkCancel::new();
        execute_check_all(&tx, rt, &cancel);
        drop(tx);
        self.drain(&rx);
    }

    fn run_apply(&mut self, rt: &PipeHelperRuntime) {
        let packages = self.state.packages().to_vec();
        self.console.clear();
        self.dispatch(Event::StartApply);
        let (tx, rx) = async_channel::unbounded();
        let cancel = WorkCancel::new();
        execute_apply_all(&tx, packages, rt, &cancel);
        drop(tx);
        self.drain(&rx);
    }

    fn run_reboot(&mut self, rt: &PipeHelperRuntime) {
        let (tx, rx) = async_channel::unbounded();
        execute_systemctl_reboot(&tx, rt);
        drop(tx);
        self.drain(&rx);
    }
}

fn push_empty_four_source_check(rt: &PipeHelperRuntime) {
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_empty.txt", 0);
    rt.local().push_flatpak_appstream_ok();
    rt.local().push_flatpak_empty(true);
    rt.local().push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], 0);
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_no_updates.txt", 2);
}

#[test]
fn e2e_fake_helper_honors_quit_bye_protocol() {
    let rt = PipeHelperRuntime::new();
    let mut child = {
        let _guard = rt.fork_lock().read().unwrap();
        Command::new(rt.script_path())
            .arg(HELPER_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fake helper")
    };

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut hello = String::new();
    stdout.read_line(&mut hello).unwrap();
    assert_eq!(hello.trim_end(), HELLO_LINE);

    writeln!(stdin, "QUIT").unwrap();
    stdin.flush().unwrap();

    let mut bye = String::new();
    stdout.read_line(&mut bye).unwrap();
    assert_eq!(bye.trim_end(), BYE_LINE);

    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn e2e_journey_1_happy_path_check_apply_and_reboot_inhibitor_recovery() {
    let rt = PipeHelperRuntime::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    rt.local().set_now(t0);

    // Phase 1: 4-source Check over real helper pipe + local fixture sources.
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_classic.txt", 100);
    rt.local().push_local_fixture(
        "dnf",
        dnf::REPOQUERY_UPGRADES_ARGS,
        "dnf/repoquery_sizes.txt",
        0,
    );
    rt.local().push_flatpak_appstream_ok();
    rt.local().push_flatpak_query(
        true,
        Ok(flatpak_check_from_fixture("flatpak/check_user_normal.json", None)),
    );
    rt.local().push_flatpak_query(
        false,
        Ok(flatpak_check_from_fixture(
            "flatpak/check_system_nvidia_extension.json",
            None,
        )),
    );
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], 0);
    rt.push_helper_fixture(
        HelperCommand::CheckFwupd,
        "fwupd/get_updates_multi_device.json",
        0,
    );

    // Phase 2: Full 4-source Apply + post-apply check-reboot-needed on SAME helper process.
    rt.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_happy_kernel.txt", 0);
    rt.local().push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_progress_happy.txt",
        0,
    );
    rt.local().push_local_fixture(
        "flatpak",
        &["update", "--system", "-y", "--noninteractive"],
        "flatpak/apply_nothing_to_update.txt",
        0,
    );
    rt.push_helper_fixture(HelperCommand::ApplyFwupd, "fwupd/apply_happy_reboot.txt", 0);
    rt.push_helper_fixture(
        HelperCommand::CheckFwupdReboot,
        "fwupd/check_reboot_needed_true.json",
        0,
    );

    // Phase 3: Reboot hits systemd block inhibitor first, then succeeds on Retry.
    rt.local().push_local_fixture(
        "systemctl",
        &["reboot"],
        "systemctl/reboot_block_inhibitor.txt",
        1,
    );
    rt.local().push_local_lines("systemctl", &["reboot"], vec![], Ok(0));

    let mut h = PipelineHarness::new();
    h.run_check(&rt, false);

    assert!(rt.has_live_session());
    assert_eq!(rt.recorded_pids().len(), 1);
    let helper_pid = rt.recorded_pids()[0];

    let Phase::Ready {
        ref packages,
        ref soft_errors,
    } = h.state.phase
    else {
        panic!("expected Ready after check, got {:?}", h.state.phase);
    };
    assert!(soft_errors.is_empty());
    assert_eq!(packages.len(), 17);
    let expected_bytes =
        45_252_164_u64 + 145_752_064_u64 + 251_000_000_u64 + 391_118_848_u64;
    assert_eq!(total_download_bytes(packages), Some(expected_bytes));
    let steam = packages
        .iter()
        .find(|p| p.name == "Steam" && p.source == UpdateSource::FlatpakSystem)
        .expect("Steam system package");
    assert!(steam.detail.contains("nvidia"));

    let summary = source_summary_line(packages);
    let ready_hdr = window_header(
        HeaderView::Ready {
            count: packages.len(),
            summary: &summary,
        },
        false,
    );
    assert_eq!(ready_hdr.title, "17 Updates Available");

    // Apply across all 4 sources reusing the exact same helper PID.
    h.run_apply(&rt);

    assert_eq!(
        rt.recorded_pids(),
        vec![helper_pid],
        "Check + Apply must reuse a single helper PID without spawning a second helper"
    );
    assert_eq!(
        rt.recorded_invocations(),
        vec![
            ("check-dnf".into(), helper_pid),
            ("check-fwupd-refresh".into(), helper_pid),
            ("check-fwupd".into(), helper_pid),
            ("apply-dnf".into(), helper_pid),
            ("apply-fwupd".into(), helper_pid),
            ("check-fwupd-reboot".into(), helper_pid),
        ]
    );
    assert!(h.draw_count >= 5, "expected real __DRAW__ frames from pipe");
    assert!(h.saw_download_pass);
    assert!(h.saw_apply_pass);
    assert!(h.saw_apply_progress_reset);
    assert!(h.saw_verifying_heading);

    // Intermediate \r redraw percentages must not pollute the committed log.
    let log_text = h.console.full_text();
    assert!(!log_text.contains("10% |   1.1 MiB/s"), "{log_text}");
    assert!(log_text.contains("Complete!"));
    assert!(log_text.contains("Successfully installed firmware"));

    let Phase::Done {
        packages: ref done_pkgs,
        failed,
        needs_reboot,
        ref failure_reasons,
        ref duration,
    } = h.state.phase
    else {
        panic!("expected Done, got {:?}", h.state.phase);
    };
    assert_eq!(failed, 0);
    assert!(needs_reboot);
    assert!(failure_reasons.is_empty());
    assert!(done_pkgs.iter().all(|p| p.status == PackageStatus::Completed));
    let done_hdr = window_header(
        HeaderView::Done {
            failed: false,
            subtitle: &done_header_subtitle(0, 17, 8, 6, 3, duration),
        },
        false,
    );
    assert_eq!(done_hdr.title, "All Updates Applied");

    // Reboot hits block inhibitor -> Failed(RetryAction::Reboot), then Retry Reboot succeeds.
    h.run_reboot(&rt);
    match &h.state.phase {
        Phase::Failed {
            title,
            detail,
            retry: RetryAction::Reboot,
        } => {
            assert_eq!(title, REBOOT_FAILED_TITLE);
            assert!(detail.contains("blocking shutdown"), "{detail}");
            assert!(detail.contains("Try again"), "{detail}");
            assert_eq!(fail_retry_label(RetryAction::Reboot), "Restart");
        }
        other => panic!("expected Failed(Reboot) for block inhibitor, got {other:?}"),
    }

    h.run_reboot(&rt);
    // Second reboot succeeds without emitting any Fail event.
    assert!(matches!(
        &h.state.phase,
        Phase::Failed {
            retry: RetryAction::Reboot,
            ..
        }
    ));
    assert_eq!(
        rt.local()
            .local_calls()
            .iter()
            .filter(|(p, a)| p == "systemctl" && a == &vec!["reboot".to_string()])
            .count(),
        2
    );
}

#[test]
fn e2e_journey_2_empty_then_updates_partial_failure_and_check_again_recovery() {
    let rt = PipeHelperRuntime::new();

    // Check #1: Empty across DNF / Flatpak / fwupd (exit 2).
    push_empty_four_source_check(&rt);

    // Check #2: Same helper finds DNF updates + Flatpak user updates.
    rt.push_helper_lines(
        HelperCommand::CheckDnf,
        vec![
            "firefox.x86_64 140.0-1.fc44 updates".into(),
            "glib2.x86_64 2.84.1-1.fc44 updates".into(),
        ],
        100,
    );
    rt.local()
        .push_local_lines("dnf", dnf::REPOQUERY_UPGRADES_ARGS, vec![], Ok(0));
    rt.local().push_flatpak_appstream_ok();
    rt.local().push_flatpak_query(
        true,
        Ok(flatpak_check_from_fixture("flatpak/check_user_normal.json", None)),
    );
    rt.local().push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], 0);
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_empty.json", 2);

    // Apply: DNF fails with dependency conflict, Flatpak succeeds.
    rt.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_dep_conflict.txt", 1);
    rt.local().push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_progress_happy.txt",
        0,
    );

    // Check Again recovery after Warning idle: reuses same helper PID and finds no updates.
    push_empty_four_source_check(&rt);

    let mut h = PipelineHarness::new();
    h.run_check(&rt, false);
    assert!(matches!(
        &h.state.phase,
        Phase::Idle(IdleScreen::Current { detail: None, .. })
    ));
    let pid = rt.recorded_pids()[0];

    h.run_check(&rt, false);
    assert!(matches!(&h.state.phase, Phase::Ready { packages, .. } if packages.len() == 5));
    assert_eq!(rt.recorded_pids(), vec![pid]);

    h.run_apply(&rt);
    assert_eq!(rt.recorded_pids(), vec![pid]);
    match &h.state.phase {
        Phase::Done {
            packages,
            failed,
            needs_reboot,
            failure_reasons,
            ..
        } => {
            assert_eq!(*failed, 2);
            assert!(!*needs_reboot);
            assert_eq!(failure_reasons.len(), 1);
            assert!(
                failure_reasons[0].contains("dependency conflict")
                    && failure_reasons[0].contains("Problem 1:"),
                "{failure_reasons:?}"
            );
            assert!(packages
                .iter()
                .filter(|p| p.source == UpdateSource::Dnf)
                .all(|p| p.status == PackageStatus::Failed));
            assert!(packages
                .iter()
                .filter(|p| p.source == UpdateSource::FlatpakUser)
                .all(|p| p.status == PackageStatus::Completed));
        }
        other => panic!("expected partial Done, got {other:?}"),
    }
    assert!(h
        .console
        .full_text()
        .contains("Problem 2: problem with installed package"));

    h.dispatch(Event::ResetToIdle);
    let Phase::Idle(ref screen @ IdleScreen::Warning { title, ref detail, .. }) = h.state.phase
    else {
        panic!("expected Warning idle, got {:?}", h.state.phase);
    };
    assert_eq!(title, "Some updates failed");
    assert!(detail.contains("Problem 1:"));
    assert!(idle_chrome(screen).show_view_log);

    // Recover by clicking "Check Again" on the Warning idle page.
    h.run_check(&rt, false);
    assert!(matches!(
        &h.state.phase,
        Phase::Idle(IdleScreen::Current { detail: None, .. })
    ));
    assert_eq!(
        rt.recorded_pids(),
        vec![pid],
        "all 4 stages must share the original helper PID"
    );
}

#[test]
fn e2e_journey_3_auth_denied_stale_list_refresh_apply_and_mid_install_stop_recovery() {
    let rt = PipeHelperRuntime::new();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    rt.local().set_now(t0);

    // 1) Initial polkit spawn fails (helper exits 1 before HELLO_LINE -> AuthDenied).
    rt.deny_next_start();

    let mut h = PipelineHarness::new();
    h.run_check(&rt, false);
    assert!(matches!(
        &h.state.phase,
        Phase::Failed {
            title,
            retry: RetryAction::CheckAgain,
            ..
        } if title == "Authentication cancelled"
    ));
    assert!(rt.recorded_pids().is_empty());
    assert!(!rt.has_live_session());

    // 2) Retry Check succeeds, spawns helper PID P1, reaches Ready at t0.
    let dnf_4 = vec![
        "openssl.x86_64 1:3.2.2-3.fc44 updates".into(),
        "firefox.x86_64 140.0-1.fc44 updates".into(),
        "kernel-tools.x86_64 7.1.12-200.fc44 updates".into(),
        "kernel-core.x86_64 7.1.12-200.fc44 updates".into(),
    ];
    rt.push_helper_lines(HelperCommand::CheckDnf, dnf_4.clone(), 100);
    rt.local()
        .push_local_lines("dnf", dnf::REPOQUERY_UPGRADES_ARGS, vec![], Ok(0));
    rt.local().push_flatpak_appstream_ok();
    rt.local().push_flatpak_empty(true);
    rt.local().push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], 0);
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_empty.json", 2);

    h.run_check(&rt, false);
    assert_eq!(rt.recorded_pids().len(), 1);
    let pid1 = rt.recorded_pids()[0];
    assert!(matches!(&h.state.phase, Phase::Ready { packages, .. } if packages.len() == 4));

    // 3) Advance clock past 24h -> Update All prompts stale dialog -> refresh & auto-apply.
    rt.local()
        .set_now(t0 + UPDATE_LIST_MAX_AGE + Duration::from_secs(60));
    assert!(h.state.update_list_needs_refresh(rt.local().current_now()));
    let stale = stale_update_dialog();
    assert_eq!(stale.heading, "Refresh Update List?");
    assert_eq!(stale.confirm_id, "yes");

    rt.push_helper_lines(HelperCommand::CheckDnf, dnf_4, 100);
    rt.local()
        .push_local_lines("dnf", dnf::REPOQUERY_UPGRADES_ARGS, vec![], Ok(0));
    rt.local().push_flatpak_appstream_ok();
    rt.local().push_flatpak_empty(true);
    rt.local().push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], 0);
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_empty.json", 2);

    h.run_check(&rt, true);
    assert!(h.state.apply_when_ready);
    assert_eq!(rt.recorded_pids(), vec![pid1]);
    let apply_pkgs = h.state.packages().to_vec();
    assert_eq!(apply_pkgs.len(), 4);

    // 4) Auto-apply pauses inside real OS helper subprocess during Installing kernel-core.
    let all_apply_lines =
        common::runtime::fixture_to_helper_lines(&read_fixture("dnf/apply_happy_kernel.txt"));
    let stop_at = all_apply_lines
        .iter()
        .position(|l| l.contains("Installing kernel-core"))
        .expect("Installing kernel-core line");
    let gate = rt.push_helper_paused_lines(
        HelperCommand::ApplyDnf,
        all_apply_lines[..=stop_at].to_vec(),
    );

    h.console.clear();
    h.dispatch(Event::StartApply);
    assert!(!h.state.apply_when_ready);
    let (tx, rx) = async_channel::unbounded();
    let cancel = run_apply_all_with(tx, apply_pkgs, rt.clone());

    gate.wait_until_reached(Duration::from_secs(5));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        h.drain(&rx);
        if matches!(
            &h.state.phase,
            Phase::Running(r)
                if r.is_applying()
                    && r.packages
                        .iter()
                        .any(|p| p.name == "kernel-core" && p.status == PackageStatus::Installing)
        ) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let during_tx = match &h.state.phase {
        Phase::Running(r) => {
            assert!(r.is_applying());
            assert_eq!(
                r.packages
                    .iter()
                    .find(|p| p.name == "openssl")
                    .unwrap()
                    .status,
                PackageStatus::Completed
            );
            assert_eq!(
                r.packages
                    .iter()
                    .find(|p| p.name == "kernel-core")
                    .unwrap()
                    .status,
                PackageStatus::Installing
            );
            let stop_dlg = stop_update_dialog();
            assert_eq!(stop_dlg.heading, "Stop This Update?");
            assert!(stop_dlg.confirm_destructive);
            true
        }
        other => panic!("expected Running in Apply pass before Stop, got {other:?}"),
    };

    // Confirm mid-install Stop: cancel token + SIGKILL real helper subprocess + dispatch StopApply.
    cancel.cancel();
    rt.abort_session();
    while let Ok(msg) = rx.recv_blocking() {
        h.accept_msg(msg);
    }
    h.dispatch(Event::StopApply {
        during_transaction: during_tx,
    });
    assert!(!rt.has_live_session());

    match &h.state.phase {
        Phase::Failed {
            title,
            detail,
            retry,
        } => {
            assert_eq!(title, "Update stopped during install");
            assert!(detail.contains("dnf history"));
            assert_eq!(*retry, RetryAction::CheckAgain);
        }
        other => panic!("expected Failed RPM-DB warning, got {other:?}"),
    }

    // 5) Recovery Check spawns a fresh helper PID P2 != P1 and completes cleanly.
    push_empty_four_source_check(&rt);
    h.run_check(&rt, false);
    assert!(matches!(
        &h.state.phase,
        Phase::Idle(IdleScreen::Current { .. })
    ));
    let pids = rt.recorded_pids();
    assert_eq!(pids.len(), 2);
    assert_ne!(pids[0], pids[1]);
}

#[test]
fn e2e_journey_4_helper_crash_eof_mid_apply_spawns_new_session_on_check() {
    let rt = PipeHelperRuntime::new();

    // Check spawns PID P1 -> Ready with 1 DNF package.
    rt.push_helper_lines(
        HelperCommand::CheckDnf,
        vec!["firefox.x86_64 140.0-1.fc44 updates".into()],
        100,
    );
    rt.local()
        .push_local_lines("dnf", dnf::REPOQUERY_UPGRADES_ARGS, vec![], Ok(0));
    rt.local().push_flatpak_appstream_ok();
    rt.local().push_flatpak_empty(true);
    rt.local().push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], 0);
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_empty.json", 2);

    // ApplyDnf crashes mid-stream (exits without __END__) -> pipe EOF -> SessionError::Closed.
    rt.push_helper_crash_after_lines(
        HelperCommand::ApplyDnf,
        vec![
            "Updating and loading repositories:".into(),
            "Repositories loaded.".into(),
            "[1/1] firefox-0:140.0-1.fc44.x86_64 42%".into(),
        ],
    );

    // Next Check recovers transparently by spawning PID P2 -> Ready with a fresh package + soft notice check.
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_repo_error.txt", 1);
    rt.local().push_flatpak_appstream_ok();
    rt.local().push_flatpak_query(
        true,
        Ok(flatpak_check_from_fixture("flatpak/check_user_normal.json", None)),
    );
    rt.local().push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], 0);
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_empty.json", 2);

    let mut h = PipelineHarness::new();
    h.run_check(&rt, false);
    assert_eq!(rt.recorded_pids().len(), 1);
    let pid1 = rt.recorded_pids()[0];
    assert!(rt.has_live_session());

    h.run_apply(&rt);
    assert!(
        !rt.has_live_session(),
        "crashed helper EOF must evict the broken session"
    );
    match &h.state.phase {
        Phase::Failed {
            title,
            detail,
            retry: RetryAction::CheckAgain,
        } => {
            assert_eq!(title, "All updates failed");
            assert!(
                detail.contains("helper closed unexpectedly") || detail.contains("System:"),
                "{detail}"
            );
        }
        other => panic!("expected Failed after helper crash EOF, got {other:?}"),
    }
    assert!(h.console.full_text().contains("Repositories loaded."));

    // Subsequent Check transparently spawns a fresh helper PID P2 and reaches Ready (with DNF soft banner).
    h.run_check(&rt, false);
    let pids = rt.recorded_pids();
    assert_eq!(pids.len(), 2);
    assert_eq!(pids[0], pid1);
    assert_ne!(pids[0], pids[1]);
    assert!(rt.has_live_session());
    match &h.state.phase {
        Phase::Ready {
            packages,
            soft_errors,
        } => {
            assert_eq!(packages.len(), 3);
            assert_eq!(soft_errors.len(), 1);
            assert_eq!(
                soft_error_banner_title(soft_errors.len()),
                "1 source could not be checked"
            );
        }
        other => panic!("expected Ready with soft error after respawn, got {other:?}"),
    }
}
