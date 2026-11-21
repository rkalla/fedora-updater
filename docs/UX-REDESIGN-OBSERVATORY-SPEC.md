# Fedora Updater — Observatory UX Redesign & Native Platform Specification

**Document ID:** `UX-REDESIGN-OBSERVATORY-SPEC`  
**Status:** Locked Design Direction — **Variant B "Observatory" (Dual-Mode)**  
**Authored by:** NEON Design & Architecture  
**Codebase Baseline:** `main` @ `49899d9` (`#19` test-hardening program `#20`–`#25` complete; includes recovery UX `c130c3e`, `OrchestratorRuntime` seam `#23`, headless GTK4 E2E suite `#24`, and follow-ups `#26`–`#27`)  
**Visual Reference Mockup:** [docs/mockups/neon-variant-b.html](docs/mockups/neon-variant-b.html)  
**Forensic Audit & Platform Probe:** [docs/mockups/neon-assessment.md](docs/mockups/neon-assessment.md)  
**Delivery Model:** Two sequential, independently shippable Programs (**Phase 1: Observatory UI Rebuild** → **Phase 2: Native Fedora 44 Platform Engine**)

---

## Executive Summary

**Variant B — "Observatory"** is locked as the Next-Gen UX architecture for Fedora Updater. It delivers a dual-mode native GNOME / libadwaita experience ("have our cake and eat it too"):

1. **Expanded Workbench Mode (Default Desktop `960 × 676`):** A `220px` tonal `Adw.NavigationSplitView` sidebar provides persistent system pulse telemetry, source filters, embedded `Activity Log` / `Update History` views, and a dedicated footer slot for Fedora release-upgrade guidance. The detail pane hosts virtualized package rows, selective controls, an `Adw.SplitButton` action menu, and a slide-out `Adw.OverlaySplitView` Package Inspector.
2. **Focused "Lumen" Mode (Sidebar Hidden or Collapsed `< 720px`):** Toggling the HeaderBar sidebar button (`sidebar-show-left-symbolic`) hides the navigation sidebar and automatically centers the detail workspace inside a `680px Adw.Clamp` spine—delivering the calm, single-column focus of Variant A on demand, with sidebar preference persisted cleanly via XDG config JSON.

Implementation proceeds in **two strict phases** so UI polish ships safely with zero execution-layer regression risk before swapping the underlying backends to native Fedora 44 system APIs:

* **Phase 1 (Tier 1 — Observatory UI Rebuild):** Replaces `src/ui/*` and `src/style.css` (plus small presentation/model view helpers) on top of the existing `orchestrator` / `helper_mode` / `libflatpak` discovery pipeline. Fixes all 11 visual and structural defects identified in [docs/mockups/neon-assessment.md](docs/mockups/neon-assessment.md) and honors every recovery invariant added in `c130c3e`.
* **Phase 2 (Tier 2 — Native Fedora 44 Platform Engine):** Incrementally replaces CLI stdout scraping (`pkexec` + `stdbuf` + regex + `/var/cache/libdnf5` cache polling) with Fedora 44's verified system D-Bus and C-library APIs (`org.rpm.dnf.v0`, `org.freedesktop.fwupd`, `org.freedesktop.login1`, and extended `libflatpak.so.0` `FlatpakTransaction`), lighting up zero-password Update Checks, real CVE advisories, real `old → new` RPM versions, live MB/s throughput sparklines, selective updates, offline reboot staging, and update history—with graceful CLI fallback.

---

## 1. Locked UX Direction & Dual-Mode Adaptive Geometry

### 1.1 Dual-Mode Shell Behavior

| Mode | Trigger | Window Geometry | Sidebar Behavior | Detail Geometry | Inspector Behavior |
|---|---|---|---|---|---|
| **Workbench Mode** | Default desktop (`width >= 720px`, `sidebar_open = true`) | Default `960 × 676` (user-resizable) | `Adw.NavigationSplitView` left pane visible (`min-sidebar-width: 210`, `max-sidebar-width: 260`, `sidebar-width-fraction: 0.24`, ~`220–240px`) | Fills remaining width (`~720px`) with `Adw.Clamp (maximum-size: 680)` around stacked content | `Adw.OverlaySplitView` right pane (`282px`, `collapsed: false` when a row is active; user-closable) |
| **Focused "Lumen" Mode** | User clicks HeaderBar sidebar toggle (`sidebar_open = false`) at `width >= 720px` | Keeps current window size (or `780 × 676` if user resizes) | `Adw.NavigationSplitView` `show-sidebar: false` (`collapsed: true`) | Detail pane fills full width centered cleanly in `680px Adw.Clamp`, recreating Variant A's single-column rhythm | `Adw.OverlaySplitView` opens inline (`282px`) or overlay sheet when `width < 780px` |
| **Compact Breakpoint** | `Adw.Breakpoint` (`max-width: 720px`) | Adaptive width (`500–719px`) | `Adw.NavigationSplitView` auto-sets `collapsed: true`; HeaderBar reveals sidebar drawer toggle button | Full width with `12px` outer clamp padding | `Adw.OverlaySplitView` `collapsed: true` (slides over detail pane cleanly when row is clicked) |
| **Narrow Mobile Breakpoint** | `Adw.Breakpoint` (`max-width: 500px`) | Narrow (`<= 500px`, tested at `420px`) | `Adw.NavigationSplitView` `collapsed: true`; push/pop drawer via HeaderBar `☰` button | `Adw.WindowTitle` hides non-essential subtitle (`narrow: true`); search entry moves onto secondary bar or filter row; bottom actions wrap cleanly | `Adw.OverlaySplitView` `collapsed: true`, `show-sidebar: false` by default; tapping a row pushes/overlays the full-width Inspector sheet with explicit back/close button |

### 1.2 Standalone UI State Persistence (No GSettings Schema)

