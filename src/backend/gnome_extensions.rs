//! Native GNOME Shell Extensions (`org.gnome.Shell.Extensions`) session D-Bus integration.
//!
//! Probes `org.gnome.Shell.Extensions` on the user's session D-Bus, discovers
//! pending updates for per-user extensions (`type == 2`, skipping system RPM
//! extensions with `type == 1`), and applies selected updates via
//! `InstallRemoteExtension(uuid)`. Silently reports unavailable on non-GNOME
//! desktops and headless environments.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::time::Duration;

use serde_json::Value as JsonValue;
use zbus::blocking::connection::Builder as ConnectionBuilder;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

use crate::model::{Package, PackageStatus, UpdateSource};
use crate::orchestrator::WorkCancel;

use super::logind::is_dbus_unavailable;
use super::{NativeOutcome, ProgressHint};

pub const GNOME_SHELL_DEST: &str = "org.gnome.Shell";
pub const GNOME_EXTENSIONS_DEST: &str = "org.gnome.Shell.Extensions";
pub const GNOME_EXTENSIONS_PATH: &str = "/org/gnome/Shell/Extensions";
pub const GNOME_EXTENSIONS_IFACE: &str = "org.gnome.Shell.Extensions";

/// System-installed extension (managed by DNF / RPM, never updated via E.G.O.).
pub const EXTENSION_TYPE_SYSTEM: u32 = 1;
/// User-installed extension under `~/.local/share/gnome-shell/extensions/`.
pub const EXTENSION_TYPE_PER_USER: u32 = 2;

const CHECK_DBUS_TIMEOUT: Duration = Duration::from_secs(20);
const INSTALL_DBUS_TIMEOUT: Duration = Duration::from_secs(120);

/// Pure parsed metadata for a single GNOME Shell extension entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GnomeExtensionInfo {
    pub uuid: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub ext_type: Option<u32>,
    pub has_update: bool,
    pub version: Option<String>,
    pub version_name: Option<String>,
    pub target_version: Option<String>,
}

/// Outcome of applying selected GNOME Shell extension updates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GnomeExtensionsApplyOutcome {
    pub completed: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub errors: Vec<String>,
}

/// True when `org.gnome.Shell.Extensions` is already owned on the session bus,
/// or when `org.gnome.Shell` is actively running and `org.gnome.Shell.Extensions`
/// is D-Bus-activatable (avoiding false positives on KDE/Sway hosts that happen
/// to have `gnome-shell` packages installed on disk).
pub fn is_service_available(ext_owned: bool, shell_owned: bool, ext_activatable: bool) -> bool {
    ext_owned || (shell_owned && ext_activatable)
}

fn check_bus_service_available(conn: &Connection) -> bool {
    let Ok(dbus) = DBusProxy::new(conn) else {
        return false;
    };
    let ext_owned = zbus::names::BusName::try_from(GNOME_EXTENSIONS_DEST)
        .ok()
        .and_then(|n| dbus.name_has_owner(n).ok())
        .unwrap_or(false);
    if ext_owned {
        return true;
    }
    let shell_owned = zbus::names::BusName::try_from(GNOME_SHELL_DEST)
        .ok()
        .and_then(|n| dbus.name_has_owner(n).ok())
        .unwrap_or(false);
    let ext_activatable = dbus
        .list_activatable_names()
        .map(|names| names.iter().any(|n| n.as_str() == GNOME_EXTENSIONS_DEST))
        .unwrap_or(false);
    is_service_available(ext_owned, shell_owned, ext_activatable)
}

/// Probe whether `org.gnome.Shell.Extensions` is available on the user's session D-Bus.
pub fn gnome_extensions_available() -> bool {
    let Ok(conn) = Connection::session() else {
        return false;
    };
    check_bus_service_available(&conn)
}

/// Alias matching sibling native backends (`dnf_dbus::is_available`, `fwupd_dbus::is_available`).
pub fn is_available() -> bool {
    gnome_extensions_available()
}

