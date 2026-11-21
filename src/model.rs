//! Shared application model (no GTK).

use std::fmt;
use std::time::Instant;

/// Where an update item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateSource {
    Dnf,
    FlatpakUser,
    FlatpakSystem,
    Firmware,
}

impl UpdateSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dnf => "System",
            Self::FlatpakUser => "Apps",
            Self::FlatpakSystem => "Apps",
            Self::Firmware => "Firmware",
        }
    }

    pub fn badge(self) -> &'static str {
        match self {
            Self::Dnf => "DNF",
            Self::FlatpakUser => "FP",
            Self::FlatpakSystem => "FP",
            Self::Firmware => "FW",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Self::Dnf => "src-dnf",
            Self::FlatpakUser | Self::FlatpakSystem => "src-flatpak",
            Self::Firmware => "src-firmware",
        }
    }

    pub fn console_tag(self) -> &'static str {
        match self {
            Self::Dnf => "dnf",
            Self::FlatpakUser => "flatpak-user",
            Self::FlatpakSystem => "flatpak-system",
            Self::Firmware => "fwupd",
        }
    }

    /// Apply order: DNF → Flatpak user → Flatpak system → Firmware.
    pub fn apply_order(self) -> u8 {
        match self {
            Self::Dnf => 0,
            Self::FlatpakUser => 1,
            Self::FlatpakSystem => 2,
            Self::Firmware => 3,
        }
    }
}

impl fmt::Display for UpdateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPurpose {
    Check,
    Apply,
}

impl fmt::Display for AuthPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthPurpose::Check => write!(f, "check for updates"),
            AuthPurpose::Apply => write!(f, "apply updates"),
        }
    }
}

/// Which idle page the window is showing. The presenter maps each variant to copy;
/// the view does not choose a screen by matching subtitle text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdleScreen {
    /// Never checked in this session.
    Fresh,
    /// A check finished with nothing to install, or the user left the Done page.
    /// `detail` is set only when a backend reported a soft error.
    Current {
        checked_at: String,
        detail: Option<String>,
    },
}

/// What the Failed page's Retry button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAction {
    CheckAgain,
    Reboot,
}

/// Download vs apply inside one backend source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPass {
    Download,
    Apply,
}

/// `[current/total]` for the active download or transaction.
/// Built only when both numbers are present and `total > 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageCount {
    pub current: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageStatus {
    Pending,
    Downloading,
    Installing,
    Completed,
    Failed,
}

impl PackageStatus {
    pub fn is_done(self) -> bool {
        matches!(self, PackageStatus::Completed | PackageStatus::Failed)
    }

    pub fn label(self) -> &'static str {
        match self {
            PackageStatus::Pending => "queued",
            PackageStatus::Downloading => "downloading",
            PackageStatus::Installing => "installing",
            PackageStatus::Completed => "done",
            PackageStatus::Failed => "failed",
        }
    }
}

/// One updatable unit (RPM, Flatpak ref, or firmware device).
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub arch: String,
    pub version: String,
    pub repo: String,
    pub size_bytes: Option<u64>,
    pub old_version: Option<String>,
    pub status: PackageStatus,
    pub source: UpdateSource,
    pub detail: String,
}

impl Package {
    pub fn new_dnf(
        name: impl Into<String>,
        arch: impl Into<String>,
        version: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        let name = name.into();
        let arch = arch.into();
        let version = version.into();
        let repo = repo.into();
        let id = format!("dnf:{name}.{arch}:{version}");
        Self {
            id,
            name: name.clone(),
            arch,
            version: version.clone(),
            repo: repo.clone(),
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source: UpdateSource::Dnf,
            detail: format!("{version} · {repo}"),
        }
    }

    pub fn size_label(&self) -> Option<String> {
        self.size_bytes.map(format_bytes)
    }

    pub fn version_line(&self) -> String {
        match &self.old_version {
            Some(old) => format!("{old} → {}", self.version),
            None => {
                if self.detail.is_empty() {
                    self.version.clone()
                } else {
                    self.detail.clone()
                }
            }
        }
    }