Because `./bin/fedora-updater` is distributed as a portable single multi-call binary (copyable standalone without installing system GSettings schemas), **the app must never call `gio::Settings` without a compiled schema**.

* **Module:** `src/ui/prefs.rs`
* **Path:** `$XDG_CONFIG_HOME/fedora-updater/ui.json` (fallback `$HOME/.config/fedora-updater/ui.json`)
* **Schema (`UiPrefs` via `serde` + `serde_json`):**
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct UiPrefs {
      pub sidebar_open: bool,   // default: true
      pub window_width: i32,    // default: 960
      pub window_height: i32,   // default: 676
  }
  ```
* **Failure Safety:** Missing, unreadable, or corrupt JSON files silently fall back to `UiPrefs::default()`. Saves occur debounced on resize/toggle and on `close_request`, and never block shutdown. When `FEDORA_UPDATER_PREVIEW` or `FEDORA_UPDATER_SCREENSHOT` is set, persistence is bypassed so preview runs remain deterministic.

### 1.3 Keyboard Navigation & Accessibility

* **`F9` (or `Ctrl+\`):** Toggle `sidebar_open` between Workbench and Focused "Lumen" modes.
* **`Ctrl+F`:** Focus the package search entry (when on `Ready` or `Activity Log`).
* **`Ctrl+R` / `F5`:** Trigger Refresh when on `Ready` or `Idle(Current/Warning)`.
* **`Escape`:** Dismiss open inspector pane (when `collapsed`), `Adw.Dialog` / `Adw.AlertDialog`, or bottom sheet console drawer.
* **`Enter` on Fresh Idle:** `default_widget` remains armed on `Check for Updates` exclusively on `IdleScreen::Fresh`.
* **Accessible Roles & Labels:** Every icon-only button (`sidebar_toggle`, `header_refresh`, `header_menu`, inspector close) carries explicit `tooltip-text` and `Gtk.Accessible` labels. Status is never communicated by color alone (every status pill pairs color with explicit text or icon).

---

## 2. GTK 4 / libadwaita Blueprint & Rust UI Architecture

### 2.1 Crate Compatibility (`Cargo.toml`)

Verified against `Cargo.toml` (`gtk = "0.10"` `v4_14`, `libadwaita = "0.8"` `v1_7`) and Fedora 44 runtime (`gtk4 4.22.5`, `libadwaita 1.9.4`):

| Widget / API | Introduced In | Status in `Cargo.toml` (`v1_7`) | Role in Observatory |
|---|---|---|---|
| `Adw.NavigationSplitView` / `Adw.NavigationPage` | libadwaita 1.4 | Available | Primary adaptive two-pane shell + dual-mode collapse |
| `Adw.OverlaySplitView` | libadwaita 1.4 | Available | Slide-out / overlay Package Inspector pane |
| `Adw.SplitButton` | libadwaita 1.0 | Available | Phase 2 multi-mode Apply action (`Live`, `Restart`, `Offline`, `Download`) |
| `Adw.ToggleGroup` | libadwaita 1.7 | Available | Activity Log source toggles (`All \| DNF \| FP \| FW`) & compact filter switches |
| `Adw.BottomSheet` | libadwaita 1.6 | Available | Live expandable transaction console drawer during `Checking` / `Running` |
| `Adw.WrapBox` | libadwaita 1.7 | Available | Responsive badge/tag clusters and narrow-safe action strips |
| `Adw.Dialog` / `Adw.AlertDialog` | libadwaita 1.5 | Available | Fedora upgrade guide, About dialog, Stale >24h & Stop Apply prompts |
| `Adw.Spinner` | libadwaita 1.6 | Available | Fixed-slot active indicator (`Gtk.Stack` child) |
| `Adw.SpringAnimation` | libadwaita 1.0 | Available | Smooth interpolation of `ProgressRing` arc angle |
| `Gtk.ListView` + `Gtk.SignalListItemFactory` | GTK 4.0 | Available | Virtualized O(1) visible-row package list replacing `rows::refill_group` |
| `Gtk.DrawingArea` + Cairo | GTK 4.0 | Available | `ProgressRing` (48px) and `ThroughputSparkline` (Phase 2) |

**No `Cargo.toml` version bump is required for Phase 1.** Phase 2 adds `zbus` (and optionally `zvariant`) for pure-Rust async D-Bus communication.

### 2.2 Target `src/ui/` Module Decomposition

```text
src/ui/
  mod.rs             Shared builder helpers & module wiring
  window.blp         Declarative Adw.ApplicationWindow -> Adw.ToastOverlay -> Adw.NavigationSplitView
  window.rs          State binding, WorkerMsg loop, timers, StyleManager .is-dark sync, Widgets::for_test() / widgets_from_builder / paint_quiet_chrome_with test seams
  sidebar.rs         System Pulse card, Sources nav list (counts + active filter), Views (Updates/Log/History),
                     Running Pipeline Telemetry sidebar, bottom Fedora Release Upgrade card
  queue_list.rs      PackageObject (glib::Object subclass) + gio::ListStore + Gtk.CustomFilter + Gtk.FilterListModel + Gtk.ListView
  inspector.rs       Adw.OverlaySplitView Package Inspector renderer (Phase 1 provenance + Phase 2 CVEs/changelogs)
  charts.rs          Cairo Gtk.DrawingArea widgets: ProgressRing (Adw.SpringAnimation) + ThroughputSparkline
  activity_log.rs    Embedded filterable Activity Log page (All/DNF/FP/FW + search + Copy All) over ConsoleBuffer
  history.rs         Update History timeline page (Phase 2 dnf5daemon History + fwupd GetHistory; hidden in Phase 1)
  dialogs.rs         Adw.AboutDialog, Adw.AlertDialog (Stale >24h, Stop Apply), Fedora Upgrade two-command Adw.Dialog
  prefs.rs           Standalone XDG JSON persistence (sidebar_open, window dimensions)
  preview.rs         FEDORA_UPDATER_PREVIEW=<state> presets including dual-mode & recovery states
