//! Fixture-backed unit and parser regression tests consuming `tests/fixtures/`.

mod common;

use common::fixtures::{read_fixture, read_fixture_value, read_flatpak_fixture};
use fedora_updater::backend::brew::{
    parse_outdated_json, parse_progress_line as parse_brew_progress, upgrade_names_from_packages,
};
use fedora_updater::backend::containers::{
    container_names_from_packages, filter_toolbox_duplicates, list_exit_is_empty,
    parse_distrobox_list, parse_progress_line as parse_container_progress, parse_toolbox_list,
};
use fedora_updater::backend::dnf::{
    apply_download_sizes, check_exit_is_hard_error, check_exit_is_ok, detect_reboot_needed,
    parse_check_update, parse_progress_line as parse_dnf_progress, parse_repoquery_sizes,
};
use fedora_updater::backend::flatpak::{
    packages_from_flatpak_updates, parse_progress_line as parse_flatpak_progress,
    update_args_for_refs, update_ref_from_package,
};
use fedora_updater::backend::fwupd::{
    detect_reboot_needed as fwupd_detect_reboot, parse_get_updates,
    parse_progress_line as parse_fwupd_progress, parse_reboot_needed_output,
};
use fedora_updater::backend::gnome_extensions::{
    extension_uuids_from_packages, interpret_install_reply, parse_extensions_json,
};
use fedora_updater::backend::rpm_ostree::{
    check_exit_is_hard_error as rpm_ostree_exit_is_hard_error,
    check_exit_is_no_updates as rpm_ostree_exit_is_no_updates,
    detect_reboot_needed as rpm_ostree_detect_reboot, parse_progress_line as parse_rpm_ostree_progress,
    parse_status_json, parse_upgrade_check,
};
use fedora_updater::backend::{detect_reboot, parse_progress, ProgressHint};
use fedora_updater::model::{total_download_bytes, Package, PackageStatus, UpdateSource};
use fedora_updater::release_upgrade::{
    days_since_active, newest_active_newer_than, offer_from_collections, os_release_version,
    ReleaseOffer,
};