    /// True when a backend progress line named this package. Token-exact so
    /// `kernel` does not steal `kernel-core`.
    pub fn matches_progress_name(&self, hint: &str) -> bool {
        let hint = hint.trim();
        if hint.is_empty() {
            return false;
        }
        if self.name == hint || self.id == hint {
            return true;
        }
        if !self.arch.is_empty() && format!("{}.{}", self.name, self.arch) == hint {
            return true;
        }
        // Flatpak/fwupd ids: `flatpak:user:app/org.foo/x86_64/stable`, `fwupd:<device>`.
        self.id.split([':', '/']).any(|part| part == hint)
    }
}

#[derive(Debug, Clone)]
pub struct Stopwatch {
    start: Instant,
}

impl Stopwatch {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    pub fn elapsed_label(&self) -> String {
        let secs = self.elapsed_secs();
        let m = secs / 60;
        let s = secs % 60;
        if m == 0 {
            format!("{s}s")
        } else {
            format!("{m}m {s:02}s")
        }
    }
}

pub fn count_by_source(packages: &[Package]) -> (usize, usize, usize) {
    let mut dnf = 0;
    let mut flatpak = 0;
    let mut firmware = 0;
    for p in packages {
        match p.source {
            UpdateSource::Dnf => dnf += 1,
            UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem => flatpak += 1,
            UpdateSource::Firmware => firmware += 1,
        }
    }
    (dnf, flatpak, firmware)
}

/// Parse a package size from DNF/Flatpak display text or a raw byte count.
pub fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let lower = s.to_ascii_lowercase().replace(',', "");
    let split_at = lower.rfind(|c: char| c.is_ascii_digit() || c == '.')?;
    let num = lower[..=split_at].trim();
    let unit = lower[split_at + 1..].trim();
    let n: f64 = num.parse().ok()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    let mul = match unit {
        "" | "b" | "byte" | "bytes" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mul).round() as u64)
}

pub fn format_bytes(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < K * K {
        format_size_unit(b / K, "KB")
    } else if b < K * K * K {
        format_size_unit(b / (K * K), "MB")
    } else {
        format_size_unit(b / (K * K * K), "GB")
    }
}

fn format_size_unit(n: f64, unit: &str) -> String {
    if n >= 10.0 {
        format!("{n:.0} {unit}")
    } else {
        format!("{n:.1} {unit}")
    }
}

pub fn total_download_bytes(packages: &[Package]) -> Option<u64> {
    let mut total = 0u64;
    let mut any = false;
    for p in packages {
        if let Some(n) = p.size_bytes {
            total = total.saturating_add(n);
            any = true;
        }
    }
    any.then_some(total)
}

pub fn sort_for_apply(packages: &mut [Package]) {
    packages.sort_by_key(|p| (p.source.apply_order(), p.name.clone()));
}

pub fn sources_present(packages: &[Package]) -> Vec<UpdateSource> {
    let mut out = Vec::new();
    for src in [
        UpdateSource::Dnf,
        UpdateSource::FlatpakUser,
        UpdateSource::FlatpakSystem,
        UpdateSource::Firmware,
    ] {
        if packages.iter().any(|p| p.source == src) {
            out.push(src);
        }
    }
    out
}

/// Mark all items for a source as completed/failed based on exit code.
pub fn finalize_source(packages: &mut [Package], source: UpdateSource, ok: bool) {
    for p in packages.iter_mut().filter(|p| p.source == source) {
        if p.status.is_done() {
            continue;
        }
        p.status = if ok {
            PackageStatus::Completed
        } else {
            PackageStatus::Failed
        };
    }
}

