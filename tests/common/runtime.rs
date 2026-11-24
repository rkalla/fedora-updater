#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fedora_updater::backend::brew::LINUXBREW_SYSTEM_PATH;
use fedora_updater::backend::containers::{DISTROBOX_DEFAULT_PATH, TOOLBOX_DEFAULT_PATH};

use fedora_updater::backend::dnf_dbus::{
    upgrade_specs_from_packages, DnfApplyOutcome, DnfHistoryTransaction, ThroughputSample,
};
use fedora_updater::backend::flatpak::{
    packages_from_flatpak_updates, FlatpakApplyOutcome, FlatpakCheck,
};
use fedora_updater::backend::fwupd_dbus::{FwupdApplyOutcome, FwupdHistoryEntry};
use fedora_updater::backend::gnome_extensions::{
    apply_extensions_with, GnomeExtensionsApplyOutcome,
};
use fedora_updater::backend::logind::RebootPreflight;
use fedora_updater::backend::rpm_ostree::OstreeStatus;
use fedora_updater::backend::{NativeOutcome, ProgressHint};
use fedora_updater::helper_protocol::{HelperCommand, DRAW_PREFIX};
use fedora_updater::model::{ApplyMode, Package};
use fedora_updater::orchestrator::{OrchestratorRuntime, WorkCancel};
use fedora_updater::privilege::SessionError;

use super::fixtures::{read_fixture, read_flatpak_fixture};

/// Convert raw fixture output (possibly containing inline `\r` redraws or CRLF)
/// into the wire lines emitted by `helper_mode` (`__DRAW__ ...` for redraws,
/// plain strings for committed lines).
pub fn fixture_to_helper_lines(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in raw.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let segments: Vec<&str> = line.split('\r').filter(|s| !s.is_empty()).collect();
        if let Some((last, head)) = segments.split_last() {
            for seg in head {
                out.push(format!("{DRAW_PREFIX}{seg}"));
            }
            out.push((*last).to_string());
        }
    }
    out
}

/// Split plain command stdout/stderr fixture text into committed lines.
pub fn fixture_to_local_lines(raw: &str) -> Vec<String> {
    raw.lines().map(String::from).collect()
}

/// Build a [`FlatpakCheck`] from one of the checked-in `flatpak/*.json` fixtures.
pub fn flatpak_check_from_fixture(rel: &str, warning: Option<&str>) -> FlatpakCheck {
    let fx = read_flatpak_fixture(rel);
    let packages = packages_from_flatpak_updates(fx.update_source(), &fx.refs, &fx.ops);
    FlatpakCheck {
        packages,
        warning: warning.map(str::to_string),
    }
}

pub struct EnsureSessionStep {
    pub result: Result<(), SessionError>,
    pub hook: Option<Box<dyn FnMut() + Send>>,
}

pub struct HelperStep {
    pub lines: Vec<String>,
    pub result: Result<i32, SessionError>,
    pub hook_after_line: Option<(usize, Box<dyn FnMut() + Send>)>,
}

pub struct LocalStep {
    pub lines: Vec<String>,
    pub result: Result<i32, String>,
    pub hook_after_line: Option<(usize, Box<dyn FnMut() + Send>)>,
}

pub struct FwupdDbusApplyStep {
    pub lines: Vec<String>,
    pub hints: Vec<ProgressHint>,
    pub outcome: NativeOutcome<FwupdApplyOutcome>,
}

pub type DnfOnStartHook = Box<dyn FnMut(&WorkCancel) + Send>;

pub struct DnfDbusApplyStep {
    pub lines: Vec<String>,
    pub draws: Vec<String>,
    pub hints: Vec<ProgressHint>,
    pub throughput: Vec<ThroughputSample>,
    pub on_start: Option<DnfOnStartHook>,
    pub outcome: NativeOutcome<DnfApplyOutcome>,
}

pub type FlatpakOnStartHook = Box<dyn FnMut(&WorkCancel) + Send>;

pub struct FlatpakNativeApplyStep {
    pub lines: Vec<String>,
    pub draws: Vec<String>,
    pub hints: Vec<ProgressHint>,
    pub on_start: Option<FlatpakOnStartHook>,
    pub outcome: NativeOutcome<FlatpakApplyOutcome>,
}