#[test]
fn dnf_fixtures_cover_check_repoquery_apply_and_errors() {
    // 1. Classic check-update + dedup + epochs + multi-arch + noarch
    let classic_raw = read_fixture("dnf/check_update_classic.txt");
    let mut classic = parse_check_update(&classic_raw);
    assert_eq!(classic.len(), 8);
    assert!(check_exit_is_ok(100, &classic));
    assert!(!check_exit_is_hard_error(100, &classic));
    assert_eq!(classic[2].name, "openssl");
    assert_eq!(classic[2].version, "1:3.2.2-3.fc44");
    assert_eq!(classic[3].name, "bind-libs");
    assert_eq!(classic[3].version, "32:9.18.50-2.fc44");
    assert_eq!(
        classic
            .iter()
            .filter(|p| p.name == "firefox")
            .count(),
        1,
        "duplicate firefox row in fixture must deduplicate"
    );
    assert_eq!(
        classic
            .iter()
            .filter(|p| p.name == "mesa-dri-drivers")
            .count(),
        2
    );

    // 2. Repoquery sizes matching & intentional partial misses (firefox, kernel-tools)
    let repoquery_raw = read_fixture("dnf/repoquery_sizes.txt");
    let sizes = parse_repoquery_sizes(&repoquery_raw);
    assert_eq!(sizes.len(), 6);
    let matched = apply_download_sizes(&mut classic, &sizes);
    assert_eq!(matched, 6);
    assert!(classic
        .iter()
        .find(|p| p.name == "firefox")
        .unwrap()
        .size_bytes
        .is_none());
    assert!(classic
        .iter()
        .find(|p| p.name == "kernel-tools")
        .unwrap()
        .size_bytes
        .is_none());

    // 3. dnf5 table check-update with Obsoleting packages & inline MiB/KiB sizes
    let dnf5_raw = read_fixture("dnf/check_update_dnf5_table.txt");
    let dnf5_pkgs = parse_check_update(&dnf5_raw);
    assert_eq!(dnf5_pkgs.len(), 6);
    assert!(dnf5_pkgs.iter().all(|p| p.size_bytes.is_some()));
    assert!(dnf5_pkgs.iter().any(|p| p.name == "pipewire-pulseaudio"));
    assert!(detect_reboot(UpdateSource::Dnf, "", &dnf5_pkgs));

    // 4. Empty check-update
    let empty_raw = read_fixture("dnf/check_update_empty.txt");
    let empty_pkgs = parse_check_update(&empty_raw);
    assert!(empty_pkgs.is_empty());
    assert!(check_exit_is_ok(0, &empty_pkgs));
    assert!(!check_exit_is_hard_error(0, &empty_pkgs));

    // 5. Repo/mirror error
    let repo_err_raw = read_fixture("dnf/check_update_repo_error.txt");
    let repo_err_pkgs = parse_check_update(&repo_err_raw);
    assert!(repo_err_pkgs.is_empty());
    assert!(check_exit_is_hard_error(1, &repo_err_pkgs));
    assert!(!check_exit_is_ok(1, &repo_err_pkgs));

    // 6. Happy kernel apply transcript with preview table, `\r` redraws, verify, scriptlet, Complete!
    let happy_raw = read_fixture("dnf/apply_happy_kernel.txt");
    assert!(happy_raw.contains('\r'));
    let segments: Vec<&str> = happy_raw
        .split(['\r', '\n'])
        .filter(|s| !s.is_empty())
        .collect();
    let hints: Vec<ProgressHint> = segments
        .iter()
        .map(|seg| parse_progress(UpdateSource::Dnf, seg))
        .collect();

    // Preview table + Transaction Summary counts before `[1/4]` must emit zero status/package hints.
    let first_download_pos = segments
        .iter()
        .position(|s| s.starts_with("[1/4]"))
        .expect("[1/4] present");
    for (idx, seg) in segments[..first_download_pos].iter().enumerate() {
        assert_eq!(
            hints[idx].status, None,
            "preview line `{seg}` must not set status"
        );
        assert_eq!(
            hints[idx].package_name, None,
            "preview line `{seg}` must not set package_name"
        );
    }

    assert!(hints
        .iter()
        .any(|h| h.status == Some(PackageStatus::Downloading)
            && h.package_name.as_deref() == Some("openssl")
            && h.stage_current == Some(1)
            && h.stage_total == Some(4)));
    assert!(hints
        .iter()
        .any(|h| h.status == Some(PackageStatus::Installing)
            && h.package_name.as_deref() == Some("kernel-core")
            && h.stage_current == Some(4)
            && h.stage_total == Some(8)));
    assert!(hints
        .iter()
        .any(|h| h.phase_label.as_deref() == Some("Verifying")
            && h.stage_current == Some(8)
            && h.package_name.is_none()));
    assert!(hints
        .iter()
        .any(|h| h.phase_label.as_deref() == Some("Complete") && h.progress == Some(1.0)));
    assert!(detect_reboot_needed(
        &happy_raw,
        &[classic[0].clone()], // kernel-core
    ));

    // 7 & 8. Dep conflict and RPM lock error produce no bogus install/reboot hints.
    for path in ["dnf/apply_dep_conflict.txt", "dnf/apply_rpm_lock_error.txt"] {
        let log = read_fixture(path);
        for line in log.lines() {
            let h = parse_dnf_progress(line);
            assert!(
                h.status.is_none() && h.package_name.is_none(),
                "error fixture `{path}` line `{line}` produced spurious hint {h:?}"
            );
        }
        assert!(!detect_reboot_needed(&log, &[]));
    }
}