fn is_timeout_error(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::InputOutput(io_err) => io_err.kind() == ErrorKind::TimedOut,
        zbus::Error::MethodError(name, _, _) => {
            let s = name.as_str();
            s == "org.freedesktop.DBus.Error.NoReply"
                || s == "org.freedesktop.DBus.Error.TimedOut"
                || s == "org.freedesktop.DBus.Error.Timeout"
        }
        other => other.to_string().to_ascii_lowercase().contains("timed out"),
    }
}

fn is_session_dbus_unavailable(err: &zbus::Error) -> bool {
    !is_timeout_error(err) && is_dbus_unavailable(err)
}

fn open_extensions_proxy(timeout: Duration) -> Result<Proxy<'static>, NativeOutcome<()>> {
    let builder = ConnectionBuilder::session().map_err(|e| {
        if is_session_dbus_unavailable(&e) {
            NativeOutcome::Unavailable(e.to_string())
        } else {
            NativeOutcome::Handled(Err(format!("Could not connect to session D-Bus: {e}")))
        }
    })?;
    let conn = builder.method_timeout(timeout).build().map_err(|e| {
        if is_session_dbus_unavailable(&e) {
            NativeOutcome::Unavailable(e.to_string())
        } else {
            NativeOutcome::Handled(Err(format!("Could not connect to session D-Bus: {e}")))
        }
    })?;

    if !check_bus_service_available(&conn) {
        return Err(NativeOutcome::Unavailable(
            "org.gnome.Shell.Extensions is not available on session D-Bus".into(),
        ));
    }

    Proxy::new(
        &conn,
        GNOME_EXTENSIONS_DEST,
        GNOME_EXTENSIONS_PATH,
        GNOME_EXTENSIONS_IFACE,
    )
    .map_err(|e| NativeOutcome::Unavailable(e.to_string()))
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

fn value_as_bool(val: &Value<'_>) -> Option<bool> {
    match unwrap_value(val) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn value_as_u32(val: &Value<'_>) -> Option<u32> {
    match unwrap_value(val) {
        Value::U32(n) => Some(*n),
        Value::U64(n) => u32::try_from(*n).ok(),
        Value::U16(n) => Some(u32::from(*n)),
        Value::U8(n) => Some(u32::from(*n)),
        Value::I64(n) => u32::try_from(*n).ok(),
        Value::I32(n) => u32::try_from(*n).ok(),
        Value::I16(n) => u32::try_from(*n).ok(),
        Value::F64(v) if v.is_finite() && *v >= 0.0 && v.fract() == 0.0 && *v <= f64::from(u32::MAX) => {
            Some(*v as u32)
        }
        Value::Str(s) => s.as_str().trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn value_as_version_string(val: &Value<'_>) -> Option<String> {
    match unwrap_value(val) {
        Value::U64(n) => Some(format!("v{n}")),
        Value::U32(n) => Some(format!("v{n}")),
        Value::U16(n) => Some(format!("v{n}")),
        Value::U8(n) => Some(format!("v{n}")),
        Value::I64(n) if *n >= 0 => Some(format!("v{n}")),
        Value::I32(n) if *n >= 0 => Some(format!("v{n}")),
        Value::F64(v) if v.is_finite() && *v >= 0.0 => {
            if v.fract() == 0.0 && *v <= 9_007_199_254_740_991.0 {
                Some(format!("v{}", *v as u64))
            } else {
                Some(v.to_string())
            }
        }
        Value::Str(s) => format_str_version(s.as_str()),
        _ => None,
    }
}

fn format_str_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|b| b.is_ascii_digit()) {
        Some(format!("v{trimmed}"))
    } else {
        Some(trimmed.to_string())
    }
}

fn json_as_version_string(val: &JsonValue) -> Option<String> {
    match val {
        JsonValue::Number(n) => {
            if let Some(u) = n.as_u64() {
                Some(format!("v{u}"))
            } else if let Some(f) = n.as_f64().filter(|v| v.is_finite() && *v >= 0.0) {
                if f.fract() == 0.0 && f <= 9_007_199_254_740_991.0 {
                    Some(format!("v{}", f as u64))
                } else {
                    Some(f.to_string())
                }
            } else {
                None
            }
        }
        JsonValue::String(s) => format_str_version(s),
        _ => None,
    }
}

fn json_as_u32(val: &JsonValue) -> Option<u32> {
    match val {
        JsonValue::Number(n) => {
            if let Some(u) = n.as_u64() {
                u32::try_from(u).ok()
            } else if let Some(f) = n.as_f64() {
                (f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= f64::from(u32::MAX))
                    .then_some(f as u32)
            } else {
                None
            }
        }
        JsonValue::String(s) => s.trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn format_installed_version(version: Option<&str>, version_name: Option<&str>) -> Option<String> {
    let ver = version.map(str::trim).filter(|s| !s.is_empty());
    let name = version_name.map(str::trim).filter(|s| !s.is_empty());
    match (ver, name) {
        (Some(v), Some(vn)) => {
            let v_digits = v.strip_prefix('v').unwrap_or(v);
            let vn_digits = vn.strip_prefix('v').unwrap_or(vn);
            if vn.eq_ignore_ascii_case(v) || vn_digits == v_digits {
                Some(v.to_string())
            } else {
                Some(format!("{vn} ({v})"))
            }
        }
        (Some(v), None) => Some(v.to_string()),
        (None, Some(vn)) => Some(vn.to_string()),
        (None, None) => None,
    }
}

fn dict_str(map: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| value_as_str(v))
}

/// Convert parsed extension entries into pending Observatory [`Package`] items,
/// keeping only per-user extensions (`type == 2`) with `hasUpdate == true`.
pub fn packages_from_extensions(
    entries: impl IntoIterator<Item = GnomeExtensionInfo>,
) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut seen = HashSet::new();

    for entry in entries {
        if entry.ext_type != Some(EXTENSION_TYPE_PER_USER) || !entry.has_update {
            continue;
        }
        let uuid = entry.uuid.trim().to_string();
        if uuid.is_empty() || !seen.insert(uuid.clone()) {
            continue;
        }
        let name = entry.name.unwrap_or_default();
        let current_version = format_installed_version(
            entry.version.as_deref(),
            entry.version_name.as_deref(),
        );
        let mut pkg = Package::new_gnome_extension(
            &uuid,
            name,
            current_version,
            entry.description,
            entry.url,
        );
        if let Some(target) = entry
            .target_version
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            pkg.version = target.to_string();
            pkg.detail = match &pkg.old_version {
                Some(old) => format!("{old} → {target}"),
                None => format!("{target} · {uuid}"),
            };
        }
        packages.push(pkg);
    }

    packages.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    packages
}

