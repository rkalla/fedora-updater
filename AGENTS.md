# AGENTS.md — Fedora Updater

Guidance for coding agents working in this repository.

## Codebase Ownership & Design

- **Act as a long-term maintainer, not a one-off patch writer.** Prefer clean, unified designs over bolting onto awkward structure or duplicating logic.
- **Refactor in path:** When touching an area with duplicated code, bloated functions, or tangled abstractions, consolidate and clean it up as part of the change rather than adding another copy-pasted branch.
- **Push back on poor fit:** If a requested change conflicts with the architecture or would accumulate avoidable debt, say so and propose a cleaner design before writing code.
- **Preserve behavior:** Never silently alter user-facing behavior, CLI/IPC contracts, or privilege/security boundaries unless explicitly requested. If a cleaner design requires a behavior change, propose it and wait for confirmation.
- **Match scope to the task:**
  - For broad features or refactors, reshape module boundaries freely.
  - For small bug fixes, keep refactors scoped to the code directly involved and call out larger structural cleanups as follow-up ideas instead of bundling them unasked.

## Architecture Overview

Single multi-call Rust binary (`fedora-updater` GNOME/GTK4/Libadwaita GUI and `fedora-updater --helper` pkexec fallback helper) backed by the `fedora_updater` library crate (`src/lib.rs`).

- **State & presentation (pure/testable):**
  - `src/model.rs` — domain types, update items, history, metrics.
  - `src/state.rs` — pure state machine (`AppState`, `Event`, `Phase`, `reduce`).
  - `src/presenter.rs` — view-model projection from `AppState` to UI state.
  - `src/console.rs` — streaming console output buffer and deltas.
- **Execution & backends:**
  - `src/orchestrator.rs` — check/apply/history/release-upgrade workflow execution (`OrchestratorRuntime`).
  - `src/release_upgrade.rs` — Fedora release-upgrade flow.
  - `src/backend/` — `dnf_dbus.rs` / `dnf.rs`, `flatpak.rs` (runtime `dlopen` of `libflatpak.so.0`), `fwupd_dbus.rs` / `fwupd.rs`, `logind.rs`.
- **Privilege boundary:**
  - `src/helper_protocol.rs` — wire protocol and strict hardcoded command allowlist (`HelperCommand::argv()`).
  - `src/helper_mode.rs` — `--helper` entrypoint executed under `pkexec`.
  - `src/privilege/` — `session.rs` / `pool.rs` managing helper sessions.
- **GTK4 / Libadwaita UI:**
  - `src/ui/` — Blueprint template (`window.blp`), `window.rs`, `sidebar.rs`, `queue_list.rs`, `rows.rs`, `inspector.rs`, `charts.rs`, `history.rs`, `activity_log.rs`, `dialogs.rs`, `prefs.rs`, `preview.rs`.
  - `src/style.css` — Libadwaita stylesheet.

## Non-Negotiable Invariants

1. **Zero-password D-Bus checks & history:** `dnf5daemon-server` sessions in `src/backend/dnf_dbus.rs` must call `SessionManager.open_session` with an empty option map (`{}`) so `org.rpm.dnf.v0.base.Config.override` (`auth_admin`) never triggers a Polkit prompt during discovery or history queries.
2. **Strict `--helper` allowlist:** `HelperCommand::argv()` in `src/helper_protocol.rs` must permit only fixed, hardcoded `/usr/bin/dnf` and `/usr/bin/fwupdmgr` argument lists, with zero environment-variable overrides in `--helper` mode.
3. **Single portable binary & lean runtime:** Do not add `tokio` to `Cargo.toml` (use GLib/GTK main-loop channels, `async-channel`, blocking OS threads, and `zbus` blocking proxies). Keep UI preferences in standalone XDG JSON (`$XDG_CONFIG_HOME/fedora-updater/ui.json`, no GSettings schemas). Keep `libflatpak.so.0` dynamically loaded at runtime.
4. **Offline deterministic tests:** `cargo test` must remain 100% offline using `tests/fixtures/` and test harnesses. Annotate any live-host D-Bus/DNF test with `#[ignore]`.
5. **80/20 developer toolchain vs. Custom Hook boundary:** Built-in developer toolchain backends require an isolated, unprivileged manager with a fast, read-only check query producing exact per-item `old -> new` diffs and selective apply (`rustup`, `mise`). Everything else (`uv`, `pipx`, `cargo-update`, `npm -g`, `nvm`/`fnm`, `pyenv`, `sdkman`, `conda`, etc.) belongs in opt-in, strictly unprivileged `CustomHook` entries (assisted by static `HOOK_TEMPLATES` form presets in Preferences) rather than hardcoded CLI output parsers or DSLs.

## Verification

Before committing code changes, run the relevant checks (all four for full code changes; skip unaffected build/test steps for docs-only edits):

```bash
cargo test                                # fast offline unit/state/functional/pipe tests
./scripts/test-ui-headless.sh             # headless GTK4/Libadwaita UI E2E tests
cargo clippy --all-targets -- -D warnings # strict lints, zero warnings
./build                                   # stripped LTO release binary -> ./bin/fedora-updater
```

For UI iteration without live updates, use `FEDORA_UPDATER_PREVIEW=<preset> ./bin/fedora-updater` (see `src/ui/preview.rs`) and optional `FEDORA_UPDATER_SCREENSHOT=/path/to/shot.png`.