pub type GnomeExtInstallHandler =
    Box<dyn FnMut(&str, &WorkCancel) -> Result<String, String> + Send>;

pub struct GnomeExtApplyStep {
    pub lines: Vec<String>,
    pub hints: Vec<ProgressHint>,
    pub on_start: Option<DnfOnStartHook>,
    pub install_handler: Option<GnomeExtInstallHandler>,
    pub outcome: NativeOutcome<GnomeExtensionsApplyOutcome>,
}

struct LocalMatcher {
    program: String,
    required_args: Vec<String>,
    step: LocalStep,
}

impl LocalMatcher {
    fn matches(&self, program: &str, args: &[&str]) -> bool {
        self.program == program
            && self
                .required_args
                .iter()
                .all(|req| args.contains(&req.as_str()))
    }
}

pub type LocalEnvCall = (String, Vec<String>, Vec<(String, String)>);

struct FakeState {
    now: SystemTime,
    ensure_session_calls: usize,
    helper_calls: Vec<HelperCommand>,
    local_calls: Vec<(String, Vec<String>)>,
    local_env_calls: Vec<LocalEnvCall>,
    ostree_booted: bool,
    rpm_ostree_binary: Option<PathBuf>,
    rpm_ostree_status_steps: VecDeque<NativeOutcome<OstreeStatus>>,
    cancel_rpm_ostree_calls: usize,
    brew_binary: Option<PathBuf>,
    rustup_binary: Option<PathBuf>,
    mise_binary: Option<PathBuf>,
    uv_binary: Option<PathBuf>,
    pipx_binary: Option<PathBuf>,
    distrobox_binary: Option<PathBuf>,
    toolbox_binary: Option<PathBuf>,
    flatpak_queries: Vec<bool>,
    flatpak_native_apply_calls: Vec<(bool, Vec<String>)>,
    dnf_dbus_available: bool,
    dnf_dbus_check_calls: usize,
    dnf_dbus_apply_calls: Vec<(Vec<String>, ApplyMode)>,
    fwupd_dbus_available: bool,
    fwupd_dbus_check_calls: usize,
    fwupd_dbus_apply_calls: usize,
    fwupd_dbus_reboot_calls: usize,
    gnome_extensions_available: bool,
    gnome_extensions_check_calls: usize,
    gnome_extensions_apply_calls: Vec<Vec<String>>,
    logind_preflight_calls: usize,
    logind_reboot_calls: usize,
    ensure_session_steps: VecDeque<EnsureSessionStep>,
    helper_steps: HashMap<HelperCommand, VecDeque<HelperStep>>,
    local_steps: VecDeque<LocalMatcher>,
    flatpak_user_steps: VecDeque<Result<FlatpakCheck, String>>,
    flatpak_system_steps: VecDeque<Result<FlatpakCheck, String>>,
    flatpak_native_apply_steps: VecDeque<FlatpakNativeApplyStep>,
    dnf_dbus_check_steps: VecDeque<NativeOutcome<Vec<Package>>>,
    dnf_dbus_apply_steps: VecDeque<DnfDbusApplyStep>,
    dnf_history_steps: VecDeque<NativeOutcome<Vec<DnfHistoryTransaction>>>,
    fwupd_dbus_check_steps: VecDeque<NativeOutcome<Vec<Package>>>,
    fwupd_dbus_apply_steps: VecDeque<FwupdDbusApplyStep>,
    fwupd_dbus_reboot_steps: VecDeque<NativeOutcome<bool>>,
    fwupd_history_steps: VecDeque<NativeOutcome<Vec<FwupdHistoryEntry>>>,
    gnome_extensions_check_steps: VecDeque<NativeOutcome<Vec<Package>>>,
    gnome_extensions_apply_steps: VecDeque<GnomeExtApplyStep>,
    logind_preflight_steps: VecDeque<NativeOutcome<RebootPreflight>>,
    logind_reboot_steps: VecDeque<(Vec<String>, NativeOutcome<()>)>,
}

