//! Sequential multi-backend check/apply using one privileged helper session.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;

use async_channel::Sender;

use crate::backend::{
    self, brew, dnf, dnf_dbus, flatpak, fwupd, fwupd_dbus, gnome_extensions, logind, ProgressHint,
};
pub use crate::backend::NativeOutcome;
use crate::helper_protocol::{split_helper_line, HelperCommand, HelperOutput};
use crate::model::{ApplyMode, AuthPurpose, Package, RetryAction, UpdateSource};
use crate::privilege::{
    close_session, kill_recorded_local_command, run_cancellable_local_command,
    run_cancellable_local_command_env, run_local_command, session_is_alive, with_session,
    SessionError, SessionEvent,
};
use crate::state::Event as StateEvent;

/// Enabled/disabled source filter applied during check and apply pipelines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceConfig {
    pub disabled_keys: BTreeSet<String>,
}

impl SourceConfig {
    pub fn all_enabled() -> Self {
        Self::default()
    }

    pub fn from_disabled_keys(keys: impl IntoIterator<Item = String>) -> Self {
        let mut disabled_keys = BTreeSet::new();
        for key in keys {
            let trimmed = key.trim().to_ascii_lowercase();
            if !trimmed.is_empty() {
                disabled_keys.insert(trimmed);
            }
        }
        Self { disabled_keys }
    }

    pub fn is_key_enabled(&self, pref_key: &str) -> bool {
        let normalized = pref_key.trim().to_ascii_lowercase();
        !self.disabled_keys.contains(&normalized)
    }

    pub fn is_enabled(&self, source: UpdateSource) -> bool {
        self.is_key_enabled(source.pref_key())
    }

    pub fn enabled_sources(&self) -> Vec<UpdateSource> {
        UpdateSource::ALL
            .iter()
            .copied()
            .filter(|s| self.is_enabled(*s))
            .collect()
    }

    pub fn all_disabled(&self) -> bool {
        UpdateSource::ALL.iter().all(|s| !self.is_enabled(*s))
    }

    pub fn disable(&mut self, pref_key: &str) {
        let normalized = pref_key.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            self.disabled_keys.insert(normalized);
        }
    }

    pub fn enable(&mut self, pref_key: &str) {
        let normalized = pref_key.trim().to_ascii_lowercase();
        self.disabled_keys.remove(&normalized);
    }
}

/// Detected host availability for a preference-level update source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAvailability {
    pub pref_key: &'static str,
    pub installed: bool,
    pub subtitle: String,
}

fn command_exists(program: &str) -> bool {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.join(program).is_file() {
                return true;
            }
        }
    }
    Path::new("/usr/bin").join(program).is_file()
}

/// Probe host availability of supported update sources without spawning processes.
pub fn probe_source_availability(
    native_dnf: bool,
    native_fwupd: bool,
    gnome_ext_available: bool,
) -> BTreeMap<String, SourceAvailability> {
    let brew_bin = brew::find_brew_binary();
    probe_source_availability_with(
        native_dnf,
        native_fwupd,
        brew_bin.as_deref(),
        gnome_ext_available,
    )
}

/// Injectable variant of [`probe_source_availability`] for deterministic offline tests.
pub fn probe_source_availability_with(
    native_dnf: bool,
    native_fwupd: bool,
    brew_bin: Option<&Path>,
    gnome_ext_available: bool,
) -> BTreeMap<String, SourceAvailability> {
    let mut map = BTreeMap::new();
    let dnf_cli = command_exists("dnf");
    let dnf_installed = native_dnf || dnf_cli;
    let dnf_sub = if native_dnf {
        "Installed (dnf5daemon D-Bus)"
    } else if dnf_cli {
        "Installed (CLI)"
    } else {
        "Not installed"
    };
    map.insert(
        "dnf".into(),
        SourceAvailability {
            pref_key: "dnf",
            installed: dnf_installed,
            subtitle: dnf_sub.into(),
        },
    );

    let fp_installed = command_exists("flatpak");
    map.insert(
        "flatpak".into(),
        SourceAvailability {
            pref_key: "flatpak",
            installed: fp_installed,
            subtitle: if fp_installed {
                "Installed"
            } else {
                "Not installed"
            }
            .into(),
        },
    );

    let brew_installed = brew_bin.is_some();
    let brew_sub = match brew_bin {
        Some(path) => format!("Installed ({})", path.display()),
        None => "Not installed".into(),
    };
    map.insert(
        "gnome-extensions".into(),
        SourceAvailability {
            pref_key: "gnome-extensions",
            installed: gnome_ext_available,
            subtitle: if gnome_ext_available {
                "Available (session D-Bus)".into()
            } else {
                "Not available (requires GNOME Shell)".into()
            },
        },
    );

    map.insert(
        "homebrew".into(),
        SourceAvailability {
            pref_key: "homebrew",
            installed: brew_installed,
            subtitle: brew_sub,
        },
    );

    let fw_cli = command_exists("fwupdmgr");
    let fw_installed = native_fwupd || fw_cli;
    let fw_sub = if native_fwupd {
        "Installed (fwupd D-Bus)"
    } else if fw_cli {
        "Installed (CLI)"
    } else {
        "Not installed"
    };
    map.insert(
        "firmware".into(),
        SourceAvailability {
            pref_key: "firmware",
            installed: fw_installed,
            subtitle: fw_sub.into(),
        },
    );

    map
}

/// Optimistic default availability used before live host probing or in offline tests.
pub fn default_source_availability() -> BTreeMap<String, SourceAvailability> {
    let mut map = BTreeMap::new();
    for src in UpdateSource::pref_sources() {
        let key = src.pref_key();
        let installed = src.installed_by_default();
        map.insert(
            key.into(),
            SourceAvailability {
                pref_key: key,
                installed,
                subtitle: if installed {
                    "Installed".into()
                } else {
                    "Not installed".into()
                },
            },
        );
    }
    map
}

#[derive(Debug, Clone)]
pub enum WorkerMsg {
    SessionStarted,
    ConsoleLine { tag: String, text: String },
    ConsoleDraw { tag: String, text: String },
    Throughput {
        bytes_per_sec: f64,
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Event(StateEvent),
}

/// Incremented when a Check/Apply starts or the user cancels auth.
/// In-flight workers compare against the generation they captured at spawn.
static WORK_GEN: AtomicU64 = AtomicU64::new(0);
static ACTIVE_GLOBAL_CANCEL: Mutex<Option<WorkCancel>> = Mutex::new(None);

fn begin_background_work() -> u64 {
    let gen = WORK_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let cancel = WorkCancel::new();
    let prev = {
        let mut guard = ACTIVE_GLOBAL_CANCEL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.replace(cancel)
    };
    if let Some(prev_cancel) = prev {
        prev_cancel.cancel();
    }
    gen
}

fn work_is_current(gen: u64) -> bool {
    WORK_GEN.load(Ordering::SeqCst) == gen
}

/// Invalidate in-flight Check/Apply after the user cancels the auth wait or clicks Stop.
pub fn cancel_background_work() {
    WORK_GEN.fetch_add(1, Ordering::SeqCst);
    let prev = {
        let mut guard = ACTIVE_GLOBAL_CANCEL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    if let Some(prev_cancel) = prev {
        prev_cancel.cancel();
    }
    flatpak::cancel_active_transaction();
    dnf_dbus::cancel_active_transaction();
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

type CancelHooks = Vec<Box<dyn FnOnce() + Send>>;

/// Per-run cancellation handle for injected [`OrchestratorRuntime`] workers
/// and native `libflatpak` `GCancellable` transactions.
#[derive(Clone, Default)]
pub struct WorkCancel {
    cancelled: Arc<AtomicBool>,
    hooks: Arc<Mutex<CancelHooks>>,
}

impl std::fmt::Debug for WorkCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkCancel")
            .field("cancelled", &self.cancelled.load(Ordering::SeqCst))
            .finish()
    }
}

impl WorkCancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let hooks = {
            let mut guard = self.hooks.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *guard)
        };
        for hook in hooks {
            hook();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Register a one-shot hook executed immediately when this token is cancelled.
    pub fn on_cancel(&self, hook: impl FnOnce() + Send + 'static) {
        if self.is_cancelled() {
            hook();
            return;
        }
        let mut guard = self.hooks.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_cancelled() {
            drop(guard);
            hook();
        } else {
            guard.push(Box::new(hook));
        }
    }
}

#[derive(Clone)]
enum CancelCheck {
    GlobalGen(u64),
    Token(WorkCancel),
}

