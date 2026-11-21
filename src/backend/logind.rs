//! Native `systemd-logind` (`org.freedesktop.login1.Manager`) D-Bus integration.
//!
//! Queries `CanReboot()` and `ListInhibitors()` to detect active shutdown block
//! inhibitors before invoking `Reboot(true)`, while falling back cleanly when
//! `org.freedesktop.login1` is unavailable.

use std::fmt;

use serde::Deserialize;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, Proxy};

use super::NativeOutcome;

pub const LOGIND_DEST: &str = "org.freedesktop.login1";
pub const LOGIND_PATH: &str = "/org/freedesktop/login1";
pub const LOGIND_IFACE: &str = "org.freedesktop.login1.Manager";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanRebootStatus {
    Yes,
    Challenge,
    No,
    Na,
    Other(String),
}

impl CanRebootStatus {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "yes" => Self::Yes,
            "challenge" => Self::Challenge,
            "no" => Self::No,
            "na" => Self::Na,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Yes | Self::Challenge)
    }
}

impl fmt::Display for CanRebootStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yes => f.write_str("yes"),
            Self::Challenge => f.write_str("challenge"),
            Self::No => f.write_str("no"),
            Self::Na => f.write_str("na"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// Active inhibitor entry returned by `org.freedesktop.login1.Manager.ListInhibitors`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ShutdownInhibitor {
    pub what: String,
    pub who: String,
    pub why: String,
    pub mode: String,
    #[serde(default)]
    pub uid: u32,
    #[serde(default)]
    pub pid: u32,
}

impl ShutdownInhibitor {
    pub fn from_tuple(raw: (String, String, String, String, u32, u32)) -> Self {
        Self {
            what: raw.0,
            who: raw.1,
            why: raw.2,
            mode: raw.3,
            uid: raw.4,
            pid: raw.5,
        }
    }

    /// True only when this entry actively blocks `shutdown` (ignoring `delay`
    /// locks and `sleep`/`handle-*-key` locks).
    pub fn blocks_shutdown(&self) -> bool {
        self.mode.trim().eq_ignore_ascii_case("block")
            && self
                .what
                .split(':')
                .any(|tok| tok.trim().eq_ignore_ascii_case("shutdown"))
    }
}

/// Result of pre-flight reboot capability + shutdown inhibitor checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebootPreflight {
    pub can_reboot: CanRebootStatus,
    pub blockers: Vec<ShutdownInhibitor>,
}

impl RebootPreflight {
    pub fn from_raw(can_reboot: &str, inhibitors: Vec<ShutdownInhibitor>) -> Self {
        let can_reboot = CanRebootStatus::parse(can_reboot);
        let blockers = inhibitors
            .into_iter()
            .filter(ShutdownInhibitor::blocks_shutdown)
            .collect();
        Self {
            can_reboot,
            blockers,
        }
    }

    /// Actionable explanation when reboot is currently blocked or disallowed.
    pub fn blocking_detail(&self) -> Option<String> {
        if !self.can_reboot.is_allowed() {
            return Some(match &self.can_reboot {
                CanRebootStatus::Na => {
                    "System restart is not supported in this environment (logind returned \"na\")."
                        .to_string()
                }
                other => format!(
                    "System restart is currently not permitted (logind CanReboot returned \"{other}\")."
                ),
            });
        }

        if self.blockers.is_empty() {
            return None;
        }

        if self.blockers.len() == 1 {
            let b = &self.blockers[0];
            let who = display_who(&b.who);
            let why = b.why.trim();
            return Some(if why.is_empty() {
                format!("Restart is currently blocked by {who}. Wait for it to finish, then try again.")
            } else {
                format!(
                    "Restart is currently blocked by {who} ({why}). Wait for it to finish, then try again."
                )
            });
        }

        let summary: Vec<String> = self
            .blockers
            .iter()
            .map(|b| {
                let who = display_who(&b.who);
                let why = b.why.trim();
                if why.is_empty() {
                    who.to_string()
                } else {
                    format!("{who} ({why})")
                }
            })
            .collect();
        Some(format!(
            "Restart is currently blocked by: {}. Wait for them to finish, then try again.",
            summary.join("; ")
        ))
    }
}

fn display_who(who: &str) -> &str {
    let trimmed = who.trim();
    if trimmed.is_empty() {
        "another process"
    } else {
        trimmed
    }
}

/// Classify `zbus` errors that indicate the target daemon or system D-Bus is absent.
pub(crate) fn is_dbus_unavailable(err: &zbus::Error) -> bool {
    match err {
        zbus::Error::InterfaceNotFound | zbus::Error::InputOutput(_) => true,
        zbus::Error::MethodError(name, _, _) => {
            let s = name.as_str();
            s == "org.freedesktop.DBus.Error.ServiceUnknown"
                || s == "org.freedesktop.DBus.Error.NameHasNoOwner"
                || s == "org.freedesktop.DBus.Error.UnknownObject"
                || s == "org.freedesktop.DBus.Error.UnknownInterface"
                || s == "org.freedesktop.DBus.Error.UnknownMethod"
                || s.starts_with("org.freedesktop.DBus.Error.Spawn.")
        }
        other => {
            let msg = other.to_string().to_ascii_lowercase();
            msg.contains("no such file or directory")
                || msg.contains("connection refused")
                || msg.contains("dbus_system_bus_address")
                || msg.contains("serviceunknown")
                || msg.contains("namehasnoowner")
        }
    }
}

