//! Native `dnf5daemon-server` (`org.rpm.dnf.v0`) D-Bus integration.
//!
//! Opens unprivileged `SessionManager` sessions with an **empty** option map `{}`
//! (never overriding `config`, so `org.rpm.dnf.v0.base.Config.override` `auth_admin`
//! polkit prompts never fire during update checks or history queries). Runs selective
//! and multi-mode transactions (`Live`, `LiveAndReboot`, `OfflineOnReboot`,
//! `DownloadOnly`) with live MB/s throughput telemetry, 4-stage progress signals,
//! and download-phase `Goal.cancel()` support.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::future::{poll_fn, Future};
use std::io::{self, Read};
use std::os::fd::{FromRawFd, OwnedFd as StdOwnedFd};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Instant;

use serde::Deserialize;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, Proxy};
use zbus::export::futures_core::Stream;
use zbus::message::Type as MessageType;
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedObjectPath, OwnedValue, Value};
use zbus::{MatchRule, MessageStream};

use crate::model::{
    AdvisoryKind, ApplyMode, Package, PackageAdvisory, PackageStatus, UpdateSource,
};
use crate::orchestrator::WorkCancel;

use super::dnf::{package_implies_reboot, strip_to_package_name};
use super::fwupd_dbus::{FwupdHistoryEntry, FwupdUpdateState};
use super::logind::is_dbus_unavailable;
use super::{NativeOutcome, ProgressHint};

pub const DNF_DEST: &str = "org.rpm.dnf.v0";
pub const DNF_ROOT_PATH: &str = "/org/rpm/dnf/v0";
pub const SESSION_MANAGER_IFACE: &str = "org.rpm.dnf.v0.SessionManager";
pub const BASE_IFACE: &str = "org.rpm.dnf.v0.Base";
pub const RPM_IFACE: &str = "org.rpm.dnf.v0.rpm.Rpm";
pub const ADVISORY_IFACE: &str = "org.rpm.dnf.v0.Advisory";
pub const GOAL_IFACE: &str = "org.rpm.dnf.v0.Goal";
pub const HISTORY_IFACE: &str = "org.rpm.dnf.v0.History";
pub const OFFLINE_IFACE: &str = "org.rpm.dnf.v0.Offline";

pub const LIST_PACKAGE_ATTRS: &[&str] = &[
    "name",
    "epoch",
    "version",
    "release",
    "arch",
    "repo_id",
    "download_size",
    "install_size",
    "summary",
    "evr",
];

pub const ADVISORY_ATTRS: &[&str] = &[
    "advisoryid",
    "name",
    "title",
    "type",
    "severity",
    "description",
    "references",
    "collections",
];

pub const HISTORY_TRANSACTION_ATTRS: &[&str] = &[
    "id",
    "start",
    "end",
    "user_id",
    "description",
    "status",
    "releasever",
    "comment",
];

pub const HISTORY_PACKAGE_ATTRS: &[&str] = &[
    "name",
    "epoch",
    "version",
    "release",
    "arch",
    "evr",
    "repo_id",
    "action",
    "reason",
];

#[derive(Clone)]
struct ActiveDnfSession {
    path: String,
    cancelled: Arc<AtomicBool>,
    in_transaction: Arc<AtomicBool>,
}

static ACTIVE_DNF_SESSION: Mutex<Option<ActiveDnfSession>> = Mutex::new(None);

fn set_active_session(session: Option<ActiveDnfSession>) {
    let mut guard = ACTIVE_DNF_SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = session;
}

/// Request `Goal.cancel()` on an in-flight download transaction when still cancelable.
pub fn cancel_active_transaction() -> bool {
    let session = {
        ACTIVE_DNF_SESSION
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    };
    let Some(session) = session else {
        return false;
    };
    if session.in_transaction.load(Ordering::SeqCst) {
        return false;
    }
    session.cancelled.store(true, Ordering::SeqCst);
    cancel_goal_by_path(&session.path).unwrap_or(false)
}

fn cancel_goal_by_path(session_path: &str) -> Result<bool, String> {
    let conn = Connection::system().map_err(|e| format!("System D-Bus unavailable: {e}"))?;
    let proxy = Proxy::new(&conn, DNF_DEST, session_path, GOAL_IFACE)
        .map_err(|e| format!("Goal proxy failed: {e}"))?;
    let (ok, err_msg): (bool, String) = proxy
        .call("cancel", &())
        .map_err(|e| format!("Goal.cancel failed: {e}"))?;
    if !ok && !err_msg.trim().is_empty() {
        return Err(err_msg);
    }
    Ok(ok)
}

/// Options passed to `SessionManager.open_session`.
///
/// MUST remain completely empty (`{}`), never passing `"config"` overrides,
/// because `org.rpm.dnf.v0.policy` maps `base.Config.override` to `auth_admin`.
pub fn open_session_options() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}

/// Options passed to `Goal.resolve`.
pub fn resolve_options() -> HashMap<&'static str, Value<'static>> {
    let mut opts = HashMap::new();
    opts.insert("allow_erasing", Value::Bool(false));
    opts.insert("interactive", Value::Bool(false));
    opts
}

/// Options passed to `Goal.do_transaction`.
pub fn do_transaction_options(mode: ApplyMode) -> HashMap<&'static str, Value<'static>> {
    let mut opts = HashMap::new();
    opts.insert(
        "offline",
        Value::Bool(mode == ApplyMode::OfflineOnReboot),
    );
    opts.insert(
        "downloadonly",
        Value::Bool(mode == ApplyMode::DownloadOnly),
    );
    opts.insert("interactive", Value::Bool(true));
    opts
}

/// True when `org.rpm.dnf.v0` is running or activatable on the system D-Bus.
pub fn is_available() -> bool {
    let Ok(conn) = Connection::system() else {
        return false;
    };
    let Ok(dbus) = DBusProxy::new(&conn) else {
        return false;
    };
    let name: zbus::names::BusName<'_> = match DNF_DEST.try_into() {
        Ok(name) => name,
        Err(_) => return false,
    };
    if dbus.name_has_owner(name).unwrap_or(false) {
        return true;
    }
    dbus.list_activatable_names()
        .map(|names| names.iter().any(|n| n.as_str() == DNF_DEST))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DnfRpmRecord {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub epoch: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub repo_id: String,
    #[serde(default)]
    pub download_size: Option<u64>,
    #[serde(default)]
    pub install_size: Option<u64>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub evr: String,
}

impl DnfRpmRecord {
    pub fn display_evr(&self) -> String {
        format_display_evr(
            &self.evr,
            self.epoch.as_deref(),
            self.version.as_deref(),
            self.release.as_deref(),
        )
    }
}

/// Format an EVR string cleanly for UI display, stripping redundant `"0:"` epoch.
pub fn format_display_evr(
    evr: &str,
    epoch: Option<&str>,
    version: Option<&str>,
    release: Option<&str>,
) -> String {
    let trimmed = evr.trim();
    if !trimmed.is_empty() {
        return trimmed.strip_prefix("0:").unwrap_or(trimmed).to_string();
    }
    let ver = version.map(str::trim).unwrap_or("");
    if ver.is_empty() {
        return String::new();
    }
    let mut base = match release.map(str::trim).filter(|s| !s.is_empty()) {
        Some(rel) => format!("{ver}-{rel}"),
        None => ver.to_string(),
    };
    if let Some(ep) = epoch.map(str::trim).filter(|s| !s.is_empty() && *s != "0") {
        base = format!("{ep}:{base}");
    }
    base
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DnfAdvisoryReference {
    #[serde(default)]
    pub id: String,
    #[serde(default, alias = "type")]
    pub ref_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DnfAdvisoryPackage {
    #[serde(default, alias = "n")]
    pub name: String,
    #[serde(default, alias = "a")]
    pub arch: String,
    #[serde(default, alias = "e")]
    pub epoch: Option<String>,
    #[serde(default, alias = "v")]
    pub version: Option<String>,
    #[serde(default, alias = "r")]
    pub release: Option<String>,
    #[serde(default)]
    pub nevra: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DnfAdvisoryRecord {
    #[serde(default)]
    pub advisoryid: Option<i64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, alias = "type")]
    pub kind: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub references: Vec<DnfAdvisoryReference>,
    #[serde(default)]
    pub packages: Vec<DnfAdvisoryPackage>,
}

impl DnfAdvisoryRecord {
    pub fn advisory_id(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        self.advisoryid
            .map(|id| id.to_string())
            .unwrap_or_default()
    }

    pub fn to_package_advisory(&self) -> PackageAdvisory {
        let kind = match self.kind.trim().to_ascii_lowercase().as_str() {
            "security" => AdvisoryKind::Security,
            "bugfix" => AdvisoryKind::Bugfix,
            "newpackage" => AdvisoryKind::NewPackage,
            _ => AdvisoryKind::Enhancement,
        };
        let severity = self
            .severity
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none"))
            .map(ToString::to_string);

        let mut cves = Vec::new();
        let mut bugzillas = Vec::new();
        for reference in &self.references {
            let id = reference.id.trim();
            if id.is_empty() {
                continue;
            }
            let rtype = reference.ref_type.trim().to_ascii_lowercase();
            if rtype == "cve" || id.to_ascii_uppercase().starts_with("CVE-") {
                let norm = id.to_ascii_uppercase();
                if !cves.contains(&norm) {
                    cves.push(norm);
                }
            } else if rtype == "bugzilla" || rtype == "rhbz" {
                let norm = if id.to_ascii_uppercase().starts_with("RHBZ") {
                    id.to_string()
                } else {
                    format!("RHBZ#{id}")
                };
                if !bugzillas.contains(&norm) {
                    bugzillas.push(norm);
                }
            }
        }

        let description = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);

        PackageAdvisory {
            id: self.advisory_id(),
            kind,
            severity,
            cves,
            bugzillas,
            description,
        }
    }

    pub fn applies_to(&self, name: &str, arch: &str) -> bool {
        self.packages.iter().any(|pkg| {
            pkg.name == name && (pkg.arch.is_empty() || arch.is_empty() || pkg.arch == arch)
        })
    }
}

