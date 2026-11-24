//! Offline functional flows and UX-recovery tests exercising the full
//! `orchestrator -> WorkerMsg -> state::reduce -> ConsoleBuffer -> presenter` pipeline
//! against checked-in `tests/fixtures/` via [`FakeRuntime`].

mod common;

use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use async_channel::Receiver;
use common::fixtures::{read_fixture, read_fixture_value};
use common::runtime::{
    fixture_to_helper_lines, flatpak_check_from_fixture, DnfDbusApplyStep, FakeRuntime,
    FlatpakNativeApplyStep, FwupdDbusApplyStep, HelperStep,
};
use fedora_updater::backend::dnf_dbus::{
    merge_history, packages_from_dnf_dbus, parse_list_fd_ndjson, DnfAdvisoryRecord,
    DnfApplyOutcome, DnfDbusSignal, DnfHistoryTransaction, DnfRpmRecord, DnfSignalTracker,
    HistorySource,
};
use fedora_updater::backend::flatpak::{
    FlatpakCancellable, FlatpakOperationMeta, FlatpakProgressSnapshot,
    FlatpakTransactionTracker, FLATPAK_TRANSACTION_ERROR_DETAILS_NON_FATAL,
};
use fedora_updater::backend::fwupd_dbus::{
    devices_need_reboot, evaluate_battery_safety, package_from_device_and_release,
    FwupdApplyOutcome, FwupdDevice, FwupdHistoryEntry, FwupdHostPower, FwupdStatus,
    FwupdUpdateState,
};
use fedora_updater::backend::logind::{RebootPreflight, ShutdownInhibitor};
use fedora_updater::backend::{dnf, NativeOutcome};
use fedora_updater::console::{ConsoleBuffer, ConsoleDelta};
use fedora_updater::helper_protocol::{HelperCommand, DRAW_PREFIX};
use fedora_updater::model::{
    count_security, format_bytes, total_download_bytes, AdvisoryKind, ApplyMode, IdleScreen,
    Package, PackageStatus, RetryAction, RunPass, UpdateSource,
};
use fedora_updater::orchestrator::{
    can_skip_apply_auth_ui, can_skip_check_auth_ui, execute_apply_all, execute_apply_all_with_mode,
    execute_apply_all_with_mode_and_config, execute_check_all, execute_check_all_with_config,
    execute_load_history, execute_systemctl_reboot, run_apply_all_with, run_check_all_with,
    run_systemctl_reboot_with, session_started_event, SourceConfig, WorkCancel, WorkerMsg,
    REBOOT_FAILED_TITLE, REBOOT_SPAWN_FAILED_TITLE,
};
use fedora_updater::presenter::{
    about_time_left, check_progress, checking_footnote, done_header_subtitle, fail_retry_label,
    idle_chrome, now_playing_subtitle, now_playing_title, quiet_ui, run_heading,
    running_progress_captions, soft_error_banner_title, source_summary_line, stale_update_dialog,
    stop_update_dialog, window_header, HeaderView, QuietInput, QuietMode,
};
use fedora_updater::privilege::SessionError;
use fedora_updater::state::{
    phase_name, reduce, session_started_purpose, AppState, Event, Phase, UPDATE_LIST_MAX_AGE,
};

struct FlowHarness {
    state: AppState,
    console: ConsoleBuffer,
    deltas: Vec<ConsoleDelta>,
    throughput_samples: Vec<(f64, u64, u64)>,
    phases: Vec<&'static str>,
    check_snapshots: Vec<(Vec<UpdateSource>, f64, u32, &'static str)>,
    draw_count: usize,
    saw_download_pass: bool,
    saw_apply_pass: bool,
    saw_apply_progress_reset: bool,
    saw_verifying_heading: bool,
}

impl Default for FlowHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowHarness {
    fn new() -> Self {
        Self::with_state(AppState::default())
    }

    fn with_state(state: AppState) -> Self {
        let initial_phase = phase_name(&state.phase);
        Self {
            state,
            console: ConsoleBuffer::new(2_000),
            deltas: Vec::new(),
            throughput_samples: Vec::new(),
            phases: vec![initial_phase],
            check_snapshots: Vec::new(),
            draw_count: 0,
            saw_download_pass: false,
            saw_apply_pass: false,
            saw_apply_progress_reset: false,
            saw_verifying_heading: false,
        }
    }

    fn ready(packages: Vec<Package>) -> Self {
        Self::with_state(AppState {
            phase: Phase::Ready {
                packages,
                soft_errors: vec![],
            },
            ..Default::default()
        })
    }

    fn dispatch(&mut self, event: Event) {
        reduce(&mut self.state, event).expect("valid state transition");
        let name = phase_name(&self.state.phase);
        if self.phases.last().copied() != Some(name) {
            self.phases.push(name);
        }
        match &self.state.phase {
            Phase::Checking {
                checked_sources, ..
            } => {
                let (frac, groups, status) = check_progress(checked_sources);
                self.check_snapshots
                    .push((checked_sources.clone(), frac, groups, status));
            }
            Phase::Running(running) => {
                if running.pass == RunPass::Download {
                    self.saw_download_pass = true;
                }
                if running.pass == RunPass::Apply {
                    if !self.saw_apply_pass && (running.overall_progress - 0.0).abs() < f64::EPSILON
                    {
                        self.saw_apply_progress_reset = true;
                    }
                    self.saw_apply_pass = true;
                }
                if run_heading(&running.phase_label, running.pass) == "Verifying" {
                    self.saw_verifying_heading = true;
                }
            }
            _ => {}
        }
    }

    fn drain(&mut self, rx: &Receiver<WorkerMsg>) {
        while let Ok(msg) = rx.try_recv() {
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
                WorkerMsg::Throughput {
                    bytes_per_sec,
                    downloaded_bytes,
                    total_bytes,
                } => {
                    self.throughput_samples
                        .push((bytes_per_sec, downloaded_bytes, total_bytes));
                }
                WorkerMsg::Event(ev) => self.dispatch(ev),
            }
        }
    }

    fn run_check(&mut self, rt: &FakeRuntime, then_apply: bool) {
        self.console.clear();
        self.dispatch(Event::StartCheck { then_apply });
        let (tx, rx) = async_channel::unbounded();
        let cancel = WorkCancel::new();
        execute_check_all(&tx, rt, &cancel);
        drop(tx);
        self.drain(&rx);
    }

    fn run_apply(&mut self, rt: &FakeRuntime) {
        let packages = self.state.packages().to_vec();
        self.console.clear();
        self.dispatch(Event::StartApply);
        let (tx, rx) = async_channel::unbounded();
        let cancel = WorkCancel::new();
        execute_apply_all(&tx, packages, rt, &cancel);
        drop(tx);
        self.drain(&rx);
    }

    fn run_apply_mode(&mut self, rt: &FakeRuntime, selected_ids: Vec<String>, mode: ApplyMode) {
        self.console.clear();
        self.dispatch(Event::StartSelectiveApply { selected_ids, mode });
        let packages = self.state.packages().to_vec();
        let (tx, rx) = async_channel::unbounded();
        let cancel = WorkCancel::new();
        execute_apply_all_with_mode(&tx, packages, mode, rt, &cancel);
        drop(tx);
        self.drain(&rx);
    }

    fn run_reboot(&mut self, rt: &FakeRuntime) {
        let (tx, rx) = async_channel::unbounded();
        execute_systemctl_reboot(&tx, rt);
        drop(tx);
        self.drain(&rx);
    }
}

fn wait_until_rx_closed(h: &mut FlowHarness, rx: &Receiver<WorkerMsg>) {
    while let Ok(msg) = rx.recv_blocking() {
        match msg {
            WorkerMsg::SessionStarted => {
                if let Some(purpose) = session_started_purpose(&h.state.phase) {
                    h.dispatch(session_started_event(purpose));
                }
            }
            WorkerMsg::ConsoleLine { tag, text } => {
                let d = h.console.push_committed_tagged(&tag, &text);
                h.deltas.push(d);
            }
            WorkerMsg::ConsoleDraw { tag, text } => {
                let d = h.console.push_live_tagged(&tag, &text);
                h.deltas.push(d);
                h.draw_count += 1;
            }
            WorkerMsg::Throughput {
                bytes_per_sec,
                downloaded_bytes,
                total_bytes,
            } => {
                h.throughput_samples
                    .push((bytes_per_sec, downloaded_bytes, total_bytes));
            }
            WorkerMsg::Event(ev) => h.dispatch(ev),
        }
    }
}

fn push_happy_four_source_check(rt: &FakeRuntime) {
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_classic.txt", 100);
    rt.push_local_fixture(
        "dnf",
        dnf::REPOQUERY_UPGRADES_ARGS,
        "dnf/repoquery_sizes.txt",
        0,
    );
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_query(
        true,
        Ok(flatpak_check_from_fixture(
            "flatpak/check_user_normal.json",
            None,
        )),
    );
    rt.push_flatpak_query(
        false,
        Ok(flatpak_check_from_fixture(
            "flatpak/check_system_nvidia_extension.json",
            Some("Using offline cached flatpak metadata"),
        )),
    );
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], Ok(0));
    rt.push_helper_fixture(
        HelperCommand::CheckFwupd,
        "fwupd/get_updates_multi_device.json",
        0,
    );
}

#[test]
fn check_full_four_sources_happy_path() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let rt = FakeRuntime::new();
    rt.set_now(t0);
    push_happy_four_source_check(&rt);

    let mut h = FlowHarness::default();
    h.run_check(&rt, false);

    assert_eq!(h.phases, vec!["Idle", "Authenticating", "Checking", "Ready"]);
    assert_eq!(rt.ensure_session_calls(), 1);
    assert_eq!(
        rt.helper_calls(),
        vec![
            HelperCommand::CheckDnf,
            HelperCommand::CheckFwupdRefresh,
            HelperCommand::CheckFwupd,
        ]
    );
    assert_eq!(rt.flatpak_queries(), vec![true, false]);
    assert_eq!(
        rt.local_calls(),
        vec![
            (
                "dnf".into(),
                dnf::REPOQUERY_UPGRADES_ARGS
                    .iter()
                    .map(|s| (*s).into())
                    .collect()
            ),
            (
                "flatpak".into(),
                vec![
                    "update".into(),
                    "--user".into(),
                    "--appstream".into(),
                    "--noninteractive".into(),
                ]
            ),
            (
                "flatpak".into(),
                vec![
                    "update".into(),
                    "--system".into(),
                    "--appstream".into(),
                    "--noninteractive".into(),
                ]
            ),
        ]
    );

    // Check progress stepped sequentially through Dnf -> FlatpakUser -> FlatpakSystem -> Firmware.
    assert!(h
        .check_snapshots
        .iter()
        .any(|(srcs, frac, groups, _)| srcs.as_slice() == [UpdateSource::Dnf]
            && (*frac - 0.25).abs() < 1e-9
            && *groups == 1));
    assert!(h
        .check_snapshots
        .iter()
        .any(|(srcs, frac, groups, status)| srcs.len() == 3
            && (*frac - 0.75).abs() < 1e-9
            && *groups == 2
            && *status == "System done · Apps done · Firmware in progress"));
    assert_eq!(
        checking_footnote(3, about_time_left(10, 0.75).as_deref()),
        "Less than a minute left · 3 of 3 sources"
    );

    assert_eq!(h.state.updates_listed_at, Some(t0));
    assert!(!h.state.update_list_needs_refresh(t0));

    let Phase::Ready {
        packages,
        soft_errors,
    } = &h.state.phase
    else {
        panic!("expected Ready, got {:?}", h.state.phase);
    };
    assert!(soft_errors.is_empty(), "unexpected errors: {soft_errors:?}");
    // 8 unique DNF + 3 user flatpak + 3 system flatpak + 3 firmware = 17 packages
    assert_eq!(packages.len(), 17);
    assert!(packages
        .windows(2)
        .all(|w| w[0].source.apply_order() <= w[1].source.apply_order()));

    let expected_bytes =
        45_252_164_u64 + 145_752_064_u64 + 251_000_000_u64 + 391_118_848_u64;
    assert_eq!(total_download_bytes(packages), Some(expected_bytes));
    let summary = source_summary_line(packages);
    assert_eq!(
        summary,
        format!("8 system · 6 apps · 3 firmware · {}", format_bytes(expected_bytes))
    );

    let wide = window_header(
        HeaderView::Ready {
            count: packages.len(),
            summary: &summary,
        },
        false,
    );
    assert_eq!(wide.title, "17 Updates Available");
    assert_eq!(wide.subtitle, summary);
    let narrow = window_header(
        HeaderView::Ready {
            count: packages.len(),
            summary: &summary,
        },
        true,
    );
    assert_eq!(narrow.subtitle, "");

    // Console captures repoquery download match count, NVIDIA extension lines, and Flatpak warning.
    let log = h.console.full_text();
    assert!(log.contains("Download sizes for 6 package(s)"), "{log}");
    assert!(
        log.contains("Steam installs org.freedesktop.Platform.GL32.nvidia-570-133-07"),
        "{log}"
    );
    assert!(log.contains("Using offline cached flatpak metadata"), "{log}");
}

