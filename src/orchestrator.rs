//! Sequential multi-backend check/apply using one privileged helper session.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;

use async_channel::Sender;

use crate::backend::{
    self, brew, cleanup, containers, dnf, dnf_dbus, flatpak, fwupd, fwupd_dbus, gnome_extensions,
    hooks, logind, mise, python_tools, rpm_ostree, rustup, ProgressHint,
};
pub use crate::backend::NativeOutcome;
use crate::helper_protocol::{split_helper_line, HelperCommand, HelperOutput};
use crate::model::{ApplyMode, AuthPurpose, Package, PackageStatus, RetryAction, UpdateSource};
use crate::privilege::{
    close_session, kill_recorded_local_command, run_cancellable_local_command,
    run_cancellable_local_command_env, run_local_command, session_is_alive, with_session,
    SessionError, SessionEvent,
};
use crate::state::Event as StateEvent;

fn normalize_config_keys(keys: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    keys.into_iter()
        .map(|key| key.trim().to_ascii_lowercase())
        .filter(|key| !key.is_empty())
        .collect()
}

/// Enabled/disabled source filter applied during check and apply pipelines.
/// Opt-in sources (`Distrobox`, `Toolbx`) remain disabled unless explicitly enabled,
/// and `CustomHook` activates only when at least one enabled valid hook is configured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceConfig {
    pub disabled_keys: BTreeSet<String>,
    pub enabled_opt_in_keys: BTreeSet<String>,
    pub custom_hooks: Vec<hooks::CustomHook>,
}

impl SourceConfig {
    pub fn all_enabled() -> Self {
        let enabled_opt_in_keys = UpdateSource::ALL
            .iter()
            .copied()
            .filter(|s| s.is_opt_in())
            .map(|s| s.pref_key().to_string())
            .collect();
        Self {
            disabled_keys: BTreeSet::new(),
            enabled_opt_in_keys,
            custom_hooks: Vec::new(),
        }
    }

    pub fn from_disabled_keys(keys: impl IntoIterator<Item = String>) -> Self {
        Self::from_prefs(keys, std::iter::empty())
    }

    pub fn from_prefs(
        disabled_keys: impl IntoIterator<Item = String>,
        enabled_opt_in_keys: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::from_prefs_with_hooks(disabled_keys, enabled_opt_in_keys, Vec::new())
    }

    pub fn from_prefs_with_hooks(
        disabled_keys: impl IntoIterator<Item = String>,
        enabled_opt_in_keys: impl IntoIterator<Item = String>,
        custom_hooks: Vec<hooks::CustomHook>,
    ) -> Self {
        Self {
            disabled_keys: normalize_config_keys(disabled_keys),
            enabled_opt_in_keys: normalize_config_keys(enabled_opt_in_keys),
            custom_hooks: custom_hooks
                .into_iter()
                .map(|h| h.normalized())
                .filter(hooks::CustomHook::is_valid)
                .collect(),
        }
    }

    pub fn with_custom_hooks(mut self, custom_hooks: Vec<hooks::CustomHook>) -> Self {
        self.custom_hooks = custom_hooks
            .into_iter()
            .map(|h| h.normalized())
            .filter(hooks::CustomHook::is_valid)
            .collect();
        self
    }

    pub fn is_key_enabled(&self, pref_key: &str) -> bool {
        let normalized = pref_key.trim().to_ascii_lowercase();
        if UpdateSource::pref_key_is_opt_in(&normalized) {
            self.enabled_opt_in_keys.contains(&normalized)
                && !self.disabled_keys.contains(&normalized)
        } else {
            !self.disabled_keys.contains(&normalized)
        }
    }

    pub fn is_enabled(&self, source: UpdateSource) -> bool {
        if source == UpdateSource::CustomHook {
            return self.is_key_enabled(source.pref_key())
                && !hooks::enabled_hooks_with_ids(&self.custom_hooks).is_empty();
        }
        self.is_key_enabled(source.pref_key())
    }

    pub fn enabled_sources(&self) -> Vec<UpdateSource> {
        self.enabled_sources_for_host(false)
    }

    pub fn enabled_sources_for_host(&self, ostree_booted: bool) -> Vec<UpdateSource> {
        UpdateSource::ALL
            .iter()
            .copied()
            .filter(|s| s.is_applicable_for_host(ostree_booted) && self.is_enabled(*s))
            .collect()
    }

    pub fn all_disabled(&self) -> bool {
        self.all_disabled_for_host(false)
    }

    pub fn all_disabled_for_host(&self, ostree_booted: bool) -> bool {
        !UpdateSource::ALL
            .iter()
            .any(|s| s.is_applicable_for_host(ostree_booted) && self.is_enabled(*s))
    }

    pub fn disable(&mut self, pref_key: &str) {
        let normalized = pref_key.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return;
        }
        if UpdateSource::pref_key_is_opt_in(&normalized) {
            self.enabled_opt_in_keys.remove(&normalized);
        } else {
            self.disabled_keys.insert(normalized);
        }
    }

    pub fn enable(&mut self, pref_key: &str) {
        let normalized = pref_key.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return;
        }
        self.disabled_keys.remove(&normalized);
        if UpdateSource::pref_key_is_opt_in(&normalized) {
            self.enabled_opt_in_keys.insert(normalized);
        }
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

/// Optional executable paths injected into [`probe_source_availability_for_host_with`]
/// for deterministic offline tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostBinaryPaths<'a> {
    pub brew_bin: Option<&'a Path>,
    pub rustup_bin: Option<&'a Path>,
    pub mise_bin: Option<&'a Path>,
    pub uv_bin: Option<&'a Path>,
    pub pipx_bin: Option<&'a Path>,
    pub rpm_ostree_bin: Option<&'a Path>,
    pub distrobox_bin: Option<&'a Path>,
    pub toolbox_bin: Option<&'a Path>,
}

/// Probe host availability of supported update sources without spawning processes.
pub fn probe_source_availability(
    native_dnf: bool,
    native_fwupd: bool,
    gnome_ext_available: bool,
) -> BTreeMap<String, SourceAvailability> {
    probe_source_availability_for_host(false, native_dnf, native_fwupd, gnome_ext_available)
}

/// Probe host availability of update sources for the given host mode without spawning processes.
pub fn probe_source_availability_for_host(
    ostree_booted: bool,
    native_dnf: bool,
    native_fwupd: bool,
    gnome_ext_available: bool,
) -> BTreeMap<String, SourceAvailability> {
    let brew_bin = brew::find_brew_binary();
    let rustup_bin = rustup::find_rustup_binary();
    let mise_bin = mise::find_mise_binary();
    let uv_bin = python_tools::find_uv_binary();
    let pipx_bin = python_tools::find_pipx_binary();
    let distrobox_bin = containers::find_distrobox_binary();
    let toolbox_bin = containers::find_toolbox_binary();
    let rpm_ostree_bin = if ostree_booted {
        rpm_ostree::find_rpm_ostree_binary()
    } else {
        None
    };
    probe_source_availability_for_host_with(
        ostree_booted,
        native_dnf,
        native_fwupd,
        gnome_ext_available,
        HostBinaryPaths {
            brew_bin: brew_bin.as_deref(),
            rustup_bin: rustup_bin.as_deref(),
            mise_bin: mise_bin.as_deref(),
            uv_bin: uv_bin.as_deref(),
            pipx_bin: pipx_bin.as_deref(),
            rpm_ostree_bin: rpm_ostree_bin.as_deref(),
            distrobox_bin: distrobox_bin.as_deref(),
            toolbox_bin: toolbox_bin.as_deref(),
        },
    )
}

/// Injectable variant of [`probe_source_availability`] for deterministic offline tests.
pub fn probe_source_availability_with(
    native_dnf: bool,
    native_fwupd: bool,
    brew_bin: Option<&Path>,
    gnome_ext_available: bool,
) -> BTreeMap<String, SourceAvailability> {
    probe_source_availability_for_host_with(
        false,
        native_dnf,
        native_fwupd,
        gnome_ext_available,
        HostBinaryPaths {
            brew_bin,
            ..Default::default()
        },
    )
}

/// Host-mode-aware injectable variant of [`probe_source_availability`] for offline tests.
pub fn probe_source_availability_for_host_with(
    ostree_booted: bool,
    native_dnf: bool,
    native_fwupd: bool,
    gnome_ext_available: bool,
    paths: HostBinaryPaths<'_>,
) -> BTreeMap<String, SourceAvailability> {
    let mut map = BTreeMap::new();
    if ostree_booted {
        let ostree_installed = paths.rpm_ostree_bin.is_some();
        let ostree_sub = match paths.rpm_ostree_bin {
            Some(path) => format!("Installed ({})", path.display()),
            None => "Not installed".into(),
        };
        map.insert(
            "rpm-ostree".into(),
            SourceAvailability {
                pref_key: "rpm-ostree",
                installed: ostree_installed,
                subtitle: ostree_sub,
            },
        );
    } else {
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
    }

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

    for (key, bin) in [
        ("homebrew", paths.brew_bin),
        ("rustup", paths.rustup_bin),
        ("mise", paths.mise_bin),
        ("uv", paths.uv_bin),
        ("pipx", paths.pipx_bin),
    ] {
        let installed = bin.is_some();
        let subtitle = match bin {
            Some(path) => format!("Installed ({})", path.display()),
            None => "Not installed".into(),
        };
        map.insert(
            key.into(),
            SourceAvailability {
                pref_key: key,
                installed,
                subtitle,
            },
        );
    }

    for (key, bin) in [
        ("distrobox", paths.distrobox_bin),
        ("toolbx", paths.toolbox_bin),
    ] {
        let installed = bin.is_some();
        let subtitle = if installed {
            "Updates installed containers when enabled"
        } else {
            "Not installed"
        };
        map.insert(
            key.into(),
            SourceAvailability {
                pref_key: key,
                installed,
                subtitle: subtitle.into(),
            },
        );
    }

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
    default_source_availability_for_host(false)
}