/// Compute signed installed size delta `(new - installed)` safely.
pub fn compute_install_size_delta(new_size: Option<u64>, installed_size: Option<u64>) -> Option<i64> {
    let diff = i128::from(new_size?) - i128::from(installed_size?);
    Some(diff.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// Parse NDJSON lines written to `org.rpm.dnf.v0.rpm.Rpm.list_fd`.
pub fn parse_list_fd_ndjson(raw: &str) -> Result<Vec<DnfRpmRecord>, String> {
    let mut out = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rec: DnfRpmRecord = serde_json::from_str(trimmed)
            .map_err(|e| format!("Invalid Rpm.list_fd JSON on line {}: {e}", idx + 1))?;
        out.push(rec);
    }
    Ok(out)
}

/// Assemble enriched [`Package`] models from `upgrades`, `installed` lookup, and `advisories`.
pub fn packages_from_dnf_dbus(
    upgrades: &[DnfRpmRecord],
    installed: &[DnfRpmRecord],
    advisories: &[DnfAdvisoryRecord],
) -> Vec<Package> {
    let mut installed_exact: HashMap<(&str, &str), &DnfRpmRecord> =
        HashMap::with_capacity(installed.len());
    let mut installed_by_name: HashMap<&str, &DnfRpmRecord> =
        HashMap::with_capacity(installed.len());
    for rec in installed {
        installed_exact.insert((rec.name.as_str(), rec.arch.as_str()), rec);
        installed_by_name.entry(rec.name.as_str()).or_insert(rec);
    }

    let mut seen_ids = HashSet::new();
    let mut packages = Vec::with_capacity(upgrades.len());
    for upg in upgrades {
        let version = upg.display_evr();
        if upg.name.is_empty() || version.is_empty() {
            continue;
        }
        let repo = if upg.repo_id.is_empty() {
            "updates".to_string()
        } else {
            upg.repo_id.clone()
        };
        let mut pkg = Package::new_dnf(upg.name.clone(), upg.arch.clone(), version, repo);
        if !seen_ids.insert(pkg.id.clone()) {
            continue;
        }
        pkg.size_bytes = upg.download_size;
        pkg.summary = upg
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);

        let matched_inst = installed_exact
            .get(&(upg.name.as_str(), upg.arch.as_str()))
            .copied()
            .or_else(|| installed_by_name.get(upg.name.as_str()).copied());
        if let Some(inst) = matched_inst {
            let old = inst.display_evr();
            if !old.is_empty() {
                pkg.old_version = Some(old);
            }
            pkg.install_size_delta = compute_install_size_delta(upg.install_size, inst.install_size);
        }

        let mut attached_ids = HashSet::new();
        for adv in advisories {
            if adv.applies_to(&upg.name, &upg.arch) {
                let parsed = adv.to_package_advisory();
                if parsed.id.is_empty() || attached_ids.insert(parsed.id.clone()) {
                    pkg.advisories.push(parsed);
                }
            }
        }
        packages.push(pkg);
    }
    packages
}

