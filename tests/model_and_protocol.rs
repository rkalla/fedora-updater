//! Extra hardening tests for model helpers and protocol edge cases.

use fedora_updater::helper_protocol::{
    format_end_line, parse_end_line, ClientRequest, HelperCommand,
};
use fedora_updater::model::{
    apply_progress_hint, count_by_source, finalize_source, overall_progress, sort_for_apply,
    sources_present, Package, PackageStatus, UpdateSource,
};

#[test]
fn apply_order_is_dnf_flatpak_firmware() {
    let mut pkgs = vec![
        Package {
            id: "fw".into(),
            name: "UEFI".into(),
            arch: String::new(),
            version: "1".into(),
            repo: "LVFS".into(),
            size: None,
            old_version: None,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: fedora_updater::AdvisoryKind::Security,
            source: UpdateSource::Firmware,
            detail: String::new(),
        },
        Package::new_dnf("kernel", "x86_64", "1", "updates"),
        Package {
            id: "fp".into(),
            name: "App".into(),
            arch: String::new(),
            version: "1".into(),
            repo: "flathub".into(),
            size: None,
            old_version: None,
            status: PackageStatus::Pending,
            progress: 0.0,
            kind: fedora_updater::AdvisoryKind::Unknown,
            source: UpdateSource::FlatpakUser,
            detail: String::new(),
        },
    ];
    sort_for_apply(&mut pkgs);
    assert_eq!(pkgs[0].source, UpdateSource::Dnf);
    assert_eq!(pkgs[1].source, UpdateSource::FlatpakUser);
    assert_eq!(pkgs[2].source, UpdateSource::Firmware);
    assert_eq!(
        sources_present(&pkgs),
        vec![
            UpdateSource::Dnf,
            UpdateSource::FlatpakUser,
            UpdateSource::Firmware
        ]
    );
    let (d, f, w) = count_by_source(&pkgs);
    assert_eq!((d, f, w), (1, 1, 1));
}

#[test]
fn finalize_and_progress_helpers() {
    let mut pkgs = vec![
        Package::new_dnf("a", "x86_64", "1", "u"),
        Package::new_dnf("b", "x86_64", "1", "u"),
    ];
    apply_progress_hint(
        &mut pkgs,
        UpdateSource::Dnf,
        Some("a"),
        Some(PackageStatus::Installing),
        Some(0.4),
    );
    assert_eq!(pkgs[0].status, PackageStatus::Installing);
    finalize_source(&mut pkgs, UpdateSource::Dnf, true);
    assert!(pkgs.iter().all(|p| p.status == PackageStatus::Completed));
    assert!((overall_progress(&pkgs) - 1.0).abs() < f64::EPSILON);

    finalize_source(&mut pkgs, UpdateSource::Dnf, false);
    // already done — leave completed
    assert!(pkgs.iter().all(|p| p.status == PackageStatus::Completed));
}

#[test]
fn protocol_rejects_path_traversal_and_shell() {
    for bad in [
        "RUN check-dnf && reboot",
        "RUN ../evil",
        "RUN check-dnf\nRUN apply-dnf",
        "EXEC check-dnf",
        "run check-dnf",
    ] {
        assert!(ClientRequest::parse(bad).is_err(), "should reject: {bad}");
    }
}

#[test]
fn end_line_roundtrip() {
    for code in [0, 1, 100, -1, 127] {
        let line = format_end_line(code);
        // negative codes still format; parse may fail for negative depending on parse
        if code >= 0 {
            assert_eq!(parse_end_line(&line), Some(code));
        }
    }
}

#[test]
fn every_helper_command_has_nonempty_safe_argv() {
    let all = [
        HelperCommand::CheckDnf,
        HelperCommand::ApplyDnf,
        HelperCommand::CheckFwupdRefresh,
        HelperCommand::CheckFwupd,
        HelperCommand::ApplyFwupd,
        HelperCommand::CheckFwupdReboot,
        HelperCommand::ApplyFlatpakSystem,
    ];
    for cmd in all {
        let argv = cmd.argv();
        assert!(!argv.is_empty());
        // First token is the program name — no absolute weirdness required, but no spaces
        assert!(!argv[0].contains(' '));
        assert_eq!(HelperCommand::parse(cmd.as_id()), Some(cmd));
    }
}

#[test]
fn dnf_apply_is_update_not_upgrade() {
    let argv = HelperCommand::ApplyDnf.argv();
    assert!(argv.contains(&"update"));
    assert!(!argv.contains(&"upgrade"));
    assert!(argv.contains(&"-y"));
}