fn open_logind_proxy() -> Result<Proxy<'static>, NativeOutcome<()>> {
    let conn = Connection::system().map_err(|e| {
        if is_dbus_unavailable(&e) {
            NativeOutcome::Unavailable(e.to_string())
        } else {
            NativeOutcome::Handled(Err(format!("Could not connect to system D-Bus: {e}")))
        }
    })?;

    // Check whether org.freedesktop.login1 exists on the bus (owned or activatable).
    if let Ok(dbus) = DBusProxy::new(&conn) {
        let owned = zbus::names::BusName::try_from(LOGIND_DEST)
            .ok()
            .and_then(|n| dbus.name_has_owner(n).ok())
            .unwrap_or(false);
        if !owned {
            let activatable = dbus
                .list_activatable_names()
                .map(|names| names.iter().any(|n| n.as_str() == LOGIND_DEST))
                .unwrap_or(false);
            if !activatable {
                return Err(NativeOutcome::Unavailable(
                    "org.freedesktop.login1 is not available on system D-Bus".into(),
                ));
            }
        }
    }

    Proxy::new(&conn, LOGIND_DEST, LOGIND_PATH, LOGIND_IFACE)
        .map_err(|e| NativeOutcome::Unavailable(e.to_string()))
}

/// Query `org.freedesktop.login1.Manager` `CanReboot()` and `ListInhibitors()`.
pub fn query_reboot_preflight() -> NativeOutcome<RebootPreflight> {
    let proxy = match open_logind_proxy() {
        Ok(p) => p,
        Err(NativeOutcome::Unavailable(reason)) => return NativeOutcome::Unavailable(reason),
        Err(NativeOutcome::Handled(Err(e))) => return NativeOutcome::Handled(Err(e)),
        Err(NativeOutcome::Handled(Ok(()))) => unreachable!(),
    };

    let can_reboot: String = match proxy.call("CanReboot", &()) {
        Ok(val) => val,
        Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
        Err(e) => {
            return NativeOutcome::Handled(Err(format!("logind CanReboot failed: {e}")));
        }
    };

    let raw_inhibitors: Vec<(String, String, String, String, u32, u32)> =
        match proxy.call("ListInhibitors", &()) {
            Ok(list) => list,
            Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
            Err(e) => {
                return NativeOutcome::Handled(Err(format!("logind ListInhibitors failed: {e}")));
            }
        };

    let inhibitors = raw_inhibitors
        .into_iter()
        .map(ShutdownInhibitor::from_tuple)
        .collect();
    NativeOutcome::Handled(Ok(RebootPreflight::from_raw(&can_reboot, inhibitors)))
}

/// Run pre-flight checks and invoke `org.freedesktop.login1.Manager.Reboot(true)`.
pub fn request_reboot_interactive(on_line: &mut dyn FnMut(String)) -> NativeOutcome<()> {
    let proxy = match open_logind_proxy() {
        Ok(p) => p,
        Err(out) => return out,
    };

    let can_reboot: String = match proxy.call("CanReboot", &()) {
        Ok(val) => val,
        Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
        Err(e) => {
            return NativeOutcome::Handled(Err(format!("logind CanReboot failed: {e}")));
        }
    };

    let raw_inhibitors: Vec<(String, String, String, String, u32, u32)> =
        match proxy.call("ListInhibitors", &()) {
            Ok(list) => list,
            Err(e) if is_dbus_unavailable(&e) => return NativeOutcome::Unavailable(e.to_string()),
            Err(e) => {
                return NativeOutcome::Handled(Err(format!("logind ListInhibitors failed: {e}")));
            }
        };

    let inhibitors: Vec<ShutdownInhibitor> = raw_inhibitors
        .into_iter()
        .map(ShutdownInhibitor::from_tuple)
        .collect();
    let preflight = RebootPreflight::from_raw(&can_reboot, inhibitors);
    on_line(format!(
        "logind CanReboot={can_reboot}, shutdown blockers={}",
        preflight.blockers.len()
    ));

    if let Some(detail) = preflight.blocking_detail() {
        on_line(detail.clone());
        return NativeOutcome::Handled(Err(detail));
    }

    on_line("Requesting interactive system restart via systemd-logind…".into());
    match proxy.call::<_, _, ()>("Reboot", &(true,)) {
        Ok(()) => NativeOutcome::Handled(Ok(())),
        Err(e) if is_dbus_unavailable(&e) => NativeOutcome::Unavailable(e.to_string()),
        Err(e) => {
            let msg = e.to_string();
            on_line(format!("logind Reboot failed: {msg}"));
            let mapped = if msg.to_ascii_lowercase().contains("inhibitor") {
                crate::orchestrator::reboot_failure_detail(&msg, 1)
            } else {
                msg
            };
            NativeOutcome::Handled(Err(mapped))
        }
    }
}