impl CancelCheck {
    fn is_current(&self) -> bool {
        match self {
            Self::GlobalGen(gen) => work_is_current(*gen),
            Self::Token(token) => !token.is_cancelled(),
        }
    }

    fn work_cancel(&self) -> WorkCancel {
        match self {
            Self::Token(token) => token.clone(),
            Self::GlobalGen(_) => ACTIVE_GLOBAL_CANCEL
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_default(),
        }
    }
}

/// Host I/O abstraction used by the check, apply, and reboot pipelines.
pub trait OrchestratorRuntime: Send + 'static {
    fn ensure_session(&self) -> Result<(), SessionError>;
    fn run_helper(
        &self,
        cmd: HelperCommand,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, SessionError>;
    fn run_local(
        &self,
        program: &str,
        args: &[&str],
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String>;
    fn run_cancellable_local(
        &self,
        program: &str,
        args: &[&str],
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        self.run_local(program, args, on_line)
    }
    fn run_cancellable_local_env(
        &self,
        program: &str,
        args: &[&str],
        _envs: &[(&str, &str)],
        _cancel: Option<&WorkCancel>,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        self.run_cancellable_local(program, args, on_line)
    }
    fn brew_binary(&self) -> Option<PathBuf> {
        None
    }
    fn brew_available(&self) -> bool {
        self.brew_binary().is_some()
    }
    fn gnome_extensions_available(&self) -> bool {
        false
    }
    fn check_gnome_extensions(&self, _on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        NativeOutcome::Unavailable(
            "GNOME Shell Extensions D-Bus unavailable in test runtime".into(),
        )
    }
    fn apply_gnome_extensions(
        &self,
        _uuids: &[String],
        _cancel: &WorkCancel,
        _on_line: &mut dyn FnMut(String),
        _on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<gnome_extensions::GnomeExtensionsApplyOutcome> {
        NativeOutcome::Unavailable(
            "GNOME Shell Extensions D-Bus unavailable in test runtime".into(),
        )
    }
    fn query_flatpak(&self, user: bool) -> Result<flatpak::FlatpakCheck, String>;
    fn apply_flatpak_native(
        &self,
        _user: bool,
        _refs: &[String],
        _cancel: &WorkCancel,
        _on_line: &mut dyn FnMut(String),
        _on_draw: &mut dyn FnMut(String),
        _on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<flatpak::FlatpakApplyOutcome> {
        NativeOutcome::Unavailable("libflatpak transaction apply unavailable in test runtime".into())
    }
    fn dnf_dbus_available(&self) -> bool {
        false
    }
    fn check_dnf_dbus(&self, _on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        NativeOutcome::Unavailable("dnf5daemon D-Bus unavailable in test runtime".into())
    }
    #[allow(clippy::too_many_arguments)]
    fn apply_dnf_dbus(
        &self,
        _packages: &[Package],
        _mode: ApplyMode,
        _cancel: &WorkCancel,
        _on_line: &mut dyn FnMut(String),
        _on_draw: &mut dyn FnMut(String),
        _on_progress: &mut dyn FnMut(ProgressHint),
        _on_throughput: &mut dyn FnMut(dnf_dbus::ThroughputSample),
    ) -> NativeOutcome<dnf_dbus::DnfApplyOutcome> {
        NativeOutcome::Unavailable("dnf5daemon D-Bus unavailable in test runtime".into())
    }
    fn query_dnf_history(&self, _limit: i64) -> NativeOutcome<Vec<dnf_dbus::DnfHistoryTransaction>> {
        NativeOutcome::Unavailable("dnf5daemon D-Bus unavailable in test runtime".into())
    }
    fn query_fwupd_history(&self) -> NativeOutcome<Vec<fwupd_dbus::FwupdHistoryEntry>> {
        NativeOutcome::Unavailable("fwupd D-Bus unavailable in test runtime".into())
    }
    fn fwupd_dbus_available(&self) -> bool {
        false
    }
    fn check_fwupd_dbus(&self, _on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        NativeOutcome::Unavailable("fwupd D-Bus unavailable in test runtime".into())
    }
    fn apply_fwupd_dbus(
        &self,
        _packages: &[Package],
        _on_line: &mut dyn FnMut(String),
        _on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<fwupd_dbus::FwupdApplyOutcome> {
        NativeOutcome::Unavailable("fwupd D-Bus unavailable in test runtime".into())
    }
    fn check_fwupd_reboot_dbus(&self) -> NativeOutcome<bool> {
        NativeOutcome::Unavailable("fwupd D-Bus unavailable in test runtime".into())
    }
    fn reboot_preflight(&self) -> NativeOutcome<logind::RebootPreflight> {
        NativeOutcome::Unavailable("logind D-Bus unavailable in test runtime".into())
    }
    fn reboot_via_logind(&self, _on_line: &mut dyn FnMut(String)) -> NativeOutcome<()> {
        NativeOutcome::Unavailable("logind D-Bus unavailable in test runtime".into())
    }
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Production [`OrchestratorRuntime`] backed by OS processes, polkit helper, D-Bus, and libflatpak.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRuntime;

impl OrchestratorRuntime for SystemRuntime {
    fn ensure_session(&self) -> Result<(), SessionError> {
        with_session(|_s| Ok(()))
    }

    fn run_helper(
        &self,
        cmd: HelperCommand,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, SessionError> {
        with_session(|session| {
            session.run_command(cmd, |ev| {
                if let SessionEvent::Line(l) = ev {
                    on_line(l);
                }
            })
        })
    }

    fn run_local(
        &self,
        program: &str,
        args: &[&str],
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        run_local_command(program, args, on_line).map_err(|e| e.to_string())
    }

    fn run_cancellable_local(
        &self,
        program: &str,
        args: &[&str],
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        run_cancellable_local_command(program, args, on_line).map_err(|e| e.to_string())
    }

    fn run_cancellable_local_env(
        &self,
        program: &str,
        args: &[&str],
        envs: &[(&str, &str)],
        cancel: Option<&WorkCancel>,
        on_line: &mut dyn FnMut(String),
    ) -> Result<i32, String> {
        run_cancellable_local_command_env(program, args, envs, cancel, on_line)
            .map_err(|e| e.to_string())
    }

    fn brew_binary(&self) -> Option<PathBuf> {
        brew::find_brew_binary()
    }

    fn gnome_extensions_available(&self) -> bool {
        gnome_extensions::is_available()
    }

    fn check_gnome_extensions(&self, on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        gnome_extensions::check_updates(on_line)
    }

    fn apply_gnome_extensions(
        &self,
        uuids: &[String],
        cancel: &WorkCancel,
        on_line: &mut dyn FnMut(String),
        on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<gnome_extensions::GnomeExtensionsApplyOutcome> {
        gnome_extensions::apply_gnome_extensions(uuids, cancel, on_line, on_progress)
    }

    fn query_flatpak(&self, user: bool) -> Result<flatpak::FlatpakCheck, String> {
        flatpak::query_updates(user)
    }

    fn apply_flatpak_native(
        &self,
        user: bool,
        refs: &[String],
        cancel: &WorkCancel,
        on_line: &mut dyn FnMut(String),
        on_draw: &mut dyn FnMut(String),
        on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<flatpak::FlatpakApplyOutcome> {
        flatpak::apply_updates(user, refs, cancel, on_line, on_draw, on_progress)
    }

    fn dnf_dbus_available(&self) -> bool {
        dnf_dbus::is_available()
    }

    fn check_dnf_dbus(&self, on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        dnf_dbus::check_updates(on_line)
    }

    fn apply_dnf_dbus(
        &self,
        packages: &[Package],
        mode: ApplyMode,
        cancel: &WorkCancel,
        on_line: &mut dyn FnMut(String),
        on_draw: &mut dyn FnMut(String),
        on_progress: &mut dyn FnMut(ProgressHint),
        on_throughput: &mut dyn FnMut(dnf_dbus::ThroughputSample),
    ) -> NativeOutcome<dnf_dbus::DnfApplyOutcome> {
        dnf_dbus::apply_updates(
            packages,
            mode,
            cancel,
            on_line,
            on_draw,
            on_progress,
            on_throughput,
        )
    }

    fn query_dnf_history(&self, limit: i64) -> NativeOutcome<Vec<dnf_dbus::DnfHistoryTransaction>> {
        dnf_dbus::get_history(limit)
    }

    fn query_fwupd_history(&self) -> NativeOutcome<Vec<fwupd_dbus::FwupdHistoryEntry>> {
        fwupd_dbus::get_history()
    }

    fn fwupd_dbus_available(&self) -> bool {
        fwupd_dbus::is_available()
    }

    fn check_fwupd_dbus(&self, on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
        fwupd_dbus::check_updates(on_line)
    }

    fn apply_fwupd_dbus(
        &self,
        packages: &[Package],
        on_line: &mut dyn FnMut(String),
        on_progress: &mut dyn FnMut(ProgressHint),
    ) -> NativeOutcome<fwupd_dbus::FwupdApplyOutcome> {
        fwupd_dbus::apply_updates(packages, on_line, on_progress)
    }

    fn check_fwupd_reboot_dbus(&self) -> NativeOutcome<bool> {
        fwupd_dbus::check_reboot_needed()
    }

    fn reboot_preflight(&self) -> NativeOutcome<logind::RebootPreflight> {
        logind::query_reboot_preflight()
    }

    fn reboot_via_logind(&self, on_line: &mut dyn FnMut(String)) -> NativeOutcome<()> {
        logind::request_reboot_interactive(on_line)
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Generation- or token-scoped sender so a cancelled worker stops emitting events.
struct WorkChannel<'a> {
    tx: &'a Sender<WorkerMsg>,
    cancel: CancelCheck,
}

impl<'a> WorkChannel<'a> {
    fn is_current(&self) -> bool {
        self.cancel.is_current()
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

fn map_session_start_err(ch: &WorkChannel, e: SessionError, retry: RetryAction) {
    match e {
        SessionError::AuthDenied => ch.event(StateEvent::Fail {
            title: "Authentication cancelled".into(),
            detail: "polkit did not grant permission for the update helper.".into(),
            retry,
        }),
        other => ch.event(StateEvent::Fail {
            title: "Could not start privileged helper".into(),
            detail: other.to_string(),
            retry,
        }),
    }
}

/// Full check pipeline. Reuses the shared helper session when already elevated
/// (no second password); otherwise starts it once via polkit.
pub fn run_check_all(tx: Sender<WorkerMsg>) {
    run_check_with_config(tx, SourceConfig::all_enabled());
}

/// Full check pipeline honoring disabled sources in `config`.
pub fn run_check_with_config(tx: Sender<WorkerMsg>, config: SourceConfig) {
    let gen = begin_background_work();
    thread::spawn(move || {
        execute_check_all_inner(&tx, &SystemRuntime, CancelCheck::GlobalGen(gen), &config);
    });
}

/// Spawn [`execute_check_all`] on a background thread using a custom [`OrchestratorRuntime`].
pub fn run_check_all_with<R: OrchestratorRuntime>(
    tx: Sender<WorkerMsg>,
    runtime: R,
) -> WorkCancel {
    run_check_all_with_config(tx, runtime, SourceConfig::all_enabled())
}

/// Spawn [`execute_check_all_with_config`] on a background thread using a custom [`OrchestratorRuntime`].
pub fn run_check_all_with_config<R: OrchestratorRuntime>(
    tx: Sender<WorkerMsg>,
    runtime: R,
    config: SourceConfig,
) -> WorkCancel {
    let cancel = WorkCancel::new();
    let worker_cancel = cancel.clone();
    thread::spawn(move || {
        execute_check_all_inner(&tx, &runtime, CancelCheck::Token(worker_cancel), &config);
    });
    cancel
}

/// Synchronously run the full check pipeline against `runtime`.
pub fn execute_check_all<R: OrchestratorRuntime>(
    tx: &Sender<WorkerMsg>,
    runtime: &R,
    cancel: &WorkCancel,
) {
    execute_check_all_with_config(tx, runtime, cancel, &SourceConfig::all_enabled());
}

/// Synchronously run the check pipeline against `runtime`, skipping sources disabled in `config`.
pub fn execute_check_all_with_config<R: OrchestratorRuntime>(
    tx: &Sender<WorkerMsg>,
    runtime: &R,
    cancel: &WorkCancel,
    config: &SourceConfig,
) {
    execute_check_all_inner(tx, runtime, CancelCheck::Token(cancel.clone()), config);
}

fn execute_check_all_inner<R: OrchestratorRuntime>(
    tx: &Sender<WorkerMsg>,
    runtime: &R,
    cancel: CancelCheck,
    config: &SourceConfig,
) {
    let ch = WorkChannel { tx, cancel };
    if config.all_disabled() {
        ch.event(StateEvent::Fail {
            title: "All update sources are disabled".into(),
            detail: "Enable at least one update source in Preferences (Ctrl+,) to check for updates.".into(),
            retry: RetryAction::CheckAgain,
        });
        return;
    }

    let dnf_enabled = config.is_enabled(UpdateSource::Dnf);
    // Use zero-password dnf5daemon SessionManager.open_session({}) when available;
    // only elevate the helper session up front when falling back to CLI DNF.
    let use_native_dnf = dnf_enabled && runtime.dnf_dbus_available();
    let mut session_ensured = false;
    if dnf_enabled && !use_native_dnf {
        let started = runtime.ensure_session();
        if !ch.is_current() {
            return;
        }
        if let Err(e) = started {
            map_session_start_err(&ch, e, RetryAction::CheckAgain);
            return;
        }
        session_ensured = true;
    }
    ch.send(WorkerMsg::SessionStarted);

    let mut all = Vec::new();
    let mut soft_errors = Vec::new();

    if !ch.is_current() {
        return;
    }

    // 1) DNF — prefer native org.rpm.dnf.v0 (zero-password), falling back to CLI helper.
    if dnf_enabled {
        let dnf_res = if use_native_dnf {
            match runtime.check_dnf_dbus(&mut |l| ch.line("dnf", l)) {
                NativeOutcome::Handled(res) => res,
                NativeOutcome::Unavailable(_) => {
                    if !session_ensured {
                        match runtime.ensure_session() {
                            Ok(()) => session_ensured = true,
                            Err(e) => return map_session_start_err(&ch, e, RetryAction::CheckAgain),
                        }
                    }
                    if !ch.is_current() {
                        return;
                    }
                    run_helper_check(&ch, runtime, HelperCommand::CheckDnf, "dnf", |log, code| {
                        let mut pkgs = dnf::parse_check_update(log);
                        if dnf::check_exit_is_hard_error(code, &pkgs) {
                            Err(format!("dnf check-update failed (exit {code})"))
                        } else {
                            attach_dnf_download_sizes(&ch, runtime, &mut pkgs);
                            Ok(pkgs)
                        }
                    })
                }
            }
        } else {
            run_helper_check(&ch, runtime, HelperCommand::CheckDnf, "dnf", |log, code| {
                let mut pkgs = dnf::parse_check_update(log);
                if dnf::check_exit_is_hard_error(code, &pkgs) {
                    Err(format!("dnf check-update failed (exit {code})"))
                } else {
                    attach_dnf_download_sizes(&ch, runtime, &mut pkgs);
                    Ok(pkgs)
                }
            })
        };
        if !ch.is_current() {
            return;
        }
        emit_backend_check(&ch, UpdateSource::Dnf, dnf_res, &mut all, &mut soft_errors);
    }

    // 2) Flatpak appstream (unprivileged). Names and versions come from this
    // metadata; the update set itself comes from libflatpak below.
    let fp_user_enabled = config.is_enabled(UpdateSource::FlatpakUser);
    let fp_sys_enabled = config.is_enabled(UpdateSource::FlatpakSystem);
    if fp_user_enabled {
        if !ch.is_current() {
            return;
        }
        let _ = runtime.run_cancellable_local(
            "flatpak",
            &["update", "--user", "--appstream", "--noninteractive"],
            &mut |l| ch.line("flatpak-user", l),
        );
    }
    if fp_sys_enabled {
        if !ch.is_current() {
            return;
        }
        let _ = runtime.run_cancellable_local(
            "flatpak",
            &["update", "--system", "--appstream", "--noninteractive"],
            &mut |l| ch.line("flatpak-system", l),
        );
    }

    // 3–4) Flatpak lists. Same decision as GNOME Software: a ref is included
    // when its commit changed or a related extension still needs installing.
    if fp_user_enabled {
        if !ch.is_current() {
            return;
        }
        let flatpak_user_res =
            run_flatpak_check(&ch, runtime, true).map_err(|e| format!("flatpak user: {e}"));
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
    }
    if fp_sys_enabled {
        if !ch.is_current() {
            return;
        }
        let flatpak_system_res =
            run_flatpak_check(&ch, runtime, false).map_err(|e| format!("flatpak system: {e}"));
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
    }

    // 4) GNOME Shell Extensions — unprivileged session D-Bus check when active and enabled.
    if config.is_enabled(UpdateSource::GnomeExtensions) && runtime.gnome_extensions_available() {
        if !ch.is_current() {
            return;
        }
        if let NativeOutcome::Handled(ext_res) =
            runtime.check_gnome_extensions(&mut |l| ch.line("gnome-ext", l))
        {
            if !ch.is_current() {
                return;
            }
            emit_backend_check(
                &ch,
                UpdateSource::GnomeExtensions,
                ext_res,
                &mut all,
                &mut soft_errors,
            );
        }
    }

    // 5) Homebrew — unprivileged user-space check when detected and enabled.
    if config.is_enabled(UpdateSource::Homebrew) {
        if let Some(brew_bin) = brew_program(runtime) {
            if !ch.is_current() {
                return;
            }
            let brew_res = run_brew_check(&ch, runtime, &brew_bin, &mut soft_errors);
            if !ch.is_current() {
                return;
            }
            emit_backend_check(
                &ch,
                UpdateSource::Homebrew,
                brew_res,
                &mut all,
                &mut soft_errors,
            );
        }
    }

    // 5) fwupd — prefer native org.freedesktop.fwupd D-Bus, falling back cleanly
    // to the Phase 1 privileged fwupdmgr CLI helper when unavailable.
    if config.is_enabled(UpdateSource::Firmware) {
        if !ch.is_current() {
            return;
        }
        let fwupd_res = match runtime.check_fwupd_dbus(&mut |l| ch.line("fwupd", l)) {
            NativeOutcome::Handled(res) => res,
            NativeOutcome::Unavailable(_) => {
                let ready = if session_ensured {
                    Ok(())
                } else {
                    match runtime.ensure_session() {
                        Ok(()) => {
                            session_ensured = true;
                            Ok(())
                        }
                        Err(e) => Err(e.to_string()),
                    }
                };
                match ready {
                    Ok(()) => {
                        if !ch.is_current() {
                            return;
                        }
                        let _ = run_helper_check(
                            &ch,
                            runtime,
                            HelperCommand::CheckFwupdRefresh,
                            "fwupd",
                            |_log, _code| Ok(Vec::new()),
                        );
                        if !ch.is_current() {
                            return;
                        }

                        run_helper_check(&ch, runtime, HelperCommand::CheckFwupd, "fwupd", |log, code| {
                            let pkgs = fwupd::parse_get_updates(log);
                            if code != 0 && pkgs.is_empty() {
                                let lower = log.to_ascii_lowercase();
                                if lower.contains("not found") || lower.contains("no such file") {
                                    return Err("fwupd not available".into());
                                }
                                if fwupd::check_output_is_no_updates(log, code) {
                                    return Ok(vec![]);
                                }
                                return Err(format!("fwupdmgr get-updates exit {code}"));
                            }
                            Ok(pkgs)
                        })
                    }
                    Err(err) => Err(err),
                }
            }
        };
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
    }
    let _ = session_ensured;

    if !ch.is_current() {
        return;
    }

    // Leave session open for Apply and later Checks (app-lifetime elevation).
    ch.event(StateEvent::CheckComplete {
        packages: all,
        soft_errors,
        listed_at: runtime.now(),
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

fn attach_dnf_download_sizes<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    pkgs: &mut [Package],
) {
    if pkgs.is_empty() || !ch.is_current() {
        return;
    }
    let mut log = String::new();
    match runtime.run_cancellable_local("dnf", dnf::REPOQUERY_UPGRADES_ARGS, &mut |l| {
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

fn run_helper_check<R, F>(
    ch: &WorkChannel,
    runtime: &R,
    cmd: HelperCommand,
    tag: &str,
    parse: F,
) -> Result<Vec<Package>, String>
where
    R: OrchestratorRuntime,
    F: Fn(&str, i32) -> Result<Vec<Package>, String>,
{
    let mut log = String::new();
    let code = runtime
        .run_helper(cmd, &mut |l| {
            accept_helper_line(ch, tag, &mut log, &l);
        })
        .map_err(|e| e.to_string())?;
    if !ch.is_current() {
        return Err("cancelled".into());
    }
    parse(&log, code)
}

fn run_flatpak_check<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    user: bool,
) -> Result<Vec<Package>, String> {
    let tag = if user {
        "flatpak-user"
    } else {
        "flatpak-system"
    };
    let scope = if user { "user" } else { "system" };
    let check = runtime.query_flatpak(user)?;
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

fn brew_program<R: OrchestratorRuntime>(runtime: &R) -> Option<String> {
    runtime
        .brew_binary()
        .map(|path| path.to_string_lossy().into_owned())
}

fn run_brew_check<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    brew_bin: &str,
    soft_errors: &mut Vec<String>,
) -> Result<Vec<Package>, String> {
    let work_cancel = ch.cancel.work_cancel();
    let mut refresh_log = String::new();
    let refresh_warning = match runtime.run_cancellable_local_env(
        brew_bin,
        &["update", "--quiet"],
        brew::CHECK_REFRESH_ENV,
        Some(&work_cancel),
        &mut |l| {
            if !l.trim().is_empty() {
                ch.line("brew", l.clone());
            }
            refresh_log.push_str(&l);
            refresh_log.push('\n');
        },
    ) {
        Ok(0) => None,
        Ok(code) => {
            let detail = summarize_apply_failure(UpdateSource::Homebrew, &refresh_log, code);
            ch.line("brew", format!("Tap refresh warning: {detail}"));
            Some(format!("Homebrew tap refresh failed (exit {code})"))
        }
        Err(err) => {
            if !ch.is_current() {
                return Err("cancelled".into());
            }
            ch.line("brew", format!("Tap refresh warning: {err}"));
            Some(format!("Homebrew tap refresh failed: {err}"))
        }
    };

    if !ch.is_current() {
        return Err("cancelled".into());
    }

    let mut json_log = String::new();
    let code = runtime
        .run_cancellable_local_env(
            brew_bin,
            &["outdated", "--json=v2"],
            brew::NONINTERACTIVE_NO_AUTO_UPDATE_ENV,
            Some(&work_cancel),
            &mut |l| {
                json_log.push_str(&l);
                json_log.push('\n');
            },
        )
        .map_err(|e| format!("homebrew: {e}"))?;

    if !ch.is_current() {
        return Err("cancelled".into());
    }

    if code != 0 {
        for line in json_log.lines().map(str::trim).filter(|l| !l.is_empty()) {
            ch.line("brew", line.to_string());
        }
        return Err(format!("homebrew: brew outdated exited with {code}"));
    }

    match brew::parse_outdated_json(&json_log) {
        Ok(pkgs) => {
            ch.line("brew", format!("{} update(s) (brew)", pkgs.len()));
            for pkg in &pkgs {
                let ver = match pkg.old_version.as_deref() {
                    Some(old) => format!("{old} → {}", pkg.version),
                    None => pkg.version.clone(),
                };
                ch.line("brew", format!("  {} ({ver})", pkg.name));
            }
            if let Some(warn) = refresh_warning {
                soft_errors.push(warn);
            }
            Ok(pkgs)
        }
        Err(err) => {
            for line in json_log
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .take(8)
            {
                ch.line("brew", line.to_string());
            }
            Err(format!("homebrew: {err}"))
        }
    }
}

/// Apply all pending sources sequentially (`ApplyMode::Live`). Reuses the
/// shared helper session when still open (no second password).
pub fn run_apply_all(tx: Sender<WorkerMsg>, packages: Vec<Package>) {
    run_apply_all_with_mode(tx, packages, ApplyMode::Live);
}

/// Apply selected packages using the specified execution mode.
pub fn run_apply_all_with_mode(tx: Sender<WorkerMsg>, packages: Vec<Package>, mode: ApplyMode) {
    run_apply_all_with_mode_and_config(tx, packages, mode, SourceConfig::all_enabled());
}

/// Apply selected packages using the specified execution mode and [`SourceConfig`].
pub fn run_apply_all_with_mode_and_config(
    tx: Sender<WorkerMsg>,
    packages: Vec<Package>,
    mode: ApplyMode,
    config: SourceConfig,
) {
    let gen = begin_background_work();
    thread::spawn(move || {
        execute_apply_all_inner(
            &tx,
            packages,
            mode,
            &SystemRuntime,
            CancelCheck::GlobalGen(gen),
            &config,
        );
    });
}

/// Spawn [`execute_apply_all`] on a background thread using a custom [`OrchestratorRuntime`].
pub fn run_apply_all_with<R: OrchestratorRuntime>(
    tx: Sender<WorkerMsg>,
    packages: Vec<Package>,
    runtime: R,
) -> WorkCancel {
    run_apply_all_with_runtime_mode(tx, packages, ApplyMode::Live, runtime)
}

/// Spawn [`execute_apply_all_with_mode`] on a background thread using a custom [`OrchestratorRuntime`].
pub fn run_apply_all_with_runtime_mode<R: OrchestratorRuntime>(
    tx: Sender<WorkerMsg>,
    packages: Vec<Package>,
    mode: ApplyMode,
    runtime: R,
) -> WorkCancel {
    let cancel = WorkCancel::new();
    let worker_cancel = cancel.clone();
    thread::spawn(move || {
        execute_apply_all_inner(
            &tx,
            packages,
            mode,
            &runtime,
            CancelCheck::Token(worker_cancel),
            &SourceConfig::all_enabled(),
        );
    });
    cancel
}

/// Synchronously run the full live apply pipeline against `runtime`.
pub fn execute_apply_all<R: OrchestratorRuntime>(
    tx: &Sender<WorkerMsg>,
    packages: Vec<Package>,
    runtime: &R,
    cancel: &WorkCancel,
) {
    execute_apply_all_with_mode(tx, packages, ApplyMode::Live, runtime, cancel);
}

/// Synchronously run the apply pipeline with explicit [`ApplyMode`] against `runtime`.
pub fn execute_apply_all_with_mode<R: OrchestratorRuntime>(
    tx: &Sender<WorkerMsg>,
    packages: Vec<Package>,
    mode: ApplyMode,
    runtime: &R,
    cancel: &WorkCancel,
) {
    execute_apply_all_with_mode_and_config(
        tx,
        packages,
        mode,
        runtime,
        cancel,
        &SourceConfig::all_enabled(),
    );
}

/// Synchronously run the apply pipeline filtering out any disabled sources in `config`.
pub fn execute_apply_all_with_mode_and_config<R: OrchestratorRuntime>(
    tx: &Sender<WorkerMsg>,
    packages: Vec<Package>,
    mode: ApplyMode,
    runtime: &R,
    cancel: &WorkCancel,
    config: &SourceConfig,
) {
    execute_apply_all_inner(
        tx,
        packages,
        mode,
        runtime,
        CancelCheck::Token(cancel.clone()),
        config,
    );
}

fn execute_apply_all_inner<R: OrchestratorRuntime>(
    tx: &Sender<WorkerMsg>,
    packages: Vec<Package>,
    mode: ApplyMode,
    runtime: &R,
    cancel: CancelCheck,
    config: &SourceConfig,
) {
    let ch = WorkChannel { tx, cancel };
    let packages: Vec<Package> = packages
        .into_iter()
        .filter(|p| config.is_enabled(p.source))
        .collect();
    let sources = crate::model::sources_present(&packages);
    if sources.is_empty() {
        ch.event(StateEvent::Fail {
            title: "Nothing to apply".into(),
            detail: "Package list was empty.".into(),
            retry: RetryAction::CheckAgain,
        });
        return;
    }

    // Native dnf5daemon / fwupd D-Bus handles its own polkit, and Flatpak runs as the user.
    // Only allocate our helper session up front when CLI DNF or CLI fwupd fallback is needed.
    let needs_dnf_helper = sources.contains(&UpdateSource::Dnf) && !runtime.dnf_dbus_available();
    let needs_fw_helper = mode != ApplyMode::DownloadOnly
        && sources.contains(&UpdateSource::Firmware)
        && !runtime.fwupd_dbus_available();
    let needs_session = needs_dnf_helper || needs_fw_helper;
    let mut session_ensured = false;

    if needs_session {
        if let Err(e) = runtime.ensure_session() {
            map_session_start_err(&ch, e, RetryAction::RetryApply);
            return;
        }
        session_ensured = true;
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
            phase_label: if mode == ApplyMode::DownloadOnly
                && matches!(source, UpdateSource::Dnf | UpdateSource::Homebrew)
            {
                "Downloading packages".into()
            } else {
                format!("Updating {}", source.label())
            },
            stage_current: None,
            stage_total: None,
        });

        let (code, log, native_reboot) = match source {
            UpdateSource::Dnf => {
                let dnf_pkgs: Vec<Package> = packages
                    .iter()
                    .filter(|p| p.source == UpdateSource::Dnf)
                    .cloned()
                    .collect();
                let work_cancel = ch.cancel.work_cancel();
                let mut native_log = String::new();
                let outcome = runtime.apply_dnf_dbus(
                    &dnf_pkgs,
                    mode,
                    &work_cancel,
                    &mut |l| {
                        native_log.push_str(&l);
                        native_log.push('\n');
                        ch.line(tag, l);
                    },
                    &mut |draw| ch.live_line(tag, draw),
                    &mut |hint| emit_progress_hint(&ch, source, hint),
                    &mut |tp| {
                        ch.send(WorkerMsg::Throughput {
                            bytes_per_sec: tp.bytes_per_sec,
                            downloaded_bytes: tp.downloaded_bytes,
                            total_bytes: tp.total_bytes,
                        });
                    },
                );
                match outcome {
                    NativeOutcome::Handled(Ok(res)) => {
                        if res.cancelled || !ch.is_current() {
                            return;
                        }
                        (0, native_log, res.needs_reboot)
                    }
                    NativeOutcome::Handled(Err(reason)) => {
                        log_native_apply_error(&ch, tag, &mut native_log, &reason);
                        (1, native_log, false)
                    }
                    NativeOutcome::Unavailable(_) => {
                        let ready = if session_ensured {
                            Ok(())
                        } else {
                            match runtime.ensure_session() {
                                Ok(()) => {
                                    session_ensured = true;
                                    Ok(())
                                }
                                Err(e) => Err(e),
                            }
                        };
                        match ready {
                            Ok(()) => {
                                let (c, l) =
                                    run_helper_apply(&ch, runtime, HelperCommand::ApplyDnf, tag, source);
                                (c, l, false)
                            }
                            Err(e) => {
                                ch.line(tag, format!("error: {e}"));
                                (-1, e.to_string(), false)
                            }
                        }
                    }
                }
            }
            UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => {
                if mode == ApplyMode::DownloadOnly {
                    ch.line(
                        tag,
                        format!(
                            "Skipping {} install during Download Only mode.",
                            source.label()
                        ),
                    );
                    (0, String::new(), false)
                } else {
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
                        (0, String::new(), false)
                    } else {
                        ch.line(
                            tag,
                            format!("Updating {} ref(s): {}", refs.len(), refs.join(", ")),
                        );
                        let work_cancel = ch.cancel.work_cancel();
                        let mut native_log = String::new();
                        let outcome = runtime.apply_flatpak_native(
                            user,
                            &refs,
                            &work_cancel,
                            &mut |l| {
                                native_log.push_str(&l);
                                native_log.push('\n');
                                ch.line(tag, l);
                            },
                            &mut |draw| ch.live_line(tag, draw),
                            &mut |hint| emit_progress_hint(&ch, source, hint),
                        );
                        match outcome {
                            NativeOutcome::Handled(Ok(_)) => (0, native_log, false),
                            NativeOutcome::Handled(Err(reason)) => {
                                log_native_apply_error(&ch, tag, &mut native_log, &reason);
                                (1, native_log, false)
                            }
                            NativeOutcome::Unavailable(_) => {
                                let args = flatpak::update_args_for_refs(user, &refs);
                                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                                let (c, l) =
                                    run_local_apply(&ch, runtime, "flatpak", &arg_refs, tag, source);
                                (c, l, false)
                            }
                        }
                    }
                }
            }
            UpdateSource::GnomeExtensions => {
                if mode == ApplyMode::DownloadOnly {
                    ch.line(
                        tag,
                        "Skipping GNOME Shell extension install during Download Only mode.".into(),
                    );
                    (0, String::new(), false)
                } else if mode == ApplyMode::OfflineOnReboot {
                    ch.line(
                        tag,
                        "Skipping GNOME Shell extensions during offline reboot transaction (apply live instead)."
                            .into(),
                    );
                    (0, String::new(), false)
                } else {
                    let uuids = gnome_extensions::extension_uuids_from_packages(&packages);
                    if uuids.is_empty() {
                        (0, String::new(), false)
                    } else {
                        let work_cancel = ch.cancel.work_cancel();
                        let mut native_log = String::new();
                        let outcome = runtime.apply_gnome_extensions(
                            &uuids,
                            &work_cancel,
                            &mut |l| {
                                native_log.push_str(&l);
                                native_log.push('\n');
                                ch.line(tag, l);
                            },
                            &mut |hint| emit_progress_hint(&ch, source, hint),
                        );
                        match outcome {
                            NativeOutcome::Handled(Ok(res)) => {
                                if res.cancelled || !ch.is_current() {
                                    return;
                                }
                                if res.completed > 0 {
                                    let note = crate::presenter::GNOME_EXT_RELOGIN_NOTE;
                                    native_log.push_str(note);
                                    native_log.push('\n');
                                    ch.line(tag, note.to_string());
                                }
                                if res.failed > 0 {
                                    if res.errors.is_empty() {
                                        let msg = format!(
                                            "{} GNOME Shell extension update(s) failed",
                                            res.failed
                                        );
                                        log_native_apply_error(&ch, tag, &mut native_log, &msg);
                                    } else {
                                        for err in &res.errors {
                                            log_native_apply_error(&ch, tag, &mut native_log, err);
                                        }
                                    }
                                    (1, native_log, false)
                                } else {
                                    (0, native_log, false)
                                }
                            }
                            NativeOutcome::Handled(Err(reason)) => {
                                log_native_apply_error(&ch, tag, &mut native_log, &reason);
                                (1, native_log, false)
                            }
                            NativeOutcome::Unavailable(reason) => {
                                let msg = if reason.trim().is_empty() {
                                    "GNOME Shell Extensions D-Bus service is unavailable".to_string()
                                } else {
                                    reason
                                };
                                log_native_apply_error(&ch, tag, &mut native_log, &msg);
                                (-1, native_log, false)
                            }
                        }
                    }
                }
            }
            UpdateSource::Homebrew => {
                if mode == ApplyMode::OfflineOnReboot {
                    ch.line(
                        tag,
                        "Skipping Homebrew during offline reboot transaction (apply live instead)."
                            .into(),
                    );
                    (0, String::new(), false)
                } else {
                    match brew_program(runtime) {
                        None => {
                            let msg = "Homebrew (brew) executable not found";
                            ch.line(tag, format!("error: {msg}"));
                            (-1, msg.to_string(), false)
                        }
                        Some(brew_bin) => {
                            let names = brew::upgrade_names_from_packages(&packages);
                            if names.is_empty() {
                                (0, String::new(), false)
                            } else {
                                let subcmd = if mode == ApplyMode::DownloadOnly {
                                    "fetch"
                                } else {
                                    "upgrade"
                                };
                                let mut args = Vec::with_capacity(1 + names.len());
                                args.push(subcmd);
                                args.extend(names.iter().map(String::as_str));
                                let work_cancel = ch.cancel.work_cancel();
                                let (c, l) = run_local_apply_with_env(
                                    &ch,
                                    runtime,
                                    &brew_bin,
                                    &args,
                                    brew::NONINTERACTIVE_NO_AUTO_UPDATE_ENV,
                                    Some(&work_cancel),
                                    tag,
                                    source,
                                );
                                (c, l, false)
                            }
                        }
                    }
                }
            }
            UpdateSource::Firmware => {
                if mode == ApplyMode::DownloadOnly {
                    ch.line(
                        tag,
                        "Skipping firmware flash during Download Only mode.".into(),
                    );
                    (0, String::new(), false)
                } else {
                    let fw_pkgs: Vec<Package> = packages
                        .iter()
                        .filter(|p| p.source == UpdateSource::Firmware)
                        .cloned()
                        .collect();
                    let mut native_log = String::new();
                    let outcome = runtime.apply_fwupd_dbus(
                        &fw_pkgs,
                        &mut |l| {
                            native_log.push_str(&l);
                            native_log.push('\n');
                            ch.line(tag, l);
                        },
                        &mut |hint| emit_progress_hint(&ch, source, hint),
                    );
                    match outcome {
                        NativeOutcome::Handled(Ok(res)) => (0, native_log, res.needs_reboot),
                        NativeOutcome::Handled(Err(reason)) => {
                            log_native_apply_error(&ch, tag, &mut native_log, &reason);
                            (1, native_log, false)
                        }
                        NativeOutcome::Unavailable(_) => {
                            let ready = if session_ensured {
                                Ok(())
                            } else {
                                match runtime.ensure_session() {
                                    Ok(()) => {
                                        session_ensured = true;
                                        Ok(())
                                    }
                                    Err(e) => Err(e),
                                }
                            };
                            match ready {
                                Ok(()) => {
                                    let (c, l) = run_helper_apply(
                                        &ch,
                                        runtime,
                                        HelperCommand::ApplyFwupd,
                                        tag,
                                        source,
                                    );
                                    (c, l, false)
                                }
                                Err(e) => {
                                    ch.line(tag, format!("error: {e}"));
                                    (-1, e.to_string(), false)
                                }
                            }
                        }
                    }
                }
            }
        };

        if !ch.is_current() {
            return;
        }

        let detect_fallback_reboot = if mode == ApplyMode::DownloadOnly {
            false
        } else {
            backend::detect_reboot(source, &log, &packages)
        };
        let reboot = code == 0 && (native_reboot || detect_fallback_reboot);
        if reboot {
            overall_reboot = true;
        }
        let error = if code == 0 {
            None
        } else {
            Some(summarize_apply_failure(source, &log, code))
        };

        ch.event(StateEvent::BackendApplyDone {
            source,
            success: code == 0,
            needs_reboot: reboot,
            error,
        });
    }

    if !ch.is_current() {
        return;
    }

    if mode != ApplyMode::DownloadOnly && sources.contains(&UpdateSource::Firmware) {
        match runtime.check_fwupd_reboot_dbus() {
            NativeOutcome::Handled(Ok(true)) => {
                overall_reboot = true;
            }
            NativeOutcome::Handled(Ok(false)) => {}
            NativeOutcome::Handled(Err(e)) => {
                ch.line("fwupd", format!("Reboot status check skipped: {e}"));
            }
            NativeOutcome::Unavailable(_) => {
                let ensure_ok = session_ensured || runtime.ensure_session().is_ok();
                if ensure_ok {
                    ch.line("fwupd", "Checking whether a reboot is needed…".into());
                    let mut log = String::new();
                    if let Ok(code) = runtime.run_helper(HelperCommand::CheckFwupdReboot, &mut |l| {
                        accept_helper_line(&ch, "fwupd", &mut log, &l);
                    }) {
                        if fwupd::parse_reboot_needed_output(&log, code) {
                            overall_reboot = true;
                        }
                    }
                }
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
}

fn emit_progress_hint(ch: &WorkChannel, source: UpdateSource, hint: ProgressHint) {
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

fn emit_progress_from_line(ch: &WorkChannel, source: UpdateSource, l: &str) {
    let hint = backend::parse_progress(source, l);
    emit_progress_hint(ch, source, hint);
}

fn run_helper_apply<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    cmd: HelperCommand,
    tag: &str,
    source: UpdateSource,
) -> (i32, String) {
    let mut log = String::new();
    match runtime.run_helper(cmd, &mut |l| {
        let text = accept_helper_line(ch, tag, &mut log, &l);
        emit_progress_from_line(ch, source, &text);
    }) {
        Ok(code) => (code, log),
        Err(e) => {
            ch.line(tag, format!("error: {e}"));
            (-1, format!("{log}\n{e}"))
        }
    }
}

fn run_local_apply<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    program: &str,
    args: &[&str],
    tag: &str,
    source: UpdateSource,
) -> (i32, String) {
    run_local_apply_with_env(ch, runtime, program, args, &[], None, tag, source)
}

#[allow(clippy::too_many_arguments)]
fn run_local_apply_with_env<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    cancel: Option<&WorkCancel>,
    tag: &str,
    source: UpdateSource,
) -> (i32, String) {
    let mut log = String::new();
    let mut handle_line = |l: String| {
        log.push_str(&l);
        log.push('\n');
        ch.line(tag, l.clone());
        emit_progress_from_line(ch, source, &l);
    };
    let result = if envs.is_empty() && cancel.is_none() {
        runtime.run_cancellable_local(program, args, &mut handle_line)
    } else {
        runtime.run_cancellable_local_env(program, args, envs, cancel, &mut handle_line)
    };
    match result {
        Ok(code) => {
            // "Nothing to update" is success (exit 0) — treat as ok even if unexpected.
            (code, log)
        }
        Err(e) => {
            ch.line(tag, format!("error: {e}"));
            (-1, e)
        }
    }
}

fn truncate_summary(s: &str, max_chars: usize) -> String {
    let clean: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = clean.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn strip_prefix_ascii_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Emit a native backend error to the activity console once when the backend
/// returned `Err(reason)` without having already logged it via `on_line`.
fn log_native_apply_error(
    ch: &WorkChannel,
    tag: &str,
    native_log: &mut String,
    reason: &str,
) {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return;
    }
    if !native_log.contains(trimmed) {
        let mut emitted = false;
        for line in trimmed.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let text = if !emitted && strip_prefix_ascii_ci(line, "error:").is_none() {
                format!("error: {line}")
            } else {
                line.to_string()
            };
            ch.line(tag, text);
            emitted = true;
        }
        native_log.push_str(trimmed);
        native_log.push('\n');
    }
}

fn prefix_source(source: UpdateSource, msg: &str) -> String {
    let label_prefix = format!("{}:", source.label());
    if strip_prefix_ascii_ci(msg, &label_prefix).is_some() {
        msg.to_string()
    } else {
        format!("{label_prefix} {msg}")
    }
}

/// Extract a concise, user-facing failure reason from a backend apply log.
pub fn summarize_apply_failure(source: UpdateSource, log: &str, exit_code: i32) -> String {
    let lower_full = log.to_ascii_lowercase();
    if source == UpdateSource::Dnf
        && (lower_full.contains("/var/lib/rpm/.rpm.lock")
            || lower_full.contains("rpm database is locked")
            || lower_full.contains("could not acquire lock"))
    {
        return "System: RPM database is locked by another process. Wait a moment and try again."
            .into();
    }

    if source == UpdateSource::Dnf {
        if let Some(problem) = log.lines().map(str::trim).find(|line| {
            let l = line.to_ascii_lowercase();
            l.starts_with("problem ")
                || l.starts_with("problem:")
                || l.contains("nothing provides")
                || l.contains("cannot install")
        }) {
            return format!(
                "System: dependency conflict — {}",
                truncate_summary(problem, 140)
            );
        }
        if let Some(resolve) = log
            .lines()
            .map(str::trim)
            .find(|line| line.to_ascii_lowercase().contains("failed to resolve"))
        {
            return format!(
                "System: dependency conflict — {}",
                truncate_summary(resolve, 140)
            );
        }
    }

    if source == UpdateSource::Firmware {
        if let Some(batt) = log.lines().map(str::trim).find(|line| {
            let l = line.to_ascii_lowercase();
            (l.contains("battery") || l.contains("ac power"))
                && (l.contains("ac power")
                    || l.contains("threshold")
                    || l.contains("too low")
                    || l.contains("required"))
        }) {
            return prefix_source(source, &truncate_summary(batt, 160));
        }
        if lower_full.contains("battery")
            && (lower_full.contains("ac power")
                || lower_full.contains("threshold")
                || lower_full.contains("too low"))
        {
            return "Firmware: Connect AC power and try again (battery too low).".into();
        }
    }

    for line in log.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = strip_prefix_ascii_ci(line, "error:") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return prefix_source(source, &truncate_summary(rest, 160));
            }
        }
    }

    if let Some(line) = log.lines().map(str::trim).find(|line| {
        let l = line.to_ascii_lowercase();
        l.starts_with("failed") || l.starts_with("cannot ")
    }) {
        return prefix_source(source, &truncate_summary(line, 160));
    }

    if let Some(last) = log.lines().map(str::trim).rfind(|l| !l.is_empty()) {
        return prefix_source(source, &truncate_summary(last, 160));
    }

    format!("{} update failed (exit {exit_code})", source.label())
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
    run_systemctl_reboot_with(tx, SystemRuntime);
}

/// Spawn [`execute_systemctl_reboot`] on a background thread using a custom [`OrchestratorRuntime`].
pub fn run_systemctl_reboot_with<R: OrchestratorRuntime>(tx: Sender<WorkerMsg>, runtime: R) {
    thread::spawn(move || {
        execute_systemctl_reboot(&tx, &runtime);
    });
}

/// Synchronously run native `systemd-logind` reboot with inhibitor pre-flight,
/// falling back to `systemctl reboot` when D-Bus is unavailable.
pub fn execute_systemctl_reboot<R: OrchestratorRuntime>(tx: &Sender<WorkerMsg>, runtime: &R) {
    match runtime.reboot_via_logind(&mut |l| line(tx, "logind", l)) {
        NativeOutcome::Handled(Ok(())) => return,
        NativeOutcome::Handled(Err(detail)) => {
            send(
                tx,
                WorkerMsg::Event(reboot_fail_event(REBOOT_FAILED_TITLE, detail)),
            );
            return;
        }
        NativeOutcome::Unavailable(_) => {}
    }

    let mut log = String::new();
    match runtime.run_local("systemctl", &["reboot"], &mut |l| {
        if !log.is_empty() {
            log.push('\n');
        }
        log.push_str(&l);
        line(tx, "systemctl", l);
    }) {
        Ok(0) => {}
        Ok(code) => send(
            tx,
            WorkerMsg::Event(reboot_fail_event(
                REBOOT_FAILED_TITLE,
                reboot_failure_detail(&log, code),
            )),
        ),
        Err(e) => send(
            tx,
            WorkerMsg::Event(reboot_fail_event(REBOOT_SPAWN_FAILED_TITLE, e)),
        ),
    }
}

pub fn session_started_event(purpose: AuthPurpose) -> StateEvent {
    StateEvent::SessionReady { purpose }
}

/// Close elevated helper and terminate any active local or native Flatpak work.
/// Call when the app window is closing — not after individual Check/Apply flows,
/// so the user is not re-prompted mid-session.
pub fn release_privileges() {
    dnf_dbus::cancel_active_transaction();
    flatpak::cancel_active_transaction();
    kill_recorded_local_command();
    close_session();
}

/// Whether Check/Apply can skip the polkit wait UI (helper already elevated).
pub fn can_skip_auth_ui() -> bool {
    session_is_alive()
}

/// Whether Check can bypass the polkit helper wait screen (`dnf5daemon` is zero-password).
pub fn can_skip_check_auth_ui(native_dnf: bool) -> bool {
    native_dnf || session_is_alive()
}

/// Whether Apply can bypass the helper wait screen (`dnf5daemon` prompts directly via polkit).
pub fn can_skip_apply_auth_ui(native_dnf: bool) -> bool {
    native_dnf || session_is_alive()
}

/// Load merged chronological update history across `org.rpm.dnf.v0.History` and `fwupd`.
pub fn execute_load_history<R: OrchestratorRuntime>(
    runtime: &R,
    limit: i64,
) -> Vec<dnf_dbus::HistoryEntry> {
    let dnf_txns = match runtime.query_dnf_history(limit) {
        NativeOutcome::Handled(Ok(list)) => list,
        _ => Vec::new(),
    };
    let fwupd_entries = match runtime.query_fwupd_history() {
        NativeOutcome::Handled(Ok(list)) => list,
        _ => Vec::new(),
    };
    dnf_dbus::merge_history(&dnf_txns, &fwupd_entries)
}

/// Load merged update history using [`SystemRuntime`].
pub fn load_history_system(limit: i64) -> Vec<dnf_dbus::HistoryEntry> {
    execute_load_history(&SystemRuntime, limit)
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
        let ch = WorkChannel {
            tx: &tx,
            cancel: CancelCheck::GlobalGen(gen),
        };
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
        let ch = WorkChannel {
            tx: &tx,
            cancel: CancelCheck::GlobalGen(gen),
        };
        map_session_start_err(&ch, SessionError::AuthDenied, RetryAction::CheckAgain);
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::Event(StateEvent::Fail {
                retry: RetryAction::CheckAgain,
                ..
            }))
        ));

        map_session_start_err(&ch, SessionError::AuthDenied, RetryAction::RetryApply);
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkerMsg::Event(StateEvent::Fail {
                retry: RetryAction::RetryApply,
                ..
            }))
        ));
    }

    #[test]
    fn summarize_apply_failure_extracts_actionable_causes() {
        let rpm_lock = summarize_apply_failure(
            UpdateSource::Dnf,
            "Waiting for process\nError: Could not acquire lock on /var/lib/rpm/.rpm.lock",
            1,
        );
        assert!(rpm_lock.contains("RPM database is locked"), "{rpm_lock}");

        let dep_conflict = summarize_apply_failure(
            UpdateSource::Dnf,
            "Failed to resolve the transaction:\nProblem 1: package foo requires bar\n",
            1,
        );
        assert!(dep_conflict.contains("dependency conflict"), "{dep_conflict}");
        assert!(dep_conflict.contains("Problem 1: package foo"), "{dep_conflict}");

        let battery = summarize_apply_failure(
            UpdateSource::Firmware,
            "Downloading…: 100.0%\nCannot update firmware while on battery power (battery level 18% < 25%); connect AC power and try again.\n",
            1,
        );
        assert!(battery.starts_with("Firmware:"), "{battery}");
        assert!(battery.contains("AC power"), "{battery}");

        let flatpak_err = summarize_apply_failure(
            UpdateSource::FlatpakUser,
            "Looking for updates…\nerror: Failed to update org.signal.Signal: Socket I/O timed out\n",
            1,
        );
        assert!(flatpak_err.starts_with("Apps:"), "{flatpak_err}");
        assert!(flatpak_err.contains("Socket I/O timed out"), "{flatpak_err}");
        assert!(!flatpak_err.starts_with("Apps: error:"), "{flatpak_err}");

        let empty = summarize_apply_failure(UpdateSource::Dnf, "  \n", 137);
        assert_eq!(empty, "System update failed (exit 137)");
    }

    #[test]
    fn work_channel_drops_events_after_cancel() {
        let _guard = lock_work_gen();
        let (tx, rx) = async_channel::unbounded();
        let gen = begin_background_work();
        let ch = WorkChannel {
            tx: &tx,
            cancel: CancelCheck::GlobalGen(gen),
        };

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

    struct CancellingGlobalRuntime {
        cancel_on_ensure_session: bool,
        cancel_on_check_dnf: bool,
        helper_calls: Mutex<Vec<HelperCommand>>,
    }

    impl OrchestratorRuntime for CancellingGlobalRuntime {
        fn ensure_session(&self) -> Result<(), SessionError> {
            if self.cancel_on_ensure_session {
                cancel_background_work();
            }
            Ok(())
        }

        fn run_helper(
            &self,
            cmd: HelperCommand,
            on_line: &mut dyn FnMut(String),
        ) -> Result<i32, SessionError> {
            self.helper_calls.lock().unwrap().push(cmd);
            if self.cancel_on_check_dnf && cmd == HelperCommand::CheckDnf {
                on_line("firefox.x86_64 140.0-1.fc44 updates".into());
                cancel_background_work();
                on_line("late-line-after-cancel".into());
            }
            Ok(0)
        }

        fn run_local(
            &self,
            _program: &str,
            _args: &[&str],
            _on_line: &mut dyn FnMut(String),
        ) -> Result<i32, String> {
            panic!("local command must not be called after cancel");
        }

        fn query_flatpak(&self, _user: bool) -> Result<flatpak::FlatpakCheck, String> {
            panic!("flatpak check must not be called after cancel");
        }
    }

    #[test]
    fn cancel_background_work_stops_global_gen_check_pipeline() {
        let _guard = lock_work_gen();

        // Cancel during ensure_session: emits nothing and never invokes CheckDnf.
        let (tx, rx) = async_channel::unbounded();
        let gen = begin_background_work();
        let rt_auth = CancellingGlobalRuntime {
            cancel_on_ensure_session: true,
            cancel_on_check_dnf: false,
            helper_calls: Mutex::new(Vec::new()),
        };
        execute_check_all_inner(
            &tx,
            &rt_auth,
            CancelCheck::GlobalGen(gen),
            &SourceConfig::all_enabled(),
        );
        assert!(rx.try_recv().is_err());
        assert!(rt_auth.helper_calls.lock().unwrap().is_empty());

        // Cancel mid-CheckDnf: drops late lines, emits no BackendCheckDone / CheckComplete.
        let (tx, rx) = async_channel::unbounded();
        let gen = begin_background_work();
        let rt_dnf = CancellingGlobalRuntime {
            cancel_on_ensure_session: false,
            cancel_on_check_dnf: true,
            helper_calls: Mutex::new(Vec::new()),
        };
        execute_check_all_inner(
            &tx,
            &rt_dnf,
            CancelCheck::GlobalGen(gen),
            &SourceConfig::all_enabled(),
        );
        let mut msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }
        assert!(matches!(msgs.first(), Some(WorkerMsg::SessionStarted)));
        assert!(msgs.iter().any(|m| matches!(
            m,
            WorkerMsg::ConsoleLine { text, .. } if text == "firefox.x86_64 140.0-1.fc44 updates"
        )));
        assert!(!msgs.iter().any(|m| matches!(
            m,
            WorkerMsg::ConsoleLine { text, .. } if text == "late-line-after-cancel"
        )));
        assert!(!msgs
            .iter()
            .any(|m| matches!(m, WorkerMsg::Event(_))));
        assert_eq!(*rt_dnf.helper_calls.lock().unwrap(), vec![HelperCommand::CheckDnf]);
    }

    #[test]
    fn source_config_filters_and_detects_all_disabled() {
        let mut cfg = SourceConfig::all_enabled();
        assert!(!cfg.all_disabled());
        assert_eq!(cfg.enabled_sources(), UpdateSource::ALL);

        cfg.disable("  FLATPAK  ");
        assert!(!cfg.is_enabled(UpdateSource::FlatpakUser));
        assert!(!cfg.is_enabled(UpdateSource::FlatpakSystem));
        assert!(cfg.is_enabled(UpdateSource::Dnf));
        assert!(cfg.is_enabled(UpdateSource::GnomeExtensions));
        assert!(cfg.is_enabled(UpdateSource::Homebrew));
        assert!(cfg.is_enabled(UpdateSource::Firmware));
        assert_eq!(
            cfg.enabled_sources(),
            vec![
                UpdateSource::Dnf,
                UpdateSource::GnomeExtensions,
                UpdateSource::Homebrew,
                UpdateSource::Firmware,
            ]
        );

        cfg.disable("dnf");
        cfg.disable("gnome-extensions");
        cfg.disable("homebrew");
        cfg.disable("firmware");
        assert!(cfg.all_disabled());
        assert!(cfg.enabled_sources().is_empty());

        cfg.enable("flatpak");
        assert!(!cfg.all_disabled());
        assert_eq!(
            cfg.enabled_sources(),
            vec![UpdateSource::FlatpakUser, UpdateSource::FlatpakSystem]
        );

        let from_keys = SourceConfig::from_disabled_keys([" ".into(), "Firmware".into()]);
        assert!(!from_keys.is_enabled(UpdateSource::Firmware));
        assert!(from_keys.is_enabled(UpdateSource::Dnf));
        assert!(from_keys.is_enabled(UpdateSource::GnomeExtensions));
        assert!(from_keys.is_enabled(UpdateSource::Homebrew));
    }

    #[test]
    fn default_and_probed_source_availability_cover_all_pref_sources() {
        let defaults = default_source_availability();
        for src in UpdateSource::pref_sources() {
            let entry = defaults.get(src.pref_key()).expect("default availability");
            assert_eq!(entry.installed, src.installed_by_default());
            if src.installed_by_default() {
                assert_eq!(entry.subtitle, "Installed");
            } else {
                assert_eq!(entry.subtitle, "Not installed");
            }
        }

        let probed_no_brew = probe_source_availability_with(true, false, None, false);
        assert_eq!(
            probed_no_brew.get("dnf").map(|s| s.subtitle.as_str()),
            Some("Installed (dnf5daemon D-Bus)")
        );
        assert!(probed_no_brew.get("dnf").unwrap().installed);
        assert!(probed_no_brew.contains_key("flatpak"));
        assert!(!probed_no_brew.get("gnome-extensions").unwrap().installed);
        assert_eq!(
            probed_no_brew.get("gnome-extensions").unwrap().subtitle,
            "Not available (requires GNOME Shell)"
        );
        assert!(!probed_no_brew.get("homebrew").unwrap().installed);
        assert_eq!(probed_no_brew.get("homebrew").unwrap().subtitle, "Not installed");
        assert!(probed_no_brew.contains_key("firmware"));

        let probed_with_brew_and_ext = probe_source_availability_with(
            true,
            false,
            Some(std::path::Path::new(brew::LINUXBREW_SYSTEM_PATH)),
            true,
        );
        assert!(probed_with_brew_and_ext.get("gnome-extensions").unwrap().installed);
        assert_eq!(
            probed_with_brew_and_ext
                .get("gnome-extensions")
                .unwrap()
                .subtitle,
            "Available (session D-Bus)"
        );
        assert!(probed_with_brew_and_ext.get("homebrew").unwrap().installed);
        assert_eq!(
            probed_with_brew_and_ext.get("homebrew").unwrap().subtitle,
            "Installed (/home/linuxbrew/.linuxbrew/bin/brew)"
        );
    }
}
