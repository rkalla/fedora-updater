# Offline Test Fixture Corpus

Checked-in, deterministic, offline test inputs for unit, functional, and end-to-end suites ([rkalla/fedora-updater#20](https://github.com/rkalla/fedora-updater/issues/20)).

All 74 fixture files below are indexed in `tests/common/fixtures.rs::ALL_FIXTURES` and validated offline by `tests/fixture_corpus.rs`.

## Usage in Tests

Integration tests import `mod common;` and read fixtures via `common::fixtures`:

- `read_fixture("dnf/check_update_classic.txt") -> String`
- `read_fixture_value("fwupd/get_updates_multi_device.json") -> serde_json::Value`
- `read_flatpak_fixture("flatpak/check_system_nvidia_extension.json") -> FlatpakFixture`

## Domain Index

### `dnf/` (8 fixtures)

| File | Simulated Command & Exit | Scenario & Edge-Case Signals |
|---|---|---|
| `dnf/check_update_classic.txt` | `dnf check-update --refresh` (exit 100) | Classic `name.arch version repo` lines, metadata/repo noise headers, epochs (`1:`, `32:`), multi-arch twin (`mesa-dri-drivers.i686` + `x86_64`), `noarch`, `kernel-core` + `kernel-tools`, duplicate `firefox` row for dedup |
| `dnf/check_update_dnf5_table.txt` | `dnf check-update --refresh` (exit 100) | dnf5 column table (`Package Arch Version Repository Size`), `Upgrading:` / `Obsoleting packages:` headers, core reboot RPMs (`systemd`, `glibc`, `kernel-core`, `dbus-broker`), `MiB` / `KiB` sizes, `Transaction Summary` noise |
| `dnf/check_update_empty.txt` | `dnf check-update --refresh` (exit 0) | Metadata sync and `Last metadata expiration check:` header with zero updates |
| `dnf/check_update_repo_error.txt` | `dnf check-update --refresh` (exit 1) | DNS resolution error (`Curl error (6)`) and SSL peer error (`Curl error (60)`), zero parsed packages (hard check failure) |
| `dnf/repoquery_sizes.txt` | `dnf -q repoquery --upgrades --queryformat ...` (exit 0) | Noise header/malformed lines + tab-separated NEVRA byte counts (plus space-separated `tzdata`), intentionally omitting `firefox` & `kernel-tools` to test partial misses |
| `dnf/apply_happy_kernel.txt` | `dnf update -y` (exit 0) | Complete dnf5 transaction: `Upgrading:` / `Installing:` preview table, raw `\r` (`0x0d`) live download redraws `[1/4]..[4/4]`, `Running transaction`, `Verify package files`, `Prepare transaction`, `Upgrading` / `Installing kernel-core` (reboot trigger), `Running scriptlet`, `Cleanup / Verifying`, `Complete!` |
| `dnf/apply_dep_conflict.txt` | `dnf update -y` (exit 1) | Dependency conflict (`Problem 1`, `Problem 2: nothing provides ...`, `--skip-broken` hint) |
| `dnf/apply_rpm_lock_error.txt` | `dnf update -y` (exit 1) | Concurrent `dnf5daemon-server` lock on `/var/lib/rpm/.rpm.lock`, transaction aborted cleanly |

### `dnf_dbus/` (4 fixtures)

| File | Simulated Call / Signal Stream | Scenario & Edge-Case Signals |
|---|---|---|
| `dnf_dbus/list_fd_stream.ndjson` | `org.rpm.dnf.v0.rpm.Rpm.list_fd` | Multi-line NDJSON stream of RPM upgrades (`openssl`, `kernel-core`, `vim-enhanced` with epoch `0:`/`2:`, `download_size`, `install_size`, `summary`) |
| `dnf_dbus/check_upgrades_and_advisories.json` | `Rpm.list` (`installed`) + `Advisory.list` | Installed baselines for `old → new` & `install_size_delta` cross-ref plus Security/Bugfix advisories (`CVE-2026-2401`, `RHBZ#2458001`, severity) |
| `dnf_dbus/apply_signals_happy.json` | `Base.download_*` + `Rpm.transaction_*` | Download ticks at 1s intervals (MB/s sparkline samples), `transaction_before_begin` cancel cutoff, `elem_progress`/`action_*`, and `verify_*` |
| `dnf_dbus/history_transactions.json` | `org.rpm.dnf.v0.History.list` | Multi-package upgraded & single-package installed RPM transactions for chronological timeline merge |

### `flatpak/` (6 fixtures)

| File | Simulated Command / Call | Scenario & Edge-Case Signals |
|---|---|---|
| `flatpak/check_user_normal.json` | `libflatpak` user check (`scope: "user"`) | Two user apps (`Signal`, `Thunderbird`) + one runtime (`org.gnome.Platform/x86_64/48` with empty `version` testing branch fallback), AppStream `summary` metadata, installed size delta, and matching `Update` byte sizes |
| `flatpak/check_system_nvidia_extension.json` | `libflatpak` system check (`scope: "system"`) | `Steam` on current commit pulling NVIDIA `GL32` `Install` op (`251 MB`), AppStream summaries, two runtimes sharing `GL.nvidia-570-133-07` `Install` op (extension named on both rows, bytes counted once), and one `Uninstall` op to ignore |
| `flatpak/apply_progress_happy.txt` | `flatpak update --noninteractive` (exit 0) | Mix of bare IDs (`org.signal.Signal/x86_64/stable`) and prefixed refs (`app/org.mozilla.Thunderbird/x86_64/stable`), `Downloading`/`Updating`/`Installing` percentages, `Updates complete.` |
| `flatpak/apply_nothing_to_update.txt` | `flatpak update --noninteractive` (exit 0) | No-op apply output (`Nothing to update.`) |
| `flatpak/apply_remote_error.txt` | `flatpak update --noninteractive` (exit 1) | Network remote pull failure (`Socket I/O timed out`) |
| `flatpak/native_transaction_events.json` | Native `FlatpakTransaction` signal sequence | Planned direct + related NVIDIA extension ops, `FlatpakTransactionProgress` snapshots (`bytes_transferred`, `start_time_us`, estimating/deploying phases), and `NON_FATAL` vs fatal error flags |

### `fwupd/` (11 fixtures)

| File | Simulated Command & Exit | Scenario & Edge-Case Signals |
|---|---|---|
| `fwupd/get_updates_multi_device.json` | `fwupdmgr get-updates --json` (exit 0) | Realistic `"Devices"` array with 3 devices (`UEFI System Firmware`, `UEFI dbx`, `WD_BLACK SN850X NVMe SSD` with multiple `Releases`) |
| `fwupd/get_updates_empty.json` | `fwupdmgr get-updates --json` (exit 0 / 2) | Valid JSON `{ "Devices": [] }` yielding zero firmware packages |
| `fwupd/get_updates_corrupt.txt` | `fwupdmgr get-updates --json` | Truncated malformed JSON testing fallback resilience |
| `fwupd/get_updates_text.txt` | `fwupdmgr get-updates` (text) | Plain-text device blocks with `Device ID:`, `Version:`, `New version:`, `Bootloader Version:` (ignored), and `├─` / `└─` tree glyphs |
| `fwupd/get_updates_no_updates.txt` | `fwupdmgr get-updates` (exit 2) | Real Fedora no-updates bullet listing (`Devices with no available firmware updates:\n • System Firmware`) |
| `fwupd/apply_happy_reboot.txt` | `fwupdmgr update -y ...` (exit 0) | Full firmware apply phases (`Downloading…`, `Decompressing…`, `Writing… Percentage=60`, `Verifying…: 80.0%`, `Restarting device…: 99.0%`, `Waiting…: 100.0%`, `Successfully installed firmware`), post-apply bullet list, and `An update requires a reboot to complete.` |
| `fwupd/apply_battery_error.txt` | `fwupdmgr update -y ...` (exit 1) | Low-battery / missing AC power abort mid-run after download/decompress |
| `fwupd/check_reboot_needed_true.json` | `fwupdmgr check-reboot-needed --json` (exit 0) | JSON device carrying `"needs-reboot"` flag under exit 0 |
| `fwupd/check_reboot_needed_false.json` | `fwupdmgr check-reboot-needed --json` (exit 2) | Empty `"Devices"` array under non-zero exit code |
| `fwupd/dbus_devices_and_upgrades.json` | `org.freedesktop.fwupd` (`GetDevices`/`GetUpgrades`) | Devices with `REQUIRE_AC` flag, host & peripheral `BatteryLevel`/`BatteryThreshold`, HTML `<p>` release notes, and `NEEDS_REBOOT` device |
| `fwupd/dbus_history.json` | `org.freedesktop.fwupd` (`GetHistory`) | Realistic `Success` and `Failed` firmware history entries with target versions and error reasons |

### `brew/` (6 fixtures)

| File | Simulated Command & Exit | Scenario & Edge-Case Signals |
|---|---|---|
| `brew/outdated_v2.json` | `brew outdated --json=v2` (exit 0) | Multiple outdated formulae (`ripgrep`, multi-installed-versions `cmake`, tapped `oven-sh/bun/bun`) plus an outdated cask (`visual-studio-code`) |
| `brew/outdated_empty.json` | `brew outdated --json=v2` (exit 0) | Clean `{ "formulae": [], "casks": [] }` JSON response yielding zero updates |
| `brew/outdated_pinned.json` | `brew outdated --json=v2` (exit 0) | Mix of pinned (`node` with `pinned: true` / `pinned_version`, pinned cask `firefox`) and unpinned (`fd`) entries |
| `brew/outdated_malformed.txt` | `brew outdated --json=v2` | Error prefix + truncated JSON testing soft-error resilience |
| `brew/upgrade_log.txt` | `brew upgrade ...` (exit 0) | Realistic Homebrew upgrade stream (`==> Fetching`, `==> Downloading`, `==> Upgrading N outdated packages:` summary ignore, `==> Pouring`, `🍺 .../Cellar/...`, `==> Running \`brew cleanup\``) |
| `brew/upgrade_error.txt` | `brew upgrade ...` (exit 1) | Partial failure where `ripgrep` completes cleanly (`🍺`) before `Warning:` / `Error: fd: SHA256 mismatch` aborts |

### `release_upgrade/` (7 fixtures)

| File | Simulated Source | Scenario & Edge-Case Signals |
|---|---|---|
| `release_upgrade/collections_f45_active.json` | PkgDB collections API | `devel` Under Development + `45` Active (`date_updated`) + `44` Active + `42` EOL |
| `release_upgrade/collections_already_current.json` | PkgDB collections API | Highest Active Fedora Linux is `44` (machine on 44 receives no offer) |
| `release_upgrade/collections_non_fedora_and_multi.json` | PkgDB collections API | `Fedora EPEL` `99` Active, `Fedora ELN` `100` Active, multiple Active Fedora Linux releases (`44`, `45` missing `date_updated`, `46` with `date_updated`) |
| `release_upgrade/collections_malformed.txt` | HTTP 502 HTML page | Non-JSON error body from upstream mirror returning no upgrade offer |
| `release_upgrade/os_release_quoted.txt` | `/etc/os-release` | Double-quoted `VERSION_ID="44"` |
| `release_upgrade/os_release_bare.txt` | `/etc/os-release` | Unquoted `VERSION_ID=44` |
| `release_upgrade/os_release_missing_version.txt` | `/etc/os-release` | Snapshot manifest missing `VERSION_ID` |

### `systemctl/` (3 fixtures)

| File | Simulated Command & Exit | Scenario & Edge-Case Signals |
|---|---|---|
| `systemctl/logind_inhibitors.json` | `org.freedesktop.login1.Manager.ListInhibitors` | Mix of `delay`, `handle-power-key` block, and `shutdown` block inhibitors |
| `systemctl/reboot_block_inhibitor.txt` | `systemctl reboot` (exit 1) | logind `Operation denied due to active block inhibitor` output triggering user-friendly try-again recovery message |
| `systemctl/reboot_logind_error.txt` | `systemctl reboot` (exit 1) | Generic logind DBus/timeout failure preserved verbatim |

### `gnome_extensions/` (3 fixtures)

| File | Simulated D-Bus Source | Scenario & Edge-Case Signals |
|---|---|---|
| `gnome_extensions/list_extensions_mixed.json` | `org.gnome.Shell.Extensions.ListExtensions` | Mixed system RPM (`type: 1` with/without `hasUpdate: true`, both skipped), up-to-date user (`type: 2`, `hasUpdate: false`), full-metadata updateable user extension, float `version: 54.0` + distinct `version-name`, and minimal extension omitting optional fields |
| `gnome_extensions/list_extensions_empty.json` | `org.gnome.Shell.Extensions.ListExtensions` | System extension + up-to-date user extension yielding zero extension updates |
| `gnome_extensions/install_replies.json` | `org.gnome.Shell.Extensions.InstallRemoteExtension` | Canonical reply cases: `"successful"`, `"cancelled"`, `"error"`, and session D-Bus timeout error |

### `rpm_ostree/` (6 fixtures)

| File | Simulated Command & Exit | Scenario & Edge-Case Signals |
|---|---|---|
| `rpm_ostree/upgrade_check_diff.txt` | `rpm-ostree upgrade --check` (exit 0) | Pending Silverblue deployment with `Version:`, `Commit:`, `SecAdvisories:`, `Diff:`, `Upgraded:`, `Downgraded:`, `Added:`, and `Removed:` entries |
| `rpm_ostree/upgrade_check_none.txt` | `rpm-ostree upgrade --check` (exit 77) | Clean `No upgrade available.` response handled as zero pending updates |
| `rpm_ostree/upgrade_check_oci.txt` | `rpm-ostree upgrade --check` (exit 0) | Image-mode OCI digest deployment (`Digest: sha256:...`, `Version:`) without per-RPM diff sections |
| `rpm_ostree/status.json` | `rpm-ostree status --json` (exit 0) | Silverblue deployment metadata (`booted: true`, layered `htop`/`distrobox`, `origin`, and `cached-update`) |
| `rpm_ostree/upgrade_apply_log.txt` | `rpm-ostree upgrade` (exit 0) | Full staged apply stream (`Receiving metadata objects`, `Receiving objects: N%`, `Checking out tree`, `Overlaying`, `Writing OSTree commit`, `Staging deployment... done`, `systemctl reboot`) |
| `rpm_ostree/upgrade_error.txt` | `rpm-ostree upgrade` (exit 1) | Active daemon transaction lock (`Transaction in progress; please wait or run rpm-ostree cancel`) |

### `containers/` (7 fixtures)

| File | Simulated Command & Exit | Scenario & Edge-Case Signals |
|---|---|---|
| `containers/distrobox_list.txt` | `distrobox list --no-color` (exit 0) | Pipe-delimited container table with running Arch Linux (`Up 3 hours`) and exited Fedora/Ubuntu containers |
| `containers/toolbox_list.txt` | `toolbox list --containers` (exit 0) | Columnar 5-column table (`CONTAINER ID`, `CONTAINER NAME`, `CREATED`, `STATUS`, `IMAGE NAME`) with unique Toolbx containers |
| `containers/toolbox_list_with_duplicates.txt` | `toolbox list --containers` (exit 0) | Mixed listing overlapping Distrobox containers by ID/name and ID prefix plus one unique Toolbx container for deduplication |
| `containers/empty.txt` | `distrobox list --no-color` (exit 0) | Header-only empty container listing yielding zero discovered containers |
| `containers/distrobox_upgrade_log.txt` | `distrobox upgrade arch-dev` (exit 0) | Unprivileged rootless Distrobox upgrade stream ending cleanly with `Container arch-dev upgraded successfully.` |
| `containers/toolbox_upgrade_log.txt` | `toolbox run --container fedora-toolbox-43 sudo dnf upgrade -y` (exit 0) | Unprivileged rootless Toolbx inner DNF upgrade stream ending cleanly with `Complete!` |
| `containers/upgrade_error.txt` | `distrobox upgrade ubuntu-lts` (exit 1) | Non-zero container upgrade failure log preserved for per-container error isolation |

### `toolchains/` (13 fixtures)

| File | Simulated Command & Exit | Scenario & Edge-Case Signals |
|---|---|---|
| `toolchains/rustup_check.txt` | `rustup check` (exit 100) | Mixed `Update available :` and lowercase `update available:` lines, SHA+date nightly tags, up-to-date toolchain, `rustc` noise line, and `rustup` self-update |
| `toolchains/rustup_check_empty.txt` | `rustup check` (exit 0) | Clean up-to-date toolchains and `rustup` self-update check yielding zero updates |
| `toolchains/rustup_update_log.txt` | `rustup update --no-self-update ...` (exit 0) | Syncing channels, `downloading component` percentages, `installing component`, and `<toolchain> updated` lines |
| `toolchains/mise_outdated.json` | `mise outdated --json` (exit 0) | Map of outdated tools (`node`, `ripgrep`) plus `ubi:jdx/usage` with `current: null` and `mise.toml` source metadata |
| `toolchains/mise_outdated_empty.json` | `mise outdated --json` (exit 0) | Empty JSON object `{}` when all mise tools are up to date |
| `toolchains/mise_outdated_malformed.txt` | `mise outdated --json` (exit 0) | Non-JSON warning output failing cleanly during check |
| `toolchains/mise_upgrade_log.txt` | `mise upgrade node ubi:jdx/usage` (exit 0) | Download percentage, install phase, and `✓ installed` completion lines |
| `toolchains/uv_tool_list.txt` | `uv tool list` (exit 0) | Installed isolated Python CLI tools (`httpie`, `ruff`) and executable lists at column 0 |
| `toolchains/uv_upgrade_dry_run.txt` | `uv tool upgrade --all --dry-run` (exit 0) | Mixed `Updated <tool>` and `- pkg==old` / `+ pkg==new` diff lines including internal `urllib3` dep filtered out |
| `toolchains/uv_upgrade_log.txt` | `uv tool upgrade <tool>` (exit 0) | Per-tool download, executable install, and `Updated <tool>` completion output |
| `toolchains/pipx_list.json` | `pipx list --json` (exit 0) | Isolated pipx venv metadata with unpinned `ansible-lint` and `pinned: true` `black` |
| `toolchains/pipx_outdated.txt` | `pipx list --outdated` (exit 0) | Outdated lines for unpinned `ansible-lint` and pinned `black` (pinned entry skipped cleanly) |
| `toolchains/pipx_upgrade_log.txt` | `pipx upgrade ansible-lint` (exit 0) | Per-package `upgrading ...` and `upgraded package ...` completion lines |
