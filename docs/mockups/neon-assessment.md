# NEON Redesign Assessment — Fedora Updater (Next-Gen Architecture & UX Blueprint)

**Prepared by:** NEON Design & Architecture  
**Target Stack:** Rust · GTK 4 (4.14–4.22; host verified at `4.22.5`) · libadwaita (`1.7+`; host verified at `1.9.4`) · Blueprint (`*.blp`) · Fedora 44 Workstation  
**Deliverables:**
- `docs/mockups/neon-assessment.md` — Forensic diagnosis, full state/flow/edge-case inventory, Next-Gen native platform proposal, and 1:1 GTK4/libadwaita translation playbook
- `docs/mockups/neon-index.html` — NEON Interactive Showcase Hub & Comparison Matrix
- `docs/mockups/neon-variant-a.html` — **Variant A: "Lumen"** (Refined Single-Column Editorial)
- `docs/mockups/neon-variant-b.html` — **Variant B: "Observatory"** (Adaptive Split-Pane Workbench)
- `docs/mockups/neon-variant-c.html` — **Variant C: "Cadence"** (Stage-Driven Guided Bento Flow)

---

## Executive Summary

The original Grok-era UI was terse and utilitarian. The subsequent `libadwaita` audit (`docs/mockups/libadwaita-audit.html`) fixed several hygiene issues (adding `Adw.AlertDialog`, `Adw.HeaderBar` menu, `Adw.ToastOverlay`, `Adw.Breakpoint`, and removing raw HTML-style stat cards), **but stripping visual hierarchy without replacing it with intentional geometry, spacing, and telemetry turned the app into a "hobbled facsimile"**:

- The `Ready` screen (`screenshots/ready.png` & `src/ui/window.blp`) lost its summary framing, orphaned captions on the left edge, retained redundant source labels, and dropped `old → new` versions on real DNF checks.
- The `Running` screen (`screenshots/running.png`) jammed widgets flush against an unpadded `.card` border, used `opacity: 0.0` to hide the spinner (leaving a permanent dead gap), misaligned the progress bar width against `Adw.Clamp`, and trapped the completed expander outside the scroll region.
- The `Done` screen (`screenshots/done.png` → `src/ui/window.rs`) overcorrected by erasing visual closure altogether and cramming summary telemetry into a clipping `Adw.WindowTitle` subtitle.

At the same time, **this host already runs `dnf5daemon-server 5.4.8.0`, `fwupd 2.1.9`, `libflatpak.so.0`, and `systemd-logind`**. Moving from CLI stdout scraping (`pkexec` + `stdbuf` + regex) to Fedora's native D-Bus / C-library APIs eliminates password prompts during Update Checks, unlocks real CVE advisories, real `old → new` versions, download throughput (MB/s), selective updates, offline updates, update history, and firmware battery safety guards.

---

## 1. Forensic Diagnosis: Why the Current Libadwaita Refresh Feels Hobbled

Every defect below was verified directly in the current codebase (`src/ui/window.blp`, `src/ui/window.rs`, `src/ui/rows.rs`, `src/style.css`, `src/backend/dnf.rs`, `src/model.rs`, `screenshots/*.png`):