#[test]
fn check_empty_up_to_date_and_fwupd_exit_2_no_updates() {
    for fwupd_fixture in [
        "fwupd/get_updates_no_updates.txt",
        "fwupd/get_updates_empty.json",
    ] {
        let rt = FakeRuntime::new();
        rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_empty.txt", 0);
        rt.push_flatpak_appstream_ok();
        rt.push_flatpak_empty(true);
        rt.push_flatpak_empty(false);
        rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], Ok(0));
        // Real fwupdmgr exits 2 when nothing has updates; must NOT become a soft error.
        rt.push_helper_fixture(HelperCommand::CheckFwupd, fwupd_fixture, 2);

        let mut h = FlowHarness::default();
        h.run_check(&rt, false);

        assert!(h.state.updates_listed_at.is_none());
        assert_eq!(
            h.phases,
            vec!["Idle", "Authenticating", "Checking", "Idle"]
        );
        // Never called dnf repoquery on an empty package list.
        assert!(!rt
            .local_calls()
            .iter()
            .any(|(prog, _)| prog == "dnf"));

        let Phase::Idle(ref screen @ IdleScreen::Current { ref checked_at, ref detail }) =
            h.state.phase
        else {
            panic!("expected clean Current idle for {fwupd_fixture}, got {:?}", h.state.phase);
        };
        assert_eq!(checked_at, "just now");
        assert!(detail.is_none());

        let chrome = idle_chrome(screen);
        assert_eq!(chrome.title, "System is up to date");
        assert_eq!(chrome.button, "Check Again");
        assert!(chrome.flat);
        assert!(!chrome.show_view_log);
    }
}

#[test]
fn check_dnf_repo_error_with_other_sources_succeeding_shows_ready_banner() {
    let rt = FakeRuntime::new();
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_repo_error.txt", 1);
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_query(
        true,
        Ok(flatpak_check_from_fixture(
            "flatpak/check_user_normal.json",
            None,
        )),
    );
    rt.push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], Ok(0));
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_text.txt", 0);

    let mut h = FlowHarness::default();
    h.run_check(&rt, false);

    // DNF failed hard -> repoquery must NOT run, yet Flatpak and fwupd continue.
    assert!(!rt.local_calls().iter().any(|(prog, _)| prog == "dnf"));

    let Phase::Ready {
        packages,
        soft_errors,
    } = &h.state.phase
    else {
        panic!("expected Ready with soft banner, got {:?}", h.state.phase);
    };
    assert_eq!(soft_errors, &vec!["dnf check-update failed (exit 1)".to_string()]);
    assert_eq!(soft_error_banner_title(soft_errors.len()), "1 source could not be checked");
    assert_eq!(packages.len(), 5); // 3 user flatpak + 2 fwupd text devices
    assert!(h.console.full_text().contains("Curl error (6)"), "{}", h.console.full_text());
}

#[test]
fn check_all_sources_fail_or_fwupd_corrupt_surfaces_warning_idle() {
    // 1) All backends fail: DNF repo error + both Flatpaks Err + fwupdmgr missing
    let rt = FakeRuntime::new();
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_repo_error.txt", 1);
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_query(true, Err("flatpak user offline".into()));
    rt.push_flatpak_query(false, Err("flatpak system offline".into()));
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], Ok(0));
    rt.push_helper_lines(
        HelperCommand::CheckFwupd,
        vec!["fwupdmgr: command not found".into()],
        Ok(127),
    );

    let mut h = FlowHarness::default();
    h.run_check(&rt, false);

    let Phase::Idle(ref screen @ IdleScreen::Warning { title, ref detail, .. }) = h.state.phase
    else {
        panic!("expected Warning idle, got {:?}", h.state.phase);
    };
    assert_eq!(title, "Could not check all sources");
    assert!(detail.contains("dnf check-update failed (exit 1)"), "{detail}");
    assert!(detail.contains("flatpak user: flatpak user offline"), "{detail}");
    assert!(detail.contains("fwupd not available"), "{detail}");
    assert_eq!(soft_error_banner_title(4), "4 sources could not be checked");

    let chrome = idle_chrome(screen);
    assert_eq!(chrome.icon, "dialog-warning-symbolic");
    assert_eq!(chrome.button, "Check Again");
    assert!(chrome.show_view_log);

    // 2) Corrupt truncated fwupd JSON + exit 1 stays a soft error, not treated as no-updates.
    let rt2 = FakeRuntime::new();
    rt2.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_empty.txt", 0);
    rt2.push_flatpak_appstream_ok();
    rt2.push_flatpak_empty(true);
    rt2.push_flatpak_empty(false);
    rt2.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], Ok(0));
    rt2.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_corrupt.txt", 2);

    let mut h2 = FlowHarness::default();
    h2.run_check(&rt2, false);
    assert!(matches!(
        &h2.state.phase,
        Phase::Idle(IdleScreen::Warning { detail, .. }) if detail.contains("fwupdmgr get-updates exit 2")
    ));
}

#[test]
fn check_dnf5_table_survives_repoquery_error() {
    let rt = FakeRuntime::new();
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_dnf5_table.txt", 100);
    rt.push_local_lines(
        "dnf",
        dnf::REPOQUERY_UPGRADES_ARGS,
        vec![],
        Err("repoquery timed out".into()),
    );
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_empty(true);
    rt.push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], Ok(0));
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_no_updates.txt", 2);

    let mut h = FlowHarness::default();
    h.run_check(&rt, false);

    // Repoquery error is logged cleanly without marking DNF failed or blocking Ready.
    let Phase::Ready {
        packages,
        soft_errors,
    } = &h.state.phase
    else {
        panic!("expected Ready despite repoquery error, got {:?}", h.state.phase);
    };
    assert!(soft_errors.is_empty());
    assert!(!packages.is_empty());
    assert!(
        h.console
            .full_text()
            .contains("Could not query download sizes: repoquery timed out"),
        "{}",
        h.console.full_text()
    );
}

#[test]
fn check_polkit_auth_denied_and_helper_spawn_error_recover_on_retry() {
    let rt = FakeRuntime::new();
    rt.push_ensure_session(Err(SessionError::AuthDenied));
    rt.push_ensure_session(Err(SessionError::HelperNotFound("/usr/bin/fedora-updater".into())));
    push_happy_four_source_check(&rt);

    let mut h = FlowHarness::default();
    // Attempt 1: polkit denied -> Failed(CheckAgain)
    h.run_check(&rt, false);
    match &h.state.phase {
        Phase::Failed {
            title,
            detail,
            retry,
        } => {
            assert_eq!(title, "Authentication cancelled");
            assert!(detail.contains("polkit"), "{detail}");
            assert_eq!(*retry, RetryAction::CheckAgain);
            assert_eq!(fail_retry_label(*retry), "Check Again");
        }
        other => panic!("expected Failed CheckAgain, got {other:?}"),
    }
    assert!(rt.helper_calls().is_empty());

    // Attempt 2: helper missing -> Failed("Could not start privileged helper", CheckAgain)
    h.run_check(&rt, false);
    match &h.state.phase {
        Phase::Failed { title, retry, .. } => {
            assert_eq!(title, "Could not start privileged helper");
            assert_eq!(*retry, RetryAction::CheckAgain);
        }
        other => panic!("expected Failed spawn error, got {other:?}"),
    }

    // Attempt 3: succeeds -> Ready
    h.run_check(&rt, false);
    assert_eq!(phase_name(&h.state.phase), "Ready");
}

#[test]
fn apply_full_four_sources_order_live_draws_preview_and_reboot() {
    let rt = FakeRuntime::new();
    // Script apply for all 4 sources:
    // DNF packages matching dnf/apply_happy_kernel.txt
    let mut initial_pkgs = vec![
        Package::new_dnf("openssl", "x86_64", "1:3.2.2-3.fc44", "updates"),
        Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates"),
        Package::new_dnf("kernel-tools", "x86_64", "7.1.12-200.fc44", "updates"),
        Package::new_dnf("kernel-core", "x86_64", "7.1.12-200.fc44", "updates"),
    ];
    let fp_user = flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages;
    let fp_sys =
        flatpak_check_from_fixture("flatpak/check_system_nvidia_extension.json", None).packages;
    let fw_pkgs = fedora_updater::backend::fwupd::parse_get_updates(&read_fixture(
        "fwupd/get_updates_multi_device.json",
    ));
    initial_pkgs.extend(fp_user);
    initial_pkgs.extend(fp_sys);
    initial_pkgs.extend(fw_pkgs);

    let mut h = FlowHarness::ready(initial_pkgs);

    rt.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_happy_kernel.txt", 0);
    rt.push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_progress_happy.txt",
        0,
    );
    rt.push_local_fixture(
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

    h.run_apply(&rt);

    // Helper order: ApplyDnf -> ApplyFwupd -> CheckFwupdReboot
    assert_eq!(
        rt.helper_calls(),
        vec![
            HelperCommand::ApplyDnf,
            HelperCommand::ApplyFwupd,
            HelperCommand::CheckFwupdReboot,
        ]
    );
    // Local commands order and exact flatpak update argvs
    let locals = rt.local_calls();
    assert_eq!(locals.len(), 2);
    assert_eq!(locals[0].0, "flatpak");
    assert_eq!(
        locals[0].1,
        vec![
            "update",
            "--user",
            "-y",
            "--noninteractive",
            "app/org.signal.Signal/x86_64/stable",
            "app/org.mozilla.Thunderbird/x86_64/stable",
            "runtime/org.gnome.Platform/x86_64/48",
        ]
    );
    assert_eq!(locals[1].0, "flatpak");
    assert_eq!(
        locals[1].1,
        vec![
            "update",
            "--system",
            "-y",
            "--noninteractive",
            "app/com.valvesoftware.Steam/x86_64/stable",
            "runtime/org.gnome.Platform/x86_64/48",
            "runtime/org.freedesktop.Platform/x86_64/24.08",
        ]
    );

    // Live \r draws came through ConsoleDraw without leaking intermediate redraw % into full_text.
    assert_eq!(h.draw_count, 5);
    assert!(h.deltas.contains(&ConsoleDelta::ReplacedLast));
    let full = h.console.full_text();
    assert!(!full.contains("10% |   1.1 MiB/s"), "{full}");
    assert!(full.contains("100% |   4.2 MiB/s"), "{full}");
    assert!(full.contains("── Applying System ──"), "{full}");
    assert!(full.contains("── Applying Apps ──"), "{full}");
    assert!(full.contains("── Applying Firmware ──"), "{full}");

    // Download pass -> Apply pass reset + Verifying heading verified.
    assert!(h.saw_download_pass);
    assert!(h.saw_apply_pass);
    assert!(h.saw_apply_progress_reset);
    assert!(h.saw_verifying_heading);

    // Final Done state + presenter header.
    let Phase::Done {
        packages,
        needs_reboot,
        failed,
        failure_reasons,
        ref duration,
    } = h.state.phase
    else {
        panic!("expected Done, got {:?}", h.state.phase);
    };
    assert!(needs_reboot);
    assert_eq!(failed, 0);
    assert!(failure_reasons.is_empty());
    assert_eq!(packages.len(), 13);
    assert!(packages.iter().all(|p| p.status == PackageStatus::Completed));

    let subtitle = done_header_subtitle(0, 13, 4, 6, 3, duration);
    let hdr = window_header(
        HeaderView::Done {
            failed: false,
            subtitle: &subtitle,
        },
        false,
    );
    assert_eq!(hdr.title, "All Updates Applied");
    assert!(hdr.subtitle.contains("4 system / 6 apps / 3 firmware"), "{}", hdr.subtitle);
}

