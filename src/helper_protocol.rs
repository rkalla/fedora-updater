//! Wire protocol between the GUI and `fedora-updater-helper`.
//!
//! Text-based, line-oriented, easy to test without polkit:
//!
//! ```text
//! → RUN check-dnf
//! ← (raw command stdout/stderr lines)
//! ← __END__ <exit_code>
//! → QUIT
//! ← __BYE__
//! ```

/// Marker line ending a command's output stream.
pub const END_PREFIX: &str = "__END__ ";
pub const BYE_LINE: &str = "__BYE__";
pub const HELLO_LINE: &str = "__READY__ fedora-updater-helper";
/// A carriage-return progress update. The GUI replaces the open console line.
pub const DRAW_PREFIX: &str = "__DRAW__ ";

/// One helper stdout record after the GUI has split protocol markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperOutput {
    /// Newline-terminated output.
    Line(String),
    /// In-progress `\r` update. Not part of the command log.
    Draw(String),
}

/// Separate a live progress record from a committed line.
pub fn split_helper_line(line: &str) -> HelperOutput {
    match line.strip_prefix(DRAW_PREFIX) {
        Some(text) => HelperOutput::Draw(text.to_string()),
        None => HelperOutput::Line(line.to_string()),
    }
}

/// Allowlisted privileged operations the helper will execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HelperCommand {
    CheckDnf,
    ApplyDnf,
    CheckFwupdRefresh,
    CheckFwupd,
    ApplyFwupd,
    CheckFwupdReboot,
}

impl HelperCommand {
    pub fn as_id(self) -> &'static str {
        match self {
            Self::CheckDnf => "check-dnf",
            Self::ApplyDnf => "apply-dnf",
            Self::CheckFwupdRefresh => "check-fwupd-refresh",
            Self::CheckFwupd => "check-fwupd",
            Self::ApplyFwupd => "apply-fwupd",
            Self::CheckFwupdReboot => "check-fwupd-reboot",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        match id.trim() {
            "check-dnf" => Some(Self::CheckDnf),
            "apply-dnf" => Some(Self::ApplyDnf),
            "check-fwupd-refresh" => Some(Self::CheckFwupdRefresh),
            "check-fwupd" => Some(Self::CheckFwupd),
            "apply-fwupd" => Some(Self::ApplyFwupd),
            "check-fwupd-reboot" => Some(Self::CheckFwupdReboot),
            _ => None,
        }
    }

    /// argv for the host tool. Never shell-interpolated.
    pub fn argv(self) -> &'static [&'static str] {
        match self {
            // --refresh forces repodata re-fetch (third-party repos like Brave)
            // so Check matches GNOME Software rather than a stale 48h cache.
            Self::CheckDnf => &["/usr/bin/dnf", "check-update", "--refresh"],
            Self::ApplyDnf => &["/usr/bin/dnf", "update", "-y"],
            Self::CheckFwupdRefresh => &["/usr/bin/fwupdmgr", "refresh"],
            Self::CheckFwupd => &["/usr/bin/fwupdmgr", "get-updates", "--json"],
            // --no-unreported-check / --no-device-prompt: post-update report and
            // device pickers block on stdin (the helper protocol pipe), same hang
            // as an interactive reboot prompt.
            Self::ApplyFwupd => &[
                "/usr/bin/fwupdmgr",
                "update",
                "-y",
                "--no-reboot-check",
                "--no-unreported-check",
                "--no-device-prompt",
            ],
            // --json disables the interactive "Restart now?" prompt. Without it
            // fwupdmgr waits on stdin after apply and the GUI never reaches Done.
            Self::CheckFwupdReboot => &["/usr/bin/fwupdmgr", "check-reboot-needed", "--json"],
        }
    }

    pub fn encode_run(self) -> String {
        format!("RUN {}", self.as_id())
    }
}

/// Parse a client → helper request line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRequest {
    Run(HelperCommand),
    Quit,
}

impl ClientRequest {
    pub fn parse(line: &str) -> Result<Self, String> {
        let line = line.trim();
        if line.is_empty() {
            return Err("empty request".into());
        }
        if line.eq_ignore_ascii_case("QUIT") {
            return Ok(Self::Quit);
        }
        if let Some(rest) = line.strip_prefix("RUN ") {
            let cmd = HelperCommand::parse(rest)
                .ok_or_else(|| format!("unknown or disallowed command: {rest}"))?;
            return Ok(Self::Run(cmd));
        }
        Err(format!("invalid request: {line}"))
    }
}