/// Convert selected DNF packages into `rpm.Rpm.upgrade` NEVRA/name.arch specs.
pub fn upgrade_specs_from_packages(packages: &[Package]) -> Vec<String> {
    let mut specs = Vec::new();
    let mut seen = HashSet::new();
    for pkg in packages.iter().filter(|p| p.source == UpdateSource::Dnf) {
        let spec = if pkg.arch.trim().is_empty() {
            pkg.name.trim().to_string()
        } else {
            format!("{}.{}", pkg.name.trim(), pkg.arch.trim())
        };
        if !spec.is_empty() && seen.insert(spec.clone()) {
            specs.push(spec);
        }
    }
    specs
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnfApplyOutcome {
    pub needs_reboot: bool,
    pub offline_scheduled: bool,
    pub download_only: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThroughputSample {
    pub bytes_per_sec: f64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

/// Structured D-Bus signals emitted on `org.rpm.dnf.v0.Base` and `org.rpm.dnf.v0.rpm.Rpm`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "signal")]
pub enum DnfDbusSignal {
    #[serde(rename = "download_add_new")]
    DownloadAddNew {
        download_id: String,
        description: String,
        total_to_download: i64,
    },
    #[serde(rename = "download_progress")]
    DownloadProgress {
        download_id: String,
        total_to_download: i64,
        downloaded: i64,
        #[serde(default)]
        timestamp_ms: u64,
    },
    #[serde(rename = "download_end")]
    DownloadEnd {
        download_id: String,
        transfer_status: u32,
        #[serde(default)]
        message: String,
        #[serde(default)]
        timestamp_ms: u64,
    },
    #[serde(rename = "download_mirror_failure")]
    DownloadMirrorFailure {
        download_id: String,
        message: String,
        url: String,
        #[serde(default)]
        metadata: String,
    },
    #[serde(rename = "transaction_before_begin")]
    TransactionBeforeBegin { total: u64 },
    #[serde(rename = "transaction_transaction_start")]
    TransactionTransactionStart { total: u64 },
    #[serde(rename = "transaction_transaction_progress")]
    TransactionTransactionProgress { processed: u64, total: u64 },
    #[serde(rename = "transaction_transaction_stop")]
    TransactionTransactionStop { total: u64 },
    #[serde(rename = "transaction_elem_progress")]
    TransactionElemProgress {
        nevra: String,
        processed: u64,
        total: u64,
    },
    #[serde(rename = "transaction_action_start")]
    TransactionActionStart {
        nevra: String,
        action: u32,
        total: u64,
    },
    #[serde(rename = "transaction_action_progress")]
    TransactionActionProgress {
        nevra: String,
        processed: u64,
        total: u64,
    },
    #[serde(rename = "transaction_action_stop")]
    TransactionActionStop { nevra: String, total: u64 },
    #[serde(rename = "transaction_script_start")]
    TransactionScriptStart { nevra: String, scriptlet_type: u32 },
    #[serde(rename = "transaction_script_stop")]
    TransactionScriptStop {
        nevra: String,
        scriptlet_type: u32,
        return_code: u64,
    },
    #[serde(rename = "transaction_script_error")]
    TransactionScriptError {
        nevra: String,
        scriptlet_type: u32,
        return_code: u64,
    },
    #[serde(rename = "transaction_verify_start")]
    TransactionVerifyStart { total: u64 },
    #[serde(rename = "transaction_verify_progress")]
    TransactionVerifyProgress { processed: u64, total: u64 },
    #[serde(rename = "transaction_verify_stop")]
    TransactionVerifyStop { total: u64 },
    #[serde(rename = "transaction_unpack_error")]
    TransactionUnpackError { nevra: String },
    #[serde(rename = "transaction_after_complete")]
    TransactionAfterComplete { success: bool },
}

#[derive(Debug, Default, Clone)]
struct DownloadItemState {
    description: String,
    package_name: Option<String>,
    total_bytes: u64,
    downloaded_bytes: u64,
    finished: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct TrackedStep {
    pub hint: Option<ProgressHint>,
    pub log_line: Option<String>,
    pub draw_line: Option<String>,
    pub throughput: Option<ThroughputSample>,
    pub entered_transaction: bool,
    pub transaction_finished: Option<bool>,
}

/// Pure offline state machine converting `org.rpm.dnf.v0` signals into
/// UI progress hints, 1-second MB/s sparkline samples, and cancel boundaries.
#[derive(Debug, Clone)]
pub struct DnfSignalTracker {
    in_transaction: bool,
    planned_download_total: u64,
    downloads: HashMap<String, DownloadItemState>,
    last_sample_ms: Option<u64>,
    last_sample_bytes: u64,
    emitted_initial_sample: bool,
    txn_total: u64,
}

impl DnfSignalTracker {
    pub fn new(planned_download_total: u64) -> Self {
        Self {
            in_transaction: false,
            planned_download_total,
            downloads: HashMap::new(),
            last_sample_ms: None,
            last_sample_bytes: 0,
            emitted_initial_sample: false,
            txn_total: 0,
        }
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    pub fn total_downloaded_bytes(&self) -> u64 {
        self.downloads.values().map(|d| d.downloaded_bytes).sum()
    }

    pub fn effective_download_total(&self) -> u64 {
        let observed: u64 = self.downloads.values().map(|d| d.total_bytes).sum();
        self.planned_download_total
            .max(observed)
            .max(self.total_downloaded_bytes())
    }

    pub fn apply(&mut self, sig: DnfDbusSignal) -> TrackedStep {
        match sig {
            DnfDbusSignal::DownloadAddNew {
                download_id,
                description,
                total_to_download,
            } => {
                let total = total_to_download.max(0) as u64;
                let pkg = download_package_name(&download_id, &description);
                self.downloads.insert(
                    download_id,
                    DownloadItemState {
                        description: description.clone(),
                        package_name: pkg.clone(),
                        total_bytes: total,
                        downloaded_bytes: 0,
                        finished: false,
                    },
                );
                let (done, count) = self.download_counts();
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: pkg,
                        status: Some(PackageStatus::Downloading),
                        progress: Some(0.0),
                        phase_label: Some("Downloading packages".into()),
                        stage_current: (count > 0).then_some(done),
                        stage_total: (count > 0).then_some(count),
                    }),
                    log_line: (!description.trim().is_empty())
                        .then(|| format!("Downloading {description}")),
                    ..Default::default()
                }
            }
            DnfDbusSignal::DownloadProgress {
                download_id,
                total_to_download,
                downloaded,
                timestamp_ms,
            } => {
                let total = total_to_download.max(0) as u64;
                let dl = downloaded.max(0) as u64;
                let pkg = {
                    let entry = self
                        .downloads
                        .entry(download_id.clone())
                        .or_insert_with(|| DownloadItemState {
                            package_name: download_package_name(&download_id, ""),
                            ..Default::default()
                        });
                    if total > 0 {
                        entry.total_bytes = total;
                    }
                    entry.downloaded_bytes = dl.max(entry.downloaded_bytes);
                    entry.package_name.clone()
                };
                let frac = (total > 0).then(|| (dl as f64 / total as f64).clamp(0.0, 1.0));
                let (done, count) = self.download_counts();
                let draw_line = pkg.as_ref().map(|name| match frac {
                    Some(f) => format!("Downloading {name} {:.0}%", f * 100.0),
                    None => format!("Downloading {name}"),
                });
                let throughput = self.sample_throughput(timestamp_ms, false);
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: pkg,
                        status: Some(PackageStatus::Downloading),
                        progress: frac,
                        phase_label: Some("Downloading packages".into()),
                        stage_current: (count > 0).then_some(done),
                        stage_total: (count > 0).then_some(count),
                    }),
                    draw_line,
                    throughput,
                    ..Default::default()
                }
            }
            DnfDbusSignal::DownloadEnd {
                download_id,
                transfer_status,
                message,
                timestamp_ms,
            } => {
                let (pkg, desc) = {
                    let entry = self.downloads.entry(download_id).or_default();
                    entry.finished = true;
                    if transfer_status <= 1 && entry.total_bytes > 0 {
                        entry.downloaded_bytes = entry.total_bytes;
                    }
                    (entry.package_name.clone(), entry.description.clone())
                };
                let (done, count) = self.download_counts();
                let throughput = self.sample_throughput(timestamp_ms, true);
                let log_line = if transfer_status >= 2 && !message.trim().is_empty() {
                    Some(format!("Download failed ({desc}): {message}"))
                } else {
                    None
                };
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: pkg,
                        status: Some(PackageStatus::Downloading),
                        progress: (transfer_status <= 1).then_some(1.0),
                        phase_label: Some("Downloading packages".into()),
                        stage_current: (count > 0).then_some(done),
                        stage_total: (count > 0).then_some(count),
                    }),
                    log_line,
                    throughput,
                    ..Default::default()
                }
            }
            DnfDbusSignal::DownloadMirrorFailure {
                message,
                url,
                ..
            } => TrackedStep {
                log_line: Some(format!("Mirror error ({url}): {message}")),
                ..Default::default()
            },
            DnfDbusSignal::TransactionBeforeBegin { total } => {
                self.in_transaction = true;
                self.txn_total = total;
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: None,
                        status: None,
                        progress: Some(0.0),
                        phase_label: Some("Preparing transaction".into()),
                        stage_current: (total > 0).then_some(0),
                        stage_total: (total > 0).then_some(total as u32),
                    }),
                    log_line: Some(format!("Running RPM transaction ({total} items)…")),
                    entered_transaction: true,
                    ..Default::default()
                }
            }
            DnfDbusSignal::TransactionTransactionStart { total } => {
                self.in_transaction = true;
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: None,
                        status: None,
                        progress: Some(0.0),
                        phase_label: Some("Preparing transaction".into()),
                        stage_current: None,
                        stage_total: (total > 0).then_some(total as u32),
                    }),
                    ..Default::default()
                }
            }
            DnfDbusSignal::TransactionTransactionProgress { processed, total } => TrackedStep {
                hint: Some(ProgressHint {
                    package_name: None,
                    status: None,
                    progress: (total > 0).then(|| (processed as f64 / total as f64).clamp(0.0, 1.0)),
                    phase_label: Some("Preparing transaction".into()),
                    stage_current: None,
                    stage_total: None,
                }),
                ..Default::default()
            },
            DnfDbusSignal::TransactionTransactionStop { .. } => TrackedStep {
                hint: Some(ProgressHint {
                    package_name: None,
                    status: None,
                    progress: Some(1.0),
                    phase_label: Some("Preparing transaction".into()),
                    stage_current: None,
                    stage_total: None,
                }),
                ..Default::default()
            },
            DnfDbusSignal::TransactionElemProgress {
                nevra,
                processed,
                total,
            } => {
                self.in_transaction = true;
                if total > 0 {
                    self.txn_total = total;
                }
                let pkg = clean_nevra_name(&nevra);
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: pkg,
                        status: Some(PackageStatus::Installing),
                        progress: None,
                        phase_label: Some("Installing packages".into()),
                        stage_current: (total > 0).then_some(processed.min(total) as u32),
                        stage_total: (total > 0).then_some(total as u32),
                    }),
                    ..Default::default()
                }
            }
            DnfDbusSignal::TransactionActionStart { nevra, action, .. } => {
                self.in_transaction = true;
                let verb = rpm_action_verb(action);
                let pkg = clean_nevra_name(&nevra);
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: pkg,
                        status: Some(PackageStatus::Installing),
                        progress: Some(0.0),
                        phase_label: Some(rpm_action_phase(action).into()),
                        stage_current: None,
                        stage_total: None,
                    }),
                    log_line: Some(format!("{verb} {nevra}")),
                    ..Default::default()
                }
            }
            DnfDbusSignal::TransactionActionProgress {
                nevra,
                processed,
                total,
            } => {
                let pkg = clean_nevra_name(&nevra);
                let frac = (total > 0).then(|| (processed as f64 / total as f64).clamp(0.0, 1.0));
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: pkg,
                        status: Some(PackageStatus::Installing),
                        progress: frac,
                        phase_label: Some("Installing packages".into()),
                        stage_current: None,
                        stage_total: None,
                    }),
                    ..Default::default()
                }
            }
            DnfDbusSignal::TransactionActionStop { nevra, .. } => {
                let pkg = clean_nevra_name(&nevra);
                TrackedStep {
                    hint: Some(ProgressHint {
                        package_name: pkg,
                        status: Some(PackageStatus::Completed),
                        progress: Some(1.0),
                        phase_label: Some("Installing packages".into()),
                        stage_current: None,
                        stage_total: None,
                    }),
                    ..Default::default()
                }
            }
            DnfDbusSignal::TransactionScriptStart {
                nevra,
                scriptlet_type,
            } => TrackedStep {
                log_line: Some(format!(
                    "Running {} scriptlet for {nevra}",
                    scriptlet_type_label(scriptlet_type)
                )),
                ..Default::default()
            },
            DnfDbusSignal::TransactionScriptStop { .. } => TrackedStep::default(),
            DnfDbusSignal::TransactionScriptError {
                nevra,
                scriptlet_type,
                return_code,
            } => TrackedStep {
                log_line: Some(format!(
                    "Scriptlet error ({}, exit {return_code}) for {nevra}",
                    scriptlet_type_label(scriptlet_type)
                )),
                ..Default::default()
            },
            DnfDbusSignal::TransactionVerifyStart { total } => TrackedStep {
                hint: Some(ProgressHint {
                    package_name: None,
                    status: None,
                    progress: Some(0.0),
                    phase_label: Some("Verifying packages".into()),
                    stage_current: (total > 0).then_some(0),
                    stage_total: (total > 0).then_some(total as u32),
                }),
                log_line: Some("Verifying transaction packages…".into()),
                ..Default::default()
            },
            DnfDbusSignal::TransactionVerifyProgress { processed, total } => TrackedStep {
                hint: Some(ProgressHint {
                    package_name: None,
                    status: None,
                    progress: (total > 0).then(|| (processed as f64 / total as f64).clamp(0.0, 1.0)),
                    phase_label: Some("Verifying packages".into()),
                    stage_current: (total > 0).then_some(processed.min(total) as u32),
                    stage_total: (total > 0).then_some(total as u32),
                }),
                ..Default::default()
            },
            DnfDbusSignal::TransactionVerifyStop { total } => TrackedStep {
                hint: Some(ProgressHint {
                    package_name: None,
                    status: None,
                    progress: Some(1.0),
                    phase_label: Some("Verifying packages".into()),
                    stage_current: (total > 0).then_some(total as u32),
                    stage_total: (total > 0).then_some(total as u32),
                }),
                ..Default::default()
            },
            DnfDbusSignal::TransactionUnpackError { nevra } => TrackedStep {
                log_line: Some(format!("Unpack error: {nevra}")),
                ..Default::default()
            },
            DnfDbusSignal::TransactionAfterComplete { success } => TrackedStep {
                hint: success.then(|| ProgressHint {
                    package_name: None,
                    status: None,
                    progress: Some(1.0),
                    phase_label: Some("Complete".into()),
                    stage_current: (self.txn_total > 0).then_some(self.txn_total as u32),
                    stage_total: (self.txn_total > 0).then_some(self.txn_total as u32),
                }),
                log_line: Some(if success {
                    "Complete!".into()
                } else {
                    "RPM transaction finished with errors.".into()
                }),
                transaction_finished: Some(success),
                ..Default::default()
            },
        }
    }

    fn download_counts(&self) -> (u32, u32) {
        let pkg_entries: Vec<&DownloadItemState> = self
            .downloads
            .values()
            .filter(|d| d.package_name.is_some())
            .collect();
        let total = pkg_entries.len() as u32;
        let done = pkg_entries.iter().filter(|d| d.finished).count() as u32;
        (done, total)
    }

    fn sample_throughput(&mut self, timestamp_ms: u64, force_min_window: bool) -> Option<ThroughputSample> {
        let downloaded_bytes = self.total_downloaded_bytes();
        let Some(prev_ms) = self.last_sample_ms else {
            self.last_sample_ms = Some(timestamp_ms);
            self.last_sample_bytes = downloaded_bytes;
            return None;
        };
        let elapsed_ms = timestamp_ms.saturating_sub(prev_ms);
        let min_window = if force_min_window || !self.emitted_initial_sample {
            250
        } else {
            1000
        };
        if elapsed_ms < min_window {
            return None;
        }
        let delta_bytes = downloaded_bytes.saturating_sub(self.last_sample_bytes);
        if force_min_window && delta_bytes == 0 && self.emitted_initial_sample {
            return None;
        }
        let bytes_per_sec = (delta_bytes as f64) * 1000.0 / (elapsed_ms as f64);
        self.last_sample_ms = Some(timestamp_ms);
        self.last_sample_bytes = downloaded_bytes;
        self.emitted_initial_sample = true;
        Some(ThroughputSample {
            bytes_per_sec,
            downloaded_bytes,
            total_bytes: self.effective_download_total(),
        })
    }
}

fn download_package_name(download_id: &str, description: &str) -> Option<String> {
    if download_id.starts_with("repo:") {
        return None;
    }
    let candidate = if !description.trim().is_empty() {
        description.trim()
    } else {
        download_id
            .strip_prefix("package:")
            .unwrap_or(download_id)
            .trim()
    };
    let candidate = candidate.strip_suffix(".rpm").unwrap_or(candidate);
    if candidate.is_empty() || candidate.contains(' ') || !candidate.contains('-') {
        return None;
    }
    let stripped = strip_to_package_name(candidate);
    (!stripped.is_empty()).then_some(stripped)
}

fn clean_nevra_name(nevra: &str) -> Option<String> {
    let trimmed = nevra.trim();
    if trimmed.is_empty() {
        return None;
    }
    let stripped = strip_to_package_name(trimmed);
    (!stripped.is_empty()).then_some(stripped)
}

fn rpm_action_verb(action: u32) -> &'static str {
    match action {
        1 => "Installing",
        2 => "Upgrading",
        3 => "Downgrading",
        4 => "Reinstalling",
        5 | 6 => "Removing",
        7 => "Updating reason",
        _ => "Processing",
    }
}

fn rpm_action_phase(action: u32) -> &'static str {
    match action {
        5 | 6 => "Removing packages",
        _ => "Installing packages",
    }
}

fn scriptlet_type_label(code: u32) -> &'static str {
    match code {
        1 => "pre-install",
        2 => "post-install",
        3 => "pre-uninstall",
        4 => "post-uninstall",
        5 => "pre-transaction",
        6 => "post-transaction",
        7 => "trigger-pre-install",
        8 => "trigger-install",
        9 => "trigger-uninstall",
        10 => "trigger-post-uninstall",
        11 => "sysusers",
        12 => "preun-transaction",
        13 => "postun-transaction",
        _ => "package",
    }
}

/// Source category shown on Observatory's `Update History` page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySource {
    Dnf,
    Firmware,
}

