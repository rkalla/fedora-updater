#![allow(dead_code)]

use std::path::{Path, PathBuf};

use fedora_updater::backend::flatpak::{FlatpakListedRef, FlatpakOpKind, FlatpakResolvedOp};
use fedora_updater::model::UpdateSource;
use serde::de::DeserializeOwned;
use serde::Deserialize;

/// Complete list of checked-in offline fixture files relative to `tests/fixtures/`.
pub const ALL_FIXTURES: &[&str] = &[
    // dnf/
    "dnf/check_update_classic.txt",
    "dnf/check_update_dnf5_table.txt",
    "dnf/check_update_empty.txt",
    "dnf/check_update_repo_error.txt",
    "dnf/repoquery_sizes.txt",
    "dnf/apply_happy_kernel.txt",
    "dnf/apply_dep_conflict.txt",
    "dnf/apply_rpm_lock_error.txt",
    // dnf_dbus/
    "dnf_dbus/list_fd_stream.ndjson",
    "dnf_dbus/check_upgrades_and_advisories.json",
    "dnf_dbus/apply_signals_happy.json",
    "dnf_dbus/history_transactions.json",
    // flatpak/
    "flatpak/check_user_normal.json",
    "flatpak/check_system_nvidia_extension.json",
    "flatpak/apply_progress_happy.txt",
    "flatpak/apply_nothing_to_update.txt",
    "flatpak/apply_remote_error.txt",
    "flatpak/native_transaction_events.json",
    // fwupd/
    "fwupd/get_updates_multi_device.json",
    "fwupd/get_updates_empty.json",
    "fwupd/get_updates_corrupt.txt",
    "fwupd/get_updates_text.txt",
    "fwupd/get_updates_no_updates.txt",
    "fwupd/apply_happy_reboot.txt",
    "fwupd/apply_battery_error.txt",
    "fwupd/check_reboot_needed_true.json",
    "fwupd/check_reboot_needed_false.json",
    "fwupd/dbus_devices_and_upgrades.json",
    "fwupd/dbus_history.json",
    // brew/
    "brew/outdated_v2.json",
    "brew/outdated_empty.json",
    "brew/outdated_pinned.json",
    "brew/outdated_malformed.txt",
    "brew/upgrade_log.txt",
    "brew/upgrade_error.txt",
    // release_upgrade/
    "release_upgrade/collections_f45_active.json",
    "release_upgrade/collections_already_current.json",
    "release_upgrade/collections_non_fedora_and_multi.json",
    "release_upgrade/collections_malformed.txt",
    "release_upgrade/os_release_quoted.txt",
    "release_upgrade/os_release_bare.txt",
    "release_upgrade/os_release_missing_version.txt",
    // systemctl/
    "systemctl/logind_inhibitors.json",
    "systemctl/reboot_block_inhibitor.txt",
    "systemctl/reboot_logind_error.txt",
    // gnome_extensions/
    "gnome_extensions/list_extensions_mixed.json",
    "gnome_extensions/list_extensions_empty.json",
    "gnome_extensions/install_replies.json",
    // rpm_ostree/
    "rpm_ostree/upgrade_check_diff.txt",
    "rpm_ostree/upgrade_check_none.txt",
    "rpm_ostree/upgrade_check_oci.txt",
    "rpm_ostree/status.json",
    "rpm_ostree/upgrade_apply_log.txt",
    "rpm_ostree/upgrade_error.txt",
    // containers/
    "containers/distrobox_list.txt",
    "containers/toolbox_list.txt",
    "containers/toolbox_list_with_duplicates.txt",
    "containers/empty.txt",
    "containers/distrobox_upgrade_log.txt",
    "containers/toolbox_upgrade_log.txt",
    "containers/upgrade_error.txt",
    // toolchains/
    "toolchains/rustup_check.txt",
    "toolchains/rustup_check_empty.txt",
    "toolchains/rustup_update_log.txt",
    "toolchains/mise_outdated.json",
    "toolchains/mise_outdated_empty.json",
    "toolchains/mise_outdated_malformed.txt",
    "toolchains/mise_upgrade_log.txt",
    "toolchains/uv_tool_list.txt",
    "toolchains/uv_upgrade_dry_run.txt",
    "toolchains/uv_upgrade_log.txt",
    "toolchains/pipx_list.json",
    "toolchains/pipx_outdated.txt",
    "toolchains/pipx_upgrade_log.txt",
];

#[derive(Debug, Clone)]
pub struct FlatpakFixture {
    pub scope: String,
    pub refs: Vec<FlatpakListedRef>,
    pub ops: Vec<FlatpakResolvedOp>,
}

impl FlatpakFixture {
    pub fn update_source(&self) -> UpdateSource {
        match self.scope.as_str() {
            "user" => UpdateSource::FlatpakUser,
            "system" => UpdateSource::FlatpakSystem,
            other => panic!("unexpected flatpak fixture scope `{other}`"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct FlatpakFixtureDoc {
    scope: String,
    refs: Vec<FlatpakListedRefDto>,
    ops: Vec<FlatpakResolvedOpDto>,
}

#[derive(Debug, Deserialize)]
struct FlatpakListedRefDto {
    formatted_ref: String,
    name: String,
    arch: String,
    branch: String,
    version: String,
    origin: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    installed_size_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
enum FlatpakOpKindDto {
    Install,
    Update,
    Uninstall,
}

#[derive(Debug, Deserialize)]
struct FlatpakResolvedOpDto {
    formatted_ref: String,
    kind: FlatpakOpKindDto,
    download_bytes: u64,
    #[serde(default)]
    installed_size_bytes: u64,
    #[serde(default)]
    related_to: Vec<String>,
}

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

pub fn fixture_path(rel: &str) -> PathBuf {
    fixtures_root().join(rel)
}

pub fn read_fixture(rel: &str) -> String {
    let path = fixture_path(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture `{}` ({}): {e}", rel, path.display()))
}

pub fn read_fixture_bytes(rel: &str) -> Vec<u8> {
    let path = fixture_path(rel);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture bytes `{}` ({}): {e}", rel, path.display()))
}

pub fn read_fixture_json<T: DeserializeOwned>(rel: &str) -> T {
    let raw = read_fixture(rel);
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("failed to parse JSON fixture `{rel}`: {e}"))
}

pub fn read_fixture_value(rel: &str) -> serde_json::Value {
    read_fixture_json(rel)
}

pub fn read_flatpak_fixture(rel: &str) -> FlatpakFixture {
    let doc: FlatpakFixtureDoc = read_fixture_json(rel);
    let refs = doc
        .refs
        .into_iter()
        .map(|r| FlatpakListedRef {
            formatted_ref: r.formatted_ref,
            name: r.name,
            arch: r.arch,
            branch: r.branch,
            version: r.version,
            origin: r.origin,
            summary: r.summary,
            installed_size_bytes: r.installed_size_bytes,
        })
        .collect();
    let ops = doc
        .ops
        .into_iter()
        .map(|o| FlatpakResolvedOp {
            formatted_ref: o.formatted_ref,
            kind: match o.kind {
                FlatpakOpKindDto::Install => FlatpakOpKind::Install,
                FlatpakOpKindDto::Update => FlatpakOpKind::Update,
                FlatpakOpKindDto::Uninstall => FlatpakOpKind::Uninstall,
            },
            download_bytes: o.download_bytes,
            installed_size_bytes: o.installed_size_bytes,
            related_to: o.related_to,
        })
        .collect();
    FlatpakFixture {
        scope: doc.scope,
        refs,
        ops,
    }
}