/// Parse `org.gnome.Shell.Extensions.ListExtensions` D-Bus output (`a{sa{sv}}`).
pub fn parse_extensions_map(map: &HashMap<String, HashMap<String, OwnedValue>>) -> Vec<Package> {
    let entries = map.iter().map(|(key, props)| {
        let uuid = dict_str(props, "uuid")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| key.trim().to_string());
        let name = dict_str(props, "name");
        let description = dict_str(props, "description");
        let url = dict_str(props, "url");
        let ext_type = props.get("type").and_then(|v| value_as_u32(v));
        let has_update = props
            .get("hasUpdate")
            .and_then(|v| value_as_bool(v))
            .unwrap_or(false);
        let version = props.get("version").and_then(|v| value_as_version_string(v));
        let version_name = dict_str(props, "version-name");
        let target_version = props
            .get("update-version")
            .or_else(|| props.get("target-version"))
            .and_then(|v| value_as_str(v).or_else(|| value_as_version_string(v)));

        GnomeExtensionInfo {
            uuid,
            name,
            description,
            url,
            ext_type,
            has_update,
            version,
            version_name,
            target_version,
        }
    });
    packages_from_extensions(entries)
}

/// Parse a JSON fixture matching the `ListExtensions` dictionary schema (`{ "<uuid>": { ... } }`).
pub fn parse_extensions_json(raw: &str) -> Result<Vec<Package>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty GNOME Shell extensions JSON".into());
    }
    let doc: JsonValue = serde_json::from_str(trimmed)
        .map_err(|e| format!("invalid GNOME Shell extensions JSON: {e}"))?;
    let obj = doc
        .as_object()
        .ok_or_else(|| "GNOME Shell extensions JSON root must be an object".to_string())?;

    let mut entries = Vec::with_capacity(obj.len());
    for (key, val) in obj {
        let Some(props) = val.as_object() else {
            continue;
        };
        let uuid = props
            .get("uuid")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| key.trim())
            .to_string();
        let name = props
            .get("name")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string);
        let description = props
            .get("description")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string);
        let url = props
            .get("url")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string);
        let ext_type = props.get("type").and_then(json_as_u32);
        let has_update = props
            .get("hasUpdate")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let version = props.get("version").and_then(json_as_version_string);
        let version_name = props
            .get("version-name")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string);
        let target_version = props
            .get("update-version")
            .or_else(|| props.get("target-version"))
            .and_then(|v| {
                v.as_str()
                    .map(ToString::to_string)
                    .or_else(|| json_as_version_string(v))
            });

        entries.push(GnomeExtensionInfo {
            uuid,
            name,
            description,
            url,
            ext_type,
            has_update,
            version,
            version_name,
            target_version,
        });
    }

    Ok(packages_from_extensions(entries))
}