impl HistorySource {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Dnf => "DNF",
            Self::Firmware => "FW",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Self::Dnf => "src-dnf",
            Self::Firmware => "src-firmware",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HistoryPackageChange {
    pub name: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub evr: String,
    #[serde(default)]
    pub original_evr: Option<String>,
    #[serde(default)]
    pub action: String,
}

impl HistoryPackageChange {
    pub fn display_version_change(&self) -> String {
        let to = format_display_evr(&self.evr, None, None, None);
        match self
            .original_evr
            .as_deref()
            .map(|old| format_display_evr(old, None, None, None))
            .filter(|old| !old.is_empty())
        {
            Some(from) if !to.is_empty() => format!("{from} → {to}"),
            _ => to,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DnfHistoryTransaction {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub start: i64,
    #[serde(default)]
    pub end: i64,
    #[serde(default)]
    pub user_id: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub upgraded: Vec<HistoryPackageChange>,
    #[serde(default)]
    pub installed: Vec<HistoryPackageChange>,
    #[serde(default)]
    pub removed: Vec<HistoryPackageChange>,
    #[serde(default)]
    pub downgraded: Vec<HistoryPackageChange>,
    #[serde(default)]
    pub reinstalled: Vec<HistoryPackageChange>,
}

/// Unified chronological timeline entry merging DNF5 transactions and `fwupd` firmware installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: String,
    pub source: HistorySource,
    pub timestamp: u64,
    pub title: String,
    pub description: String,
    pub status: String,
    pub succeeded: bool,
    pub changes: Vec<HistoryPackageChange>,
}

/// Merge DNF5 history transactions and `fwupd` history entries newest-first.
pub fn merge_history(
    dnf_txns: &[DnfHistoryTransaction],
    fwupd_entries: &[FwupdHistoryEntry],
) -> Vec<HistoryEntry> {
    let mut out = Vec::with_capacity(dnf_txns.len() + fwupd_entries.len());

    for txn in dnf_txns {
        let mut changes = Vec::new();
        for (verb, list) in [
            ("Upgraded", &txn.upgraded),
            ("Installed", &txn.installed),
            ("Downgraded", &txn.downgraded),
            ("Reinstalled", &txn.reinstalled),
            ("Removed", &txn.removed),
        ] {
            for item in list {
                let mut entry = item.clone();
                if entry.action.is_empty() {
                    entry.action = verb.to_string();
                }
                changes.push(entry);
            }
        }

        let ts = txn.end.max(txn.start).max(0) as u64;
        let total_changed = changes.len();
        let title = if let Some(first) = changes.first() {
            if total_changed == 1 {
                first.name.clone()
            } else {
                format!("{} + {} more", first.name, total_changed - 1)
            }
        } else if !txn.description.trim().is_empty() {
            txn.description.trim().to_string()
        } else {
            format!("Transaction #{}", txn.id)
        };
        let status = if txn.status.trim().is_empty() {
            "Ok".to_string()
        } else {
            txn.status.trim().to_string()
        };
        let succeeded = status.eq_ignore_ascii_case("ok");
        out.push(HistoryEntry {
            id: format!("dnf-txn-{}", txn.id),
            source: HistorySource::Dnf,
            timestamp: ts,
            title,
            description: txn.description.trim().to_string(),
            status,
            succeeded,
            changes,
        });
    }

    for fw in fwupd_entries {
        let ts = fw.modified.or(fw.created).unwrap_or(0);
        let new_ver = fw
            .target_version
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&fw.version);
        let old_ver = fw
            .target_version
            .as_deref()
            .filter(|s| !s.trim().is_empty() && *s != fw.version)
            .map(|_| fw.version.clone());
        let succeeded = matches!(
            fw.update_state,
            FwupdUpdateState::Success | FwupdUpdateState::NeedsReboot
        );
        let status = match fw.update_state {
            FwupdUpdateState::Success => "Ok",
            FwupdUpdateState::NeedsReboot => "Restart Required",
            FwupdUpdateState::Failed | FwupdUpdateState::FailedTransient => "Failed",
            FwupdUpdateState::Pending => "Pending",
            FwupdUpdateState::Unknown => "Unknown",
        }
        .to_string();
        let mut desc = fw.plugin.clone().unwrap_or_else(|| "LVFS firmware".into());
        if let Some(err) = fw.update_error.as_deref().filter(|s| !s.trim().is_empty()) {
            desc = format!("{desc} · {err}");
        }
        out.push(HistoryEntry {
            id: format!("fwupd-{}-{ts}", fw.device_id),
            source: HistorySource::Firmware,
            timestamp: ts,
            title: fw.name.clone(),
            description: desc,
            status,
            succeeded,
            changes: vec![HistoryPackageChange {
                name: fw.name.clone(),
                arch: String::new(),
                evr: new_ver.to_string(),
                original_evr: old_ver,
                action: "Firmware".to_string(),
            }],
        });
    }

    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id)));
    out
}

/// RAII wrapper ensuring every `SessionManager.open_session` call closes cleanly.
struct SessionGuard<'a> {
    conn: &'a Connection,
    path: OwnedObjectPath,
}

impl<'a> SessionGuard<'a> {
    fn open(conn: &'a Connection) -> Result<Self, NativeOutcome<()>> {
        let proxy = match Proxy::new(conn, DNF_DEST, DNF_ROOT_PATH, SESSION_MANAGER_IFACE) {
            Ok(p) => p,
            Err(e) if is_dbus_unavailable(&e) => {
                return Err(NativeOutcome::Unavailable(e.to_string()));
            }
            Err(e) => return Err(NativeOutcome::Handled(Err(format!("SessionManager proxy failed: {e}")))),
        };
        let opts = open_session_options();
        match proxy.call::<_, _, OwnedObjectPath>("open_session", &(opts,)) {
            Ok(path) => Ok(Self { conn, path }),
            Err(e) if is_dbus_unavailable(&e) => Err(NativeOutcome::Unavailable(e.to_string())),
            Err(e) => Err(NativeOutcome::Handled(Err(format!(
                "SessionManager.open_session failed: {e}"
            )))),
        }
    }

    fn path_str(&self) -> &str {
        self.path.as_str()
    }
}

impl Drop for SessionGuard<'_> {
    fn drop(&mut self) {
        if let Ok(proxy) = Proxy::new(self.conn, DNF_DEST, DNF_ROOT_PATH, SESSION_MANAGER_IFACE) {
            let _: Result<bool, _> = proxy.call("close_session", &(self.path.as_ref(),));
        }
    }
}

/// Zero-password update check over `org.rpm.dnf.v0`.
pub fn check_updates(on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
    let conn = match Connection::system() {
        Ok(c) => c,
        Err(e) => return NativeOutcome::Unavailable(format!("System D-Bus unavailable: {e}")),
    };
    let guard = match SessionGuard::open(&conn) {
        Ok(g) => g,
        Err(NativeOutcome::Unavailable(r)) => return NativeOutcome::Unavailable(r),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };

    let base_proxy = match Proxy::new(&conn, DNF_DEST, guard.path_str(), BASE_IFACE) {
        Ok(p) => p,
        Err(e) => return NativeOutcome::Handled(Err(format!("Base proxy failed: {e}"))),
    };
    on_line("Loading repository metadata via dnf5daemon…".into());
    match base_proxy.call::<_, _, bool>("read_all_repos", &()) {
        Ok(true) => {}
        Ok(false) => {
            return NativeOutcome::Handled(Err("dnf5daemon Base.read_all_repos returned false".into()));
        }
        Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
        Err(e) => {
            return NativeOutcome::Handled(Err(format!("Base.read_all_repos failed: {e}")));
        }
    }

    let rpm_proxy = match Proxy::new(&conn, DNF_DEST, guard.path_str(), RPM_IFACE) {
        Ok(p) => p,
        Err(e) => return NativeOutcome::Handled(Err(format!("Rpm proxy failed: {e}"))),
    };

    let upgrades = match query_rpm_records(&rpm_proxy, "upgrades", &[]) {
        Ok(u) => u,
        Err(e) => return NativeOutcome::Handled(Err(e)),
    };
    if upgrades.is_empty() {
        on_line("0 system package updates via dnf5daemon".into());
        return NativeOutcome::Handled(Ok(Vec::new()));
    }

    let mut pattern_names: Vec<String> = upgrades.iter().map(|r| r.name.clone()).collect();
    pattern_names.sort();
    pattern_names.dedup();
    let installed = query_rpm_records(&rpm_proxy, "installed", &pattern_names).unwrap_or_default();

    let advisories = query_advisories(&conn, guard.path_str()).unwrap_or_else(|e| {
        on_line(format!("Advisory lookup skipped: {e}"));
        Vec::new()
    });

    let packages = packages_from_dnf_dbus(&upgrades, &installed, &advisories);
    let sec_count = crate::model::count_security(&packages);
    on_line(format!(
        "{} system package update(s) via dnf5daemon ({sec_count} security)",
        packages.len()
    ));
    NativeOutcome::Handled(Ok(packages))
}

fn query_rpm_records(
    rpm_proxy: &Proxy<'_>,
    scope: &str,
    patterns: &[String],
) -> Result<Vec<DnfRpmRecord>, String> {
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("scope", Value::Str(scope.into()));
    opts.insert("package_attrs", Value::from(LIST_PACKAGE_ATTRS.to_vec()));
    if !patterns.is_empty() {
        let refs: Vec<&str> = patterns.iter().map(String::as_str).collect();
        opts.insert("patterns", Value::from(refs));
    }

    match query_rpm_list_fd(rpm_proxy, &opts) {
        Ok(recs) => Ok(recs),
        Err(fd_err) => {
            // Fallback to direct `list()` for small queries if FD transfer isn't supported.
            let raw: Vec<HashMap<String, OwnedValue>> = rpm_proxy
                .call("list", &(opts,))
                .map_err(|e| format!("Rpm.list_fd ({fd_err}) and Rpm.list ({e}) failed"))?;
            Ok(raw.iter().map(parse_rpm_dict).collect())
        }
    }
}

fn create_pipe() -> Result<(File, StdOwnedFd), String> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(format!("pipe2 failed: {}", io::Error::last_os_error()));
    }
    let read_file = unsafe { File::from_raw_fd(fds[0]) };
    let write_fd = unsafe { StdOwnedFd::from_raw_fd(fds[1]) };
    Ok((read_file, write_fd))
}

