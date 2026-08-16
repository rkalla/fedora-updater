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

/// Allowlisted privileged operations the helper will execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HelperCommand {
    CheckDnf,
    ApplyDnf,
    CheckFwupdRefresh,
    CheckFwupd,
    ApplyFwupd,
    CheckFwupdReboot,
    ApplyFlatpakSystem,
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
            Self::ApplyFlatpakSystem => "apply-flatpak-system",
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
            "apply-flatpak-system" => Some(Self::ApplyFlatpakSystem),
            _ => None,
        }
    }

    /// argv for the host tool. Never shell-interpolated.
    pub fn argv(self) -> &'static [&'static str] {
        match self {
            // --refresh forces repodata re-fetch (third-party repos like Brave)
            // so Check matches GNOME Software rather than a stale 48h cache.
            Self::CheckDnf => &["dnf", "check-update", "--refresh"],
            Self::ApplyDnf => &["dnf", "update", "-y"],
            Self::CheckFwupdRefresh => &["fwupdmgr", "refresh"],
            Self::CheckFwupd => &["fwupdmgr", "get-updates", "--json"],
            Self::ApplyFwupd => &["fwupdmgr", "update", "-y", "--no-reboot-check"],
            Self::CheckFwupdReboot => &["fwupdmgr", "check-reboot-needed"],
            Self::ApplyFlatpakSystem => {
                &["flatpak", "update", "--system", "-y", "--noninteractive"]
            }
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

    #[test]
    fn roundtrip_commands() {
        for cmd in [
            HelperCommand::CheckDnf,
            HelperCommand::ApplyDnf,
            HelperCommand::CheckFwupdRefresh,
            HelperCommand::CheckFwupd,
            HelperCommand::ApplyFwupd,
            HelperCommand::CheckFwupdReboot,
            HelperCommand::ApplyFlatpakSystem,
        ] {
            assert_eq!(HelperCommand::parse(cmd.as_id()), Some(cmd));
            let req = ClientRequest::parse(&cmd.encode_run()).unwrap();
            assert_eq!(req, ClientRequest::Run(cmd));
            assert!(!cmd.argv().is_empty());
        }
    }

    #[test]
    fn rejects_unknown_and_injection() {
        assert!(ClientRequest::parse("RUN rm -rf /").is_err());
        assert!(ClientRequest::parse("RUN check-dnf; reboot").is_err());
        assert!(ClientRequest::parse("RUN dnf update").is_err());
        assert!(ClientRequest::parse("").is_err());
    }

    #[test]
    fn quit_and_end() {
        assert_eq!(ClientRequest::parse("QUIT").unwrap(), ClientRequest::Quit);
        assert_eq!(parse_end_line("__END__ 100"), Some(100));
        assert_eq!(parse_end_line("__END__ 0"), Some(0));
        assert_eq!(parse_end_line("not an end"), None);
    }

    #[test]
    fn argv_never_uses_shell_metacharacters() {
        for cmd in [
            HelperCommand::CheckDnf,
            HelperCommand::ApplyDnf,
            HelperCommand::ApplyFlatpakSystem,
            HelperCommand::ApplyFwupd,
        ] {
            for arg in cmd.argv() {
                assert!(!arg.contains(';'));
                assert!(!arg.contains('|'));
                assert!(!arg.contains('&'));
                assert!(!arg.contains('`'));
                assert!(!arg.contains('$'));
            }
        }
    }
}