#[test]
fn apply_flatpak_only_never_starts_privileged_helper_session() {
    let rt = FakeRuntime::new();
    let pkgs = flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages;
    rt.push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_progress_happy.txt",
        0,
    );

    let mut h = FlowHarness::ready(pkgs);
    h.run_apply(&rt);

    assert_eq!(rt.ensure_session_calls(), 0);
    assert!(rt.helper_calls().is_empty());
    assert!(matches!(
        h.state.phase,
        Phase::Done {
            failed: 0,
            needs_reboot: false,
            ..
        }
    ));
}

#[test]
fn apply_firmware_only_uses_check_fwupd_reboot_for_reboot_flag() {
    let fw_pkgs = fedora_updater::backend::fwupd::parse_get_updates(&read_fixture(
        "fwupd/get_updates_multi_device.json",
    ));

    // Case 1: exit 0 check-reboot-needed -> needs_reboot == true
    let rt_yes = FakeRuntime::new();
    rt_yes.push_helper_lines(
        HelperCommand::ApplyFwupd,
        vec!["Successfully installed firmware".into()],
        Ok(0),
    );
    rt_yes.push_helper_fixture(
        HelperCommand::CheckFwupdReboot,
        "fwupd/check_reboot_needed_true.json",
        0,
    );
    let mut h_yes = FlowHarness::ready(fw_pkgs.clone());
    h_yes.run_apply(&rt_yes);
    assert_eq!(
        rt_yes.helper_calls(),
        vec![HelperCommand::ApplyFwupd, HelperCommand::CheckFwupdReboot]
    );
    assert!(matches!(
        h_yes.state.phase,
        Phase::Done {
            failed: 0,
            needs_reboot: true,
            ..
        }
    ));

    // Case 2: exit 2 check-reboot-needed -> needs_reboot == false
    let rt_no = FakeRuntime::new();
    rt_no.push_helper_lines(
        HelperCommand::ApplyFwupd,
        vec!["Successfully installed firmware".into()],
        Ok(0),
    );
    rt_no.push_helper_fixture(
        HelperCommand::CheckFwupdReboot,
        "fwupd/check_reboot_needed_false.json",
        2,
    );
    let mut h_no = FlowHarness::ready(fw_pkgs);
    h_no.run_apply(&rt_no);
    assert!(matches!(
        h_no.state.phase,
        Phase::Done {
            failed: 0,
            needs_reboot: false,
            ..
        }
    ));
}

#[test]
fn apply_partial_failure_dnf_dep_conflict_while_flatpak_and_fwupd_succeed() {
    let rt = FakeRuntime::new();
    let mut pkgs = vec![
        Package::new_dnf("kernel-core", "x86_64", "7.1.12-200.fc44", "updates"),
        Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates"),
    ];
    pkgs.extend(flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages);
    pkgs.extend(fedora_updater::backend::fwupd::parse_get_updates(
        &read_fixture("fwupd/get_updates_multi_device.json"),
    ));

    rt.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_dep_conflict.txt", 1);
    rt.push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_progress_happy.txt",
        0,
    );
    rt.push_helper_fixture(HelperCommand::ApplyFwupd, "fwupd/apply_happy_reboot.txt", 0);
    rt.push_helper_fixture(
        HelperCommand::CheckFwupdReboot,
        "fwupd/check_reboot_needed_true.json",
        0,
    );

    let mut h = FlowHarness::ready(pkgs);
    h.run_apply(&rt);

    let Phase::Done {
        ref packages,
        needs_reboot,
        failed,
        ref failure_reasons,
        ref duration,
    } = h.state.phase
    else {
        panic!("expected partial Done, got {:?}", h.state.phase);
    };
    assert!(needs_reboot);
    assert_eq!(failed, 2);
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
        .filter(|p| p.source != UpdateSource::Dnf)
        .all(|p| p.status == PackageStatus::Completed));

    let hdr = window_header(
        HeaderView::Done {
            failed: true,
            subtitle: &done_header_subtitle(failed, 6, 0, 3, 3, duration),
        },
        false,
    );
    assert_eq!(hdr.title, "Updates Finished With Problems");
    assert!(h.console.full_text().contains("Problem 2: problem with installed package"));

    h.dispatch(Event::ResetToIdle);
    let Phase::Idle(ref screen @ IdleScreen::Warning { title, ref detail, .. }) = h.state.phase
    else {
        panic!("expected Warning idle after ResetToIdle, got {:?}", h.state.phase);
    };
    assert_eq!(title, "Some updates failed");
    assert!(detail.contains("Problem 1:"), "{detail}");
    assert!(idle_chrome(screen).show_view_log);
}

#[test]
fn apply_partial_failure_dnf_rpm_lock_does_not_falsely_flag_reboot() {
    // Even with `kernel-core` in the transaction, a FAILED DNF apply must NOT
    // mark `needs_reboot = true` because the kernel was never installed!
    let rt = FakeRuntime::new();
    let mut pkgs = vec![Package::new_dnf(
        "kernel-core",
        "x86_64",
        "7.1.12-200.fc44",
        "updates",
    )];
    pkgs.extend(flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages);

    rt.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_rpm_lock_error.txt", 1);
    rt.push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_progress_happy.txt",
        0,
    );

    let mut h = FlowHarness::ready(pkgs);
    h.run_apply(&rt);

    match &h.state.phase {
        Phase::Done {
            failed,
            needs_reboot,
            failure_reasons,
            ..
        } => {
            assert_eq!(*failed, 1);
            assert!(
                !*needs_reboot,
                "failed kernel-core DNF apply must not falsely require reboot"
            );
            assert!(failure_reasons[0].contains("RPM database is locked"));
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn apply_flatpak_user_failure_while_dnf_and_flatpak_system_succeed() {
    let rt = FakeRuntime::new();
    let mut pkgs = vec![Package::new_dnf(
        "firefox",
        "x86_64",
        "140.0-1.fc44",
        "updates",
    )];
    pkgs.extend(flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages);
    pkgs.extend(
        flatpak_check_from_fixture("flatpak/check_system_nvidia_extension.json", None).packages,
    );

    rt.push_helper_lines(
        HelperCommand::ApplyDnf,
        vec![
            "Running transaction".into(),
            "[1/1] Upgrading firefox-0:140.0-1.fc44.x86_64 100%".into(),
            "Complete!".into(),
        ],
        Ok(0),
    );
    rt.push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_remote_error.txt",
        1,
    );
    rt.push_local_fixture(
        "flatpak",
        &["update", "--system", "-y", "--noninteractive"],
        "flatpak/apply_nothing_to_update.txt",
        0,
    );

    let mut h = FlowHarness::ready(pkgs);
    h.run_apply(&rt);

    let Phase::Done {
        packages,
        failed,
        needs_reboot,
        failure_reasons,
        ..
    } = &h.state.phase
    else {
        panic!("expected Done with FlatpakUser partial failure, got {:?}", h.state.phase);
    };
    assert_eq!(*failed, 3);
    assert!(!*needs_reboot);
    assert_eq!(failure_reasons.len(), 1);
    assert!(
        failure_reasons[0].contains("Socket I/O timed out"),
        "{failure_reasons:?}"
    );
    assert!(packages
        .iter()
        .filter(|p| p.source == UpdateSource::FlatpakUser)
        .all(|p| p.status == PackageStatus::Failed));
    assert!(packages
        .iter()
        .filter(|p| p.source != UpdateSource::FlatpakUser)
        .all(|p| p.status == PackageStatus::Completed));
}

#[test]
fn apply_total_failures_surface_actionable_reasons_and_preserve_console() {
    // 1) DNF RPM lock total failure
    let rt_lock = FakeRuntime::new();
    rt_lock.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_rpm_lock_error.txt", 1);
    let mut h_lock = FlowHarness::ready(vec![Package::new_dnf(
        "firefox",
        "x86_64",
        "140.0-1.fc44",
        "updates",
    )]);
    h_lock.run_apply(&rt_lock);
    assert!(matches!(
        &h_lock.state.phase,
        Phase::Failed {
            title,
            detail,
            retry: RetryAction::CheckAgain
        } if title == "All updates failed" && detail.contains("RPM database is locked")
    ));
    assert!(h_lock.console.full_text().contains("dnf5daemon-server"));

    // 2) Firmware battery error total failure
    let rt_batt = FakeRuntime::new();
    rt_batt.push_helper_fixture(HelperCommand::ApplyFwupd, "fwupd/apply_battery_error.txt", 1);
    rt_batt.push_helper_fixture(
        HelperCommand::CheckFwupdReboot,
        "fwupd/check_reboot_needed_false.json",
        2,
    );
    let mut h_batt = FlowHarness::ready(fedora_updater::backend::fwupd::parse_get_updates(
        &read_fixture("fwupd/get_updates_multi_device.json"),
    ));
    h_batt.run_apply(&rt_batt);
    assert!(matches!(
        &h_batt.state.phase,
        Phase::Failed {
            title,
            detail,
            retry: RetryAction::CheckAgain
        } if title == "All updates failed" && detail.contains("battery") && detail.contains("AC power")
    ));

    // 3) Flatpak remote error total failure
    let rt_fp = FakeRuntime::new();
    rt_fp.push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_remote_error.txt",
        1,
    );
    let mut h_fp =
        FlowHarness::ready(flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages);
    h_fp.run_apply(&rt_fp);
    assert!(matches!(
        &h_fp.state.phase,
        Phase::Failed {
            detail,
            retry: RetryAction::CheckAgain,
            ..
        } if detail.contains("Apps: Failed to update org.signal.Signal")
    ));
}

#[test]
fn apply_empty_package_list_and_polkit_denied_retry_apply() {
    // 1) Empty package list passed straight to execute_apply_all emits clean Fail without session.
    let rt_empty = FakeRuntime::new();
    let (tx, rx) = async_channel::unbounded();
    let mut h_empty = FlowHarness::default();
    execute_apply_all(&tx, vec![], &rt_empty, &WorkCancel::new());
    drop(tx);
    h_empty.drain(&rx);
    assert_eq!(rt_empty.ensure_session_calls(), 0);
    assert!(matches!(
        &h_empty.state.phase,
        Phase::Failed {
            title,
            retry: RetryAction::CheckAgain,
            ..
        } if title == "Nothing to apply"
    ));

    // 2) Apply polkit AuthDenied preserves packages and retries via RetryAction::RetryApply.
    let rt = FakeRuntime::new();
    rt.push_ensure_session(Err(SessionError::AuthDenied));
    rt.push_ensure_session(Ok(()));
    rt.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_happy_kernel.txt", 0);

    let pkgs = vec![
        Package::new_dnf("openssl", "x86_64", "1:3.2.2-3.fc44", "updates"),
        Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates"),
        Package::new_dnf("kernel-tools", "x86_64", "7.1.12-200.fc44", "updates"),
        Package::new_dnf("kernel-core", "x86_64", "7.1.12-200.fc44", "updates"),
    ];
    let mut h = FlowHarness::ready(pkgs);

    h.run_apply(&rt);
    match &h.state.phase {
        Phase::Failed {
            title,
            retry: RetryAction::RetryApply,
            ..
        } => {
            assert_eq!(title, "Authentication cancelled");
            assert_eq!(fail_retry_label(RetryAction::RetryApply), "Retry Update");
        }
        other => panic!("expected Failed(RetryApply), got {other:?}"),
    }
    assert_eq!(h.state.packages().len(), 4);

    // Clicking "Retry Update" dispatches StartApply and completes cleanly.
    h.run_apply(&rt);
    assert!(matches!(
        h.state.phase,
        Phase::Done {
            failed: 0,
            needs_reboot: true,
            ..
        }
    ));
}