#[test]
fn flatpak_fixtures_cover_user_nvidia_extensions_and_apply_logs() {
    // 1. User check fixture (includes runtime branch-as-version fallback)
    let user_fx = read_flatpak_fixture("flatpak/check_user_normal.json");
    let user_pkgs =
        packages_from_flatpak_updates(user_fx.update_source(), &user_fx.refs, &user_fx.ops);
    assert_eq!(user_pkgs.len(), 3);
    assert_eq!(
        total_download_bytes(&user_pkgs),
        Some(44_040_192 + 89_128_960 + 12_582_912)
    );
    assert_eq!(user_pkgs[2].version, "48");
    let user_refs: Vec<String> = user_pkgs
        .iter()
        .filter_map(update_ref_from_package)
        .collect();
    assert_eq!(user_refs.len(), 3);
    let user_args = update_args_for_refs(true, &user_refs);
    assert_eq!(&user_args[..4], &["update", "--user", "-y", "--noninteractive"]);
    assert_eq!(&user_args[4..], user_refs.as_slice());

    // 2. System NVIDIA extension fixture (related extension, shared ext once-only bytes, uninstall ignored)
    let sys_fx = read_flatpak_fixture("flatpak/check_system_nvidia_extension.json");
    let sys_pkgs =
        packages_from_flatpak_updates(sys_fx.update_source(), &sys_fx.refs, &sys_fx.ops);
    assert_eq!(sys_pkgs.len(), 3);
    assert_eq!(
        sys_pkgs[0].detail,
        "1.0.0.87 · flathub · system · installs org.freedesktop.Platform.GL32.nvidia-570-133-07"
    );
    assert_eq!(sys_pkgs[0].size_bytes, Some(251_000_000));
    assert_eq!(
        sys_pkgs[1].detail,
        "48 · flathub · system · installs org.freedesktop.Platform.GL.nvidia-570-133-07"
    );
    assert_eq!(sys_pkgs[1].size_bytes, Some(391_118_848));
    assert!(sys_pkgs[2]
        .detail
        .contains("installs org.freedesktop.Platform.GL.nvidia-570-133-07"));
    assert!(!sys_pkgs[2].detail.contains("nvidia-565-77"));
    assert_eq!(sys_pkgs[2].size_bytes, None);
    assert_eq!(
        total_download_bytes(&sys_pkgs),
        Some(251_000_000 + 391_118_848)
    );

    // 3. Apply progress happy (both bare IDs and `app/` prefixed refs)
    let happy_log = read_fixture("flatpak/apply_progress_happy.txt");
    let hints: Vec<ProgressHint> = happy_log
        .lines()
        .map(|l| parse_progress(UpdateSource::FlatpakSystem, l))
        .collect();
    assert_eq!(
        hints[1].package_name.as_deref(),
        Some("org.signal.Signal")
    );
    assert_eq!(hints[1].status, Some(PackageStatus::Downloading));
    assert_eq!(hints[1].progress, Some(0.45));
    assert_eq!(
        hints[4].package_name.as_deref(),
        Some("org.mozilla.Thunderbird")
    );
    assert_eq!(hints[4].status, Some(PackageStatus::Downloading));
    assert_eq!(hints[4].progress, Some(0.60));
    assert_eq!(
        hints[5].package_name.as_deref(),
        Some("org.mozilla.Thunderbird")
    );
    assert_eq!(hints[5].status, Some(PackageStatus::Installing));
    assert_eq!(hints[5].progress, Some(1.0));

    // 4. Nothing to update
    let noop_log = read_fixture("flatpak/apply_nothing_to_update.txt");
    let noop_hints: Vec<ProgressHint> = noop_log.lines().map(parse_flatpak_progress).collect();
    assert!(noop_hints
        .iter()
        .any(|h| h.phase_label.as_deref() == Some("Nothing to update")));
    assert!(noop_hints.iter().all(|h| h.status.is_none()));

    // 5. Remote error
    let err_log = read_fixture("flatpak/apply_remote_error.txt");
    for line in err_log.lines() {
        assert_eq!(parse_flatpak_progress(line), ProgressHint::default());
    }
    assert!(!detect_reboot(UpdateSource::FlatpakSystem, &happy_log, &sys_pkgs));
}

