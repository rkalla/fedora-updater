//! fwupd / `fwupdmgr` parsers.

use crate::model::{AdvisoryKind, Package, PackageStatus, UpdateSource};

use super::ProgressHint;

/// Parse `fwupdmgr get-updates` — prefers JSON, falls back to text devices.
pub fn parse_get_updates(output: &str) -> Vec<Package> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // JSON may be a single object or array, or preceded by noise
    if let Some(json_start) = trimmed.find(['{', '[']) {
        let json_part = &trimmed[json_start..];
        if let Ok(pkgs) = parse_json_updates(json_part) {
            if !pkgs.is_empty() || json_part.starts_with('{') || json_part.starts_with('[') {
                return pkgs;
            }
        }
    }
    parse_text_updates(trimmed)
}

fn parse_json_updates(json: &str) -> Result<Vec<Package>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let mut packages = Vec::new();

    // Common shapes:
    // { "Devices": [ { "Name": "...", "DeviceId": "...", "Releases": [ {"Version": "..."} ] } ] }
    // or a top-level array of devices
    let devices = if let Some(arr) = value.as_array() {
        arr.clone()
    } else if let Some(arr) = value.get("Devices").and_then(|d| d.as_array()) {
        arr.clone()
    } else if value.get("Name").is_some() || value.get("DeviceId").is_some() {
        vec![value]
    } else {
        // Empty object / no updates
        Vec::new()
    };

    for dev in devices {
        let name = dev
            .get("Name")
            .or_else(|| dev.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("Firmware device")
            .to_string();
        let device_id = dev
            .get("DeviceId")
            .or_else(|| dev.get("deviceId"))
            .or_else(|| dev.get("Id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let version = dev
            .get("Releases")
            .and_then(|r| r.as_array())
            .and_then(|arr| arr.first())
            .and_then(|rel| rel.get("Version").or_else(|| rel.get("version")))
            .and_then(|v| v.as_str())
            .or_else(|| dev.get("Version").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        let current = dev
            .get("Version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Only include devices that actually have a release/update signal
        let has_release = dev
            .get("Releases")
            .and_then(|r| r.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(!version.is_empty());
        if !has_release && device_id.is_empty() {
            continue;
        }

        let id = if device_id.is_empty() {
            format!("fwupd:{name}:{version}")
        } else {
            format!("fwupd:{device_id}")
        };

        let detail = match &current {
            Some(cur) if !version.is_empty() => format!("{cur} → {version}"),
            _ if !version.is_empty() => version.clone(),
            _ => "firmware update available".into(),
        };

        packages.push(Package {
            id,
            name,
            arch: String::new(),
            version,
            repo: "LVFS".into(),
            size: None,
            old_version: current,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: AdvisoryKind::Security, // firmware updates treated as important
            source: UpdateSource::Firmware,
            detail,
        });
    }

    Ok(packages)
}

fn parse_text_updates(output: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_id: Option<String> = None;
    let mut current_version: Option<String> = None;

    let flush = |packages: &mut Vec<Package>,
                 name: &mut Option<String>,
                 id: &mut Option<String>,
                 ver: &mut Option<String>| {
        if let Some(n) = name.take() {
            let device_id = id.take().unwrap_or_default();
            let version = ver.take().unwrap_or_default();
            let pkg_id = if device_id.is_empty() {
                format!("fwupd:{n}")
            } else {
                format!("fwupd:{device_id}")
            };
            packages.push(Package {
                id: pkg_id,
                name: n,
                arch: String::new(),
                version: version.clone(),
                repo: "LVFS".into(),
                size: None,
                old_version: None,
                status: PackageStatus::Pending,
                progress: 0.0,
                kind: AdvisoryKind::Security,
                source: UpdateSource::Firmware,
                detail: if version.is_empty() {
                    "firmware update available".into()
                } else {
                    version
                },
            });
        }
    };

    for line in output.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if lower.contains("no updates available") || lower.starts_with("no updatable") {
            return Vec::new();
        }
        // Device name lines are often not indented and not key:value
        if !t.contains(':') && !t.starts_with('│') && !t.starts_with('├') && !t.starts_with('└')
        {
            flush(
                &mut packages,
                &mut current_name,
                &mut current_id,
                &mut current_version,
            );
            current_name = Some(t.to_string());
            continue;
        }
        if let Some((k, v)) = t.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key.contains("device id") || key == "deviceid" || key == "id" {
                current_id = Some(val);
            } else if key.contains("version") && !key.contains("bootloader") {
                current_version = Some(val);
            } else if key == "name" {
                current_name = Some(val);
            }
        }
    }
    flush(
        &mut packages,
        &mut current_name,
        &mut current_id,
        &mut current_version,
    );
    packages
}

pub fn parse_progress_line(line: &str) -> ProgressHint {
    let lower = line.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    let mut hint = ProgressHint::default();
    // Post-apply inventory listings are not progress and must not match "update".
    if trimmed.starts_with("devices with")
        || trimmed.starts_with('•')
        || trimmed.starts_with('*')
        || trimmed.starts_with("- ")
    {
        return hint;
    }
    if lower.contains("successfully installed") {
        hint.status = Some(PackageStatus::Completed);
        hint.phase_label = Some("Firmware installed".into());
        hint.progress = Some(1.0);
    } else if lower.contains("downloading") {
        hint.status = Some(PackageStatus::Downloading);
        hint.phase_label = Some("Downloading firmware".into());
    } else if lower.contains("installing")
        || lower.contains("writing")
        || lower.contains("updating")
    {
        hint.status = Some(PackageStatus::Installing);
        hint.phase_label = Some("Installing firmware".into());
    } else if lower.contains("decompressing") {
        hint.phase_label = Some("Decompressing firmware".into());
    } else if lower.contains("verifying") {
        hint.phase_label = Some("Verifying firmware".into());
    } else if lower.contains("restarting device") {
        hint.phase_label = Some("Restarting device".into());
    } else if lower.contains("waiting") {
        hint.phase_label = Some("Waiting".into());
    }
    if hint.progress.is_none() {
        if let Some(pct) = extract_percent(line) {
            hint.progress = Some((pct / 100.0).clamp(0.0, 1.0));
        }
    }
    hint
}

fn extract_percent(line: &str) -> Option<f64> {
    for token in line.split_whitespace() {
        if let Some(num) = token.strip_suffix('%') {
            if let Ok(v) = num.parse::<f64>() {
                if (0.0..=100.0).contains(&v) {
                    return Some(v);
                }
            }
        }
    }
    // Percentage=45 style
    if let Some(idx) = lower_find(line, "percentage") {
        let rest = &line[idx..];
        for token in rest.split(|c: char| !c.is_ascii_digit() && c != '.') {
            if let Ok(v) = token.parse::<f64>() {
                if (0.0..=100.0).contains(&v) {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn lower_find(hay: &str, needle: &str) -> Option<usize> {
    hay.to_ascii_lowercase().find(needle)
}

pub fn detect_reboot_needed(log: &str) -> bool {
    let lower = log.to_ascii_lowercase();
    if lower.contains("no reboot") {
        return false;
    }
    // Do not match bare "restart" — "Restarting device…" is a per-device phase,
    // not a request to reboot the machine.
    lower.contains("requires a reboot")
        || lower.contains("reboot to complete")
        || lower.contains("needs-reboot")
        || lower.contains("pending reboot")
        || lower.contains("restart your system")
        || lower.contains("restart now")
}

/// `fwupdmgr check-reboot-needed --json`: exit 0 means a reboot is needed,
/// NOTHING_TO_DO (and other non-zero codes) means it is not.
/// Text output is a fallback for hosts that ignore `--json`.
pub fn parse_reboot_needed_output(output: &str, exit_code: i32) -> bool {
    let lower = output.to_ascii_lowercase();
    if lower.contains("no reboot") || lower.contains("nothing to do") {
        return false;
    }
    if exit_code == 0 {
        return true;
    }
    detect_reboot_needed(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_devices() {
        let json = r#"{
          "Devices": [
            {
              "Name": "UEFI Device Firmware",
              "DeviceId": "abc123",
              "Version": "1.0.0",
              "Releases": [ { "Version": "1.0.1" } ]
            }
          ]
        }"#;
        let pkgs = parse_get_updates(json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "UEFI Device Firmware");
        assert_eq!(pkgs[0].source, UpdateSource::Firmware);
        assert!(pkgs[0].detail.contains('→'));
    }

    #[test]
    fn parse_no_updates_text() {
        let pkgs = parse_get_updates("No updates available\n");
        assert!(pkgs.is_empty());
    }

    #[test]
    fn reboot_detection() {
        assert!(detect_reboot_needed(
            "An update requires a reboot to complete"
        ));
        assert!(!detect_reboot_needed("Successfully installed firmware"));
        assert!(
            !detect_reboot_needed("Restarting device…: 99.0%"),
            "device restart is not a system reboot"
        );
        assert!(
            !detect_reboot_needed("No reboot is necessary"),
            "negative phrasing must not count as needing a reboot"
        );
    }

    #[test]
    fn json_check_reboot_needed_uses_exit_zero_contract() {
        // fwupdmgr check-reboot-needed --json: 0 = reboot needed, NOTHING_TO_DO otherwise.
        assert!(parse_reboot_needed_output("", 0));
        assert!(parse_reboot_needed_output("{}", 0));
        assert!(!parse_reboot_needed_output("", 2));
        assert!(!parse_reboot_needed_output("No reboot is necessary", 2));
        assert!(parse_reboot_needed_output(
            "An update requires a reboot to complete",
            1
        ));
    }

    #[test]
    fn success_line_marks_firmware_complete() {
        let h = parse_progress_line("Successfully installed firmware");
        assert_eq!(h.status, Some(PackageStatus::Completed));
        assert_eq!(h.progress, Some(1.0));
        assert_eq!(h.phase_label.as_deref(), Some("Firmware installed"));
    }

    #[test]
    fn firmware_phase_lines_keep_work_progress() {
        let verifying = parse_progress_line("Verifying…: 1.0%");
        assert_eq!(verifying.phase_label.as_deref(), Some("Verifying firmware"));
        assert_eq!(verifying.progress, Some(0.01));

        let restarting = parse_progress_line("Restarting device…: 99.0%");
        assert_eq!(restarting.phase_label.as_deref(), Some("Restarting device"));
        assert_eq!(restarting.progress, Some(0.99));
        assert_ne!(restarting.status, Some(PackageStatus::Completed));

        let waiting = parse_progress_line("Waiting…: 100.0%");
        assert_eq!(waiting.phase_label.as_deref(), Some("Waiting"));
        assert_eq!(waiting.progress, Some(1.0));
    }

    #[test]
    fn inventory_listing_is_not_progress() {
        let h = parse_progress_line("Devices with no available firmware updates:");
        assert_eq!(h, ProgressHint::default());
        let h = parse_progress_line(" • System Firmware");
        assert_eq!(h, ProgressHint::default());
    }
}
