# Security Policy

## Supported Versions

Security fixes are applied to the `main` branch and included in subsequent releases.

| Version / Branch | Supported |
|---|---|
| `main` / latest release | Yes |
| Older commits / builds | Best effort — please update to `main` first |

## Privilege & Security Boundaries

Fedora Updater is designed around a strict separation between the unprivileged GUI and privileged package/firmware operations:

- The desktop GUI runs entirely as the unprivileged user.
- Update discovery and history queries via `dnf5daemon-server` D-Bus open sessions with empty config overrides (`{}`) and never request admin elevation.
- Native privileged transactions run through Polkit-authorized Fedora system services (`dnf5daemon-server`, `fwupd`, `systemd-logind`).
- When the CLI fallback (`fedora-updater --helper`) is launched via `pkexec`, it executes only fixed, hardcoded allowlisted command arrays for `/usr/bin/dnf` and `/usr/bin/fwupdmgr` defined in [`src/helper_protocol.rs`](src/helper_protocol.rs) and ignores environment overrides inside helper mode.

## Reporting a Vulnerability

Please **do not** report security vulnerabilities (especially privilege-escalation, Polkit bypass, helper command injection, or D-Bus policy issues) in public GitHub issues.

Instead, please report them privately:

1. Check whether [Private Vulnerability Reporting](https://github.com/rkalla/fedora-updater/security/advisories/new) is enabled on the repository and submit a confidential GitHub Security Advisory.
2. Include a clear description of the vulnerability, affected file(s) or command paths, reproduction steps, and potential impact.

We will acknowledge reports promptly, investigate, and coordinate a fix before public disclosure.