fn query_rpm_list_fd(
    rpm_proxy: &Proxy<'_>,
    opts: &HashMap<&str, Value<'_>>,
) -> Result<Vec<DnfRpmRecord>, String> {
    let (mut read_file, write_fd) = create_pipe()?;
    let reader_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = read_file.read_to_string(&mut buf);
        buf
    });

    let fd = OwnedFd::from(write_fd);
    let call_res: Result<(String,), _> = rpm_proxy.call("list_fd", &(opts, fd));
    // Note: `fd` was consumed cleanly inside `call` and dropped after sending, so the writer
    // side in this process is closed even if the call failed.
    let _transfer_id = call_res.map_err(|e| format!("Rpm.list_fd call failed: {e}"))?.0;

    let raw = reader_handle
        .join()
        .map_err(|_| "list_fd reader thread panicked".to_string())?;
    parse_list_fd_ndjson(&raw)
}

fn query_advisories(conn: &Connection, session_path: &str) -> Result<Vec<DnfAdvisoryRecord>, String> {
    let adv_proxy = Proxy::new(conn, DNF_DEST, session_path, ADVISORY_IFACE)
        .map_err(|e| format!("Advisory proxy failed: {e}"))?;
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("availability", Value::Str("updates".into()));
    opts.insert("advisory_attrs", Value::from(ADVISORY_ATTRS.to_vec()));
    let raw: Vec<HashMap<String, OwnedValue>> = adv_proxy
        .call("list", &(opts,))
        .map_err(|e| format!("Advisory.list failed: {e}"))?;
    Ok(raw.iter().map(parse_advisory_dict).collect())
}

/// Zero-password transaction history query over `org.rpm.dnf.v0.History`.
pub fn get_history(limit: i64) -> NativeOutcome<Vec<DnfHistoryTransaction>> {
    let conn = match Connection::system() {
        Ok(c) => c,
        Err(e) => return NativeOutcome::Unavailable(format!("System D-Bus unavailable: {e}")),
    };
    let guard = match SessionGuard::open(&conn) {
        Ok(g) => g,
        Err(NativeOutcome::Unavailable(r)) => return NativeOutcome::Unavailable(r),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };
    let proxy = match Proxy::new(&conn, DNF_DEST, guard.path_str(), HISTORY_IFACE) {
        Ok(p) => p,
        Err(e) => return NativeOutcome::Handled(Err(format!("History proxy failed: {e}"))),
    };

    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("limit", Value::I64(limit.max(1)));
    opts.insert("include_packages", Value::Bool(true));
    opts.insert(
        "transaction_attrs",
        Value::from(HISTORY_TRANSACTION_ATTRS.to_vec()),
    );
    opts.insert(
        "package_attrs",
        Value::from(HISTORY_PACKAGE_ATTRS.to_vec()),
    );

    match proxy.call::<_, _, Vec<HashMap<String, OwnedValue>>>("list", &(opts,)) {
        Ok(list) => NativeOutcome::Handled(Ok(list.iter().map(parse_dnf_history_dict).collect())),
        Err(e) if is_dbus_unavailable(&e) => NativeOutcome::Unavailable(e.to_string()),
        Err(e) => NativeOutcome::Handled(Err(format!("History.list failed: {e}"))),
    }
}