/// Deterministic offline [`OrchestratorRuntime`] for functional tests.
#[derive(Clone)]
pub struct FakeRuntime {
    inner: Arc<Mutex<FakeState>>,
}

impl Default for FakeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRuntime {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeState {
                now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                ensure_session_calls: 0,
                helper_calls: Vec::new(),
                local_calls: Vec::new(),
                local_env_calls: Vec::new(),
                ostree_booted: false,
                rpm_ostree_binary: None,
                rpm_ostree_status_steps: VecDeque::new(),
                cancel_rpm_ostree_calls: 0,
                brew_binary: None,
                rustup_binary: None,
                mise_binary: None,
                uv_binary: None,
                pipx_binary: None,
                distrobox_binary: None,
                toolbox_binary: None,
                flatpak_queries: Vec::new(),
                flatpak_native_apply_calls: Vec::new(),
                dnf_dbus_available: false,
                dnf_dbus_check_calls: 0,
                dnf_dbus_apply_calls: Vec::new(),
                fwupd_dbus_available: false,
                fwupd_dbus_check_calls: 0,
                fwupd_dbus_apply_calls: 0,
                fwupd_dbus_reboot_calls: 0,
                gnome_extensions_available: false,
                gnome_extensions_check_calls: 0,
                gnome_extensions_apply_calls: Vec::new(),
                logind_preflight_calls: 0,
                logind_reboot_calls: 0,
                ensure_session_steps: VecDeque::new(),
                helper_steps: HashMap::new(),
                local_steps: VecDeque::new(),
                flatpak_user_steps: VecDeque::new(),
                flatpak_system_steps: VecDeque::new(),
                flatpak_native_apply_steps: VecDeque::new(),
                dnf_dbus_check_steps: VecDeque::new(),
                dnf_dbus_apply_steps: VecDeque::new(),
                dnf_history_steps: VecDeque::new(),
                fwupd_dbus_check_steps: VecDeque::new(),
                fwupd_dbus_apply_steps: VecDeque::new(),
                fwupd_dbus_reboot_steps: VecDeque::new(),
                fwupd_history_steps: VecDeque::new(),
                gnome_extensions_check_steps: VecDeque::new(),
                gnome_extensions_apply_steps: VecDeque::new(),
                logind_preflight_steps: VecDeque::new(),
                logind_reboot_steps: VecDeque::new(),
            })),
        }
    }

    pub fn set_now(&self, now: SystemTime) {
        self.inner.lock().unwrap().now = now;
    }

    pub fn current_now(&self) -> SystemTime {
        self.inner.lock().unwrap().now
    }

    pub fn advance_now(&self, delta: Duration) {
        let mut s = self.inner.lock().unwrap();
        s.now += delta;
    }

    pub fn push_ensure_session(&self, result: Result<(), SessionError>) {
        self.inner
            .lock()
            .unwrap()
            .ensure_session_steps
            .push_back(EnsureSessionStep { result, hook: None });
    }

    pub fn push_ensure_session_hook(
        &self,
        result: Result<(), SessionError>,
        hook: impl FnMut() + Send + 'static,
    ) {
        self.inner
            .lock()
            .unwrap()
            .ensure_session_steps
            .push_back(EnsureSessionStep {
                result,
                hook: Some(Box::new(hook)),
            });
    }

    pub fn push_helper_step(&self, cmd: HelperCommand, step: HelperStep) {
        self.inner
            .lock()
            .unwrap()
            .helper_steps
            .entry(cmd)
            .or_default()
            .push_back(step);
    }

    pub fn push_helper_lines(
        &self,
        cmd: HelperCommand,
        lines: Vec<String>,
        result: Result<i32, SessionError>,
    ) {
        self.push_helper_step(
            cmd,
            HelperStep {
                lines,
                result,
                hook_after_line: None,
            },
        );
    }

    pub fn push_helper_fixture(&self, cmd: HelperCommand, rel: &str, exit_code: i32) {
        let lines = fixture_to_helper_lines(&read_fixture(rel));
        self.push_helper_lines(cmd, lines, Ok(exit_code));
    }

    pub fn push_local_step(&self, program: &str, required_args: &[&str], step: LocalStep) {
        self.inner.lock().unwrap().local_steps.push_back(LocalMatcher {
            program: program.into(),
            required_args: required_args.iter().map(|s| (*s).to_string()).collect(),
            step,
        });
    }

    pub fn push_local_lines(
        &self,
        program: &str,
        required_args: &[&str],
        lines: Vec<String>,
        result: Result<i32, String>,
    ) {
        self.push_local_step(
            program,
            required_args,
            LocalStep {
                lines,
                result,
                hook_after_line: None,
            },
        );
    }

    pub fn push_local_fixture(
        &self,
        program: &str,
        required_args: &[&str],
        rel: &str,
        exit_code: i32,
    ) {
        let lines = fixture_to_local_lines(&read_fixture(rel));
        self.push_local_lines(program, required_args, lines, Ok(exit_code));
    }

    /// Enqueue empty OK appstream sync commands for both `--user` and `--system`.
    pub fn push_flatpak_appstream_ok(&self) {
        self.push_local_lines(
            "flatpak",
            &["update", "--user", "--appstream", "--noninteractive"],
            Vec::new(),
            Ok(0),
        );
        self.push_local_lines(
            "flatpak",
            &["update", "--system", "--appstream", "--noninteractive"],
            Vec::new(),
            Ok(0),
        );
    }

    pub fn push_flatpak_query(&self, user: bool, result: Result<FlatpakCheck, String>) {
        let mut s = self.inner.lock().unwrap();
        if user {
            s.flatpak_user_steps.push_back(result);
        } else {
            s.flatpak_system_steps.push_back(result);
        }
    }

    pub fn push_flatpak_empty(&self, user: bool) {
        self.push_flatpak_query(
            user,
            Ok(FlatpakCheck {
                packages: Vec::new(),
                warning: None,
            }),
        );
    }

    pub fn ensure_session_calls(&self) -> usize {
        self.inner.lock().unwrap().ensure_session_calls
    }

    pub fn helper_calls(&self) -> Vec<HelperCommand> {
        self.inner.lock().unwrap().helper_calls.clone()
    }

    pub fn local_calls(&self) -> Vec<(String, Vec<String>)> {
        self.inner.lock().unwrap().local_calls.clone()
    }

    pub fn local_env_calls(&self) -> Vec<LocalEnvCall> {
        self.inner.lock().unwrap().local_env_calls.clone()
    }

    pub fn set_brew_available(&self, available: bool) {
        self.set_brew_binary(available.then(|| PathBuf::from(LINUXBREW_SYSTEM_PATH)));
    }

    pub fn set_brew_binary(&self, path: Option<PathBuf>) {
        self.inner.lock().unwrap().brew_binary = path;
    }

    pub fn set_rustup_binary(&self, path: Option<PathBuf>) {
        self.inner.lock().unwrap().rustup_binary = path;
    }

    pub fn set_mise_binary(&self, path: Option<PathBuf>) {
        self.inner.lock().unwrap().mise_binary = path;
    }

    pub fn set_uv_binary(&self, path: Option<PathBuf>) {
        self.inner.lock().unwrap().uv_binary = path;
    }

    pub fn set_pipx_binary(&self, path: Option<PathBuf>) {
        self.inner.lock().unwrap().pipx_binary = path;
    }

    pub fn set_distrobox_available(&self, available: bool) {
        self.set_distrobox_binary(available.then(|| PathBuf::from(DISTROBOX_DEFAULT_PATH)));
    }

    pub fn set_distrobox_binary(&self, path: Option<PathBuf>) {
        self.inner.lock().unwrap().distrobox_binary = path;
    }

    pub fn set_toolbox_available(&self, available: bool) {
        self.set_toolbox_binary(available.then(|| PathBuf::from(TOOLBOX_DEFAULT_PATH)));
    }

    pub fn set_toolbox_binary(&self, path: Option<PathBuf>) {
        self.inner.lock().unwrap().toolbox_binary = path;
    }

    fn execute_local_step(
        &self,
        program: &str,
        args: &[&str],
        envs: Option<&[(&str, &str)]>,
        cancel: Option<&WorkCancel>,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        if cancel.is_some_and(WorkCancel::is_cancelled) {
            return Ok(-1);
        }
        let step = {
            let mut s = self.inner.lock().unwrap();
            let owned_args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            s.local_calls.push((program.into(), owned_args.clone()));
            if let Some(env_pairs) = envs {
                s.local_env_calls.push((
                    program.into(),
                    owned_args,
                    env_pairs
                        .iter()
                        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                        .collect(),
                ));
            }
            let pos = s
                .local_steps
                .iter()
                .position(|m| m.matches(program, args));
            pos.and_then(|idx| s.local_steps.remove(idx)).map(|m| m.step)
        };
        let Some(mut step) = step else {
            panic!("FakeRuntime: unscripted local command `{program}` args={args:?}");
        };
        let mut hook = step.hook_after_line.take();
        for (idx, line) in step.lines.into_iter().enumerate() {
            if cancel.is_some_and(WorkCancel::is_cancelled) {
                return Ok(-1);
            }
            on_line(line);
            if let Some((target_idx, ref mut f)) = hook {
                if idx == target_idx {
                    f();
                }
            }
        }
        if cancel.is_some_and(WorkCancel::is_cancelled) {
            return Ok(-1);
        }
        step.result
    }

    pub fn flatpak_queries(&self) -> Vec<bool> {
        self.inner.lock().unwrap().flatpak_queries.clone()
    }

    pub fn push_flatpak_native_apply(&self, step: FlatpakNativeApplyStep) {
        self.inner
            .lock()
            .unwrap()
            .flatpak_native_apply_steps
            .push_back(step);
    }

    pub fn flatpak_native_apply_calls(&self) -> Vec<(bool, Vec<String>)> {
        self.inner.lock().unwrap().flatpak_native_apply_calls.clone()
    }

    pub fn set_dnf_dbus_available(&self, available: bool) {
        self.inner.lock().unwrap().dnf_dbus_available = available;
    }

    pub fn push_dnf_dbus_check(&self, outcome: NativeOutcome<Vec<Package>>) {
        self.inner
            .lock()
            .unwrap()
            .dnf_dbus_check_steps
            .push_back(outcome);
    }

    pub fn push_dnf_dbus_apply(&self, step: DnfDbusApplyStep) {
        self.inner
            .lock()
            .unwrap()
            .dnf_dbus_apply_steps
            .push_back(step);
    }

    pub fn push_dnf_history(&self, outcome: NativeOutcome<Vec<DnfHistoryTransaction>>) {
        self.inner
            .lock()
            .unwrap()
            .dnf_history_steps
            .push_back(outcome);
    }

    pub fn push_fwupd_history(&self, outcome: NativeOutcome<Vec<FwupdHistoryEntry>>) {
        self.inner
            .lock()
            .unwrap()
            .fwupd_history_steps
            .push_back(outcome);
    }

    pub fn dnf_dbus_check_calls(&self) -> usize {
        self.inner.lock().unwrap().dnf_dbus_check_calls
    }

    pub fn dnf_dbus_apply_calls(&self) -> Vec<(Vec<String>, ApplyMode)> {
        self.inner.lock().unwrap().dnf_dbus_apply_calls.clone()
    }

    pub fn set_fwupd_dbus_available(&self, available: bool) {
        self.inner.lock().unwrap().fwupd_dbus_available = available;
    }

    pub fn push_fwupd_dbus_check(&self, outcome: NativeOutcome<Vec<Package>>) {
        self.inner
            .lock()
            .unwrap()
            .fwupd_dbus_check_steps
            .push_back(outcome);
    }

    pub fn push_fwupd_dbus_apply(&self, step: FwupdDbusApplyStep) {
        self.inner
            .lock()
            .unwrap()
            .fwupd_dbus_apply_steps
            .push_back(step);
    }

    pub fn push_fwupd_dbus_reboot_needed(&self, outcome: NativeOutcome<bool>) {
        self.inner
            .lock()
            .unwrap()
            .fwupd_dbus_reboot_steps
            .push_back(outcome);
    }

    pub fn push_logind_preflight(&self, outcome: NativeOutcome<RebootPreflight>) {
        self.inner
            .lock()
            .unwrap()
            .logind_preflight_steps
            .push_back(outcome);
    }

    pub fn push_logind_reboot(&self, lines: Vec<String>, outcome: NativeOutcome<()>) {
        self.inner
            .lock()
            .unwrap()
            .logind_reboot_steps
            .push_back((lines, outcome));
    }

    pub fn fwupd_dbus_check_calls(&self) -> usize {
        self.inner.lock().unwrap().fwupd_dbus_check_calls
    }

    pub fn fwupd_dbus_apply_calls(&self) -> usize {
        self.inner.lock().unwrap().fwupd_dbus_apply_calls
    }

    pub fn fwupd_dbus_reboot_calls(&self) -> usize {
        self.inner.lock().unwrap().fwupd_dbus_reboot_calls
    }

    pub fn logind_reboot_calls(&self) -> usize {
        self.inner.lock().unwrap().logind_reboot_calls
    }

    pub fn set_gnome_extensions_available(&self, available: bool) {
        self.inner.lock().unwrap().gnome_extensions_available = available;
    }

    pub fn push_gnome_extensions_check(&self, outcome: NativeOutcome<Vec<Package>>) {
        self.inner
            .lock()
            .unwrap()
            .gnome_extensions_check_steps
            .push_back(outcome);
    }

    pub fn push_gnome_extensions_apply(&self, step: GnomeExtApplyStep) {
        self.inner
            .lock()
            .unwrap()
            .gnome_extensions_apply_steps
            .push_back(step);
    }

    pub fn gnome_extensions_check_calls(&self) -> usize {
        self.inner.lock().unwrap().gnome_extensions_check_calls
    }

    pub fn gnome_extensions_apply_calls(&self) -> Vec<Vec<String>> {
        self.inner
            .lock()
            .unwrap()
            .gnome_extensions_apply_calls
            .clone()
    }

    pub fn set_ostree_booted(&self, ostree: bool) {
        self.inner.lock().unwrap().ostree_booted = ostree;
    }

    pub fn set_rpm_ostree_binary(&self, path: Option<PathBuf>) {
        self.inner.lock().unwrap().rpm_ostree_binary = path;
    }

    pub fn push_rpm_ostree_status(&self, outcome: NativeOutcome<OstreeStatus>) {
        self.inner
            .lock()
            .unwrap()
            .rpm_ostree_status_steps
            .push_back(outcome);
    }

    pub fn cancel_rpm_ostree_calls(&self) -> usize {
        self.inner.lock().unwrap().cancel_rpm_ostree_calls
    }
}

