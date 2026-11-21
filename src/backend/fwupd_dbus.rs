//! Native `fwupd` (`org.freedesktop.fwupd`) D-Bus integration.
//!
//! Queries devices, available upgrades, release notes, history, and host/device
//! battery thresholds directly over system D-Bus, running trusted LVFS capsule
//! installs with live `Status` / `PercentageFull` / `DeviceRequest` progress.

use std::collections::HashMap;
use std::fs::{self, DirBuilder, File};
use std::future::{poll_fn, Future};
use std::os::fd::OwnedFd as StdOwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, Proxy};
use zbus::export::futures_core::Stream;
use zbus::fdo::PropertiesProxy as AsyncPropertiesProxy;
use zbus::zvariant::{OwnedFd, OwnedValue, Value};

use crate::model::{Package, PackageStatus, UpdateSource};

use super::logind::{self, is_dbus_unavailable};
use super::{NativeOutcome, ProgressHint};

pub const FWUPD_DEST: &str = "org.freedesktop.fwupd";
pub const FWUPD_PATH: &str = "/";
pub const FWUPD_IFACE: &str = "org.freedesktop.fwupd";

// Bitmask constants verified against Fwupd 2.0 GIR (`fwupd 2.1.9` on Fedora 44).
pub const DEVICE_FLAG_INTERNAL: u64 = 1 << 0;
pub const DEVICE_FLAG_UPDATABLE: u64 = 1 << 1;
pub const DEVICE_FLAG_REQUIRE_AC: u64 = 1 << 3;
pub const DEVICE_FLAG_LOCKED: u64 = 1 << 4;
pub const DEVICE_FLAG_NEEDS_REBOOT: u64 = 1 << 8;
pub const DEVICE_FLAG_NEEDS_SHUTDOWN: u64 = 1 << 17;

/// Sentinel used by `fwupd` when no battery telemetry exists (e.g. desktops).
pub const BATTERY_LEVEL_INVALID: u32 = 101;

static TEMP_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

/// `FwupdStatus` enum (`org.freedesktop.fwupd.Status` property).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FwupdStatus {
    Unknown = 0,
    Idle = 1,
    Loading = 2,
    Decompressing = 3,
    DeviceRestart = 4,
    DeviceWrite = 5,
    DeviceVerify = 6,
    Scheduling = 7,
    Downloading = 8,
    DeviceRead = 9,
    DeviceErase = 10,
    WaitingForAuth = 11,
    DeviceBusy = 12,
    Shutdown = 13,
    WaitingForUser = 14,
}

impl FwupdStatus {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Idle,
            2 => Self::Loading,
            3 => Self::Decompressing,
            4 => Self::DeviceRestart,
            5 => Self::DeviceWrite,
            6 => Self::DeviceVerify,
            7 => Self::Scheduling,
            8 => Self::Downloading,
            9 => Self::DeviceRead,
            10 => Self::DeviceErase,
            11 => Self::WaitingForAuth,
            12 => Self::DeviceBusy,
            13 => Self::Shutdown,
            14 => Self::WaitingForUser,
            _ => Self::Unknown,
        }
    }

    pub fn phase_label(self) -> Option<&'static str> {
        match self {
            Self::Unknown | Self::Idle => None,
            Self::Loading | Self::DeviceRead => Some("Preparing firmware"),
            Self::Decompressing => Some("Decompressing firmware"),
            Self::DeviceRestart => Some("Restarting device"),
            Self::DeviceWrite | Self::DeviceErase | Self::Scheduling => Some("Installing firmware"),
            Self::DeviceVerify => Some("Verifying firmware"),
            Self::Downloading => Some("Downloading firmware"),
            Self::WaitingForAuth => Some("Waiting for authorization"),
            Self::DeviceBusy | Self::WaitingForUser => Some("Waiting"),
            Self::Shutdown => Some("Shutting down"),
        }
    }

    pub fn package_status(self) -> Option<PackageStatus> {
        match self {
            Self::Downloading => Some(PackageStatus::Downloading),
            Self::DeviceWrite
            | Self::DeviceErase
            | Self::Decompressing
            | Self::DeviceVerify
            | Self::DeviceRestart
            | Self::Scheduling => Some(PackageStatus::Installing),
            _ => None,
        }
    }

    /// Map daemon status + `0.0..=100.0` `PercentageFull` into [`ProgressHint`].
    pub fn to_progress_hint(self, percentage_full: f64, device_name: Option<&str>) -> ProgressHint {
        let progress = if percentage_full.is_finite() && (0.0..=100.0).contains(&percentage_full) {
            Some(percentage_full / 100.0)
        } else {
            None
        };
        ProgressHint {
            package_name: device_name.map(ToString::to_string),
            status: self.package_status(),
            progress,
            phase_label: self.phase_label().map(ToString::to_string),
            stage_current: None,
            stage_total: None,
        }
    }
}

/// `FwupdUpdateState` enum (`org.freedesktop.fwupd` device/history `UpdateState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[repr(u32)]
#[serde(from = "u32")]
pub enum FwupdUpdateState {
    #[default]
    Unknown = 0,
    Pending = 1,
    Success = 2,
    Failed = 3,
    NeedsReboot = 4,
    FailedTransient = 5,
}

impl From<u32> for FwupdUpdateState {
    fn from(value: u32) -> Self {
        Self::from_u32(value)
    }
}