/// Apply a progress hint. Unnamed lines must not poke the first remaining
/// package — except an explicit Completed status, which firmware uses for
/// "Successfully installed firmware" (no device name on that line).
///
/// Download name-chasing does not complete packages (DNF names every RPM
/// once per download, then again per install/verify). Installing name-switch
/// completes the previous *installing* item only. A hint with no status leaves
/// the package status untouched and returns `Some(index)`.
/// Already-done packages are ignored so verify passes cannot resurrect them.
pub fn apply_progress_hint(
    packages: &mut [Package],
    source: UpdateSource,
    package_hint: Option<&str>,
    status_hint: Option<PackageStatus>,
) -> Option<usize> {
    let Some(hint) = package_hint.map(str::trim).filter(|s| !s.is_empty()) else {
        if status_hint != Some(PackageStatus::Completed) {
            return None;
        }
        let idx = packages
            .iter()
            .position(|p| p.source == source && !p.status.is_done())?;
        packages[idx].status = PackageStatus::Completed;
        return Some(idx);
    };
    // Same RPM name can be i686 and x86_64. The first row may already be done
    // from the other arch; keep looking so the twin still updates.
    let index = packages
        .iter()
        .position(|p| p.source == source && !p.status.is_done() && p.matches_progress_name(hint))?;
    match status_hint {
        Some(PackageStatus::Downloading) => {
            for (i, p) in packages.iter_mut().enumerate() {
                if i != index && p.source == source && p.status == PackageStatus::Downloading {
                    p.status = PackageStatus::Pending;
                }
            }
            packages[index].status = PackageStatus::Downloading;
        }
        Some(PackageStatus::Installing) => {
            for (i, p) in packages.iter_mut().enumerate() {
                if i == index || p.source != source {
                    continue;
                }
                if p.status == PackageStatus::Installing {
                    p.status = PackageStatus::Completed;
                } else if p.status == PackageStatus::Downloading {
                    p.status = PackageStatus::Pending;
                }
            }
            packages[index].status = PackageStatus::Installing;
        }
        None => {}
        Some(status) => {
            packages[index].status = status;
        }
    }
    Some(index)
}