/// Execute selective or full DNF5 update transaction via `org.rpm.dnf.v0`.
pub fn apply_updates(
    packages: &[Package],
    mode: ApplyMode,
    cancel: &WorkCancel,
    on_line: &mut dyn FnMut(String),
    on_draw: &mut dyn FnMut(String),
    on_progress: &mut dyn FnMut(ProgressHint),
    on_throughput: &mut dyn FnMut(ThroughputSample),
) -> NativeOutcome<DnfApplyOutcome> {
    let specs = upgrade_specs_from_packages(packages);
    if specs.is_empty() {
        return NativeOutcome::Handled(Ok(DnfApplyOutcome {
            needs_reboot: false,
            offline_scheduled: false,
            download_only: mode == ApplyMode::DownloadOnly,
            cancelled: false,
        }));
    }
    if cancel.is_cancelled() {
        return NativeOutcome::Handled(Ok(DnfApplyOutcome {
            needs_reboot: false,
            offline_scheduled: false,
            download_only: mode == ApplyMode::DownloadOnly,
            cancelled: true,
        }));
    }

    let conn = match Connection::system() {
        Ok(c) => c,
        Err(e) => return NativeOutcome::Unavailable(format!("System D-Bus unavailable: {e}")),
    };
    let guard = match SessionGuard::open(&conn) {
        Ok(g) => g,
        Err(NativeOutcome::Unavailable(r)) => return NativeOutcome::Unavailable(r),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };
    let session_path = guard.path_str().to_string();

    let base_proxy = match Proxy::new(&conn, DNF_DEST, session_path.as_str(), BASE_IFACE) {
        Ok(p) => p,
        Err(e) => return NativeOutcome::Handled(Err(format!("Base proxy failed: {e}"))),
    };
    match base_proxy.call::<_, _, bool>("read_all_repos", &()) {
        Ok(true) => {}
        Ok(false) => {
            return NativeOutcome::Handled(Err("dnf5daemon Base.read_all_repos returned false".into()));
        }
        Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
        Err(e) => return NativeOutcome::Handled(Err(format!("Base.read_all_repos failed: {e}"))),
    }

    let rpm_proxy = match Proxy::new(&conn, DNF_DEST, session_path.as_str(), RPM_IFACE) {
        Ok(p) => p,
        Err(e) => return NativeOutcome::Handled(Err(format!("Rpm proxy failed: {e}"))),
    };
    let empty_opts: HashMap<&str, Value<'_>> = HashMap::new();
    let spec_refs: Vec<&str> = specs.iter().map(String::as_str).collect();
    if let Err(e) = rpm_proxy.call::<_, _, ()>("upgrade", &(spec_refs, empty_opts)) {
        if is_dbus_unavailable(&e) {
            return NativeOutcome::Unavailable(e.to_string());
        }
        return NativeOutcome::Handled(Err(format!("Rpm.upgrade failed: {e}")));
    }

    let goal_proxy = match Proxy::new(&conn, DNF_DEST, session_path.as_str(), GOAL_IFACE) {
        Ok(p) => p,
        Err(e) => return NativeOutcome::Handled(Err(format!("Goal proxy failed: {e}"))),
    };
    type ResolveItem = (
        String,
        String,
        String,
        HashMap<String, OwnedValue>,
        HashMap<String, OwnedValue>,
    );
    let (_items, resolve_result): (Vec<ResolveItem>, u32) =
        match goal_proxy.call("resolve", &(resolve_options(),)) {
            Ok(res) => res,
            Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
            Err(e) => return NativeOutcome::Handled(Err(format!("Goal.resolve failed: {e}"))),
        };
    if resolve_result == 2 {
        let problems: Vec<String> = goal_proxy
            .call("get_transaction_problems_string", &())
            .unwrap_or_default();
        let detail = if problems.is_empty() {
            "Failed to resolve DNF transaction dependencies.".into()
        } else {
            problems.join("\n")
        };
        on_line(detail.clone());
        return NativeOutcome::Handled(Err(detail));
    }

    let planned_bytes: u64 = packages
        .iter()
        .filter(|p| p.source == UpdateSource::Dnf)
        .filter_map(|p| p.size_bytes)
        .sum();
    let mut tracker = DnfSignalTracker::new(planned_bytes);

    let active_cancelled = Arc::new(AtomicBool::new(false));
    let active_in_txn = Arc::new(AtomicBool::new(false));
    set_active_session(Some(ActiveDnfSession {
        path: session_path.clone(),
        cancelled: active_cancelled.clone(),
        in_transaction: active_in_txn.clone(),
    }));
    {
        let cancel_path = session_path.clone();
        let flag_txn = active_in_txn.clone();
        let flag_can = active_cancelled.clone();
        cancel.on_cancel(move || {
            if !flag_txn.load(Ordering::SeqCst) {
                flag_can.store(true, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let _ = cancel_goal_by_path(&cancel_path);
                });
            }
        });
    }

    on_progress(ProgressHint {
        package_name: None,
        status: None,
        progress: Some(0.0),
        phase_label: Some("Downloading packages".into()),
        stage_current: None,
        stage_total: None,
    });

    let exec_res = run_transaction_with_signals(
        &session_path,
        mode,
        cancel,
        &active_cancelled,
        &active_in_txn,
        &mut tracker,
        on_line,
        on_draw,
        on_progress,
        on_throughput,
    );

    set_active_session(None);

    let was_cancelled = active_cancelled.load(Ordering::SeqCst)
        || (cancel.is_cancelled() && !tracker.in_transaction());

    match exec_res {
        Ok(after_ok) => {
            if was_cancelled {
                on_line("Download cancelled cleanly before RPM transaction.".into());
                return NativeOutcome::Handled(Ok(DnfApplyOutcome {
                    needs_reboot: false,
                    offline_scheduled: false,
                    download_only: mode == ApplyMode::DownloadOnly,
                    cancelled: true,
                }));
            }
            if !after_ok {
                return NativeOutcome::Handled(Err("RPM transaction failed.".into()));
            }

            let mut offline_scheduled = false;
            if mode == ApplyMode::OfflineOnReboot {
                if let Err(e) = finalize_offline_transaction(&conn, &session_path, on_line) {
                    return NativeOutcome::Handled(Err(e));
                }
                offline_scheduled = true;
            }

            let needs_reboot = if mode == ApplyMode::DownloadOnly {
                false
            } else if mode == ApplyMode::OfflineOnReboot {
                true
            } else {
                packages
                    .iter()
                    .any(|p| p.source == UpdateSource::Dnf && package_implies_reboot(&p.name))
            };

            NativeOutcome::Handled(Ok(DnfApplyOutcome {
                needs_reboot,
                offline_scheduled,
                download_only: mode == ApplyMode::DownloadOnly,
                cancelled: false,
            }))
        }
        Err(err) => {
            if was_cancelled {
                on_line("Download cancelled cleanly before RPM transaction.".into());
                NativeOutcome::Handled(Ok(DnfApplyOutcome {
                    needs_reboot: false,
                    offline_scheduled: false,
                    download_only: mode == ApplyMode::DownloadOnly,
                    cancelled: true,
                }))
            } else {
                NativeOutcome::Handled(Err(err))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_transaction_with_signals(
    session_path: &str,
    mode: ApplyMode,
    cancel: &WorkCancel,
    active_cancelled: &Arc<AtomicBool>,
    active_in_txn: &Arc<AtomicBool>,
    tracker: &mut DnfSignalTracker,
    on_line: &mut dyn FnMut(String),
    on_draw: &mut dyn FnMut(String),
    on_progress: &mut dyn FnMut(ProgressHint),
    on_throughput: &mut dyn FnMut(ThroughputSample),
) -> Result<bool, String> {
    let start = Instant::now();
    let mut after_complete_ok: Option<bool> = None;

    let ctx = glib::MainContext::new();
    let call_res = ctx.block_on(async {
        let conn = zbus::Connection::system()
            .await
            .map_err(|e| format!("System D-Bus connection failed: {e}"))?;
        let goal_proxy = zbus::Proxy::new(&conn, DNF_DEST, session_path, GOAL_IFACE)
            .await
            .map_err(|e| format!("Goal async proxy failed: {e}"))?;

        let base_rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(BASE_IFACE)
            .map_err(|e| e.to_string())?
            .build()
            .to_owned();
        let rpm_rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(RPM_IFACE)
            .map_err(|e| e.to_string())?
            .build()
            .to_owned();

        let mut base_stream = MessageStream::for_match_rule(base_rule, &conn, None)
            .await
            .map_err(|e| format!("Could not subscribe to Base signals: {e}"))?;
        let mut rpm_stream = MessageStream::for_match_rule(rpm_rule, &conn, None)
            .await
            .map_err(|e| format!("Could not subscribe to Rpm signals: {e}"))?;

        let txn_opts = do_transaction_options(mode);
        let txn_args = (txn_opts,);
        let mut do_fut = Box::pin(goal_proxy.call::<_, _, ()>("do_transaction", &txn_args));
        let mut cancel_sent = false;

        poll_fn(|cx| {
            if cancel.is_cancelled()
                && !active_in_txn.load(Ordering::SeqCst)
                && !cancel_sent
            {
                cancel_sent = true;
                active_cancelled.store(true, Ordering::SeqCst);
                let cancel_path = session_path.to_string();
                std::thread::spawn(move || {
                    let _ = cancel_goal_by_path(&cancel_path);
                });
            }

            while let Poll::Ready(Some(msg_res)) = Pin::new(&mut base_stream).poll_next(cx) {
                if let Ok(msg) = msg_res {
                    let ts_ms = start.elapsed().as_millis() as u64;
                    if let Some(sig) = decode_dnf_dbus_signal(&msg, session_path, ts_ms) {
                        dispatch_tracked_step(
                            tracker.apply(sig),
                            active_in_txn,
                            &mut after_complete_ok,
                            on_line,
                            on_draw,
                            on_progress,
                            on_throughput,
                        );
                    }
                }
            }

            while let Poll::Ready(Some(msg_res)) = Pin::new(&mut rpm_stream).poll_next(cx) {
                if let Ok(msg) = msg_res {
                    let ts_ms = start.elapsed().as_millis() as u64;
                    if let Some(sig) = decode_dnf_dbus_signal(&msg, session_path, ts_ms) {
                        dispatch_tracked_step(
                            tracker.apply(sig),
                            active_in_txn,
                            &mut after_complete_ok,
                            on_line,
                            on_draw,
                            on_progress,
                            on_throughput,
                        );
                    }
                }
            }

            match do_fut.as_mut().poll(cx) {
                Poll::Ready(res) => Poll::Ready(res.map_err(|e| e.to_string())),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    });

    call_res?;
    Ok(after_complete_ok.unwrap_or(true))
}

fn dispatch_tracked_step(
    step: TrackedStep,
    active_in_txn: &AtomicBool,
    after_complete_ok: &mut Option<bool>,
    on_line: &mut dyn FnMut(String),
    on_draw: &mut dyn FnMut(String),
    on_progress: &mut dyn FnMut(ProgressHint),
    on_throughput: &mut dyn FnMut(ThroughputSample),
) {
    if step.entered_transaction {
        active_in_txn.store(true, Ordering::SeqCst);
    }
    if let Some(ok) = step.transaction_finished {
        *after_complete_ok = Some(ok);
    }
    if let Some(line) = step.log_line {
        on_line(line);
    }
    if let Some(draw) = step.draw_line {
        on_draw(draw);
    }
    if let Some(sample) = step.throughput {
        on_throughput(sample);
    }
    if let Some(hint) = step.hint {
        on_progress(hint);
    }
}

fn finalize_offline_transaction(
    conn: &Connection,
    session_path: &str,
    on_line: &mut dyn FnMut(String),
) -> Result<(), String> {
    let proxy = Proxy::new(conn, DNF_DEST, session_path, OFFLINE_IFACE)
        .map_err(|e| format!("Offline proxy failed: {e}"))?;

    let (pending, status_map): (bool, HashMap<String, OwnedValue>) =
        proxy.call("get_status", &()).unwrap_or((false, HashMap::new()));
    let state_str = status_map
        .get("status")
        .and_then(value_as_string)
        .unwrap_or_default();

    let mut interactive_opts = HashMap::new();
    interactive_opts.insert("interactive", Value::Bool(true));

    if !pending || state_str != "ready" {
        let (scheduled, msg): (bool, String) = proxy
            .call("schedule_for_next_boot", &(interactive_opts.clone(),))
            .map_err(|e| format!("Offline.schedule_for_next_boot failed: {e}"))?;
        if !scheduled {
            return Err(if msg.trim().is_empty() {
                "Offline.schedule_for_next_boot returned false".into()
            } else {
                msg
            });
        }
    }

    let (ok, msg): (bool, String) = proxy
        .call(
            "set_finish_action_with_options",
            &("reboot", interactive_opts),
        )
        .map_err(|e| format!("Offline.set_finish_action_with_options failed: {e}"))?;
    if !ok {
        on_line(format!("Offline finish action note: {msg}"));
    }
    on_line("Offline system update scheduled for next restart.".into());
    Ok(())
}

fn decode_dnf_dbus_signal(
    msg: &zbus::Message,
    expected_session: &str,
    ts_ms: u64,
) -> Option<DnfDbusSignal> {
    let hdr = msg.header();
    let iface = hdr.interface()?.as_str();
    let member = hdr.member()?.as_str();
    let body = msg.body();

    match (iface, member) {
        (BASE_IFACE, "download_add_new") => {
            let (sess, download_id, description, total_to_download): (
                OwnedObjectPath,
                String,
                String,
                i64,
            ) = body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::DownloadAddNew {
                download_id,
                description,
                total_to_download,
            })
        }
        (BASE_IFACE, "download_progress") => {
            let (sess, download_id, total_to_download, downloaded): (
                OwnedObjectPath,
                String,
                i64,
                i64,
            ) = body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::DownloadProgress {
                download_id,
                total_to_download,
                downloaded,
                timestamp_ms: ts_ms,
            })
        }
        (BASE_IFACE, "download_end") => {
            let (sess, download_id, transfer_status, message): (
                OwnedObjectPath,
                String,
                u32,
                String,
            ) = body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::DownloadEnd {
                download_id,
                transfer_status,
                message,
                timestamp_ms: ts_ms,
            })
        }
        (BASE_IFACE, "download_mirror_failure") => {
            let (sess, download_id, message, url, metadata): (
                OwnedObjectPath,
                String,
                String,
                String,
                String,
            ) = body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::DownloadMirrorFailure {
                download_id,
                message,
                url,
                metadata,
            })
        }
        (RPM_IFACE, "transaction_before_begin") => {
            let (sess, total): (OwnedObjectPath, u64) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionBeforeBegin { total })
        }
        (RPM_IFACE, "transaction_transaction_start") => {
            let (sess, total): (OwnedObjectPath, u64) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionTransactionStart { total })
        }
        (RPM_IFACE, "transaction_transaction_progress") => {
            let (sess, processed, total): (OwnedObjectPath, u64, u64) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionTransactionProgress { processed, total })
        }
        (RPM_IFACE, "transaction_transaction_stop") => {
            let (sess, total): (OwnedObjectPath, u64) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionTransactionStop { total })
        }
        (RPM_IFACE, "transaction_elem_progress") => {
            let (sess, nevra, processed, total): (OwnedObjectPath, String, u64, u64) =
                body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::TransactionElemProgress {
                nevra,
                processed,
                total,
            })
        }
        (RPM_IFACE, "transaction_action_start") => {
            let (sess, nevra, action, total): (OwnedObjectPath, String, u32, u64) =
                body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::TransactionActionStart {
                nevra,
                action,
                total,
            })
        }
        (RPM_IFACE, "transaction_action_progress") => {
            let (sess, nevra, processed, total): (OwnedObjectPath, String, u64, u64) =
                body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(
                DnfDbusSignal::TransactionActionProgress {
                    nevra,
                    processed,
                    total,
                },
            )
        }
        (RPM_IFACE, "transaction_action_stop") => {
            let (sess, nevra, total): (OwnedObjectPath, String, u64) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionActionStop { nevra, total })
        }
        (RPM_IFACE, "transaction_script_start") => {
            let (sess, nevra, scriptlet_type): (OwnedObjectPath, String, u32) =
                body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::TransactionScriptStart {
                nevra,
                scriptlet_type,
            })
        }
        (RPM_IFACE, "transaction_script_stop") => {
            let (sess, nevra, scriptlet_type, return_code): (OwnedObjectPath, String, u32, u64) =
                body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::TransactionScriptStop {
                nevra,
                scriptlet_type,
                return_code,
            })
        }
        (RPM_IFACE, "transaction_script_error") => {
            let (sess, nevra, scriptlet_type, return_code): (OwnedObjectPath, String, u32, u64) =
                body.deserialize().ok()?;
            (sess.as_str() == expected_session).then_some(DnfDbusSignal::TransactionScriptError {
                nevra,
                scriptlet_type,
                return_code,
            })
        }
        (RPM_IFACE, "transaction_verify_start") => {
            let (sess, total): (OwnedObjectPath, u64) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionVerifyStart { total })
        }
        (RPM_IFACE, "transaction_verify_progress") => {
            let (sess, processed, total): (OwnedObjectPath, u64, u64) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionVerifyProgress { processed, total })
        }
        (RPM_IFACE, "transaction_verify_stop") => {
            let (sess, total): (OwnedObjectPath, u64) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionVerifyStop { total })
        }
        (RPM_IFACE, "transaction_unpack_error") => {
            let (sess, nevra): (OwnedObjectPath, String) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionUnpackError { nevra })
        }
        (RPM_IFACE, "transaction_after_complete") => {
            let (sess, success): (OwnedObjectPath, bool) = body.deserialize().ok()?;
            (sess.as_str() == expected_session)
                .then_some(DnfDbusSignal::TransactionAfterComplete { success })
        }
        _ => None,
    }
}