| # | Exact Location | Observed Defect | UX Consequence |
|---|---|---|---|
| **1** | `src/ui/window.blp` L356–416 | `current_package` has `styles ["card"]` with **zero inner margins or padding** (`style.css` defines no padding on `.card`). | Badge, title, and percentage slam flush against the rounded card border—looking broken next to spacious GNOME apps. |
| **2** | `src/style.css` L22–24 vs `src/ui/window.blp` L224, L265, L348, L427, L485 | `.progress-block` spans the full `760px` window width (`margin: 0 12px 12px`—zero top margin under HeaderBar), while `Adw.Clamp` beneath it caps at `700px` (+ 12px margins). | Progress bar edges visibly overhang the cards and `Adw.PreferencesGroup` below; zero top breathing room beneath the window chrome. |
| **3** | `src/ui/window.blp` L276–283 | Caption `"Nothing is applied until Update All is chosen"` sits outside `Adw.Clamp` at `margin-start: 12`. | Orphaned caption hugs the far-left window edge while the package list sits centered inside `700px` clamp. |
| **4** | `src/ui/window.rs` L807–811 & L893–900 | `now_playing_spinner.set_opacity(0.0)` hides the `18x18` spinner visually while keeping its layout allocation. | Leaves a dead empty square gap between the `DNF`/`FP`/`FW` badge and package title whenever per-item percentage is shown. |
| **5** | `src/ui/rows.rs` L37, L104–107 & `src/backend/dnf.rs` L103–146 & `src/model.rs` L178 | Triple source redundancy: prefix `[DNF]` badge + subtitle `· System` + fallback trailing `[DNF]` badge when `size_bytes` is `None`. Meanwhile `parse_classic_line`/`parse_dnf5_table_line` never populate `old_version`, so live DNF rows show `version · updates · System` instead of `old → new` (and Flatpak shows `128.2.0 · flathub · user · Apps`). | Visual noise on every row, wasted horizontal space, and less useful version comparison in production than in `preview.rs` mock data! |
| **6** | `src/style.css` L60–63 | `.package-badge.src-flatpak` uses `color: @purple_1` (`#dc8add`) text on tinted background. | Passes dark mode barely, **fails WCAG contrast in Adwaita Light mode** (pastel lilac text on near-white). |
| **7** | `src/ui/window.blp` L439–454 & `src/ui/window.rs` L717–729 | `run_completed_group` (`Adw.ExpanderRow`) lives **outside** `run_list_scroll`. | Expanding a 45-row completed transaction crushes the active remaining list or forces window resizing (prompting the `restore_default_window_size` hack). |
| **8** | `src/ui/window.rs` L1105–1195 & `src/ui/rows.rs` L150–164 | `rows::refill_group` tears down and rebuilds every `ActionRow` widget on every `BackendCheckDone` or phase transition. | Non-virtualized O(N) widget churn causes frame drops during 100+ package DNF transactions. |
| **9** | `src/ui/window.blp` L52–105 & `src/ui/window.rs` L1034 | `upgrade_notice` sits in a second `[top]` toolbar slot across **every** `Phase`—including `Authenticating`, `Checking`, and `Running`. | A persistent giant card competes with active downloads and steals vertical height when the user is mid-transaction. |
| **10** | `src/ui/window.rs` L1143–1196 & `src/presenter.rs` L332–345 | `Done` phase has no hero closure—just a `Gtk.ScrolledWindow` under a dense header subtitle (`"6 upgraded · 4 system / 1 apps / 1 firmware · 6m 42s"`) that clips early on medium/narrow windows. | Feels abrupt and anti-climactic after a 6-minute system update; lacks clear confirmation of *which* packages triggered a reboot. |
| **11** | `src/ui/window.blp` L14–49, L286–299, L552–604 | `Adw.ToolbarView` omits `top-bar-style: flat` / `bottom-bar-style: raised` (or `border`) separation, and the bottom console uses custom `Gtk.Revealer` instead of `Adw.BottomSheet` / `Gtk.TextView` padding polish. | Scrollable lists bleed awkwardly into top/bottom actions without Adwaita's crisp hairline/undershoot treatment. |

---

## 2. Exhaustive State Machine, Flow & Edge-Case Inventory

Verified across `src/state.rs`, `src/orchestrator.rs`, `src/presenter.rs`, `src/release_upgrade.rs`, `src/console.rs`, `src/backend/{mod,dnf,flatpak,fwupd}.rs`, `src/privilege/{pool,session}.rs`, `src/helper_mode.rs`, `src/helper_protocol.rs`, `src/main.rs`, and `src/ui/{window,dialogs,rows,preview}.rs`:

### 2.1 Core Phases & Sub-States
1. **`Phase::Idle(IdleScreen::Fresh)`** — Initial state before any check. `default_widget` focused on `"Check for Updates"` suggested pill so Enter triggers Check.
2. **`Phase::Idle(IdleScreen::Current { checked_at, detail })`** — After empty check (`checked_at: "just now"`) or after leaving `Done` (`ResetToIdle`). If soft errors occurred during an otherwise-empty check, `detail` carries `"No updates available (<joined errors>)"`.
3. **`Phase::Authenticating { purpose, packages, last_checked }`** — Waiting for `pkexec` polkit dialog. Skipped completely (`can_skip_auth_ui()`) if helper session is already alive in `privilege::pool::SESSION`. Supports `CancelAuth` (increments `WORK_GEN`, returns to `Idle` preserving `last_checked`!). Never collects password in-app.
4. **`Phase::Checking { packages_so_far, soft_errors, checked_sources }`** — Sequential checks across 4 backends grouped into 3 user-facing groups (`System` = `Dnf`, `Apps` = `FlatpakUser` + `FlatpakSystem`, `Firmware` = `Firmware`). Progress starts at `0.05` (so bar never looks frozen), increments per backend (`0.25`, `0.50`, `0.75`, `1.00`), shows `about_time_left` once `progress >= 0.08 && elapsed >= 5s`, streams discovered packages, and reveals live console drawer.
5. **`Phase::Ready { packages, soft_errors }`** — Sorted by `apply_order()` (`Dnf` → `FlatpakUser` → `FlatpakSystem` → `Firmware`) then `name`. Shows count, summary + total download bytes (if known), optional `Adw.Banner` for `soft_errors`, Refresh button, and single `Update All` CTA.
6. **`Phase::Running(RunningState)`** — Sequential apply (`RunPass::Download` → `RunPass::Apply` detected via `should_enter_apply`). Tracks `active_index`, `current_name`, `work_progress` (per-item %), `stage` (`[cur/total]`), `overall_progress`, `needs_reboot`, and `failed_sources`.
7. **`Phase::Done { packages, duration, needs_reboot, failed }`** — Covers three distinct visual situations:
   - Clean finish (`failed == 0 && !needs_reboot`)
   - Reboot recommended (`needs_reboot == true`) — surfaces `Restart Now` as the sole suggested action, demoting `Done` to default style.
   - Partial failure (`0 < failed < packages.len()`) — header changes to `"Updates Finished With Problems"`, failed rows sort to the top.
   *(Note: if `failed == packages.len()` and non-empty, `reduce()` converts to `Phase::Failed` with `"All updates failed"`!)*
8. **`Phase::Failed { title, detail, retry }`** — Triggered by `AuthDenied`, helper spawn failure, all packages failing, `StopApply { during_transaction: true }`, or `systemctl reboot` failure (`RetryAction::Reboot` switches primary button label to `"Restart"`).

### 2.2 Critical Edge Cases & System Invariants (Must Survive Redesign)
1. **Stale Update List (`UPDATE_LIST_MAX_AGE` = 24h)**: `update_list_needs_refresh(now)` checks `updates_listed_at` (ignoring backwards clock jumps and `None` preview lists). If stale, clicking `Update All` opens `Adw.AlertDialog` (`"Refresh Update List?"`) → sets `apply_when_ready = true` (`StartCheck { then_apply: true }`) → auto-starts `begin_apply` immediately upon non-empty `CheckComplete`.
2. **Soft Check Errors**: Failure in one backend (e.g., `fwupd` offline or Flatpak remote unreachable) must **not** abort the remaining backends. Surfaces concise `Adw.Banner` (`"1 source could not be checked"` / `"N sources could not be checked"`) with `"View Log"` action.
3. **Quiet / Stall / Daemon Contention (`quiet_ui`)**:
   - `quiet_secs >= 5 && at_full_item`: show spinner over stale 100% item.
   - `quiet_secs >= 5` during DNF Download: poll `/var/cache/libdnf5/<repo>/packages/*.rpm` growth → show `"Downloading <pkg> — <size> and rising"`.
   - `quiet_secs >= 15`: suppress ETA (`show_eta = false`) rather than lying with a drifting estimate.
   - `quiet_secs >= 120` (no cache growth): reveal stall `Adw.Banner` (`"No output for N minutes."`) + scan `/proc/*/comm` for `dnf5daemon-server` → append `"Waiting on GNOME Software."`.