#[test]
fn fwupd_fixtures_cover_json_text_fallback_progress_and_reboot() {
    // 1. Multi-device JSON
    let multi_raw = read_fixture("fwupd/get_updates_multi_device.json");
    let multi = parse_get_updates(&multi_raw);
    assert_eq!(multi.len(), 3);
    assert_eq!(multi[0].name, "UEFI System Firmware");
    assert_eq!(multi[0].old_version.as_deref(), Some("1.18.0"));
    assert_eq!(multi[0].version, "1.19.2");
    assert_eq!(multi[0].detail, "1.18.0 → 1.19.2");
    assert_eq!(multi[1].name, "UEFI dbx");
    assert_eq!(multi[2].name, "WD_BLACK SN850X NVMe SSD");
    assert_eq!(multi[2].version, "620461WD");

    // 2. Empty JSON
    let empty_raw = read_fixture("fwupd/get_updates_empty.json");
    assert!(parse_get_updates(&empty_raw).is_empty());

    // 3. Corrupt JSON falls back cleanly to empty
    let corrupt_raw = read_fixture("fwupd/get_updates_corrupt.txt");
    assert!(parse_get_updates(&corrupt_raw).is_empty());

    // 4. Text updates (tree glyphs + ignores Bootloader Version)
    let text_raw = read_fixture("fwupd/get_updates_text.txt");
    let text_pkgs = parse_get_updates(&text_raw);
    assert_eq!(text_pkgs.len(), 2);
    assert_eq!(text_pkgs[0].name, "UEFI System Firmware");
    assert_eq!(text_pkgs[0].version, "1.19.2");
    assert_eq!(text_pkgs[1].name, "UEFI dbx");
    assert_eq!(text_pkgs[1].version, "20250501");

    // 5. No-updates bullet list returns empty (not phantom bullet packages)
    let no_updates_raw = read_fixture("fwupd/get_updates_no_updates.txt");
    assert!(parse_get_updates(&no_updates_raw).is_empty());

    // 6. Happy apply + reboot detection
    let happy_log = read_fixture("fwupd/apply_happy_reboot.txt");
    let hints: Vec<ProgressHint> = happy_log
        .lines()
        .map(|l| parse_progress(UpdateSource::Firmware, l))
        .collect();
    assert_eq!(hints[0].status, Some(PackageStatus::Downloading));
    assert_eq!(hints[1].phase_label.as_deref(), Some("Decompressing firmware"));
    assert_eq!(hints[2].status, Some(PackageStatus::Installing));
    assert_eq!(hints[2].progress, Some(0.60));
    assert_eq!(hints[3].phase_label.as_deref(), Some("Verifying firmware"));
    assert_eq!(hints[4].phase_label.as_deref(), Some("Restarting device"));
    assert_ne!(hints[4].status, Some(PackageStatus::Completed));
    assert_eq!(hints[5].phase_label.as_deref(), Some("Waiting"));
    assert_ne!(hints[5].status, Some(PackageStatus::Completed));
    assert_eq!(hints[6].status, Some(PackageStatus::Completed));
    assert_eq!(hints[6].progress, Some(1.0));
    assert_eq!(hints[7], ProgressHint::default());
    assert_eq!(hints[8], ProgressHint::default());
    assert!(fwupd_detect_reboot(&happy_log));
    assert!(detect_reboot(UpdateSource::Firmware, &happy_log, &[]));

    // 7. Battery error mid-run: error line does not emit 18% progress or reboot
    let battery_log = read_fixture("fwupd/apply_battery_error.txt");
    let battery_hints: Vec<ProgressHint> =
        battery_log.lines().map(parse_fwupd_progress).collect();
    assert_eq!(battery_hints[2], ProgressHint::default());
    assert!(!fwupd_detect_reboot(&battery_log));

    // 8 & 9. check-reboot-needed JSON fixtures
    let reboot_true = read_fixture("fwupd/check_reboot_needed_true.json");
    assert!(parse_reboot_needed_output(&reboot_true, 0));
    assert!(fwupd_detect_reboot(&reboot_true));

    let reboot_false = read_fixture("fwupd/check_reboot_needed_false.json");
    assert!(!parse_reboot_needed_output(&reboot_false, 2));
}

