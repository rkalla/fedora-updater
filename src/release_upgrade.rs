//! Notice that a newer Fedora Linux release is Active.
//!
//! The list is the same public collections document GNOME Software reads.
//! This module only decides whether to show a reminder. It never downloads
//! or installs a release.

use serde::Deserialize;

/// Same document GNOME Software's fedora-pkgdb-collections plugin fetches.
pub const COLLECTIONS_URL: &str = "https://admin.fedoraproject.org/pkgdb/api/collections/";

pub const UPGRADE_DIALOG_HEADING: &str = "Upgrade from the terminal";

#[derive(Debug, Deserialize)]
struct CollectionsDoc {
    collections: Vec<Collection>,
}

#[derive(Debug, Deserialize)]
struct Collection {
    status: String,
    version: String,
    #[serde(default)]
    date_updated: Option<String>,
}

/// A newer Active release, plus how many calendar days it has been Active.
///
/// `days_since_release` is absent when the collections row has no usable date.
/// The reminder still shows; only the age line is hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOffer {
    pub version: u32,
    pub days_since_release: Option<u32>,
}

/// Highest `Active` release number strictly above `current`.
///
/// `devel`, `EOL`, and any version that is not a number are ignored.
/// A document that cannot be read offers nothing.
pub fn newest_active_newer_than(json: &str, current: u32) -> Option<u32> {
    let doc: CollectionsDoc = serde_json::from_str(json).ok()?;
    doc.collections
        .iter()
        .filter(|item| item.status == "Active")
        .filter_map(|item| item.version.parse::<u32>().ok())
        .filter(|version| *version > current)
        .max()
}

/// `VERSION_ID` from an os-release document. Quotes are optional.
pub fn os_release_version(text: &str) -> Option<u32> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("VERSION_ID=") else {
            continue;
        };
        let rest = rest.trim().trim_matches('"').trim_matches('\'');
        if let Ok(version) = rest.parse() {
            return Some(version);
        }
    }
    None
}

/// Calendar days from an Active row's `date_updated` to `today` (`YYYY-MM-DD`).
/// A missing date, a date in the future, or a version that is not Active yields nothing.
pub fn days_since_active(json: &str, version: u32, today: &str) -> Option<u32> {
    let doc: CollectionsDoc = serde_json::from_str(json).ok()?;
    let stamp = doc
        .collections
        .iter()
        .find(|item| item.status == "Active" && item.version.parse::<u32>().ok() == Some(version))?
        .date_updated
        .as_deref()?;
    days_between(stamp, today)
}

/// Whole calendar days from `release_stamp` to `today`.
/// Each value is `YYYY-MM-DD`, optionally followed by a time.
pub fn days_between(release_stamp: &str, today: &str) -> Option<u32> {
    let release = civil_days(release_stamp)?;
    let today = civil_days(today)?;
    let delta = today.checked_sub(release)?;
    u32::try_from(delta).ok()
}

fn civil_days(stamp: &str) -> Option<i64> {
    let date = stamp.trim().get(..10)?;
    let mut parts = date.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !is_civil_date(year, month, day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

fn is_civil_date(year: i32, month: u32, day: u32) -> bool {
    if !(1990..=2100).contains(&year) || !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let last = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return false,
    };
    day <= last
}

/// Days since 1970-01-01. Howard Hinnant's civil calendar algorithm.
fn days_from_civil(mut year: i32, month: u32, day: u32) -> i64 {
    year -= i32::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = (year - era * 400) as u32;
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era as i64 * 146097 + i64::from(day_of_era) - 719468
}

pub fn offer_from_collections(json: &str, current: u32, today: &str) -> Option<ReleaseOffer> {
    let version = newest_active_newer_than(json, current)?;
    Some(ReleaseOffer {
        version,
        days_since_release: days_since_active(json, version, today),
    })
}

pub fn release_age_caption(days: u32) -> String {
    match days {
        0 => "(Released today)".to_string(),
        1 => "(1 day since release)".to_string(),
        n => format!("({n} days since release)"),
    }
}

pub fn banner_title(version: u32) -> String {
    format!("Fedora Linux {version} is available")
}

pub fn upgrade_button_name(version: u32) -> String {
    format!("How to upgrade to Fedora Linux {version}")
}

/// Download, then reboot. Two steps on purpose: the reboot must not be chained
/// onto the download, because dnf stops for confirmation and a chained reboot
/// would run as soon as the download exits.
pub fn upgrade_command_lines(version: u32) -> [String; 2] {
    [
        format!("sudo dnf system-upgrade download --releasever={version}"),
        "sudo dnf system-upgrade reboot".to_string(),
    ]
}

pub fn upgrade_commands(version: u32) -> String {
    upgrade_command_lines(version).join("\n")
}

pub fn upgrade_dialog_body() -> &'static str {
    "This app does not run a release upgrade. Download the new release, then reboot to install it."
}

