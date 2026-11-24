//! Offline sanity and integrity verification for the checked-in `tests/fixtures/` corpus.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::fixtures::{
    fixture_path, fixtures_root, read_fixture, read_fixture_bytes, read_fixture_value,
    read_flatpak_fixture, ALL_FIXTURES,
};
use fedora_updater::backend::flatpak::FlatpakOpKind;
use fedora_updater::model::UpdateSource;

fn collect_files(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {} failed: {e}", dir.display()))
    {
        let entry = entry.expect("dir entry");
        let path: PathBuf = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("strip prefix")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel);
        }
    }
}

#[test]
fn all_fixtures_exist_are_non_empty_and_indexed_in_readme() {
    assert_eq!(ALL_FIXTURES.len(), 74, "expected 74 fixtures in corpus");

    let expected: BTreeSet<String> = ALL_FIXTURES.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        expected.len(),
        ALL_FIXTURES.len(),
        "duplicate path in ALL_FIXTURES"
    );

    let root = fixtures_root();
    let mut on_disk = BTreeSet::new();
    collect_files(&root, &root, &mut on_disk);
    assert!(
        on_disk.remove("README.md"),
        "tests/fixtures/README.md must exist"
    );
    assert_eq!(
        on_disk, expected,
        "disk files under tests/fixtures/ must match ALL_FIXTURES exactly"
    );

    let readme = read_fixture("README.md");
    assert!(!readme.trim().is_empty());

    for rel in ALL_FIXTURES {
        let path = fixture_path(rel);
        assert!(path.is_file(), "missing fixture file: {}", path.display());
        let raw = read_fixture_bytes(rel);
        assert!(
            !raw.is_empty(),
            "fixture `{rel}` must not be empty ({})",
            path.display()
        );
        assert!(
            readme.contains(rel),
            "tests/fixtures/README.md must document `{rel}`"
        );
    }
}

#[test]
fn all_json_fixtures_parse_cleanly() {
    let json_files: Vec<&&str> = ALL_FIXTURES
        .iter()
        .filter(|rel| rel.ends_with(".json"))
        .collect();
    assert!(!json_files.is_empty());
    for rel in json_files {
        let value = read_fixture_value(rel);
        assert!(
            value.is_object() || value.is_array(),
            "JSON fixture `{rel}` must be an object or array"
        );
    }
}

#[test]
fn dnf_fixtures_contain_expected_edge_case_signals() {
    let classic = read_fixture("dnf/check_update_classic.txt");
    assert!(classic.contains("kernel-core.x86_64"));
    assert!(classic.contains("1:3.2.2-3.fc44"));
    assert!(classic.contains("32:9.18.50-2.fc44"));
    assert!(classic.contains("mesa-dri-drivers.i686"));
    assert!(classic.contains("mesa-dri-drivers.x86_64"));

    let dnf5 = read_fixture("dnf/check_update_dnf5_table.txt");
    assert!(dnf5.contains("Obsoleting packages:"));
    assert!(dnf5.contains("systemd"));
    assert!(dnf5.contains("MiB"));
    assert!(dnf5.contains("KiB"));

    let empty = read_fixture("dnf/check_update_empty.txt");
    assert!(empty.contains("Repositories loaded."));
    assert!(empty.contains("Last metadata expiration check:"));

    let repo_err = read_fixture("dnf/check_update_repo_error.txt");
    assert!(repo_err.contains("Curl error (6)"));
    assert!(repo_err.contains("Curl error (60)"));
    assert!(repo_err.contains("Failed to synchronize cache"));

    let repoquery = read_fixture("dnf/repoquery_sizes.txt");
    assert!(repoquery.contains('\t'), "repoquery fixture must use tabs");
    assert!(repoquery.contains("kernel-core\tx86_64\t7.1.12-200.fc44\t21727761"));
    assert!(repoquery.contains("bind-libs\tx86_64\t32:9.18.50-2.fc44\t1388459"));

    let happy_bytes = read_fixture_bytes("dnf/apply_happy_kernel.txt");
    assert!(
        happy_bytes.contains(&b'\r'),
        "apply_happy_kernel.txt must contain raw CR bytes for live download redraws"
    );
    let happy = String::from_utf8(happy_bytes).expect("utf-8 apply_happy_kernel.txt");
    assert!(happy.contains("Upgrading:"));
    assert!(happy.contains("Installing kernel-core"));
    assert!(happy.contains("Running transaction"));
    assert!(happy.contains("Complete!"));

    let conflict = read_fixture("dnf/apply_dep_conflict.txt");
    assert!(conflict.contains("Problem 1:"));
    assert!(conflict.contains("nothing provides"));
    assert!(conflict.contains("--skip-broken"));

    let lock = read_fixture("dnf/apply_rpm_lock_error.txt");
    assert!(lock.contains("/var/lib/rpm/.rpm.lock"));
    assert!(lock.contains("dnf5daemon-server"));
}

