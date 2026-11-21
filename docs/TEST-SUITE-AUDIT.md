# Fedora Updater — Test Suite Audit, Coverage Gap Matrix & UX Recovery Analysis

Parent program: [rkalla/fedora-updater#19](https://github.com/rkalla/fedora-updater/issues/19) · Audit scope: [rkalla/fedora-updater#20](https://github.com/rkalla/fedora-updater/issues/20)

---

## 0. Executive Summary & Baseline

As of #19 / #20 kickoff, the repository contains **152 tests total** (**151 offline** + **1 `#[ignore]`** live-host test):

| Target / File | Total | Offline | Ignored | Keep | Strengthen | Consolidate | Replace |
|---|---:|---:|---:|---:|---:|---:|---:|
| `src/backend/dnf.rs` | 18 | 18 | 0 | 14 | 4 | 0 | 0 |
| `src/backend/flatpak.rs` | 5 | 5 | 0 | 4 | 1 | 0 | 0 |
| `src/backend/fwupd.rs` | 7 | 7 | 0 | 5 | 2 | 0 | 0 |
| `src/console.rs` | 3 | 3 | 0 | 2 | 1 | 0 | 0 |
| `src/helper_mode.rs` | 7 | 7 | 0 | 5 | 1 | 1 | 0 |
| `src/helper_protocol.rs` | 7 | 7 | 0 | 3 | 4 | 0 | 0 |
| `src/model.rs` | 9 | 9 | 0 | 8 | 1 | 0 | 0 |
| `src/orchestrator.rs` | 6 | 6 | 0 | 6 | 0 | 0 | 0 |
| `src/presenter.rs` | 17 | 17 | 0 | 14 | 2 | 1 | 0 |
| `src/privilege/session.rs` | 3 | 3 | 0 | 2 | 0 | 1 | 0 |
| `src/release_upgrade.rs` | 12 | 12 | 0 | 10 | 2 | 0 | 0 |
| `src/state.rs` | 2 | 2 | 0 | 2 | 0 | 0 | 0 |
| `src/ui/dialogs.rs` | 1 | 1 | 0 | 1 | 0 | 0 | 0 |
| `src/ui/preview.rs` | 2 | 2 | 0 | 2 | 0 | 0 | 0 |
| `src/ui/window.rs` | 2 | 2 | 0 | 2 | 0 | 0 | 0 |
| `tests/helper_session.rs` | 4 | 3 | 1 | 4 | 0 | 0 | 0 |
| `tests/model_and_protocol.rs` | 6 | 6 | 0 | 4 | 2 | 0 | 0 |
| `tests/state_machine_exhaustive.rs` | 41 | 41 | 0 | 38 | 3 | 0 | 0 |
| **Total** | **152** | **151** | **1** | **126** | **23** | **3** | **0** |

### Key Structural Strengths Today
- **`tests/state_machine_exhaustive.rs` (41 tests):** Strong regression coverage over `state::reduce`, 24-hour `Ready` expiration, `apply_when_ready` lifetime, `StopApply` modes, download-vs-apply pass isolation, and token-exact progress hints.
- **`src/backend/dnf.rs` & `src/model.rs`:** Strong unit regression tests built around real-world bugs (`kernel` vs `kernel-core`, epochs in NEVRA, `\r` vs transaction preview headers, multi-arch `i686`/`x86_64`, download vs install status resets).
- **`src/helper_mode.rs` & `tests/helper_session.rs`:** Solid subprocess protocol coverage including leaked grandchild FD handling in `drain_child` and non-blocking `close_session` during a hung helper command.

### Key Blind Spots & Risk Areas
1. **No checked-in fixture corpus (`tests/fixtures/`):** All existing parser and state tests embed tiny inline strings; multi-repo check output, dependency conflicts, RPM locks, NVIDIA Flatpak extensions, multi-device fwupd JSON, and live `\r` transcripts are not shared across unit/functional/E2E tiers.
2. **Zero pipeline/functional orchestration coverage (`src/orchestrator.rs`):** `run_check_all`, `run_apply_all`, and `run_systemctl_reboot` directly call concrete `privilege::{with_session, run_local_command}`, `backend::flatpak::query_updates`, and `SystemTime::now()`, leaving the real glue between parsers, helper IPC, and `state::reduce` completely unexercised offline (`#23`).
3. **Zero live GTK4 / Libadwaita widget & signal coverage (`src/ui/{window,dialogs,rows}.rs`):** Existing UI tests only assert Blueprint-compiled XML substrings (`window.ui` / `log_window.ui`), `preview::state_for` cases, and `RefCell` panic discipline — `render()`, row styling, alert dialogs, View Log window behavior, and console drawer updates are never exercised against real widgets (`#24`).
4. **Zero unit tests for `privilege/pool.rs`, `flatpak::parse_progress_line`, `model::parse_size_bytes`, `fwupd::parse_text_updates`:** Auditing already exposed concrete parser bugs (`fwupd::parse_text_updates` turning `" • System Firmware"` under `"Devices with no available firmware updates:"` into a phantom update; `orchestrator.rs:234` treating fwupd exit 2 no-updates text as an error; `flatpak::parse_progress_line` failing on `app/<id>/...` tokens).
5. **Genuine Missing UX Recovery Features Filed (and Resolved Pre-#21):**
   - [rkalla/fedora-updater#26](https://github.com/rkalla/fedora-updater/issues/26) *(Resolved)*: Check failures with 0 packages now transition to `IdleScreen::Warning` (`dialog-warning-symbolic`, `"Could not check all sources"`) and expose a `View Log` button on `idle_page`.
   - [rkalla/fedora-updater#27](https://github.com/rkalla/fedora-updater/issues/27) *(Resolved)*: Apply failures summarize actionable backend errors (`summarize_apply_failure`), `Phase::Done { failed > 0 }` surfaces `done_problem_banner` + `done_retry_btn` and resets to `IdleScreen::Warning`, `stop_apply` / `release_privileges` kill cancellable local commands (`kill_recorded_local_command`), and Apply auth cancellation/denial preserves `Ready` packages / `RetryAction::RetryApply`.

---

## 1. Complete Inventory of All 152 Existing Tests

### 1.1 `src/backend/dnf.rs` (18 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `parse_classic_check_update` | Unit | Parses `name.arch version repo` lines, preserves epoch `1:...`, ignores `Last metadata` header | Strengthen | Exercise checked-in `tests/fixtures/dnf/check_update_classic.txt` + duplicate dedup + dnf5 table lines (#21) |
| `parse_check_update_parses_optional_size_bytes` | Unit | Parses optional size suffix (`98.2 MB`, `112 MiB`) in both classic and dnf5 table formats | Strengthen | Expand to `tests/fixtures/dnf/check_update_dnf5_table.txt` covering `Obsoleting`, `KiB`, and table headers (#21) |
| `parse_progress_fraction` | Unit | Extracts `[cur/total]` and stripped package name on `Installing`; keeps `progress == None` without `%` | Keep | Protects transaction step count vs item `%` distinction |
| `download_line_is_not_an_install_and_keeps_both_fractions` | Unit | dnf5 `[n/total] nevra pp%` without "downloading" maps to `PackageStatus::Downloading`, keeps both fractions | Keep | Prevents download lines from falsely triggering apply completion |
| `upgrade_line_keeps_transaction_count_apart_from_item_percent` | Unit | `[16/137] Upgrading nevra 100%` yields `Installing` + `(16, 137)` + `1.0` | Keep | Direct regression guard for epoch + transaction count + item percent |
| `epoch_nevra_keeps_the_package_name` | Unit | Both `name-epoch:ver-rel.arch` (dnf5) and `epoch:name-ver-rel.arch` (dnf4) strip cleanly to `name` | Keep | Covers multi-hyphen RPM names with epochs (`glibc-gconv-extra`, `libswscale-free`) |
| `transaction_steps_advance_the_count_without_a_package_name` | Unit | `Prepare transaction` and `Verify package files` emit count & phase without bogus package names | Keep | Prevents UI from rendering `"Prepare"` as an active package |
| `transaction_table_dump_is_not_a_named_package` | Unit | Table lines and section headers (`Upgrading:`, `Installing:`, `Removing:`) yield empty hint | Keep | Guards against transaction preview table flipping UI into Apply |
| `check_exit_codes` | Unit | Exit `0` / `100` / non-zero-with-packages are OK; exit `1` with 0 packages is hard error | Keep | Protects `dnf check-update` exit 100 contract |
| `kernel_core_in_pending_transaction_implies_reboot` | Unit | Pending `kernel-core` package triggers `detect_reboot_needed` | Keep | Guards reboot banner when kernel update is queued/applied |
| `kernel_tools_and_devel_do_not_imply_reboot` | Unit | `kernel-tools`, `kernel-tools-libs`, `kernel-devel`, `kernel-devel-matched` do not trigger reboot | Keep | Prevents false reboot prompts on userspace kernel add-ons |
| `core_system_packages_imply_reboot` | Unit | Core system RPMs (`kernel`, `kernel-modules*`, `glibc`, `systemd`, `dbus-broker`, `linux-firmware`, `grub2-*`) require reboot | Keep | Covers core reboot package list |
| `ordinary_packages_do_not_imply_reboot` | Unit | Normal desktop packages (`firefox`, `openssl`) do not request reboot | Keep | Prevents noisy reboot banner after ordinary updates |
| `dnf_log_reboot_phrase_still_counts` | Unit | Log phrase `"Reboot to apply"` triggers reboot even for non-kernel packages | Keep | Protects explicit dnf reboot message fallback |
| `dnf_log_negative_and_package_name_reboot_phrases_do_not_false_positive` | Unit | `"No reboot required"` and `"Upgrading: dracut-config-reboot"` do not trigger false reboot unless kernel present | Keep | Direct negative-phrase & package-name false-positive guard |
| `repoquery_format_separates_packages_with_newlines` | Unit | `REPOQUERY_UPGRADES_ARGS` queryformat ends with `\n` | Keep | Prevents dnf5 concatenation bug across packages |
| `parse_repoquery_sizes_skips_noise_and_matches_nevra` | Unit | Parses `name\tarch\tevr\tbytes`, ignores repo headers, matches packages via `apply_download_sizes` | Strengthen | Also test fixture `repoquery_sizes.txt`, non-tab spaces, and preserving pre-populated `size_bytes` (#21) |
| `cache_growth_reports_the_newest_rpm_and_whether_it_grew` | Unit | `poll_cache_growth` detects newest fresh `.rpm` under `<repo>/packages/` and `rising` on second poll | Strengthen | Add missing root dir, non-`.rpm` ignored, direct-in-repo fallback, and cursor reset (#21) |

### 1.2 `src/backend/flatpak.rs` (5 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `update_ref_roundtrip` | Unit | `update_ref_from_package` strips `flatpak:<scope>:` prefix back to ref string | Strengthen | Add missing-prefix / empty-ref negatives + `update_args_for_refs` coverage (#21) |
| `related_extension_is_named_on_the_parent_row` | Unit | Current parent ref with related `Install` extension stays listed, names ext ID in `detail`, gets size | Keep | Core GNOME-Software-parity invariant for NVIDIA GL extensions |
| `shared_extension_download_is_counted_once` | Unit | Extension pulled by 2 parents appears in both `detail` strings but attributes download bytes once | Keep | Prevents double-counting total Flatpak download size |
| `commit_update_does_not_claim_an_extension_install` | Unit | Direct `Update` op on ref populates `size_bytes` without `"installs ..."` in `detail` | Keep | Keeps normal app updates distinct from extension installs |
| `uninstall_is_not_described_as_an_install` | Unit | `FlatpakOpKind::Uninstall` is ignored for `"installs ..."` and download size | Keep | Prevents EOL extension uninstalls from inflating size or misleading copy |

### 1.3 `src/backend/fwupd.rs` (7 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `parse_json_devices` | Unit | `parse_get_updates` extracts `Name`, `DeviceId`, current→new release version into `Firmware` package | Strengthen | Exercise `get_updates_multi_device.json`, `get_updates_empty.json`, top-level array, single-device object, corrupt fallback (#21) |
| `parse_no_updates_text` | Unit | Plain `"No updates available"` yields empty vec | Strengthen | Test real `get_updates_no_updates.txt` (`"Devices with no available firmware updates:\n • System Firmware"`) & `get_updates_text.txt` — exposes current bug (#21) |
| `reboot_detection` | Unit | `"requires a reboot to complete"` → true; `"Restarting device..."` and `"No reboot is necessary"` → false | Keep | Critical distinction between device restart and host reboot |
| `json_check_reboot_needed_uses_exit_zero_contract` | Unit | `parse_reboot_needed_output` treats exit `0` (without `"no reboot"`/`"nothing to do"`) as true and exit `2` as false unless log states reboot | Keep | Protects exit-code contract of `fwupdmgr check-reboot-needed --json` |
| `success_line_marks_firmware_complete` | Unit | `"Successfully installed firmware"` emits `Completed`, `1.0`, `"Firmware installed"` | Keep | Needed because fwupd has no per-device name on completion line |
| `firmware_phase_lines_keep_work_progress` | Unit | `Verifying…`, `Restarting device…`, `Waiting…`, and `Percentage=45` extract work progress without premature completion | Keep | Covers real fwupd CLI line formats |
| `inventory_listing_is_not_progress` | Unit | `"Devices with..."` and `" • ..."` lines yield `ProgressHint::default()` | Keep | Prevents post-install inventory from triggering spurious phase transitions |

### 1.4 `src/console.rs` (3 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `carriage_return_replaces_the_open_console_line` | Unit | `push_live_tagged` sets `live_tail` and replaces open line cleanly; subsequent committed line finalizes it | Keep | Core invariant for dnf5 `\r` progress without log flooding |
| `console_buffer_caps` | Unit | Pushing 50 lines into `max_lines == 10` returns `Appended` for first 10 then `TrimmedAndAppended`, keeping newest 10 | Keep | Bounds memory and TextView growth |
| `push_and_clear_track_deltas_and_emptiness` | Unit | `push`, `clear`, `len`, `is_empty`, `last_line`, `full_text` stay consistent across trim and clear | Strengthen | Cover `max_lines == 0`, `max_lines == 1` with live-tail replacement & eviction (#21) |

### 1.5 `src/helper_mode.rs` (7 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `carriage_return_is_live_and_crlf_is_one_line` | Unit | `StreamSplitter` holds trailing `\r` across reads so `\r\n` stays one `Line`, while `\r` before text emits `Live` | Keep | Subtle streaming UTF-8/byte-boundary bug guard |
| `finish_commits_a_held_carriage_return` | Unit | EOF after a trailing `\r` commits final content as `Line` rather than dropping or emitting `Live` | Keep | Prevents losing last progress line on child exit |
| `empty_segments_are_skipped` | Unit | Bare `\n\r\n` produces no empty pieces | Keep | Avoids blank console spam |
| `stdbuf_disables_block_buffering` | Unit | Checks `STDBUF_ARGS == ["-o0", "-e0"]` (not `-oL`) | Strengthen | Currently asserts a private const array; replace/augment with actual `spawn_unbuffered` test using stdio `\r` flush (#22) |
| `only_allowlisted_argv` | Unit | Checks `CheckDnf`, `ApplyDnf`, `ApplyFwupd` argvs | Consolidate | Subset of `helper_protocol` + `model_and_protocol` tests; consolidate into exhaustive all-6-command allowlist assertion (#22) |
| `leaked_grandchild_fd_does_not_hang_drain_child` | Unit | `drain_child` finishes within `< 2s` (actual ~300ms grace) even when a background child (`sleep 60 &`) keeps stdout/stderr FD open | Keep | Prevents UI deadlock if a scriptlet or daemon forks cleanly |
| `drain_child_returns_exit_code_and_both_streams_normally` | Unit | Normal child stdout, stderr, and non-zero exit code (`7`) are all captured and returned | Keep | Baseline subprocess pipe-drain check |

### 1.6 `src/helper_protocol.rs` (7 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `roundtrip_commands` | Unit | All 6 `HelperCommand` variants round-trip `as_id()` ↔ `parse()` and `encode_run()` ↔ `ClientRequest::parse()` | Strengthen | Also assert `argv()[0].starts_with("/usr/bin/")` for all 6 variants (#22) |
| `rejects_unknown_and_injection` | Unit | Rejects `RUN rm -rf /`, `; reboot`, `dnf update`, `apply-flatpak-system`, empty line | Strengthen | Add `Quit` casing (`"quit"`, `"QUIT "`), `RUN` newline/null/NUL-byte, whitespace-only, `RUN  check-dnf` (#22) |
| `quit_and_end` | Unit | Parses `"QUIT"` and `"__END__ 100"` / `"__END__ 0"`, rejects non-end & `__DRAW__` | Strengthen | Cover negative codes (`"__END__ -1"`), overflow, and malformed `"__END__ abc"` (#22) |
| `draw_marker_is_not_a_committed_line` | Unit | `split_helper_line` separates `__DRAW__ ` from committed lines and `__END__` | Keep | Keeps protocol framing unambiguous |
| `argv_never_uses_shell_metacharacters` | Unit | Checks `CheckDnf`, `ApplyDnf`, `ApplyFwupd` for shell metacharacters (`;`, `\|`, `&`, `` ` ``, `$`) | Strengthen | Only checks 3 of 6 commands; must check ALL 6 variants plus quotes, redirects, newlines, and absolute paths (#22) |
| `check_fwupd_reboot_uses_json_so_it_cannot_prompt` | Unit | `CheckFwupdReboot` passes `--json` and never `-y` / `--assume-yes` | Keep | Critical guard against `fwupdmgr` interactive reboot prompt deadlock/reboot |
| `apply_fwupd_skips_post_update_prompts` | Unit | `ApplyFwupd` includes `-y`, `--no-reboot-check`, `--no-unreported-check`, `--no-device-prompt` | Keep | Guards against interactive stdin hangs in helper pipe |

### 1.7 `src/model.rs` (9 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `matches_progress_name_is_token_exact` | Unit | Exact package name, `name.arch`, and Flatpak app-ID token match; substrings (`kernel` vs `kernel-core`, `Thunder` vs `Thunderbird`) do not | Keep | Core token-matching invariant |
| `unnamed_progress_does_not_touch_packages` | Unit | `None`/empty/whitespace `package_hint` with non-Completed status returns `None` and leaves packages untouched | Keep | Prevents header lines from altering first package |
| `unnamed_completed_hint_finishes_first_open_row_for_source` | Unit | `package_hint: None` + `Some(Completed)` advances first non-done package matching `source` (used by firmware) | Keep | Documents intentional firmware exception |
| `second_arch_receives_progress_after_the_first_is_done` | Unit | Duplicate package name (`i686` Completed, `x86_64` Pending) selects second row | Keep | Prevents multi-arch `i686` completion from shadowing `x86_64` |
| `named_progress_completes_previous_live_item` | Unit | Naming a second `Installing` package marks previous `Installing` package `Completed` | Keep | Core package checklist advancement logic |
| `statusless_hint_returns_index_without_changing_rows` | Unit | `status_hint: None` finds index without mutating any package status | Keep | Protects metadata/preview hints |
| `download_and_install_hints_isolate_sources_and_reset_siblings` | Unit | Download reset (`Downloading -> Pending`) stays isolated per `UpdateSource`, never touching sibling Flatpak rows | Keep | Prevents cross-backend status clobbering |
| `done_package_is_not_resurrected_as_current` | Unit | `Completed` package is skipped by later `Installing` hint | Keep | Guards against DNF `Verifying` pass un-checking completed rows |
| `format_bytes_uses_compact_units` | Unit | Formats B/KB/MB/GB cleanly (`900 B`, `12 KB`, `21 MB`, `1.2 GB`) | Strengthen | Add 0 B, 1023 B, 1024 B, `<10` decimal boundary (`9.9 KB` vs `10 KB`), `parse_size_bytes` units/invalid input, `version_line`, `UpdateSource` & `PackageStatus` helpers (#21) |

### 1.8 `src/orchestrator.rs` (6 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `live_draw_is_not_a_committed_console_line` | Unit | `accept_helper_line` routes `__DRAW__ ` to `WorkerMsg::ConsoleDraw` without polluting `log` | Keep | Prevents `\r` redraws from entering post-apply `detect_reboot` log scans |
| `block_inhibitor_explains_try_again` | Unit | `reboot_failure_detail` translates `"block inhibitor"` to user-friendly `"try again"` hint | Keep | Protects systemd shutdown-inhibitor UX copy (complement with fixture test in #23) |
| `other_systemctl_output_is_shown` | Unit | Arbitrary non-empty `systemctl reboot` stderr/stdout is preserved verbatim | Keep | Avoids swallowing diagnostics |
| `empty_output_falls_back_to_exit_code` | Unit | Whitespace-only output formats `"systemctl reboot exited with <code>"` | Keep | Fallback diagnostic clarity |
| `retry_on_reboot_failed_retries_reboot` | Unit | Reboot failures emit `RetryAction::Reboot`; `SessionError::AuthDenied` emits `RetryAction::CheckAgain` | Keep | Ensures Retry button reboots vs re-checks appropriately |
| `work_channel_drops_events_after_cancel` | Unit | `WorkChannel` stops emitting after `cancel_background_work()` advances `WORK_GEN` | Keep | Guards cancellation race (note: global `WORK_GEN` needs mutex / injected token in #23) |

### 1.9 `src/presenter.rs` (17 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `idle_chrome_fresh` | Unit | `IdleScreen::Fresh` yields primary pill `"Check for Updates"` CTA and correct copy | Keep | HIG & UX contract |
| `idle_chrome_current_without_detail` | Unit | `IdleScreen::Current` without detail renders flat `"Check Again"` and `"Last checked ..."` | Keep | Verifies de-emphasized idle state |
| `idle_chrome_current_with_detail` | Unit | `IdleScreen::Current { detail: Some(...) }` overrides default description with detail | Keep | Protects detail pass-through (note UX issue #26 for title/icon) |
| `check_progress_groups_flatpak` | Unit | Progress grouping merges `FlatpakUser` + `FlatpakSystem` into 1 group | Consolidate | Strictly tested inside `check_step_names_the_cursor` (#21) |
| `check_step_names_the_cursor` | Unit | Tests all progress fractions/group counts/status strings including out-of-order `[Dnf, Firmware]` | Keep | Thorough state table check |
| `about_time_left_needs_enough_signal` | Unit | Hides ETA when `elapsed < 5` or `progress < 0.08`, returns `Some` later | Strengthen | Add exact threshold boundaries (`4s/5s`, `0.079/0.08`), `<45s`, `45..90s`, `>=90s`, and `progress >= 1.0` (#21) |
| `running_progress_captions_lead_with_done_count` | Unit | Captions lead with `"X done"` / `"X downloaded"` rather than inverted `"remaining"` | Keep | Direct regression guard for UI label consistency |
| `quiet_download_hides_eta_and_names_a_growing_cache_file` | Unit | `quiet_ui` priority order (`Flowing`, `ItemAtFull`, ETA cutoff `15s`, `CacheGrowing`, `Stalled` for DNF & Flatpak) | Keep | Covers all 4 `QuietMode` variants |
| `stalled_banner_names_the_daemon_only_when_asked` | Unit | `daemon_running == true` says `"Waiting on GNOME Software."`, never `"dnf daemon"` or `"Last line:"` | Keep | Regression guard on user-facing terminology |
| `cache_activity_note_mentions_growth` | Unit | Formats `"Downloading <pkg> — <size>"` + `" and rising"` iff `rising` | Keep | Matches `quiet_download` expectations |
| `source_summary_line_appends_rolled_up_download_size` | Unit | Sums sizes into Ready caption | Strengthen | Weak `ends_with(" MB") \|\| ends_with(" GB")` assertion; assert exact byte total `"1 system · 1 apps · 0 firmware · 185 MB"` (#21) |
| `source_summary_line_omits_size_when_unknown` | Unit | Omits size suffix when all packages have `size_bytes: None` | Keep | Avoids `"· 0 B"` clutter |
| `run_heading_switches_with_the_stage` | Unit | `Verifying` stays distinct even in `RunPass::Apply`; `Download` vs `Apply` headings map accurately | Keep | Header bar title contract |
| `now_playing_prefers_named_package` | Unit | Preference hierarchy: active non-done package > `current_name` > `phase_label`; subtitle avoids duplicate text | Keep | Protects now-playing card clarity (also test done active_index fallback in #21) |
| `header_titles_follow_the_phase_table` | Unit | Exhaustive check of `window_header` wide/narrow, `checking_footnote`, `done_header_subtitle`, `soft_error_banner_title`, constants | Keep | Core header chrome table guard |
| `alert_dialogs_use_the_accepted_copy` | Unit | Exact response IDs, labels, Destructive/Suggested flags, and defaults for Stale & Stop alerts | Keep | Protects HIG button roles & response IDs |
| `fail_retry_label_follows_the_action` | Unit | `CheckAgain -> "Check Again"`, `Reboot -> "Restart"` | Keep | Retry button label contract |

### 1.10 `src/privilege/session.rs` (3 tests — Unit / OS Boundary)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `helper_path_respects_env` | Unit | `helper_path_with` priority order: override > `current_exe` > `/usr/bin/fedora-updater` > `"fedora-updater"` | Keep | Pure precedence check without touching env |
| `run_local_command_drains_large_stderr_without_deadlock` | Unit / OS | `run_local_command` drains >128 KiB stderr (beyond 64 KiB pipe buffer) concurrently with stdout in `< 5s` | Keep | Real pipe-deadlock regression test |
| `helper_flag_is_stable` | Unit | `assert_eq!(HELPER_FLAG, "--helper")` | Consolidate | Trivial constant equality; fold into helper spawn/protocol test suite (#22) |

### 1.11 `src/release_upgrade.rs` (12 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `active_45_is_offered_to_a_44_install` | Unit | Active 45 triggers offer on Fedora 44 | Keep | Core happy path (also run against fixture in #21) |
| `a_machine_already_on_the_newest_active_release_is_offered_nothing` | Unit | Current == highest Active returns `None` | Keep | Prevents offering current release |
| `the_offer_is_the_highest_active_number_not_the_next_one` | Unit | Fedora 43 sees 45 (highest Active) rather than 44 | Keep | Prevents stale step-by-step offer |
| `a_bad_collections_document_offers_nothing` | Unit | Invalid JSON, missing key, empty array safely return `None` | Keep | Network/JSON resilience |
| `os_release_version_reads_a_bare_or_quoted_id` | Unit | Parses `VERSION_ID=44` and `VERSION_ID="44"`, returns `None` if absent | Strengthen | Test single quotes, whitespace, non-numeric, and `tests/fixtures/release_upgrade/os_release_*.txt` (#21) |
| `reminder_copy_names_the_detected_release_and_the_two_commands` | Unit | Banner title, accessible button label, and 2-line commands format cleanly | Keep | Copy contract |
| `upgrade_commands_are_two_separate_steps` | Unit | Download and reboot stay separate lines without `&&` | Keep | Prevents unattended auto-reboot before dnf prompt |
| `release_age_is_calendar_days_from_the_active_row` | Unit | `days_between` / `days_since_active` exact diffs, same-day 0, future `None`, leap-year `2024-02-28..03-01` | Strengthen | Add century leap-year (`2000`, `2100`), invalid dates (`2025-02-29`, month 13, day 0), boundaries (#21) |
| `release_age_caption_uses_the_parenthetical` | Unit | Singular/plural/today wording `(Released today)`, `(1 day...)`, `(N days...)` | Keep | Grammar copy contract |
| `an_offer_carries_the_age_when_the_active_row_has_a_date` | Unit | `offer_from_collections` returns `days_since_release: Some` when `date_updated` present, `None` when absent | Keep | Graceful optional-age handling |
| `active_non_fedora_linux_entries_are_ignored` | Unit | `Fedora EPEL` etc. never trigger upgrade banner | Keep | Prevents EPEL version `99` false upgrade notice |
| `today_local_formats_a_valid_calendar_date` | Unit | `today_local()` succeeds and passes `civil_days` validation | Keep | Checks GLib local-time formatting |

### 1.12 `src/state.rs` (2 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `should_enter_apply_distinguishes_download_preview_and_apply_steps` | Unit | Protects `phase_is_download`, preview bare `"Installing"` ignore, real phase entry, named package entry | Keep | Core download-to-apply switch heuristic |
| `reset_live_packages_preserves_done_statuses` | Unit | `reset_live_packages` resets `Downloading`/`Installing` to `Pending` without touching `Completed`/`Failed` | Keep | Guards pass reset on multi-stage apply |

### 1.13 `src/ui/*` (5 tests — Unit / Static Template Checks)

| File | Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|---|
| `src/ui/dialogs.rs` | `log_template_has_text_view` | Unit | Every `obj(&builder, ...)` ID in `dialogs.rs` exists in compiled `log_window.ui` + has `GtkTextView` | Keep | Fast compile-time Blueprint ID check; complement with live GTK widget tests in #24 |
| `src/ui/preview.rs` | `preview_state_for_idle_and_failed` | Unit | Preview keys `idle`, unknown, `idle-current`/`up-to-date`, `failed` map accurately | Keep | Guards `FEDORA_UPDATER_PREVIEW`; cover all remaining preview keys in #24 |
| `src/ui/preview.rs` | `many_running_packages_matches_a_large_transaction` | Unit | `many_running_packages()` produces >=70 rows with >=40 completed | Keep | Ensures overflow/expander scroll preview remains realistic |
| `src/ui/window.rs` | `window_template_has_adwaita_pages` | Unit | Every `obj(&builder, ...)` ID in `window.rs` exists in `window.ui` + key structural layout properties | Keep | Guards Blueprint refactors; complement with rendered widget tests in #24 |
| `src/ui/window.rs` | `session_started_commit_leaves_auth_and_panics_if_a_borrow_is_held` | Unit | `commit_session_started` moves `Authenticating -> Checking` and fails loudly rather than silently dropping if `RefCell` borrowed | Keep | Direct regression guard for `RefMut` borrow bug during polkit handshake |

### 1.14 `tests/helper_session.rs` (4 tests — Functional / E2E)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `helper_hello_quit` | E2E | Spawns compiled `fedora-updater --helper`, reads `HELLO_LINE`, sends `QUIT`, asserts `BYE_LINE` & exit 0 | Keep | Real binary multi-call handshake E2E |
| `helper_rejects_unknown_command` | E2E | Real `--helper` rejects `RUN rm-rf-root` with `__END__ 2`, stays alive for `QUIT` | Keep | Real IPC security allowlist rejection |
| `helper_runs_check_dnf_if_present` | E2E (`#[ignore]`) | Runs live `RUN check-dnf` against host `/usr/bin/dnf` and asserts `__END__` within 120s | Keep | Stays `#[ignore]` for live-host opt-in smoke testing |
| `close_session_does_not_block_when_helper_command_hangs` | Functional / E2E | Hung helper holding `SESSION` mutex is killed via `close_session()` in `< 2s`, unblocking worker & GTK thread | Keep | Critical deadlock prevention test (expand pool/protocol suite in #22 / #24) |

### 1.15 `tests/model_and_protocol.rs` (6 tests — Unit)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `apply_order_is_dnf_flatpak_firmware` | Unit | `sort_for_apply`, `sources_present`, and `count_by_source` follow DNF → Flatpak → Firmware order | Keep | Note: include `FlatpakSystem` in extended model tests in #21 |
| `finalize_and_progress_helpers` | Unit | `finalize_source` marks non-done rows Completed/Failed without overwriting already-Completed rows | Keep | Protects partial progress finalization |
| `protocol_rejects_path_traversal_and_shell` | Unit | `ClientRequest::parse` rejects `&&`, `../`, embedded `\n`, lowercase `run`, `EXEC` | Keep | Security boundary coverage |
| `end_line_roundtrip` | Unit | Tests `format_end_line`/`parse_end_line` round-trip | Strengthen | Currently skips `code < 0` (`-1`) despite `i32` parser support and overlaps with `quit_and_end` (#22) |
| `every_helper_command_has_nonempty_safe_argv` | Unit | Iterates all 6 `HelperCommand` variants for non-empty argv, no spaces in `argv[0]`, `parse` roundtrip | Strengthen | Assert `argv[0].starts_with("/usr/bin/")` (remove comment saying `"no absolute weirdness required"`) (#22) |
| `dnf_apply_is_update_not_upgrade` | Unit | `ApplyDnf.argv()` uses `update` `-y` rather than `upgrade` | Keep | CLI semantic contract |

### 1.16 `tests/state_machine_exhaustive.rs` (41 tests — Unit / Functional)

| Test | Tier | Invariant Guarded | Disposition | Rationale & Target Child |
|---|---|---|---|---|
| `happy_path_multi_source` | Functional | Full Check → Ready → Apply → Done multi-source flow preserves sort order & aggregates `needs_reboot` | Keep | Core end-to-end reducer journey |
| `illegal_transitions` | Functional | Invalid events from `Idle` return `Err` | Strengthen | Only tests `Idle`; verify invalid transitions from `Authenticating`, `Checking`, `Ready`, `Running`, `Done`, `Failed` too (#23) |
| `cannot_apply_empty_ready` | Unit | `StartApply` on empty `Ready` returns `Err` | Keep | Guards invariant before entering `Running` |
| `in_app_cancel_auth_returns_idle` | Functional | `CancelAuth` exits `Authenticating -> Idle`, fails outside auth | Keep | Auth cancellation path |
| `auth_cancel_style_fail` | Functional | `Fail` during auth moves to `Failed` then `ResetToIdle -> Idle` | Keep | Error recovery transition |
| `progress_marks_package_and_completes_previous` | Functional | Successive named `Installing` hints complete previous item cleanly | Keep | Package checklist flow |
| `unnamed_progress_does_not_burn_down_or_poke_first_package` | Functional | Unnamed `ApplyProgress` leaves all packages `Pending` & preserves `work_progress` | Keep | Prevents false package advancement |
| `kernel_hint_does_not_complete_kernel_core` | Functional | Naming `kernel` activates `kernel` only, not `kernel-core` | Keep | Token isolation during live run |
| `verify_pass_does_not_move_current_back_to_completed` | Functional | `Verifying` hint for already-Completed `firefox` does not displace active `kernel` | Keep | Multi-pass dnf5 UI stability |
| `source_switch_progress_does_not_mark_first_package` | Functional | Initial source announcement resets live indicators without touching package statuses | Keep | Source transition cleanliness |
| `soft_errors_on_empty_check_surface_message` | Functional | Empty `CheckComplete` with `soft_errors` surfaces `"No updates available (...)"` in `Idle` | Keep | Note: behavior under UX review in [#26](https://github.com/rkalla/fedora-updater/issues/26) |
| `dnf_failure_does_not_block_done_if_others_ok` | Functional | Failed DNF + OK Flatpak finishes in `Done` with `failed == 1` | Strengthen | Assert `needs_reboot` preservation, 3+ sources partial fail, and document ResetToIdle UX bug [#27](https://github.com/rkalla/fedora-updater/issues/27) (#23) |
| `session_ready_wrong_purpose_rejected` | Unit | `SessionReady(Apply)` while `Authenticating(Check)` returns `Err` | Keep | Auth purpose mismatch guard |
| `a_release_note_stays_across_a_check_and_does_not_change_phase` | Functional | `ReleaseNoted` persists across `StartCheck` and `None` clears without side effects | Keep | Phase-orthogonal banner state |
| `check_flow_no_updates` | Functional | Check with 0 updates → `IdleScreen::Current` `"Last checked just now"` | Keep | Up-to-date flow |
| `all_failed_goes_to_failed_phase` | Functional | All-failed apply transitions to `Phase::Failed { retry: CheckAgain }` | Keep | Prevents misleading Done screen when nothing succeeded |
| `done_resets_to_current_idle` | Functional | `Done` + `ResetToIdle` → `IdleScreen::Current` | Strengthen | Also test `ResetToIdle` from `Done { failed > 0 }` & `Failed` after UX decision in [#27](https://github.com/rkalla/fedora-updater/issues/27) |
| `empty_phase_label_does_not_clobber_current_phase` | Functional | Empty `phase_label` keeps existing phase and preserves `RunPass::Apply` | Keep | Guards sparse firmware/progress hints |
| `download_pass_does_not_finish_packages_and_apply_restarts_the_bar` | Functional | Download counts fill bar in `Download` pass without completing packages; `Running transaction` resets bar to 0.0 and enters `Apply` pass | Keep | Flagship multi-pass progress state test |
| `firmware_success_progress_completes_the_live_item` | Functional | Unnamed `Completed` firmware progress completes live firmware row | Keep | Firmware completion contract |
| `ready_list_expires_after_24_hours` | Unit | `update_list_needs_refresh` false `<= 24h` & on backward clock, true `> 24h` | Keep | Exact boundary guard for `UPDATE_LIST_MAX_AGE` |
| `empty_check_clears_list_timestamp` | Unit | Empty `CheckComplete` clears `updates_listed_at` | Keep | Prevents stale trigger on empty state |
| `stale_timestamp_does_not_block_other_phases` | Unit | `update_list_needs_refresh` only applies in `Phase::Ready` | Keep | Prevents blocking non-Ready phases |
| `preview_ready_without_timestamp_can_still_apply` | Unit | Missing `updates_listed_at` stays applicable | Keep | Supports `FEDORA_UPDATER_PREVIEW=ready` |
| `transaction_preview_does_not_mark_downloads_complete` | Functional | Feeding `dnf` headers + download lines via `parse_progress` stays in `Download` with 0 completed | Keep | Multi-stage parser+reducer invariant |
| `bare_installing_status_does_not_start_the_apply_pass` | Unit | `Installing`/`Upgrading` status without package name or real phase does not enter `Apply` | Keep | Protects preview header lines |
| `stage_count_requires_a_positive_total` | Unit | `stage_total == 0` or partial `Some`/`None` clears `stage` cleanly | Keep | Avoids division by zero / bogus `1/0` UI |
| `stage_count_sets_both` | Unit | `[2/4]` sets `StageCount { 2, 4 }` and preserves across `None/None` progress | Keep | Progress bar stability between bracket lines |
| `source_switch_returns_to_download` | Functional | Advancing from `Dnf(Apply)` to `FlatpakUser` resets `pass = Download`, `stage = None`, `overall_progress = 0.0` | Keep | Multi-backend apply pass isolation |
| `stop_before_install_returns_every_package_to_pending` | Functional | `StopApply { during_transaction: false }` returns to `Ready` with all packages `Pending` & soft notice | Keep | Safe pre-transaction cancellation |
| `stop_during_install_warns_that_the_rpm_database_may_be_half_applied` | Functional | `StopApply { during_transaction: true }` goes to `Failed` mentioning `dnf history` | Keep | RPM DB safety warning |
| `stop_outside_a_run_is_rejected` | Unit | `StopApply` on `Idle` returns `Err` | Keep | Prevents invalid stop outside `Running` |
| `apply_when_ready_survives_until_ready` | Functional | `StartCheck { then_apply: true }` preserves `apply_when_ready` through `SessionReady`/`BackendCheckDone`/`CheckComplete` | Keep | Stale-list refresh-and-apply chain |
| `empty_check_clears_apply_when_ready` | Functional | Refresh finding 0 updates clears `apply_when_ready` and shows up-to-date | Keep | Prevents dangling auto-apply |
| `cancel_clears_apply_when_ready_and_drops_detail` | Functional | `CancelAuth` restores `last_checked` and clears `apply_when_ready` and stale error `detail` | Keep | Auth cancel restoration contract |
| `fail_clears_apply_when_ready` | Functional | `Fail` clears `apply_when_ready` | Keep | Prevents delayed auto-apply after failure |
| `apply_reset_and_stop_clear_the_flag_and_session_ready_leaves_it` | Functional | Tests `apply_when_ready` clearing on `StartApply`/`ResetToIdle`/`StopApply` vs preservation on `SessionReady` | Keep | Comprehensive flag lifecycle check |
| `session_started_purpose_follows_the_phase` | Unit | `session_started_purpose` returns `Some` for `Authenticating`/`Checking`/`Running`, `None` otherwise | Keep | Late `SessionStarted` message safety |
| `reboot_fail_event_retries_reboot` | Unit | Reboot fail uses `RetryAction::Reboot`; apply fail uses `RetryAction::CheckAgain` | Keep | Retry semantics split |
| `running_state_helpers` | Unit | Verifies `done_and_total()`, `is_applying()`, `enter_apply_pass()`, `reset_for_source()` | Keep | Helper method contracts |
| `failed_transition_leaves_state_unchanged` | Unit | Error transitions leave `phase`, flags, and timestamps untouched | Keep | No-partial-mutation atomicity guarantee |

---

## 2. Module-by-Module Coverage Gap Matrix

| Module | Current Tests | Tested Surface | Untested / Under-Tested Invariants & Bugs Found | Planned Hardening |
|---|---:|---|---|---|
| `src/backend/mod.rs` | 3 | `parse_progress` & `detect_reboot` dispatch across all 4 sources, `extract_percent` range/format guards | **#21 Complete** — all backend traitless dispatch and percentage extraction invariants covered | Done (#21) |
| `src/backend/dnf.rs` | 21 (+ `tests/unit_fixtures.rs`) | Classic & dnf5 check-update, dedup, all 10 supported arches, epochs, Obsoleting/summary headers, repoquery tab/space & pre-set preservation, download/install/verify/scriptlet/cleanup/complete progress, exit codes, all reboot packages/phrases & negative guards, `poll_cache_growth` (missing, stale, non-rpm, steady, rotation) | **#21 Complete** — fixed `Transaction Summary:` package-count lines (`Installing: 1 package`), `Obsoleting` check-update header, and `[8/8] Cleanup / Verifying` non-alphabetic `"/"` token extraction; all 8 `tests/fixtures/dnf/*` fixtures consumed | Done (#21) |
| `src/backend/flatpak.rs` | 8 (+ `tests/unit_fixtures.rs`) | `update_ref_roundtrip` + invalid IDs, `update_args_for_refs` (`--user`/`--system`/`--noninteractive`), `packages_from_flatpak_updates` (NVIDIA extensions, shared ext once-only bytes on earliest parent, uninstall ignored, duplicate refs, empty field fallbacks), `parse_progress_line` (`app/`/`runtime/` prefixes, nothing-to-update, `error:` silence) | **#21 Complete** — fixed `parse_progress_line` prefix stripping before `split('/')`, bogus dotted/percent app-ID rejection, `error:` short-circuit, and `packages_from_flatpak_updates` first-index preservation on duplicate refs; all 5 `tests/fixtures/flatpak/*` fixtures consumed | Done (#21) |
| `src/backend/fwupd.rs` | 8 (+ `tests/unit_fixtures.rs`) | Multi-device/array/single JSON & empty/corrupt fallback, text tree glyphs & `Bootloader Version` ignore, `"no available firmware updates"` bullet ignore, all phase lines + `Percentage=`, battery/error line ignore, `detect_reboot_needed`, `parse_reboot_needed_output`, `check_output_is_no_updates` (exit 2 vs `"not found"` / corrupt JSON) | **#21 & #23 Complete** — fixed `parse_text_updates` ignoring `"Devices with no available..."` / bullet / JSON-syntax lines, `parse_progress_line` ignoring fatal `Cannot`/`Error:`/`Failed` lines, and added exit-2 no-updates / empty-JSON classifier (#23); all 9 `tests/fixtures/fwupd/*` fixtures consumed | Done (#21, #23) |
| `src/release_upgrade.rs` | 12 (+ `tests/unit_fixtures.rs`) | Offer selection (`> current`, max Active, EPEL/ELN filtering), bad JSON/HTML, `VERSION_ID` bare/double/single quotes & comments, upgrade steps, calendar days including century leap rules (`2000`/`2100`), invalid/UTF-8 dates, optional age | **#21 Complete** — all 7 `tests/fixtures/release_upgrade/*` fixtures consumed | Done (#21) |
| `src/console.rs` | 5 | Live `\r` replacement, cap=10 trim deltas, cap=0 and cap=1 live-tail eviction, `push` & `clear` resetting `live_tail`, reference ring-model lockstep replay across every `ConsoleDelta` | **#21 Complete** | Done (#21) |
| `src/model.rs` | 12 (+ 2 in `tests/model_and_protocol.rs`) | Token matching, unnamed progress isolation, multi-arch twins, download/install source isolation, `format_bytes` boundaries, `parse_size_bytes` (commas/units/invalid), `total_download_bytes` saturation, `version_line`, `UpdateSource`/`AuthPurpose`/`PackageStatus`/`Stopwatch` helpers, `sort_for_apply`/`finalize_source`/`overall_progress` | **#21 Complete** | Done (#21) |
| `src/presenter.rs` | 17 | Idle chrome (including `Warning` + `View Log`), `check_progress` (consolidated Flatpak grouping), `about_time_left` exact thresholds, running captions & `None` source, `quiet_ui` threshold edges (`4/5`, `14/15`, `119/120`), exact byte totals in `source_summary_line`, `run_heading`, `now_playing_*` fallbacks, full `HeaderView` table, `ready_banner_title` (`Stopped before install` vs check errors), alert dialogs, `fail_retry_label` | **#21 & #24 Complete** — consolidated duplicate `check_progress_groups_flatpak`, replaced weak suffix check in `source_summary_line_appends_rolled_up_download_size`, and added `ready_banner_title` (#24) | Done (#21, #24) |
| `src/state.rs` | 2 (unit) + 49 (`state_machine_exhaustive.rs`) | Multi-source Happy/Fail/Stop paths, `apply_when_ready`, `should_enter_apply`, `reset_live_packages`, `UPDATE_LIST_MAX_AGE`, illegal transitions across all 7 phases (`Idle`/`Authenticating`/`Checking`/`Ready`/`Running`/`Done`/`Failed`), `SessionReady` idempotency, 3-source partial fail + reboot preservation, `ResetToIdle` across `Done`/`Failed`/`Idle(Warning)`, repeated `RetryApply` preservation, unknown/completed `ApplyProgress` hints & `ReleaseNoted` preservation | **#23 Complete** — strengthened `illegal_transitions`, `dnf_failure_does_not_block_done_if_others_ok`, and `done_resets_to_current_idle`, plus new invariant tests | Done (#23) |
| `src/helper_protocol.rs` | 7 (+ `tests/model_and_protocol.rs`) | Strict allowlist & uniqueness across all 6 `HelperCommand` variants (exact argvs, `/usr/bin/{dnf,fwupdmgr}`, zero metacharacters/whitespace), `ClientRequest::parse` casing/extra-args/traversal/NUL/CR/LF rejections, `split_helper_line` (`__DRAW__ ` vs committed/empty/nested), `format_end_line`/`parse_end_line` negative & malformed bounds | **#22 Complete** — consolidated subset argv checks, fixed missing negative-code & absolute `/usr/bin/` assertions in `tests/model_and_protocol.rs` | Done (#22) |
| `src/helper_mode.rs` | 12 (+ `tests/helper_session.rs`) | `StreamSplitter` (`\r`/`\r\n` split across reads, multi-byte & invalid UTF-8 across 1/2-byte reads, consecutive `\r` draws, EOF & empty segments), `emit_piece` `__DRAW__ ` framing, `spawn_unbuffered` `stdbuf -o0 -e0` + `PYTHONUNBUFFERED=1` + `Stdio::null()` stdin, `drain_child` (>128 KiB concurrent stdout+stderr, leaked grandchild FD grace window, empty & non-zero exits, interleaved `\r`/`\r\n`), multi-call `--helper` extra-arg exit 2 & malformed requests (`tests/helper_session.rs`) | **#22 Complete** — consolidated redundant `only_allowlisted_argv` into `helper_protocol` | Done (#22) |
| `src/privilege/session.rs` | 9 | `helper_path_with` precedence, `run_local_command` (>128 KiB stderr, concurrent >128 KiB stdout+stderr, non-zero exit, missing exe `Spawn`, empty exit 0), `start_direct` failure modes (empty stdout `AuthDenied`, wrong hello `Protocol`, relative `HelperNotFound`, absolute `Spawn`), sequential command PID reuse & `__DRAW__` streaming, mid-command EOF `Closed` & `is_running()` false, `kill_recorded_local_command` | **#22 Complete** — added process-group kill & reap on `start_with` `AuthDenied`/`Protocol` error paths; consolidated trivial `helper_flag_is_stable` into `tests/helper_session.rs` | Done (#22) |
| `src/privilege/pool.rs` | 5 (+ `tests/helper_session.rs`) | Shared `SESSION` reuse across calls, `session_is_alive()` dead-helper detection & respawn, eviction on `SessionError::{Closed, Io, AuthDenied}`, `AuthDenied` start leaving pool empty, `SESSION` mutex poison/panic recovery, non-blocking `close_session()` + grandchild `SIGKILL` during hung command | **#22 Complete** — added serialized unit & public-API integration pool tests (`PRIVILEGE_TEST_LOCK` / `SESSION_TEST_LOCK`) | Done (#22) |
| `src/orchestrator.rs` | 8 (+ 18 `tests/functional_flows.rs`) | `OrchestratorRuntime` / `SystemRuntime` seam + `WorkCancel` token (`run_*_with`, `execute_*`), `accept_helper_line` live draw, `reboot_failure_detail`, `reboot_fail_event`, global `WORK_GEN` & `cancel_background_work` pipeline stop, `summarize_apply_failure`, full check/apply/reboot/stop/stale/auth functional flows across `tests/fixtures/` | **#23 Complete** — fixed fwupd exit-2 `"Devices with no available firmware updates:"` / empty-JSON false error and prevented failed apply from falsely flagging `needs_reboot`; `WorkCancel` isolates parallel functional runs | Done (#23) |
| `src/ui/window.rs` | 3 (single serialized GTK E2E + 2 unit) (+ 5 Layer A `tests/e2e_pipeline.rs`) | All Blueprint IDs via fixed `builder_ids_in_source` (`obj(&builder,` & `obj(builder,`) + layout constraints, `commit_session_started` borrow panic, un-presented live `Widgets::for_test()` across all 7 phases (`Idle`/`Authenticating`/`Checking`/`Ready`/`Running`/`Done`/`Failed`), `ready_update` sensitivity, `ready_banner_title` pre-install Stop copy, `narrow` breakpoint subtitle hiding, console drawer toggle + incremental `apply_console_delta` (`Appended`, `ReplacedLast`, `TrimmedAndAppended`), `run_completed` expander user-open persistence, `done_installed` Failed-first sorting, `paint_quiet_chrome_with` quiet/stall banners, release-upgrade banner, full `handle_worker_with` replay; headless Broadway verified via `scripts/test-ui-headless.sh` | **#24 Complete** — fixed `ready_banner` after pre-install `StopApply`, added empty-list `ready_update` sensitivity guard, and fixed `builder_ids_in_source` to validate all `widgets_from_builder` IDs | Done (#24) |
| `src/ui/dialogs.rs` | 1 + GTK E2E (`ui::window::tests`) | `log_window.ui` ID check, `build_confirm_dialog` (`stale_update_dialog` & `stop_update_dialog` headings/bodies/responses/appearances/defaults & response callback gating), `build_upgrade_dialog` & `command_row` (2 separate copyable commands with no trailing `\n`), `build_log_window` (`TextView` buffer, read-only, monospace, Escape close) | **#24 Complete** — extracted un-presented builders (`build_log_window`, `build_upgrade_dialog`, `build_confirm_dialog`) tested under headless Broadway | Done (#24) |
| `src/ui/rows.rs` | GTK E2E (`ui::window::tests`) | `source_badge` (DNF/FP/FW CSS classes), `package_row` (`show_status` true/false, badge fallback, NVIDIA 2-line subtitle wrapping), `checklist_row` + `sync_checklist_row` CSS transitions across all 5 `PackageStatus` states, `finished_row` (`mark_completed` toggle, Failed always badged, binary size labels), `refill_group`/`refill_expander`/`append_expander` | **#24 Complete** | Done (#24) |
| `src/ui/preview.rs` | 2 | All 13 preview strings (`idle`, `idle-current`/`up-to-date`, `idle-warning`, `auth`, `checking`, `ready`, `ready-many`, `ready-warn`, `running`, `running-many`/`running-expanded`, `done`, `done-mixed`, `failed`), unknown fallback, `many_running_packages`, `initial_console()` | **#24 Complete** | Done (#24) |

---

## 3. Edge-Case & UX Recovery Matrix

Each real-world scenario below is traced end-to-end: **Fixture / Backend Signal → Orchestrator → `state::reduce` → Presenter & GTK UX Affordance → Current Coverage → Discovered Gaps / Bugs**.

| # | Scenario | Fixture(s) | Expected Backend & Orchestrator Behavior | Expected `state::reduce` Transition | Expected Presenter & GTK UX Affordance | Current Coverage | Gaps / Bugs / Issue Links |
|---|---|---|---|---|---|---|---|
| 1 | **DNF dependency conflict** during Apply | `tests/fixtures/dnf/apply_dep_conflict.txt` | Helper streams `Problem: cannot install...` lines; exits 1 → `BackendApplyDone { Dnf, success: false, needs_reboot: false, error: Some(...) }`; subsequent Flatpak/fwupd still run sequentially | If DNF only source → `Phase::Failed { detail: "...dependency conflict — Problem 1...", retry: CheckAgain }`. If other sources succeed → `Phase::Done { failed > 0, failure_reasons }` | **All failed:** `fail_page` shows summarized conflict inline + `Check Again` + `View Log`. **Partial:** `Done` shows `"Updates Finished With Problems"`, `done_problem_banner`, `done_retry_btn`, Failed rows first | `all_failed_surfaces_backend_failure_reasons_inline`, `summarize_apply_failure_extracts_actionable_causes`, `partial_failure_done_preserves_failure_reasons_and_resets_to_warning_idle`, `apply_partial_failure_dnf_dep_conflict_while_flatpak_and_fwupd_succeed`, `e2e_journey_2_empty_then_updates_partial_failure_and_check_again_recovery`, `gtk_widget_window_rows_and_dialogs_e2e` | **Resolved in [#27](https://github.com/rkalla/fedora-updater/issues/27); functional (#23) + Layer A pipe E2E & Layer B GTK E2E (#24) complete.** |
| 2 | **Broken repo / mirror unreachable** during Check | `tests/fixtures/dnf/check_update_repo_error.txt` | `dnf check-update` exits 1, 0 packages → `BackendCheckDone { Dnf, [], soft_error: Some("dnf check-update failed (exit 1)") }` + `soft_errors.push(...)`; Flatpak/fwupd still checked | **Updates found in other source:** `Phase::Ready { soft_errors: [..] }`. **No updates in any source:** `Phase::Idle(IdleScreen::Warning { title: "Could not check all sources", .. })` | **In `Ready`:** `ready_banner` reveals `"1 source could not be checked"` + `"View Log"`. **In `Idle`:** warning icon (`dialog-warning-symbolic`), `"Could not check all sources"`, `"Check Again"` pill, and `idle_log` `"View Log"` button | `soft_errors_on_empty_check_surface_message`, `idle_chrome_warning_surfaces_problems_and_view_log`, `check_dnf_repo_error_with_other_sources_succeeding_shows_ready_banner`, `check_all_sources_fail_or_fwupd_corrupt_surfaces_warning_idle`, `e2e_journey_4_helper_crash_eof_mid_apply_spawns_new_session_on_check`, `gtk_widget_window_rows_and_dialogs_e2e` | **Resolved in [#26](https://github.com/rkalla/fedora-updater/issues/26); functional (#23) + Layer A pipe E2E & Layer B GTK E2E (#24) complete.** |
| 3 | **RPM DB lock / `dnf5daemon-server` stall** | `tests/fixtures/dnf/apply_rpm_lock_error.txt` (lock) + silent pause during Apply (stall) | **Lock error:** helper exits 1 with `/var/lib/rpm/.rpm.lock` message → `BackendApplyDone { Dnf, success: false, error: Some("System: RPM database is locked...") }`. **Stall (120s+ no output):** UI timer polls `poll_cache_growth` & `dnf_daemon_is_running()` | Lock error → `Failed` or `Done { failed > 0 }` with lock guidance inline (never sets `needs_reboot`); during stall stays `Phase::Running` | During quiet download with growing cache → spinner + `"Downloading <pkg> — <size> and rising"`; on `>=120s` true stall with daemon → `run_stall` banner `"No output for N minutes. Waiting on GNOME Software."` + Stop button remains available | `summarize_apply_failure_extracts_actionable_causes`, `apply_partial_failure_dnf_rpm_lock_does_not_falsely_flag_reboot`, `apply_total_failures_surface_actionable_reasons_and_preserve_console`, `quiet_download_hides_eta_and_names_a_growing_cache_file`, `stalled_banner_names_the_daemon_only_when_asked`, `gtk_widget_window_rows_and_dialogs_e2e` | **Lock UX (#27), failed-apply reboot bug (#23), and GTK quiet/stall `paint_quiet_chrome_with` E2E (#24) all resolved.** |
| 4 | **Partial multi-source failure** (DNF fails, Flatpak/fwupd succeed) | `dnf/apply_dep_conflict.txt` + `flatpak/apply_progress_happy.txt` + `fwupd/apply_happy_reboot.txt` | DNF `BackendApplyDone(false, false, Some(...))`; Flatpak & Firmware continue; if Firmware sets reboot/CheckFwupdReboot true → `ApplyComplete { needs_reboot: true }` | Transitions to `Phase::Done { failed, failure_reasons, needs_reboot: true }`; `Done` click → `IdleScreen::Warning` (`"Some updates failed"`) | Header `"Updates Finished With Problems"`, `done_problem_banner` + `done_retry_btn` + `reboot_banner` (suggested-action stays on Reboot if needed, else Retry), Failed rows first | `dnf_failure_does_not_block_done_if_others_ok`, `partial_failure_done_preserves_failure_reasons_and_resets_to_warning_idle`, `apply_partial_failure_dnf_dep_conflict_while_flatpak_and_fwupd_succeed`, `apply_flatpak_user_failure_while_dnf_and_flatpak_system_succeed`, `e2e_journey_2_empty_then_updates_partial_failure_and_check_again_recovery`, `gtk_widget_window_rows_and_dialogs_e2e` | **Resolved in [#27](https://github.com/rkalla/fedora-updater/issues/27); functional (#23) + Layer A pipe E2E & Layer B GTK E2E (#24) complete.** |
| 5 | **>24h stale Ready list** | `tests/fixtures/dnf/check_update_classic.txt` (aged > 24h) | Clicking `Update All` checks `state.update_list_needs_refresh(now)`. If >86400s → `prompt_stale_update_list`. Cancel leaves `Ready`; Confirm (`_Refresh`) starts check with `then_apply: true`, clears `apply_when_ready` if 0 updates, else chains `begin_apply` | `Ready` + `StartCheck { then_apply: true }` → `Checking` → `Ready` → auto `StartApply` (or `Idle(Current)` if empty) | `Adw.AlertDialog` `"Refresh Update List?"`, `_Cancel` (`no`, close), suggested `_Refresh` (`yes`, default) | `ready_list_expires_after_24_hours`, `apply_when_ready_survives_until_ready`, `empty_check_clears_apply_when_ready`, `stale_ready_list_over_24h_refreshes_and_auto_applies`, `e2e_journey_3_auth_denied_stale_list_refresh_apply_and_mid_install_stop_recovery`, `gtk_widget_window_rows_and_dialogs_e2e` | **Orchestrator (#23), Layer A pipe E2E & Layer B `build_confirm_dialog`/`handle_worker_with` GTK E2E (#24) complete.** |
| 6 | **Polkit auth denied / cancelled** | Helper exits with empty stdout (`SessionError::AuthDenied`) or user clicks `Cancel` in-app | **Check polkit denied:** `Fail { retry: CheckAgain }`. **Apply polkit denied:** `Fail { retry: RetryApply }` preserves Ready list. **In-app Cancel:** `CancelAuth` restores `Ready` (if Apply) or `Idle` (Check) | **Polkit denied:** `Failed` (resumes `Ready` packages via `Retry Update`). **In-app Cancel:** restores `Ready` (on Apply) or `Idle(Current/Fresh)` (on Check) | **Apply denied:** `"Authentication cancelled"` + **`"Retry Update"`**. **In-app Cancel:** seamless return to Ready list or Idle | `in_app_cancel_auth_returns_idle`, `cancel_auth_during_apply_restores_ready_list`, `apply_auth_denied_preserves_packages_for_retry_apply`, `check_polkit_auth_denied_and_helper_spawn_error_recover_on_retry`, `apply_empty_package_list_and_polkit_denied_retry_apply`, `e2e_journey_3_auth_denied_stale_list_refresh_apply_and_mid_install_stop_recovery`, `gtk_widget_window_rows_and_dialogs_e2e` | **Resolved in [#27](https://github.com/rkalla/fedora-updater/issues/27); functional (#23) + Layer A pipe E2E & Layer B GTK E2E (#24) complete.** |
| 7 | **Stop before install (`Download`) vs Stop mid-install (`Apply`)** | `tests/fixtures/dnf/apply_happy_kernel.txt` (Stop during `[2/4] firefox` vs during `Installing kernel-core`) | UI checks `running.is_applying()`. If Download: immediate `stop_apply(false)` (`cancel_background_work` + `kill_recorded_helper` + `kill_recorded_local_command`). If Apply: `prompt_stop_during_transaction` → `stop_apply(true)` | **Before install:** `Ready { packages (all Pending), soft_errors: ["Stopped before install..."] }`. **During install:** `Failed { title: "Update stopped during install", detail: "...Check dnf history...", retry: CheckAgain }` | **Download:** button says `"Stop"`, immediate return to Ready list with `ready_banner` showing `"Stopped before install. Downloaded packages were not applied."`. **Apply:** destructive `"Stop…"`, AlertDialog `"Stop This Update?"`, Failed warns about `dnf history`; local `flatpak update` killed cleanly | `stop_before_install_returns_every_package_to_pending`, `stop_during_install_warns_that_the_rpm_database_may_be_half_applied`, `stop_before_install_vs_stop_during_rpm_apply`, `kill_recorded_local_command_terminates_running_local_process`, `e2e_journey_3_auth_denied_stale_list_refresh_apply_and_mid_install_stop_recovery`, `gtk_widget_window_rows_and_dialogs_e2e` | **Local Flatpak kill (#27), functional (#23), `ready_banner_title` UI bug fix, Layer A real helper kill E2E, and Layer B AlertDialog/banner GTK E2E (#24) all resolved.** |
| 8 | **Flatpak related extension (NVIDIA) & partial/remote failure** | `tests/fixtures/flatpak/check_system_nvidia_extension.json`, `apply_nothing_to_update.txt`, `apply_remote_error.txt` | **Check:** `libflatpak` lists parent refs (even on current commit) + resolved `Install`/`Uninstall` ops → `packages_from_flatpak_updates` names ext on parent rows and counts shared ext bytes once. **Apply:** `flatpak update --user/--system -y --noninteractive <refs...>`; exit 0 no-op OK, non-zero remote error fails source | `Ready` shows `"installs org.freedesktop.Platform.GL..."` in row subtitle & accurate rolled-up byte size; apply remote failure marks Flatpak rows `Failed` | Source badge `FP`; detail shows version + origin + scope + extension list (`subtitle_lines == 2`); remote failure summarized inline via `Apps: Failed to update...` + console | `related_extension_is_named_on_the_parent_row`, `shared_extension_download_is_counted_once`, `check_full_four_sources_happy_path`, `apply_flatpak_only_never_starts_privileged_helper_session`, `apply_flatpak_user_failure_while_dnf_and_flatpak_system_succeed`, `e2e_journey_1_happy_path_check_apply_and_reboot_inhibitor_recovery`, `gtk_widget_window_rows_and_dialogs_e2e` | **Parser (#21), functional (#23), and live GTK `rows::package_row` 2-line wrapping E2E (#24) complete.** |
| 9 | **fwupd AC-power/battery error, no updates (exit 2), corrupt/text fallback, reboot-needed** | `tests/fixtures/fwupd/*.json` & `*.txt` | **Check:** `CheckFwupdRefresh` (ignored error) → `CheckFwupd` (`fwupdmgr get-updates --json`). Prefers JSON; text fallback. Exit 2 no-updates / empty-JSON treated as OK. **Apply:** `ApplyFwupd` (`-y --no-reboot-check --no-unreported-check --no-device-prompt`); **Post-apply:** `CheckFwupdReboot` (exit 0 = reboot) | JSON/text devices → `Firmware` rows; battery error exit non-zero → `Failed` / `Done` with `Firmware: Cannot update firmware while on battery power...` inline; `CheckFwupdReboot` exit 0 → `needs_reboot = true` | `FW` badge; `old → new` version subtitle; `Successfully installed firmware` finishes open FW row; `needs_reboot` reveals `reboot_banner` `"Restart Now"` | `check_output_is_no_updates_handles_exit_2_and_errors`, `check_empty_up_to_date_and_fwupd_exit_2_no_updates`, `check_all_sources_fail_or_fwupd_corrupt_surfaces_warning_idle`, `apply_firmware_only_uses_check_fwupd_reboot_for_reboot_flag`, `apply_total_failures_surface_actionable_reasons_and_preserve_console`, `e2e_journey_1_happy_path_check_apply_and_reboot_inhibitor_recovery` | **Battery UX (#27), text/progress (#21), `orchestrator.rs` exit-2 bug (#23), and Layer A real helper pipe E2E (#24) all resolved.** |
| 10 | **`systemctl reboot` block inhibitor & logind error** | `tests/fixtures/systemctl/reboot_block_inhibitor.txt`, `reboot_logind_error.txt` | User clicks `"Restart Now"` (`reboot_banner`) → toast `"Rebooting…"` → `run_systemctl_reboot` spawns `systemctl reboot`. Non-zero exit maps via `reboot_failure_detail` → `Fail { title: "Reboot failed", retry: RetryAction::Reboot }` | Transitions `Done` → `Failed { retry: RetryAction::Reboot }` | Fail `StatusPage` title `"Reboot failed"`, detail explains `"A process is still blocking shutdown... Try again in a few seconds."` (or raw logind error), Retry button labeled **`"Restart"`** (not `"Check Again"`) | `block_inhibitor_explains_try_again`, `other_systemctl_output_is_shown`, `empty_output_falls_back_to_exit_code`, `retry_on_reboot_failed_retries_reboot`, `systemctl_reboot_inhibitor_logind_spawn_error_and_retry_recovery`, `e2e_journey_1_happy_path_check_apply_and_reboot_inhibitor_recovery`, `gtk_widget_window_rows_and_dialogs_e2e` | **Orchestrator (#23), Layer A E2E & Layer B GTK `Failed(Reboot)` / `reboot_banner` verification (#24) complete.** |

---

## 4. Prioritized Action Map for #21–#25

- **#21 (Parsers, Console, Model, Presenter, Release Upgrade Unit Hardening — Completed):** Fixed `flatpak::parse_progress_line` ref prefix / `error:` parsing, `flatpak::packages_from_flatpak_updates` duplicate-ref first-index attribution, `fwupd::parse_text_updates` `"no available firmware updates"` / corrupt JSON fallback & `parse_progress_line` error ignore, and `dnf::parse_progress_line` `Transaction Summary:` count lines & `Cleanup / Verifying` `"/"` token extraction; added `tests/unit_fixtures.rs` consuming all 29 domain fixtures in `tests/fixtures/{dnf,flatpak,fwupd,release_upgrade}/`; expanded unit coverage across `backend/{mod,dnf,flatpak,fwupd}`, `release_upgrade`, `console`, `model`, and `presenter` (consolidating `check_progress_groups_flatpak`). Now at **183 offline tests + 1 `#[ignore]`**.
- **#22 (Privilege Boundary & Helper Protocol/Mode Hardening — Completed):** Enforced absolute `/usr/bin/{dnf,fwupdmgr}` paths, unique IDs, exact argvs, and shell-metacharacter checks across all 6 `HelperCommand` variants plus negative `__END__` & malformed `ClientRequest` bounds in `helper_protocol` and `tests/model_and_protocol.rs`; hardened `helper_mode` (`StreamSplitter` split UTF-8 & live `\r`, `emit_piece`, real `spawn_unbuffered`, >128 KiB concurrent `drain_child`, plus real `--helper` extra-arg exit 2 & malformed request E2E in `tests/helper_session.rs`); added process-group cleanup on `PrivilegedSession::start_with` handshake errors; added serialized `privilege/session.rs` and `privilege/pool.rs` unit & public-API tests covering reuse, dead-helper respawn, `Closed`/`Io`/`AuthDenied` eviction, mutex poison recovery, `run_local_command`, and grandchild termination on hung `close_session()`; consolidated redundant `only_allowlisted_argv` and `helper_flag_is_stable`. Now at **201 offline tests + 1 `#[ignore]`**.
- **#23 (Orchestrator Runtime Seam & Functional Flow Suite — Completed):** Introduced `OrchestratorRuntime` trait, `SystemRuntime`, `WorkCancel`, `run_*_with`, and synchronous `execute_*` entry points in `src/orchestrator.rs` without altering public `run_check_all`/`run_apply_all`/`run_systemctl_reboot` signatures; fixed `fwupd::check_output_is_no_updates` exit-2 `"Devices with no available firmware updates:"` / empty-JSON false error and stopped failed apply (`code != 0`) from falsely triggering `needs_reboot`; built `FakeRuntime` (`tests/common/runtime.rs`) + 18 offline functional flow tests (`tests/functional_flows.rs`) covering all Section 3 scenarios (consuming all 31 checked-in fixtures across all domains, including `systemctl/*`); strengthened `illegal_transitions` (all 7 phases), `dnf_failure_does_not_block_done_if_others_ok`, `done_resets_to_current_idle`, and `state::reduce` invariants in `tests/state_machine_exhaustive.rs` (49 tests). Now at **218 offline tests + 1 `#[ignore]`**.
- **#24 (End-to-End Pipeline + Real-Helper + Headless GTK4/Libadwaita UX Suite — Completed):** Built Layer A (`tests/common/helper_e2e.rs` + `tests/e2e_pipeline.rs`, 5 tests) executing full multi-stage user journeys (happy 4-source check/apply/reboot-inhibitor recovery, empty-then-updates partial conflict failure recovery, auth denial + >24h stale auto-refresh + mid-install helper `SIGKILL` Stop recovery, and mid-apply helper crash EOF session eviction/respawn) over real OS pipes via `PrivilegedSession::start_direct`; fixed `ready_banner_title` pre-install Stop copy (`src/presenter.rs`), added empty-list `ready_update` sensitivity guard and full `builder_ids_in_source` template verification (`src/ui/{mod,window}.rs`); and built Layer B single-thread serialized GTK4/Libadwaita E2E (`ui::window::tests::gtk_widget_window_rows_and_dialogs_e2e`, plus expanded `ui::preview::tests`) with `scripts/test-ui-headless.sh` (`gtk4-broadwayd` + `GDK_BACKEND=broadway` + `FEDORA_UPDATER_REQUIRE_GTK=1`, zero desktop windows presented). Now at **223 offline tests + 1 `#[ignore]`**.
- **#25 (Regression Sensitivity & Program Closure):** Execute deliberate mutation/regression checks across parsers, state machine, helper allowlist, and UI widgets to verify every tier catches real breakage, eliminate flakes, verify `< 5s` offline runtime, sync `README.md`, and close #19.
- **UX Enhancement Issues (#26, #27):** Implemented cleanly ahead of `#21` (`IdleScreen::Warning`, `idle_log`, `RetryAction::RetryApply`, `summarize_apply_failure`, `done_problem_banner`, `done_retry_btn`, `kill_recorded_local_command`).