#[test]
fn cancel_during_auth_and_mid_check_drops_late_events() {
    // 1) Cancel during Authenticating via run_check_all_with (background thread)
    let rt1 = FakeRuntime::new();
    let (hit_tx, hit_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    rt1.push_ensure_session_hook(Ok(()), move || {
        let _ = hit_tx.send(());
        let _ = gate_rx.recv();
    });

    let mut h1 = FlowHarness::default();
    h1.dispatch(Event::StartCheck { then_apply: false });
    assert_eq!(phase_name(&h1.state.phase), "Authenticating");

    let (tx1, rx1) = async_channel::unbounded();
    let cancel1 = run_check_all_with(tx1, rt1.clone());
    hit_rx.recv_timeout(Duration::from_secs(5)).expect("worker entered ensure_session");
    cancel1.cancel();
    h1.dispatch(Event::CancelAuth);
    let _ = gate_tx.send(());
    wait_until_rx_closed(&mut h1, &rx1);

    assert_eq!(phase_name(&h1.state.phase), "Idle");
    assert!(rt1.helper_calls().is_empty());

    // 2) Cancel mid-CheckDnf drops late lines and never touches Flatpak or fwupd.
    let rt2 = FakeRuntime::new();
    let (hit2_tx, hit2_rx) = mpsc::channel();
    let (gate2_tx, gate2_rx) = mpsc::channel();
    let mut dnf_lines = fixture_to_helper_lines(&read_fixture("dnf/check_update_classic.txt"));
    dnf_lines.push("late-package.x86_64 9.9-1.fc44 updates".into());
    rt2.push_helper_step(
        HelperCommand::CheckDnf,
        HelperStep {
            lines: dnf_lines,
            result: Ok(100),
            hook_after_line: Some((
                5,
                Box::new(move || {
                    let _ = hit2_tx.send(());
                    let _ = gate2_rx.recv();
                }),
            )),
        },
    );

    let mut h2 = FlowHarness::default();
    h2.dispatch(Event::StartCheck { then_apply: false });
    let (tx2, rx2) = async_channel::unbounded();
    let cancel2 = run_check_all_with(tx2, rt2.clone());
    hit2_rx.recv_timeout(Duration::from_secs(5)).expect("worker reached line 5");
    cancel2.cancel();
    let _ = gate2_tx.send(());
    wait_until_rx_closed(&mut h2, &rx2);

    assert_eq!(rt2.helper_calls(), vec![HelperCommand::CheckDnf]);
    assert!(rt2.local_calls().is_empty());
    assert!(rt2.flatpak_queries().is_empty());
    assert!(!h2.console.full_text().contains("late-package"));
    assert_eq!(phase_name(&h2.state.phase), "Checking");
}

#[test]
fn stop_before_install_vs_stop_during_rpm_apply() {
    let apply_lines = fixture_to_helper_lines(&read_fixture("dnf/apply_happy_kernel.txt"));
    let download_stop_idx = apply_lines
        .iter()
        .position(|l| l.contains("[2/4] firefox") && !l.starts_with(DRAW_PREFIX))
        .expect("firefox 100% download line");
    let apply_stop_idx = apply_lines
        .iter()
        .position(|l| l.contains("Installing kernel-core"))
        .expect("kernel-core installing line");

    let initial_pkgs = || {
        vec![
            Package::new_dnf("openssl", "x86_64", "1:3.2.2-3.fc44", "updates"),
            Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates"),
            Package::new_dnf("kernel-tools", "x86_64", "7.1.12-200.fc44", "updates"),
            Package::new_dnf("kernel-core", "x86_64", "7.1.12-200.fc44", "updates"),
        ]
    };

    // Case A: Stop during Download pass -> immediate return to Ready, every package Pending.
    let rt_dl = FakeRuntime::new();
    let (hit_dl_tx, hit_dl_rx) = mpsc::channel();
    let (gate_dl_tx, gate_dl_rx) = mpsc::channel();
    rt_dl.push_helper_step(
        HelperCommand::ApplyDnf,
        HelperStep {
            lines: apply_lines.clone(),
            result: Ok(0),
            hook_after_line: Some((
                download_stop_idx,
                Box::new(move || {
                    let _ = hit_dl_tx.send(());
                    let _ = gate_dl_rx.recv();
                }),
            )),
        },
    );

    let mut h_dl = FlowHarness::ready(initial_pkgs());
    h_dl.dispatch(Event::StartApply);
    let (tx_dl, rx_dl) = async_channel::unbounded();
    let cancel_dl = run_apply_all_with(tx_dl, h_dl.state.packages().to_vec(), rt_dl);

    hit_dl_rx.recv_timeout(Duration::from_secs(5)).expect("reached download line");
    h_dl.drain(&rx_dl);
    let during_tx_dl = match &h_dl.state.phase {
        Phase::Running(r) => {
            assert!(!r.is_applying(), "expected Download pass, got {:?}", r.pass);
            assert_eq!(run_heading(&r.phase_label, r.pass), "Downloading");
            let title = now_playing_title(&r.packages, r.active_index, r.current_name.as_deref(), &r.phase_label);
            assert_eq!(title, "firefox");
            assert_eq!(now_playing_subtitle(&title, &r.phase_label, r.current_source), "Downloading packages · System");
            let (foot, meta) = running_progress_captions(r.current_source, 2, 4, r.pass);
            assert_eq!(foot, "System · 2 of 4 downloaded");
            assert_eq!(meta, "2 downloaded · 2 remaining");
            let quiet = quiet_ui(QuietInput {
                quiet_secs: 6,
                at_full_item: r.work_progress.is_some_and(|p| p >= 0.999),
                watching_dnf: true,
                ..Default::default()
            });
            assert_eq!(quiet.mode, QuietMode::ItemAtFull);
            r.is_applying()
        }
        other => panic!("expected Running during download, got {other:?}"),
    };
    cancel_dl.cancel();
    h_dl.dispatch(Event::StopApply {
        during_transaction: during_tx_dl,
    });
    let _ = gate_dl_tx.send(());
    wait_until_rx_closed(&mut h_dl, &rx_dl);

    match &h_dl.state.phase {
        Phase::Ready {
            packages,
            soft_errors,
        } => {
            assert!(packages.iter().all(|p| p.status == PackageStatus::Pending));
            assert!(soft_errors[0].contains("Stopped before install"));
        }
        other => panic!("expected Ready after download stop, got {other:?}"),
    }

    // Case B: Stop during Apply pass -> confirms stop_update_dialog, transitions to Failed warning of dnf history.
    let rt_ap = FakeRuntime::new();
    let (hit_ap_tx, hit_ap_rx) = mpsc::channel();
    let (gate_ap_tx, gate_ap_rx) = mpsc::channel();
    rt_ap.push_helper_step(
        HelperCommand::ApplyDnf,
        HelperStep {
            lines: apply_lines,
            result: Ok(0),
            hook_after_line: Some((
                apply_stop_idx,
                Box::new(move || {
                    let _ = hit_ap_tx.send(());
                    let _ = gate_ap_rx.recv();
                }),
            )),
        },
    );

    let mut h_ap = FlowHarness::ready(initial_pkgs());
    h_ap.dispatch(Event::StartApply);
    let (tx_ap, rx_ap) = async_channel::unbounded();
    let cancel_ap = run_apply_all_with(tx_ap, h_ap.state.packages().to_vec(), rt_ap);

    hit_ap_rx.recv_timeout(Duration::from_secs(5)).expect("reached apply transaction line");
    h_ap.drain(&rx_ap);
    let during_tx_ap = match &h_ap.state.phase {
        Phase::Running(r) => {
            assert!(r.is_applying(), "expected Apply pass");
            assert_eq!(run_heading(&r.phase_label, r.pass), "Applying");
            assert_eq!(
                r.packages.iter().find(|p| p.name == "openssl").unwrap().status,
                PackageStatus::Completed
            );
            assert_eq!(
                r.packages.iter().find(|p| p.name == "kernel-core").unwrap().status,
                PackageStatus::Installing
            );
            let dlg = stop_update_dialog();
            assert_eq!(dlg.heading, "Stop This Update?");
            assert!(dlg.confirm_destructive);
            r.is_applying()
        }
        other => panic!("expected Running during transaction, got {other:?}"),
    };
    cancel_ap.cancel();
    h_ap.dispatch(Event::StopApply {
        during_transaction: during_tx_ap,
    });
    let _ = gate_ap_tx.send(());
    wait_until_rx_closed(&mut h_ap, &rx_ap);

    match &h_ap.state.phase {
        Phase::Failed {
            title,
            detail,
            retry,
        } => {
            assert_eq!(title, "Update stopped during install");
            assert!(detail.contains("dnf history"));
            assert_eq!(*retry, RetryAction::CheckAgain);
        }
        other => panic!("expected Failed warning after mid-install stop, got {other:?}"),
    }
}

#[test]
fn stale_ready_list_over_24h_refreshes_and_auto_applies() {
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let rt = FakeRuntime::new();
    rt.set_now(t0);
    push_happy_four_source_check(&rt);

    let mut h = FlowHarness::default();
    h.run_check(&rt, false);
    assert!(!h.state.update_list_needs_refresh(t0 + UPDATE_LIST_MAX_AGE));

    // Advance past 24 hours -> stale dialog applies, confirming refresh-then-apply.
    rt.set_now(t0 + UPDATE_LIST_MAX_AGE + Duration::from_secs(1));
    assert!(h.state.update_list_needs_refresh(rt.current_now()));
    let stale_dlg = stale_update_dialog();
    assert_eq!(stale_dlg.heading, "Refresh Update List?");
    assert_eq!(stale_dlg.confirm_id, "yes");

    // Enqueue refreshed check returning the 4 packages matching dnf/apply_happy_kernel.txt + happy apply.
    rt.push_helper_lines(
        HelperCommand::CheckDnf,
        vec![
            "openssl.x86_64 1:3.2.2-3.fc44 updates".into(),
            "firefox.x86_64 140.0-1.fc44 updates".into(),
            "kernel-tools.x86_64 7.1.12-200.fc44 updates".into(),
            "kernel-core.x86_64 7.1.12-200.fc44 updates".into(),
        ],
        Ok(100),
    );
    rt.push_local_lines("dnf", dnf::REPOQUERY_UPGRADES_ARGS, vec![], Ok(0));
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_empty(true);
    rt.push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], Ok(0));
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_empty.json", 2);
    rt.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_happy_kernel.txt", 0);

    h.run_check(&rt, true);
    assert!(h.state.apply_when_ready);
    assert!(matches!(&h.state.phase, Phase::Ready { packages, .. } if packages.len() == 4));
    // Window::handle_worker immediately triggers begin_apply when apply_when_ready && !empty.
    h.run_apply(&rt);
    assert!(!h.state.apply_when_ready);
    assert!(matches!(
        h.state.phase,
        Phase::Done {
            failed: 0,
            needs_reboot: true,
            ..
        }
    ));

    // Also verify stale refresh returning 0 updates clears apply_when_ready -> Idle(Current).
    h.state = AppState {
        updates_listed_at: Some(t0),
        phase: Phase::Ready {
            packages: vec![Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates")],
            soft_errors: vec![],
        },
        ..Default::default()
    };
    assert!(h.state.update_list_needs_refresh(rt.current_now()));
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_empty.txt", 0);
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_empty(true);
    rt.push_flatpak_empty(false);
    rt.push_helper_lines(HelperCommand::CheckFwupdRefresh, vec![], Ok(0));
    rt.push_helper_fixture(HelperCommand::CheckFwupd, "fwupd/get_updates_no_updates.txt", 2);

    h.run_check(&rt, true);
    assert!(!h.state.apply_when_ready);
    assert!(matches!(
        h.state.phase,
        Phase::Idle(IdleScreen::Current { detail: None, .. })
    ));
}