```

*Note on `src/ui/log_window.blp`:* Keep `win.view-log` action intact! In Observatory, activating `win.view-log` navigates directly to the embedded `Activity Log` view inside the detail pane (and closes any modal banner/dialog), eliminating the redundant modal log popup while remaining 100% compatible with existing menu/banner triggers. Remove `log_window.blp` once all callers route through `activity_log.rs` (updating `build.rs` template list accordingly).

### 2.3 Virtualized `Gtk.ListView` Contract (`src/ui/queue_list.rs`)

Replace `rows::refill_group` (which currently destroys and recreates every `Adw.ActionRow` on every `BackendCheckDone` or `ApplyProgress`) with a virtualized list:

1. **`PackageObject` (`glib::Object`):** Wraps `Package` properties needed by row factories (`id`, `name`, `source`, `old_version`, `version`, `repo`, `size_bytes`, `status`, `is_current`, `item_progress`, `needs_reboot`, `advisory_summary`, `selected`).
2. **In-Place Diffing:** On `Phase::Checking`, `Phase::Ready`, `Phase::Running`, and `Phase::Done`, diff `gio::ListStore` items in place by `Package::id` rather than clearing/re-adding all items on every `100ms` progress tick.
3. **Filtering:** `Gtk.FilterListModel` driven by `Gtk.CustomFilter` combining `SidebarSourceFilter` (`All | Security | Dnf | Flatpak | Firmware`) and `Gtk.SearchEntry` text (`name`, `repo`, `version`, `advisory`).
4. **Row Layout (Single Clean Source Signal):**
   * `[CheckButton]` *(hidden in Phase 1; visible in Phase 2)*
   * `[Source Badge]` (`DNF` / `FP` / `FW` — removed trailing duplicate badge!)
   * `[Main Column]`
     * Title row: `name` + micro-tags (`Security` / `CVE-...`, `Restart`, `+ <extension>`)
     * Subtitle row (`.dimmed`, `.ver-diff`): `old → new` *(or `version · repo` when `old_version` is absent)* with `.numeric` alignment
   * `[Trailing Column]` (right-aligned): download size (`2.1 MB`) on `Ready`/`Checking`/`Done`, or live status (`Installing · 72%`, `Queued`, `Done`, `Failed`) on `Running`.
5. **Running Queue + Completed Expander Placement:** Completed packages remain **inside** the single detail `Gtk.ScrolledWindow` (under an `Adw.ExpanderRow` below the active/queued `Gtk.ListView`), completely preventing the expanded list from crushing active cards or resizing the window.

---

## 3. Visual Design System, CSS Tokens & Accessibility Contract

### 3.1 Resolving All 11 Forensic Defects

| Defect from Previous Refresh | Observatory Spec Fix |
|---|---|
| 1. Unpadded `.card` slamming widgets against border | All `.card` containers wrap an explicit inner `Gtk.Box` with `margin-top/bottom: 14–18` and `margin-start/end: 16–20` (or CSS `padding: 16px 18px` on custom card wrappers). |
| 2. Overhanging `.progress-block` vs `700px` `Adw.Clamp` | Progress bar, hero card, filter bar, `Gtk.ListView`, and bottom dock content share one `Adw.Clamp (maximum-size: 680)` spine. |
| 3. Orphan `"Nothing is applied..."` caption on far-left edge | Placed inside the clamped bottom bar / aligned detail column; never orphaned against the window edge. |
| 4. Ghost `18×18` gap from `now_playing_spinner.set_opacity(0.0)` | Fixed `22×22` `Gtk.Stack` (`transition-type: crossfade`, `150ms`) swapping `Adw.Spinner` ↔ status symbolic icon / empty placeholder without shifting layout. |
| 5. Triple source redundancy (`[DNF]` + `· System` + trailing `[DNF]`) | Trailing column shows **only** size (when known) or status—never repeats the source badge. Subtitle omits redundant `· System` / `· Apps` suffix when badge is visible. |
| 6. `@purple_1` (`#dc8add`) Flatpak badge failing Light Mode contrast | Dynamic theme-scoped CSS class `.is-dark` driven by `Adw.StyleManager` (see §3.2). |
| 7. `run_completed_group` outside scroll area | Moved **inside** the main `Gtk.ScrolledWindow` alongside `Gtk.ListView`. Delete `restore_default_window_size` workaround. |
| 8. O(N) `refill_group` widget teardown | Virtualized `Gtk.ListView` + `gio::ListStore` in-place property updates. |
| 9. Persistent `upgrade_notice` fighting active HeaderBar | Moved cleanly to the **Sidebar Footer Card** (and tucked neatly inside `Idle`/`Ready`/`Done` content clamp when sidebar is hidden in Lumen mode)—**never shown during `Authenticating` or `Running`**. |
| 10. Abrupt `Done` phase stuffing telemetry into clipping subtitle | Clean header title (`All Updates Applied`) + explicit `Restart Recommended` card naming triggering packages (`kernel`, `UEFI dbx`) + clean `Installed` list + closure actions. |
| 11. Flat `Adw.ToolbarView` lacking top/bottom bar separation | `top-bar-style: flat` on centered `Adw.StatusPage` views (`Idle`, `Auth`, `Failed`); `top-bar-style: raised` and `bottom-bar-style: raised` on scrolled list/running/done views. |

### 3.2 GTK4 Dark / Light / High-Contrast Theme Binding

GTK4 CSS does **not** evaluate web `@media (prefers-color-scheme: dark)`. `src/ui/window.rs` **must** bind `adw::StyleManager::default()` on startup and on `notify::dark`:

```rust
let style_mgr = libadwaita::StyleManager::default();
let sync_dark = move |mgr: &libadwaita::StyleManager, win: &libadwaita::ApplicationWindow| {
    if mgr.is_dark() {
        win.add_css_class("is-dark");
    } else {
        win.remove_css_class("is-dark");
    }
};
```

**WCAG-Safe Token Table (`src/style.css`):**

| Token / Role | Adwaita Light (default) | Adwaita Dark (`window.is-dark`) | Notes |
|---|---|---|---|
| DNF Source Badge (`src-dnf`) | fg `#1c71d8` (`@blue_4`), bg `alpha(@accent_bg_color, 0.12)` | fg `#78aeed` (`@blue_2`), bg `alpha(@accent_bg_color, 0.16)` | 4.5:1+ contrast verified |
| Flatpak Badge (`src-flatpak`) | fg `#813d9c` (`@purple_4`), bg `alpha(#813d9c, 0.12)` | fg `#dc8add` (`@purple_1`), bg `alpha(#dc8add, 0.16)` | Replaces unreadable Light `#dc8add` |
| Firmware Badge (`src-firmware`) | fg `#9e5a00`, bg `alpha(@warning_bg_color, 0.16)` | fg `#f8c555`, bg `alpha(@warning_bg_color, 0.16)` | Warm amber with legible contrast |
| Security Micro-Tag (`.tag-sec`) | fg `#c01c28`, bg `alpha(@error_bg_color, 0.11)` | fg `#ff7b72`, bg `alpha(@error_bg_color, 0.16)` | Paired with explicit `"Security"` / CVE text |
| Restart Micro-Tag (`.tag-reboot`) | fg `#9e5a00`, bg `alpha(@warning_bg_color, 0.15)` | fg `#f8c555`, bg `alpha(@warning_bg_color, 0.16)` | Paired with `"Restart"` text |
| Pending / Done Row Dimming | Use native `.dimmed` label class | Use native `.dimmed` label class | Respects High-Contrast `--dim-opacity` |
| Active Row Highlight | `alpha(@accent_bg_color, 0.10)` | `alpha(@accent_bg_color, 0.14)` | Applied cleanly inside `.boxed-list` |
| Sidebar Surface | `@headerbar_bg_color` / `#ebebee` via `Adw.NavigationSplitView` native sidebar pane | Native `Adw.NavigationSplitView` dark sidebar surface | Zero hardcoded hex backgrounds on panes |

### 3.3 Custom Cairo/GSK Graphics (`src/ui/charts.rs`)

1. **`ProgressRing` (`48 × 48` `Gtk.DrawingArea`):**
   * Track arc: `alpha(@window_fg_color, 0.12)`, `3.5px` stroke width.
   * Fill arc: `@accent_bg_color` (transitions to `@success_bg_color` at completion), rounded line cap, starting at `-PI / 2`.
   * Center label: `.numeric` percentage (`11px`, bold) overlaid in `Gtk.Overlay` so GTK handles font scaling & RTL/accessibility natively.
   * Driven by `Adw.SpringAnimation` (`damping: 1.0`, `stiffness: 120.0`) on fraction updates.
2. **`ThroughputSparkline` (`200 × 44` `Gtk.DrawingArea` — Phase 2 active):**
   * Renders up to 60 rolling 1-second samples of download throughput (`bytes_per_sec`).
   * Draws smooth `2px` polyline in `@accent_color` with a vertical linear gradient fill (`alpha(@accent_bg_color, 0.28)` → `0.0`).
   * Peak and transferred totals sit in a single-line `.numeric` caption underneath (`white-space: nowrap` / `ellipsize: none` to prevent label wrapping).

---

## 4. State Machine, View-Model & Complete Invariant Preservation

### 4.1 Baseline State Inventory (Verified on `49899d9`)

Every existing `Phase`, `IdleScreen`, and `RetryAction` must map cleanly into Observatory:

| State / Variant | Observatory Shell Behavior | Primary & Recovery Actions |
|---|---|---|
| `Phase::Idle(IdleScreen::Fresh)` | `StatusPage`: `"Check for Updates"`, suggested pill CTA (`default_widget`), Sidebar shows System Pulse + History (Phase 2) + Fedora Release card | `Check for Updates` (`StartCheck { then_apply: false }`) |
| `Phase::Idle(IdleScreen::Current { checked_at, detail })` | `StatusPage`: `"System is up to date"`, `object-select-symbolic` emblem, description shows `detail` or `Last checked {checked_at}` | Flat `Check Again` |
| `Phase::Idle(IdleScreen::Warning { title, detail, checked_at })` | `StatusPage`: `dialog-warning-symbolic`, `title` (`"Could not check all sources"` / `"Some updates failed"`), `detail` in description, **never claims up to date** | Suggested pill `Check Again` + flat `View Log` (`idle_log` → Activity Log view) |
| `Phase::Authenticating { purpose, packages, last_checked }` | `StatusPage`: `"Authentication Required"`, HeaderBar subtitle `"Checking for updates"` or `"Applying updates"`, `Adw.Spinner` + `Waiting for authentication`. Fedora upgrade card hidden. | `Cancel` (`Event::CancelAuth` returns to `Ready` if `AuthPurpose::Apply && !packages.is_empty()`, else restores `Idle` preserving `last_checked` and increments `WORK_GEN`) |
| `Phase::Checking { packages_so_far, soft_errors, checked_sources }` | Header `"Checking for Updates"`, aligned `Gtk.ProgressBar` (`0.05` floor), 3-source sidebar/card checklist (`System`, `Apps`, `Firmware`), streaming `Gtk.ListView`, `Adw.BottomSheet` live console | `Cancel` (`cancel_background_work`) |
| `Phase::Ready { packages, soft_errors }` | Optional `Adw.Banner` (`soft_errors`), Sidebar source filters + count pills, Search, `Gtk.ListView`, `Adw.OverlaySplitView` Inspector, bottom bar `"Nothing changes until you apply"` + `Update All` | `Refresh`, `Update All` (checks `update_list_needs_refresh(now)` > 24h → `Adw.AlertDialog`), `View Log` on banner |
| `Phase::Running(RunningState)` | `sidebar` switches to Pipeline Telemetry, 4-stage breadcrumb (`Download → Prepare → Install → Verify`), padded active card (`ProgressRing` + fixed-slot `Gtk.Stack`), `quiet_ui` stall `Adw.Banner`, in-scroll `Gtk.ListView` + completed `Adw.ExpanderRow`, `Adw.BottomSheet` console | `Stop` (plain, immediate safe abort during `RunPass::Download` → returns to `Ready` with banner) OR `Stop…` (destructive + `Adw.AlertDialog` during `RunPass::Apply` → `Phase::Failed`) |
| `Phase::Done { packages, duration, needs_reboot, failed, failure_reasons }` | **Three sub-cases:** (1) Clean (`!needs_reboot && failed == 0`) → `Done` is sole `suggested-action`; (2) Reboot (`needs_reboot`) → `Restart Now` `Adw.Banner`/card is sole `suggested-action` naming triggering packages; (3) Partial failure (`failed > 0`) → `done_problem_banner` (`View Log`) + `done_retry_btn` (`Check Again`, suggested if `!needs_reboot`) + failed rows sorted first | `Restart Now`, `Check Again` (if `failed > 0`), `View Full Log` (→ Activity Log), `Done` (`ResetToIdle`) |
| `Phase::Failed { title, detail, retry }` | `StatusPage`: `dialog-error-symbolic`, `title` + `detail` | Primary pill driven by `fail_retry_label(retry)`: `CheckAgain` → `"Check Again"`, `RetryApply` → `"Retry Update"` (resuming preserved `retry_apply_packages`), `Reboot` → `"Restart"`; plus flat `View Log` |