/// Extract deduplicated GNOME Shell extension UUIDs from pending packages.
pub fn extension_uuids_from_packages(packages: &[Package]) -> Vec<String> {
    let mut uuids = Vec::new();
    let mut seen = HashSet::new();
    for pkg in packages
        .iter()
        .filter(|p| p.source == UpdateSource::GnomeExtensions)
    {
        let uuid = pkg
            .id
            .strip_prefix("gnome-ext:")
            .unwrap_or(pkg.name.as_str())
            .trim();
        if !uuid.is_empty() && seen.insert(uuid.to_string()) {
            uuids.push(uuid.to_string());
        }
    }
    uuids
}

/// Map `InstallRemoteExtension(uuid)` D-Bus reply or error into `Ok(())` / actionable `Err`.
pub fn interpret_install_reply(uuid: &str, reply: Result<String, String>) -> Result<(), String> {
    match reply {
        Ok(status) => {
            let trimmed = status.trim();
            if trimmed.eq_ignore_ascii_case("successful") {
                Ok(())
            } else if trimmed.eq_ignore_ascii_case("cancelled") {
                Err(format!("Installation cancelled for {uuid}"))
            } else if trimmed.is_empty() {
                Err(format!("Empty reply installing {uuid}"))
            } else {
                Err(format!("Unexpected reply installing {uuid}: {trimmed}"))
            }
        }
        Err(err) => {
            let detail = err.trim();
            if detail.is_empty() {
                Err(format!("Failed to install {uuid}"))
            } else {
                Err(format!("Failed to install {uuid}: {detail}"))
            }
        }
    }
}

/// Check for pending user-installed GNOME Shell extension updates over session D-Bus.
pub fn check_updates(on_line: &mut dyn FnMut(String)) -> NativeOutcome<Vec<Package>> {
    let proxy = match open_extensions_proxy(CHECK_DBUS_TIMEOUT) {
        Ok(p) => p,
        Err(NativeOutcome::Unavailable(reason)) => return NativeOutcome::Unavailable(reason),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };

    on_line("Checking GNOME Shell extensions for updates…".into());
    let mut check_warning: Option<String> = None;
    match proxy.call::<_, _, ()>("CheckForUpdates", &()) {
        Ok(()) => {}
        Err(e) if is_session_dbus_unavailable(&e) => {
            return NativeOutcome::Unavailable(e.to_string());
        }
        Err(e) => {
            let msg = format!("GNOME Shell CheckForUpdates failed: {e}");
            on_line(format!("Warning: {msg}"));
            check_warning = Some(msg);
        }
    }

    let raw_map: HashMap<String, HashMap<String, OwnedValue>> =
        match proxy.call("ListExtensions", &()) {
            Ok(m) => m,
            Err(e) if is_session_dbus_unavailable(&e) => {
                return NativeOutcome::Unavailable(e.to_string());
            }
            Err(e) => {
                return NativeOutcome::Handled(Err(format!(
                    "GNOME Shell ListExtensions failed: {e}"
                )));
            }
        };

    let packages = parse_extensions_map(&raw_map);
    if packages.is_empty() {
        if let Some(warn) = check_warning {
            return NativeOutcome::Handled(Err(warn));
        }
    }

    on_line(format!("{} GNOME Shell extension update(s)", packages.len()));
    for pkg in &packages {
        let ver = match pkg.old_version.as_deref() {
            Some(old) => format!("{old} → {}", pkg.version),
            None => pkg.version.clone(),
        };
        on_line(format!("  {} ({ver})", pkg.name));
    }

    NativeOutcome::Handled(Ok(packages))
}