#[test]
fn systemctl_reboot_inhibitor_logind_spawn_error_and_retry_recovery() {
    let rt = FakeRuntime::new();
    // 1) Active block inhibitor fixture (exit 1)
    rt.push_local_fixture(
        "systemctl",
        &["reboot"],
        "systemctl/reboot_block_inhibitor.txt",
        1,
    );
    // 2) Generic logind error fixture (exit 1)
    rt.push_local_fixture(
        "systemctl",
        &["reboot"],
        "systemctl/reboot_logind_error.txt",
        1,
    );
    // 3) Spawn failure
    rt.push_local_lines("systemctl", &["reboot"], vec![], Err("No such file or directory".into()));
    // 4) Retry succeeds
    rt.push_local_lines("systemctl", &["reboot"], vec![], Ok(0));

    let mut h = FlowHarness::with_state(AppState {
        phase: Phase::Done {
            packages: vec![],
            duration: "12s".into(),
            needs_reboot: true,
            failed: 0,
            failure_reasons: vec![],
        },
        ..Default::default()
    });

    // Inhibitor failure -> RetryAction::Reboot ("Restart"), console preserves PackageKit line.
    h.run_reboot(&rt);
    match &h.state.phase {
        Phase::Failed {
            title,
            detail,
            retry,
        } => {
            assert_eq!(title, REBOOT_FAILED_TITLE);
            assert!(detail.contains("blocking shutdown"), "{detail}");
            assert!(detail.contains("Try again"), "{detail}");
            assert_eq!(*retry, RetryAction::Reboot);
            assert_eq!(fail_retry_label(*retry), "Restart");
        }
        other => panic!("expected Failed(Reboot) for inhibitor, got {other:?}"),
    }
    assert!(h.console.full_text().contains("PackageKit"));

    // Logind error preserved verbatim.
    h.run_reboot(&rt);
    assert!(matches!(
        &h.state.phase,
        Phase::Failed {
            title,
            detail,
            retry: RetryAction::Reboot,
        } if title == REBOOT_FAILED_TITLE && detail == "Failed to talk to logind: Connection timed out"
    ));

    // Spawn error -> REBOOT_SPAWN_FAILED_TITLE + Reboot retry.
    h.run_reboot(&rt);
    assert!(matches!(
        &h.state.phase,
        Phase::Failed {
            title,
            detail,
            retry: RetryAction::Reboot,
        } if title == REBOOT_SPAWN_FAILED_TITLE && detail.contains("No such file")
    ));

    // Retry via background thread `run_systemctl_reboot_with` succeeds with zero Fail events.
    let (tx, rx) = async_channel::unbounded();
    run_systemctl_reboot_with(tx, rt.clone());
    let mut retry_harness = FlowHarness::default();
    wait_until_rx_closed(&mut retry_harness, &rx);
    assert_eq!(retry_harness.phases, vec!["Idle"]);
    assert_eq!(
        rt.local_calls(),
        vec![
            ("systemctl".into(), vec!["reboot".into()]),
            ("systemctl".into(), vec!["reboot".into()]),
            ("systemctl".into(), vec!["reboot".into()]),
            ("systemctl".into(), vec!["reboot".into()]),
        ]
    );
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct FwupdDbusCheckFixture {
    host_power: FwupdHostPower,
    devices: Vec<FwupdDevice>,
}

#[test]
fn native_fwupd_dbus_check_apply_history_and_battery_safety_flows() {
    // 1) Parse offline D-Bus fixtures (`fwupd/dbus_devices_and_upgrades.json` + `dbus_history.json`).
    let check_fx: FwupdDbusCheckFixture = serde_json::from_value(read_fixture_value(
        "fwupd/dbus_devices_and_upgrades.json",
    ))
    .expect("deserialize fwupd dbus fixture");
    assert_eq!(check_fx.devices.len(), 3);
    assert!(devices_need_reboot(&check_fx.devices));

    // Device 0 (`System Firmware` with REQUIRE_AC & OnExternalPower=false) and Device 1
    // (`Wireless Receiver` with 12% < 20%) both trigger battery safety warnings.
    let sys_pkg = package_from_device_and_release(
        &check_fx.devices[0],
        &check_fx.devices[0].releases[0],
        &check_fx.host_power,
    );
    assert!(sys_pkg
        .preflight_warning
        .as_deref()
        .unwrap()
        .contains("requires AC power"));
    assert!(sys_pkg
        .summary
        .as_deref()
        .unwrap()
        .contains("CVE-2026-1144"));

    let usb_safety = evaluate_battery_safety(&check_fx.devices[1], &check_fx.host_power);
    let usb_warn = usb_safety.reason().expect("low battery warning");
    assert!(usb_warn.contains("12% < 20%"), "{usb_warn}");

    let history: Vec<FwupdHistoryEntry> =
        serde_json::from_value(read_fixture_value("fwupd/dbus_history.json"))
            .expect("deserialize fwupd history fixture");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].update_state, FwupdUpdateState::Success);
    assert_eq!(history[1].update_state, FwupdUpdateState::Failed);
    assert_eq!(history[1].target_version.as_deref(), Some("20260707"));

    // 2) Full Check + Firmware-only Native D-Bus Apply skips helper ensure_session on apply and runs 0 fwupdmgr CLI calls!
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let rt = FakeRuntime::new();
    rt.set_now(t0);
    rt.set_fwupd_dbus_available(true);

    // Check scripts: DNF & Flatpak return empty, fwupd D-Bus returns plugged-in firmware package.
    let plugged = FwupdHostPower {
        host_battery_level: 85,
        host_battery_threshold: 25,
        on_external_power: Some(true),
    };
    let clean_sys_pkg = package_from_device_and_release(
        &check_fx.devices[0],
        &check_fx.devices[0].releases[0],
        &plugged,
    );
    assert!(clean_sys_pkg.preflight_warning.is_none());

    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_empty.txt", 0);
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_empty(true);
    rt.push_flatpak_empty(false);
    rt.push_fwupd_dbus_check(NativeOutcome::Handled(Ok(vec![clean_sys_pkg.clone()])));

    let mut h = FlowHarness::default();
    h.run_check(&rt, false);
    assert_eq!(rt.fwupd_dbus_check_calls(), 1);
    assert!(
        !rt.helper_calls().contains(&HelperCommand::CheckFwupd)
            && !rt.helper_calls().contains(&HelperCommand::CheckFwupdRefresh),
        "native fwupd D-Bus must bypass fwupdmgr helper checks: {:?}",
        rt.helper_calls()
    );
    assert_eq!(h.state.packages().len(), 1);

    // Apply Firmware-only update via D-Bus: `ensure_session` stays at 1 (from check, NOT 2).
    rt.push_fwupd_dbus_apply(FwupdDbusApplyStep {
        lines: vec!["Installing firmware for System Firmware…".into()],
        hints: vec![
            FwupdStatus::Downloading.to_progress_hint(50.0, Some("System Firmware")),
            FwupdStatus::DeviceWrite.to_progress_hint(80.0, Some("System Firmware")),
            FwupdStatus::DeviceVerify.to_progress_hint(100.0, Some("System Firmware")),
        ],
        outcome: NativeOutcome::Handled(Ok(FwupdApplyOutcome { needs_reboot: true })),
    });
    rt.push_fwupd_dbus_reboot_needed(NativeOutcome::Handled(Ok(true)));

    h.run_apply(&rt);
    assert_eq!(rt.ensure_session_calls(), 1);
    assert_eq!(rt.fwupd_dbus_apply_calls(), 1);
    assert_eq!(rt.fwupd_dbus_reboot_calls(), 1);
    assert!(
        !rt.helper_calls().contains(&HelperCommand::ApplyFwupd)
            && !rt.helper_calls().contains(&HelperCommand::CheckFwupdReboot),
        "native fwupd D-Bus apply must bypass fwupdmgr helper: {:?}",
        rt.helper_calls()
    );
    assert!(matches!(
        &h.state.phase,
        Phase::Done {
            needs_reboot: true,
            failed: 0,
            ..
        }
    ));

    // 3) Mixed DNF + Battery-blocked Firmware apply -> DNF succeeds while Firmware fails with actionable AC power reason.
    let rt_mixed = FakeRuntime::new();
    rt_mixed.set_fwupd_dbus_available(true);
    let dnf_pkg = Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates");
    rt_mixed.push_helper_lines(HelperCommand::ApplyDnf, vec!["Complete!".into()], Ok(0));
    let blocked_msg = sys_pkg.preflight_warning.clone().unwrap();
    rt_mixed.push_fwupd_dbus_apply(FwupdDbusApplyStep {
        lines: vec![blocked_msg.clone()],
        hints: vec![],
        outcome: NativeOutcome::Handled(Err(blocked_msg)),
    });
    rt_mixed.push_fwupd_dbus_reboot_needed(NativeOutcome::Handled(Ok(false)));

    let mut h_mixed = FlowHarness::ready(vec![dnf_pkg, sys_pkg]);
    h_mixed.run_apply(&rt_mixed);
    match &h_mixed.state.phase {
        Phase::Done {
            failed,
            failure_reasons,
            needs_reboot,
            ..
        } => {
            assert_eq!(*failed, 1);
            assert!(!*needs_reboot);
            assert_eq!(failure_reasons.len(), 1);
            assert!(
                failure_reasons[0].starts_with("Firmware: ")
                    && !failure_reasons[0].starts_with("Firmware: Firmware:"),
                "{:?}",
                failure_reasons
            );
            assert!(failure_reasons[0].contains("AC power"), "{:?}", failure_reasons);
        }
        other => panic!("expected partial-failure Done, got {other:?}"),
    }
}

#[test]
fn native_logind_reboot_inhibitor_guard_and_cli_fallback() {
    let inhibitors: Vec<ShutdownInhibitor> =
        serde_json::from_value(read_fixture_value("systemctl/logind_inhibitors.json"))
            .expect("deserialize logind inhibitors fixture");
    let preflight = RebootPreflight::from_raw("yes", inhibitors);
    assert_eq!(preflight.blockers.len(), 1);
    let detail = preflight.blocking_detail().expect("shutdown blocker detail");
    assert!(detail.contains("dnf5daemon-server"), "{detail}");
    assert!(detail.contains("Installing system updates"), "{detail}");

    let rt = FakeRuntime::new();
    // 1) Native logind blocks shutdown with actionable who/why -> RetryAction::Reboot, 0 systemctl calls.
    rt.push_logind_reboot(
        vec!["logind CanReboot=yes, shutdown blockers=1".into()],
        NativeOutcome::Handled(Err(detail.clone())),
    );
    // 2) Native logind CanReboot="no" -> RetryAction::Reboot, 0 systemctl calls.
    rt.push_logind_reboot(
        vec![],
        NativeOutcome::Handled(Err(RebootPreflight::from_raw("no", vec![])
            .blocking_detail()
            .unwrap())),
    );
    // 3) Native logind Reboot(true) succeeds -> 0 Fail events, 0 systemctl calls.
    rt.push_logind_reboot(
        vec!["Requesting interactive system restart via systemd-logind…".into()],
        NativeOutcome::Handled(Ok(())),
    );
    // 4) Fourth call has nothing scripted -> Unavailable -> falls back to `systemctl reboot`.
    rt.push_local_lines("systemctl", &["reboot"], vec![], Ok(0));

    let mut h = FlowHarness::default();
    h.run_reboot(&rt);
    assert!(matches!(
        &h.state.phase,
        Phase::Failed {
            title,
            detail: d,
            retry: RetryAction::Reboot,
        } if title == REBOOT_FAILED_TITLE && d.contains("dnf5daemon-server")
    ));
    assert!(rt.local_calls().is_empty());

    h.run_reboot(&rt);
    assert!(matches!(
        &h.state.phase,
        Phase::Failed {
            title,
            detail: d,
            retry: RetryAction::Reboot,
        } if title == REBOOT_FAILED_TITLE && d.contains("not permitted")
    ));
    assert!(rt.local_calls().is_empty());

    let mut h_ok = FlowHarness::default();
    h_ok.run_reboot(&rt);
    assert_eq!(h_ok.phases, vec!["Idle"]);
    assert!(h_ok
        .console
        .full_text()
        .contains("[logind] Requesting interactive system restart"));
    assert!(rt.local_calls().is_empty());

    // Fallback when logind D-Bus is unavailable invokes `systemctl reboot`.
    h_ok.run_reboot(&rt);
    assert_eq!(rt.logind_reboot_calls(), 4);
    assert_eq!(
        rt.local_calls(),
        vec![("systemctl".into(), vec!["reboot".into()])]
    );
}

#[derive(serde::Deserialize)]
struct NativeProgressSampleFixture {
    snapshot: FlatpakProgressSnapshot,
    now_us: u64,
}

#[derive(serde::Deserialize)]
struct NativeTxFixture {
    requested_refs: Vec<String>,
    ops: Vec<FlatpakOperationMeta>,
    progress_samples: Vec<NativeProgressSampleFixture>,
    non_fatal_details: u32,
    fatal_details: u32,
}