4. **Two-Tier Stop Semantics**:
   - During `RunPass::Download`: button is plain `"Stop"`, cancels immediately (`cancel_background_work` + `kill_recorded_helper`), resets all packages to `Pending`, returns to `Phase::Ready` with soft banner `"Stopped before install. Downloaded packages were not applied."`.
   - During `RunPass::Apply`: button becomes destructive `"Stop…"` (`stop_update_dialog()`), warns about RPM database state, transitions to `Phase::Failed` (`"Update stopped during install"`).
5. **Multi-Arch RPM Twins & Token-Exact Matching**: `matches_progress_name` matches `kernel` strictly without hijacking `kernel-core`, and `apply_progress_hint` skips already-done `i686` rows so the `x86_64` twin updates properly.
6. **Unlisted Transitive Dependencies**: When DNF logs a dependency not in `Ready.packages`, `running.current_name` updates for the active card while `active_index` clears safely.
7. **Flatpak Related Extensions (`libflatpak` FFI)**: `query_raw` resolves `FlatpakTransaction` up to `ready-pre-auth` then cancels before downloading, surfacing extensions like `org.freedesktop.Platform.GL.nvidia-*` on the parent app row while deduplicating shared download bytes across parents.
8. **Carriage-Return Console (`ConsoleBuffer`)**: Bounded to `8,000` lines; `\r` progress redraws emit `ConsoleDraw` (`ReplacedLast`) so live progress updates replace the active line in place instead of flooding scrollback.
9. **Reboot Detection Matrix**: Triggered by core DNF allowlist (`kernel`, `kernel-core*`, `kernel-modules*`, `kernel-uki*`, `glibc`, `systemd`, `dbus*`, `linux-firmware`, `microcode_ctl`, `amd-ucode-firmware`, `intel-microcode`, `dracut`, `grub2*`, `shim*` — explicitly excluding `kernel-tools` & `kernel-devel`), DNF log strings, or `fwupdmgr check-reboot-needed --json` returning exit `0`.
10. **Reboot Block Inhibitor Recovery**: When `systemctl reboot` fails due to a systemd shutdown block inhibitor (`reboot_failure_detail`), `RetryAction::Reboot` offers an immediate `"Restart"` recovery pill.
11. **Fedora Release Upgrade Advisor**: Polls `https://admin.fedoraproject.org/pkgdb/api/collections/` vs `/etc/os-release` `VERSION_ID`, selects highest `Active` `Fedora Linux` version (`> current`), calculates `days_since_release`, and presents two **separate non-chained** copyable commands (`sudo dnf system-upgrade download --releasever=N` then `sudo dnf system-upgrade reboot`) because `dnf` prompts interactively during download.
12. **Non-Blocking Teardown**: Window close fires `kill_recorded_helper()` (`kill(-pid, SIGKILL)`) + `PR_SET_PDEATHSIG` inside child pre_exec so a hung `fwupdmgr` never freezes the GTK thread or leaves orphan root helpers.
13. **Single-Instance & Preview Mode**: `dev.fedora.Updater` enforces single-instance desktop activation unless `FEDORA_UPDATER_PREVIEW` or `FEDORA_UPDATER_SCREENSHOT` sets `NON_UNIQUE`.
14. **Adaptive Narrow Viewport**: Breakpoint at `max-width: 500px` must keep lists, progress, and primary actions unobstructed without crushing header titles.
15. **Theme Fidelity**: Seamless support for Adwaita Dark, Adwaita Light, and High-Contrast (`--dim-opacity` via `.dimmed` instead of hardcoded `opacity: 0.72`).

