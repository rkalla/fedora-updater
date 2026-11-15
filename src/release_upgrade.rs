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

pub fn banner_title(version: u32) -> String {
    format!("Fedora Linux {version} is available")
}

pub fn upgrade_button_name(version: u32) -> String {
    format!("How to upgrade to Fedora Linux {version}")
}

pub fn upgrade_commands(version: u32) -> String {
    format!(
        "sudo dnf system-upgrade download --releasever={version}\nsudo dnf system-upgrade reboot"
    )
}

pub fn upgrade_dialog_body() -> &'static str {
    "This app does not run a release upgrade. Download the new release, then reboot to install it."
}

/// Read this machine's os-release and the public collections list.
/// Nothing is offered when either read fails.
pub fn lookup_installed() -> Option<u32> {
    let os = std::fs::read_to_string("/etc/os-release").ok()?;
    let current = os_release_version(&os)?;
    let json = fetch_collections()?;
    newest_active_newer_than(&json, current)
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
}