impl OrchestratorRuntime for FakeRuntime {
    fn ensure_session(&self) -> Result<(), SessionError> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.ensure_session_calls += 1;
            s.ensure_session_steps.pop_front()
        };
        match step {
            Some(mut st) => {
                if let Some(mut hook) = st.hook.take() {
                    hook();
                }
                st.result
            }
            None => Ok(()),
        }
    }

    fn run_helper(
        &self,
        cmd: HelperCommand,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, SessionError> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.helper_calls.push(cmd);
            s.helper_steps
                .get_mut(&cmd)
                .and_then(VecDeque::pop_front)
        };
        let Some(mut step) = step else {
            panic!("FakeRuntime: unscripted helper command {cmd:?}");
        };
        let mut hook = step.hook_after_line.take();
        for (idx, line) in step.lines.into_iter().enumerate() {
            on_line(line);
            if let Some((target_idx, ref mut f)) = hook {
                if idx == target_idx {
                    f();
                }
            }
        }
        step.result
    }

    fn run_local(
        &self,
        program: &str,
        args: &[&str],
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        self.execute_local_step(program, args, None, None, on_line)
    }

    fn run_cancellable_local_env(
        &self,
        program: &str,
        args: &[&str],
        envs: &[(&str, &str)],
        cancel: Option<&WorkCancel>,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        self.execute_local_step(program, args, Some(envs), cancel, on_line)
    }

    fn is_ostree_booted(&self) -> bool {
        self.inner.lock().unwrap().ostree_booted
    }

    fn rpm_ostree_binary(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().rpm_ostree_binary.clone()
    }

    fn query_rpm_ostree_status(&self) -> NativeOutcome<OstreeStatus> {
        self.inner
            .lock()
            .unwrap()
            .rpm_ostree_status_steps
            .pop_front()
            .unwrap_or_else(|| NativeOutcome::Unavailable("rpm-ostree status not scripted".into()))
    }

    fn cancel_rpm_ostree(&self) -> bool {
        let mut s = self.inner.lock().unwrap();
        s.cancel_rpm_ostree_calls += 1;
        true
    }

    fn brew_binary(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().brew_binary.clone()
    }

    fn rustup_binary(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().rustup_binary.clone()
    }

    fn mise_binary(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().mise_binary.clone()
    }

    fn uv_binary(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().uv_binary.clone()
    }

    fn pipx_binary(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().pipx_binary.clone()
    }

    fn distrobox_binary(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().distrobox_binary.clone()
    }

    fn toolbox_binary(&self) -> Option<PathBuf> {
        self.inner.lock().unwrap().toolbox_binary.clone()
    }

    fn query_flatpak(&self, user: bool) -> Result<FlatpakCheck, String> {
        let res = {
            let mut s = self.inner.lock().unwrap();
            s.flatpak_queries.push(user);
            if user {
                s.flatpak_user_steps.pop_front()
            } else {
                s.flatpak_system_steps.pop_front()
            }
        };
        res.unwrap_or_else(|| {
            panic!("FakeRuntime: unscripted flatpak query user={user}")
        })
    }

    fn apply_flatpak_native(
        &self,
        user: bool,
        refs: &[String],
        cancel: &WorkCancel,
        on_line: &mut dyn FnMut(String),
        on_draw: &mut dyn FnMut(String),
        on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<FlatpakApplyOutcome> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.flatpak_native_apply_calls.push((user, refs.to_vec()));
            s.flatpak_native_apply_steps.pop_front()
        };
        let Some(mut step) = step else {
            return NativeOutcome::Unavailable("native FlatpakTransaction not scripted".into());
        };
        if let Some(mut on_start) = step.on_start.take() {
            on_start(cancel);
        }
        for l in step.lines {
            on_line(l);
        }
        for d in step.draws {
            on_draw(d);
        }
        for h in step.hints {
            on_progress(h);
        }
        step.outcome
    }

    fn dnf_dbus_available(&self) -> bool {
        self.inner.lock().unwrap().dnf_dbus_available
    }

    fn check_dnf_dbus(&self, _on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        let mut s = self.inner.lock().unwrap();
        s.dnf_dbus_check_calls += 1;
        s.dnf_dbus_check_steps
            .pop_front()
            .unwrap_or_else(|| NativeOutcome::Unavailable("dnf5daemon check not scripted".into()))
    }

    fn apply_dnf_dbus(
        &self,
        packages: &[Package],
        mode: ApplyMode,
        cancel: &WorkCancel,
        on_line: &mut dyn FnMut(String),
        on_draw: &mut dyn FnMut(String),
        on_progress: &mut dyn FnMut(ProgressHint),
        on_throughput: &mut dyn FnMut(ThroughputSample),
    ) -> NativeOutcome<DnfApplyOutcome> {
        let specs = upgrade_specs_from_packages(packages);
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.dnf_dbus_apply_calls.push((specs, mode));
            s.dnf_dbus_apply_steps.pop_front()
        };
        let Some(mut step) = step else {
            return NativeOutcome::Unavailable("dnf5daemon apply not scripted".into());
        };
        if let Some(mut on_start) = step.on_start.take() {
            on_start(cancel);
        }
        for l in step.lines {
            on_line(l);
        }
        for d in step.draws {
            on_draw(d);
        }
        for h in step.hints {
            on_progress(h);
        }
        for tp in step.throughput {
            on_throughput(tp);
        }
        step.outcome
    }

    fn query_dnf_history(&self, _limit: i64) -> NativeOutcome<Vec<DnfHistoryTransaction>> {
        self.inner
            .lock()
            .unwrap()
            .dnf_history_steps
            .pop_front()
            .unwrap_or_else(|| NativeOutcome::Unavailable("dnf5daemon history not scripted".into()))
    }

    fn query_fwupd_history(&self) -> NativeOutcome<Vec<FwupdHistoryEntry>> {
        self.inner
            .lock()
            .unwrap()
            .fwupd_history_steps
            .pop_front()
            .unwrap_or_else(|| NativeOutcome::Unavailable("fwupd history not scripted".into()))
    }

    fn fwupd_dbus_available(&self) -> bool {
        self.inner.lock().unwrap().fwupd_dbus_available
    }

    fn check_fwupd_dbus(&self, _on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        let mut s = self.inner.lock().unwrap();
        s.fwupd_dbus_check_calls += 1;
        s.fwupd_dbus_check_steps
            .pop_front()
            .unwrap_or_else(|| NativeOutcome::Unavailable("fwupd D-Bus not scripted".into()))
    }

    fn apply_fwupd_dbus(
        &self,
        _packages: &[Package],
        on_line: &mut dyn FnMut(String),
        on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<FwupdApplyOutcome> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.fwupd_dbus_apply_calls += 1;
            s.fwupd_dbus_apply_steps.pop_front()
        };
        let Some(step) = step else {
            return NativeOutcome::Unavailable("fwupd D-Bus apply not scripted".into());
        };
        for l in step.lines {
            on_line(l);
        }
        for h in step.hints {
            on_progress(h);
        }
        step.outcome
    }

    fn check_fwupd_reboot_dbus(&self) -> NativeOutcome<bool> {
        let mut s = self.inner.lock().unwrap();
        s.fwupd_dbus_reboot_calls += 1;
        s.fwupd_dbus_reboot_steps
            .pop_front()
            .unwrap_or_else(|| NativeOutcome::Unavailable("fwupd D-Bus reboot check not scripted".into()))
    }

    fn gnome_extensions_available(&self) -> bool {
        self.inner.lock().unwrap().gnome_extensions_available
    }

    fn check_gnome_extensions(&self, on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.gnome_extensions_check_calls += 1;
            s.gnome_extensions_check_steps.pop_front()
        };
        let Some(outcome) = step else {
            return NativeOutcome::Unavailable(
                "GNOME Shell Extensions D-Bus unavailable in test runtime".into(),
            );
        };
        if matches!(outcome, NativeOutcome::Handled(_)) {
            on_line("Checking GNOME Shell extensions for updates…".into());
        }
        outcome
    }

    fn apply_gnome_extensions(
        &self,
        uuids: &[String],
        cancel: &WorkCancel,
        on_line: &mut dyn FnMut(String),
        on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<GnomeExtensionsApplyOutcome> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.gnome_extensions_apply_calls.push(uuids.to_vec());
            s.gnome_extensions_apply_steps.pop_front()
        };
        let Some(mut step) = step else {
            return NativeOutcome::Unavailable(
                "GNOME Shell Extensions D-Bus apply not scripted".into(),
            );
        };
        if let Some(mut on_start) = step.on_start.take() {
            on_start(cancel);
        }
        if let Some(mut handler) = step.install_handler.take() {
            let outcome =
                apply_extensions_with(uuids, cancel, on_line, on_progress, |uuid| {
                    handler(uuid, cancel)
                });
            return NativeOutcome::Handled(Ok(outcome));
        }
        for l in step.lines {
            on_line(l);
        }
        for h in step.hints {
            on_progress(h);
        }
        step.outcome
    }

    fn reboot_preflight(&self) -> NativeOutcome<RebootPreflight> {
        let mut s = self.inner.lock().unwrap();
        s.logind_preflight_calls += 1;
        s.logind_preflight_steps
            .pop_front()
            .unwrap_or_else(|| NativeOutcome::Unavailable("logind preflight not scripted".into()))
    }

    fn reboot_via_logind(&self, on_line: &mut dyn FnMut(String)) -> NativeOutcome<()> {
        let step = {
            let mut s = self.inner.lock().unwrap();
            s.logind_reboot_calls += 1;
            s.logind_reboot_steps.pop_front()
        };
        let Some((lines, outcome)) = step else {
            return NativeOutcome::Unavailable("logind D-Bus not scripted".into());
        };
        for l in lines {
            on_line(l);
        }
        outcome
    }

    fn now(&self) -> SystemTime {
        self.inner.lock().unwrap().now
    }
}