### 4.2 Core Invariants That Must Not Regress

1. **24h Staleness Policy (`UPDATE_LIST_MAX_AGE`):** `StartApply` checks `update_list_needs_refresh(SystemTime::now())`; stale lists prompt `Adw.AlertDialog` (`"Refresh Update List?"`), setting `apply_when_ready = true` and auto-applying if the refreshed list is non-empty.
2. **`quiet_ui` & Stall Contention:** Preserves 5s (`at_full_item` spinner, `/var/cache/libdnf5` RPM growth polling), 15s (hide unreliable ETA), and 120s (`"No output for N minutes"` + `dnf_daemon_is_running()` check for `dnf5daemon-server` → `"Waiting on GNOME Software."`) behavior in Phase 1.
3. **Two-Tier Stop & Subprocess Termination:** Stopping kills **both** `kill_recorded_helper()` and `kill_recorded_local_command()` immediately and increments `WORK_GEN`.
4. **Multi-Arch RPM Twins & Token-Exact Matching:** `i686` and `x86_64` twins track separately without `kernel` hijacking `kernel-core`; unlisted transitive dependencies update `current_name` safely.
5. **Flatpak Related Extensions:** Shared extension downloads (e.g. `org.freedesktop.Platform.GL.nvidia-*`) remain attached to parent rows with download bytes counted once.
6. **`ConsoleBuffer` (`8,000` lines):** Preserves `\r` live redraw (`ConsoleDelta::ReplacedLast`) separate from committed command logs (`ConsoleDelta::Appended`).
7. **Reboot Allowlist & `fwupdmgr` Contract:** Keeps `package_implies_reboot` core list (excluding `kernel-tools` / `kernel-devel`) and `fwupdmgr check-reboot-needed --json` exit `0` detection.
8. **Fedora Release Upgrade Advisor:** Keeps `COLLECTIONS_URL` check, `days_since_release` badge, and two **separate non-chained** copyable commands (`sudo dnf system-upgrade download --releasever=N`, `sudo dnf system-upgrade reboot`).
9. **Non-Blocking Teardown & Single Instance:** `close_request` drops privileges immediately without blocking on `QUIT`/`__BYE__`; `dev.fedora.Updater` remains single-instance except under preview/screenshot flags.

### 4.3 View-Model & State Extensions (`model.rs` / `state.rs` / `presenter.rs`)

Add cleanly without breaking existing `reduce()` contracts:

```rust
/// Sub-view displayed in Observatory's detail pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailView {
    #[default]
    Updates,
    ActivityLog,
    History, // Populated in Phase 2
}

/// Phase 2 execution mode selected via Adw.SplitButton.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApplyMode {
    #[default]
    Live,
    LiveAndReboot,
    OfflineOnReboot,
    DownloadOnly,
}

/// Structured advisory metadata (populated in Phase 2; optional in Phase 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageAdvisory {
    pub id: String,
    pub kind: AdvisoryKind,     // Security | Bugfix | Enhancement | NewPackage
    pub severity: Option<String>,
    pub cves: Vec<String>,
    pub bugzillas: Vec<String>,
    pub description: Option<String>,
}
```

And on `Package` (defaulting cleanly to `None` / `Vec::new()` in Phase 1):
* `pub summary: Option<String>`
* `pub install_size_delta: Option<i64>`
* `pub advisories: Vec<PackageAdvisory>`

---

## 5. Phase 1 (Tier 1) — Observatory UI Rebuild Specification

### 5.1 Scope & Boundaries
Phase 1 transforms the presentation and widget layer (`src/ui/*`, `src/style.css`, and pure view helpers in `src/presenter.rs` / `src/model.rs`) while **leaving `src/orchestrator.rs`, `src/helper_mode.rs`, `src/helper_protocol.rs`, `src/privilege/*`, and `src/backend/*` execution behavior unchanged**.

### 5.2 Active Features vs. Cleanly Deferred Phase 2 Surface (Zero Dead Controls)

To avoid shipping inert checkboxes or placeholder tabs in Phase 1:

* **Shipped & Fully Active in Phase 1:**
  * `Adw.NavigationSplitView` dual-mode shell (expanded Workbench + collapsed `680px` Focused "Lumen" mode + `ui.json` persistence).
  * Sidebar System Pulse card, live Source Filters (`All`, `System`, `Apps`, `Firmware`) with real counts, `Activity Log` view entry, and bottom Fedora Release Upgrade card.
  * Virtualized `Gtk.ListView` + `Gtk.FilterListModel` + `Gtk.SearchEntry`.
  * `Adw.OverlaySplitView` Package Inspector showing source badge, repo, arch, `old_version` / `version`, download size, `package_implies_reboot` warning tag, and Flatpak related-extension breakdown.
  * Embedded filterable `Activity Log` (`All | DNF | Flatpak | fwupd` + search + Copy All) wired to `ConsoleBuffer` deltas.
  * `Running` telemetry deck: `48px ProgressRing` (`Adw.SpringAnimation`), 4-stage breadcrumb, padded active card with `22×22` fixed-slot `Gtk.Stack`, in-scroll remaining/completed `Adw.ExpanderRow`, `Adw.BottomSheet` live console drawer, and sidebar live source checklist + elapsed/ETA.
  * Restored `Done` closure + full `c130c3e` recovery UX (`IdleScreen::Warning`, `RetryAction::RetryApply`, `done_problem_banner`, `done_retry_btn`, `CancelAuth` → `Ready`).
  * Dark/Light/High-Contrast CSS tokens (`StyleManager::notify::dark` → `.is-dark`).
  * Updated `src/ui/preview.rs` supporting all Observatory states and dual-mode capture.

* **Hidden (`visible: false`) Until Phase 2 Lands:**
  * Per-row selective update `Gtk.CheckButton` & `"Toggle All Selected"` strip (Phase 1 keeps plain `Update All` CTA).
  * `Adw.SplitButton` dropdown options (`Update & Restart`, `Prepare Offline Update`, `Download Only`) — Phase 1 renders standard `Gtk.Button` `"Update All"`.
  * `Security Fixes (N)` sidebar filter pill & CVE badges (shown only once `advisories` are populated).
  * `Update History` sidebar navigation row.
  * `ThroughputSparkline` inside Running sidebar (Phase 1 shows elapsed time, stage count, and `quiet_ui` status in that sidebar slot).

---

## 6. Phase 2 (Tier 2) — Native Fedora 44 Platform Engine Specification

### 6.1 Host Capability Baseline (Verified on Fedora 44 Workstation)

| Service / Library | Host Version | Polkit Rules Observed | Key Benefit Over CLI Scraping |
|---|---|---|---|
| `org.rpm.dnf.v0` (`dnf5daemon-server`) | `5.4.8.0` | `execute_trusted_transaction` = `auth_admin_keep`; **read/list/advisory/resolve/history = no prompt** *(avoid `base.Config.override` which sets `auth_admin`)* | Zero-auth Check; eliminates GNOME Software lock contention; real `old → new`; CVEs; MB/s telemetry; `Goal.cancel()`; `History.list()`; `Offline` |
| `libflatpak.so.0` | `0.11804.0` | `org.freedesktop.Flatpak.{app-update,runtime-update,appstream-update}` = `yes` | `FlatpakTransactionProgress` (`progress`, `bytes_transferred`, `status`); `GCancellable`; AppStream icons & changelogs |
| `org.freedesktop.fwupd` (`fwupd`) | `2.1.9` | Signed LVFS internal/hotplug updates & refresh = `yes` | `PercentageFull`, `Status`, `GetHistory`, release changelogs, **`BatteryLevel` vs `BatteryThreshold`** pre-flight guard |
| `org.freedesktop.login1.Manager` | systemd | `Reboot(true)` interactive polkit-aware | `CanReboot()`, `ListInhibitors()` pre-flight inspection naming active blockers |

### 6.2 Transport Choice: `zbus` + Extended `libflatpak` FFI

* Add `zbus` (blocking or async `zbus::blocking::Connection` / `tokio`-free `async-io` matching GLib worker threads cleanly) to `Cargo.toml` in Phase 2.
* Keep all D-Bus calls behind cleanly factored backend traits / `OrchestratorRuntime` implementations so offline unit and pipeline tests mock D-Bus responses cleanly from `tests/fixtures/` without touching the system bus.
* **Graceful CLI Fallback:** On startup, probe `org.rpm.dnf.v0` / `org.freedesktop.fwupd` availability on the system bus. If a daemon is unavailable (e.g., containerized test environment or minimal spin without `dnf5daemon-server`), transparently route through the Phase 1 `PrivilegedSession` CLI helper and keep Phase-2-only controls hidden.

### 6.3 Backend Engine Contracts

1. **`fwupd` Engine (`src/backend/fwupd_dbus.rs`):**
   * Query `org.freedesktop.fwupd` (`GetDevices`, `GetUpgrades`, `GetReleases`, `GetHistory`).
   * **Battery Safety Pre-Flight:** Compare device/host `BatteryLevel` against `BatteryThreshold` (and AC status) before flashing; surface actionable warning inside Observatory Inspector & `Done` partial failure recovery if unmet.
   * Download/refresh LVFS metadata & pass file descriptors via `UpdateMetadata` / `Install` (or `libfwupd.so.3` FFI `FwupdClient`), tracking `PercentageFull` + `Status` + `DeviceRequest` signals.
2. **Reboot & Notifications (`src/backend/logind.rs`):**
   * Call `org.freedesktop.login1.Manager.CanReboot()` and `ListInhibitors()` before displaying/invoking Reboot, surfacing active blockers clearly instead of failing blindly.
   * Execute `Reboot(true)` on user confirmation.
   * Send `gio::Notification` (`"Updates Ready to Review"` / `"Updates Installed — Restart Recommended"`) via `Application::send_notification` when `window.is_active() == false`.