pub fn overall_progress(packages: &[Package]) -> f64 {
    if packages.is_empty() {
        return 0.0;
    }
    let done = packages.iter().filter(|p| p.status.is_done()).count();
    done as f64 / packages.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_progress_name_is_token_exact() {
        let kernel = Package::new_dnf("kernel", "x86_64", "1", "updates");
        let core = Package::new_dnf("kernel-core", "x86_64", "1", "updates");
        assert!(kernel.matches_progress_name("kernel"));
        assert!(kernel.matches_progress_name("kernel.x86_64"));
        assert!(!kernel.matches_progress_name("kernel-core"));
        assert!(core.matches_progress_name("kernel-core"));
        assert!(!core.matches_progress_name("kernel"));

        let fp = Package {
            id: "flatpak:user:app/org.mozilla.Thunderbird/x86_64/stable".into(),
            name: "Thunderbird".into(),
            arch: "x86_64".into(),
            version: "1".into(),
            repo: "flathub".into(),
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source: UpdateSource::FlatpakUser,
            detail: String::new(),
        };
        assert!(fp.matches_progress_name("Thunderbird"));
        assert!(fp.matches_progress_name("org.mozilla.Thunderbird"));
        assert!(!fp.matches_progress_name("Thunder"));
    }

    #[test]
    fn unnamed_progress_does_not_touch_packages() {
        let mut pkgs = vec![
            Package::new_dnf("abrt", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        let untouched = pkgs.clone();
        for status in [
            Some(PackageStatus::Pending),
            Some(PackageStatus::Downloading),
            Some(PackageStatus::Installing),
            Some(PackageStatus::Failed),
            None,
        ] {
            for hint in [None, Some(""), Some("   ")] {
                assert!(apply_progress_hint(&mut pkgs, UpdateSource::Dnf, hint, status).is_none());
            }
        }
        assert_eq!(pkgs, untouched);
    }

    fn firmware_pkg(name: &str) -> Package {
        Package {
            id: format!("fwupd:{name}"),
            name: name.into(),
            arch: String::new(),
            version: "1".into(),
            repo: "LVFS".into(),
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source: UpdateSource::Firmware,
            detail: String::new(),
        }
    }

    #[test]
    fn unnamed_completed_hint_finishes_first_open_row_for_source() {
        let dnf = Package::new_dnf("firefox", "x86_64", "1", "updates");
        let mut pkgs = vec![dnf, firmware_pkg("System Firmware"), firmware_pkg("UEFI CA")];
        pkgs[1].status = PackageStatus::Failed;

        let idx = apply_progress_hint(
            &mut pkgs,
            UpdateSource::Firmware,
            None,
            Some(PackageStatus::Completed),
        );
        assert_eq!(idx, Some(2));
        assert_eq!(pkgs[0].status, PackageStatus::Pending);
        assert_eq!(pkgs[1].status, PackageStatus::Failed);
        assert_eq!(pkgs[2].status, PackageStatus::Completed);

        assert!(apply_progress_hint(
            &mut pkgs,
            UpdateSource::Firmware,
            Some("   "),
            Some(PackageStatus::Completed),
        )
        .is_none());
    }

    #[test]
    fn second_arch_receives_progress_after_the_first_is_done() {
        let mut pkgs = vec![
            Package::new_dnf("mesa-dri-drivers", "i686", "1", "updates"),
            Package::new_dnf("mesa-dri-drivers", "x86_64", "1", "updates"),
        ];
        pkgs[0].status = PackageStatus::Completed;
        let idx = apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("mesa-dri-drivers"),
            Some(PackageStatus::Installing),
        );
        assert_eq!(idx, Some(1));
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Installing);
    }

    #[test]
    fn named_progress_completes_previous_live_item() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
        );
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("kernel"),
            Some(PackageStatus::Installing),
        );
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Installing);
    }

    #[test]
    fn statusless_hint_returns_index_without_changing_rows() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
        );
        let idx = apply_progress_hint(&mut pkgs, UpdateSource::Dnf, Some("kernel"), None);
        assert_eq!(idx, Some(1));
        assert_eq!(pkgs[0].status, PackageStatus::Installing);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
    }

    #[test]
    fn download_and_install_hints_isolate_sources_and_reset_siblings() {
        let mut flatpak = Package::new_dnf("Thunderbird", "", "128", "flathub");
        flatpak.source = UpdateSource::FlatpakUser;
        flatpak.status = PackageStatus::Downloading;

        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("wget", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
            flatpak,
        ];
        assert_eq!(
            apply_progress_hint(
                &mut pkgs,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Downloading),
            ),
            Some(0)
        );
        assert_eq!(pkgs[0].status, PackageStatus::Downloading);
        assert_eq!(pkgs[3].status, PackageStatus::Downloading);

        assert_eq!(
            apply_progress_hint(
                &mut pkgs,
                UpdateSource::Dnf,
                Some("wget"),
                Some(PackageStatus::Downloading),
            ),
            Some(1)
        );
        assert_eq!(pkgs[0].status, PackageStatus::Pending);
        assert_eq!(pkgs[1].status, PackageStatus::Downloading);
        assert_eq!(pkgs[3].status, PackageStatus::Downloading);

        assert_eq!(
            apply_progress_hint(
                &mut pkgs,
                UpdateSource::Dnf,
                Some("firefox"),
                Some(PackageStatus::Installing),
            ),
            Some(0)
        );
        assert_eq!(pkgs[0].status, PackageStatus::Installing);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
        assert_eq!(pkgs[3].status, PackageStatus::Downloading);

        assert_eq!(
            apply_progress_hint(
                &mut pkgs,
                UpdateSource::Dnf,
                Some("kernel"),
                Some(PackageStatus::Failed),
            ),
            Some(2)
        );
        assert_eq!(pkgs[2].status, PackageStatus::Failed);
    }

    #[test]
    fn done_package_is_not_resurrected_as_current() {
        let mut pkgs = vec![
            Package::new_dnf("firefox", "x86_64", "1", "updates"),
            Package::new_dnf("kernel", "x86_64", "1", "updates"),
        ];
        pkgs[0].status = PackageStatus::Completed;
        assert!(apply_progress_hint(
            &mut pkgs,
            UpdateSource::Dnf,
            Some("firefox"),
            Some(PackageStatus::Installing),
        )
        .is_none());
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Pending);
    }

    #[test]
    fn format_bytes_uses_compact_units() {
        assert_eq!(format_bytes(900), "900 B");
        assert_eq!(format_bytes(12 * 1024), "12 KB");
        assert_eq!(format_bytes(21727761), "21 MB");
        assert_eq!(format_bytes(1_288_490_189), "1.2 GB");
    }
}