#[test]
fn native_flatpak_transaction_apply_telemetry_extensions_and_appstream_summary() {
    // 1) Discovery fixtures populate AppStream summary & install size delta while
    // preserving shared NVIDIA extension deduplication and single-counted bytes.
    let user_pkgs = flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages;
    let sys_pkgs =
        flatpak_check_from_fixture("flatpak/check_system_nvidia_extension.json", None).packages;
    assert_eq!(
        user_pkgs[0].summary.as_deref(),
        Some("Private messenger with end-to-end encryption")
    );
    assert_eq!(user_pkgs[0].install_size_delta, Some(8_925_568));
    assert_eq!(
        sys_pkgs[0].summary.as_deref(),
        Some("Launcher for the Steam software distribution service")
    );
    assert!(sys_pkgs[0]
        .related_extensions()
        .contains(&"org.freedesktop.Platform.GL32.nvidia-570-133-07".to_string()));

    // 2) Drive FlatpakTransactionTracker from the offline native transaction fixture.
    let tx_fx: NativeTxFixture =
        serde_json::from_value(read_fixture_value("flatpak/native_transaction_events.json"))
            .expect("deserialize native flatpak transaction fixture");
    let mut tracker = FlatpakTransactionTracker::new(tx_fx.requested_refs.clone());
    let mut sys_lines = Vec::new();
    let mut sys_draws = Vec::new();
    let mut sys_hints = Vec::new();

    for ev in tracker.on_ready(tx_fx.ops.clone()) {
        if let Some(l) = ev.console_line {
            sys_lines.push(l);
        }
    }
    let op1_start = tracker.on_new_operation(tx_fx.ops[0].clone());
    if let Some(l) = op1_start.console_line {
        sys_lines.push(l);
    }
    if let Some(h) = op1_start.hint {
        sys_hints.push(h);
    }
    for sample in &tx_fx.progress_samples {
        if let Some(ev) = tracker.on_operation_progress(&sample.snapshot, sample.now_us) {
            if let Some(l) = ev.console_line {
                sys_lines.push(l);
            }
            if let Some(d) = ev.console_draw {
                sys_draws.push(d);
            }
            if let Some(h) = ev.hint {
                sys_hints.push(h);
            }
        }
    }
    let op1_done = tracker.on_operation_done(&tx_fx.ops[0], 0);
    assert!(
        op1_done.hint.is_none(),
        "Steam parent completion must wait for its NVIDIA extension op"
    );
    for op in &tx_fx.ops[1..] {
        let st = tracker.on_new_operation(op.clone());
        if let Some(l) = st.console_line {
            sys_lines.push(l);
        }
        if let Some(h) = st.hint {
            sys_hints.push(h);
        }
        let dn = tracker.on_operation_done(op, 0);
        if let Some(l) = dn.console_line {
            sys_lines.push(l);
        }
        if let Some(h) = dn.hint {
            sys_hints.push(h);
        }
    }
    let sys_outcome = tracker
        .finish_result(true, None, false)
        .expect("native system outcome");
    assert_eq!(sys_outcome.completed_ops, 3);
    assert!(sys_outcome.any_changed);
    assert_eq!(sys_outcome.throughput_samples.len(), 2);

    // Non-fatal vs fatal error handling verified directly from fixture flags.
    let mut err_tracker = FlatpakTransactionTracker::new(tx_fx.requested_refs);
    err_tracker.on_ready(tx_fx.ops.clone());
    err_tracker.on_new_operation(tx_fx.ops[0].clone());
    let (_, cont_nonfatal) = err_tracker.on_operation_error(
        &tx_fx.ops[0],
        "transient CDN 503",
        tx_fx.non_fatal_details,
        false,
    );
    assert!(cont_nonfatal);
    assert_eq!(
        tx_fx.non_fatal_details,
        FLATPAK_TRANSACTION_ERROR_DETAILS_NON_FATAL
    );
    let (_, cont_fatal) =
        err_tracker.on_operation_error(&tx_fx.ops[0], "Socket I/O timed out", tx_fx.fatal_details, false);
    assert!(!cont_fatal);

    // 3) Full native apply pipeline for FlatpakUser + FlatpakSystem bypasses CLI and ensure_session.
    let mut combined = user_pkgs;
    combined.extend(sys_pkgs);

    let rt = FakeRuntime::new();
    let mut user_tracker =
        FlatpakTransactionTracker::new(vec!["app/org.signal.Signal/x86_64/stable".into()]);
    let user_op = FlatpakOperationMeta {
        formatted_ref: "app/org.signal.Signal/x86_64/stable".into(),
        kind: fedora_updater::backend::flatpak::FlatpakOpKind::Update,
        remote: "flathub".into(),
        download_bytes: 44_040_192,
        installed_size_bytes: 378_925_568,
        related_to: vec![],
        is_skipped: false,
    };
    let mut user_lines = Vec::new();
    let mut user_draws = Vec::new();
    let mut user_hints = Vec::new();
    for ev in user_tracker.on_ready(vec![user_op.clone()]) {
        if let Some(l) = ev.console_line {
            user_lines.push(l);
        }
    }
    let u_start = user_tracker.on_new_operation(user_op.clone());
    user_lines.extend(u_start.console_line);
    user_hints.extend(u_start.hint);
    if let Some(u_prog) = user_tracker.on_operation_progress(
        &FlatpakProgressSnapshot {
            status: "Downloading".into(),
            progress_pct: 50,
            bytes_transferred: 22_020_096,
            is_estimating: false,
            start_time_us: 1_000_000,
        },
        2_000_000,
    ) {
        user_draws.extend(u_prog.console_draw);
        user_hints.extend(u_prog.hint);
    }
    let u_done = user_tracker.on_operation_done(&user_op, 0);
    user_lines.extend(u_done.console_line);
    user_hints.extend(u_done.hint);
    let user_outcome = user_tracker.finish_result(true, None, false).unwrap();

    rt.push_flatpak_native_apply(FlatpakNativeApplyStep {
        lines: user_lines,
        draws: user_draws,
        hints: user_hints,
        on_start: None,
        outcome: NativeOutcome::Handled(Ok(user_outcome)),
    });
    rt.push_flatpak_native_apply(FlatpakNativeApplyStep {
        lines: sys_lines,
        draws: sys_draws,
        hints: sys_hints,
        on_start: None,
        outcome: NativeOutcome::Handled(Ok(sys_outcome)),
    });

    let mut h = FlowHarness::ready(combined);
    h.run_apply(&rt);

    assert_eq!(rt.ensure_session_calls(), 0);
    assert!(rt.helper_calls().is_empty());
    assert!(
        rt.local_calls().is_empty(),
        "native FlatpakTransaction must bypass flatpak CLI: {:?}",
        rt.local_calls()
    );
    let calls = rt.flatpak_native_apply_calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].0, "FlatpakUser must execute before FlatpakSystem");
    assert!(!calls[1].0, "FlatpakSystem must execute after FlatpakUser");
    assert!(h.draw_count >= 4, "expected live ConsoleDraw telemetry");
    assert!(h.saw_download_pass);
    assert!(h.saw_apply_pass);

    let full = h.console.full_text();
    assert!(!full.contains("15%…"), "redraw ticks must stay out of log: {full}");
    assert!(full.contains("Prepared 3 Flatpak operation(s)"), "{full}");

    assert!(matches!(
        &h.state.phase,
        Phase::Done {
            failed: 0,
            needs_reboot: false,
            ..
        }
    ));
}

#[test]
fn native_flatpak_transaction_cancel_and_cli_fallback() {
    // 1) Cancellation via WorkCancel propagates directly to a real GLib GCancellable and exits cleanly.
    let rt_cancel = FakeRuntime::new();
    let pkgs = flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages;
    let cancellable = FlatpakCancellable::new().expect("create GCancellable");
    let cancellable_hook = cancellable.clone();

    rt_cancel.push_flatpak_native_apply(FlatpakNativeApplyStep {
        lines: vec!["[1/3] Updating org.signal.Signal".into()],
        draws: vec![],
        hints: vec![],
        on_start: Some(Box::new(move |cancel| {
            let c = cancellable_hook.clone();
            cancel.on_cancel(move || c.cancel());
            cancel.cancel();
        })),
        outcome: NativeOutcome::Handled(Err("cancelled".into())),
    });

    let mut h_cancel = FlowHarness::ready(pkgs.clone());
    h_cancel.console.clear();
    h_cancel.dispatch(Event::StartApply);
    let (tx, rx) = async_channel::unbounded();
    let cancel_token = WorkCancel::new();
    execute_apply_all(&tx, pkgs.clone(), &rt_cancel, &cancel_token);
    drop(tx);
    h_cancel.drain(&rx);

    assert!(
        cancellable.is_cancelled(),
        "Stop / WorkCancel must cancel GCancellable"
    );
    assert!(
        rt_cancel.local_calls().is_empty(),
        "cancelled native transaction must NOT fall back to CLI"
    );
    assert_eq!(phase_name(&h_cancel.state.phase), "Running");
    h_cancel.dispatch(Event::StopApply {
        during_transaction: false,
    });
    assert!(matches!(
        &h_cancel.state.phase,
        Phase::Ready { soft_errors, .. }
            if soft_errors == &vec!["Stopped before install. Downloaded packages were not applied.".to_string()]
    ));

    // 2) Unavailable native apply falls back transparently to `flatpak update` CLI and succeeds.
    let rt_fb = FakeRuntime::new();
    rt_fb.push_flatpak_native_apply(FlatpakNativeApplyStep {
        lines: vec![],
        draws: vec![],
        hints: vec![],
        on_start: None,
        outcome: NativeOutcome::Unavailable("libflatpak missing transaction symbol".into()),
    });
    rt_fb.push_local_fixture(
        "flatpak",
        &["update", "--user", "-y", "--noninteractive"],
        "flatpak/apply_progress_happy.txt",
        0,
    );
    let mut h_fb = FlowHarness::ready(pkgs);
    h_fb.run_apply(&rt_fb);

    assert_eq!(rt_fb.flatpak_native_apply_calls().len(), 1);
    assert_eq!(rt_fb.local_calls().len(), 1);
    assert_eq!(rt_fb.local_calls()[0].0, "flatpak");
    assert!(matches!(
        &h_fb.state.phase,
        Phase::Done {
            failed: 0,
            needs_reboot: false,
            ..
        }
    ));

    // 3) Actionable fatal native error is reported without invoking CLI fallback.
    let rt_err = FakeRuntime::new();
    let one_pkg = vec![flatpak_check_from_fixture("flatpak/check_user_normal.json", None)
        .packages[0]
        .clone()];
    rt_err.push_flatpak_native_apply(FlatpakNativeApplyStep {
        lines: vec!["error: Failed to update org.signal.Signal: Socket I/O timed out".into()],
        draws: vec![],
        hints: vec![],
        on_start: None,
        outcome: NativeOutcome::Handled(Err("Failed to update org.signal.Signal: Socket I/O timed out".into())),
    });
    let mut h_err = FlowHarness::ready(one_pkg);
    h_err.run_apply(&rt_err);
    assert!(rt_err.local_calls().is_empty());
    assert!(matches!(
        &h_err.state.phase,
        Phase::Failed { title, detail, retry: RetryAction::CheckAgain }
            if title == "All updates failed" && detail.contains("Socket I/O timed out")
    ));
}

#[derive(serde::Deserialize)]
struct DnfDbusCheckFixture {
    installed: Vec<DnfRpmRecord>,
    advisories: Vec<DnfAdvisoryRecord>,
}

fn packages_from_dnf_dbus_fixtures() -> Vec<Package> {
    let upgrades = parse_list_fd_ndjson(&read_fixture("dnf_dbus/list_fd_stream.ndjson"))
        .expect("parse list_fd_stream.ndjson");
    let extra: DnfDbusCheckFixture =
        serde_json::from_value(read_fixture_value("dnf_dbus/check_upgrades_and_advisories.json"))
            .expect("parse check_upgrades_and_advisories.json");
    packages_from_dnf_dbus(&upgrades, &extra.installed, &extra.advisories)
}