pub fn parse_rpm_dict(dict: &HashMap<String, OwnedValue>) -> DnfRpmRecord {
    DnfRpmRecord {
        id: dict.get("id").and_then(value_as_i64),
        name: dict.get("name").and_then(value_as_string).unwrap_or_default(),
        epoch: dict.get("epoch").and_then(value_as_string),
        version: dict.get("version").and_then(value_as_string),
        release: dict.get("release").and_then(value_as_string),
        arch: dict.get("arch").and_then(value_as_string).unwrap_or_default(),
        repo_id: dict
            .get("repo_id")
            .and_then(value_as_string)
            .unwrap_or_default(),
        download_size: dict.get("download_size").and_then(value_as_u64),
        install_size: dict.get("install_size").and_then(value_as_u64),
        summary: dict.get("summary").and_then(value_as_string),
        evr: dict.get("evr").and_then(value_as_string).unwrap_or_default(),
    }
}

pub fn parse_advisory_dict(dict: &HashMap<String, OwnedValue>) -> DnfAdvisoryRecord {
    let mut references = Vec::new();
    if let Some(refs_val) = dict.get("references") {
        if let Ok(tuples) = Vec::<(String, String, String, String)>::try_from(
            refs_val
                .try_clone()
                .unwrap_or_else(|_| OwnedValue::from(0u8)),
        ) {
            for (id, ref_type, title, url) in tuples {
                references.push(DnfAdvisoryReference {
                    id,
                    ref_type,
                    title,
                    url,
                });
            }
        }
    }

    let mut packages = Vec::new();
    if let Some(cols_val) = dict.get("collections") {
        if let Ok(cols) = Vec::<HashMap<String, OwnedValue>>::try_from(
            cols_val
                .try_clone()
                .unwrap_or_else(|_| OwnedValue::from(0u8)),
        ) {
            for col in cols {
                if let Some(pkgs_val) = col.get("packages") {
                    if let Ok(raw_pkgs) = Vec::<HashMap<String, OwnedValue>>::try_from(
                        pkgs_val
                            .try_clone()
                            .unwrap_or_else(|_| OwnedValue::from(0u8)),
                    ) {
                        for pkg_dict in raw_pkgs {
                            let name = pkg_dict
                                .get("n")
                                .or_else(|| pkg_dict.get("name"))
                                .and_then(value_as_string)
                                .unwrap_or_default();
                            if name.is_empty() {
                                continue;
                            }
                            packages.push(DnfAdvisoryPackage {
                                name,
                                arch: pkg_dict
                                    .get("a")
                                    .or_else(|| pkg_dict.get("arch"))
                                    .and_then(value_as_string)
                                    .unwrap_or_default(),
                                epoch: pkg_dict
                                    .get("e")
                                    .or_else(|| pkg_dict.get("epoch"))
                                    .and_then(value_as_string),
                                version: pkg_dict
                                    .get("v")
                                    .or_else(|| pkg_dict.get("version"))
                                    .and_then(value_as_string),
                                release: pkg_dict
                                    .get("r")
                                    .or_else(|| pkg_dict.get("release"))
                                    .and_then(value_as_string),
                                nevra: pkg_dict.get("nevra").and_then(value_as_string),
                            });
                        }
                    }
                }
            }
        }
    }

    DnfAdvisoryRecord {
        advisoryid: dict.get("advisoryid").and_then(value_as_i64),
        name: dict.get("name").and_then(value_as_string).unwrap_or_default(),
        title: dict
            .get("title")
            .and_then(value_as_string)
            .unwrap_or_default(),
        kind: dict.get("type").and_then(value_as_string).unwrap_or_default(),
        severity: dict.get("severity").and_then(value_as_string),
        description: dict.get("description").and_then(value_as_string),
        references,
        packages,
    }
}

pub fn parse_dnf_history_dict(dict: &HashMap<String, OwnedValue>) -> DnfHistoryTransaction {
    DnfHistoryTransaction {
        id: dict.get("id").and_then(value_as_i64).unwrap_or(0),
        start: dict.get("start").and_then(value_as_i64).unwrap_or(0),
        end: dict.get("end").and_then(value_as_i64).unwrap_or(0),
        user_id: dict.get("user_id").and_then(value_as_u64).unwrap_or(0) as u32,
        description: dict
            .get("description")
            .and_then(value_as_string)
            .unwrap_or_default(),
        status: dict
            .get("status")
            .and_then(value_as_string)
            .unwrap_or_default(),
        upgraded: parse_history_pkg_list(dict.get("upgraded"), "Upgraded"),
        installed: parse_history_pkg_list(dict.get("installed"), "Installed"),
        removed: parse_history_pkg_list(dict.get("removed"), "Removed"),
        downgraded: parse_history_pkg_list(dict.get("downgraded"), "Downgraded"),
        reinstalled: parse_history_pkg_list(dict.get("reinstalled"), "Reinstalled"),
    }
}

fn parse_history_pkg_list(val: Option<&OwnedValue>, default_action: &str) -> Vec<HistoryPackageChange> {
    let Some(val) = val else {
        return Vec::new();
    };
    let Ok(items) = Vec::<HashMap<String, OwnedValue>>::try_from(
        val.try_clone().unwrap_or_else(|_| OwnedValue::from(0u8)),
    ) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| {
            let name = item.get("name").and_then(value_as_string)?;
            let evr = item
                .get("evr")
                .and_then(value_as_string)
                .unwrap_or_else(|| {
                    format_display_evr(
                        "",
                        item.get("epoch").and_then(value_as_string).as_deref(),
                        item.get("version").and_then(value_as_string).as_deref(),
                        item.get("release").and_then(value_as_string).as_deref(),
                    )
                });
            Some(HistoryPackageChange {
                name,
                arch: item.get("arch").and_then(value_as_string).unwrap_or_default(),
                evr,
                original_evr: item.get("original_evr").and_then(value_as_string),
                action: item
                    .get("action")
                    .and_then(value_as_string)
                    .unwrap_or_else(|| default_action.to_string()),
            })
        })
        .collect()
}