3. **Flatpak Transaction Engine (`src/backend/flatpak.rs`):**
   * Extend existing `dlopen("libflatpak.so.0")` table to instantiate `FlatpakTransaction`, register `new-operation`, `operation-progress` (`FlatpakTransactionProgress`), `operation-done`, `operation-error`, and execute `flatpak_transaction_run` with a live `GCancellable`.
   * Extract AppStream summary, version, and extension relationships directly without spawning `flatpak update` CLI subprocesses.
4. **`dnf5daemon-server` Engine (`src/backend/dnf_dbus.rs`):**
   * **Check Path (Zero Password):**
     1. `SessionManager.open_session({})` *(never pass `config` overrides so polkit does not fire `base.Config.override`)*.
     2. `Base.read_all_repos()`.
     3. `rpm.Rpm.list` / `list_fd` (`scope: "upgrades"` + `scope: "installed"` lookup to populate exact `old_version` + `download_size` + `install_size` + `summary`!).
     4. `Advisory.list({ "availability": "updates", "advisory_attrs": [...] })` → attach `PackageAdvisory` (`CVE`, `RHBZ`, `severity`, `description`) onto matching `Package` entries, lighting up `Security Fixes` filters and Inspector CVE tags.
   * **Apply Path (Single `auth_admin_keep` Prompt & Selective/Offline Modes):**
     1. `rpm.Rpm.upgrade(selected_specs, {})` (supports selective subset when user unchecks rows!).
     2. `Goal.resolve({ "allow_erasing": false })` → inspect problems (`get_transaction_problems_string`).
     3. `Goal.do_transaction({ "offline": mode == ApplyMode::OfflineOnReboot, "downloadonly": mode == ApplyMode::DownloadOnly, "interactive": true })` (calls `Offline.schedule_for_next_boot` + `set_finish_action` when offline mode selected).
     4. Subscribe to `Base.download_add_new` / `download_progress` (feeding real MB/s samples into `ThroughputSparkline`!) / `download_end` and `Rpm.transaction_before_begin` / `transaction_elem_progress` / `transaction_action_start/progress/stop` / `transaction_verify_*` / `transaction_script_*` / `transaction_after_complete` (mapping accurately to the 4-stage breadcrumb `Download → Prepare → Install → Verify`).
     5. Safe Cancel: `Goal.cancel()` aborts downloads cleanly without corrupting RPM DB; once `transaction_before_begin` fires, UI transitions `Stop` → destructive `Stop…` (cancellation refused by daemon once RPM transaction starts).
   * **History Path (Zero Password):**
     * `org.rpm.dnf.v0.History.list({ "limit": 50 })` merged with `fwupd.GetHistory()` populates the `Update History` sidebar view.

---

## 7. Testing, Verification & Post-`#19` Harmonization

Both phases must preserve 100% offline determinism (`cargo test` `< 5s`, zero network/live-host requirements) and integrate directly with the `#19` hardening infrastructure:

* **Unit Layer:** Pure tests for `prefs.rs` (missing/corrupt/round-trip JSON), `PackageObject` filter predicates, `charts.rs` geometry & spring clamps, `presenter.rs` recovery/copy functions, and Phase 2 D-Bus/advisory deserializers against `tests/fixtures/`.
* **Functional Layer (`OrchestratorRuntime` from `#23`):** Phase 2 DNF5/fwupd/Flatpak engines plug into the runtime abstraction introduced by `#23`, verifying zero-auth checks, selective subsets, offline scheduling, `Goal.cancel()`, battery threshold rejection, and CLI fallback against offline fixtures.
* **Widget & E2E Layer (`#24`):** Headless GTK4/libadwaita tests (using `widgets_from_builder` / `gtk4-broadwayd`) verify dual-mode `Adw.NavigationSplitView` collapse (`<720px` and `<=500px`), virtualized `Gtk.ListView` updates across `Checking` → `Ready` → `Running` → `Done`, `Adw.OverlaySplitView` Inspector rendering, `Activity Log` filtering over `ConsoleBuffer`, and all recovery banners/dialogs (`IdleScreen::Warning`, `RetryApply`, `done_problem_banner`, `done_retry_btn`, `CancelAuth` → `Ready`, Stale >24h, Stop Apply).
* **Preview Regression:** `./scripts` / `FEDORA_UPDATER_PREVIEW` captures updated Observatory screenshots (`ready`, `ready-lumen`, `running`, `done`, `narrow`) to visually confirm alignment and Dark/Light contrast.

### Verified Post-`#19` Seams & Baseline (`49899d9`)
With `#19` (`#20`–`#25`, plus `#26`–`#27`) closed on `main` @ `49899d9`:
1. **`src/orchestrator.rs` Runtime Seam (`#23`):** `OrchestratorRuntime` defines `ensure_session`, `run_helper`, `run_local`, `run_cancellable_local`, `query_flatpak`, and `now` (implemented for live execution by `SystemRuntime`), alongside `WorkCancel`, `run_check_all_with`/`execute_check_all`, `run_apply_all_with`/`execute_apply_all`, and `run_systemctl_reboot_with`/`execute_systemctl_reboot`. Phase 2 issues plug directly into this boundary.
2. **Headless GTK4 UI E2E Harness (`#24`):** `scripts/test-ui-headless.sh` launches an ephemeral `gtk4-broadwayd` daemon in a temporary `XDG_RUNTIME_DIR` and runs `GDK_BACKEND=broadway FEDORA_UPDATER_REQUIRE_GTK=1 cargo test --bin fedora-updater -- --test-threads=1` without requiring Wayland/X11 or popping desktop windows. `src/ui/window.rs` exposes `Widgets::for_test()`, `widgets_from_builder`, and `paint_quiet_chrome_with`, verifying `dialogs::build_confirm_dialog` / `build_upgrade_dialog` / `command_row` and all recovery invariants:
   - Ready after pre-install `Stop` surfaces exact `"Stopped before install. Downloaded packages were not applied."` banner (not `"1 source could not be checked"`).
   - Ready with an empty package list disables `Update All`.
   - `run_completed_user_open` preserves user-expanded completed drawers across subsequent `Running` renders.
   - `Done` with `failed > 0` surfaces `done_problem_banner` (`View Log`) + `done_retry_btn` (`Check Again`, `suggested-action` iff `!needs_reboot`) and sorts `Failed` items before `Completed`.
   - `IdleScreen::Warning` shows `dialog-warning-symbolic`, suggested `"Check Again"`, and `idle_log` (`"View Log"`), never claiming `"System is up to date"`.
   - `RetryAction::{CheckAgain, RetryApply, Reboot}` map to `"Check Again"`, `"Retry Update"`, and `"Restart"`, and `CancelAuth` during `AuthPurpose::Apply` returns to `Phase::Ready` when packages remain.