#[test]
fn flatpak_fixtures_deserialize_and_contain_expected_signals() {
    let user = read_flatpak_fixture("flatpak/check_user_normal.json");
    assert_eq!(user.update_source(), UpdateSource::FlatpakUser);
    assert_eq!(user.refs.len(), 3);
    assert_eq!(user.ops.len(), 3);
    assert!(user.refs.iter().any(|r| r.version.is_empty() && !r.branch.is_empty()));
    assert!(user.refs.iter().any(|r| r.summary.is_some()));

    let sys = read_flatpak_fixture("flatpak/check_system_nvidia_extension.json");
    assert_eq!(sys.update_source(), UpdateSource::FlatpakSystem);
    assert!(sys.refs.iter().any(|r| r.summary.is_some()));
    assert!(sys
        .ops
        .iter()
        .any(|o| o.kind == FlatpakOpKind::Install && o.formatted_ref.contains("nvidia")));
    assert!(sys
        .ops
        .iter()
        .any(|o| o.kind == FlatpakOpKind::Install && o.related_to.len() == 2));
    assert!(sys.ops.iter().any(|o| o.kind == FlatpakOpKind::Uninstall));

    let happy = read_fixture("flatpak/apply_progress_happy.txt");
    assert!(happy.contains("Downloading "));
    assert!(happy.contains("Updating "));
    assert!(happy.contains("Installing "));
    assert!(happy.contains("Updates complete."));

    let noop = read_fixture("flatpak/apply_nothing_to_update.txt");
    assert!(noop.contains("Nothing to update."));

    let remote_err = read_fixture("flatpak/apply_remote_error.txt");
    assert!(remote_err.contains("error: Failed to update"));
    assert!(remote_err.contains("Socket I/O timed out"));

    let native_fx = read_fixture_value("flatpak/native_transaction_events.json");
    assert_eq!(native_fx["ops"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        native_fx["progress_samples"].as_array().map(Vec::len),
        Some(3)
    );
}

#[test]
fn fwupd_fixtures_contain_expected_signals() {
    let multi = read_fixture_value("fwupd/get_updates_multi_device.json");
    let devices = multi["Devices"].as_array().expect("Devices array");
    assert_eq!(devices.len(), 3);
    assert!(devices[2]["Releases"]
        .as_array()
        .is_some_and(|r| r.len() == 2));

    let empty = read_fixture_value("fwupd/get_updates_empty.json");
    assert_eq!(empty["Devices"].as_array().map(Vec::len), Some(0));

    let corrupt = read_fixture("fwupd/get_updates_corrupt.txt");
    assert!(serde_json::from_str::<serde_json::Value>(&corrupt).is_err());

    let text = read_fixture("fwupd/get_updates_text.txt");
    assert!(text.contains("Device ID:"));
    assert!(text.contains("Bootloader Version:"));
    assert!(text.contains("├─"));
    assert!(text.contains("└─"));

    let no_updates = read_fixture("fwupd/get_updates_no_updates.txt");
    assert!(no_updates.contains("Devices with no available firmware updates:"));
    assert!(no_updates.contains(" • System Firmware"));

    let happy = read_fixture("fwupd/apply_happy_reboot.txt");
    for expected in [
        "Downloading…",
        "Decompressing…",
        "Writing… Percentage=60",
        "Verifying…",
        "Restarting device…",
        "Successfully installed firmware",
        "requires a reboot to complete",
    ] {
        assert!(happy.contains(expected), "missing `{expected}`");
    }

    let battery = read_fixture("fwupd/apply_battery_error.txt");
    assert!(battery.contains("battery power"));
    assert!(battery.contains("connect AC power"));

    let reboot_true = read_fixture_value("fwupd/check_reboot_needed_true.json");
    assert!(reboot_true["Devices"][0]["Flags"]
        .as_array()
        .is_some_and(|flags| flags.iter().any(|f| f == "needs-reboot")));

    let reboot_false = read_fixture_value("fwupd/check_reboot_needed_false.json");
    assert_eq!(
        reboot_false["Devices"].as_array().map(Vec::len),
        Some(0)
    );

    let dbus_devs = read_fixture_value("fwupd/dbus_devices_and_upgrades.json");
    assert_eq!(dbus_devs["Devices"].as_array().map(Vec::len), Some(3));
    assert_eq!(dbus_devs["HostPower"]["OnExternalPower"], false);

    let dbus_hist = read_fixture_value("fwupd/dbus_history.json");
    assert_eq!(dbus_hist.as_array().map(Vec::len), Some(2));
}

#[test]
fn release_upgrade_and_systemctl_fixtures_contain_expected_signals() {
    let f45 = read_fixture_value("release_upgrade/collections_f45_active.json");
    assert!(f45["collections"]
        .as_array()
        .is_some_and(|c| c.iter().any(|r| r["version"] == "45" && r["status"] == "Active")));

    let current = read_fixture_value("release_upgrade/collections_already_current.json");
    assert!(current["collections"]
        .as_array()
        .is_some_and(|c| !c.iter().any(|r| r["version"] == "45")));

    let multi = read_fixture_value("release_upgrade/collections_non_fedora_and_multi.json");
    assert!(multi["collections"]
        .as_array()
        .is_some_and(|c| c.iter().any(|r| r["name"] == "Fedora EPEL")));
    assert!(multi["collections"]
        .as_array()
        .is_some_and(|c| c.iter().any(|r| r["version"] == "46" && r["status"] == "Active")));

    let malformed = read_fixture("release_upgrade/collections_malformed.txt");
    assert!(serde_json::from_str::<serde_json::Value>(&malformed).is_err());

    let quoted = read_fixture("release_upgrade/os_release_quoted.txt");
    assert!(quoted.contains("VERSION_ID=\"44\""));

    let bare = read_fixture("release_upgrade/os_release_bare.txt");
    assert!(bare.contains("VERSION_ID=44"));

    let missing = read_fixture("release_upgrade/os_release_missing_version.txt");
    assert!(!missing.contains("VERSION_ID"));

    let inhibitor = read_fixture("systemctl/reboot_block_inhibitor.txt");
    assert!(inhibitor.to_ascii_lowercase().contains("block inhibitor"));

    let logind_err = read_fixture("systemctl/reboot_logind_error.txt");
    assert!(logind_err.contains("Failed to talk to logind"));

    let logind_inhibit = read_fixture_value("systemctl/logind_inhibitors.json");
    assert_eq!(logind_inhibit.as_array().map(Vec::len), Some(3));
}

#[test]
fn brew_fixtures_contain_expected_signals() {
    let outdated = read_fixture_value("brew/outdated_v2.json");
    assert_eq!(outdated["formulae"].as_array().map(Vec::len), Some(3));
    assert_eq!(outdated["casks"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        outdated["formulae"][1]["installed_versions"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    let empty = read_fixture_value("brew/outdated_empty.json");
    assert_eq!(empty["formulae"].as_array().map(Vec::len), Some(0));
    assert_eq!(empty["casks"].as_array().map(Vec::len), Some(0));

    let pinned = read_fixture_value("brew/outdated_pinned.json");
    assert_eq!(pinned["formulae"][0]["pinned"], true);
    assert_eq!(pinned["formulae"][1]["pinned"], false);
    assert_eq!(pinned["casks"][0]["pinned"], true);

    let malformed = read_fixture("brew/outdated_malformed.txt");
    assert!(serde_json::from_str::<serde_json::Value>(&malformed).is_err());

    let upgrade = read_fixture("brew/upgrade_log.txt");
    assert!(upgrade.contains("==> Downloading "));
    assert!(upgrade.contains("==> Upgrading ripgrep"));
    assert!(upgrade.contains("==> Pouring "));
    assert!(upgrade.contains("🍺"));

    let upgrade_err = read_fixture("brew/upgrade_error.txt");
    assert!(upgrade_err.contains("🍺"));
    assert!(upgrade_err.contains("Error: fd: SHA256 mismatch"));
}

#[test]
fn gnome_extensions_fixtures_contain_expected_signals() {
    let mixed = read_fixture_value("gnome_extensions/list_extensions_mixed.json");
    assert_eq!(mixed["system-rpm-ext@redhat.com"]["type"], 1);
    assert_eq!(mixed["system-rpm-ext@redhat.com"]["hasUpdate"], true);
    assert_eq!(mixed["gsconnect@andyholmes.github.io"]["type"], 2);
    assert_eq!(mixed["gsconnect@andyholmes.github.io"]["hasUpdate"], false);
    assert_eq!(mixed["blur-my-shell@aunetx"]["type"], 2);
    assert_eq!(mixed["blur-my-shell@aunetx"]["hasUpdate"], true);
    assert!(mixed["minimal@example.org"].get("name").is_none());

    let empty = read_fixture_value("gnome_extensions/list_extensions_empty.json");
    assert_eq!(
        empty["apps-menu@gnome-shell-extensions.gcampax.github.com"]["type"],
        1
    );
    assert_eq!(
        empty["dash-to-dock@micxgx.gmail.com"]["hasUpdate"],
        false
    );

    let replies = read_fixture_value("gnome_extensions/install_replies.json");
    assert_eq!(replies["replies"].as_array().map(Vec::len), Some(4));
}

#[test]
fn rpm_ostree_fixtures_contain_expected_signals() {
    let diff = read_fixture("rpm_ostree/upgrade_check_diff.txt");
    assert!(diff.contains("AvailableUpdate:"));
    assert!(diff.contains("SecAdvisories:"));
    assert!(diff.contains("Upgraded:"));
    assert!(diff.contains("Added:"));
    assert!(diff.contains("Removed:"));

    let none = read_fixture("rpm_ostree/upgrade_check_none.txt");
    assert!(none.contains("No upgrade available."));

    let oci = read_fixture("rpm_ostree/upgrade_check_oci.txt");
    assert!(oci.contains("Digest: sha256:"));

    let status = read_fixture_value("rpm_ostree/status.json");
    assert_eq!(status["deployments"][0]["booted"], true);
    assert!(status["deployments"][0]["requested-packages"]
        .as_array()
        .is_some_and(|arr| arr.iter().any(|v| v == "htop")));

    let apply = read_fixture("rpm_ostree/upgrade_apply_log.txt");
    assert!(apply.contains("Receiving objects: 42%"));
    assert!(apply.contains("Staging deployment... done"));
    assert!(apply.contains("systemctl reboot"));

    let err = read_fixture("rpm_ostree/upgrade_error.txt");
    assert!(err.contains("Transaction in progress"));
}

#[test]
fn containers_fixtures_contain_expected_signals() {
    let dbox = read_fixture("containers/distrobox_list.txt");
    assert!(dbox.contains("arch-dev"));
    assert!(dbox.contains("ubuntu-lts"));

    let tbx = read_fixture("containers/toolbox_list.txt");
    assert!(tbx.contains("CONTAINER ID"));
    assert!(tbx.contains("fedora-toolbox-43"));

    let tbx_dup = read_fixture("containers/toolbox_list_with_duplicates.txt");
    assert!(tbx_dup.contains("distrobox-fedora-dev"));
    assert!(tbx_dup.contains("fedora-toolbox-43"));

    let empty = read_fixture("containers/empty.txt");
    assert!(empty.contains("ID"));

    let dbox_log = read_fixture("containers/distrobox_upgrade_log.txt");
    assert!(dbox_log.contains("Container arch-dev upgraded successfully."));

    let tbx_log = read_fixture("containers/toolbox_upgrade_log.txt");
    assert!(tbx_log.contains("Complete!"));

    let err = read_fixture("containers/upgrade_error.txt");
    assert!(err.contains("could not upgrade container"));
}

#[test]
fn toolchains_fixtures_contain_expected_signals() {
    let rust_check = read_fixture("toolchains/rustup_check.txt");
    assert!(rust_check.contains("stable-x86_64-unknown-linux-gnu"));
    assert!(rust_check.contains("nightly-x86_64-unknown-linux-gnu"));
    assert!(rust_check.contains("rustup - Update available"));

    let rust_empty = read_fixture("toolchains/rustup_check_empty.txt");
    assert!(rust_empty.contains("Up to date"));

    let rust_log = read_fixture("toolchains/rustup_update_log.txt");
    assert!(rust_log.contains("downloading component"));
    assert!(rust_log.contains("installing component"));
    assert!(rust_log.contains("updated - rustc"));

    let mise = read_fixture_value("toolchains/mise_outdated.json");
    assert_eq!(mise["node"]["latest"], "22.15.0");
    assert!(mise["ubi:jdx/usage"]["current"].is_null());

    let mise_empty = read_fixture_value("toolchains/mise_outdated_empty.json");
    assert_eq!(mise_empty.as_object().map(|o| o.len()), Some(0));

    let mise_malformed = read_fixture("toolchains/mise_outdated_malformed.txt");
    assert!(mise_malformed.contains("WARN mise"));

    let mise_log = read_fixture("toolchains/mise_upgrade_log.txt");
    assert!(mise_log.contains("✓ installed"));

    let uv_list = read_fixture("toolchains/uv_tool_list.txt");
    assert!(uv_list.contains("httpie v3.2.2"));
    assert!(uv_list.contains("- http"));

    let uv_dry = read_fixture("toolchains/uv_upgrade_dry_run.txt");
    assert!(uv_dry.contains("Updated ruff v0.9.6 -> v0.9.8"));
    assert!(uv_dry.contains("urllib3==2.3.0"));

    let uv_log = read_fixture("toolchains/uv_upgrade_log.txt");
    assert!(uv_log.contains("Updated httpie v3.2.2 -> v3.2.4"));

    let pipx_list = read_fixture_value("toolchains/pipx_list.json");
    assert_eq!(
        pipx_list["venvs"]["black"]["metadata"]["main_package"]["pinned"],
        true
    );

    let pipx_out = read_fixture("toolchains/pipx_outdated.txt");
    assert!(pipx_out.contains("ansible-lint has version 24.9.0"));

    let pipx_log = read_fixture("toolchains/pipx_upgrade_log.txt");
    assert!(pipx_log.contains("upgraded package ansible-lint"));
}