fn value_as_string(v: &OwnedValue) -> Option<String> {
    match &**v {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

fn value_as_i64(v: &OwnedValue) -> Option<i64> {
    match &**v {
        Value::I16(n) => Some(i64::from(*n)),
        Value::I32(n) => Some(i64::from(*n)),
        Value::I64(n) => Some(*n),
        Value::U8(n) => Some(i64::from(*n)),
        Value::U16(n) => Some(i64::from(*n)),
        Value::U32(n) => Some(i64::from(*n)),
        Value::U64(n) => i64::try_from(*n).ok(),
        _ => None,
    }
}

fn value_as_u64(v: &OwnedValue) -> Option<u64> {
    match &**v {
        Value::U8(n) => Some(u64::from(*n)),
        Value::U16(n) => Some(u64::from(*n)),
        Value::U32(n) => Some(u64::from(*n)),
        Value::U64(n) => Some(*n),
        Value::I16(n) => (*n >= 0).then_some(*n as u64),
        Value::I32(n) => (*n >= 0).then_some(*n as u64),
        Value::I64(n) => (*n >= 0).then_some(*n as u64),
        _ => None,
    }
}

/// Expose unused-path compile sanity in non-cfg tests (`ObjectPath` sanity helper).
#[allow(dead_code)]
fn _valid_object_path(path: &str) -> bool {
    ObjectPath::try_from(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct CheckFixtureDoc {
        upgrades: Vec<DnfRpmRecord>,
        installed: Vec<DnfRpmRecord>,
        advisories: Vec<DnfAdvisoryRecord>,
    }

    #[test]
    fn open_session_options_must_never_pass_config_overrides() {
        let opts = open_session_options();
        assert!(opts.is_empty(), "open_session({{}}) must stay empty to avoid base.Config.override");
        assert!(!opts.contains_key("config"));

        let res = resolve_options();
        assert_eq!(res.get("allow_erasing"), Some(&Value::Bool(false)));

        let live = do_transaction_options(ApplyMode::Live);
        assert_eq!(live.get("offline"), Some(&Value::Bool(false)));
        assert_eq!(live.get("downloadonly"), Some(&Value::Bool(false)));
        assert_eq!(live.get("interactive"), Some(&Value::Bool(true)));

        let offline = do_transaction_options(ApplyMode::OfflineOnReboot);
        assert_eq!(offline.get("offline"), Some(&Value::Bool(true)));
        assert_eq!(offline.get("downloadonly"), Some(&Value::Bool(false)));

        let dl_only = do_transaction_options(ApplyMode::DownloadOnly);
        assert_eq!(dl_only.get("offline"), Some(&Value::Bool(false)));
        assert_eq!(dl_only.get("downloadonly"), Some(&Value::Bool(true)));
    }

    #[test]
    fn list_fd_ndjson_installed_crossref_and_advisory_attachment() {
        let ndjson = concat!(
            "{\"id\":501,\"name\":\"openssl\",\"epoch\":\"0\",\"version\":\"3.2.2\",\"release\":\"3.fc44\",\"arch\":\"x86_64\",\"repo_id\":\"updates\",\"download_size\":2400000,\"install_size\":5200000,\"summary\":\"Utilities from the general purpose cryptography library\",\"evr\":\"0:3.2.2-3.fc44\"}\n",
            "{\"id\":502,\"name\":\"kernel-core\",\"epoch\":\"0\",\"version\":\"6.14.12\",\"release\":\"300.fc44\",\"arch\":\"x86_64\",\"repo_id\":\"updates\",\"download_size\":18000000,\"install_size\":96000000,\"summary\":\"The Linux kernel\",\"evr\":\"0:6.14.12-300.fc44\"}\n"
        );
        let upgrades = parse_list_fd_ndjson(ndjson).expect("ndjson parses");
        assert_eq!(upgrades.len(), 2);

        let installed = vec![
            DnfRpmRecord {
                id: Some(10),
                name: "openssl".into(),
                epoch: Some("0".into()),
                version: Some("3.2.2".into()),
                release: Some("1.fc44".into()),
                arch: "x86_64".into(),
                repo_id: "@System".into(),
                download_size: Some(0),
                install_size: Some(5_000_000),
                summary: None,
                evr: "0:3.2.2-1.fc44".into(),
            },
            DnfRpmRecord {
                id: Some(11),
                name: "kernel-core".into(),
                epoch: Some("0".into()),
                version: Some("6.14.11".into()),
                release: Some("300.fc44".into()),
                arch: "x86_64".into(),
                repo_id: "@System".into(),
                download_size: Some(0),
                install_size: Some(95_000_000),
                summary: None,
                evr: "0:6.14.11-300.fc44".into(),
            },
        ];

        let advisories = vec![DnfAdvisoryRecord {
            advisoryid: Some(9001),
            name: "FEDORA-2026-84a1b2c3".into(),
            title: "openssl-3.2.2-3.fc44".into(),
            kind: "security".into(),
            severity: Some("Important".into()),
            description: Some("Fixes TLS 1.3 buffer over-read".into()),
            references: vec![
                DnfAdvisoryReference {
                    id: "CVE-2026-2401".into(),
                    ref_type: "cve".into(),
                    title: "CVE-2026-2401".into(),
                    url: "https://nvd.nist.gov/vuln/detail/CVE-2026-2401".into(),
                },
                DnfAdvisoryReference {
                    id: "2458001".into(),
                    ref_type: "bugzilla".into(),
                    title: "openssl bug".into(),
                    url: "https://bugzilla.redhat.com/2458001".into(),
                },
            ],
            packages: vec![DnfAdvisoryPackage {
                name: "openssl".into(),
                arch: "x86_64".into(),
                epoch: Some("0".into()),
                version: Some("3.2.2".into()),
                release: Some("3.fc44".into()),
                nevra: None,
            }],
        }];

        let pkgs = packages_from_dnf_dbus(&upgrades, &installed, &advisories);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].old_version.as_deref(), Some("3.2.2-1.fc44"));
        assert_eq!(pkgs[0].version, "3.2.2-3.fc44");
        assert_eq!(pkgs[0].install_size_delta, Some(200_000));
        assert_eq!(
            pkgs[0].summary.as_deref(),
            Some("Utilities from the general purpose cryptography library")
        );
        assert_eq!(pkgs[0].advisories.len(), 1);
        assert_eq!(pkgs[0].advisories[0].kind, AdvisoryKind::Security);
        assert_eq!(pkgs[0].advisories[0].cves, vec!["CVE-2026-2401"]);
        assert_eq!(pkgs[0].advisories[0].bugzillas, vec!["RHBZ#2458001"]);
        assert!(pkgs[0].has_security_advisory());
        assert!(!pkgs[1].has_security_advisory());
        assert_eq!(
            upgrade_specs_from_packages(&pkgs),
            vec!["openssl.x86_64", "kernel-core.x86_64"]
        );
    }

    #[test]
    fn signal_tracker_maps_downloads_throughput_stages_and_cancel_boundary() {
        let mut tracker = DnfSignalTracker::new(4_000_000);
        assert!(!tracker.in_transaction());

        let s0 = tracker.apply(DnfDbusSignal::DownloadAddNew {
            download_id: "package:1".into(),
            description: "openssl-3.2.2-3.fc44.x86_64".into(),
            total_to_download: 4_000_000,
        });
        assert!(!s0.entered_transaction);
        assert_eq!(
            s0.hint.as_ref().and_then(|h| h.package_name.as_deref()),
            Some("openssl")
        );
        assert_eq!(
            s0.hint.as_ref().and_then(|h| h.status),
            Some(PackageStatus::Downloading)
        );

        let _ = tracker.apply(DnfDbusSignal::DownloadProgress {
            download_id: "package:1".into(),
            total_to_download: 4_000_000,
            downloaded: 0,
            timestamp_ms: 0,
        });
        let tick_1s = tracker.apply(DnfDbusSignal::DownloadProgress {
            download_id: "package:1".into(),
            total_to_download: 4_000_000,
            downloaded: 2_000_000,
            timestamp_ms: 1000,
        });
        let tp = tick_1s.throughput.expect("1s throughput sample");
        assert!((tp.bytes_per_sec - 2_000_000.0).abs() < 1.0);
        assert_eq!(tp.downloaded_bytes, 2_000_000);
        assert_eq!(tp.total_bytes, 4_000_000);

        let end = tracker.apply(DnfDbusSignal::DownloadEnd {
            download_id: "package:1".into(),
            transfer_status: 0,
            message: String::new(),
            timestamp_ms: 2000,
        });
        assert_eq!(end.hint.as_ref().and_then(|h| h.progress), Some(1.0));
        assert!(!tracker.in_transaction());

        let prep = tracker.apply(DnfDbusSignal::TransactionBeforeBegin { total: 1 });
        assert!(prep.entered_transaction);
        assert!(tracker.in_transaction());
        assert_eq!(
            prep.hint.as_ref().and_then(|h| h.phase_label.as_deref()),
            Some("Preparing transaction")
        );

        let elem = tracker.apply(DnfDbusSignal::TransactionElemProgress {
            nevra: "openssl-0:3.2.2-3.fc44.x86_64".into(),
            processed: 0,
            total: 1,
        });
        assert_eq!(
            elem.hint.as_ref().and_then(|h| h.package_name.as_deref()),
            Some("openssl")
        );
        assert_eq!(
            elem.hint.as_ref().and_then(|h| h.status),
            Some(PackageStatus::Installing)
        );

        let act_stop = tracker.apply(DnfDbusSignal::TransactionActionStop {
            nevra: "openssl-0:3.2.2-3.fc44.x86_64".into(),
            total: 100,
        });
        assert_eq!(
            act_stop.hint.as_ref().and_then(|h| h.status),
            Some(PackageStatus::Completed)
        );

        let verify = tracker.apply(DnfDbusSignal::TransactionVerifyProgress {
            processed: 1,
            total: 1,
        });
        assert_eq!(
            verify.hint.as_ref().and_then(|h| h.phase_label.as_deref()),
            Some("Verifying packages")
        );
        assert_eq!(verify.hint.as_ref().and_then(|h| h.package_name.as_deref()), None);

        let done = tracker.apply(DnfDbusSignal::TransactionAfterComplete { success: true });
        assert_eq!(done.transaction_finished, Some(true));
        assert_eq!(
            done.hint.as_ref().and_then(|h| h.phase_label.as_deref()),
            Some("Complete")
        );
    }

    #[test]
    fn merge_history_orders_dnf_and_fwupd_chronologically_newest_first() {
        let dnf_txns = vec![
            DnfHistoryTransaction {
                id: 203,
                start: 1_795_224_500,
                end: 1_795_224_552,
                user_id: 1000,
                description: "dnf update -y".into(),
                status: "Ok".into(),
                upgraded: vec![HistoryPackageChange {
                    name: "exfatprogs".into(),
                    arch: "x86_64".into(),
                    evr: "1.4.4-1.fc44".into(),
                    original_evr: Some("1.4.3-1.fc44".into()),
                    action: String::new(),
                }],
                installed: vec![],
                removed: vec![],
                downgraded: vec![],
                reinstalled: vec![],
            },
            DnfHistoryTransaction {
                id: 201,
                start: 1_795_000_000,
                end: 1_795_000_010,
                user_id: 1000,
                description: "dnf install htop".into(),
                status: "Ok".into(),
                upgraded: vec![],
                installed: vec![HistoryPackageChange {
                    name: "htop".into(),
                    arch: "x86_64".into(),
                    evr: "3.3.0-1.fc44".into(),
                    original_evr: None,
                    action: String::new(),
                }],
                removed: vec![],
                downgraded: vec![],
                reinstalled: vec![],
            },
        ];

        let fw_entries = vec![FwupdHistoryEntry {
            device_id: "dev-uefi".into(),
            name: "System Firmware".into(),
            version: "0.1.32".into(),
            target_version: Some("0.1.33".into()),
            update_state: FwupdUpdateState::NeedsReboot,
            update_error: None,
            plugin: Some("uefi_capsule".into()),
            created: Some(1_795_100_000),
            modified: Some(1_795_150_000),
        }];

        let merged = merge_history(&dnf_txns, &fw_entries);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].source, HistorySource::Dnf);
        assert_eq!(merged[0].title, "exfatprogs");
        assert_eq!(
            merged[0].changes[0].display_version_change(),
            "1.4.3-1.fc44 → 1.4.4-1.fc44"
        );
        assert_eq!(merged[1].source, HistorySource::Firmware);
        assert_eq!(merged[1].title, "System Firmware");
        assert_eq!(merged[1].status, "Restart Required");
        assert!(merged[1].succeeded);
        assert_eq!(merged[1].changes[0].display_version_change(), "0.1.32 → 0.1.33");
        assert_eq!(merged[2].source, HistorySource::Dnf);
        assert_eq!(merged[2].title, "htop");
    }

    #[test]
    fn parses_real_dbus_check_fixture_shape() {
        let json = r#"{
            "upgrades": [
                {"name": "vim-enhanced", "arch": "x86_64", "repo_id": "updates", "download_size": 1800000, "install_size": 4100000, "summary": "Vi IMproved", "evr": "2:9.1.1-1.fc44"}
            ],
            "installed": [
                {"name": "vim-enhanced", "arch": "x86_64", "repo_id": "@System", "download_size": 0, "install_size": 4000000, "evr": "2:9.1.0-1.fc44"}
            ],
            "advisories": []
        }"#;
        let parsed: CheckFixtureDoc = serde_json::from_str(json).expect("json");
        let pkgs = packages_from_dnf_dbus(&parsed.upgrades, &parsed.installed, &parsed.advisories);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].old_version.as_deref(), Some("2:9.1.0-1.fc44"));
        assert_eq!(pkgs[0].version, "2:9.1.1-1.fc44");
    }

    #[test]
    #[ignore = "Requires live org.rpm.dnf.v0 (dnf5daemon-server) on host system D-Bus"]
    fn live_dnf5daemon_zero_auth_check_and_history_probe() {
        assert!(
            is_available(),
            "org.rpm.dnf.v0 should be available on Fedora 44 host"
        );
        let mut lines = Vec::new();
        match check_updates(&mut |l| lines.push(l)) {
            NativeOutcome::Handled(Ok(pkgs)) => {
                for p in pkgs.iter().take(5) {
                    assert_eq!(p.source, UpdateSource::Dnf);
                }
            }
            NativeOutcome::Handled(Err(e)) => panic!("dnf5daemon check error: {e}"),
            NativeOutcome::Unavailable(r) => panic!("unexpected Unavailable: {r}"),
        }
        let conn = Connection::system().expect("system bus");
        {
            let guard = SessionGuard::open(&conn).expect("zero-auth open_session");
            let rpm_proxy =
                Proxy::new(&conn, DNF_DEST, guard.path_str(), RPM_IFACE).expect("rpm proxy");
            let installed =
                query_rpm_records(&rpm_proxy, "installed", &["bash".into()]).expect("list_fd bash");
            assert!(
                installed
                    .iter()
                    .any(|r| r.name == "bash" && !r.display_evr().is_empty()),
                "expected installed bash via Rpm.list_fd, got {installed:?}"
            );
        }

        match get_history(5) {
            NativeOutcome::Handled(Ok(entries)) => {
                assert!(
                    !entries.is_empty() && entries.len() <= 5,
                    "expected 1..=5 transactions from History.list, got {}",
                    entries.len()
                );
            }
            NativeOutcome::Handled(Err(e)) => panic!("History.list error: {e}"),
            NativeOutcome::Unavailable(r) => panic!("unexpected Unavailable: {r}"),
        }
    }
}