/// Host-mode-aware optimistic default availability before live host probing.
pub fn default_source_availability_for_host(
    ostree_booted: bool,
) -> BTreeMap<String, SourceAvailability> {
    let mut map = BTreeMap::new();
    for src in UpdateSource::pref_sources_for_host(ostree_booted) {
        let key = src.pref_key();
        let installed = src.installed_by_default_for_host(ostree_booted);
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
    rpm_ostree::cancel_active_transaction();
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
    fn is_ostree_booted(&self) -> bool {
        false
    }
    fn rpm_ostree_binary(&self) -> Option<PathBuf> {
        None
    }
    fn query_rpm_ostree_status(&self) -> NativeOutcome<rpm_ostree::OstreeStatus> {
        NativeOutcome::Unavailable("rpm-ostree status unavailable in test runtime".into())
    }
    fn cancel_rpm_ostree(&self) -> bool {
        false
    }
    fn brew_binary(&self) -> Option<PathBuf> {
        None
    }
    fn brew_available(&self) -> bool {
        self.brew_binary().is_some()
    }
    fn rustup_binary(&self) -> Option<PathBuf> {
        None
    }
    fn mise_binary(&self) -> Option<PathBuf> {
        None
    }
    fn uv_binary(&self) -> Option<PathBuf> {
        None
    }
    fn pipx_binary(&self) -> Option<PathBuf> {
        None
    }
    fn distrobox_binary(&self) -> Option<PathBuf> {
        None
    }
    fn toolbox_binary(&self) -> Option<PathBuf> {
        None
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
    fn query_flatpak_unused(&self, _user: bool) -> NativeOutcome<Vec<flatpak::FlatpakListedRef>> {
        NativeOutcome::Unavailable("libflatpak unused query unavailable in test runtime".into())
    }
    fn has_flatpak_cli(&self) -> bool {
        false
    }
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

    fn is_ostree_booted(&self) -> bool {
        rpm_ostree::is_ostree_booted()
    }
    fn rpm_ostree_binary(&self) -> Option<PathBuf> {
        rpm_ostree::find_rpm_ostree_binary()
    }
    fn query_rpm_ostree_status(&self) -> NativeOutcome<rpm_ostree::OstreeStatus> {
        rpm_ostree::query_status()
    }
    fn cancel_rpm_ostree(&self) -> bool {
        rpm_ostree::cancel_active_transaction()
    }
    fn brew_binary(&self) -> Option<PathBuf> {
        brew::find_brew_binary()
    }

    fn rustup_binary(&self) -> Option<PathBuf> {
        rustup::find_rustup_binary()
    }

    fn mise_binary(&self) -> Option<PathBuf> {
        mise::find_mise_binary()
    }

    fn uv_binary(&self) -> Option<PathBuf> {
        python_tools::find_uv_binary()
    }

    fn pipx_binary(&self) -> Option<PathBuf> {
        python_tools::find_pipx_binary()
    }

    fn distrobox_binary(&self) -> Option<PathBuf> {
        containers::find_distrobox_binary()
    }

    fn toolbox_binary(&self) -> Option<PathBuf> {
        containers::find_toolbox_binary()
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

    fn query_flatpak_unused(&self, user: bool) -> NativeOutcome<Vec<flatpak::FlatpakListedRef>> {
        flatpak::query_unused_refs(user)
    }

    fn has_flatpak_cli(&self) -> bool {
        command_exists("flatpak")
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
    let ostree_booted = runtime.is_ostree_booted();
    if config.all_disabled_for_host(ostree_booted) {
        ch.event(StateEvent::Fail {
            title: "All update sources are disabled".into(),
            detail: "Enable at least one update source in Preferences (Ctrl+,) to check for updates.".into(),
            retry: RetryAction::CheckAgain,
        });
        return;
    }

    let dnf_enabled = !ostree_booted && config.is_enabled(UpdateSource::Dnf);
    let ostree_enabled = ostree_booted && config.is_enabled(UpdateSource::RpmOstree);
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

    // 1) System engine: rpm-ostree (zero-password) on Atomic hosts, DNF on classic hosts.
    if ostree_enabled {
        let ostree_res = run_rpm_ostree_check(&ch, runtime);
        if !ch.is_current() {
            return;
        }
        emit_backend_check(
            &ch,
            UpdateSource::RpmOstree,
            ostree_res,
            &mut all,
            &mut soft_errors,
        );
    } else if dnf_enabled {
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

    // 5b) User-space developer toolchains (rustup, mise, uv, pipx) — silent when absent.
    for (tc_source, tc_res) in [
        (
            UpdateSource::Rustup,
            config
                .is_enabled(UpdateSource::Rustup)
                .then(|| path_program(runtime.rustup_binary()))
                .flatten()
                .map(|bin| run_rustup_check(&ch, runtime, &bin)),
        ),
        (
            UpdateSource::Mise,
            config
                .is_enabled(UpdateSource::Mise)
                .then(|| path_program(runtime.mise_binary()))
                .flatten()
                .map(|bin| run_mise_check(&ch, runtime, &bin)),
        ),
        (
            UpdateSource::UvTools,
            config
                .is_enabled(UpdateSource::UvTools)
                .then(|| path_program(runtime.uv_binary()))
                .flatten()
                .map(|bin| run_uv_check(&ch, runtime, &bin)),
        ),
        (
            UpdateSource::Pipx,
            config
                .is_enabled(UpdateSource::Pipx)
                .then(|| path_program(runtime.pipx_binary()))
                .flatten()
                .map(|bin| run_pipx_check(&ch, runtime, &bin)),
        ),
    ] {
        let Some(res) = tc_res else {
            continue;
        };
        if !ch.is_current() {
            return;
        }
        emit_backend_check(&ch, tc_source, res, &mut all, &mut soft_errors);
    }

    // 6) Distrobox & Toolbx — unprivileged rootless container discovery when opted-in and installed.
    let mut discovered_distrobox: Vec<containers::ContainerInfo> = Vec::new();
    if config.is_enabled(UpdateSource::Distrobox) {
        if let Some(dbox_bin) = container_program(runtime, UpdateSource::Distrobox) {
            if !ch.is_current() {
                return;
            }
            let dbox_res = match run_container_list_check(
                &ch,
                runtime,
                UpdateSource::Distrobox,
                &dbox_bin,
                &[],
            ) {
                Ok((pkgs, infos)) => {
                    discovered_distrobox = infos;
                    Ok(pkgs)
                }
                Err(err) => Err(err),
            };
            if !ch.is_current() {
                return;
            }
            emit_backend_check(
                &ch,
                UpdateSource::Distrobox,
                dbox_res,
                &mut all,
                &mut soft_errors,
            );
        }
    }
    if config.is_enabled(UpdateSource::Toolbx) {
        if let Some(tbx_bin) = container_program(runtime, UpdateSource::Toolbx) {
            if !ch.is_current() {
                return;
            }
            let tbx_res = run_container_list_check(
                &ch,
                runtime,
                UpdateSource::Toolbx,
                &tbx_bin,
                &discovered_distrobox,
            )
            .map(|(pkgs, _)| pkgs);
            if !ch.is_current() {
                return;
            }
            emit_backend_check(
                &ch,
                UpdateSource::Toolbx,
                tbx_res,
                &mut all,
                &mut soft_errors,
            );
        }
    }

    if config.is_enabled(UpdateSource::CustomHook) {
        if !ch.is_current() {
            return;
        }
        let hooks_res = check_custom_hooks(&ch, runtime, &config.custom_hooks, &mut soft_errors);
        if !ch.is_current() {
            return;
        }
        emit_backend_check(
            &ch,
            UpdateSource::CustomHook,
            hooks_res,
            &mut all,
            &mut soft_errors,
        );
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

fn rpm_ostree_program<R: OrchestratorRuntime>(runtime: &R) -> Option<String> {
    runtime
        .rpm_ostree_binary()
        .map(|path| path.to_string_lossy().into_owned())
}

fn run_rpm_ostree_check<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
) -> Result<Vec<Package>, String> {
    let bin = rpm_ostree_program(runtime)
        .ok_or_else(|| "rpm-ostree: executable not found".to_string())?;

    let status = match runtime.query_rpm_ostree_status() {
        NativeOutcome::Handled(Ok(st)) => {
            if let Some(booted) = st.booted_deployment() {
                let ver = booted
                    .version
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("unknown");
                ch.line(
                    "rpm-ostree",
                    format!("Booted deployment: {} ({ver})", booted.repo_label()),
                );
            }
            Some(st)
        }
        NativeOutcome::Handled(Err(e)) => {
            ch.line("rpm-ostree", format!("Status query skipped: {e}"));
            None
        }
        NativeOutcome::Unavailable(_) => None,
    };

    if !ch.is_current() {
        return Err("cancelled".into());
    }

    let work_cancel = ch.cancel.work_cancel();
    let mut check_log = String::new();
    let code = runtime
        .run_cancellable_local_env(
            &bin,
            &["upgrade", "--check"],
            rpm_ostree::NONINTERACTIVE_ENV,
            Some(&work_cancel),
            &mut |l| {
                if !l.trim().is_empty() {
                    ch.line("rpm-ostree", l.clone());
                }
                check_log.push_str(&l);
                check_log.push('\n');
            },
        )
        .map_err(|e| format!("rpm-ostree: {e}"))?;

    if !ch.is_current() {
        return Err("cancelled".into());
    }

    if rpm_ostree::check_exit_is_no_updates(code, &check_log) {
        ch.line("rpm-ostree", "No upgrade available.".into());
        return Ok(Vec::new());
    }

    let pkgs = match rpm_ostree::parse_upgrade_check(&check_log, status.as_ref()) {
        Ok(pkgs) => pkgs,
        Err(err) => {
            return Err(if code != 0 {
                summarize_apply_failure(UpdateSource::RpmOstree, &check_log, code)
            } else {
                format!("rpm-ostree: {err}")
            });
        }
    };

    if rpm_ostree::check_exit_is_hard_error(code, &check_log, &pkgs) {
        return Err(summarize_apply_failure(
            UpdateSource::RpmOstree,
            &check_log,
            code,
        ));
    }

    ch.line(
        "rpm-ostree",
        format!("{} update(s) in pending deployment", pkgs.len()),
    );
    Ok(pkgs)
}

fn brew_program<R: OrchestratorRuntime>(runtime: &R) -> Option<String> {
    runtime
        .brew_binary()
        .map(|path| path.to_string_lossy().into_owned())
}

fn container_program<R: OrchestratorRuntime>(
    runtime: &R,
    source: UpdateSource,
) -> Option<String> {
    match source {
        UpdateSource::Distrobox => runtime
            .distrobox_binary()
            .map(|path| path.to_string_lossy().into_owned()),
        UpdateSource::Toolbx => runtime
            .toolbox_binary()
            .map(|path| path.to_string_lossy().into_owned()),
        _ => None,
    }
}

fn run_container_list_check<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    source: UpdateSource,
    bin: &str,
    existing_distrobox: &[containers::ContainerInfo],
) -> Result<(Vec<Package>, Vec<containers::ContainerInfo>), String> {
    let tag = source.console_tag();
    let (args, label) = match source {
        UpdateSource::Distrobox => (&["list", "--no-color"][..], "Distrobox"),
        UpdateSource::Toolbx => (&["list", "--containers"][..], "Toolbx"),
        _ => return Ok((Vec::new(), Vec::new())),
    };
    let work_cancel = ch.cancel.work_cancel();
    let mut log = String::new();
    let code = runtime
        .run_cancellable_local_env(
            bin,
            args,
            containers::NONINTERACTIVE_ENV,
            Some(&work_cancel),
            &mut |l| {
                log.push_str(&l);
                log.push('\n');
            },
        )
        .map_err(|e| format!("{tag}: {e}"))?;

    if !ch.is_current() || work_cancel.is_cancelled() {
        return Err("cancelled".into());
    }

    let infos = match source {
        UpdateSource::Distrobox => containers::parse_distrobox_list(&log),
        UpdateSource::Toolbx => {
            let parsed = containers::parse_toolbox_list(&log);
            containers::filter_toolbox_duplicates(parsed, existing_distrobox)
        }
        _ => Vec::new(),
    };

    if infos.is_empty() && !containers::list_exit_is_empty(code, &log) {
        for line in log.lines().map(str::trim).filter(|l| !l.is_empty()).take(8) {
            ch.line(tag, line.to_string());
        }
        return Err(format!("{tag}: {} list exited with {code}", label.to_ascii_lowercase()));
    }

    if !infos.is_empty() {
        ch.line(tag, format!("{} {} container(s)", infos.len(), label));
        for info in &infos {
            ch.line(
                tag,
                format!("  {} ({} · {})", info.name, info.image, info.status),
            );
        }
    }

    let pkgs = infos
        .iter()
        .cloned()
        .map(|info| info.into_package(source))
        .collect();
    Ok((pkgs, infos))
}

enum ContainerApplyOutcome {
    Cancelled,
    Finished {
        code: i32,
        log: String,
        error: Option<String>,
    },
}

fn run_container_apply<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    source: UpdateSource,
    packages: &[Package],
    mode: ApplyMode,
) -> ContainerApplyOutcome {
    let tag = source.console_tag();
    let (display_name, bin_name) = match source {
        UpdateSource::Distrobox => ("Distrobox containers", "distrobox"),
        UpdateSource::Toolbx => ("Toolbx containers", "toolbox"),
        _ => {
            return ContainerApplyOutcome::Finished {
                code: 0,
                log: String::new(),
                error: None,
            }
        }
    };

    if mode == ApplyMode::DownloadOnly {
        ch.line(
            tag,
            format!("Skipping {display_name} during Download Only mode."),
        );
        return ContainerApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }
    if mode == ApplyMode::OfflineOnReboot {
        ch.line(
            tag,
            format!(
                "Skipping {display_name} during offline reboot transaction (apply live instead)."
            ),
        );
        return ContainerApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }

    let Some(bin) = container_program(runtime, source) else {
        let msg = format!("{bin_name} executable not found");
        ch.line(tag, format!("error: {msg}"));
        return ContainerApplyOutcome::Finished {
            code: -1,
            log: msg.clone(),
            error: Some(format!("{}: {msg}", source.label())),
        };
    };

    let targets = containers::container_names_from_packages(packages, source);
    if targets.is_empty() {
        return ContainerApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }

    let work_cancel = ch.cancel.work_cancel();
    let mut full_log = String::new();
    let mut failed_errors = Vec::new();
    let label_prefix = format!("{}: ", source.label());

    for (_pkg_id, name) in targets {
        if !ch.is_current() || work_cancel.is_cancelled() {
            return ContainerApplyOutcome::Cancelled;
        }

        ch.line(tag, format!("── Upgrading {name} ──"));
        emit_progress_hint(
            ch,
            source,
            ProgressHint {
                package_name: Some(name.clone()),
                status: Some(PackageStatus::Installing),
                progress: None,
                phase_label: Some(format!("Upgrading {name}")),
                stage_current: None,
                stage_total: None,
            },
        );

        let args: Vec<&str> = match source {
            UpdateSource::Distrobox => vec!["upgrade", name.as_str()],
            UpdateSource::Toolbx => vec![
                "run",
                "--container",
                name.as_str(),
                "sudo",
                "dnf",
                "upgrade",
                "-y",
            ],
            _ => unreachable!(),
        };

        let mut per_log = String::new();
        let exec_res = runtime.run_cancellable_local_env(
            &bin,
            &args,
            containers::NONINTERACTIVE_ENV,
            Some(&work_cancel),
            &mut |l| {
                per_log.push_str(&l);
                per_log.push('\n');
                full_log.push_str(&l);
                full_log.push('\n');
                ch.line(tag, l.clone());

                let mut hint = containers::parse_progress_line(&l);
                if hint.progress.is_some() || hint.phase_label.is_some() {
                    hint.package_name = Some(name.clone());
                    hint.status = match hint.status {
                        Some(PackageStatus::Downloading) => Some(PackageStatus::Downloading),
                        _ => Some(PackageStatus::Installing),
                    };
                    emit_progress_hint(ch, source, hint);
                }
            },
        );

        if !ch.is_current() || work_cancel.is_cancelled() {
            return ContainerApplyOutcome::Cancelled;
        }

        match exec_res {
            Ok(0) => {
                ch.line(tag, format!("Container {name} upgraded."));
                emit_progress_hint(
                    ch,
                    source,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Completed),
                        progress: Some(1.0),
                        phase_label: Some("Complete".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
            Ok(code) => {
                let base_reason = summarize_apply_failure(source, &per_log, code);
                let stripped = strip_prefix_ascii_ci(&base_reason, &label_prefix)
                    .unwrap_or(&base_reason)
                    .trim();
                let reason = if stripped.contains(&name) {
                    stripped.to_string()
                } else {
                    format!("{name}: {stripped}")
                };
                if !per_log
                    .lines()
                    .any(|l| strip_prefix_ascii_ci(l.trim(), "error:").is_some())
                {
                    ch.line(tag, format!("error: {reason}"));
                }
                failed_errors.push(reason);
                emit_progress_hint(
                    ch,
                    source,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Failed),
                        progress: None,
                        phase_label: Some("Failed".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
            Err(err) => {
                let reason = format!("{name}: {err}");
                ch.line(tag, format!("error: {reason}"));
                full_log.push_str(&reason);
                full_log.push('\n');
                failed_errors.push(reason);
                emit_progress_hint(
                    ch,
                    source,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Failed),
                        progress: None,
                        phase_label: Some("Failed".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
        }
    }

    if !ch.is_current() || work_cancel.is_cancelled() {
        return ContainerApplyOutcome::Cancelled;
    }

    if failed_errors.is_empty() {
        ContainerApplyOutcome::Finished {
            code: 0,
            log: full_log,
            error: None,
        }
    } else {
        ContainerApplyOutcome::Finished {
            code: 1,
            log: full_log,
            error: Some(format!("{}: {}", source.label(), failed_errors.join("; "))),
        }
    }
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

fn path_program(path: Option<PathBuf>) -> Option<String> {
    path.map(|p| p.to_string_lossy().into_owned())
}

fn unsupported_apply_mode_reason(label: &str, mode: ApplyMode) -> Option<String> {
    match mode {
        ApplyMode::DownloadOnly => Some(format!("Skipping {label} during Download Only mode.")),
        ApplyMode::OfflineOnReboot => Some(format!(
            "Skipping {label} during offline reboot transaction (apply live instead)."
        )),
        ApplyMode::Live | ApplyMode::LiveAndReboot => None,
    }
}

fn log_truncated_lines(ch: &WorkChannel, tag: &str, log: &str) {
    for line in log
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(8)
    {
        ch.line(tag, line.to_string());
    }
}

fn log_discovered_toolchain_packages(ch: &WorkChannel, tag: &str, pkgs: &[Package]) {
    ch.line(tag, format!("{} update(s) ({tag})", pkgs.len()));
    for pkg in pkgs {
        ch.line(tag, format!("  {} ({})", pkg.name, pkg.version_line()));
    }
}

fn run_capture_local<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    bin: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    work_cancel: &WorkCancel,
    tag: &str,
) -> Result<(i32, String), String> {
    let mut log = String::new();
    let code = runtime
        .run_cancellable_local_env(bin, args, envs, Some(work_cancel), &mut |l| {
            log.push_str(&l);
            log.push('\n');
        })
        .map_err(|e| format!("{tag}: {e}"))?;
    if !ch.is_current() || work_cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    Ok((code, log))
}

fn run_rustup_check<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    rustup_bin: &str,
) -> Result<Vec<Package>, String> {
    let work_cancel = ch.cancel.work_cancel();
    let (code, log) = run_capture_local(
        ch,
        runtime,
        rustup_bin,
        &["check"],
        &[],
        &work_cancel,
        "rustup",
    )?;
    if !rustup::check_exit_is_ok(code) {
        log_truncated_lines(ch, "rustup", &log);
        return Err(format!("rustup: rustup check exited with {code}"));
    }
    let pkgs = rustup::parse_rustup_check_for_binary(&log, Some(Path::new(rustup_bin)));
    log_discovered_toolchain_packages(ch, "rustup", &pkgs);
    Ok(pkgs)
}

fn run_mise_check<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    mise_bin: &str,
) -> Result<Vec<Package>, String> {
    let work_cancel = ch.cancel.work_cancel();
    let (code, json_log) = run_capture_local(
        ch,
        runtime,
        mise_bin,
        &["outdated", "--json"],
        &[("MISE_YES", "1")],
        &work_cancel,
        "mise",
    )?;
    if code != 0 {
        log_truncated_lines(ch, "mise", &json_log);
        return Err(format!("mise: mise outdated exited with {code}"));
    }
    match mise::parse_mise_outdated_json(&json_log) {
        Ok(pkgs) => {
            log_discovered_toolchain_packages(ch, "mise", &pkgs);
            Ok(pkgs)
        }
        Err(err) => {
            log_truncated_lines(ch, "mise", &json_log);
            Err(format!("mise: {err}"))
        }
    }
}

fn run_uv_check<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    uv_bin: &str,
) -> Result<Vec<Package>, String> {
    let work_cancel = ch.cancel.work_cancel();
    let (code, list_log) =
        run_capture_local(ch, runtime, uv_bin, &["tool", "list"], &[], &work_cancel, "uv")?;
    if code != 0 {
        log_truncated_lines(ch, "uv", &list_log);
        return Err(format!("uv: uv tool list exited with {code}"));
    }
    let installed = python_tools::parse_uv_tool_list(&list_log);
    if installed.is_empty() {
        ch.line("uv", "0 update(s) (uv)".into());
        return Ok(Vec::new());
    }

    let (dry_code, dry_log) = run_capture_local(
        ch,
        runtime,
        uv_bin,
        &["tool", "upgrade", "--all", "--dry-run"],
        &[],
        &work_cancel,
        "uv",
    )?;
    if dry_code == 0 {
        let pkgs = python_tools::parse_uv_upgrade_dry_run(&dry_log, &installed);
        log_discovered_toolchain_packages(ch, "uv", &pkgs);
        return Ok(pkgs);
    }
    if !python_tools::flag_is_unsupported(&dry_log) {
        log_truncated_lines(ch, "uv", &dry_log);
        return Err(format!(
            "uv: uv tool upgrade --all --dry-run exited with {dry_code}"
        ));
    }

    let (out_code, out_log) = run_capture_local(
        ch,
        runtime,
        uv_bin,
        &["tool", "list", "--outdated"],
        &[],
        &work_cancel,
        "uv",
    )?;
    if out_code == 0 {
        let pkgs = python_tools::parse_uv_tool_list_outdated(&out_log, &installed);
        log_discovered_toolchain_packages(ch, "uv", &pkgs);
        return Ok(pkgs);
    }
    if !python_tools::flag_is_unsupported(&out_log) {
        log_truncated_lines(ch, "uv", &out_log);
        return Err(format!("uv: uv tool list --outdated exited with {out_code}"));
    }

    let (dir_code, dir_log) =
        run_capture_local(ch, runtime, uv_bin, &["tool", "dir"], &[], &work_cancel, "uv")?;
    if dir_code == 0 {
        if let Some(tool_dir) = dir_log.lines().map(str::trim).find(|l| !l.is_empty()) {
            let mut pkgs = Vec::new();
            let mut checked_any_venv = false;
            for tool in &installed {
                let raw_python = Path::new(tool_dir).join(&tool.name).join("bin/python");
                let python = if raw_python.is_file() {
                    raw_python
                } else {
                    let norm_name = python_tools::normalize_python_name(&tool.name);
                    let norm_python = Path::new(tool_dir).join(norm_name).join("bin/python");
                    if !norm_python.is_file() {
                        continue;
                    }
                    norm_python
                };
                let python_str = python.to_string_lossy();
                let (pip_code, pip_json) = run_capture_local(
                    ch,
                    runtime,
                    uv_bin,
                    &[
                        "pip",
                        "list",
                        "--outdated",
                        "--format",
                        "json",
                        "--python",
                        &python_str,
                    ],
                    &[],
                    &work_cancel,
                    "uv",
                )?;
                if pip_code != 0 {
                    continue;
                }
                checked_any_venv = true;
                if let Ok(Some(pkg)) = python_tools::parse_uv_pip_outdated_json(&pip_json, tool) {
                    pkgs.push(pkg);
                }
            }
            if checked_any_venv {
                log_discovered_toolchain_packages(ch, "uv", &pkgs);
                return Ok(pkgs);
            }
        }
    }

    ch.line(
        "uv",
        "Check-only outdated inspection is not supported by this uv version; leaving uv tools untouched.".into(),
    );
    Ok(Vec::new())
}

fn run_pipx_check<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    pipx_bin: &str,
) -> Result<Vec<Package>, String> {
    let work_cancel = ch.cancel.work_cancel();
    let (list_code, list_json) = run_capture_local(
        ch,
        runtime,
        pipx_bin,
        &["list", "--json"],
        &[],
        &work_cancel,
        "pipx",
    )?;
    if list_code != 0 {
        log_truncated_lines(ch, "pipx", &list_json);
        return Err(format!("pipx: pipx list --json exited with {list_code}"));
    }
    let installed = match python_tools::parse_pipx_list_json(&list_json) {
        Ok(items) => items,
        Err(err) => {
            log_truncated_lines(ch, "pipx", &list_json);
            return Err(format!("pipx: {err}"));
        }
    };
    if installed.iter().all(|pkg| pkg.pinned) {
        ch.line("pipx", "0 update(s) (pipx)".into());
        return Ok(Vec::new());
    }

    let (out_code, out_log) = run_capture_local(
        ch,
        runtime,
        pipx_bin,
        &["list", "--outdated"],
        &[],
        &work_cancel,
        "pipx",
    )?;
    if out_code == 0 {
        let pkgs = python_tools::parse_pipx_outdated_text(&out_log, &installed);
        log_discovered_toolchain_packages(ch, "pipx", &pkgs);
        return Ok(pkgs);
    }
    if python_tools::flag_is_unsupported(&out_log) {
        ch.line(
            "pipx",
            "pipx list --outdated is unsupported by this pipx version; skipping without mutating venvs.".into(),
        );
        return Ok(Vec::new());
    }
    log_truncated_lines(ch, "pipx", &out_log);
    Err(format!("pipx: pipx list --outdated exited with {out_code}"))
}

enum ToolchainApplyOutcome {
    Cancelled,
    Finished {
        code: i32,
        log: String,
        error: Option<String>,
    },
}

fn run_rustup_apply<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    packages: &[Package],
    mode: ApplyMode,
) -> ToolchainApplyOutcome {
    let tag = UpdateSource::Rustup.console_tag();
    if let Some(reason) = unsupported_apply_mode_reason("Rustup", mode) {
        ch.line(tag, reason);
        return ToolchainApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }
    let Some(bin) = path_program(runtime.rustup_binary()) else {
        let msg = "rustup executable not found".to_string();
        ch.line(tag, format!("error: {msg}"));
        return ToolchainApplyOutcome::Finished {
            code: -1,
            log: msg,
            error: None,
        };
    };

    let plan = rustup::split_apply_targets(None, packages, Some(Path::new(&bin)));
    if plan.toolchains.is_empty() && !plan.self_update {
        return ToolchainApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }

    let work_cancel = ch.cancel.work_cancel();
    let mut combined_log = String::new();
    let mut exit_code = 0;

    if !plan.toolchains.is_empty() {
        let mut args = vec!["update", "--no-self-update"];
        args.extend(plan.toolchains.iter().map(String::as_str));
        let (code, log) = run_local_apply_with_env(
            ch,
            runtime,
            &bin,
            &args,
            &[],
            Some(&work_cancel),
            tag,
            UpdateSource::Rustup,
        );
        combined_log.push_str(&log);
        if !ch.is_current() || work_cancel.is_cancelled() {
            return ToolchainApplyOutcome::Cancelled;
        }
        if code != 0 {
            return ToolchainApplyOutcome::Finished {
                code,
                log: combined_log,
                error: None,
            };
        }
        exit_code = code;
    }

    if plan.self_update {
        let (code, log) = run_local_apply_with_env(
            ch,
            runtime,
            &bin,
            &["self", "update"],
            &[],
            Some(&work_cancel),
            tag,
            UpdateSource::Rustup,
        );
        if !combined_log.is_empty() && !log.is_empty() {
            combined_log.push('\n');
        }
        combined_log.push_str(&log);
        if !ch.is_current() || work_cancel.is_cancelled() {
            return ToolchainApplyOutcome::Cancelled;
        }
        exit_code = code;
    }

    ToolchainApplyOutcome::Finished {
        code: exit_code,
        log: combined_log,
        error: None,
    }
}

fn run_mise_apply<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    packages: &[Package],
    mode: ApplyMode,
) -> ToolchainApplyOutcome {
    let tag = UpdateSource::Mise.console_tag();
    if let Some(reason) = unsupported_apply_mode_reason("Mise", mode) {
        ch.line(tag, reason);
        return ToolchainApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }
    let Some(bin) = path_program(runtime.mise_binary()) else {
        let msg = "mise executable not found".to_string();
        ch.line(tag, format!("error: {msg}"));
        return ToolchainApplyOutcome::Finished {
            code: -1,
            log: msg,
            error: None,
        };
    };

    let names = mise::upgrade_names_from_packages(None, packages);
    if names.is_empty() {
        return ToolchainApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }

    let mut args = Vec::with_capacity(1 + names.len());
    args.push("upgrade");
    args.extend(names.iter().map(String::as_str));
    let work_cancel = ch.cancel.work_cancel();
    let (code, log) = run_local_apply_with_env(
        ch,
        runtime,
        &bin,
        &args,
        &[("MISE_YES", "1")],
        Some(&work_cancel),
        tag,
        UpdateSource::Mise,
    );
    if !ch.is_current() || work_cancel.is_cancelled() {
        return ToolchainApplyOutcome::Cancelled;
    }
    ToolchainApplyOutcome::Finished {
        code,
        log,
        error: None,
    }
}

fn run_per_tool_local_apply<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    source: UpdateSource,
    packages: &[Package],
    mode: ApplyMode,
) -> ToolchainApplyOutcome {
    let tag = source.console_tag();
    let (display_name, bin_name, bin, names) = match source {
        UpdateSource::UvTools => (
            "uv tools",
            "uv",
            path_program(runtime.uv_binary()),
            python_tools::uv_tool_names_from_packages(None, packages),
        ),
        UpdateSource::Pipx => (
            "pipx packages",
            "pipx",
            path_program(runtime.pipx_binary()),
            python_tools::pipx_package_names_from_packages(None, packages),
        ),
        _ => {
            return ToolchainApplyOutcome::Finished {
                code: 0,
                log: String::new(),
                error: None,
            }
        }
    };

    if let Some(reason) = unsupported_apply_mode_reason(display_name, mode) {
        ch.line(tag, reason);
        return ToolchainApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }
    let Some(bin) = bin else {
        let msg = format!("{bin_name} executable not found");
        ch.line(tag, format!("error: {msg}"));
        return ToolchainApplyOutcome::Finished {
            code: -1,
            log: msg.clone(),
            error: Some(format!("{}: {msg}", source.label())),
        };
    };
    if names.is_empty() {
        return ToolchainApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }

    let work_cancel = ch.cancel.work_cancel();
    let mut full_log = String::new();
    let mut failed_errors = Vec::new();
    let label_prefix = format!("{}: ", source.label());

    for name in names {
        if !ch.is_current() || work_cancel.is_cancelled() {
            return ToolchainApplyOutcome::Cancelled;
        }
        ch.line(tag, format!("── Upgrading {name} ──"));
        emit_progress_hint(
            ch,
            source,
            ProgressHint {
                package_name: Some(name.clone()),
                status: Some(PackageStatus::Installing),
                progress: None,
                phase_label: Some(format!("Upgrading {name}")),
                stage_current: None,
                stage_total: None,
            },
        );

        let args: Vec<&str> = match source {
            UpdateSource::UvTools => vec!["tool", "upgrade", name.as_str()],
            UpdateSource::Pipx => vec!["upgrade", name.as_str()],
            _ => unreachable!(),
        };
        let mut per_log = String::new();
        let exec_res = runtime.run_cancellable_local_env(
            &bin,
            &args,
            &[],
            Some(&work_cancel),
            &mut |l| {
                per_log.push_str(&l);
                per_log.push('\n');
                full_log.push_str(&l);
                full_log.push('\n');
                ch.line(tag, l.clone());

                let mut hint = backend::parse_progress(source, &l);
                if hint.status.is_some() || hint.progress.is_some() || hint.phase_label.is_some() {
                    if hint.package_name.is_none() {
                        hint.package_name = Some(name.clone());
                    }
                    emit_progress_hint(ch, source, hint);
                }
            },
        );
        if !ch.is_current() || work_cancel.is_cancelled() {
            return ToolchainApplyOutcome::Cancelled;
        }

        match exec_res {
            Ok(0) => {
                emit_progress_hint(
                    ch,
                    source,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Completed),
                        progress: Some(1.0),
                        phase_label: Some("Complete".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
            Ok(code) => {
                let base_reason = summarize_apply_failure(source, &per_log, code);
                let stripped = strip_prefix_ascii_ci(&base_reason, &label_prefix)
                    .unwrap_or(&base_reason)
                    .trim();
                let reason = if stripped.contains(&name) {
                    stripped.to_string()
                } else {
                    format!("{name}: {stripped}")
                };
                if !per_log
                    .lines()
                    .any(|l| strip_prefix_ascii_ci(l.trim(), "error:").is_some())
                {
                    ch.line(tag, format!("error: {reason}"));
                }
                failed_errors.push(reason);
                emit_progress_hint(
                    ch,
                    source,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Failed),
                        progress: None,
                        phase_label: Some("Failed".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
            Err(err) => {
                let reason = format!("{name}: {err}");
                ch.line(tag, format!("error: {reason}"));
                full_log.push_str(&reason);
                full_log.push('\n');
                failed_errors.push(reason);
                emit_progress_hint(
                    ch,
                    source,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Failed),
                        progress: None,
                        phase_label: Some("Failed".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
        }
    }

    if failed_errors.is_empty() {
        ToolchainApplyOutcome::Finished {
            code: 0,
            log: full_log,
            error: None,
        }
    } else {
        ToolchainApplyOutcome::Finished {
            code: 1,
            log: full_log,
            error: Some(format!("{}: {}", source.label(), failed_errors.join("; "))),
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

        let (code, log, native_reboot, explicit_error) = match source {
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
                        (0, native_log, res.needs_reboot, None)
                    }
                    NativeOutcome::Handled(Err(reason)) => {
                        log_native_apply_error(&ch, tag, &mut native_log, &reason);
                        (1, native_log, false, None)
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
                                (c, l, false, None)
                            }
                            Err(e) => {
                                ch.line(tag, format!("error: {e}"));
                                (-1, e.to_string(), false, None)
                            }
                        }
                    }
                }
            }
            UpdateSource::RpmOstree => {
                if mode == ApplyMode::DownloadOnly {
                    ch.line(
                        tag,
                        "Skipping Atomic OS deployment during Download Only mode.".into(),
                    );
                    (0, String::new(), false, None)
                } else {
                    if mode == ApplyMode::OfflineOnReboot {
                        ch.line(
                            tag,
                            "Staging Atomic OS deployment for next boot (rpm-ostree upgrade)…"
                                .into(),
                        );
                    }
                    match rpm_ostree_program(runtime) {
                        None => {
                            let msg = "rpm-ostree executable not found";
                            ch.line(tag, format!("error: {msg}"));
                            (-1, msg.to_string(), false, None)
                        }
                        Some(ostree_bin) => {
                            let _active_guard = rpm_ostree::ApplyActiveGuard::enter();
                            let work_cancel = ch.cancel.work_cancel();
                            work_cancel.on_cancel(|| {
                                let _ = rpm_ostree::cancel_active_transaction();
                            });
                            let (c, l) = run_local_apply_with_env(
                                &ch,
                                runtime,
                                &ostree_bin,
                                &["upgrade"],
                                rpm_ostree::NONINTERACTIVE_ENV,
                                Some(&work_cancel),
                                tag,
                                source,
                            );
                            if !ch.is_current() || work_cancel.is_cancelled() {
                                let _ = runtime.cancel_rpm_ostree();
                                return;
                            }
                            let staged =
                                c == 0 && rpm_ostree::detect_reboot_needed(&l, &packages);
                            (c, l, staged, None)
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
                    (0, String::new(), false, None)
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
                        (0, String::new(), false, None)
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
                            NativeOutcome::Handled(Ok(_)) => (0, native_log, false, None),
                            NativeOutcome::Handled(Err(reason)) => {
                                log_native_apply_error(&ch, tag, &mut native_log, &reason);
                                (1, native_log, false, None)
                            }
                            NativeOutcome::Unavailable(_) => {
                                let args = flatpak::update_args_for_refs(user, &refs);
                                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                                let (c, l) =
                                    run_local_apply(&ch, runtime, "flatpak", &arg_refs, tag, source);
                                (c, l, false, None)
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
                    (0, String::new(), false, None)
                } else if mode == ApplyMode::OfflineOnReboot {
                    ch.line(
                        tag,
                        "Skipping GNOME Shell extensions during offline reboot transaction (apply live instead)."
                            .into(),
                    );
                    (0, String::new(), false, None)
                } else {
                    let uuids = gnome_extensions::extension_uuids_from_packages(&packages);
                    if uuids.is_empty() {
                        (0, String::new(), false, None)
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
                                    (1, native_log, false, None)
                                } else {
                                    (0, native_log, false, None)
                                }
                            }
                            NativeOutcome::Handled(Err(reason)) => {
                                log_native_apply_error(&ch, tag, &mut native_log, &reason);
                                (1, native_log, false, None)
                            }
                            NativeOutcome::Unavailable(reason) => {
                                let msg = if reason.trim().is_empty() {
                                    "GNOME Shell Extensions D-Bus service is unavailable".to_string()
                                } else {
                                    reason
                                };
                                log_native_apply_error(&ch, tag, &mut native_log, &msg);
                                (-1, native_log, false, None)
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
                    (0, String::new(), false, None)
                } else {
                    match brew_program(runtime) {
                        None => {
                            let msg = "Homebrew (brew) executable not found";
                            ch.line(tag, format!("error: {msg}"));
                            (-1, msg.to_string(), false, None)
                        }
                        Some(brew_bin) => {
                            let names = brew::upgrade_names_from_packages(&packages);
                            if names.is_empty() {
                                (0, String::new(), false, None)
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
                                (c, l, false, None)
                            }
                        }
                    }
                }
            }
            UpdateSource::Rustup => match run_rustup_apply(&ch, runtime, &packages, mode) {
                ToolchainApplyOutcome::Cancelled => return,
                ToolchainApplyOutcome::Finished { code, log, error } => {
                    (code, log, false, error)
                }
            },
            UpdateSource::Mise => match run_mise_apply(&ch, runtime, &packages, mode) {
                ToolchainApplyOutcome::Cancelled => return,
                ToolchainApplyOutcome::Finished { code, log, error } => {
                    (code, log, false, error)
                }
            },
            UpdateSource::UvTools | UpdateSource::Pipx => {
                match run_per_tool_local_apply(&ch, runtime, source, &packages, mode) {
                    ToolchainApplyOutcome::Cancelled => return,
                    ToolchainApplyOutcome::Finished { code, log, error } => {
                        (code, log, false, error)
                    }
                }
            }
            UpdateSource::Distrobox | UpdateSource::Toolbx => {
                match run_container_apply(&ch, runtime, source, &packages, mode) {
                    ContainerApplyOutcome::Cancelled => return,
                    ContainerApplyOutcome::Finished { code, log, error } => {
                        (code, log, false, error)
                    }
                }
            }
            UpdateSource::CustomHook => {
                if mode == ApplyMode::DownloadOnly {
                    ch.line(
                        tag,
                        "Skipping custom user hooks during Download Only mode.".into(),
                    );
                    (0, String::new(), false, None)
                } else {
                    match run_custom_hooks_apply(&ch, runtime, &packages, &config.custom_hooks) {
                        ToolchainApplyOutcome::Cancelled => return,
                        ToolchainApplyOutcome::Finished { code, log, error } => {
                            (code, log, false, error)
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
                    (0, String::new(), false, None)
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
                        NativeOutcome::Handled(Ok(res)) => (0, native_log, res.needs_reboot, None),
                        NativeOutcome::Handled(Err(reason)) => {
                            log_native_apply_error(&ch, tag, &mut native_log, &reason);
                            (1, native_log, false, None)
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
                                    (c, l, false, None)
                                }
                                Err(e) => {
                                    ch.line(tag, format!("error: {e}"));
                                    (-1, e.to_string(), false, None)
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
            explicit_error.or_else(|| Some(summarize_apply_failure(source, &log, code)))
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

fn check_custom_hooks<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    custom_hooks: &[hooks::CustomHook],
    soft_errors: &mut Vec<String>,
) -> Result<Vec<Package>, String> {
    let tag = UpdateSource::CustomHook.console_tag();
    let work_cancel = ch.cancel.work_cancel();
    let active = hooks::enabled_hooks_with_ids(custom_hooks);
    if active.is_empty() {
        return Ok(Vec::new());
    }

    let mut pkgs = Vec::new();
    let mut hook_errors = Vec::new();

    for (id, hook) in active {
        if !ch.is_current() || work_cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        if let Some(check_cmd) = hook.check_command.as_deref() {
            ch.line(tag, format!("Checking custom hook \"{}\"…", hook.name));
            let mut out = String::new();
            let exec_res = runtime.run_cancellable_local_env(
                "sh",
                &["-c", check_cmd],
                &[],
                Some(&work_cancel),
                &mut |l| {
                    out.push_str(&l);
                    out.push('\n');
                },
            );
            if !ch.is_current() || work_cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            match exec_res {
                Ok(code) => {
                    let decision = hooks::interpret_check_result(code, &out);
                    match &decision {
                        hooks::HookCheckResult::UpToDate => {
                            ch.line(tag, format!("  {}: up to date", hook.name));
                        }
                        hooks::HookCheckResult::UpdateAvailable { summary, .. } => {
                            ch.line(tag, format!("  {}: {}", hook.name, summary));
                            if let Some(pkg) =
                                hooks::package_from_checked_hook(&id, &hook, &decision)
                            {
                                pkgs.push(pkg);
                            }
                        }
                        hooks::HookCheckResult::Failed(detail) => {
                            let msg = format!("{}: {detail}", hook.name);
                            ch.line(tag, format!("  error: {msg}"));
                            hook_errors.push(msg);
                        }
                    }
                }
                Err(err) => {
                    let msg = format!("{}: {err}", hook.name);
                    ch.line(tag, format!("  error: {msg}"));
                    hook_errors.push(msg);
                }
            }
        } else {
            ch.line(
                tag,
                format!(
                    "  {}: custom user command enabled (no check command)",
                    hook.name
                ),
            );
            pkgs.push(hooks::package_from_unverified_hook(&id, &hook));
        }
    }

    if !hook_errors.is_empty() {
        let joined = format!("Custom hooks check: {}", hook_errors.join("; "));
        if pkgs.is_empty() {
            return Err(joined);
        }
        soft_errors.push(joined);
    }

    Ok(pkgs)
}

fn run_custom_hooks_apply<R: OrchestratorRuntime>(
    ch: &WorkChannel,
    runtime: &R,
    packages: &[Package],
    custom_hooks: &[hooks::CustomHook],
) -> ToolchainApplyOutcome {
    let tag = UpdateSource::CustomHook.console_tag();
    let work_cancel = ch.cancel.work_cancel();
    let selected: Vec<&Package> = packages
        .iter()
        .filter(|p| p.source == UpdateSource::CustomHook)
        .collect();
    if selected.is_empty() {
        return ToolchainApplyOutcome::Finished {
            code: 0,
            log: String::new(),
            error: None,
        };
    }

    let mut full_log = String::new();
    let mut failed_errors = Vec::new();

    for pkg in selected {
        if !ch.is_current() || work_cancel.is_cancelled() {
            return ToolchainApplyOutcome::Cancelled;
        }
        let resolved = hooks::resolve_hooks_for_packages(custom_hooks, &[pkg]);
        let Some((_, hook, name)) = resolved.into_iter().next() else {
            let reason = format!("{}: custom hook is disabled or no longer configured", pkg.name);
            ch.line(tag, format!("error: {reason}"));
            full_log.push_str(&reason);
            full_log.push('\n');
            failed_errors.push(reason);
            emit_progress_hint(
                ch,
                UpdateSource::CustomHook,
                ProgressHint {
                    package_name: Some(pkg.name.clone()),
                    status: Some(PackageStatus::Failed),
                    progress: None,
                    phase_label: Some("Failed".into()),
                    stage_current: None,
                    stage_total: None,
                },
            );
            continue;
        };

        ch.line(tag, format!("── Running hook: {name} (unprivileged user) ──"));
        emit_progress_hint(
            ch,
            UpdateSource::CustomHook,
            ProgressHint {
                package_name: Some(name.clone()),
                status: Some(PackageStatus::Installing),
                progress: None,
                phase_label: Some(format!("Running {name}")),
                stage_current: None,
                stage_total: None,
            },
        );

        let mut per_log = String::new();
        let exec_res = runtime.run_cancellable_local_env(
            "sh",
            &["-c", hook.apply_command.as_str()],
            &[],
            Some(&work_cancel),
            &mut |l| {
                per_log.push_str(&l);
                per_log.push('\n');
                full_log.push_str(&l);
                full_log.push('\n');
                ch.line(tag, l.clone());

                let mut hint = backend::parse_progress(UpdateSource::CustomHook, &l);
                if hint.status.is_some() || hint.progress.is_some() || hint.phase_label.is_some() {
                    if hint.package_name.is_none() {
                        hint.package_name = Some(name.clone());
                    }
                    emit_progress_hint(ch, UpdateSource::CustomHook, hint);
                }
            },
        );
        if !ch.is_current() || work_cancel.is_cancelled() {
            return ToolchainApplyOutcome::Cancelled;
        }

        match exec_res {
            Ok(0) => {
                emit_progress_hint(
                    ch,
                    UpdateSource::CustomHook,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Completed),
                        progress: Some(1.0),
                        phase_label: Some("Complete".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
            Ok(code) => {
                let reason = format!("{name}: exited with {code}");
                ch.line(tag, format!("error: {reason}"));
                failed_errors.push(reason);
                emit_progress_hint(
                    ch,
                    UpdateSource::CustomHook,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Failed),
                        progress: None,
                        phase_label: Some("Failed".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
            Err(err) => {
                let reason = format!("{name}: {err}");
                ch.line(tag, format!("error: {reason}"));
                full_log.push_str(&reason);
                full_log.push('\n');
                failed_errors.push(reason);
                emit_progress_hint(
                    ch,
                    UpdateSource::CustomHook,
                    ProgressHint {
                        package_name: Some(name),
                        status: Some(PackageStatus::Failed),
                        progress: None,
                        phase_label: Some("Failed".into()),
                        stage_current: None,
                        stage_total: None,
                    },
                );
            }
        }
    }

    if failed_errors.is_empty() {
        ToolchainApplyOutcome::Finished {
            code: 0,
            log: full_log,
            error: None,
        }
    } else {
        ToolchainApplyOutcome::Finished {
            code: 1,
            log: full_log,
            error: Some(format!("Custom hooks: {}", failed_errors.join("; "))),
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

    if source == UpdateSource::RpmOstree {
        if lower_full.contains("transaction in progress")
            || lower_full.contains("active transaction")
            || lower_full.contains("already in progress")
        {
            return "System (Atomic): Another rpm-ostree transaction is already in progress. Wait a moment or cancel it and try again."
                .into();
        }
        if lower_full.contains("polkit")
            || lower_full.contains("not authorized")
            || lower_full.contains("permission denied")
            || lower_full.contains("authentication")
        {
            return "System (Atomic): Authorization cancelled or denied by rpm-ostreed.".into();
        }
        if let Some(conflict) = log.lines().map(str::trim).find(|line| {
            let l = line.to_ascii_lowercase();
            l.contains("could not depsolve")
                || l.contains("cannot install")
                || l.starts_with("problem ")
                || (l.contains("conflict") && !l.starts_with("error:"))
        }) {
            return format!(
                "System (Atomic): layered package conflict — {}",
                truncate_summary(conflict, 140)
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

/// Summary of a user-confirmed post-update cleanup execution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupRunOutcome {
    pub removed_count: usize,
    pub reclaimed_bytes: u64,
    pub failed_steps: Vec<String>,
    pub cancelled: bool,
    pub remaining_plan: cleanup::CleanupPlan,
}

/// Probe unused Flatpak runtimes and Homebrew leaves without modifying system state.
pub fn discover_cleanup(
    config: &SourceConfig,
    cancel: Option<&WorkCancel>,
) -> Result<cleanup::CleanupPlan, String> {
    discover_cleanup_with(&SystemRuntime, config, cancel)
}

/// Testable read-only unused cleanup discovery over an injected [`OrchestratorRuntime`].
pub fn discover_cleanup_with<R: OrchestratorRuntime>(
    runtime: &R,
    config: &SourceConfig,
    cancel: Option<&WorkCancel>,
) -> Result<cleanup::CleanupPlan, String> {
    if cancel.is_some_and(WorkCancel::is_cancelled) {
        return Err("cancelled".into());
    }

    let mut plan = cleanup::CleanupPlan::default();

    if config.is_enabled(UpdateSource::FlatpakUser) {
        for user in [true, false] {
            if cancel.is_some_and(WorkCancel::is_cancelled) {
                return Err("cancelled".into());
            }
            let scope_label = if user {
                "User Flatpak"
            } else {
                "System Flatpak"
            };
            match runtime.query_flatpak_unused(user) {
                NativeOutcome::Handled(Ok(refs)) => {
                    plan.items
                        .extend(cleanup::cleanup_items_from_flatpak_refs(&refs, user));
                }
                NativeOutcome::Handled(Err(err)) => {
                    plan.warnings.push(format!("{scope_label} cleanup probe: {err}"));
                }
                NativeOutcome::Unavailable(_) => {
                    if runtime.has_flatpak_cli() {
                        let args = cleanup::flatpak_uninstall_unused_dry_run_args(user);
                        let mut out = String::new();
                        let res = runtime.run_cancellable_local_env(
                            "flatpak",
                            &args,
                            &[],
                            cancel,
                            &mut |l| {
                                out.push_str(&l);
                                out.push('\n');
                            },
                        );
                        if cancel.is_some_and(WorkCancel::is_cancelled) {
                            return Err("cancelled".into());
                        }
                        match res {
                            Ok(code) => {
                                let parsed = cleanup::parse_flatpak_unused_dry_run(&out, user);
                                if !parsed.is_empty() {
                                    plan.items.extend(parsed);
                                } else {
                                    let lower = out.to_ascii_lowercase();
                                    let unsupported = lower.contains("unknown option");
                                    let nothing = lower.contains("nothing unused");
                                    if code != 0 && code != 1 && !unsupported && !nothing {
                                        plan.warnings.push(format!(
                                            "{scope_label} cleanup probe exited with {code}"
                                        ));
                                    }
                                }
                            }
                            Err(err) => {
                                plan.warnings.push(format!("{scope_label} cleanup probe: {err}"));
                            }
                        }
                    }
                }
            }
        }
    }

    if config.is_enabled(UpdateSource::Homebrew) {
        if let Some(brew_bin) = runtime.brew_binary() {
            if cancel.is_some_and(WorkCancel::is_cancelled) {
                return Err("cancelled".into());
            }
            let brew_str = brew_bin.to_string_lossy();
            let mut out = String::new();
            let res = runtime.run_cancellable_local_env(
                brew_str.as_ref(),
                &["autoremove", "--dry-run"],
                brew::NONINTERACTIVE_NO_AUTO_UPDATE_ENV,
                cancel,
                &mut |l| {
                    out.push_str(&l);
                    out.push('\n');
                },
            );
            if cancel.is_some_and(WorkCancel::is_cancelled) {
                return Err("cancelled".into());
            }
            match res {
                Ok(code) => {
                    let parsed = cleanup::parse_brew_autoremove_dry_run(&out);
                    if !parsed.is_empty() {
                        plan.items.extend(parsed);
                    } else if code != 0 {
                        plan.warnings
                            .push(format!("Homebrew cleanup probe exited with {code}"));
                    }
                }
                Err(err) => {
                    plan.warnings.push(format!("Homebrew cleanup probe: {err}"));
                }
            }
        }
    }

    plan.normalize();
    Ok(plan)
}

/// Execute a user-confirmed cleanup plan on the host and refresh remaining reclaimable items.
pub fn run_cleanup(
    plan: &cleanup::CleanupPlan,
    config: &SourceConfig,
    cancel: Option<&WorkCancel>,
    tx: Option<&Sender<WorkerMsg>>,
) -> CleanupRunOutcome {
    run_cleanup_with(&SystemRuntime, plan, config, cancel, tx)
}

/// Testable user-confirmed cleanup runner over an injected [`OrchestratorRuntime`].
///
/// Cleanup commands execute strictly locally without `--helper` / `pkexec`.
pub fn run_cleanup_with<R: OrchestratorRuntime>(
    runtime: &R,
    plan: &cleanup::CleanupPlan,
    config: &SourceConfig,
    cancel: Option<&WorkCancel>,
    tx: Option<&Sender<WorkerMsg>>,
) -> CleanupRunOutcome {
    let emit = |tag: &str, text: String| {
        if let Some(sender) = tx {
            line(sender, tag, text);
        }
    };

    if plan.is_empty() {
        return CleanupRunOutcome::default();
    }
    if cancel.is_some_and(WorkCancel::is_cancelled) {
        return CleanupRunOutcome {
            cancelled: true,
            remaining_plan: plan.clone(),
            ..CleanupRunOutcome::default()
        };
    }

    let mut failed_steps = Vec::new();
    let mut cancelled = false;

    for (enabled, tag, label, user) in [
        (
            plan.has_flatpak_user(),
            cleanup::CleanupSource::FlatpakUser.console_tag(),
            cleanup::CleanupSource::FlatpakUser.label(),
            true,
        ),
        (
            plan.has_flatpak_system(),
            cleanup::CleanupSource::FlatpakSystem.console_tag(),
            cleanup::CleanupSource::FlatpakSystem.label(),
            false,
        ),
    ] {
        if !enabled {
            continue;
        }
        if cancel.is_some_and(WorkCancel::is_cancelled) {
            cancelled = true;
            break;
        }
        emit("orchestrator", format!("── Cleaning unused {label} ──"));
        let args = cleanup::flatpak_uninstall_unused_args(user);
        let res = runtime.run_cancellable_local_env(
            "flatpak",
            &args,
            &[],
            cancel,
            &mut |l| emit(tag, l),
        );
        if cancel.is_some_and(WorkCancel::is_cancelled) {
            cancelled = true;
            break;
        }
        match res {
            Ok(0) => {}
            Ok(code) => {
                let msg = format!("{label} exited with {code}");
                emit(tag, format!("error: {msg}"));
                failed_steps.push(msg);
            }
            Err(err) => {
                let msg = format!("{label}: {err}");
                emit(tag, format!("error: {msg}"));
                failed_steps.push(msg);
            }
        }
    }

    if !cancelled && plan.has_homebrew() {
        if cancel.is_some_and(WorkCancel::is_cancelled) {
            cancelled = true;
        } else if let Some(brew_bin) = runtime.brew_binary() {
            let tag = cleanup::CleanupSource::Homebrew.console_tag();
            emit("orchestrator", "── Cleaning unused Homebrew formulae ──".into());
            let brew_str = brew_bin.to_string_lossy();
            let res = runtime.run_cancellable_local_env(
                brew_str.as_ref(),
                &["autoremove"],
                brew::NONINTERACTIVE_NO_AUTO_UPDATE_ENV,
                cancel,
                &mut |l| emit(tag, l),
            );
            if cancel.is_some_and(WorkCancel::is_cancelled) {
                cancelled = true;
            } else {
                match res {
                    Ok(0) => {}
                    Ok(code) => {
                        let msg = format!("Homebrew exited with {code}");
                        emit(tag, format!("error: {msg}"));
                        failed_steps.push(msg);
                    }
                    Err(err) => {
                        let msg = format!("Homebrew: {err}");
                        emit(tag, format!("error: {msg}"));
                        failed_steps.push(msg);
                    }
                }
            }
        } else {
            failed_steps.push("Homebrew binary not found".into());
        }
    }

    let remaining_plan = if cancelled {
        plan.clone()
    } else {
        discover_cleanup_with(runtime, config, cancel).unwrap_or_default()
    };

    let remaining_keys: BTreeSet<(cleanup::CleanupSource, String)> = remaining_plan
        .items
        .iter()
        .map(|item| (item.source, item.id.clone()))
        .collect();
    let mut removed_count = 0usize;
    let mut reclaimed_bytes = 0u64;
    for item in &plan.items {
        if !remaining_keys.contains(&(item.source, item.id.clone())) {
            removed_count += 1;
            if let Some(bytes) = item.size_bytes {
                reclaimed_bytes = reclaimed_bytes.saturating_add(bytes);
            }
        }
    }

    CleanupRunOutcome {
        removed_count,
        reclaimed_bytes,
        failed_steps,
        cancelled,
        remaining_plan,
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
    rpm_ostree::cancel_active_transaction();
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

/// Whether Check requires opening our `--helper` polkit session up front.
/// Atomic OSTree hosts check zero-password via `rpm-ostree` and never use `--helper`.
pub fn check_requires_helper_auth(ostree_booted: bool, native_dnf: bool, config: &SourceConfig) -> bool {
    !ostree_booted && config.is_enabled(UpdateSource::Dnf) && !can_skip_check_auth_ui(native_dnf)
}

/// Whether Apply can bypass the helper wait screen (`dnf5daemon` prompts directly via polkit).
pub fn can_skip_apply_auth_ui(native_dnf: bool) -> bool {
    native_dnf || session_is_alive()
}

/// Whether Apply can skip our `--helper` auth prompt screen for this batch.
/// Only CLI `dnf` (`!native_dnf`) or non-download CLI `fwupdmgr` (`!native_fwupd`) need `--helper`;
/// `rpm-ostree` talks to `rpm-ostreed` Polkit directly without `--helper`.
pub fn apply_skips_helper_auth_ui(
    packages: &[Package],
    mode: ApplyMode,
    native_dnf: bool,
    native_fwupd: bool,
) -> bool {
    if session_is_alive() {
        return true;
    }
    let needs_dnf_helper = !native_dnf && packages.iter().any(|p| p.source == UpdateSource::Dnf);
    let needs_fw_helper = mode != ApplyMode::DownloadOnly
        && !native_fwupd
        && packages.iter().any(|p| p.source == UpdateSource::Firmware);
    !(needs_dnf_helper || needs_fw_helper)
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
    use std::collections::VecDeque;
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
        let default_cfg = SourceConfig::default();
        assert!(!default_cfg.is_enabled(UpdateSource::Distrobox));
        assert!(!default_cfg.is_enabled(UpdateSource::Toolbx));
        assert!(default_cfg.is_enabled(UpdateSource::Dnf));
        assert!(default_cfg.is_enabled(UpdateSource::Rustup));
        assert!(default_cfg.is_enabled(UpdateSource::Mise));
        assert!(default_cfg.is_enabled(UpdateSource::UvTools));
        assert!(default_cfg.is_enabled(UpdateSource::Pipx));

        let mut cfg = SourceConfig::all_enabled();
        assert!(!cfg.all_disabled());
        assert!(!cfg.all_disabled_for_host(true));
        let expected_classic = vec![
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::FlatpakSystem,
            UpdateSource::GnomeExtensions,
            UpdateSource::Homebrew,
            UpdateSource::Rustup,
            UpdateSource::Mise,
            UpdateSource::UvTools,
            UpdateSource::Pipx,
            UpdateSource::Distrobox,
            UpdateSource::Toolbx,
            UpdateSource::Firmware,
        ];
        assert_eq!(cfg.enabled_sources(), expected_classic);
        assert_eq!(cfg.enabled_sources_for_host(false), expected_classic);
        assert_eq!(
            cfg.enabled_sources_for_host(true),
            vec![
                UpdateSource::RpmOstree,
                UpdateSource::FlatpakUser,
                UpdateSource::FlatpakSystem,
                UpdateSource::GnomeExtensions,
                UpdateSource::Homebrew,
                UpdateSource::Rustup,
                UpdateSource::Mise,
                UpdateSource::UvTools,
                UpdateSource::Pipx,
                UpdateSource::Distrobox,
                UpdateSource::Toolbx,
                UpdateSource::Firmware,
            ]
        );

        cfg.disable("  FLATPAK  ");
        assert!(!cfg.is_enabled(UpdateSource::FlatpakUser));
        assert!(!cfg.is_enabled(UpdateSource::FlatpakSystem));
        assert!(cfg.is_enabled(UpdateSource::Dnf));
        assert!(cfg.is_enabled(UpdateSource::RpmOstree));
        assert!(cfg.is_enabled(UpdateSource::GnomeExtensions));
        assert!(cfg.is_enabled(UpdateSource::Homebrew));
        assert!(cfg.is_enabled(UpdateSource::Rustup));
        assert!(cfg.is_enabled(UpdateSource::Mise));
        assert!(cfg.is_enabled(UpdateSource::UvTools));
        assert!(cfg.is_enabled(UpdateSource::Pipx));
        assert!(cfg.is_enabled(UpdateSource::Distrobox));
        assert!(cfg.is_enabled(UpdateSource::Toolbx));
        assert!(cfg.is_enabled(UpdateSource::Firmware));
        assert_eq!(
            cfg.enabled_sources(),
            vec![
                UpdateSource::Dnf,
                UpdateSource::GnomeExtensions,
                UpdateSource::Homebrew,
                UpdateSource::Rustup,
                UpdateSource::Mise,
                UpdateSource::UvTools,
                UpdateSource::Pipx,
                UpdateSource::Distrobox,
                UpdateSource::Toolbx,
                UpdateSource::Firmware,
            ]
        );

        cfg.disable("dnf");
        cfg.disable("gnome-extensions");
        cfg.disable("homebrew");
        cfg.disable("rustup");
        cfg.disable("mise");
        cfg.disable("uv");
        cfg.disable("pipx");
        cfg.disable("distrobox");
        cfg.disable("toolbx");
        cfg.disable("firmware");
        assert!(cfg.all_disabled());
        assert!(cfg.enabled_sources().is_empty());
        assert!(!cfg.all_disabled_for_host(true));
        cfg.disable("rpm-ostree");
        assert!(cfg.all_disabled_for_host(true));

        cfg.enable("flatpak");
        assert!(!cfg.all_disabled());
        assert!(!cfg.all_disabled_for_host(true));
        assert_eq!(
            cfg.enabled_sources(),
            vec![UpdateSource::FlatpakUser, UpdateSource::FlatpakSystem]
        );

        let from_keys = SourceConfig::from_disabled_keys([" ".into(), "Firmware".into()]);
        assert!(!from_keys.is_enabled(UpdateSource::Firmware));
        assert!(from_keys.is_enabled(UpdateSource::Dnf));
        assert!(from_keys.is_enabled(UpdateSource::GnomeExtensions));
        assert!(from_keys.is_enabled(UpdateSource::Homebrew));
        assert!(from_keys.is_enabled(UpdateSource::Rustup));
        assert!(from_keys.is_enabled(UpdateSource::Mise));
        assert!(from_keys.is_enabled(UpdateSource::UvTools));
        assert!(from_keys.is_enabled(UpdateSource::Pipx));
        assert!(!from_keys.is_enabled(UpdateSource::Distrobox));
        assert!(!from_keys.is_enabled(UpdateSource::Toolbx));

        let with_opt_in = SourceConfig::from_prefs([], ["  Toolbx  ".into()]);
        assert!(with_opt_in.is_enabled(UpdateSource::Toolbx));
        assert!(!with_opt_in.is_enabled(UpdateSource::Distrobox));
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

        let ostree_defaults = default_source_availability_for_host(true);
        assert!(ostree_defaults.contains_key("rpm-ostree"));
        assert!(!ostree_defaults.contains_key("dnf"));
        assert!(ostree_defaults["rpm-ostree"].installed);

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
        for key in ["homebrew", "rustup", "mise", "uv", "pipx", "distrobox", "toolbx"] {
            assert!(!probed_no_brew.get(key).unwrap().installed);
            assert_eq!(probed_no_brew.get(key).unwrap().subtitle, "Not installed");
        }
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

        let probed_ostree = probe_source_availability_for_host_with(
            true,
            false,
            true,
            false,
            HostBinaryPaths {
                rpm_ostree_bin: Some(std::path::Path::new(rpm_ostree::RPM_OSTREE_DEFAULT_PATH)),
                rustup_bin: Some(std::path::Path::new("/home/alice/.cargo/bin/rustup")),
                mise_bin: Some(std::path::Path::new("/home/alice/.local/bin/mise")),
                uv_bin: Some(std::path::Path::new("/home/alice/.local/bin/uv")),
                pipx_bin: Some(std::path::Path::new("/home/alice/.local/bin/pipx")),
                distrobox_bin: Some(std::path::Path::new(containers::DISTROBOX_DEFAULT_PATH)),
                toolbox_bin: Some(std::path::Path::new(containers::TOOLBOX_DEFAULT_PATH)),
                ..Default::default()
            },
        );
        assert!(!probed_ostree.contains_key("dnf"));
        assert!(probed_ostree["rpm-ostree"].installed);
        assert_eq!(
            probed_ostree["rpm-ostree"].subtitle,
            "Installed (/usr/bin/rpm-ostree)"
        );
        assert!(probed_ostree["rustup"].installed);
        assert_eq!(
            probed_ostree["rustup"].subtitle,
            "Installed (/home/alice/.cargo/bin/rustup)"
        );
        assert!(probed_ostree["mise"].installed);
        assert!(probed_ostree["uv"].installed);
        assert!(probed_ostree["pipx"].installed);
        assert!(probed_ostree["distrobox"].installed);
        assert_eq!(
            probed_ostree["distrobox"].subtitle,
            "Updates installed containers when enabled"
        );
        assert!(probed_ostree["toolbx"].installed);
        assert_eq!(
            probed_ostree["toolbx"].subtitle,
            "Updates installed containers when enabled"
        );
        assert_eq!(
            probed_ostree["firmware"].subtitle,
            "Installed (fwupd D-Bus)"
        );
    }

    #[test]
    fn ostree_auth_ui_gates_skip_helper_except_cli_fwupd() {
        let cfg = SourceConfig::all_enabled();
        assert!(!check_requires_helper_auth(true, false, &cfg));
        assert!(check_requires_helper_auth(false, false, &cfg) || can_skip_check_auth_ui(false));
        assert!(!check_requires_helper_auth(false, true, &cfg));

        let ostree_pkg = Package::new_rpm_ostree(
            "upgraded",
            "mesa",
            "x86_64",
            Some("25.0".into()),
            "25.1",
            "fedora",
            "",
            None,
        );
        let fw_pkg = {
            let mut p = Package::new_dnf("UEFI dbx", "", "20241101", "fwupd");
            p.source = UpdateSource::Firmware;
            p
        };
        let dnf_pkg = Package::new_dnf("openssl", "x86_64", "3.2", "updates");

        assert!(apply_skips_helper_auth_ui(
            std::slice::from_ref(&ostree_pkg),
            ApplyMode::Live,
            false,
            false
        ));
        assert!(apply_skips_helper_auth_ui(
            &[ostree_pkg.clone(), fw_pkg.clone()],
            ApplyMode::Live,
            false,
            true
        ));
        assert!(!apply_skips_helper_auth_ui(
            &[ostree_pkg.clone(), fw_pkg.clone()],
            ApplyMode::Live,
            false,
            false
        ) || crate::privilege::session_is_alive());
        assert!(apply_skips_helper_auth_ui(
            &[ostree_pkg, fw_pkg],
            ApplyMode::DownloadOnly,
            false,
            false
        ));
        assert!(apply_skips_helper_auth_ui(
            &[dnf_pkg],
            ApplyMode::Live,
            true,
            false
        ));
    }

    #[test]
    fn source_config_custom_hooks_activate_only_when_enabled_valid_hooks_exist() {
        let mut cfg = SourceConfig::all_enabled();
        assert!(!cfg.is_enabled(UpdateSource::CustomHook));

        cfg = cfg.with_custom_hooks(vec![hooks::CustomHook {
            name: "Disabled".into(),
            check_command: None,
            apply_command: "echo skip".into(),
            enabled: false,
        }]);
        assert!(!cfg.is_enabled(UpdateSource::CustomHook));

        cfg = cfg.with_custom_hooks(vec![hooks::CustomHook {
            name: "Cargo update".into(),
            check_command: Some("cargo install-update --list".into()),
            apply_command: "cargo install-update -a".into(),
            enabled: true,
        }]);
        assert!(cfg.is_enabled(UpdateSource::CustomHook));

        cfg.disable("custom-hooks");
        assert!(!cfg.is_enabled(UpdateSource::CustomHook));
    }

    #[derive(Default)]
    struct CleanupAndHookTestRuntime {
        ensure_calls: std::sync::atomic::AtomicUsize,
        helper_calls: std::sync::atomic::AtomicUsize,
        unused_user: Mutex<VecDeque<NativeOutcome<Vec<flatpak::FlatpakListedRef>>>>,
        unused_system: Mutex<VecDeque<NativeOutcome<Vec<flatpak::FlatpakListedRef>>>>,
        local_cmds: Mutex<Vec<(String, Vec<String>)>>,
        local_results: Mutex<VecDeque<(i32, Vec<String>)>>,
        brew_path: Option<PathBuf>,
    }

    impl OrchestratorRuntime for CleanupAndHookTestRuntime {
        fn ensure_session(&self) -> Result<(), SessionError> {
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn run_helper(
            &self,
            _cmd: HelperCommand,
            _on_line: &mut dyn FnMut(String),
        ) -> Result<i32, SessionError> {
            self.helper_calls.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }

        fn run_local(
            &self,
            _program: &str,
            _args: &[&str],
            _on_line: &mut dyn FnMut(String),
        ) -> Result<i32, String> {
            Ok(0)
        }

        fn run_cancellable_local(
            &self,
            program: &str,
            args: &[&str],
            on_line: &mut dyn FnMut(String),
        ) -> Result<i32, String> {
            self.local_cmds.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            let (code, lines) = self
                .local_results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or((0, Vec::new()));
            for l in lines {
                on_line(l);
            }
            Ok(code)
        }

        fn brew_binary(&self) -> Option<PathBuf> {
            self.brew_path.clone()
        }

        fn query_flatpak(&self, _user: bool) -> Result<flatpak::FlatpakCheck, String> {
            Ok(flatpak::FlatpakCheck {
                packages: Vec::new(),
                warning: None,
            })
        }

        fn query_flatpak_unused(
            &self,
            user: bool,
        ) -> NativeOutcome<Vec<flatpak::FlatpakListedRef>> {
            let queue = if user {
                &self.unused_user
            } else {
                &self.unused_system
            };
            queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| NativeOutcome::Handled(Ok(Vec::new())))
        }
    }

    #[test]
    fn cleanup_discovery_and_execution_never_call_helper_and_refresh_state() {
        let rt = CleanupAndHookTestRuntime {
            brew_path: Some(PathBuf::from("/home/linuxbrew/.linuxbrew/bin/brew")),
            ..Default::default()
        };
        let fp_ref = flatpak::FlatpakListedRef {
            formatted_ref: "runtime/org.gnome.Platform/x86_64/45".into(),
            name: "GNOME Platform".into(),
            arch: "x86_64".into(),
            branch: "45".into(),
            version: "45".into(),
            origin: "flathub".into(),
            summary: None,
            installed_size_bytes: Some(800_000_000),
        };
        rt.unused_user
            .lock()
            .unwrap()
            .push_back(NativeOutcome::Handled(Ok(vec![fp_ref])));
        rt.unused_user
            .lock()
            .unwrap()
            .push_back(NativeOutcome::Handled(Ok(Vec::new())));
        rt.unused_system
            .lock()
            .unwrap()
            .push_back(NativeOutcome::Handled(Ok(Vec::new())));
        rt.unused_system
            .lock()
            .unwrap()
            .push_back(NativeOutcome::Handled(Ok(Vec::new())));

        let mut local = rt.local_results.lock().unwrap();
        // 1) Initial discovery: brew autoremove --dry-run
        local.push_back((0, vec!["==> Would autoremove 1 unneeded formulae:".into(), "gdbm (2.0 MB)".into()]));
        // 2) Cleanup apply: flatpak uninstall --user --unused -y --noninteractive
        local.push_back((0, vec!["Uninstalling runtime/org.gnome.Platform/x86_64/45".into()]));
        // 3) Cleanup apply: brew autoremove
        local.push_back((0, vec!["Uninstalling gdbm...".into()]));
        // 4) Post-cleanup refresh: brew autoremove --dry-run (empty)
        local.push_back((0, vec!["==> No unneeded formulae to autoremove.".into()]));
        drop(local);

        let cfg = SourceConfig::default();
        let plan = discover_cleanup_with(&rt, &cfg, None).expect("cleanup discovery");
        assert_eq!(plan.len(), 2);
        assert!(plan.known_reclaimable_bytes() >= 802_000_000);

        let (tx, rx) = async_channel::unbounded();
        let outcome = run_cleanup_with(&rt, &plan, &cfg, None, Some(&tx));
        assert!(!outcome.cancelled);
        assert!(outcome.failed_steps.is_empty());
        assert_eq!(outcome.removed_count, 2);
        assert!(outcome.remaining_plan.is_empty());
        assert_eq!(rt.ensure_calls.load(Ordering::SeqCst), 0);
        assert_eq!(rt.helper_calls.load(Ordering::SeqCst), 0);
        let mut logged = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let WorkerMsg::ConsoleLine { text, .. } = msg {
                logged.push(text);
            }
        }
        assert!(logged.iter().any(|l| l.contains("Uninstalling gdbm")));
    }

    #[test]
    fn custom_hooks_check_apply_and_cancellation_run_strictly_unprivileged() {
        let rt = CleanupAndHookTestRuntime::default();
        {
            let mut local = rt.local_results.lock().unwrap();
            // Checked hook 1 reports updates.
            local.push_back((0, vec!["bat 0.24 -> 0.25".into()]));
            // Checked hook 2 reports up-to-date.
            local.push_back((0, Vec::new()));
            // Apply checked hook 1 + unverified hook 3.
            local.push_back((0, vec!["Updated bat 100%".into()]));
            local.push_back((0, vec!["Synced dotfiles".into()]));
        }

        let cfg = SourceConfig::from_prefs(
            [
                "dnf",
                "rpm-ostree",
                "flatpak",
                "gnome-extensions",
                "homebrew",
                "rustup",
                "mise",
                "uv",
                "pipx",
                "firmware",
            ]
            .into_iter()
            .map(String::from),
            [],
        )
        .with_custom_hooks(vec![
            hooks::CustomHook {
                name: "Cargo tools".into(),
                check_command: Some("cargo install-update --list".into()),
                apply_command: "sudo dnf upgrade -y && cargo install-update -a".into(),
                enabled: true,
            },
            hooks::CustomHook {
                name: "Neovim plugins".into(),
                check_command: Some("nvim --headless +Lazy check".into()),
                apply_command: "nvim --headless +Lazy sync +qa".into(),
                enabled: true,
            },
            hooks::CustomHook {
                name: "Chezmoi".into(),
                check_command: None,
                apply_command: "chezmoi update".into(),
                enabled: true,
            },
        ]);

        let (tx, rx) = async_channel::unbounded();
        let cancel = WorkCancel::new();
        execute_check_all_with_config(&tx, &rt, &cancel, &cfg);
        let mut checked_pkgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let WorkerMsg::Event(StateEvent::CheckComplete { packages, .. }) = msg {
                checked_pkgs = packages;
            }
        }
        assert_eq!(checked_pkgs.len(), 2);
        assert!(checked_pkgs.iter().all(|p| p.source == UpdateSource::CustomHook));
        assert_eq!(rt.ensure_calls.load(Ordering::SeqCst), 0);
        assert_eq!(rt.helper_calls.load(Ordering::SeqCst), 0);

        let (tx2, rx2) = async_channel::unbounded();
        execute_apply_all_with_mode_and_config(
            &tx2,
            checked_pkgs,
            ApplyMode::Live,
            &rt,
            &cancel,
            &cfg,
        );
        let mut apply_done = false;
        let mut hook_apply_ok = false;
        while let Ok(msg) = rx2.try_recv() {
            match msg {
                WorkerMsg::Event(StateEvent::BackendApplyDone {
                    source: UpdateSource::CustomHook,
                    success,
                    ..
                }) => {
                    assert!(success);
                    hook_apply_ok = true;
                }
                WorkerMsg::Event(StateEvent::ApplyComplete { needs_reboot }) => {
                    assert!(!needs_reboot);
                    apply_done = true;
                }
                _ => {}
            }
        }
        assert!(hook_apply_ok);
        assert!(apply_done);
        assert_eq!(rt.ensure_calls.load(Ordering::SeqCst), 0);
        assert_eq!(rt.helper_calls.load(Ordering::SeqCst), 0);
    }
}