/// Query `OnExternalPower` property on `org.freedesktop.login1.Manager` if available.
pub fn query_on_external_power() -> Option<bool> {
    let proxy = open_logind_proxy().ok()?;
    proxy.get_property::<bool>("OnExternalPower").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_reboot_status_parses_all_variants() {
        assert_eq!(CanRebootStatus::parse("yes"), CanRebootStatus::Yes);
        assert!(CanRebootStatus::parse("yes").is_allowed());
        assert_eq!(
            CanRebootStatus::parse("challenge"),
            CanRebootStatus::Challenge
        );
        assert!(CanRebootStatus::parse("challenge").is_allowed());
        assert_eq!(CanRebootStatus::parse("no"), CanRebootStatus::No);
        assert!(!CanRebootStatus::parse("no").is_allowed());
        assert_eq!(CanRebootStatus::parse("na"), CanRebootStatus::Na);
        assert!(!CanRebootStatus::parse("na").is_allowed());
        assert_eq!(
            CanRebootStatus::parse(" custom "),
            CanRebootStatus::Other("custom".into())
        );
        assert!(!CanRebootStatus::parse("custom").is_allowed());
    }

    #[test]
    fn inhibitor_filtering_matches_only_shutdown_blockers() {
        let delay = ShutdownInhibitor {
            what: "shutdown:sleep".into(),
            who: "GNOME Shell".into(),
            why: "Cleanup".into(),
            mode: "delay".into(),
            uid: 1000,
            pid: 100,
        };
        assert!(!delay.blocks_shutdown());

        let power_key = ShutdownInhibitor {
            what: "handle-power-key:handle-lid-switch".into(),
            who: "GNOME Shell".into(),
            why: "GNOME handles keys".into(),
            mode: "block".into(),
            uid: 1000,
            pid: 101,
        };
        assert!(!power_key.blocks_shutdown());

        let fw_blocker = ShutdownInhibitor {
            what: "shutdown".into(),
            who: "dnf-automatic".into(),
            why: "RPM transaction in progress".into(),
            mode: "block".into(),
            uid: 0,
            pid: 200,
        };
        assert!(fw_blocker.blocks_shutdown());

        let multi_blocker = ShutdownInhibitor {
            what: "shutdown:sleep".into(),
            who: "fwupd".into(),
            why: "Flashing UEFI firmware".into(),
            mode: "block".into(),
            uid: 0,
            pid: 201,
        };
        assert!(multi_blocker.blocks_shutdown());

        let preflight = RebootPreflight::from_raw(
            "yes",
            vec![delay, power_key, fw_blocker.clone(), multi_blocker],
        );
        assert_eq!(preflight.blockers.len(), 2);
        let detail = preflight.blocking_detail().expect("blockers present");
        assert!(detail.contains("dnf-automatic"), "{detail}");
        assert!(detail.contains("RPM transaction in progress"), "{detail}");
        assert!(detail.contains("fwupd"), "{detail}");
        assert!(detail.contains("Flashing UEFI firmware"), "{detail}");
    }

    #[test]
    fn preflight_blocking_detail_covers_allowed_denied_and_single_blocker() {
        let ok = RebootPreflight::from_raw("yes", Vec::new());
        assert_eq!(ok.blocking_detail(), None);

        let denied = RebootPreflight::from_raw("no", Vec::new());
        let detail = denied.blocking_detail().expect("denied detail");
        assert!(detail.contains("not permitted"), "{detail}");

        let na = RebootPreflight::from_raw("na", Vec::new());
        let detail = na.blocking_detail().expect("na detail");
        assert!(detail.contains("not supported"), "{detail}");

        let one = RebootPreflight::from_raw(
            "yes",
            vec![ShutdownInhibitor {
                what: "shutdown".into(),
                who: "Steam".into(),
                why: "Finishing cloud sync".into(),
                mode: "block".into(),
                uid: 1000,
                pid: 42,
            }],
        );
        let detail = one.blocking_detail().expect("single blocker");
        assert!(detail.contains("Steam"), "{detail}");
        assert!(detail.contains("Finishing cloud sync"), "{detail}");
        assert!(detail.contains("try again"), "{detail}");
    }

    #[test]
    #[ignore = "probes live system org.freedesktop.login1 on host"]
    fn live_logind_preflight_probe() {
        let out = query_reboot_preflight();
        match out {
            NativeOutcome::Handled(Ok(pf)) => {
                assert!(matches!(
                    pf.can_reboot,
                    CanRebootStatus::Yes | CanRebootStatus::Challenge | CanRebootStatus::No | CanRebootStatus::Na
                ));
            }
            NativeOutcome::Handled(Err(e)) => panic!("live logind error: {e}"),
            NativeOutcome::Unavailable(e) => panic!("live logind unavailable: {e}"),
        }
    }
}