#[test]
fn release_upgrade_fixtures_cover_collections_and_os_release() {
    let os_quoted = read_fixture("release_upgrade/os_release_quoted.txt");
    let os_bare = read_fixture("release_upgrade/os_release_bare.txt");
    let os_missing = read_fixture("release_upgrade/os_release_missing_version.txt");
    let current = os_release_version(&os_quoted).expect("quoted VERSION_ID");
    assert_eq!(current, 44);
    assert_eq!(os_release_version(&os_bare), Some(44));
    assert_eq!(os_release_version(&os_missing), None);

    let f45_json = read_fixture("release_upgrade/collections_f45_active.json");
    assert_eq!(newest_active_newer_than(&f45_json, current), Some(45));
    assert_eq!(
        offer_from_collections(&f45_json, current, "2026-11-15"),
        Some(ReleaseOffer {
            version: 45,
            days_since_release: Some(12),
        })
    );

    let already_json = read_fixture("release_upgrade/collections_already_current.json");
    assert_eq!(offer_from_collections(&already_json, current, "2026-11-15"), None);
    assert_eq!(newest_active_newer_than(&already_json, 43), Some(44));

    let mixed_json = read_fixture("release_upgrade/collections_non_fedora_and_multi.json");
    assert_eq!(newest_active_newer_than(&mixed_json, current), Some(46));
    assert_eq!(days_since_active(&mixed_json, 45, "2026-11-20"), None);
    assert_eq!(
        offer_from_collections(&mixed_json, current, "2026-11-20"),
        Some(ReleaseOffer {
            version: 46,
            days_since_release: Some(2),
        })
    );

    let html_err = read_fixture("release_upgrade/collections_malformed.txt");
    assert_eq!(newest_active_newer_than(&html_err, current), None);
    assert_eq!(offer_from_collections(&html_err, current, "2026-11-20"), None);
}

#[test]
fn brew_fixtures_cover_outdated_pinned_malformed_and_upgrade_logs() {
    // 1. Outdated v2 formulae + multi-installed version + custom tap + cask
    let outdated = parse_outdated_json(&read_fixture("brew/outdated_v2.json"))
        .expect("outdated_v2.json parses");
    assert_eq!(outdated.len(), 4);
    assert!(outdated.iter().all(|p| p.source == UpdateSource::Homebrew));

    assert_eq!(outdated[0].name, "ripgrep");
    assert_eq!(outdated[0].old_version.as_deref(), Some("14.1.0"));
    assert_eq!(outdated[0].version, "14.1.1");
    assert_eq!(outdated[0].repo, "homebrew/core");

    assert_eq!(outdated[1].name, "cmake");
    assert_eq!(outdated[1].old_version.as_deref(), Some("3.30.2"));
    assert_eq!(outdated[1].version, "3.30.3");

    assert_eq!(outdated[2].name, "oven-sh/bun/bun");
    assert_eq!(outdated[2].repo, "oven-sh/bun");

    assert_eq!(outdated[3].name, "visual-studio-code");
    assert_eq!(outdated[3].repo, "homebrew/cask");

    assert_eq!(
        upgrade_names_from_packages(&outdated[..3]),
        vec![
            "ripgrep".to_string(),
            "cmake".to_string(),
            "oven-sh/bun/bun".to_string(),
        ]
    );

    // 2. Empty updates JSON
    let empty = parse_outdated_json(&read_fixture("brew/outdated_empty.json"))
        .expect("outdated_empty.json parses");
    assert!(empty.is_empty());

    // 3. Pinned items are skipped while unpinned entries remain
    let pinned = parse_outdated_json(&read_fixture("brew/outdated_pinned.json"))
        .expect("outdated_pinned.json parses");
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].name, "fd");
    assert_eq!(pinned[0].version, "10.2.0");

    // 4. Malformed JSON surfaces an error cleanly
    assert!(parse_outdated_json(&read_fixture("brew/outdated_malformed.txt")).is_err());

    // 5. Happy upgrade progress hints & reboot false invariant
    let upgrade_log = read_fixture("brew/upgrade_log.txt");
    let hints: Vec<ProgressHint> = upgrade_log
        .lines()
        .map(|line| parse_progress(UpdateSource::Homebrew, line))
        .collect();
    assert!(hints
        .iter()
        .any(|h| h.package_name.as_deref() == Some("ripgrep") && h.status == Some(PackageStatus::Downloading)));
    assert!(hints
        .iter()
        .any(|h| h.package_name.as_deref() == Some("ripgrep") && h.status == Some(PackageStatus::Installing)));
    assert!(hints.iter().any(|h| {
        h.package_name.as_deref() == Some("ripgrep") && h.status == Some(PackageStatus::Completed)
    }));
    assert!(hints.iter().any(|h| {
        h.package_name.as_deref() == Some("bun") && h.status == Some(PackageStatus::Completed)
    }));
    assert!(hints.iter().any(|h| {
        parse_brew_progress("==> Running `brew cleanup`...")
            .phase_label
            .as_deref()
            == h.phase_label.as_deref()
            && h.phase_label.as_deref() == Some("Cleanup")
    }));
    assert!(!detect_reboot(UpdateSource::Homebrew, &upgrade_log, &outdated));

    // 6. Partial error log marks completed item without fabricating progress for Error line
    let err_log = read_fixture("brew/upgrade_error.txt");
    let err_hints: Vec<ProgressHint> = err_log.lines().map(parse_brew_progress).collect();
    assert!(err_hints.iter().any(|h| {
        h.package_name.as_deref() == Some("ripgrep") && h.status == Some(PackageStatus::Completed)
    }));
    assert_eq!(err_hints.last(), Some(&ProgressHint::default()));
}