/// Read this machine's os-release and the public collections list.
/// Nothing is offered when either read fails. The age uses the local calendar day.
pub fn lookup_installed() -> Option<ReleaseOffer> {
    let os = std::fs::read_to_string("/etc/os-release").ok()?;
    let current = os_release_version(&os)?;
    let json = fetch_collections()?;
    let today = today_local()?;
    offer_from_collections(&json, current, &today)
}

fn today_local() -> Option<String> {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    if now == -1 {
        return None;
    }
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    ))
}

fn fetch_collections() -> Option<String> {
    let output = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "20", COLLECTIONS_URL])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLLECTIONS: &str = r#"{
      "collections": [
        {"status": "Under Development", "version": "devel", "name": "Fedora Linux"},
        {"status": "Active", "version": "45", "name": "Fedora Linux"},
        {"status": "Active", "version": "44", "name": "Fedora Linux"},
        {"status": "EOL", "version": "42", "name": "Fedora Linux"}
      ]
    }"#;

    #[test]
    fn active_45_is_offered_to_a_44_install() {
        assert_eq!(newest_active_newer_than(COLLECTIONS, 44), Some(45));
    }

    #[test]
    fn a_machine_already_on_the_newest_active_release_is_offered_nothing() {
        assert_eq!(newest_active_newer_than(COLLECTIONS, 45), None);
    }

    #[test]
    fn the_offer_is_the_highest_active_number_not_the_next_one() {
        assert_eq!(newest_active_newer_than(COLLECTIONS, 43), Some(45));
    }

    #[test]
    fn a_bad_collections_document_offers_nothing() {
        assert_eq!(newest_active_newer_than("not json", 44), None);
        assert_eq!(newest_active_newer_than("{}", 44), None);
        assert_eq!(newest_active_newer_than(r#"{"collections":[]}"#, 44), None);
    }

    #[test]
    fn os_release_version_reads_a_bare_or_quoted_id() {
        assert_eq!(
            os_release_version("NAME=\"Fedora Linux\"\nVERSION_ID=44\n"),
            Some(44)
        );
        assert_eq!(os_release_version("VERSION_ID=\"44\"\n"), Some(44));
        assert_eq!(os_release_version("NAME=Fedora\n"), None);
    }

    #[test]
    fn reminder_copy_names_the_detected_release_and_the_two_commands() {
        assert_eq!(banner_title(45), "Fedora Linux 45 is available");
        assert_eq!(upgrade_button_name(45), "How to upgrade to Fedora Linux 45");
        assert_eq!(
            upgrade_commands(45),
            "sudo dnf system-upgrade download --releasever=45\nsudo dnf system-upgrade reboot"
        );
        assert!(upgrade_dialog_body().contains("does not run a release upgrade"));
    }

    #[test]
    fn upgrade_commands_are_two_separate_steps() {
        let lines = upgrade_command_lines(45);
        assert_eq!(
            lines,
            [
                "sudo dnf system-upgrade download --releasever=45".to_string(),
                "sudo dnf system-upgrade reboot".to_string(),
            ]
        );
        assert!(lines
            .iter()
            .all(|line| !line.contains("&&") && !line.contains('\n')));
        assert_eq!(upgrade_commands(45), lines.join("\n"));
    }

    #[test]
    fn release_age_is_calendar_days_from_the_active_row() {
        let json = r#"{
          "collections": [
            {"status": "Active", "version": "45", "name": "Fedora Linux",
             "date_updated": "2026-11-03 14:30:00"},
            {"status": "Active", "version": "44", "name": "Fedora Linux",
             "date_updated": "2026-04-28 14:30:00"}
          ]
        }"#;
        assert_eq!(days_since_active(json, 45, "2026-11-15"), Some(12));
        assert_eq!(days_since_active(json, 45, "2026-11-03"), Some(0));
        assert_eq!(days_since_active(json, 45, "2026-11-04"), Some(1));
        assert_eq!(days_since_active(json, 45, "2026-11-02"), None);
        assert_eq!(days_since_active(COLLECTIONS, 45, "2026-11-15"), None);
        assert_eq!(days_since_active(json, 45, "not-a-date"), None);
        assert_eq!(days_between("2024-02-28", "2024-03-01"), Some(2));
        assert_eq!(days_between("2026-04-28", "2026-11-03"), Some(189));
    }

    #[test]
    fn release_age_caption_uses_the_parenthetical() {
        assert_eq!(release_age_caption(0), "(Released today)");
        assert_eq!(release_age_caption(1), "(1 day since release)");
        assert_eq!(release_age_caption(12), "(12 days since release)");
    }

    #[test]
    fn an_offer_carries_the_age_when_the_active_row_has_a_date() {
        let json = r#"{
          "collections": [
            {"status": "Active", "version": "45", "name": "Fedora Linux",
             "date_updated": "2026-11-03 14:30:00"}
          ]
        }"#;
        assert_eq!(
            offer_from_collections(json, 44, "2026-11-15"),
            Some(ReleaseOffer {
                version: 45,
                days_since_release: Some(12),
            })
        );
        assert_eq!(
            offer_from_collections(COLLECTIONS, 44, "2026-11-15"),
            Some(ReleaseOffer {
                version: 45,
                days_since_release: None,
            })
        );
    }
}