#[test]
fn native_dnf5daemon_zero_auth_check_cves_and_selective_apply() {
    let pkgs = packages_from_dnf_dbus_fixtures();
    assert_eq!(pkgs.len(), 3);
    assert_eq!(count_security(&pkgs), 1);

    let openssl = pkgs.iter().find(|p| p.name == "openssl").unwrap();
    assert_eq!(openssl.old_version.as_deref(), Some("3.2.2-1.fc44"));
    assert_eq!(openssl.version, "3.2.2-3.fc44");
    assert_eq!(openssl.install_size_delta, Some(400_000));
    assert_eq!(
        openssl.summary.as_deref(),
        Some("Utilities from the general purpose cryptography library")
    );
    assert!(openssl.has_security_advisory());
    assert!(openssl.advisories.iter().any(|a| {
        a.kind == AdvisoryKind::Security
            && a.severity.as_deref() == Some("Important")
            && a.cves.iter().any(|c| c == "CVE-2026-2401")
            && a.bugzillas.iter().any(|b| b == "RHBZ#2458001")
    }));

    let kernel = pkgs
        .iter()
        .find(|p| p.name == "kernel-core")
        .unwrap();
    assert!(dnf::package_implies_reboot(&kernel.name));

    let rt = FakeRuntime::new();
    rt.set_dnf_dbus_available(true);
    rt.push_dnf_dbus_check(NativeOutcome::Handled(Ok(pkgs)));
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_empty(true);
    rt.push_flatpak_empty(false);
    rt.push_fwupd_dbus_check(NativeOutcome::Handled(Ok(Vec::new())));

    assert!(can_skip_check_auth_ui(true));
    assert!(can_skip_apply_auth_ui(true));

    let mut h = FlowHarness::new();
    h.run_check(&rt, false);
    assert_eq!(rt.dnf_dbus_check_calls(), 1);
    assert_eq!(rt.ensure_session_calls(), 0);
    assert!(rt.helper_calls().is_empty());

    let ready_pkgs = h.state.packages().to_vec();
    assert_eq!(ready_pkgs.len(), 3);
    let selected_ids: Vec<String> = ready_pkgs
        .iter()
        .filter(|p| p.name == "openssl" || p.name == "kernel-core")
        .map(|p| p.id.clone())
        .collect();
    assert_eq!(selected_ids.len(), 2);

    let signals: Vec<DnfDbusSignal> =
        serde_json::from_value(read_fixture_value("dnf_dbus/apply_signals_happy.json"))
            .expect("parse apply_signals_happy.json");
    let mut tracker = DnfSignalTracker::new(2_457_600 + 18_874_368);
    let mut lines = Vec::new();
    let mut draws = Vec::new();
    let mut hints = Vec::new();
    let mut throughput = Vec::new();
    let mut entered_txn = false;
    let mut txn_done = None;

    for sig in signals {
        let step = tracker.apply(sig);
        if step.entered_transaction {
            entered_txn = true;
        }
        if let Some(done) = step.transaction_finished {
            txn_done = Some(done);
        }
        if let Some(l) = step.log_line {
            lines.push(l);
        }
        if let Some(d) = step.draw_line {
            draws.push(d);
        }
        if let Some(hint) = step.hint {
            hints.push(hint);
        }
        if let Some(tp) = step.throughput {
            throughput.push(tp);
        }
    }
    assert!(entered_txn);
    assert_eq!(txn_done, Some(true));
    assert!(!throughput.is_empty());
    assert!(hints
        .iter()
        .any(|h| h.phase_label.as_deref() == Some("Downloading packages")));
    assert!(hints
        .iter()
        .any(|h| h.phase_label.as_deref() == Some("Preparing transaction")));
    assert!(hints
        .iter()
        .any(|h| h.phase_label.as_deref() == Some("Installing packages")));
    assert!(hints
        .iter()
        .any(|h| h.phase_label.as_deref() == Some("Verifying packages")));

    rt.push_dnf_dbus_apply(DnfDbusApplyStep {
        lines,
        draws,
        hints,
        throughput,
        on_start: None,
        outcome: NativeOutcome::Handled(Ok(DnfApplyOutcome {
            needs_reboot: true,
            offline_scheduled: false,
            download_only: false,
            cancelled: false,
        })),
    });

    h.run_apply_mode(&rt, selected_ids, ApplyMode::Live);

    assert_eq!(rt.ensure_session_calls(), 0);
    assert!(rt.helper_calls().is_empty());
    let calls = rt.dnf_dbus_apply_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, ApplyMode::Live);
    assert_eq!(calls[0].0.len(), 2);
    assert!(calls[0].0.iter().any(|s| s == "openssl.x86_64"));
    assert!(calls[0].0.iter().any(|s| s == "kernel-core.x86_64"));
    assert!(!calls[0].0.iter().any(|s| s.starts_with("vim-enhanced")));

    assert!(h.saw_download_pass);
    assert!(h.saw_verifying_heading);
    assert!(!h.throughput_samples.is_empty());
    assert!(h.throughput_samples.iter().any(|(bps, _, _)| *bps > 0.0));
    assert!(matches!(
        &h.state.phase,
        Phase::Done {
            packages,
            failed: 0,
            needs_reboot: true,
            ..
        } if packages.len() == 2
    ));
}

#[test]
fn native_dnf5daemon_offline_on_reboot_and_download_only_modes() {
    let dnf_pkgs = packages_from_dnf_dbus_fixtures();
    let sel_ids: Vec<String> = dnf_pkgs.iter().take(2).map(|p| p.id.clone()).collect();

    // 1) OfflineOnReboot schedules offline upgrade and sets needs_reboot = true.
    let rt_offline = FakeRuntime::new();
    rt_offline.set_dnf_dbus_available(true);
    rt_offline.push_dnf_dbus_apply(DnfDbusApplyStep {
        lines: vec!["Scheduled offline update for next reboot.".into()],
        draws: vec![],
        hints: vec![],
        throughput: vec![],
        on_start: None,
        outcome: NativeOutcome::Handled(Ok(DnfApplyOutcome {
            needs_reboot: true,
            offline_scheduled: true,
            download_only: false,
            cancelled: false,
        })),
    });

    let mut h_off = FlowHarness::ready(dnf_pkgs.clone());
    h_off.run_apply_mode(&rt_offline, sel_ids.clone(), ApplyMode::OfflineOnReboot);
    let calls = rt_offline.dnf_dbus_apply_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, ApplyMode::OfflineOnReboot);
    assert_eq!(rt_offline.ensure_session_calls(), 0);
    assert!(matches!(
        &h_off.state.phase,
        Phase::Done {
            packages,
            failed: 0,
            needs_reboot: true,
            ..
        } if packages.len() == 2
    ));

    // 2) DownloadOnly skips Flatpak & Firmware apply steps and does not ask to reboot.
    let mut mixed = vec![dnf_pkgs[0].clone()];
    mixed.extend(flatpak_check_from_fixture("flatpak/check_user_normal.json", None).packages);
    mixed.push(Package {
        id: "fw:dev-1".into(),
        name: "System Firmware".into(),
        arch: "firmware".into(),
        version: "0.1.32".into(),
        old_version: Some("0.1.31".into()),
        source: UpdateSource::Firmware,
        repo: "lvfs".into(),
        size_bytes: Some(1_048_576),
        install_size_delta: None,
        summary: None,
        advisories: Vec::new(),
        preflight_warning: None,
        detail: "Firmware update".into(),
        status: PackageStatus::Pending,
    });
    let all_ids: Vec<String> = mixed.iter().map(|p| p.id.clone()).collect();

    let rt_dl = FakeRuntime::new();
    rt_dl.set_dnf_dbus_available(true);
    rt_dl.push_dnf_dbus_apply(DnfDbusApplyStep {
        lines: vec!["Downloaded 1 RPM package(s); skipped install (Download Only).".into()],
        draws: vec![],
        hints: vec![],
        throughput: vec![],
        on_start: None,
        outcome: NativeOutcome::Handled(Ok(DnfApplyOutcome {
            needs_reboot: false,
            offline_scheduled: false,
            download_only: true,
            cancelled: false,
        })),
    });

    let mut h_dl = FlowHarness::ready(mixed);
    h_dl.run_apply_mode(&rt_dl, all_ids, ApplyMode::DownloadOnly);
    let dl_calls = rt_dl.dnf_dbus_apply_calls();
    assert_eq!(dl_calls.len(), 1);
    assert_eq!(dl_calls[0].1, ApplyMode::DownloadOnly);
    assert!(rt_dl.local_calls().is_empty());
    assert!(rt_dl.helper_calls().is_empty());
    assert!(h_dl
        .console
        .lines()
        .any(|l| l.contains("Download Only")));
    assert!(matches!(
        &h_dl.state.phase,
        Phase::Done {
            failed: 0,
            needs_reboot: false,
            ..
        }
    ));
}