3. **Baseline Health Verified on `49899d9`:** `cargo test --quiet` (`229 passed, 0 failed, 1 ignored` live-host test), `./scripts/test-ui-headless.sh --quiet` (`6` GTK4/Libadwaita UI E2E tests passed), and `cargo clippy --all-targets --quiet` (`0 warnings`).

---

## 8. Two-Phase GitHub Program & Child Issue Tracker

Execute strictly in sequential order (`#29` → `#30` → `#31` → `#33` → `#34` → `#35`). **Start with [#29](https://github.com/rkalla/fedora-updater/issues/29) as the first `/goal` kickoff target** once `docs/UX-REDESIGN-OBSERVATORY-SPEC.md` and `docs/mockups/neon-*` are committed on `main`.

### [Program 1 — #28](https://github.com/rkalla/fedora-updater/issues/28): Observatory Dual-Mode UI Rebuild (Phase 1 / Tier 1)
1. **[#29 — Observatory 1.1: Dual-Mode Shell, WCAG CSS Tokens, XDG UI Prefs & Cairo ProgressRing](https://github.com/rkalla/fedora-updater/issues/29)**
   * Implement `Adw.NavigationSplitView` dual-mode shell (`window.blp`, `sidebar.rs`), `$XDG_CONFIG_HOME/fedora-updater/ui.json` persistence (`prefs.rs`), `StyleManager` `.is-dark` CSS binding & WCAG-safe `style.css` tokens, 48px Cairo `ProgressRing` (`charts.rs`), and responsive `<720px` / `<=500px` breakpoints.
2. **[#30 — Observatory 1.2: Virtualized Queue, Package Inspector & Embedded Activity Log](https://github.com/rkalla/fedora-updater/issues/30)** *(after `#29`)*
   * Implement `queue_list.rs` (`PackageObject`, `gio::ListStore`, `Gtk.FilterListModel`, `Gtk.ListView`), sidebar live source filters + counts + search, `Adw.OverlaySplitView` Package Inspector (`inspector.rs`), and embedded filterable `Activity Log` (`activity_log.rs`) wired to `win.view-log`.
3. **[#31 — Observatory 1.3: Telemetry Running Deck, Closure & Recovery UX Polish + Headless E2E](https://github.com/rkalla/fedora-updater/issues/31)** *(after `#30`)*
   * Complete `Running` (4-stage breadcrumb, `22×22` fixed-slot `Gtk.Stack`, in-scroll completed `Adw.ExpanderRow`, `Adw.BottomSheet` console, sidebar telemetry), restored `Done` closure + all `c130c3e` / `#24` recovery flows, remove `log_window.blp` & `restore_default_window_size`, update `src/ui/preview.rs`, and extend headless GTK4 E2E tests. Closes `#28` and unblocks `#32`.

### [Program 2 — #32](https://github.com/rkalla/fedora-updater/issues/32): Native Fedora 44 Platform Engine (Phase 2 / Tier 2)
*(Blocked until Program 1 `#28` is closed)*
4. **[#33 — Native Platform 2.1: `fwupd` D-Bus, `login1` Reboot Guard & Completion Notifications](https://github.com/rkalla/fedora-updater/issues/33)** *(after `#28`)*
   * Add `zbus`; implement `org.freedesktop.fwupd` check/apply/history + `BatteryLevel` vs `BatteryThreshold` safety check; implement `org.freedesktop.login1.Manager` `CanReboot` / `ListInhibitors` / `Reboot(true)` and `gio::Notification` completion alerts; preserve CLI fallback.
5. **[#34 — Native Platform 2.2: `libflatpak.so.0` Transaction Apply & AppStream Metadata](https://github.com/rkalla/fedora-updater/issues/34)** *(after `#33`)*
   * Extend `src/backend/flatpak.rs` `dlopen` FFI with `FlatpakTransaction` execution, `FlatpakTransactionProgress` byte/percent telemetry, `GCancellable` stop support, and AppStream metadata for Inspector; integrate behind `OrchestratorRuntime` with fixture tests and CLI fallback.
6. **[#35 — Native Platform 2.3: `dnf5daemon-server` (`org.rpm.dnf.v0`) Engine + Selective, Offline & History UX](https://github.com/rkalla/fedora-updater/issues/35)** *(after `#34`)*
   * Implement `dnf_dbus.rs` (`SessionManager.open_session({})`, `Base`, `rpm.Rpm.list` / `list_fd`, `Advisory.list`, `Goal`, `History`, `Offline`) avoiding `base.Config.override` (`auth_admin`); light up Phase 2 Observatory UI controls (`CheckButton` selective updates, `Adw.SplitButton` Live/Restart/Offline/Download-Only modes, `ThroughputSparkline`, CVE badges, `Update History` timeline) with automatic CLI fallback when `dnf5daemon-server` is unavailable. Closes `#32`.