---

## 3. BONUS: Next-Gen Native Platform Architecture (Verified on Fedora 44)

Probing the live Fedora 44 environment confirms all native system daemons and libraries required to eliminate fragile stdout scraping are **already installed and active**:

| Component | Current Implementation | Verified Fedora 44 Native Surface | Key UX & Reliability Wins Unlocked |
|---|---|---|---|
| **System Packages (DNF)** | `pkexec fedora-updater --helper` → `stdbuf -o0 -e0 /usr/bin/dnf ...` + regex + `/var/cache/libdnf5` polling | **System D-Bus `org.rpm.dnf.v0`** (`dnf5daemon-server 5.4.8.0` active) | **1) Zero-password Update Check:** Per `/usr/share/polkit-1/actions/org.rpm.dnf.v0.policy`, `open_session`, `Base.read_all_repos`, `Rpm.list(scope="upgrades")`, `Advisory.list(availability="updates")`, `Goal.resolve()`, and `History.list()` require **no polkit prompt**! Only `Goal.do_transaction()` prompts once (`auth_admin_keep` on `execute_trusted_transaction`). *(Note: avoid `base.Config.override` which triggers `auth_admin`.)*<br>**2) Eliminates GNOME Software Lock Contention:** Uses the exact daemon GNOME Software talks to.<br>**3) Real `old → new` & Real CVEs:** `Rpm.list` + `Advisory.list` expose `advisoryid`, `type` (`security`/`bugfix`/`enhancement`), `severity`, `description`, `download_size`, `install_size`, and `reference_cves` / Bugzilla IDs.<br>**4) Typed Telemetry:** `download_progress(id, total, downloaded)` gives exact MB/s throughput & overall byte progress; `transaction_action_progress`, `transaction_verify_progress`, `transaction_script_start`, `Goal.cancel()` (clean download cancel), `History.list()`, and `Offline.schedule_for_next_boot()`. |
| **Flatpak Apps & Runtimes** | `libflatpak.so.0` `dlopen` check (up to `ready-pre-auth`) + `flatpak update` CLI subprocess | **Full `libflatpak.so.0` `FlatpakTransaction` API** | `FlatpakTransactionProgress` emits real-time `progress`, `bytes_transferred`, `status`, and `is_estimating`; `GCancellable` enables clean cancellation; AppStream metadata supplies real app icons, summaries, and release changelogs. Verified `org.freedesktop.Flatpak.policy` sets `allow_active = yes` for `app-update`, `runtime-update`, and `appstream-update`. |
| **Device Firmware** | Helper running `fwupdmgr refresh` / `get-updates --json` / `update -y ...` | **System D-Bus `org.freedesktop.fwupd`** (`fwupd 2.1.9` active; `/usr/lib64/libfwupd.so.3` present) | Exposes `GetDevices`, `GetUpgrades`, `GetReleases`, `PercentageFull`, `Status`, `DeviceRequest` signals, `GetHistory`, and `BatteryLevel` vs `BatteryThreshold` (allowing UI pre-flight warnings before flashing BIOS on low battery!). Verified `org.freedesktop.fwupd.policy` allows trusted LVFS updates (`allow_active = yes`). |
| **System Reboot & Notifications** | `systemctl reboot` subprocess + text stderr grep | **System D-Bus `org.freedesktop.login1.Manager`** + **`gio::Notification`** | Pre-flight `CanReboot()` & `ListInhibitors()` check before showing Reboot CTA, `Reboot(true)` for polkit-aware reboot, plus desktop completion notifications when running in the background. |