#[test]
fn gnome_extensions_fixtures_cover_filtering_metadata_and_install_replies() {
    // 1. Mixed extensions: skips system RPM extensions (even if hasUpdate=true) and up-to-date user items
    let mixed = parse_extensions_json(&read_fixture("gnome_extensions/list_extensions_mixed.json"))
        .expect("list_extensions_mixed.json parses");
    assert_eq!(mixed.len(), 3);
    assert_eq!(mixed[0].name, "Blur my Shell");
    assert_eq!(mixed[0].id, "gnome-ext:blur-my-shell@aunetx");
    assert_eq!(mixed[0].source, UpdateSource::GnomeExtensions);
    assert_eq!(mixed[0].old_version.as_deref(), Some("v47"));
    assert_eq!(mixed[0].version, "update available");
    assert_eq!(
        mixed[0].summary.as_deref(),
        Some("Adds a blur look to different parts of the GNOME Shell")
    );
    assert_eq!(
        mixed[0].url.as_deref(),
        Some("https://github.com/aunetx/blur-my-shell")
    );

    // Float version + distinct version-name formatting
    assert_eq!(mixed[1].name, "Caffeine");
    assert_eq!(mixed[1].old_version.as_deref(), Some("5.4.1 (v54)"));

    // Minimal entry omitting name, description, url, version
    assert_eq!(mixed[2].name, "minimal@example.org");
    assert!(mixed[2].old_version.is_none());
    assert!(mixed[2].summary.is_none());
    assert!(mixed[2].url.is_none());

    // 2. Empty/system-only listing produces zero updates
    let empty = parse_extensions_json(&read_fixture("gnome_extensions/list_extensions_empty.json"))
        .expect("list_extensions_empty.json parses");
    assert!(empty.is_empty());

    // 3. UUID extraction skips non-extension packages & progress/reboot helpers behave accurately
    let mut combined = vec![Package::new_dnf("kernel", "x86_64", "7.1.12", "updates")];
    combined.extend(mixed.clone());
    assert_eq!(
        extension_uuids_from_packages(&combined),
        vec![
            "blur-my-shell@aunetx",
            "caffeine@patapon.info",
            "minimal@example.org",
        ]
    );
    assert!(!detect_reboot(
        UpdateSource::GnomeExtensions,
        "Updated GNOME Shell extension blur-my-shell@aunetx",
        &mixed
    ));
    assert_eq!(
        parse_progress(
            UpdateSource::GnomeExtensions,
            "Updated GNOME Shell extension blur-my-shell@aunetx",
        ),
        ProgressHint::default()
    );
    assert!(mixed[0].matches_progress_name("blur-my-shell@aunetx"));

    // 4. Canonical InstallRemoteExtension replies fixture
    let replies = read_fixture_value("gnome_extensions/install_replies.json");
    for case in replies["replies"].as_array().expect("replies array") {
        let uuid = case["uuid"].as_str().expect("uuid");
        let ok = case["ok"].as_bool().expect("ok");
        let raw_reply = case["reply"].as_str().expect("reply").to_string();
        let reply = if ok { Ok(raw_reply) } else { Err(raw_reply) };
        let res = interpret_install_reply(uuid, reply);
        if case["expected_ok"].as_bool().expect("expected_ok") {
            assert!(res.is_ok(), "{uuid} should succeed");
        } else {
            let err = res.expect_err("reply should fail");
            let expected_sub = case["expected_contains"].as_str().expect("expected_contains");
            assert!(
                err.contains(expected_sub),
                "{uuid} error `{err}` should contain `{expected_sub}`"
            );
        }
    }
}