#[test]
fn native_dnf5daemon_goal_cancel_during_download_and_cli_fallback() {
    let dnf_pkgs = packages_from_dnf_dbus_fixtures();
    let one_dnf = vec![dnf_pkgs[0].clone()];

    // 1) Goal.cancel() fires during download stage before transaction begins.
    let goal_cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let goal_flag = goal_cancelled.clone();

    let rt_cancel = FakeRuntime::new();
    rt_cancel.set_dnf_dbus_available(true);
    rt_cancel.push_dnf_dbus_apply(DnfDbusApplyStep {
        lines: vec!["Downloading openssl-0:3.2.2-3.fc44.x86_64".into()],
        draws: vec![],
        hints: vec![],
        throughput: vec![],
        on_start: Some(Box::new(move |cancel| {
            let flag = goal_flag.clone();
            cancel.on_cancel(move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            cancel.cancel();
        })),
        outcome: NativeOutcome::Handled(Ok(DnfApplyOutcome {
            needs_reboot: false,
            offline_scheduled: false,
            download_only: false,
            cancelled: true,
        })),
    });

    let mut h_cancel = FlowHarness::ready(one_dnf.clone());
    h_cancel.console.clear();
    h_cancel.dispatch(Event::StartApply);
    let (tx, rx) = async_channel::unbounded();
    let cancel_token = WorkCancel::new();
    execute_apply_all_with_mode(&tx, one_dnf.clone(), ApplyMode::Live, &rt_cancel, &cancel_token);
    drop(tx);
    h_cancel.drain(&rx);

    assert!(goal_cancelled.load(std::sync::atomic::Ordering::SeqCst));
    assert!(rt_cancel.helper_calls().is_empty());
    assert_eq!(phase_name(&h_cancel.state.phase), "Running");
    h_cancel.dispatch(Event::StopApply {
        during_transaction: false,
    });
    assert!(matches!(
        &h_cancel.state.phase,
        Phase::Ready { soft_errors, .. }
            if soft_errors
                == &vec!["Stopped before install. Downloaded packages were not applied.".to_string()]
    ));

    // 2) When dnf5daemon is Unavailable, apply transparently falls back to PrivilegedSession CLI helper.
    let rt_fb = FakeRuntime::new();
    rt_fb.set_dnf_dbus_available(false);
    rt_fb.push_ensure_session(Ok(()));
    rt_fb.push_helper_fixture(HelperCommand::ApplyDnf, "dnf/apply_happy_kernel.txt", 0);

    let mut h_fb = FlowHarness::ready(one_dnf);
    h_fb.run_apply(&rt_fb);

    assert_eq!(rt_fb.ensure_session_calls(), 1);
    assert_eq!(rt_fb.helper_calls(), vec![HelperCommand::ApplyDnf]);
    assert!(matches!(
        &h_fb.state.phase,
        Phase::Done {
            packages,
            failed: 0,
            ..
        } if packages.len() == 1
    ));
}

#[test]
fn native_merged_dnf_and_fwupd_history_timeline() {
    let dnf_txns: Vec<DnfHistoryTransaction> =
        serde_json::from_value(read_fixture_value("dnf_dbus/history_transactions.json"))
            .expect("parse dnf history_transactions.json");
    let fwupd_entries: Vec<FwupdHistoryEntry> =
        serde_json::from_value(read_fixture_value("fwupd/dbus_history.json"))
            .expect("parse fwupd dbus_history.json");

    let rt = FakeRuntime::new();
    rt.push_dnf_history(NativeOutcome::Handled(Ok(dnf_txns.clone())));
    rt.push_fwupd_history(NativeOutcome::Handled(Ok(fwupd_entries.clone())));

    let merged = execute_load_history(&rt, 50);
    assert_eq!(rt.ensure_session_calls(), 0);
    assert_eq!(merged.len(), 4);

    // Chronological newest-first order across DNF and fwupd:
    // 1795224552 (DNF #203) -> 1789080623 (fwupd UEFI dbx Failed) ->
    // 1788900320 (fwupd System Firmware Success) -> 1700000015 (DNF #202)
    assert_eq!(merged[0].source, HistorySource::Dnf);
    assert_eq!(merged[0].id, "dnf-txn-203");
    assert!(merged[0].succeeded);
    assert_eq!(merged[0].changes.len(), 2);

    assert_eq!(merged[1].source, HistorySource::Firmware);
    assert!(!merged[1].succeeded);
    assert!(merged[1].title.contains("UEFI dbx"));

    assert_eq!(merged[2].source, HistorySource::Firmware);
    assert!(merged[2].succeeded);
    assert!(merged[2].title.contains("System Firmware"));

    assert_eq!(merged[3].source, HistorySource::Dnf);
    assert_eq!(merged[3].id, "dnf-txn-202");
    assert!(merged[3].succeeded);

    let direct = merge_history(&dnf_txns, &fwupd_entries);
    assert_eq!(merged, direct);
}

#[test]
fn native_apply_errors_are_logged_to_activity_console_without_duplication() {
    let dnf_pkg = packages_from_dnf_dbus_fixtures().into_iter().next().expect("dnf pkg");
    let fp_pkg = flatpak_check_from_fixture("flatpak/check_user_normal.json", None)
        .packages
        .into_iter()
        .next()
        .expect("flatpak pkg");
    let fw_pkg = Package {
        id: "fwupd:dev-1".into(),
        name: "System Firmware".into(),
        arch: String::new(),
        version: "0.1.32".into(),
        old_version: Some("0.1.31".into()),
        source: UpdateSource::Firmware,
        repo: "LVFS".into(),
        size_bytes: Some(1_048_576),
        install_size_delta: None,
        summary: None,
        advisories: Vec::new(),
        preflight_warning: None,
        detail: "0.1.31 → 0.1.32".into(),
        status: PackageStatus::Pending,
    };

    // 1) Backends returning Handled(Err(...)) without calling on_line still emit to console.
    let rt_unlogged = FakeRuntime::new();
    rt_unlogged.set_dnf_dbus_available(true);
    rt_unlogged.set_fwupd_dbus_available(true);
    rt_unlogged.push_dnf_dbus_apply(DnfDbusApplyStep {
        lines: vec!["Resolved transaction (1 item(s)); starting transaction…".into()],
        draws: vec![],
        hints: vec![],
        throughput: vec![],
        on_start: None,
        outcome: NativeOutcome::Handled(Err(
            "org.rpm.dnf.v0.Error: Session method failed\nSecond detail line".into(),
        )),
    });
    rt_unlogged.push_flatpak_native_apply(FlatpakNativeApplyStep {
        lines: vec![],
        draws: vec![],
        hints: vec![],
        on_start: None,
        outcome: NativeOutcome::Handled(Err("Socket I/O timed out".into())),
    });
    rt_unlogged.push_fwupd_dbus_apply(FwupdDbusApplyStep {
        lines: vec!["Installing System Firmware 0.1.32…".into()],
        hints: vec![],
        outcome: NativeOutcome::Handled(Err(
            "System battery is too low (10% < 25% required)".into(),
        )),
    });
    rt_unlogged.push_fwupd_dbus_reboot_needed(NativeOutcome::Handled(Ok(false)));

    let pkgs = vec![dnf_pkg.clone(), fp_pkg.clone(), fw_pkg.clone()];
    let mut h1 = FlowHarness::ready(pkgs.clone());
    h1.run_apply(&rt_unlogged);

    let log_lines1: Vec<&str> = h1.console.lines().collect();
    assert!(
        log_lines1.contains(&"[dnf] error: org.rpm.dnf.v0.Error: Session method failed"),
        "expected [dnf] error line in console, got: {log_lines1:#?}"
    );
    assert!(
        log_lines1.contains(&"[dnf] Second detail line"),
        "expected multi-line follow-up in console, got: {log_lines1:#?}"
    );
    assert!(
        log_lines1.contains(&"[flatpak-user] error: Socket I/O timed out"),
        "expected [flatpak-user] error line in console, got: {log_lines1:#?}"
    );
    assert!(
        log_lines1.contains(&"[fwupd] error: System battery is too low (10% < 25% required)"),
        "expected [fwupd] error line in console, got: {log_lines1:#?}"
    );
    assert!(matches!(
        &h1.state.phase,
        Phase::Failed { title, detail, .. }
            if title == "All updates failed"
                && detail.contains("Second detail line")
                && detail.contains("Socket I/O timed out")
                && detail.contains("System battery is too low")
    ));

    // 2) Backends that already logged the error via on_line do not duplicate it in console.
    let rt_logged = FakeRuntime::new();
    rt_logged.set_dnf_dbus_available(true);
    rt_logged.set_fwupd_dbus_available(true);
    rt_logged.push_dnf_dbus_apply(DnfDbusApplyStep {
        lines: vec!["Problem: conflicting requests".into()],
        draws: vec![],
        hints: vec![],
        throughput: vec![],
        on_start: None,
        outcome: NativeOutcome::Handled(Err("Problem: conflicting requests".into())),
    });
    rt_logged.push_flatpak_native_apply(FlatpakNativeApplyStep {
        lines: vec!["error: Failed to update org.signal.Signal: aborted".into()],
        draws: vec![],
        hints: vec![],
        on_start: None,
        outcome: NativeOutcome::Handled(Err(
            "Failed to update org.signal.Signal: aborted".into(),
        )),
    });
    rt_logged.push_fwupd_dbus_apply(FwupdDbusApplyStep {
        lines: vec!["error: Capsule rejected".into()],
        hints: vec![],
        outcome: NativeOutcome::Handled(Err("error: Capsule rejected".into())),
    });
    rt_logged.push_fwupd_dbus_reboot_needed(NativeOutcome::Handled(Ok(false)));

    let mut h2 = FlowHarness::ready(pkgs);
    h2.run_apply(&rt_logged);

    let log_lines2: Vec<&str> = h2.console.lines().collect();
    assert_eq!(
        log_lines2
            .iter()
            .filter(|l| l.contains("conflicting requests"))
            .count(),
        1,
        "dnf error should appear exactly once: {log_lines2:#?}"
    );
    assert_eq!(
        log_lines2
            .iter()
            .filter(|l| l.contains("Failed to update org.signal.Signal: aborted"))
            .count(),
        1,
        "flatpak error should appear exactly once: {log_lines2:#?}"
    );
    assert_eq!(
        log_lines2
            .iter()
            .filter(|l| l.contains("Capsule rejected"))
            .count(),
        1,
        "fwupd error should appear exactly once: {log_lines2:#?}"
    );
}

#[test]
fn disabled_source_is_skipped_cleanly_without_warnings() {
    // 1) Firmware disabled: neither fwupd D-Bus nor fwupdmgr helper is touched, zero soft errors.
    let rt = FakeRuntime::new();
    rt.push_helper_fixture(HelperCommand::CheckDnf, "dnf/check_update_classic.txt", 100);
    rt.push_local_fixture(
        "dnf",
        dnf::REPOQUERY_UPGRADES_ARGS,
        "dnf/repoquery_sizes.txt",
        0,
    );
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_query(
        true,
        Ok(flatpak_check_from_fixture("flatpak/check_user_normal.json", None)),
    );
    rt.push_flatpak_query(
        false,
        Ok(flatpak_check_from_fixture(
            "flatpak/check_system_nvidia_extension.json",
            None,
        )),
    );

    let mut cfg = SourceConfig::all_enabled();
    cfg.disable("firmware");

    let mut h = FlowHarness::new();
    h.console.clear();
    h.dispatch(Event::StartCheck { then_apply: false });
    let (tx, rx) = async_channel::unbounded();
    let cancel = WorkCancel::new();
    execute_check_all_with_config(&tx, &rt, &cancel, &cfg);
    drop(tx);
    h.drain(&rx);

    assert_eq!(rt.fwupd_dbus_check_calls(), 0);
    assert!(!rt.helper_calls().contains(&HelperCommand::CheckFwupd));
    match &h.state.phase {
        Phase::Ready {
            packages,
            soft_errors,
        } => {
            assert!(soft_errors.is_empty(), "expected zero warnings, got {soft_errors:?}");
            assert!(!packages.is_empty());
            assert!(packages.iter().all(|p| p.source != UpdateSource::Firmware));
        }
        other => panic!("expected Phase::Ready, got {other:?}"),
    }

    // 2) DNF disabled: ensure_session() is never invoked and only unprivileged Flatpak runs.
    let rt_no_dnf = FakeRuntime::new();
    rt_no_dnf.push_flatpak_appstream_ok();
    rt_no_dnf.push_flatpak_query(
        true,
        Ok(flatpak_check_from_fixture("flatpak/check_user_normal.json", None)),
    );
    rt_no_dnf.push_flatpak_empty(false);

    let mut cfg_no_dnf = SourceConfig::all_enabled();
    cfg_no_dnf.disable("dnf");
    cfg_no_dnf.disable("firmware");

    let mut h2 = FlowHarness::new();
    h2.dispatch(Event::StartCheck { then_apply: false });
    let (tx2, rx2) = async_channel::unbounded();
    let cancel2 = WorkCancel::new();
    execute_check_all_with_config(&tx2, &rt_no_dnf, &cancel2, &cfg_no_dnf);
    drop(tx2);
    h2.drain(&rx2);

    assert_eq!(rt_no_dnf.ensure_session_calls(), 0);
    assert!(rt_no_dnf.helper_calls().is_empty());
    match &h2.state.phase {
        Phase::Ready {
            packages,
            soft_errors,
        } => {
            assert!(soft_errors.is_empty());
            assert!(packages.iter().all(|p| p.source == UpdateSource::FlatpakUser));
        }
        other => panic!("expected Phase::Ready, got {other:?}"),
    }
}

#[test]
fn single_source_soft_failure_isolated_while_other_enabled_sources_succeed() {
    let rt = FakeRuntime::new();
    rt.set_dnf_dbus_available(true);
    rt.push_dnf_dbus_check(NativeOutcome::Handled(Err("DNF repo metadata expired".into())));
    rt.push_flatpak_appstream_ok();
    rt.push_flatpak_query(
        true,
        Ok(flatpak_check_from_fixture("flatpak/check_user_normal.json", None)),
    );
    rt.push_flatpak_empty(false);

    let mut cfg = SourceConfig::all_enabled();
    cfg.disable("firmware");

    let mut h = FlowHarness::new();
    h.dispatch(Event::StartCheck { then_apply: false });
    let (tx, rx) = async_channel::unbounded();
    let cancel = WorkCancel::new();
    execute_check_all_with_config(&tx, &rt, &cancel, &cfg);
    drop(tx);
    h.drain(&rx);

    match &h.state.phase {
        Phase::Ready {
            packages,
            soft_errors,
        } => {
            assert_eq!(soft_errors, &vec!["DNF repo metadata expired".to_string()]);
            assert!(!packages.is_empty());
            assert!(packages.iter().all(|p| p.source == UpdateSource::FlatpakUser));
        }
        other => panic!("expected Phase::Ready, got {other:?}"),
    }
}

#[test]
fn apply_honors_disabled_source_config() {
    let rt = FakeRuntime::new();
    rt.set_dnf_dbus_available(true);
    rt.push_dnf_dbus_apply(DnfDbusApplyStep {
        lines: vec!["Upgrading bash".into()],
        draws: vec![],
        hints: vec![],
        throughput: vec![],
        on_start: None,
        outcome: NativeOutcome::Handled(Ok(DnfApplyOutcome {
            needs_reboot: false,
            cancelled: false,
            offline_scheduled: false,
            download_only: false,
        })),
    });

    let bash = Package::new_dnf("bash", "x86_64", "5.2.37-1.fc44", "updates");
    let mut fw = Package::new_dnf("System Firmware", "", "1.2.3", "LVFS");
    fw.id = "fwupd:sys".into();
    fw.source = UpdateSource::Firmware;

    let mut cfg = SourceConfig::all_enabled();
    cfg.disable("firmware");

    let mut h = FlowHarness::ready(vec![bash.clone(), fw.clone()]);
    h.dispatch(Event::StartApply);
    let (tx, rx) = async_channel::unbounded();
    let cancel = WorkCancel::new();
    execute_apply_all_with_mode_and_config(
        &tx,
        vec![bash, fw],
        ApplyMode::Live,
        &rt,
        &cancel,
        &cfg,
    );
    drop(tx);
    h.drain(&rx);

    assert_eq!(rt.fwupd_dbus_apply_calls(), 0);
    assert_eq!(rt.dnf_dbus_apply_calls().len(), 1);
    assert!(matches!(h.state.phase, Phase::Done { failed: 0, .. }));
}