/// Parse a `__END__ <code>` trailer from the helper.
pub fn parse_end_line(line: &str) -> Option<i32> {
    let rest = line.strip_prefix(END_PREFIX)?;
    rest.trim().parse().ok()
}

pub fn format_end_line(code: i32) -> String {
    format!("{END_PREFIX}{code}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const ALL_COMMANDS: [HelperCommand; 6] = [
        HelperCommand::CheckDnf,
        HelperCommand::ApplyDnf,
        HelperCommand::CheckFwupdRefresh,
        HelperCommand::CheckFwupd,
        HelperCommand::ApplyFwupd,
        HelperCommand::CheckFwupdReboot,
    ];

    #[test]
    fn roundtrip_commands() {
        let mut seen_ids = HashSet::new();
        for cmd in ALL_COMMANDS {
            let id = cmd.as_id();
            assert!(seen_ids.insert(id), "duplicate command id: {id}");
            assert_eq!(HelperCommand::parse(id), Some(cmd));

            let req = ClientRequest::parse(&cmd.encode_run()).unwrap();
            assert_eq!(req, ClientRequest::Run(cmd));

            let argv = cmd.argv();
            assert!(!argv.is_empty(), "{cmd:?} argv must not be empty");
            assert!(
                argv[0].starts_with("/usr/bin/"),
                "{cmd:?} binary must be an absolute /usr/bin/* path: {}",
                argv[0]
            );
            assert!(
                argv[0] == "/usr/bin/dnf" || argv[0] == "/usr/bin/fwupdmgr",
                "{cmd:?} unexpected privileged binary: {}",
                argv[0]
            );
            assert!(
                !argv[0].contains(' ') && !argv[0].contains(".."),
                "{cmd:?} binary path must be clean: {}",
                argv[0]
            );

            match cmd {
                HelperCommand::CheckDnf => {
                    assert_eq!(argv, &["/usr/bin/dnf", "check-update", "--refresh"]);
                }
                HelperCommand::ApplyDnf => {
                    assert_eq!(argv, &["/usr/bin/dnf", "update", "-y"]);
                    assert!(!argv.contains(&"upgrade"));
                }
                HelperCommand::CheckFwupdRefresh => {
                    assert_eq!(argv, &["/usr/bin/fwupdmgr", "refresh"]);
                }
                HelperCommand::CheckFwupd => {
                    assert_eq!(argv, &["/usr/bin/fwupdmgr", "get-updates", "--json"]);
                }
                HelperCommand::ApplyFwupd => {
                    assert_eq!(
                        argv,
                        &[
                            "/usr/bin/fwupdmgr",
                            "update",
                            "-y",
                            "--no-reboot-check",
                            "--no-unreported-check",
                            "--no-device-prompt",
                        ]
                    );
                }
                HelperCommand::CheckFwupdReboot => {
                    assert_eq!(argv, &["/usr/bin/fwupdmgr", "check-reboot-needed", "--json"]);
                }
            }
        }
    }

    #[test]
    fn rejects_unknown_and_injection() {
        for bad in [
            "",
            "   \t\r\n",
            "RUN",
            "RUN ",
            "RUN\tcheck-dnf",
            "run check-dnf",
            "Run check-dnf",
            "EXEC check-dnf",
            "RUN rm -rf /",
            "RUN check-dnf; reboot",
            "RUN check-dnf && id",
            "RUN check-dnf | cat",
            "RUN $(id)",
            "RUN check-dnf --refresh",
            "RUN apply-dnf extra",
            "QUIT extra",
            "RUN ../bin/sh",
            "RUN /usr/bin/dnf",
            "RUN dnf update",
            "RUN apply-flatpak-system",
            "RUN check-dnf\nRUN apply-dnf",
            "RUN check-dnf\rQUIT",
            "RUN check-dnf\0",
            "QUIT\nRUN check-dnf",
        ] {
            assert!(
                ClientRequest::parse(bad).is_err(),
                "expected rejection for {bad:?}"
            );
        }

        assert!(HelperCommand::parse("apply-flatpak-system").is_none());
        assert!(HelperCommand::parse("check-dnf extra").is_none());

        for quit in ["QUIT", "quit", "Quit", "  QUIT \r\n"] {
            assert_eq!(
                ClientRequest::parse(quit),
                Ok(ClientRequest::Quit),
                "expected QUIT parse for {quit:?}"
            );
        }
        assert_eq!(
            ClientRequest::parse("RUN check-dnf\r\n"),
            Ok(ClientRequest::Run(HelperCommand::CheckDnf))
        );
        assert_eq!(
            ClientRequest::parse("RUN  check-dnf"),
            Ok(ClientRequest::Run(HelperCommand::CheckDnf))
        );
    }

    #[test]
    fn quit_and_end() {
        assert_eq!(ClientRequest::parse("QUIT").unwrap(), ClientRequest::Quit);
        for code in [0, 1, 2, 100, 127, -1, -15, i32::MIN, i32::MAX] {
            let encoded = format_end_line(code);
            assert_eq!(parse_end_line(&encoded), Some(code));
        }
        assert_eq!(parse_end_line("__END__ 100"), Some(100));
        assert_eq!(parse_end_line("__END__ 0"), Some(0));
        assert_eq!(parse_end_line("__END__  42  "), Some(42));

        for bad in [
            "__END__ abc",
            "__END__ 0 extra",
            "__END__ 1.5",
            "__END__ 99999999999999999999",
            "__END__ ",
            "__END__",
            "END 0",
            "__END_ 0",
            " __END__ 0",
            "not an end",
            "__DRAW__ [1/2] grok-bot 40%",
        ] {
            assert_eq!(parse_end_line(bad), None, "expected None for {bad:?}");
        }
    }

    #[test]
    fn draw_marker_is_not_a_committed_line() {
        assert_eq!(
            split_helper_line("__DRAW__ [26/26] grok-bot 40%"),
            HelperOutput::Draw("[26/26] grok-bot 40%".into())
        );
        assert_eq!(
            split_helper_line("__DRAW__ "),
            HelperOutput::Draw(String::new())
        );
        assert_eq!(
            split_helper_line("__DRAW__"),
            HelperOutput::Line("__DRAW__".into())
        );
        assert_eq!(
            split_helper_line("prefix __DRAW__ text"),
            HelperOutput::Line("prefix __DRAW__ text".into())
        );
        assert_eq!(
            split_helper_line("__DRAW__ __DRAW__ nested"),
            HelperOutput::Draw("__DRAW__ nested".into())
        );
        assert_eq!(split_helper_line(""), HelperOutput::Line(String::new()));
        assert_eq!(
            split_helper_line("[26/26] grok-bot 100%"),
            HelperOutput::Line("[26/26] grok-bot 100%".into())
        );
        assert_eq!(
            split_helper_line("__END__ 0"),
            HelperOutput::Line("__END__ 0".into())
        );
    }

    #[test]
    fn argv_never_uses_shell_metacharacters() {
        const FORBIDDEN: [char; 15] = [
            ';', '|', '&', '`', '$', '>', '<', '(', ')', '"', '\'', '\\', '\n', '\r', '\0',
        ];
        for cmd in ALL_COMMANDS {
            for arg in cmd.argv() {
                assert!(!arg.is_empty(), "{cmd:?} contains empty arg");
                assert_eq!(
                    arg.trim(),
                    *arg,
                    "{cmd:?} arg has leading/trailing whitespace: {arg:?}"
                );
                for ch in FORBIDDEN {
                    assert!(
                        !arg.contains(ch),
                        "{cmd:?} arg {arg:?} contains forbidden char {ch:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn check_fwupd_reboot_uses_json_so_it_cannot_prompt() {
        let argv = HelperCommand::CheckFwupdReboot.argv();
        assert_eq!(argv[0], "/usr/bin/fwupdmgr");
        assert!(argv.contains(&"check-reboot-needed"));
        assert!(
            argv.contains(&"--json"),
            "interactive check-reboot-needed prompts 'Restart now?' and deadlocks the helper"
        );
        assert!(
            !argv.contains(&"-y") && !argv.contains(&"--assume-yes"),
            "assume-yes would answer the reboot prompt and restart the machine"
        );
    }

    #[test]
    fn apply_fwupd_skips_post_update_prompts() {
        let argv = HelperCommand::ApplyFwupd.argv();
        assert!(argv.contains(&"-y"));
        assert!(argv.contains(&"--no-reboot-check"));
        assert!(
            argv.contains(&"--no-unreported-check"),
            "report-history prompt hangs the helper the same way reboot-check does"
        );
        assert!(argv.contains(&"--no-device-prompt"));
    }
}