#[test]
fn rpm_ostree_fixtures_cover_status_diff_oci_progress_and_errors() {
    let status = parse_status_json(&read_fixture("rpm_ostree/status.json"))
        .expect("status.json parses");
    let booted = status.booted_deployment().expect("booted deployment");
    assert_eq!(booted.version.as_deref(), Some("44.20261120.0"));
    assert!(booted.is_layered_package("htop"));
    assert!(!booted.is_layered_package("kernel"));

    let diff_raw = read_fixture("rpm_ostree/upgrade_check_diff.txt");
    let diff_pkgs =
        parse_upgrade_check(&diff_raw, Some(&status)).expect("upgrade_check_diff.txt parses");
    assert_eq!(diff_pkgs.len(), 7);
    assert!(diff_pkgs
        .iter()
        .all(|p| p.source == UpdateSource::RpmOstree && !p.source.supports_selective_apply()));

    let kernel = diff_pkgs.iter().find(|p| p.name == "kernel").expect("kernel");
    assert_eq!(kernel.old_version.as_deref(), Some("6.14.10-300.fc44"));
    assert_eq!(kernel.version, "6.14.11-300.fc44");
    assert_eq!(kernel.arch, "x86_64");
    assert_eq!(kernel.advisories.len(), 1);
    assert_eq!(kernel.advisories[0].id, "FEDORA-2026-11a2bc");

    let htop = diff_pkgs.iter().find(|p| p.name == "htop").expect("htop");
    assert!(htop.detail.to_ascii_lowercase().contains("layered"), "{}", htop.detail);

    assert!(diff_pkgs.iter().any(|p| p.name == "fastfetch" && p.detail.contains("Added")));
    assert!(diff_pkgs.iter().any(|p| p.name == "neofetch" && p.detail.contains("Removed")));
    assert!(diff_pkgs.iter().any(|p| p.name == "curl" && p.detail.contains("Downgraded")));

    let none_raw = read_fixture("rpm_ostree/upgrade_check_none.txt");
    assert!(rpm_ostree_exit_is_no_updates(77, &none_raw));
    assert!(parse_upgrade_check(&none_raw, Some(&status))
        .expect("no-updates check parses cleanly")
        .is_empty());

    let oci_raw = read_fixture("rpm_ostree/upgrade_check_oci.txt");
    let oci_pkgs = parse_upgrade_check(&oci_raw, None).expect("upgrade_check_oci.txt parses");
    assert_eq!(oci_pkgs.len(), 1);
    assert_eq!(oci_pkgs[0].version, "44.20261124.0");
    assert!(oci_pkgs[0]
        .summary
        .as_deref()
        .unwrap_or("")
        .contains("sha256:9f86d081884c"));

    let apply_raw = read_fixture("rpm_ostree/upgrade_apply_log.txt");
    let recv_hint = parse_rpm_ostree_progress("Receiving objects: 42% (84/200) 8.5 MB/s");
    assert_eq!(recv_hint.status, Some(PackageStatus::Downloading));
    assert_eq!(recv_hint.progress, Some(0.42));
    let commit_hint = parse_progress(UpdateSource::RpmOstree, "Writing OSTree commit... done");
    assert_eq!(commit_hint.phase_label.as_deref(), Some("Writing OSTree commit"));
    let staged_hint = parse_rpm_ostree_progress("Staging deployment... done");
    assert_eq!(staged_hint.phase_label.as_deref(), Some("Deployment staged"));
    assert_eq!(staged_hint.progress, Some(1.0));
    assert!(rpm_ostree_detect_reboot(&apply_raw, &diff_pkgs));
    assert!(detect_reboot(UpdateSource::RpmOstree, &apply_raw, &diff_pkgs));
    assert!(!rpm_ostree_detect_reboot(&none_raw, &[]));

    let err_raw = read_fixture("rpm_ostree/upgrade_error.txt");
    assert!(rpm_ostree_exit_is_hard_error(1, &err_raw, &[]));
    assert!(!rpm_ostree_detect_reboot(&err_raw, &diff_pkgs));
}