impl FwupdUpdateState {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Pending,
            2 => Self::Success,
            3 => Self::Failed,
            4 => Self::NeedsReboot,
            5 => Self::FailedTransient,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::NeedsReboot => "needs-reboot",
            Self::FailedTransient => "failed-transient",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FwupdHostPower {
    #[serde(default = "default_invalid_battery")]
    pub host_battery_level: u32,
    #[serde(default)]
    pub host_battery_threshold: u32,
    #[serde(default)]
    pub on_external_power: Option<bool>,
}

impl Default for FwupdHostPower {
    fn default() -> Self {
        Self {
            host_battery_level: BATTERY_LEVEL_INVALID,
            host_battery_threshold: 0,
            on_external_power: None,
        }
    }
}

fn default_invalid_battery() -> u32 {
    BATTERY_LEVEL_INVALID
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FwupdDevice {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub plugin: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub flags: u64,
    #[serde(default)]
    pub update_state: u32,
    #[serde(default)]
    pub update_error: Option<String>,
    #[serde(default)]
    pub battery_level: Option<u32>,
    #[serde(default)]
    pub battery_threshold: Option<u32>,
    #[serde(default)]
    pub created: Option<u64>,
    #[serde(default)]
    pub modified: Option<u64>,
    #[serde(default)]
    pub releases: Vec<FwupdRelease>,
}

impl FwupdDevice {
    pub fn is_updatable(&self) -> bool {
        (self.flags & DEVICE_FLAG_UPDATABLE) != 0 && (self.flags & DEVICE_FLAG_LOCKED) == 0
    }

    pub fn needs_reboot(&self) -> bool {
        (self.flags & (DEVICE_FLAG_NEEDS_REBOOT | DEVICE_FLAG_NEEDS_SHUTDOWN)) != 0
            || self.update_state == FwupdUpdateState::NeedsReboot as u32
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FwupdRelease {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, alias = "Size")]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub remote_id: Option<String>,
    #[serde(default)]
    pub locations: Vec<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub checksum: Option<String>,
    #[serde(default)]
    pub urgency: Option<u32>,
    #[serde(default)]
    pub created: Option<u64>,
    #[serde(default)]
    pub filename: Option<String>,
}

impl FwupdRelease {
    /// First HTTP(S) download URI from `Locations` or `Uri`.
    pub fn download_url(&self) -> Option<&str> {
        self.locations
            .iter()
            .map(String::as_str)
            .chain(self.uri.as_deref())
            .find(|u| u.starts_with("https://") || u.starts_with("http://"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FwupdRemote {
    #[serde(default)]
    pub remote_id: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub filename_cache: Option<String>,
    #[serde(default)]
    pub refresh_interval: u64,
    #[serde(default)]
    pub modification_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FwupdHistoryEntry {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub target_version: Option<String>,
    #[serde(default)]
    pub update_state: FwupdUpdateState,
    #[serde(default)]
    pub update_error: Option<String>,
    #[serde(default)]
    pub plugin: Option<String>,
    #[serde(default)]
    pub created: Option<u64>,
    #[serde(default)]
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FwupdDeviceRequest {
    #[serde(default, alias = "AppstreamId", alias = "DeviceId", alias = "RequestId")]
    pub id: String,
    #[serde(default, alias = "RequestKind")]
    pub kind: u32,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FwupdApplyOutcome {
    pub needs_reboot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatterySafetyStatus {
    Ok,
    Blocked {
        reason: String,
        battery_level: Option<u32>,
        battery_threshold: Option<u32>,
        requires_ac: bool,
    },
}

impl BatterySafetyStatus {
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Ok => None,
            Self::Blocked { reason, .. } => Some(reason.as_str()),
        }
    }
}

/// Strip simple AppStream HTML tags (`<p>`, `<li>`, etc.) and decode XML entities.
pub fn strip_html_tags(input: &str) -> String {
    let mut plain = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                if !plain.ends_with(char::is_whitespace) && !plain.is_empty() {
                    plain.push(' ');
                }
            }
            '>' => in_tag = false,
            _ if !in_tag => plain.push(ch),
            _ => {}
        }
    }
    let decoded = plain
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Evaluate pre-flight power safety using device/host `BatteryLevel` vs `BatteryThreshold`
/// and AC power state. `BATTERY_LEVEL_INVALID` (`101`) is never treated as low battery.
pub fn evaluate_battery_safety(
    device: &FwupdDevice,
    host_power: &FwupdHostPower,
) -> BatterySafetyStatus {
    let dev_name = if device.name.trim().is_empty() {
        "firmware device"
    } else {
        device.name.trim()
    };

    if let Some(dev_level) = device.battery_level.filter(|&lvl| lvl < BATTERY_LEVEL_INVALID) {
        let dev_threshold = device
            .battery_threshold
            .filter(|&t| t > 0)
            .unwrap_or(host_power.host_battery_threshold);
        if dev_threshold > 0 && dev_level < dev_threshold {
            let reason = format!(
                "{dev_name} battery is too low ({dev_level}% < {dev_threshold}% required). Charge device or connect AC power and try again."
            );
            return BatterySafetyStatus::Blocked {
                reason,
                battery_level: Some(dev_level),
                battery_threshold: Some(dev_threshold),
                requires_ac: (device.flags & DEVICE_FLAG_REQUIRE_AC) != 0,
            };
        }
    }

    let requires_ac = (device.flags & DEVICE_FLAG_REQUIRE_AC) != 0;
    let host_level = (host_power.host_battery_level < BATTERY_LEVEL_INVALID)
        .then_some(host_power.host_battery_level);
    let host_threshold = (host_power.host_battery_threshold > 0)
        .then_some(host_power.host_battery_threshold);

    if requires_ac && host_power.on_external_power == Some(false) {
        let detail = match (host_level, host_threshold) {
            (Some(lvl), Some(thr)) => format!(" ({lvl}% battery; {thr}% threshold)"),
            (Some(lvl), None) => format!(" ({lvl}% battery)"),
            _ => String::new(),
        };
        return BatterySafetyStatus::Blocked {
            reason: format!(
                "{dev_name} requires AC power to install safely{detail}. Connect AC power and try again."
            ),
            battery_level: host_level,
            battery_threshold: host_threshold,
            requires_ac: true,
        };
    }

    if let (Some(lvl), Some(thr)) = (host_level, host_threshold) {
        if lvl < thr && host_power.on_external_power != Some(true) {
            return BatterySafetyStatus::Blocked {
                reason: format!(
                    "System battery is too low for {dev_name} ({lvl}% < {thr}% required). Connect AC power and try again."
                ),
                battery_level: Some(lvl),
                battery_threshold: Some(thr),
                requires_ac,
            };
        }
    }

    BatterySafetyStatus::Ok
}

/// Build an Observatory [`Package`] from `fwupd` D-Bus device + upgrade release metadata.
pub fn package_from_device_and_release(
    device: &FwupdDevice,
    release: &FwupdRelease,
    host_power: &FwupdHostPower,
) -> Package {
    let name = if device.name.trim().is_empty() {
        "Firmware device".to_string()
    } else {
        device.name.trim().to_string()
    };
    let version = release.version.trim().to_string();
    let old_version = device
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    let id = if device.device_id.trim().is_empty() {
        format!("fwupd:{name}:{version}")
    } else {
        format!("fwupd:{}", device.device_id.trim())
    };

    let repo = match release.remote_id.as_deref().map(str::trim) {
        Some(r) if !r.is_empty() && !r.eq_ignore_ascii_case("lvfs") => r.to_string(),
        _ => "LVFS".to_string(),
    };

    let detail = match &old_version {
        Some(cur) if !version.is_empty() => format!("{cur} → {version}"),
        _ if !version.is_empty() => version.clone(),
        _ => "firmware update available".into(),
    };

    let release_desc = release
        .description
        .as_deref()
        .map(strip_html_tags)
        .filter(|s| !s.is_empty());
    let release_sum = release
        .summary
        .as_deref()
        .map(strip_html_tags)
        .filter(|s| !s.is_empty());
    let dev_sum = device
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    let summary = match (release_sum, release_desc) {
        (Some(sum), Some(desc)) if desc.to_ascii_lowercase().starts_with(&sum.to_ascii_lowercase()) => {
            Some(desc)
        }
        (Some(sum), Some(desc)) => Some(format!("{sum}\n{desc}")),
        (None, Some(desc)) => Some(desc),
        (Some(sum), None) => Some(sum),
        (None, None) => dev_sum,
    };

    let preflight_warning = evaluate_battery_safety(device, host_power)
        .reason()
        .map(ToString::to_string);

    Package {
        id,
        name,
        arch: String::new(),
        version,
        repo,
        size_bytes: release.size_bytes.filter(|&s| s > 0),
        old_version,
        status: PackageStatus::Pending,
        source: UpdateSource::Firmware,
        detail,
        summary,
        install_size_delta: None,
        advisories: Vec::new(),
        preflight_warning,
    }
}

pub fn devices_need_reboot(devices: &[FwupdDevice]) -> bool {
    devices.iter().any(FwupdDevice::needs_reboot)
}

fn unwrap_value<'a>(val: &'a Value<'a>) -> &'a Value<'a> {
    let mut cur = val;
    while let Value::Value(inner) = cur {
        cur = inner.as_ref();
    }
    cur
}

fn value_as_str(val: &Value<'_>) -> Option<String> {
    match unwrap_value(val) {
        Value::Str(s) => Some(s.as_str().to_string()),
        _ => None,
    }
}

fn value_as_u64(val: &Value<'_>) -> Option<u64> {
    match unwrap_value(val) {
        Value::U64(n) => Some(*n),
        Value::U32(n) => Some(u64::from(*n)),
        Value::U16(n) => Some(u64::from(*n)),
        Value::U8(n) => Some(u64::from(*n)),
        Value::I64(n) if *n >= 0 => Some(*n as u64),
        Value::I32(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

fn value_as_u32(val: &Value<'_>) -> Option<u32> {
    value_as_u64(val).and_then(|n| u32::try_from(n).ok())
}

fn value_as_f64(val: &Value<'_>) -> Option<f64> {
    match unwrap_value(val) {
        Value::F64(v) => Some(*v),
        Value::U32(v) => Some(f64::from(*v)),
        Value::U64(v) => Some(*v as f64),
        _ => None,
    }
}

fn value_as_bool(val: &Value<'_>) -> Option<bool> {
    match unwrap_value(val) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn value_as_str_vec(val: &Value<'_>) -> Vec<String> {
    match unwrap_value(val) {
        Value::Array(arr) => arr.iter().filter_map(value_as_str).collect(),
        _ => Vec::new(),
    }
}

fn dict_str(map: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| value_as_str(v))
}

fn dict_u64(map: &HashMap<String, OwnedValue>, key: &str) -> Option<u64> {
    map.get(key).and_then(|v| value_as_u64(v))
}

fn dict_u32(map: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    map.get(key).and_then(|v| value_as_u32(v))
}

fn dict_bool(map: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    map.get(key).and_then(|v| value_as_bool(v))
}

fn dict_str_vec(map: &HashMap<String, OwnedValue>, key: &str) -> Vec<String> {
    map.get(key)
        .map(|v| value_as_str_vec(v))
        .unwrap_or_default()
}

pub fn parse_device_dict(map: &HashMap<String, OwnedValue>) -> FwupdDevice {
    FwupdDevice {
        device_id: dict_str(map, "DeviceId").unwrap_or_default(),
        name: dict_str(map, "Name").unwrap_or_default(),
        version: dict_str(map, "Version"),
        summary: dict_str(map, "Summary"),
        plugin: dict_str(map, "Plugin"),
        vendor: dict_str(map, "Vendor"),
        flags: dict_u64(map, "Flags").unwrap_or(0),
        update_state: dict_u32(map, "UpdateState").unwrap_or(0),
        update_error: dict_str(map, "UpdateError"),
        battery_level: dict_u32(map, "BatteryLevel"),
        battery_threshold: dict_u32(map, "BatteryThreshold"),
        created: dict_u64(map, "Created"),
        modified: dict_u64(map, "Modified"),
        releases: Vec::new(),
    }
}

pub fn parse_release_dict(map: &HashMap<String, OwnedValue>) -> FwupdRelease {
    FwupdRelease {
        version: dict_str(map, "Version").unwrap_or_default(),
        summary: dict_str(map, "Summary"),
        description: dict_str(map, "Description"),
        size_bytes: dict_u64(map, "Size"),
        remote_id: dict_str(map, "RemoteId"),
        locations: dict_str_vec(map, "Locations"),
        uri: dict_str(map, "Uri"),
        checksum: dict_str(map, "Checksum"),
        urgency: dict_u32(map, "Urgency"),
        created: dict_u64(map, "Created"),
        filename: dict_str(map, "Filename"),
    }
}

pub fn parse_remote_dict(map: &HashMap<String, OwnedValue>) -> FwupdRemote {
    FwupdRemote {
        remote_id: dict_str(map, "RemoteId").unwrap_or_default(),
        enabled: dict_bool(map, "Enabled").unwrap_or(false),
        uri: dict_str(map, "Uri"),
        filename_cache: dict_str(map, "FilenameCache"),
        refresh_interval: dict_u64(map, "RefreshInterval").unwrap_or(0),
        modification_time: dict_u64(map, "ModificationTime").unwrap_or(0),
    }
}

fn extract_history_target_version(map: &HashMap<String, OwnedValue>) -> Option<String> {
    let owned = map.get("Release")?;
    let Value::Array(arr) = unwrap_value(owned) else {
        return None;
    };
    for elem in arr.iter() {
        let Value::Dict(dict) = unwrap_value(elem) else {
            continue;
        };
        for (k, v) in dict.iter() {
            if value_as_str(k).as_deref() == Some("Version") {
                if let Some(ver) = value_as_str(v).filter(|s| !s.is_empty()) {
                    return Some(ver);
                }
            }
        }
    }
    None
}

pub fn parse_history_dict(map: &HashMap<String, OwnedValue>) -> FwupdHistoryEntry {
    FwupdHistoryEntry {
        device_id: dict_str(map, "DeviceId").unwrap_or_default(),
        name: dict_str(map, "Name").unwrap_or_default(),
        version: dict_str(map, "Version").unwrap_or_default(),
        target_version: extract_history_target_version(map),
        update_state: FwupdUpdateState::from_u32(dict_u32(map, "UpdateState").unwrap_or(0)),
        update_error: dict_str(map, "UpdateError"),
        plugin: dict_str(map, "Plugin"),
        created: dict_u64(map, "Created"),
        modified: dict_u64(map, "Modified"),
    }
}

pub fn parse_device_request_dict(map: &HashMap<String, OwnedValue>) -> FwupdDeviceRequest {
    let id = dict_str(map, "AppstreamId")
        .or_else(|| dict_str(map, "DeviceId"))
        .or_else(|| dict_str(map, "RequestId"))
        .or_else(|| dict_str(map, "Id"))
        .unwrap_or_default();
    let kind = dict_u32(map, "RequestKind").unwrap_or(0);
    let message = dict_str(map, "Message");
    FwupdDeviceRequest { id, kind, message }
}

/// Probe whether `org.freedesktop.fwupd` is present or activatable on the system D-Bus.
pub fn is_available() -> bool {
    let Ok(conn) = Connection::system() else {
        return false;
    };
    let Ok(dbus) = DBusProxy::new(&conn) else {
        return false;
    };
    let owned = zbus::names::BusName::try_from(FWUPD_DEST)
        .ok()
        .and_then(|n| dbus.name_has_owner(n).ok())
        .unwrap_or(false);
    if owned {
        return true;
    }
    dbus.list_activatable_names()
        .map(|names| names.iter().any(|n| n.as_str() == FWUPD_DEST))
        .unwrap_or(false)
}

fn open_fwupd_proxy() -> Result<Proxy<'static>, NativeOutcome<()>> {
    let conn = Connection::system().map_err(|e| {
        if is_dbus_unavailable(&e) {
            NativeOutcome::Unavailable(e.to_string())
        } else {
            NativeOutcome::Handled(Err(format!("Could not connect to system D-Bus: {e}")))
        }
    })?;

    if let Ok(dbus) = DBusProxy::new(&conn) {
        let owned = zbus::names::BusName::try_from(FWUPD_DEST)
            .ok()
            .and_then(|n| dbus.name_has_owner(n).ok())
            .unwrap_or(false);
        if !owned {
            let activatable = dbus
                .list_activatable_names()
                .map(|names| names.iter().any(|n| n.as_str() == FWUPD_DEST))
                .unwrap_or(false);
            if !activatable {
                return Err(NativeOutcome::Unavailable(
                    "org.freedesktop.fwupd is not available on system D-Bus".into(),
                ));
            }
        }
    }

    Proxy::new(&conn, FWUPD_DEST, FWUPD_PATH, FWUPD_IFACE)
        .map_err(|e| NativeOutcome::Unavailable(e.to_string()))
}

fn read_host_power(proxy: &Proxy<'_>) -> FwupdHostPower {
    let host_battery_level = proxy
        .get_property::<u32>("BatteryLevel")
        .unwrap_or(BATTERY_LEVEL_INVALID);
    let host_battery_threshold = proxy.get_property::<u32>("BatteryThreshold").unwrap_or(0);
    let on_external_power = logind::query_on_external_power();
    FwupdHostPower {
        host_battery_level,
        host_battery_threshold,
        on_external_power,
    }
}

struct TempWorkDir {
    path: PathBuf,
}

impl TempWorkDir {
    fn create(prefix: &str) -> Result<Self, String> {
        let seq = TEMP_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "fedora-updater-{prefix}-{}-{seq}",
            std::process::id()
        ));
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder
            .create(&dir)
            .map_err(|e| format!("Failed to create temp directory {}: {e}", dir.display()))?;
        Ok(Self { path: dir })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn download_to_file(url: &str, dest: &Path, timeout_secs: &str) -> Result<File, String> {
    let dest_str = dest.to_string_lossy();
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", timeout_secs, "-o", &dest_str, url])
        .output()
        .map_err(|e| format!("Failed to invoke curl for {url}: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            format!("curl failed downloading {url} (exit {:?})", out.status.code())
        } else {
            format!("Failed to download {url}: {detail}")
        });
    }
    File::open(dest).map_err(|e| format!("Failed to open downloaded {}: {e}", dest.display()))
}

fn is_remote_stale(remote: &FwupdRemote, now_secs: u64) -> bool {
    let cache_exists = remote
        .filename_cache
        .as_deref()
        .is_some_and(|p| Path::new(p).exists());
    if !cache_exists || remote.modification_time == 0 {
        return true;
    }
    now_secs >= remote.modification_time.saturating_add(remote.refresh_interval)
}

fn refresh_stale_remotes(proxy: &Proxy<'_>, on_line: &mut dyn FnMut(String)) {
    let raw_remotes: Vec<HashMap<String, OwnedValue>> = match proxy.call("GetRemotes", &()) {
        Ok(r) => r,
        Err(e) => {
            on_line(format!("Could not query fwupd remotes: {e}"));
            return;
        }
    };

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for raw in &raw_remotes {
        let remote = parse_remote_dict(raw);
        let Some(uri) = remote.uri.as_deref() else {
            continue;
        };
        if !remote.enabled || (!uri.starts_with("https://") && !uri.starts_with("http://")) {
            continue;
        }
        if !is_remote_stale(&remote, now_secs) {
            on_line(format!("LVFS remote {} metadata is up to date", remote.remote_id));
            continue;
        }
        on_line(format!("Refreshing firmware metadata for {}…", remote.remote_id));
        let Ok(tmp) = TempWorkDir::create("fwupd-meta") else {
            continue;
        };
        let data_path = tmp.path().join("metadata.xml.zst");
        let sig_path = tmp.path().join("metadata.xml.zst.jcat");
        let sig_uri = format!("{uri}.jcat");
        match (
            download_to_file(uri, &data_path, "30"),
            download_to_file(&sig_uri, &sig_path, "30"),
        ) {
            (Ok(data_file), Ok(sig_file)) => {
                let data_fd = OwnedFd::from(StdOwnedFd::from(data_file));
                let sig_fd = OwnedFd::from(StdOwnedFd::from(sig_file));
                if let Err(e) =
                    proxy.call::<_, _, ()>("UpdateMetadata", &(remote.remote_id.as_str(), data_fd, sig_fd))
                {
                    on_line(format!(
                        "Could not refresh {} metadata over D-Bus: {e}",
                        remote.remote_id
                    ));
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                on_line(format!(
                    "Could not download {} metadata: {e}",
                    remote.remote_id
                ));
            }
        }
    }
}

fn is_no_updates_dbus_err(err: &zbus::Error) -> bool {
    if let zbus::Error::MethodError(name, _, _) = err {
        let s = name.as_str();
        return s.ends_with(".NothingToDo")
            || s.ends_with(".NotSupported")
            || s.ends_with(".NotFound");
    }
    let lower = err.to_string().to_ascii_lowercase();
    lower.contains("nothing to do")
        || lower.contains("no upgrades")
        || lower.contains("not supported")
}

/// Query firmware updates natively from `org.freedesktop.fwupd`.
pub fn check_updates(on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
    let proxy = match open_fwupd_proxy() {
        Ok(p) => p,
        Err(NativeOutcome::Unavailable(reason)) => return NativeOutcome::Unavailable(reason),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };

    refresh_stale_remotes(&proxy, on_line);
    let host_power = read_host_power(&proxy);

    let raw_devices: Vec<HashMap<String, OwnedValue>> = match proxy.call("GetDevices", &()) {
        Ok(d) => d,
        Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
        Err(e) => return NativeOutcome::Handled(Err(format!("fwupd GetDevices failed: {e}"))),
    };

    let mut packages = Vec::new();
    for raw in &raw_devices {
        let device = parse_device_dict(raw);
        if device.device_id.is_empty() || !device.is_updatable() {
            continue;
        }
        let upgrades: Vec<HashMap<String, OwnedValue>> =
            match proxy.call("GetUpgrades", &(device.device_id.as_str(),)) {
                Ok(list) => list,
                Err(e) if is_no_updates_dbus_err(&e) => continue,
                Err(e) if is_dbus_unavailable(&e) => {
                    return NativeOutcome::Unavailable(e.to_string());
                }
                Err(e) => {
                    on_line(format!("fwupd GetUpgrades({}) skipped: {e}", device.name));
                    continue;
                }
            };

        let Some(first_raw) = upgrades.first() else {
            continue;
        };
        let mut release = parse_release_dict(first_raw);
        if (release.summary.is_none() && release.description.is_none())
            || release.download_url().is_none()
        {
            if let Ok(rels) =
                proxy.call::<_, _, Vec<HashMap<String, OwnedValue>>>("GetReleases", &(device.device_id.as_str(),))
            {
                if let Some(matched) = rels
                    .iter()
                    .map(parse_release_dict)
                    .find(|r| r.version == release.version)
                {
                    if release.summary.is_none() {
                        release.summary = matched.summary;
                    }
                    if release.description.is_none() {
                        release.description = matched.description;
                    }
                    if release.locations.is_empty() {
                        release.locations = matched.locations;
                    }
                    if release.uri.is_none() {
                        release.uri = matched.uri;
                    }
                }
            }
        }

        let pkg = package_from_device_and_release(&device, &release, &host_power);
        if let Some(warn) = pkg.preflight_warning.as_deref() {
            on_line(format!("Power safety notice: {warn}"));
        }
        packages.push(pkg);
    }

    on_line(format!("{} firmware update(s) via D-Bus", packages.len()));
    NativeOutcome::Handled(Ok(packages))
}

/// Query firmware update history natively from `org.freedesktop.fwupd`.
pub fn get_history() -> NativeOutcome<Vec<FwupdHistoryEntry>> {
    let proxy = match open_fwupd_proxy() {
        Ok(p) => p,
        Err(NativeOutcome::Unavailable(reason)) => return NativeOutcome::Unavailable(reason),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };

    let raw_history: Vec<HashMap<String, OwnedValue>> = match proxy.call("GetHistory", &()) {
        Ok(h) => h,
        Err(e) if is_no_updates_dbus_err(&e) => return NativeOutcome::Handled(Ok(Vec::new())),
        Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
        Err(e) => return NativeOutcome::Handled(Err(format!("fwupd GetHistory failed: {e}"))),
    };

    let entries = raw_history.iter().map(parse_history_dict).collect();
    NativeOutcome::Handled(Ok(entries))
}

/// Check if any firmware device needs reboot or shutdown via `org.freedesktop.fwupd`.
pub fn check_reboot_needed() -> NativeOutcome<bool> {
    let proxy = match open_fwupd_proxy() {
        Ok(p) => p,
        Err(NativeOutcome::Unavailable(reason)) => return NativeOutcome::Unavailable(reason),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };

    let raw_devices: Vec<HashMap<String, OwnedValue>> = match proxy.call("GetDevices", &()) {
        Ok(d) => d,
        Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
        Err(e) => return NativeOutcome::Handled(Err(format!("fwupd GetDevices failed: {e}"))),
    };
    let devices: Vec<FwupdDevice> = raw_devices.iter().map(parse_device_dict).collect();
    NativeOutcome::Handled(Ok(devices_need_reboot(&devices)))
}

fn find_matching_device<'a>(devices: &'a [FwupdDevice], pkg: &Package) -> Option<&'a FwupdDevice> {
    if let Some(dev_id) = pkg.id.strip_prefix("fwupd:") {
        if let Some(found) = devices.iter().find(|d| d.device_id == dev_id) {
            return Some(found);
        }
    }
    devices.iter().find(|d| d.name == pkg.name)
}

fn install_device_with_progress(
    device_id: &str,
    device_name: &str,
    cab_file: File,
    on_line: &mut dyn FnMut(String),
    on_progress: &mut dyn FnMut(ProgressHint),
) -> Result<(), String> {
    let ctx = glib::MainContext::new();
    ctx.block_on(async {
        let conn = zbus::Connection::system()
            .await
            .map_err(|e| format!("System D-Bus connection failed: {e}"))?;
        let fwupd_proxy = zbus::Proxy::new(&conn, FWUPD_DEST, FWUPD_PATH, FWUPD_IFACE)
            .await
            .map_err(|e| format!("Could not create fwupd proxy: {e}"))?;
        let props_proxy = AsyncPropertiesProxy::builder(&conn)
            .destination(FWUPD_DEST)
            .map_err(|e| e.to_string())?
            .path(FWUPD_PATH)
            .map_err(|e| e.to_string())?
            .build()
            .await
            .map_err(|e| format!("Could not create Properties proxy: {e}"))?;

        let mut props_stream = props_proxy
            .receive_properties_changed()
            .await
            .map_err(|e| format!("Could not subscribe to fwupd properties: {e}"))?;
        let mut req_stream = fwupd_proxy
            .receive_signal("DeviceRequest")
            .await
            .map_err(|e| format!("Could not subscribe to DeviceRequest: {e}"))?;

        let fd = OwnedFd::from(StdOwnedFd::from(cab_file));
        let opts: HashMap<&str, Value<'_>> = HashMap::new();
        let install_args = (device_id, fd, opts);
        let mut install_fut = Box::pin(fwupd_proxy.call::<_, _, ()>("Install", &install_args));

        let mut last_status = FwupdStatus::Unknown;
        let mut last_pct: f64 = 0.0;

        poll_fn(|cx| {
            while let Poll::Ready(Some(sig)) = Pin::new(&mut props_stream).poll_next(cx) {
                if let Ok(args) = sig.args() {
                    if args.interface_name.as_str() == FWUPD_IFACE {
                        let changed = args.changed_properties();
                        let mut updated = false;
                        if let Some(status_val) = changed.get("Status") {
                            if let Some(code) = value_as_u32(status_val) {
                                last_status = FwupdStatus::from_u32(code);
                                updated = true;
                            }
                        }
                        if let Some(pct_val) = changed.get("PercentageFull") {
                            if let Some(pct) = value_as_f64(pct_val) {
                                last_pct = pct;
                                updated = true;
                            }
                        } else if let Some(pct_val) = changed.get("Percentage") {
                            if let Some(pct) = value_as_f64(pct_val) {
                                last_pct = pct;
                                updated = true;
                            }
                        }
                        if updated {
                            on_progress(last_status.to_progress_hint(last_pct, Some(device_name)));
                        }
                    }
                }
            }

            while let Poll::Ready(Some(msg)) = Pin::new(&mut req_stream).poll_next(cx) {
                if let Ok(body) = msg.body().deserialize::<HashMap<String, OwnedValue>>() {
                    let req = parse_device_request_dict(&body);
                    if let Some(text) = req.message.as_deref() {
                        on_line(format!("Device request ({}): {text}", device_name));
                    }
                }
            }

            match install_fut.as_mut().poll(cx) {
                Poll::Ready(res) => Poll::Ready(res.map_err(|e| e.to_string())),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    })
}

/// Apply pending firmware updates via `org.freedesktop.fwupd` with battery safety pre-flight.
pub fn apply_updates(
    packages: &[Package],
    on_line: &mut dyn FnMut(String),
    on_progress: &mut dyn FnMut(ProgressHint),
) -> NativeOutcome<FwupdApplyOutcome> {
    let proxy = match open_fwupd_proxy() {
        Ok(p) => p,
        Err(NativeOutcome::Unavailable(reason)) => return NativeOutcome::Unavailable(reason),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };

    let host_power = read_host_power(&proxy);
    let raw_devices: Vec<HashMap<String, OwnedValue>> = match proxy.call("GetDevices", &()) {
        Ok(d) => d,
        Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
        Err(e) => return NativeOutcome::Handled(Err(format!("fwupd GetDevices failed: {e}"))),
    };
    let devices: Vec<FwupdDevice> = raw_devices.iter().map(parse_device_dict).collect();

    let mut targets: Vec<(&Package, &FwupdDevice)> = Vec::new();
    for pkg in packages.iter().filter(|p| p.source == UpdateSource::Firmware) {
        if let Some(dev) = find_matching_device(&devices, pkg) {
            targets.push((pkg, dev));
        } else {
            return NativeOutcome::Handled(Err(format!(
                "Firmware device \"{}\" is no longer connected.",
                pkg.name
            )));
        }
    }

    if targets.is_empty() {
        return NativeOutcome::Handled(Ok(FwupdApplyOutcome {
            needs_reboot: devices_need_reboot(&devices),
        }));
    }

    // Battery safety pre-flight before downloading or flashing any capsule.
    for (_, dev) in &targets {
        if let BatterySafetyStatus::Blocked { reason, .. } =
            evaluate_battery_safety(dev, &host_power)
        {
            on_line(reason.clone());
            return NativeOutcome::Handled(Err(reason));
        }
    }

    let tmp_dir = match TempWorkDir::create("fwupd-cab") {
        Ok(d) => d,
        Err(e) => return NativeOutcome::Handled(Err(e)),
    };

    for (idx, (pkg, dev)) in targets.iter().enumerate() {
        let upgrades: Vec<HashMap<String, OwnedValue>> =
            match proxy.call("GetUpgrades", &(dev.device_id.as_str(),)) {
                Ok(list) => list,
                Err(e) if is_no_updates_dbus_err(&e) => {
                    on_line(format!("{} is already up to date", dev.name));
                    on_progress(ProgressHint {
                        package_name: Some(dev.name.clone()),
                        status: Some(PackageStatus::Completed),
                        progress: Some(1.0),
                        phase_label: Some("Firmware installed".into()),
                        ..Default::default()
                    });
                    continue;
                }
                Err(e) => {
                    return NativeOutcome::Handled(Err(format!(
                        "Could not look up firmware release for {}: {e}",
                        dev.name
                    )));
                }
            };

        let Some(first_raw) = upgrades.first() else {
            continue;
        };
        let mut release = parse_release_dict(first_raw);
        if release.download_url().is_none() {
            if let Ok(rels) =
                proxy.call::<_, _, Vec<HashMap<String, OwnedValue>>>("GetReleases", &(dev.device_id.as_str(),))
            {
                let parsed: Vec<FwupdRelease> = rels.iter().map(parse_release_dict).collect();
                if let Some(matched) = parsed
                    .iter()
                    .find(|r| !release.version.is_empty() && r.version == release.version)
                    .or_else(|| {
                        parsed
                            .iter()
                            .find(|r| !pkg.version.is_empty() && r.version == pkg.version)
                    })
                    .or_else(|| parsed.first())
                {
                    if release.download_url().is_none() {
                        release.locations = matched.locations.clone();
                        release.uri = matched.uri.clone();
                    }
                }
            }
        }

        let Some(url) = release.download_url() else {
            return NativeOutcome::Handled(Err(format!(
                "No LVFS download URL available for {} {}",
                dev.name, pkg.version
            )));
        };

        on_line(format!("Downloading firmware for {}…", dev.name));
        on_progress(FwupdStatus::Downloading.to_progress_hint(0.0, Some(&dev.name)));
        let cab_path = tmp_dir.path().join(format!("capsule-{idx}.cab"));
        let cab_file = match download_to_file(url, &cab_path, "120") {
            Ok(f) => f,
            Err(e) => return NativeOutcome::Handled(Err(e)),
        };

        on_line(format!("Installing firmware for {}…", dev.name));
        if let Err(e) = install_device_with_progress(
            &dev.device_id,
            &dev.name,
            cab_file,
            on_line,
            on_progress,
        ) {
            return NativeOutcome::Handled(Err(e));
        }

        on_line(format!("Successfully installed firmware for {}", dev.name));
        on_progress(ProgressHint {
            package_name: Some(dev.name.clone()),
            status: Some(PackageStatus::Completed),
            progress: Some(1.0),
            phase_label: Some("Firmware installed".into()),
            ..Default::default()
        });
    }

    let final_devices: Vec<FwupdDevice> = match proxy.call::<_, _, Vec<HashMap<String, OwnedValue>>>("GetDevices", &()) {
        Ok(raw) => raw.iter().map(parse_device_dict).collect(),
        Err(_) => devices,
    };
    NativeOutcome::Handled(Ok(FwupdApplyOutcome {
        needs_reboot: devices_need_reboot(&final_devices),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_device() -> FwupdDevice {
        FwupdDevice {
            device_id: "abc123".into(),
            name: "System Firmware".into(),
            version: Some("1.0.0".into()),
            summary: Some("UEFI System BIOS".into()),
            plugin: Some("uefi_capsule".into()),
            vendor: Some("ThinkPad".into()),
            flags: DEVICE_FLAG_INTERNAL | DEVICE_FLAG_UPDATABLE | DEVICE_FLAG_REQUIRE_AC,
            update_state: FwupdUpdateState::Pending as u32,
            update_error: None,
            battery_level: None,
            battery_threshold: None,
            created: None,
            modified: None,
            releases: Vec::new(),
        }
    }

    #[test]
    fn status_maps_cleanly_to_progress_hints() {
        let dl = FwupdStatus::Downloading.to_progress_hint(42.5, Some("UEFI dbx"));
        assert_eq!(dl.package_name.as_deref(), Some("UEFI dbx"));
        assert_eq!(dl.status, Some(PackageStatus::Downloading));
        assert_eq!(dl.phase_label.as_deref(), Some("Downloading firmware"));
        assert!((dl.progress.unwrap() - 0.425).abs() < 1e-6);

        let write = FwupdStatus::DeviceWrite.to_progress_hint(80.0, Some("NVMe SSD"));
        assert_eq!(write.status, Some(PackageStatus::Installing));
        assert_eq!(write.phase_label.as_deref(), Some("Installing firmware"));

        let verify = FwupdStatus::DeviceVerify.to_progress_hint(100.0, None);
        assert_eq!(verify.phase_label.as_deref(), Some("Verifying firmware"));
    }

    #[test]
    fn strip_html_tags_cleans_appstream_markup() {
        let html = "<p>This updates the &quot;db&quot; &amp; adds fixes.</p><ul><li>Fix A &lt;tag&gt;</li><li>Fix B</li></ul>";
        assert_eq!(
            strip_html_tags(html),
            "This updates the \"db\" & adds fixes. Fix A <tag> Fix B"
        );
    }

    #[test]
    fn evaluate_battery_safety_covers_desktop_ac_and_peripheral_thresholds() {
        let mut dev = sample_device();

        // Desktop with BATTERY_LEVEL_INVALID (101) and unknown AC power -> allowed.
        let desktop = FwupdHostPower {
            host_battery_level: BATTERY_LEVEL_INVALID,
            host_battery_threshold: 25,
            on_external_power: None,
        };
        assert_eq!(evaluate_battery_safety(&dev, &desktop), BatterySafetyStatus::Ok);

        // Laptop on battery with DEVICE_FLAG_REQUIRE_AC -> blocked cleanly.
        let unplugged = FwupdHostPower {
            host_battery_level: 60,
            host_battery_threshold: 25,
            on_external_power: Some(false),
        };
        let blocked_ac = evaluate_battery_safety(&dev, &unplugged);
        let msg = blocked_ac.reason().expect("AC required warning");
        assert!(msg.contains("System Firmware"), "{msg}");
        assert!(msg.contains("AC power"), "{msg}");

        // Laptop plugged in with battery above threshold -> ok.
        let plugged = FwupdHostPower {
            host_battery_level: 60,
            host_battery_threshold: 25,
            on_external_power: Some(true),
        };
        assert_eq!(evaluate_battery_safety(&dev, &plugged), BatterySafetyStatus::Ok);

        // Laptop on battery below threshold even without REQUIRE_AC -> blocked.
        dev.flags &= !DEVICE_FLAG_REQUIRE_AC;
        let low_host = FwupdHostPower {
            host_battery_level: 15,
            host_battery_threshold: 25,
            on_external_power: Some(false),
        };
        let blocked_low = evaluate_battery_safety(&dev, &low_host);
        let low_msg = blocked_low.reason().expect("low battery warning");
        assert!(low_msg.contains("15% < 25%"), "{low_msg}");

        // Peripheral mouse with its own low BatteryLevel vs threshold -> blocked.
        dev.name = "Wireless Mouse".into();
        dev.battery_level = Some(8);
        dev.battery_threshold = Some(20);
        let blocked_periph = evaluate_battery_safety(&dev, &plugged);
        let p_msg = blocked_periph.reason().expect("peripheral warning");
        assert!(p_msg.contains("Wireless Mouse"), "{p_msg}");
        assert!(p_msg.contains("8% < 20%"), "{p_msg}");
    }

    #[test]
    fn package_from_device_and_release_attaches_summary_size_and_warning() {
        let dev = sample_device();
        let rel = FwupdRelease {
            version: "1.0.2".into(),
            summary: Some("BIOS security update".into()),
            description: Some("<p>Fixes CVE-2026-0001 and boot timing.</p>".into()),
            size_bytes: Some(12 * 1024 * 1024),
            remote_id: Some("lvfs".into()),
            locations: vec!["https://fwupd.org/downloads/bios.cab".into()],
            uri: None,
            checksum: None,
            urgency: Some(3),
            created: None,
            filename: None,
        };
        let unplugged = FwupdHostPower {
            host_battery_level: 40,
            host_battery_threshold: 25,
            on_external_power: Some(false),
        };

        let pkg = package_from_device_and_release(&dev, &rel, &unplugged);
        assert_eq!(pkg.id, "fwupd:abc123");
        assert_eq!(pkg.repo, "LVFS");
        assert_eq!(pkg.old_version.as_deref(), Some("1.0.0"));
        assert_eq!(pkg.version, "1.0.2");
        assert_eq!(pkg.detail, "1.0.0 → 1.0.2");
        assert_eq!(pkg.size_bytes, Some(12 * 1024 * 1024));
        assert!(pkg
            .summary
            .as_deref()
            .unwrap()
            .contains("Fixes CVE-2026-0001"));
        assert!(pkg.preflight_warning.is_some());
    }

    #[test]
    fn devices_need_reboot_checks_flags_and_update_state() {
        let mut dev = sample_device();
        assert!(!devices_need_reboot(std::slice::from_ref(&dev)));

        dev.flags |= DEVICE_FLAG_NEEDS_REBOOT;
        assert!(devices_need_reboot(std::slice::from_ref(&dev)));

        dev.flags &= !DEVICE_FLAG_NEEDS_REBOOT;
        dev.flags |= DEVICE_FLAG_NEEDS_SHUTDOWN;
        assert!(devices_need_reboot(std::slice::from_ref(&dev)));

        dev.flags &= !DEVICE_FLAG_NEEDS_SHUTDOWN;
        dev.update_state = FwupdUpdateState::NeedsReboot as u32;
        assert!(devices_need_reboot(&[dev]));
    }

    #[test]
    fn zbus_history_and_device_request_dicts_parse_cleanly() {
        let mut hist_map: HashMap<String, OwnedValue> = HashMap::new();
        hist_map.insert("DeviceId".into(), Value::from("dbx0").try_into().unwrap());
        hist_map.insert("Name".into(), Value::from("UEFI dbx").try_into().unwrap());
        hist_map.insert("Version".into(), Value::from("20260402").try_into().unwrap());
        hist_map.insert("UpdateState".into(), Value::from(2u32).try_into().unwrap());

        let parsed = parse_history_dict(&hist_map);
        assert_eq!(parsed.device_id, "dbx0");
        assert_eq!(parsed.name, "UEFI dbx");
        assert_eq!(parsed.version, "20260402");
        assert_eq!(parsed.update_state, FwupdUpdateState::Success);

        let mut req_map: HashMap<String, OwnedValue> = HashMap::new();
        req_map.insert(
            "AppstreamId".into(),
            Value::from("org.fwupd.replug").try_into().unwrap(),
        );
        req_map.insert("RequestKind".into(), Value::from(2u32).try_into().unwrap());
        req_map.insert(
            "Message".into(),
            Value::from("Unplug and reconnect USB-C dock")
                .try_into()
                .unwrap(),
        );
        let req = parse_device_request_dict(&req_map);
        assert_eq!(req.id, "org.fwupd.replug");
        assert_eq!(req.kind, 2);
        assert_eq!(
            req.message.as_deref(),
            Some("Unplug and reconnect USB-C dock")
        );
    }

    #[test]
    #[ignore = "probes live system org.freedesktop.fwupd on host"]
    fn live_fwupd_dbus_devices_and_history_probe() {
        assert!(is_available(), "org.freedesktop.fwupd expected on host");
        let proxy = match open_fwupd_proxy() {
            Ok(p) => p,
            Err(other) => panic!("expected live proxy, got {other:?}"),
        };
        let raw_devices: Vec<HashMap<String, OwnedValue>> =
            proxy.call("GetDevices", &()).expect("GetDevices");
        assert!(!raw_devices.is_empty(), "expected at least one device");

        match get_history() {
            NativeOutcome::Handled(Ok(_entries)) => {}
            other => panic!("unexpected GetHistory result: {other:?}"),
        }

        let status = glib::MainContext::new().block_on(async {
            let conn = zbus::Connection::system()
                .await
                .expect("async system bus");
            let p = zbus::Proxy::new(&conn, FWUPD_DEST, FWUPD_PATH, FWUPD_IFACE)
                .await
                .expect("async fwupd proxy");
            p.get_property::<u32>("Status")
                .await
                .expect("async Status property")
        });
        assert_ne!(FwupdStatus::from_u32(status), FwupdStatus::Unknown);
    }
}