/// Core extension install loop shared between live session D-Bus and offline unit tests.
pub fn apply_extensions_with<F>(
    uuids: &[String],
    cancel: &WorkCancel,
    on_line: &mut dyn FnMut(String),
    on_progress: &mut dyn FnMut(ProgressHint),
    mut install_one: F,
) -> GnomeExtensionsApplyOutcome
where
    F: FnMut(&str) -> Result<String, String>,
{
    let mut outcome = GnomeExtensionsApplyOutcome::default();
    let total = u32::try_from(uuids.len()).unwrap_or(u32::MAX);

    for (idx, uuid) in uuids.iter().enumerate() {
        if cancel.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        let step = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        on_line(format!("Installing GNOME Shell extension {uuid}…"));
        on_progress(ProgressHint {
            package_name: Some(uuid.clone()),
            status: Some(PackageStatus::Installing),
            progress: None,
            phase_label: Some("Installing extension".into()),
            stage_current: Some(step),
            stage_total: Some(total),
        });

        let reply = install_one(uuid);
        match interpret_install_reply(uuid, reply) {
            Ok(()) => {
                outcome.completed += 1;
                on_line(format!("Updated GNOME Shell extension {uuid}"));
                on_progress(ProgressHint {
                    package_name: Some(uuid.clone()),
                    status: Some(PackageStatus::Completed),
                    progress: Some(1.0),
                    phase_label: Some("Extension updated".into()),
                    stage_current: Some(step),
                    stage_total: Some(total),
                });
            }
            Err(reason) => {
                outcome.failed += 1;
                on_line(format!("error: {reason}"));
                on_progress(ProgressHint {
                    package_name: Some(uuid.clone()),
                    status: Some(PackageStatus::Failed),
                    progress: None,
                    phase_label: Some("Extension failed".into()),
                    stage_current: Some(step),
                    stage_total: Some(total),
                });
                outcome.errors.push(reason);
            }
        }
    }

    outcome
}

