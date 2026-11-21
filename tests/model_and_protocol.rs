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
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source: UpdateSource::Firmware,
            detail: String::new(),
            summary: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        },
        Package::new_dnf("kernel", "x86_64", "1", "updates"),
        Package {
            id: "fp".into(),
            name: "App".into(),
            arch: String::new(),
            version: "1".into(),
            repo: "flathub".into(),
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source: UpdateSource::FlatpakUser,
            detail: String::new(),
            summary: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
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
        "RUN check-dnf\rQUIT",
        "RUN check-dnf\0",
        "RUN check-dnf --refresh",
        "   \t\n",
        "QUIT extra",
        "RUN",
        "EXEC check-dnf",
        "run check-dnf",
    ] {
        assert!(ClientRequest::parse(bad).is_err(), "should reject: {bad:?}");
    }
}

#[test]
fn end_line_roundtrip() {
    for code in [0, 1, 2, 100, 127, -1, -15, i32::MIN, i32::MAX] {
        let line = format_end_line(code);
        assert_eq!(parse_end_line(&line), Some(code));
    }
    for bad in [
        "__END__ abc",
        "__END__ 0 extra",
        "__END__ 99999999999999999999",
        "__END__",
        "END 0",
        "__END_ 0",
    ] {
        assert_eq!(parse_end_line(bad), None, "should reject: {bad:?}");
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
    ];
    for cmd in all {
        let argv = cmd.argv();
        assert!(!argv.is_empty());
        assert!(
            argv[0].starts_with("/usr/bin/"),
            "{cmd:?} must invoke an absolute /usr/bin/* binary, got {}",
            argv[0]
        );
        assert!(
            argv[0] == "/usr/bin/dnf" || argv[0] == "/usr/bin/fwupdmgr",
            "{cmd:?} must target /usr/bin/dnf or /usr/bin/fwupdmgr, got {}",
            argv[0]
        );
        for arg in argv {
            assert!(!arg.is_empty(), "{cmd:?} has empty arg");
            assert!(
                !arg.contains([';', '|', '&', '`', '$', '>', '<', '\n', '\r', '\0']),
                "{cmd:?} arg contains forbidden metacharacter: {arg:?}"
            );
        }
        assert_eq!(HelperCommand::parse(cmd.as_id()), Some(cmd));
        assert_eq!(
            ClientRequest::parse(&cmd.encode_run()),
            Ok(ClientRequest::Run(cmd))
        );
    }
}

#[test]
fn dnf_apply_is_update_not_upgrade() {
    let argv = HelperCommand::ApplyDnf.argv();
    assert!(argv.contains(&"update"));
    assert!(!argv.contains(&"upgrade"));
    assert!(argv.contains(&"-y"));
}