#[test]
fn containers_fixtures_cover_list_dedup_empty_and_upgrade_logs() {
    let dbox_raw = read_fixture("containers/distrobox_list.txt");
    let dbox = parse_distrobox_list(&dbox_raw);
    assert_eq!(dbox.len(), 3);
    assert_eq!(dbox[0].name, "arch-dev");
    assert_eq!(dbox[0].status, "Up 3 hours");
    assert_eq!(
        dbox[0].image,
        "quay.io/toolbx-images/archlinux-toolbox:latest"
    );
    assert_eq!(dbox[1].name, "fedora-dev");
    assert_eq!(dbox[1].status, "Exited (0) 2 days ago");
    assert_eq!(dbox[2].name, "ubuntu-lts");

    let tbx_raw = read_fixture("containers/toolbox_list.txt");
    let tbx = parse_toolbox_list(&tbx_raw);
    assert_eq!(tbx.len(), 2);
    assert_eq!(tbx[0].name, "fedora-toolbox-43");
    assert_eq!(tbx[0].status, "running");
    assert_eq!(tbx[0].created.as_deref(), Some("3 days ago"));
    assert_eq!(tbx[1].name, "f44-rawhide");
    assert_eq!(tbx[1].status, "exited");

    let tbx_dup_raw = read_fixture("containers/toolbox_list_with_duplicates.txt");
    let tbx_dup = parse_toolbox_list(&tbx_dup_raw);
    assert_eq!(tbx_dup.len(), 3);
    let deduped = filter_toolbox_duplicates(tbx_dup, &dbox);
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].name, "fedora-toolbox-43");

    let empty_raw = read_fixture("containers/empty.txt");
    assert!(parse_distrobox_list(&empty_raw).is_empty());
    assert!(parse_toolbox_list(&empty_raw).is_empty());
    assert!(list_exit_is_empty(0, &empty_raw));

    let pkgs: Vec<Package> = dbox
        .iter()
        .cloned()
        .map(|info| info.into_package(UpdateSource::Distrobox))
        .chain(
            deduped
                .iter()
                .cloned()
                .map(|info| info.into_package(UpdateSource::Toolbx)),
        )
        .collect();
    assert_eq!(pkgs.len(), 4);
    assert!(pkgs.iter().all(|p| p.old_version.is_none()));
    assert_eq!(
        pkgs[0].detail,
        "Container · quay.io/toolbx-images/archlinux-toolbox:latest · check & update on apply"
    );
    assert_eq!(
        container_names_from_packages(&pkgs, UpdateSource::Distrobox),
        vec![
            ("distrobox:arch-dev".into(), "arch-dev".into()),
            ("distrobox:fedora-dev".into(), "fedora-dev".into()),
            ("distrobox:ubuntu-lts".into(), "ubuntu-lts".into()),
        ]
    );
    assert_eq!(
        container_names_from_packages(&pkgs, UpdateSource::Toolbx),
        vec![("toolbx:fedora-toolbox-43".into(), "fedora-toolbox-43".into())]
    );

    let dbox_log = read_fixture("containers/distrobox_upgrade_log.txt");
    let dbox_hints: Vec<_> = dbox_log.lines().map(parse_container_progress).collect();
    assert!(dbox_hints
        .iter()
        .any(|h| h.status == Some(PackageStatus::Installing)));
    assert!(dbox_hints
        .iter()
        .any(|h| h.phase_label.as_deref() == Some("Complete")));
    assert!(!detect_reboot(UpdateSource::Distrobox, &dbox_log, &pkgs));

    let tbx_log = read_fixture("containers/toolbox_upgrade_log.txt");
    let tbx_sync = parse_progress(UpdateSource::Toolbx, "[1/1] Upgrading curl-0:8.11.1-2.fc43.x86_64");
    assert_eq!(tbx_sync.status, Some(PackageStatus::Installing));
    assert!(!detect_reboot(UpdateSource::Toolbx, &tbx_log, &pkgs));

    let err_log = read_fixture("containers/upgrade_error.txt");
    assert!(!detect_reboot(UpdateSource::Distrobox, &err_log, &pkgs));
    assert!(!detect_reboot(UpdateSource::Toolbx, &err_log, &pkgs));
}