/// Install selected GNOME Shell extension updates via `InstallRemoteExtension(uuid)`.
pub fn apply_gnome_extensions(
    uuids: &[String],
    cancel: &WorkCancel,
    on_line: &mut dyn FnMut(String),
    on_progress: &mut dyn FnMut(ProgressHint),
) -> NativeOutcome<GnomeExtensionsApplyOutcome> {
    if cancel.is_cancelled() {
        return NativeOutcome::Handled(Ok(GnomeExtensionsApplyOutcome {
            cancelled: true,
            ..Default::default()
        }));
    }
    if uuids.is_empty() {
        return NativeOutcome::Handled(Ok(GnomeExtensionsApplyOutcome::default()));
    }

    let proxy = match open_extensions_proxy(INSTALL_DBUS_TIMEOUT) {
        Ok(p) => p,
        Err(NativeOutcome::Unavailable(reason)) => return NativeOutcome::Unavailable(reason),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };

    let outcome = apply_extensions_with(uuids, cancel, on_line, on_progress, |uuid| {
        proxy
            .call::<_, _, String>("InstallRemoteExtension", &(uuid,))
            .map_err(|e| e.to_string())
    });

    NativeOutcome::Handled(Ok(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned_str(s: &str) -> OwnedValue {
        Value::from(s).try_into().unwrap()
    }

    fn owned_bool(b: bool) -> OwnedValue {
        Value::from(b).try_into().unwrap()
    }

    fn owned_f64(v: f64) -> OwnedValue {
        Value::from(v).try_into().unwrap()
    }

    fn owned_u32(v: u32) -> OwnedValue {
        Value::from(v).try_into().unwrap()
    }

    #[test]
    fn service_availability_guards_against_kde_sway_false_positives() {
        assert!(is_service_available(true, false, false));
        assert!(is_service_available(true, true, true));
        assert!(is_service_available(false, true, true));
        assert!(!is_service_available(false, false, true));
        assert!(!is_service_available(false, true, false));
        assert!(!is_service_available(false, false, false));
    }

    #[test]
    fn timeout_errors_are_not_misclassified_as_dbus_unavailable() {
        let timed_out = zbus::Error::from(std::io::Error::new(ErrorKind::TimedOut, "timed out"));
        assert!(is_timeout_error(&timed_out));
        assert!(!is_session_dbus_unavailable(&timed_out));

        let no_such_file = zbus::Error::from(std::io::Error::new(
            ErrorKind::NotFound,
            "No such file or directory",
        ));
        assert!(!is_timeout_error(&no_such_file));
        assert!(is_session_dbus_unavailable(&no_such_file));
    }

    #[test]
    fn parse_extensions_map_filters_user_vs_system_and_extracts_metadata() {
        let mut raw: HashMap<String, HashMap<String, OwnedValue>> = HashMap::new();

        // System RPM extension (type == 1) even with hasUpdate == true must be skipped.
        let mut sys_ext = HashMap::new();
        sys_ext.insert("uuid".into(), owned_str("apps-menu@gnome.org"));
        sys_ext.insert("name".into(), owned_str("Apps Menu"));
        sys_ext.insert("type".into(), owned_f64(1.0));
        sys_ext.insert("hasUpdate".into(), owned_bool(true));
        sys_ext.insert("version-name".into(), owned_str("50.4"));
        raw.insert("apps-menu@gnome.org".into(), sys_ext);

        // Up-to-date user extension (type == 2, hasUpdate == false) must be skipped.
        let mut current_user = HashMap::new();
        current_user.insert("uuid".into(), owned_str("dash-to-dock@micxgx.gmail.com"));
        current_user.insert("name".into(), owned_str("Dash to Dock"));
        current_user.insert("type".into(), owned_f64(2.0));
        current_user.insert("hasUpdate".into(), owned_bool(false));
        current_user.insert("version".into(), owned_f64(98.0));
        raw.insert("dash-to-dock@micxgx.gmail.com".into(), current_user);

        // User extension with full metadata and GJS f64 type/version.
        let mut blur = HashMap::new();
        blur.insert("uuid".into(), owned_str("blur-my-shell@aunetx"));
        blur.insert("name".into(), owned_str("Blur my Shell"));
        blur.insert(
            "description".into(),
            owned_str("Add a blur look to different parts of the GNOME Shell"),
        );
        blur.insert(
            "url".into(),
            owned_str("https://github.com/aunetx/blur-my-shell"),
        );
        blur.insert("type".into(), owned_f64(2.0));
        blur.insert("hasUpdate".into(), owned_bool(true));
        blur.insert("version".into(), owned_f64(66.0));
        raw.insert("blur-my-shell@aunetx".into(), blur);

        // User extension with missing optional fields and u32 version wrapped in nested Value::Value.
        let mut minimal = HashMap::new();
        minimal.insert("type".into(), owned_u32(2));
        let boxed_true = OwnedValue::try_from(Value::Value(Box::new(Value::Bool(true)))).unwrap();
        minimal.insert("hasUpdate".into(), boxed_true);
        minimal.insert("version".into(), owned_u32(14));
        raw.insert("alphabetical@example.org".into(), minimal);

        // User extension with both semantic version-name and numeric version.
        let mut gsconnect = HashMap::new();
        gsconnect.insert("uuid".into(), owned_str("gsconnect@andyholmes.github.io"));
        gsconnect.insert("name".into(), owned_str("GSConnect"));
        gsconnect.insert("type".into(), owned_f64(2.0));
        gsconnect.insert("hasUpdate".into(), owned_bool(true));
        gsconnect.insert("version".into(), owned_f64(57.0));
        gsconnect.insert("version-name".into(), owned_str("57.1.0"));
        raw.insert("gsconnect@andyholmes.github.io".into(), gsconnect);

        let pkgs = parse_extensions_map(&raw);
        assert_eq!(pkgs.len(), 3);

        // Sorted deterministically by lowercase name: alphabetical, Blur my Shell, GSConnect.
        assert_eq!(pkgs[0].id, "gnome-ext:alphabetical@example.org");
        assert_eq!(pkgs[0].name, "alphabetical@example.org");
        assert_eq!(pkgs[0].old_version.as_deref(), Some("v14"));
        assert_eq!(pkgs[0].version, "update available");
        assert!(pkgs[0].summary.is_none());
        assert!(pkgs[0].url.is_none());

        assert_eq!(pkgs[1].id, "gnome-ext:blur-my-shell@aunetx");
        assert_eq!(pkgs[1].name, "Blur my Shell");
        assert_eq!(pkgs[1].old_version.as_deref(), Some("v66"));
        assert_eq!(pkgs[1].version, "update available");
        assert_eq!(pkgs[1].repo, "extensions.gnome.org");
        assert_eq!(
            pkgs[1].summary.as_deref(),
            Some("Add a blur look to different parts of the GNOME Shell")
        );
        assert_eq!(
            pkgs[1].url.as_deref(),
            Some("https://github.com/aunetx/blur-my-shell")
        );

        assert_eq!(pkgs[2].id, "gnome-ext:gsconnect@andyholmes.github.io");
        assert_eq!(pkgs[2].old_version.as_deref(), Some("57.1.0 (v57)"));
        assert_eq!(
            extension_uuids_from_packages(&pkgs),
            vec![
                "alphabetical@example.org",
                "blur-my-shell@aunetx",
                "gsconnect@andyholmes.github.io"
            ]
        );
    }

    #[test]
    fn interpret_install_reply_and_apply_extensions_with_cover_success_cancel_and_partial_failure() {
        assert_eq!(interpret_install_reply("a@b", Ok("successful".into())), Ok(()));
        assert!(interpret_install_reply("a@b", Ok("cancelled".into()))
            .unwrap_err()
            .contains("cancelled"));
        assert!(interpret_install_reply("a@b", Ok("unknown".into()))
            .unwrap_err()
            .contains("Unexpected reply"));
        assert!(interpret_install_reply("a@b", Err("Download failed".into()))
            .unwrap_err()
            .contains("Download failed"));

        let uuids = vec!["ext-a@example.org".to_string(), "ext-b@example.org".to_string()];
        let cancel = WorkCancel::new();
        let mut lines = Vec::new();
        let mut hints = Vec::new();
        let outcome = apply_extensions_with(
            &uuids,
            &cancel,
            &mut |l| lines.push(l),
            &mut |h| hints.push(h),
            |uuid| {
                if uuid == "ext-a@example.org" {
                    Ok("successful".into())
                } else {
                    Ok("cancelled".into())
                }
            },
        );
        assert_eq!(outcome.completed, 1);
        assert_eq!(outcome.failed, 1);
        assert!(!outcome.cancelled);
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.errors[0].contains("cancelled"));
        assert!(hints.iter().any(|h| {
            h.package_name.as_deref() == Some("ext-a@example.org")
                && h.status == Some(PackageStatus::Completed)
        }));
        assert!(hints.iter().any(|h| {
            h.package_name.as_deref() == Some("ext-b@example.org")
                && h.status == Some(PackageStatus::Failed)
        }));

        // Mid-batch WorkCancel skips subsequent UUIDs cleanly.
        let cancel_mid = WorkCancel::new();
        let cancel_trigger = cancel_mid.clone();
        let mut called = Vec::new();
        let mut lines_c = Vec::new();
        let mut hints_c = Vec::new();
        let outcome_c = apply_extensions_with(
            &uuids,
            &cancel_mid,
            &mut |l| lines_c.push(l),
            &mut |h| hints_c.push(h),
            |uuid| {
                called.push(uuid.to_string());
                cancel_trigger.cancel();
                Ok("successful".into())
            },
        );
        assert_eq!(called, vec!["ext-a@example.org"]);
        assert_eq!(outcome_c.completed, 1);
        assert!(outcome_c.cancelled);
    }

    #[test]
    #[ignore = "probes live session org.gnome.Shell.Extensions on host"]
    fn live_session_gnome_extensions_probe() {
        if !is_available() {
            return;
        }
        let mut lines = Vec::new();
        let outcome = check_updates(&mut |l| lines.push(l));
        assert!(
            matches!(outcome, NativeOutcome::Handled(Ok(_))),
            "unexpected live GNOME Shell Extensions outcome: {outcome:?}"
        );
    }
}
