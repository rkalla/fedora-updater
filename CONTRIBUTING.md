# Contributing to Fedora Updater

Thank you for your interest in contributing! Whether you are fixing a bug, refining GNOME / Libadwaita UX details, or improving DNF/Flatpak/fwupd backend reliability, contributions are very welcome.

## Development Setup

### Prerequisites (Fedora 42+ / Fedora 44 recommended)

```bash
sudo dnf install gtk4-devel libadwaita-devel blueprint-compiler gcc pkgconf-pkg-config rust cargo
```

(`libflatpak.so.0` is loaded dynamically at runtime via `dlopen`, so `flatpak-devel` is not required to compile.)

### Building

```bash
./build          # stripped LTO release binary → ./bin/fedora-updater
./build --debug  # faster unstripped debug binary → ./bin/fedora-updater
```

## Rapid UI Iteration with Preview Presets

You do not need real pending system/firmware updates to test or design UI states. Launch any built-in sample state with `FEDORA_UPDATER_PREVIEW`:

```bash
FEDORA_UPDATER_PREVIEW=ready-native ./bin/fedora-updater
FEDORA_UPDATER_PREVIEW=running-sparkline ./bin/fedora-updater
FEDORA_UPDATER_PREVIEW=history ./bin/fedora-updater
FEDORA_UPDATER_PREVIEW=done-reboot ./bin/fedora-updater
```

See [`src/ui/preview.rs`](src/ui/preview.rs) for the full list of presets (`idle`, `checking`, `ready`, `ready-warn`, `running`, `done-partial`, `failed`, `log`, etc.), or pass `FEDORA_UPDATER_SCREENSHOT=/path/to/shot.png` to capture a window snapshot automatically.

## Running the Test & Lint Suite

Every pull request should pass all four checks with zero warnings:

```bash
# 1. Fast offline unit, state-machine, functional, and pipe E2E suite (< 1s)
cargo test

# 2. Headless GTK4 / Libadwaita widget, row, window, and dialog E2E tests
./scripts/test-ui-headless.sh

# 3. Strict Clippy check
cargo clippy --all-targets -- -D warnings

# 4. Release binary build
./build
```

Opt-in live-host tests that hit real `/usr/bin/dnf` or host D-Bus services are annotated with `#[ignore]` and can be run manually with `cargo test -- --ignored`.

## Architectural & Security Invariants

Please preserve these core invariants when submitting changes:

1. **Zero-password D-Bus checks & history:** `dnf5daemon-server` sessions in [`src/backend/dnf_dbus.rs`](src/backend/dnf_dbus.rs) must call `SessionManager.open_session` with an empty option map (`{}`) so `org.rpm.dnf.v0.base.Config.override` (`auth_admin`) never prompts during update discovery or history queries.
2. **Strict `--helper` allowlist:** [`HelperCommand::argv()`](src/helper_protocol.rs) in [`src/helper_protocol.rs`](src/helper_protocol.rs) permits only fixed, hardcoded `/usr/bin/dnf` and `/usr/bin/fwupdmgr` argument lists with zero environment-variable overrides in `--helper` mode.
3. **Single portable binary & zero external runtime deps:** Keep `tokio` out of `Cargo.toml` (use GLib/GTK main-loop channels and small blocking OS threads/zbus blocking proxies), and keep UI preferences in standalone XDG JSON (`$XDG_CONFIG_HOME/fedora-updater/ui.json`, no GSettings schemas).
4. **Offline deterministic tests:** Keep `cargo test` 100% offline using the checked-in [`tests/fixtures/`](tests/fixtures/README.md) corpus and `OrchestratorRuntime` / pipe harnesses. Mark any new live-system probe with `#[ignore]`.

## Opening a Pull Request

- Keep changes focused and well-scoped.
- Add or update unit/functional/UI tests covering new behavior or bug fixes.
- Run all checks listed above before pushing.
- If modifying Blueprint templates ([`src/ui/window.blp`](src/ui/window.blp)) or CSS ([`src/style.css`](src/style.css)), attach screenshots showing the updated UI.
