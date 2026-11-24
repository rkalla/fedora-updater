# Fedora Updater

[![CI](https://github.com/rkalla/fedora-updater/actions/workflows/ci.yml/badge.svg)](https://github.com/rkalla/fedora-updater/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
![Fedora 42+](https://img.shields.io/badge/Fedora-42%2B-51A2DA?logo=fedora&logoColor=white)
![GTK4 & Libadwaita](https://img.shields.io/badge/UI-GTK4_%2F_Libadwaita-4A86CF)

A fast, transparent GTK4 + Libadwaita desktop updater for Fedora that brings **system updates** (classic **DNF** and Atomic **`rpm-ostree`**), **Flatpak** (user and system, including NVIDIA/runtime extensions), **GNOME Shell Extensions**, **Homebrew**, **developer toolchains** (`rustup` & `mise`), opt-in **rootless containers** (`Distrobox` & `Toolbx`), **unprivileged Custom User Hooks**, **unused cleanup**, and **LVFS firmware** together in a single **Observatory** workspace.

[![Reviewing pending Flatpak and developer toolchain updates in Fedora Updater](screenshots/ready.png)](screenshots/ready.png)

| Live Telemetry | Update History | Safe Completion |
|:---:|:---:|:---:|
| [![Live download throughput and multi-stage progress](screenshots/running.png)](screenshots/running.png) | [![Searchable DNF and fwupd update history timeline](screenshots/history.png)](screenshots/history.png) | [![Completion summary with logind-aware restart action](screenshots/done.png)](screenshots/done.png) |

---

## Features

- **Unified multi-source Observatory view:** Inspect pending **DNF** or Atomic **`rpm-ostree`** system updates, **Flatpak** user/system apps and runtime extensions, **GNOME Shell Extensions**, **Homebrew**, **`rustup`** & **`mise`** developer toolchains, opt-in **Distrobox** / **Toolbx** containers, **Custom User Hooks**, and **fwupd** LVFS firmware in one window with color-coded source pills (`DNF` / `OSTREE` / `FP` / `EXT` / `BREW` / `RUST` / `MISE` / `DBOX` / `TBX` / `HOOK` / `FW`) and counts.
- **Dynamic source sidebar & native Preferences (`Ctrl+,`):** Automatically hides unconfigured/disabled categories in the Observatory sidebar while letting you enable or disable individual sources, opt in to rootless container updates, and manage Custom Hooks in a Libadwaita Preferences dialog.
- **Zero-password checks & history:** Update discovery (`dnf5daemon-server` D-Bus with empty session config, `rpm-ostree`, `libflatpak`, GNOME Shell session D-Bus, user-space tools, and `fwupd`) and transaction history browsing run unprivileged — authentication is only requested when applying privileged system or firmware updates.
- **Flexible apply modes:** Use per-package checkboxes and the `Update All` SplitButton to choose **Update All** (or **Stage System Update** on Atomic hosts), **Update & Restart**, **Prepare Offline Update**, or **Download Only**.
- **Security advisories & Package Inspector:** Click any update row to inspect version transitions (`old → new`), CVE and Bugzilla references, advisory severity badges, install-size deltas, and summaries.
- **Live telemetry & throughput sparkline:** Watch real-time MB/s download throughput, peak rate, 4-stage pipeline pills (`Download` → `Prepare` → `Install` → `Verify`), and cancel downloads safely mid-run.
- **Post-update unused cleanup:** Surfaces reclaimable unused Flatpak runtimes and orphaned Homebrew leaf formulae in an opt-in banner with an explicit confirmation dialog before removing anything.
- **Opt-in unprivileged Custom User Hooks:** Configure custom user-space check/apply commands (`sh -c` under your own account, zero `pkexec`/root) with editable static form presets (`uv tools`, `pipx apps`, `Cargo binaries`, `npm global`) right in Preferences.
- **Hardware & reboot safety guards:** Checks AC power / battery thresholds via `fwupd` before flashing firmware and queries `systemd-logind` shutdown inhibitors before restarting.
- **Observatory dual-mode shell:** Collapsible sidebar (`F9` / `Ctrl+\`), instant package search (`Ctrl+F`), embedded filterable **Activity Log**, **Update History** timeline, and **Fedora release-upgrade** notices.
- **Single portable binary:** Built as one multi-call executable (`fedora-updater` + `fedora-updater --helper`) with zero GSettings schemas required (UI geometry, sidebar state, source toggles, and custom hooks persist in `$XDG_CONFIG_HOME/fedora-updater/ui.json`).

---

## How It Works

Fedora Updater talks to native Fedora platform daemons, libraries, and user-space CLI managers by default and automatically falls back to standard CLI tools (run via `--helper` only when elevated privileges are needed) if a daemon is unavailable:

| Area | Native Engine (Default) | Automatic Fallback / Notes |
|---|---|---|
| **System (DNF)** | `dnf5daemon-server` (`org.rpm.dnf.v0`) D-Bus — zero-auth check & history, advisories, selective/offline/download-only apply | `dnf check-update` / `dnf update -y` via `--helper` |
| **System (Atomic)** | `rpm-ostree` status/diff/advisories detection and staged deployments on OSTree hosts | Automatic host-mode adaptation (`OSTREE` badge, single-unit staging) |
| **Flatpak** (user & system) | `libflatpak.so.0` discovery (`list_installed_refs_for_update`) & `FlatpakTransaction` with related extensions | `flatpak update --noninteractive` |
| **GNOME Extensions** | `org.gnome.Shell.Extensions` session D-Bus discovery & selective unprivileged install/update | Silently skipped outside GNOME Shell sessions |
| **Homebrew** | `brew outdated --json=v2` discovery & selective unprivileged `brew upgrade` | Auto-detected in `PATH` and `/home/linuxbrew/.linuxbrew/bin/brew` |
| **Developer Toolchains** | `rustup check` & `mise outdated --json` — fast read-only checks with exact `old → new` diffs and selective apply | Strictly unprivileged (`RUST` / `MISE` badges) |
| **Containers** *(opt-in)* | Rootless `distrobox` and `toolbx` discovery & per-container upgrade with ID deduplication | Disabled by default; opt in via Preferences (`Ctrl+,`) |
| **Custom User Hooks** *(opt-in)* | User-defined commands executed strictly as your user account (`sh -c`), assisted by static form presets (`uv tools`, `pipx apps`, `Cargo binaries`, `npm global`) | Never routed through `--helper` or `pkexec` |
| **Unused Cleanup** | Dry-run discovery for unused Flatpak refs (`flatpak uninstall --unused --assumeno`) & Homebrew leaves (`brew autoremove --dry-run`) | Always asks confirmation before deleting |
| **Firmware** | `fwupd` (`org.freedesktop.fwupd`) D-Bus — battery guards, LVFS release notes, device progress & history | `fwupdmgr refresh` / `get-updates` / `update` via `--helper` |
| **Reboot** | `systemd-logind` (`org.freedesktop.login1`) `CanReboot` / inhibitor check & `Reboot(true)` | `systemctl reboot` |

> **Built-in toolchains vs. Custom Hooks (80/20 boundary):** Built-in developer toolchain backends are reserved for isolated, unprivileged managers with a fast read-only check query producing exact per-item `old → new` diffs and selective apply (`rustup`, `mise`). Other language/tool managers (`uv`, `pipx`, `cargo install-update`, `npm -g`, `nvm`/`fnm`, etc.) run cleanly as user-owned **Custom Hooks** via editable form templates without brittle output parsers or root privileges.

---

## Download & Install

Prebuilt packages for `x86_64` (Fedora 42+) are published on [GitHub Releases](https://github.com/rkalla/fedora-updater/releases):

- **RPM package (recommended):**
  Download `fedora-updater-<version>-1.x86_64.rpm` and install with DNF:
  ```bash
  sudo dnf install ./fedora-updater-*.rpm
  ```
  Installs `/usr/bin/fedora-updater`, the GNOME `.desktop` launcher, application icons, and the Polkit policy. Remove anytime with `sudo dnf remove fedora-updater`.

- **Portable tarball (no system changes):**
  Download and extract `fedora-updater-<version>-linux-x86_64.tar.gz`, then run directly or install a user-local launcher:
  ```bash
  tar -xzf fedora-updater-*-linux-x86_64.tar.gz
  cd fedora-updater-*-linux-x86_64
  ./bin/fedora-updater
  # Optional user-local GNOME menu entry + icons:
  ./install.sh
  ```

---

## Build & Run from Source

### Prerequisites (Fedora 42+ / Fedora 44 recommended)

Install the GTK4, Libadwaita, Blueprint, C compiler, and pkg-config development packages:

```bash
sudo dnf install gtk4-devel libadwaita-devel blueprint-compiler gcc pkgconf-pkg-config
```

You also need a Rust toolchain (`cargo` and `rustc`):

```bash
sudo dnf install rust cargo
# Or install via https://rustup.rs/
```

> **Note:** `libflatpak.so.0` and platform daemons are loaded/spoken to at runtime, so `flatpak-devel` is not required to compile.

### Build

Clone the repository and run [`./build`](build):

```bash
git clone https://github.com/rkalla/fedora-updater.git
cd fedora-updater
./build
```

The `./build` script locates `blueprint-compiler` (including `~/.local/bin`), runs `cargo build --release --bin fedora-updater` with LTO + strip enabled, and writes the single standalone binary to `./bin/fedora-updater` (pass `./build --debug` for an unstripped debug build, or invoke `cargo build --release` directly).

### Run or Install

Choose any of the three ways to run:

1. **Run directly (portable, no system changes):**
   ```bash
   ./bin/fedora-updater
   ```
   You can copy `./bin/fedora-updater` anywhere and run it standalone; no companion helper binary is needed.

2. **Install a GNOME Applications menu launcher (user-local):**
   ```bash
   ./install.sh
   ```
   Installs a `.desktop` entry and sized app icons under `~/.local/share/` pointing at `./bin/fedora-updater` so you can launch **Fedora Updater** from GNOME Overview. Remove anytime with `./uninstall.sh`.

3. **Optional system-wide install with Polkit policy:**
   ```bash
   ./scripts/install-helper.sh
   ```
   Installs `/usr/bin/fedora-updater` and `/usr/share/polkit-1/actions/dev.fedora.Updater.policy` (`auth_admin_keep`), keeping privileged `--helper` sessions scoped to one authentication prompt per app session.

---

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+R` or `F5` | Check for updates |
| `Ctrl+F` | Focus package search |
| `Ctrl+,` | Open Preferences |
| `F9` or `Ctrl+\` | Toggle Observatory sidebar |

---

## Security & Privilege Model

Fedora Updater ships as a single multi-call binary with strict privilege separation:

```text
fedora-updater           → unprivileged GNOME GUI
fedora-updater --helper  → allowlisted privileged helper (launched via pkexec when needed)
```

1. **Unprivileged D-Bus checks & history:** `dnf5daemon-server` sessions are opened with an empty option map (`SessionManager.open_session({})`), never overriding repo/base config, so checking for updates and viewing history do not trigger Polkit prompts.
2. **Allowlisted helper:** When CLI fallback elevation is required (`pkexec /path/to/fedora-updater --helper`), the GUI spawns a single helper session for the window lifetime. The helper accepts only fixed, hardcoded argv allowlists for `/usr/bin/dnf` and `/usr/bin/fwupdmgr` and never touches user environment overrides.
3. **Polkit policy (`auth_admin_keep`):** [`data/dev.fedora.Updater.policy`](data/dev.fedora.Updater.policy) lets active desktop sessions keep authentication cached across operations in a run without the GUI ever handling passwords.

For local testing without Polkit elevation:

```bash
export FEDORA_UPDATER_HELPER_DIRECT=1   # run --helper without pkexec
# Optional explicit path override (still invokes --helper):
export FEDORA_UPDATER_HELPER=/path/to/fedora-updater
```

---

## Development & Testing

The test suite runs completely offline in under a second (**367 offline tests + 6 opt-in live-host tests**) across four tiers backed by a **74-file** deterministic fixture corpus under [`tests/fixtures/`](tests/fixtures/README.md):

```bash
# Fast offline unit, functional, and pipe-based E2E suite
cargo test

# 30 headless GTK4/Libadwaita widget, row, window, and dialog E2E tests (uses gtk4-broadwayd)
./scripts/test-ui-headless.sh

# Strict lint check
cargo clippy --all-targets -- -D warnings

# Opt-in live-host tests invoking real host DNF / D-Bus services
cargo test -- --ignored
```

### UI Preview Mode

Inspect any application state immediately without touching system packages or running real transactions:

```bash
FEDORA_UPDATER_PREVIEW=ready-native ./bin/fedora-updater
```

Available presets include `idle`, `idle-current`, `idle-warning`, `idle-cleanup`, `auth`, `checking`, `ready`, `ready-many`, `ready-warn`, `ready-native`, `ready-ostree`, `ready-ostree-mixed`, `ready-containers`, `ready-toolchains`, `ready-hooks`, `running`, `running-sparkline`, `running-many`, `running-expanded`, `running-ostree`, `done`, `done-reboot`, `done-partial`, `done-ostree`, `done-cleanup`, `failed`, `history`, and `log`. Set `FEDORA_UPDATER_SCREENSHOT=/path/to/out.png` to capture a window snapshot and exit cleanly.

<details>
<summary>Architecture & Module Map</summary>

```text
fedora-updater (GUI)
       │
       ├── native / user-space ──► dnf5daemon-server · rpm-ostree · libflatpak.so.0 · GNOME Shell · brew · rustup · mise · distrobox · toolbx · fwupd · logind
       └── privilege fallback  ── pkexec ──► fedora-updater --helper
                                               └── allowlisted /usr/bin/dnf & /usr/bin/fwupdmgr

src/
├── main.rs                Multi-call CLI/GUI entry point (--helper dispatch & AdwApplication)
├── lib.rs                 Library crate root
├── model.rs               Domain types (Package, PackageAdvisory, SourceCategory, UpdateSource, ApplyMode, Stopwatch)
├── state.rs               Pure state machine (AppState, Phase, RunningState, reduce)
├── orchestrator.rs        Sequential check/apply/history pipelines, native-first + fallback, cancellation
├── presenter.rs           Pure UI copy, badge formatting, counts, and status helpers
├── console.rs             Bounded console log buffer & incremental UI deltas
├── release_upgrade.rs     Fedora release-upgrade availability detection
├── helper_protocol.rs     RUN <id> / __END__ <code> wire format & strict HelperCommand allowlist
├── helper_mode.rs         Privileged allowlist worker implementation (dnf / fwupdmgr)
├── style.css              WCAG-checked Observatory & Libadwaita theme tokens
├── backend/
│   ├── mod.rs             NativeOutcome enum, user binary lookup, progress & reboot dispatch
│   ├── brew.rs            Homebrew JSON discovery, progress hints, and unprivileged apply
│   ├── cleanup.rs         Unused Flatpak ref & Homebrew autoremove dry-run discovery and cleanup
│   ├── containers.rs      Rootless Distrobox & Toolbx container discovery, deduplication, and upgrade
│   ├── dnf_dbus.rs        org.rpm.dnf.v0 D-Bus client (zero-auth check, advisories, history, download/transaction progress)
│   ├── dnf.rs             dnf5 / classic DNF check, repoquery & apply stdout/stderr parsers
│   ├── flatpak.rs         libflatpak.so.0 dynamic loader, FlatpakTransaction apply & CLI fallback
│   ├── fwupd_dbus.rs      org.freedesktop.fwupd D-Bus client (battery safety, release notes, history, progress)
│   ├── fwupd.rs           fwupdmgr JSON/text parsers & progress hints
│   ├── gnome_extensions.rs org.gnome.Shell.Extensions session D-Bus check & selective install
│   ├── hooks.rs           Opt-in unprivileged CustomHook definitions, static HOOK_TEMPLATES & check parser
│   ├── logind.rs          org.freedesktop.login1 CanReboot / inhibitor detection & reboot execution
│   ├── mise.rs            User-space mise outdated JSON parser & selective upgrade
│   ├── rpm_ostree.rs      Atomic Fedora rpm-ostree status, diff, advisories & staged deployment
│   └── rustup.rs          User-space rustup check parser & selective toolchain/self-update plan
├── privilege/
│   ├── mod.rs             Privilege session module exports
│   ├── pool.rs            Pooled privileged session lifecycle
│   └── session.rs         pkexec / direct helper pipe manager
└── ui/
    ├── mod.rs             UI module exports
    ├── window.blp         Declarative Libadwaita / Blueprint layout compiled via build.rs
    ├── window.rs          Main window controller, actions, shortcuts & reactive state rendering
    ├── sidebar.rs         Observatory sidebar (source filters, view switcher, live sparkline, execution queue)
    ├── queue_list.rs      Virtualized GtkListView package queue & selective checkbox bindings
    ├── inspector.rs       Slide-out Package Inspector (CVE/RHBZ advisories, severity, version & size deltas)
    ├── history.rs         Searchable/filterable merged DNF5 + fwupd Update History timeline
    ├── activity_log.rs    Embedded filterable Activity Log view
    ├── charts.rs          Cairo-rendered throughput sparkline & progress charts
    ├── dialogs.rs         About, Release Upgrade, cleanup confirm, error detail & confirmation dialogs
    ├── preferences.rs     Libadwaita Preferences dialog, source switches, and Custom Hook editor with presets
    ├── rows.rs            Package row widgets and status badges
    ├── prefs.rs           Standalone XDG JSON UI/source/hook persistence ($XDG_CONFIG_HOME/fedora-updater/ui.json)
    └── preview.rs         FEDORA_UPDATER_PREVIEW states & FEDORA_UPDATER_SCREENSHOT capture
```

</details>

---

## Contributing & Security

- Contributions are welcome! Read [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening a pull request.
- Found a bug or have an idea? [Open an issue](https://github.com/rkalla/fedora-updater/issues) using the bug report or feature request form.
- To report a security vulnerability privately, see [`SECURITY.md`](SECURITY.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