### Two-Tier Migration Strategy
- **Tier 1 (Immediate UI Polish — Zero Backend Risk):** Replace `window.blp`, `rows.rs`, `dialogs.rs`, `style.css`, and `presenter.rs` with the chosen NEON design variant (`A`, `B`, or `C`) on top of the existing `orchestrator.rs` + `state.rs`. Every core layout fix, GSK ring, padded card, unified clamp, `Adw.BottomSheet` console, and `Gtk.ListView` virtualization ships immediately.
- **Tier 2 (Next-Gen Native Engine):** Incrementally replace `helper_mode.rs` + CLI parsers with `zbus` (D-Bus async client for `org.rpm.dnf.v0`, `org.freedesktop.fwupd`, `org.freedesktop.login1`) and expanded `libflatpak.so.0` FFI. Because `state.rs` already consumes pure `Event` enums, the UI layer does not rewrite when Tier 2 lands—hidden telemetry slots (CVEs, `old → new` DNF diff, MB/s sparkline, History, Selective Update checkboxes, Offline Update toggle) simply light up with real data.

---

## 4. 1:1 GTK4 / libadwaita Translation Playbook

Every visual element inside all three NEON mockups adheres strictly to real GTK4 / libadwaita 1.7–1.9 capabilities:

- **No impossible effects:** Zero `backdrop-filter`, zero arbitrary multi-axis CSS grid masonry, zero custom native OS scrollbar overrides inside `.gtk-win`.
- **Virtualization (`Gtk.ListView` + `Gtk.SignalListItemFactory`):** Replace `rows::refill_group` O(N) widget destruction with `gio::ListStore` + `Gtk.FilterListModel` (`Gtk.CustomFilter` for source/severity/search) + `Gtk.ListView` styled with `.boxed-list` / `.rich-list` row chrome.
- **Zero Ghost Gaps (`Gtk.Stack`):** Replace `set_opacity(0.0)` on the active spinner with a fixed `20×20` `Gtk.Stack` (`crossfade`, `150ms`) swapping `Adw.Spinner` ↔ symbolic status icon / progress badge.
- **Native Custom Graphics (`Gtk.DrawingArea` + Cairo / `GskPath`):**
  - **Circular Progress Ring (44–48px):** 3px Cairo arc (`M_PI * -0.5` start) interpolated smoothly via `Adw.SpringAnimation` (`damping: 1.0`, `stiffness: 120`).
  - **Proportional Breakdown Bar (6px height):** Rounded-cap Cairo stacked segments (`DNF` blue / `Flatpak` iris / `Firmware` amber).
  - **Live Throughput Sparkline (Variant B):** 48-point rolling Cairo polyline with 12% accent fill gradient under the curve.
- **WCAG-Safe Semantic Palette Tokens:**
  - *Adwaita Light:* `--accent-color: #1c71d8`, `--success-color: #1a8345`, `--warning-color: #9e5a00`, `--error-color: #c01c28`, `--flatpak-color: #813d9c` (replaces unreadable `#dc8add`).
  - *Adwaita Dark:* `--accent-color: #78aeed`, `--success-color: #57e389`, `--warning-color: #f8c555`, `--error-color: #ff7b72`, `--flatpak-color: #dc8add`.
- **Typography & Rhythm:** Standard `4 / 8 / 12 / 16 / 20 / 24 px` spatial scale; `font-variant-numeric: tabular-nums` (`.numeric`) on all percentages, byte sizes, counts, and version diffs; single `680px` `Adw.Clamp` spine in single-column variants (`A` & `C`) or `230px + flex` `Adw.NavigationSplitView` in `B`.
- **Blueprint Widget Parity:**
  - `Adw.ToggleGroup` (`v1_7`) for filter tabs and view switches
  - `Adw.BottomSheet` (`v1_6`) or `Gtk.Revealer` dock for the expandable live console
  - `Adw.WrapBox` (`v1_7`) for responsive Bento cards & metadata pills at `<500px`
  - `Adw.NavigationSplitView` + `Adw.OverlaySplitView` (`v1_4`) for Variant B's adaptive sidebar & package inspector
  - `Adw.SplitButton` (`v1_0`) for Variant B's Update / Offline / Download-Only menu
  - `Adw.ToolbarView` with `top-bar-style: flat` and `bottom-bar-style: raised` for crisp separation
