//! Shared application model (no GTK).

use std::collections::BTreeMap;
use std::fmt;
use std::time::Instant;

/// Coarse navigation category grouping one or more [`UpdateSource`] backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceCategory {
    System,
    Apps,
    Extensions,
    DevAndContainers,
    Firmware,
}

impl SourceCategory {
    pub const ALL: &'static [SourceCategory] = &[
        Self::System,
        Self::Apps,
        Self::Extensions,
        Self::DevAndContainers,
        Self::Firmware,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Apps => "Apps",
            Self::Extensions => "Extensions",
            Self::DevAndContainers => "Dev & Containers",
            Self::Firmware => "Firmware",
        }
    }

    pub fn sidebar_label(self) -> &'static str {
        self.sidebar_label_for_host(false)
    }

    pub fn sidebar_label_for_host(self, ostree_booted: bool) -> &'static str {
        match self {
            Self::System if ostree_booted => "System · Atomic",
            Self::System => "System · DNF",
            Self::Apps => "Apps · Flatpak",
            Self::Extensions => "Extensions",
            Self::DevAndContainers => "Dev & Containers",
            Self::Firmware => "Firmware · LVFS",
        }
    }

    pub fn filter_key(self) -> &'static str {
        match self {
            Self::System => "source:system",
            Self::Apps => "source:apps",
            Self::Extensions => "source:extensions",
            Self::DevAndContainers => "source:dev-containers",
            Self::Firmware => "source:firmware",
        }
    }

    pub fn from_filter_key(key: &str) -> Option<Self> {
        match key {
            "source:system" => Some(Self::System),
            "source:apps" => Some(Self::Apps),
            "source:extensions" => Some(Self::Extensions),
            "source:dev-containers" => Some(Self::DevAndContainers),
            "source:firmware" => Some(Self::Firmware),
            _ => None,
        }
    }

    pub fn dot_css_class(self) -> &'static str {
        self.dot_css_class_for_host(false)
    }

    pub fn dot_css_class_for_host(self, ostree_booted: bool) -> &'static str {
        match self {
            Self::System if ostree_booted => "src-ostree",
            Self::System => "src-dnf",
            Self::Apps => "src-flatpak",
            Self::Extensions => "src-ext",
            Self::DevAndContainers => "src-container",
            Self::Firmware => "src-firmware",
        }
    }

    pub fn sort_order(self) -> u8 {
        match self {
            Self::System => 0,
            Self::Apps => 1,
            Self::Extensions => 2,
            Self::DevAndContainers => 3,
            Self::Firmware => 4,
        }
    }
}

impl fmt::Display for SourceCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Where an update item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateSource {
    Dnf,
    RpmOstree,
    FlatpakUser,
    FlatpakSystem,
    GnomeExtensions,
    Homebrew,
    Rustup,
    Mise,
    Distrobox,
    Toolbx,
    CustomHook,
    Firmware,
}

impl UpdateSource {
    pub const ALL: &'static [UpdateSource] = &[
        Self::Dnf,
        Self::RpmOstree,
        Self::FlatpakUser,
        Self::FlatpakSystem,
        Self::GnomeExtensions,
        Self::Homebrew,
        Self::Rustup,
        Self::Mise,
        Self::Distrobox,
        Self::Toolbx,
        Self::CustomHook,
        Self::Firmware,
    ];

    pub const ALL_CSS_CLASSES: &'static [&'static str] = &[
        "src-dnf",
        "src-flatpak",
        "src-firmware",
        "src-ostree",
        "src-brew",
        "src-ext",
        "src-container",
        "src-toolchain",
        "src-hook",
    ];

    pub fn all_sources() -> &'static [UpdateSource] {
        Self::ALL
    }

    pub fn category(self) -> SourceCategory {
        match self {
            Self::Dnf | Self::RpmOstree => SourceCategory::System,
            Self::FlatpakUser | Self::FlatpakSystem => SourceCategory::Apps,
            Self::GnomeExtensions => SourceCategory::Extensions,
            Self::Homebrew
            | Self::Rustup
            | Self::Mise
            | Self::Distrobox
            | Self::Toolbx
            | Self::CustomHook => SourceCategory::DevAndContainers,
            Self::Firmware => SourceCategory::Firmware,
        }
    }

    pub fn pref_key(self) -> &'static str {
        match self {
            Self::Dnf => "dnf",
            Self::RpmOstree => "rpm-ostree",
            Self::FlatpakUser | Self::FlatpakSystem => "flatpak",
            Self::GnomeExtensions => "gnome-extensions",
            Self::Homebrew => "homebrew",
            Self::Rustup => "rustup",
            Self::Mise => "mise",
            Self::Distrobox => "distrobox",
            Self::Toolbx => "toolbx",
            Self::CustomHook => "custom-hooks",
            Self::Firmware => "firmware",
        }
    }

    pub fn pref_title(self) -> &'static str {
        match self {
            Self::Dnf => "System Packages (DNF)",
            Self::RpmOstree => "System Image (rpm-ostree)",
            Self::FlatpakUser | Self::FlatpakSystem => "Flatpak Apps",
            Self::GnomeExtensions => "GNOME Shell Extensions",
            Self::Homebrew => "Homebrew (brew)",
            Self::Rustup => "Rust Toolchains (rustup)",
            Self::Mise => "Dev Runtimes (mise)",
            Self::Distrobox => "Distrobox Containers",
            Self::Toolbx => "Toolbx Containers",
            Self::CustomHook => "Custom User Hooks",
            Self::Firmware => "Firmware (fwupd / LVFS)",
        }
    }

    pub fn requires_root(self) -> bool {
        match self {
            Self::Dnf | Self::RpmOstree | Self::Firmware => true,
            Self::FlatpakUser
            | Self::FlatpakSystem
            | Self::GnomeExtensions
            | Self::Homebrew
            | Self::Rustup
            | Self::Mise
            | Self::Distrobox
            | Self::Toolbx
            | Self::CustomHook => false,
        }
    }

    /// True when individual items from this source can be selectively applied.
    /// Atomic `rpm-ostree` deployments apply together as a single unit.
    pub fn supports_selective_apply(self) -> bool {
        match self {
            Self::RpmOstree => false,
            Self::Dnf
            | Self::FlatpakUser
            | Self::FlatpakSystem
            | Self::GnomeExtensions
            | Self::Homebrew
            | Self::Rustup
            | Self::Mise
            | Self::Distrobox
            | Self::Toolbx
            | Self::CustomHook
            | Self::Firmware => true,
        }
    }

    /// True when this source applies to the current host OS mode (classic vs OSTree).
    pub fn is_applicable_for_host(self, ostree_booted: bool) -> bool {
        match self {
            Self::Dnf => !ostree_booted,
            Self::RpmOstree => ostree_booted,
            Self::FlatpakUser
            | Self::FlatpakSystem
            | Self::GnomeExtensions
            | Self::Homebrew
            | Self::Rustup
            | Self::Mise
            | Self::Distrobox
            | Self::Toolbx
            | Self::CustomHook
            | Self::Firmware => true,
        }
    }

    /// True for core Fedora sources expected on classic installations by default.
    pub fn installed_by_default(self) -> bool {
        self.installed_by_default_for_host(false)
    }

    /// True for core Fedora sources expected by default on the given host OS mode.
    pub fn installed_by_default_for_host(self, ostree_booted: bool) -> bool {
        self.is_applicable_for_host(ostree_booted)
            && !matches!(
                self,
                Self::GnomeExtensions
                    | Self::Homebrew
                    | Self::Rustup
                    | Self::Mise
                    | Self::Distrobox
                    | Self::Toolbx
                    | Self::CustomHook
            )
    }

    /// True when this source stays disabled until the user explicitly enables it in Preferences.
    pub fn is_opt_in(self) -> bool {
        matches!(self, Self::Distrobox | Self::Toolbx)
    }

    /// True when a normalized preference key matches any known [`UpdateSource`].
    pub fn is_known_pref_key(pref_key: &str) -> bool {
        let normalized = pref_key.trim().to_ascii_lowercase();
        Self::ALL.iter().any(|src| src.pref_key() == normalized)
    }

    /// True when a normalized preference key corresponds to an opt-in source.
    pub fn pref_key_is_opt_in(pref_key: &str) -> bool {
        let normalized = pref_key.trim().to_ascii_lowercase();
        Self::ALL
            .iter()
            .any(|src| src.pref_key() == normalized && src.is_opt_in())
    }

    /// Deduplicated preference-level sources for classic hosts preserving canonical order.
    pub fn pref_sources() -> Vec<UpdateSource> {
        Self::pref_sources_for_host(false)
    }

    /// Deduplicated preference-level sources for the given host OS mode.
    /// Custom hooks are configured individually in the Preferences "Custom Hooks"
    /// section rather than via a single host-binary switch.
    pub fn pref_sources_for_host(ostree_booted: bool) -> Vec<UpdateSource> {
        let mut out = Vec::new();
        for &src in Self::ALL {
            if !src.is_applicable_for_host(ostree_booted) || src == Self::CustomHook {
                continue;
            }
            if !out
                .iter()
                .any(|existing: &UpdateSource| existing.pref_key() == src.pref_key())
            {
                out.push(src);
            }
        }
        out
    }

    pub fn label(self) -> &'static str {
        self.category().label()
    }

    pub fn badge(self) -> &'static str {
        match self {
            Self::Dnf => "DNF",
            Self::RpmOstree => "OSTREE",
            Self::FlatpakUser => "FP",
            Self::FlatpakSystem => "FP",
            Self::GnomeExtensions => "EXT",
            Self::Homebrew => "BREW",
            Self::Rustup => "RUST",
            Self::Mise => "MISE",
            Self::Distrobox => "DBOX",
            Self::Toolbx => "TBX",
            Self::CustomHook => "HOOK",
            Self::Firmware => "FW",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Self::Dnf => "src-dnf",
            Self::RpmOstree => "src-ostree",
            Self::FlatpakUser | Self::FlatpakSystem => "src-flatpak",
            Self::GnomeExtensions => "src-ext",
            Self::Homebrew => "src-brew",
            Self::Rustup | Self::Mise => "src-toolchain",
            Self::Distrobox | Self::Toolbx => "src-container",
            Self::CustomHook => "src-hook",
            Self::Firmware => "src-firmware",
        }
    }

    pub fn console_tag(self) -> &'static str {
        match self {
            Self::Dnf => "dnf",
            Self::RpmOstree => "rpm-ostree",
            Self::FlatpakUser => "flatpak-user",
            Self::FlatpakSystem => "flatpak-system",
            Self::GnomeExtensions => "gnome-ext",
            Self::Homebrew => "brew",
            Self::Rustup => "rustup",
            Self::Mise => "mise",
            Self::Distrobox => "distrobox",
            Self::Toolbx => "toolbx",
            Self::CustomHook => "hook",
            Self::Firmware => "fwupd",
        }
    }

    /// Apply order: System (DNF / rpm-ostree) → Flatpak user → Flatpak system → GNOME Extensions → Homebrew → Toolchains (rustup, mise) → Containers (Distrobox, Toolbx) → Custom Hooks → Firmware.
    pub fn apply_order(self) -> u8 {
        match self {
            Self::Dnf | Self::RpmOstree => 0,
            Self::FlatpakUser => 1,
            Self::FlatpakSystem => 2,
            Self::GnomeExtensions => 3,
            Self::Homebrew => 4,
            Self::Rustup => 5,
            Self::Mise => 6,
            Self::Distrobox => 7,
            Self::Toolbx => 8,
            Self::CustomHook => 9,
            Self::Firmware => 10,
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
    /// A check finished with nothing to install, or the user left a clean Done page.
    Current {
        checked_at: String,
        detail: Option<String>,
    },
    /// Check failed on one or more sources (with 0 updates found), or the user
    /// left a Done screen that had failed updates. Never claims "System is up to date".
    Warning {
        title: &'static str,
        detail: String,
        checked_at: Option<String>,
    },
}

/// What the Failed page's Retry button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAction {
    CheckAgain,
    RetryApply,
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

/// Sub-view displayed in Observatory's detail pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailView {
    #[default]
    Updates,
    ActivityLog,
    /// Populated in Phase 2 (#35).
    History,
}

/// Execution mode for Phase 2 `Adw.SplitButton` apply options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApplyMode {
    #[default]
    Live,
    LiveAndReboot,
    OfflineOnReboot,
    DownloadOnly,
}

/// Advisory category attached in Phase 2 by DNF5/fwupd metadata engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvisoryKind {
    Security,
    Bugfix,
    Enhancement,
    NewPackage,
}

/// Structured advisory metadata (populated in Phase 2; optional in Phase 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageAdvisory {
    pub id: String,
    pub kind: AdvisoryKind,
    pub severity: Option<String>,
    pub cves: Vec<String>,
    pub bugzillas: Vec<String>,
    pub description: Option<String>,
}

/// One updatable unit (RPM, Flatpak ref, extension, Homebrew item, or firmware device).
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
    pub summary: Option<String>,
    pub url: Option<String>,
    pub install_size_delta: Option<i64>,
    pub advisories: Vec<PackageAdvisory>,
    pub preflight_warning: Option<String>,
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
            summary: None,
            url: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        }
    }

    /// Construct a user-installed GNOME Shell extension update item with `gnome-ext:<uuid>` id.
    pub fn new_gnome_extension(
        uuid: impl Into<String>,
        name: impl Into<String>,
        current_version: Option<String>,
        summary: Option<String>,
        url: Option<String>,
    ) -> Self {
        let uuid = uuid.into().trim().to_string();
        let raw_name = name.into();
        let trimmed_name = raw_name.trim();
        let display_name = if trimmed_name.is_empty() {
            uuid.clone()
        } else {
            trimmed_name.to_string()
        };
        let old_version = current_version
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let summary = summary
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let url = url
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let version = "update available".to_string();
        let id = format!("gnome-ext:{uuid}");
        let detail = match &old_version {
            Some(old) => format!("{old} → {version}"),
            None => format!("{version} · {uuid}"),
        };
        Self {
            id,
            name: display_name,
            arch: String::new(),
            version,
            repo: "extensions.gnome.org".into(),
            size_bytes: None,
            old_version,
            status: PackageStatus::Pending,
            source: UpdateSource::GnomeExtensions,
            detail,
            summary,
            url,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        }
    }

    /// Construct a Homebrew formula or cask update item with deterministic `brew:<full_name>:<version>` id.
    pub fn new_homebrew(
        name: impl Into<String>,
        old_version: Option<String>,
        version: impl Into<String>,
        tap: impl Into<String>,
        is_cask: bool,
    ) -> Self {
        let full_name = name.into().trim().to_string();
        let version = version.into().trim().to_string();
        let raw_tap = tap.into();
        let trimmed_tap = raw_tap.trim();
        let tap = if trimmed_tap.is_empty() {
            if is_cask {
                "homebrew/cask".to_string()
            } else {
                "homebrew/core".to_string()
            }
        } else {
            trimmed_tap.to_string()
        };
        let old_version = old_version
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let kind = if is_cask { "cask" } else { "formula" };
        let id = format!("brew:{full_name}:{version}");
        Self {
            id,
            name: full_name,
            arch: String::new(),
            version: version.clone(),
            repo: tap.clone(),
            size_bytes: None,
            old_version,
            status: PackageStatus::Pending,
            source: UpdateSource::Homebrew,
            detail: format!("{version} · {tap} · {kind}"),
            summary: None,
            url: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        }
    }

    /// Construct an Atomic Fedora (`rpm-ostree`) deployment package or image update item.
    #[allow(clippy::too_many_arguments)]
    pub fn new_rpm_ostree(
        kind: &str,
        name: impl Into<String>,
        arch: impl Into<String>,
        old_version: Option<String>,
        version: impl Into<String>,
        repo: impl Into<String>,
        detail: impl Into<String>,
        summary: Option<String>,
    ) -> Self {
        let name = name.into().trim().to_string();
        let arch = arch.into().trim().to_string();
        let version = version.into().trim().to_string();
        let raw_repo = repo.into();
        let repo = if raw_repo.trim().is_empty() {
            "fedora-ostree".to_string()
        } else {
            raw_repo.trim().to_string()
        };
        let old_version = old_version
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let kind_clean = kind.trim().to_ascii_lowercase();
        let id_ver = if version == "removed" {
            old_version.as_deref().unwrap_or("removed")
        } else {
            version.as_str()
        };
        let id = if arch.is_empty() {
            format!("rpm-ostree:{kind_clean}:{name}:{id_ver}")
        } else {
            format!("rpm-ostree:{kind_clean}:{name}.{arch}:{id_ver}")
        };
        Self {
            id,
            name,
            arch,
            version,
            repo,
            size_bytes: None,
            old_version,
            status: PackageStatus::Pending,
            source: UpdateSource::RpmOstree,
            detail: detail.into().trim().to_string(),
            summary: summary
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            url: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        }
    }

    /// Construct a rootless Distrobox or Toolbx container update item.
    pub fn new_container(
        source: UpdateSource,
        container_id: impl Into<String>,
        name: impl Into<String>,
        status: impl Into<String>,
        image: impl Into<String>,
        created: Option<String>,
    ) -> Self {
        let name = name.into().trim().to_string();
        let raw_status = status.into();
        let status_clean = raw_status.trim();
        let container_state = if status_clean.is_empty() {
            "unknown".to_string()
        } else {
            status_clean.to_string()
        };
        let raw_image = image.into();
        let image_clean = raw_image.trim();
        let base_image = if image_clean.is_empty() {
            "unknown".to_string()
        } else {
            image_clean.to_string()
        };
        let raw_id = container_id.into();
        let short_id: String = raw_id.trim().chars().take(12).collect();
        let created_clean = created
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let summary = match (short_id.is_empty(), created_clean) {
            (false, Some(created_at)) => {
                Some(format!("Rootless container {short_id} · Created {created_at}"))
            }
            (false, None) => Some(format!("Rootless container {short_id}")),
            (true, Some(created_at)) => Some(format!("Rootless container · Created {created_at}")),
            (true, None) => None,
        };
        let id = format!("{}:{name}", source.console_tag());
        Self {
            id,
            name,
            arch: String::new(),
            version: container_state,
            repo: base_image.clone(),
            size_bytes: None,
            old_version: None,
            status: PackageStatus::Pending,
            source,
            detail: format!("Container · {base_image} · check & update on apply"),
            summary,
            url: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        }
    }

    /// Construct a user-space developer toolchain update item (`rustup` or `mise`).
    #[allow(clippy::too_many_arguments)]
    pub fn new_toolchain(
        source: UpdateSource,
        id: impl Into<String>,
        name: impl Into<String>,
        old_version: Option<String>,
        version: impl Into<String>,
        repo: impl Into<String>,
        detail: impl Into<String>,
        summary: Option<String>,
        url: Option<String>,
    ) -> Self {
        let name = name.into().trim().to_string();
        let version = version.into().trim().to_string();
        let raw_repo = repo.into();
        let repo = if raw_repo.trim().is_empty() {
            source.console_tag().to_string()
        } else {
            raw_repo.trim().to_string()
        };
        let old_version = old_version
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let raw_detail = detail.into();
        let detail = if raw_detail.trim().is_empty() {
            format!("{version} · {repo}")
        } else {
            raw_detail.trim().to_string()
        };
        Self {
            id: id.into().trim().to_string(),
            name,
            arch: String::new(),
            version,
            repo,
            size_bytes: None,
            old_version,
            status: PackageStatus::Pending,
            source,
            detail,
            summary: summary
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            url: url
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        }
    }

    /// Related extensions extracted from Flatpak `detail` (`... · installs a, b`).
    pub fn related_extensions(&self) -> Vec<String> {
        let Some((_, rest)) = self.detail.split_once("installs ") else {
            return Vec::new();
        };
        let list_part = rest.split(" · ").next().unwrap_or(rest);
        list_part
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    /// Detail text without any trailing `installs ...` extension list.
    pub fn clean_detail_without_installs(&self) -> String {
        let trimmed = self.detail.trim();
        if let Some((prefix, _)) = trimmed.split_once(" · installs ") {
            return prefix.trim().to_string();
        }
        if trimmed.starts_with("installs ") {
            return String::new();
        }
        trimmed.to_string()
    }

    /// Primary security badge label (`CVE-...` or `"Security"`) when advisories include one.
    pub fn primary_security_tag(&self) -> Option<String> {
        for adv in self
            .advisories
            .iter()
            .filter(|a| a.kind == AdvisoryKind::Security)
        {
            if let Some(cve) = adv.cves.iter().map(|s| s.trim()).find(|s| !s.is_empty()) {
                return Some(cve.to_string());
            }
        }
        for adv in &self.advisories {
            if let Some(cve) = adv.cves.iter().map(|s| s.trim()).find(|s| !s.is_empty()) {
                return Some(cve.to_string());
            }
        }
        self.advisories
            .iter()
            .any(|a| a.kind == AdvisoryKind::Security)
            .then(|| "Security".to_string())
    }

    /// Searchable summary of attached advisory IDs and CVE identifiers.
    pub fn advisory_summary(&self) -> String {
        let mut parts = Vec::new();
        for adv in &self.advisories {
            let id = adv.id.trim();
            if !id.is_empty() {
                parts.push(id.to_string());
            }
            for cve in &adv.cves {
                let cve = cve.trim();
                if !cve.is_empty() {
                    parts.push(cve.to_string());
                }
            }
        }
        parts.join(" ")
    }

    /// True when at least one attached advisory marks a security fix or references a CVE.
    pub fn has_security_advisory(&self) -> bool {
        self.advisories
            .iter()
            .any(|a| a.kind == AdvisoryKind::Security || !a.cves.is_empty())
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
        if self.source == UpdateSource::GnomeExtensions {
            if let Some(uuid) = self.id.strip_prefix("gnome-ext:") {
                if hint == uuid {
                    return true;
                }
            }
        }
        if self.source == UpdateSource::Homebrew {
            if let Some(rest) = self.id.strip_prefix("brew:") {
                let full_name = rest.rsplit_once(':').map(|(n, _)| n).unwrap_or(rest);
                let short_name = full_name.rsplit('/').next().unwrap_or(full_name);
                return hint == full_name || hint == short_name;
            }
            let short_name = self.name.rsplit('/').next().unwrap_or(&self.name);
            return hint == short_name;
        }
        if self.source == UpdateSource::Rustup {
            return false;
        }
        if self.source == UpdateSource::Mise {
            return self
                .name
                .rsplit([':', '/'])
                .next()
                .is_some_and(|short| !short.is_empty() && short == hint);
        }
        if matches!(
            self.source,
            UpdateSource::Distrobox | UpdateSource::Toolbx | UpdateSource::CustomHook
        ) {
            return false;
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

pub fn count_by_category(packages: &[Package]) -> BTreeMap<SourceCategory, usize> {
    let mut counts = BTreeMap::new();
    for p in packages {
        *counts.entry(p.source.category()).or_insert(0) += 1;
    }
    counts
}

pub fn count_for_category(packages: &[Package], category: SourceCategory) -> usize {
    packages
        .iter()
        .filter(|p| p.source.category() == category)
        .count()
}

pub fn categories_present(packages: &[Package]) -> Vec<SourceCategory> {
    SourceCategory::ALL
        .iter()
        .copied()
        .filter(|cat| packages.iter().any(|p| p.source.category() == *cat))
        .collect()
}

pub fn count_by_source(packages: &[Package]) -> (usize, usize, usize) {
    let counts = count_by_category(packages);
    (
        counts.get(&SourceCategory::System).copied().unwrap_or(0),
        counts.get(&SourceCategory::Apps).copied().unwrap_or(0),
        counts.get(&SourceCategory::Firmware).copied().unwrap_or(0),
    )
}

/// Number of packages carrying at least one security advisory or CVE reference.
pub fn count_security(packages: &[Package]) -> usize {
    packages
        .iter()
        .filter(|p| p.has_security_advisory())
        .count()
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
    UpdateSource::ALL
        .iter()
        .copied()
        .filter(|src| packages.iter().any(|p| p.source == *src))
        .collect()
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
            summary: None,
            url: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
        };
        assert!(fp.matches_progress_name("Thunderbird"));
        assert!(fp.matches_progress_name("org.mozilla.Thunderbird"));
        assert!(!fp.matches_progress_name("Thunder"));

        let ext = Package::new_gnome_extension(
            "blur-my-shell@aunetx",
            "Blur my Shell",
            Some("v66".into()),
            Some("Adds blur to GNOME Shell".into()),
            Some("https://github.com/aunetx/blur-my-shell".into()),
        );
        assert!(ext.matches_progress_name("blur-my-shell@aunetx"));
        assert!(ext.matches_progress_name("Blur my Shell"));
        assert!(!ext.matches_progress_name("blur-my-shell"));

        let brew_tap = Package::new_homebrew(
            "oven-sh/bun/bun",
            Some("1.1.30".into()),
            "1.1.38",
            "oven-sh/bun",
            false,
        );
        assert!(brew_tap.matches_progress_name("oven-sh/bun/bun"));
        assert!(brew_tap.matches_progress_name("bun"));
        assert!(!brew_tap.matches_progress_name("oven-sh"));

        let dbox = Package::new_container(
            UpdateSource::Distrobox,
            "a1b2c3d4e5f6",
            "arch-dev",
            "Up 2 hours",
            "docker.io/library/archlinux:latest",
            None,
        );
        assert!(dbox.matches_progress_name("arch-dev"));
        assert!(dbox.matches_progress_name("distrobox:arch-dev"));
        assert!(!dbox.matches_progress_name("distrobox"));
        assert!(!dbox.matches_progress_name("arch"));

        let tbx = Package::new_container(
            UpdateSource::Toolbx,
            "0123456789ab",
            "fedora-toolbox-44",
            "exited",
            "registry.fedoraproject.org/fedora-toolbox:44",
            Some("2 weeks ago".into()),
        );
        assert!(tbx.matches_progress_name("fedora-toolbox-44"));
        assert!(tbx.matches_progress_name("toolbx:fedora-toolbox-44"));
        assert!(!tbx.matches_progress_name("toolbx"));

        let rust_tc = Package::new_toolchain(
            UpdateSource::Rustup,
            "rustup:stable-x86_64-unknown-linux-gnu",
            "stable-x86_64-unknown-linux-gnu",
            Some("1.86.0".into()),
            "1.87.0",
            "rustup",
            "",
            None,
            None,
        );
        assert!(rust_tc.matches_progress_name("stable-x86_64-unknown-linux-gnu"));
        assert!(rust_tc.matches_progress_name("rustup:stable-x86_64-unknown-linux-gnu"));
        assert!(!rust_tc.matches_progress_name("stable"));

        let mise_tool = Package::new_toolchain(
            UpdateSource::Mise,
            "mise:ubi:jdx/usage",
            "ubi:jdx/usage",
            Some("2.0.0".into()),
            "2.1.0",
            "mise",
            "",
            None,
            None,
        );
        assert!(mise_tool.matches_progress_name("ubi:jdx/usage"));
        assert!(mise_tool.matches_progress_name("usage"));
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
            summary: None,
            url: None,
            install_size_delta: None,
            advisories: Vec::new(),
            preflight_warning: None,
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
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(900), "900 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(9 * 1024 + 900), "9.9 KB");
        assert_eq!(format_bytes(10 * 1024), "10 KB");
        assert_eq!(format_bytes(12 * 1024), "12 KB");
        assert_eq!(format_bytes(1024 * 1024 - 1), "1024 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10 MB");
        assert_eq!(format_bytes(21_727_761), "21 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(format_bytes(1_288_490_189), "1.2 GB");
        assert_eq!(format_bytes(12 * 1024 * 1024 * 1024), "12 GB");
    }

    #[test]
    fn parse_size_bytes_covers_units_commas_and_invalid_inputs() {
        assert_eq!(parse_size_bytes("0"), Some(0));
        assert_eq!(parse_size_bytes("0 B"), Some(0));
        assert_eq!(parse_size_bytes("12345"), Some(12345));
        assert_eq!(parse_size_bytes("1,024"), Some(1024));
        assert_eq!(parse_size_bytes("1,024 KB"), Some(1024 * 1024));
        assert_eq!(parse_size_bytes("512 byte"), Some(512));
        assert_eq!(parse_size_bytes("512 bytes"), Some(512));
        assert_eq!(parse_size_bytes("1k"), Some(1024));
        assert_eq!(parse_size_bytes("412.0 KiB"), Some(412 * 1024));
        assert_eq!(parse_size_bytes("2.5 mb"), Some(2_621_440));
        assert_eq!(parse_size_bytes("112 MiB"), Some(112 * 1024 * 1024));
        assert_eq!(
            parse_size_bytes("1.5 GiB"),
            Some((1.5_f64 * 1024.0 * 1024.0 * 1024.0).round() as u64)
        );
        assert_eq!(
            parse_size_bytes("2 TB"),
            Some(2 * 1024 * 1024 * 1024 * 1024)
        );

        for invalid in [
            "",
            "   ",
            "-5 MB",
            "NaN MB",
            "inf MB",
            "12 XB",
            "MB",
            "1.2.3 MB",
            "abc",
        ] {
            assert_eq!(
                parse_size_bytes(invalid),
                None,
                "invalid size `{invalid}` must yield None"
            );
        }
    }

    #[test]
    fn total_download_bytes_and_package_version_line() {
        assert_eq!(total_download_bytes(&[]), None);

        let mut p1 = Package::new_dnf("firefox", "x86_64", "140.0", "updates");
        let mut p2 = Package::new_dnf("kernel", "x86_64", "7.1.12", "updates");
        assert_eq!(total_download_bytes(&[p1.clone(), p2.clone()]), None);

        p1.size_bytes = Some(0);
        assert_eq!(total_download_bytes(&[p1.clone(), p2.clone()]), Some(0));

        p2.size_bytes = Some(500);
        assert_eq!(total_download_bytes(&[p1.clone(), p2.clone()]), Some(500));

        // Saturation guard at u64::MAX.
        p1.size_bytes = Some(u64::MAX - 10);
        p2.size_bytes = Some(100);
        assert_eq!(total_download_bytes(&[p1.clone(), p2]), Some(u64::MAX));

        // Package::version_line branches
        assert_eq!(p1.version_line(), "140.0 · updates");
        p1.detail.clear();
        assert_eq!(p1.version_line(), "140.0");
        p1.old_version = Some("139.0".into());
        assert_eq!(p1.version_line(), "139.0 → 140.0");
    }

    #[test]
    fn enums_stopwatch_ordering_and_finalization_across_all_sources() {
        assert_eq!(UpdateSource::Dnf.label(), "System");
        assert_eq!(UpdateSource::FlatpakUser.label(), "Apps");
        assert_eq!(UpdateSource::FlatpakSystem.label(), "Apps");
        assert_eq!(UpdateSource::GnomeExtensions.label(), "Extensions");
        assert_eq!(UpdateSource::Homebrew.label(), "Dev & Containers");
        assert_eq!(UpdateSource::Rustup.label(), "Dev & Containers");
        assert_eq!(UpdateSource::Mise.label(), "Dev & Containers");
        assert_eq!(UpdateSource::Distrobox.label(), "Dev & Containers");
        assert_eq!(UpdateSource::Toolbx.label(), "Dev & Containers");
        assert_eq!(UpdateSource::CustomHook.label(), "Dev & Containers");
        assert_eq!(UpdateSource::Firmware.label(), "Firmware");

        assert_eq!(UpdateSource::Dnf.badge(), "DNF");
        assert_eq!(UpdateSource::FlatpakUser.badge(), "FP");
        assert_eq!(UpdateSource::FlatpakSystem.badge(), "FP");
        assert_eq!(UpdateSource::GnomeExtensions.badge(), "EXT");
        assert_eq!(UpdateSource::Homebrew.badge(), "BREW");
        assert_eq!(UpdateSource::Rustup.badge(), "RUST");
        assert_eq!(UpdateSource::Mise.badge(), "MISE");
        assert_eq!(UpdateSource::Distrobox.badge(), "DBOX");
        assert_eq!(UpdateSource::Toolbx.badge(), "TBX");
        assert_eq!(UpdateSource::CustomHook.badge(), "HOOK");
        assert_eq!(UpdateSource::Firmware.badge(), "FW");

        assert_eq!(UpdateSource::Dnf.css_class(), "src-dnf");
        assert_eq!(UpdateSource::FlatpakUser.css_class(), "src-flatpak");
        assert_eq!(UpdateSource::FlatpakSystem.css_class(), "src-flatpak");
        assert_eq!(UpdateSource::GnomeExtensions.css_class(), "src-ext");
        assert_eq!(UpdateSource::Homebrew.css_class(), "src-brew");
        assert_eq!(UpdateSource::Rustup.css_class(), "src-toolchain");
        assert_eq!(UpdateSource::Mise.css_class(), "src-toolchain");
        assert_eq!(UpdateSource::Distrobox.css_class(), "src-container");
        assert_eq!(UpdateSource::Toolbx.css_class(), "src-container");
        assert_eq!(UpdateSource::CustomHook.css_class(), "src-hook");
        assert_eq!(UpdateSource::Firmware.css_class(), "src-firmware");

        assert_eq!(UpdateSource::Dnf.console_tag(), "dnf");
        assert_eq!(UpdateSource::FlatpakUser.console_tag(), "flatpak-user");
        assert_eq!(UpdateSource::FlatpakSystem.console_tag(), "flatpak-system");
        assert_eq!(UpdateSource::GnomeExtensions.console_tag(), "gnome-ext");
        assert_eq!(UpdateSource::Homebrew.console_tag(), "brew");
        assert_eq!(UpdateSource::Rustup.console_tag(), "rustup");
        assert_eq!(UpdateSource::Mise.console_tag(), "mise");
        assert_eq!(UpdateSource::Distrobox.console_tag(), "distrobox");
        assert_eq!(UpdateSource::Toolbx.console_tag(), "toolbx");
        assert_eq!(UpdateSource::CustomHook.console_tag(), "hook");
        assert_eq!(UpdateSource::Firmware.console_tag(), "fwupd");

        assert_eq!(UpdateSource::Dnf.to_string(), "System");
        assert_eq!(AuthPurpose::Check.to_string(), "check for updates");
        assert_eq!(AuthPurpose::Apply.to_string(), "apply updates");

        assert!(!PackageStatus::Pending.is_done());
        assert!(!PackageStatus::Downloading.is_done());
        assert!(!PackageStatus::Installing.is_done());
        assert!(PackageStatus::Completed.is_done());
        assert!(PackageStatus::Failed.is_done());
        assert_eq!(PackageStatus::Pending.label(), "queued");
        assert_eq!(PackageStatus::Downloading.label(), "downloading");
        assert_eq!(PackageStatus::Installing.label(), "installing");
        assert_eq!(PackageStatus::Completed.label(), "done");
        assert_eq!(PackageStatus::Failed.label(), "failed");

        let sw = Stopwatch::start();
        assert!(sw.elapsed_secs() < 5);
        assert!(sw.elapsed_label().ends_with('s'));

        assert!((overall_progress(&[]) - 0.0).abs() < f64::EPSILON);

        let mut fp_sys = Package::new_dnf("Steam", "", "1", "flathub");
        fp_sys.source = UpdateSource::FlatpakSystem;
        let mut fp_user = Package::new_dnf("Signal", "", "1", "flathub");
        fp_user.source = UpdateSource::FlatpakUser;
        let mut pkgs = vec![
            firmware_pkg("UEFI"),
            fp_sys,
            Package::new_dnf("zlib", "x86_64", "1", "updates"),
            Package::new_dnf("bash", "x86_64", "1", "updates"),
            fp_user,
        ];
        sort_for_apply(&mut pkgs);
        assert_eq!(
            pkgs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["bash", "zlib", "Signal", "Steam", "UEFI"]
        );
        assert_eq!(
            sources_present(&pkgs),
            vec![
                UpdateSource::Dnf,
                UpdateSource::FlatpakUser,
                UpdateSource::FlatpakSystem,
                UpdateSource::Firmware,
            ]
        );
        assert_eq!(count_by_source(&pkgs), (2, 2, 1));

        // Partial fail + finalize preserves already-completed items and isolates sources.
        pkgs[0].status = PackageStatus::Completed;
        finalize_source(&mut pkgs, UpdateSource::Dnf, false);
        assert_eq!(pkgs[0].status, PackageStatus::Completed);
        assert_eq!(pkgs[1].status, PackageStatus::Failed);
        assert_eq!(pkgs[2].status, PackageStatus::Pending);
        assert!((overall_progress(&pkgs) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn detail_view_apply_mode_and_optional_package_metadata_defaults() {
        assert_eq!(DetailView::default(), DetailView::Updates);
        assert_eq!(ApplyMode::default(), ApplyMode::Live);

        let mut pkg = Package::new_dnf("openssl", "x86_64", "3.2.2-3", "updates");
        assert!(pkg.summary.is_none());
        assert!(pkg.install_size_delta.is_none());
        assert!(pkg.advisories.is_empty());
        assert!(pkg.related_extensions().is_empty());
        assert_eq!(pkg.clean_detail_without_installs(), "3.2.2-3 · updates");
        assert_eq!(pkg.primary_security_tag(), None);
        assert_eq!(pkg.advisory_summary(), "");

        pkg.detail =
            "128.2.0 · flathub · user · installs org.freedesktop.Platform.GL.nvidia-550, org.freedesktop.Platform.VAAPI.Intel"
                .into();
        assert_eq!(
            pkg.related_extensions(),
            vec![
                "org.freedesktop.Platform.GL.nvidia-550",
                "org.freedesktop.Platform.VAAPI.Intel"
            ]
        );
        assert_eq!(pkg.clean_detail_without_installs(), "128.2.0 · flathub · user");

        pkg.advisories.push(PackageAdvisory {
            id: "FEDORA-2026-84a1".into(),
            kind: AdvisoryKind::Security,
            severity: Some("Important".into()),
            cves: vec!["CVE-2026-2401".into()],
            bugzillas: vec!["RHBZ#12345".into()],
            description: Some("Fixes TLS 1.3 read".into()),
        });
        assert_eq!(
            pkg.primary_security_tag().as_deref(),
            Some("CVE-2026-2401")
        );
        assert_eq!(pkg.advisory_summary(), "FEDORA-2026-84a1 CVE-2026-2401");
        assert!(pkg.has_security_advisory());
        assert_eq!(
            count_security(&[
                pkg.clone(),
                Package::new_dnf("zlib", "x86_64", "1.3.1", "updates"),
            ]),
            1
        );

        pkg.advisories[0].cves.clear();
        assert_eq!(pkg.primary_security_tag().as_deref(), Some("Security"));
        assert!(pkg.has_security_advisory());
    }

    #[test]
    fn source_category_and_update_source_metadata_cover_all_variants() {
        assert_eq!(
            SourceCategory::ALL,
            &[
                SourceCategory::System,
                SourceCategory::Apps,
                SourceCategory::Extensions,
                SourceCategory::DevAndContainers,
                SourceCategory::Firmware,
            ]
        );
        for (idx, &cat) in SourceCategory::ALL.iter().enumerate() {
            assert_eq!(cat.sort_order() as usize, idx);
            assert_eq!(cat.to_string(), cat.label());
            assert_eq!(SourceCategory::from_filter_key(cat.filter_key()), Some(cat));
            assert!(!cat.sidebar_label().is_empty());
            assert!(cat.dot_css_class().starts_with("src-"));
        }
        assert_eq!(SourceCategory::from_filter_key("source:unknown"), None);

        assert_eq!(
            UpdateSource::all_sources(),
            &[
                UpdateSource::Dnf,
                UpdateSource::RpmOstree,
                UpdateSource::FlatpakUser,
                UpdateSource::FlatpakSystem,
                UpdateSource::GnomeExtensions,
                UpdateSource::Homebrew,
                UpdateSource::Rustup,
                UpdateSource::Mise,
                UpdateSource::Distrobox,
                UpdateSource::Toolbx,
                UpdateSource::CustomHook,
                UpdateSource::Firmware,
            ]
        );
        assert_eq!(UpdateSource::Dnf.category(), SourceCategory::System);
        assert_eq!(UpdateSource::RpmOstree.category(), SourceCategory::System);
        assert_eq!(UpdateSource::FlatpakUser.category(), SourceCategory::Apps);
        assert_eq!(UpdateSource::FlatpakSystem.category(), SourceCategory::Apps);
        assert_eq!(
            UpdateSource::GnomeExtensions.category(),
            SourceCategory::Extensions
        );
        assert_eq!(
            UpdateSource::Homebrew.category(),
            SourceCategory::DevAndContainers
        );
        assert_eq!(
            UpdateSource::Rustup.category(),
            SourceCategory::DevAndContainers
        );
        assert_eq!(
            UpdateSource::Mise.category(),
            SourceCategory::DevAndContainers
        );
        assert_eq!(
            UpdateSource::Distrobox.category(),
            SourceCategory::DevAndContainers
        );
        assert_eq!(
            UpdateSource::Toolbx.category(),
            SourceCategory::DevAndContainers
        );
        assert_eq!(
            UpdateSource::CustomHook.category(),
            SourceCategory::DevAndContainers
        );
        assert_eq!(UpdateSource::Firmware.category(), SourceCategory::Firmware);

        assert_eq!(UpdateSource::Dnf.pref_key(), "dnf");
        assert_eq!(UpdateSource::RpmOstree.pref_key(), "rpm-ostree");
        assert_eq!(UpdateSource::FlatpakUser.pref_key(), "flatpak");
        assert_eq!(UpdateSource::FlatpakSystem.pref_key(), "flatpak");
        assert_eq!(UpdateSource::GnomeExtensions.pref_key(), "gnome-extensions");
        assert_eq!(UpdateSource::Homebrew.pref_key(), "homebrew");
        assert_eq!(UpdateSource::Rustup.pref_key(), "rustup");
        assert_eq!(UpdateSource::Mise.pref_key(), "mise");
        assert_eq!(UpdateSource::Distrobox.pref_key(), "distrobox");
        assert_eq!(UpdateSource::Toolbx.pref_key(), "toolbx");
        assert_eq!(UpdateSource::CustomHook.pref_key(), "custom-hooks");
        assert_eq!(UpdateSource::Firmware.pref_key(), "firmware");

        assert!(UpdateSource::is_known_pref_key("rustup"));
        assert!(UpdateSource::is_known_pref_key(" MISE "));
        assert!(UpdateSource::is_known_pref_key("dnf"));
        assert!(!UpdateSource::is_known_pref_key("uv"));
        assert!(!UpdateSource::is_known_pref_key("uv-tools"));
        assert!(!UpdateSource::is_known_pref_key("pipx"));

        assert!(UpdateSource::Dnf.requires_root());
        assert!(UpdateSource::RpmOstree.requires_root());
        assert!(!UpdateSource::FlatpakUser.requires_root());
        assert!(!UpdateSource::FlatpakSystem.requires_root());
        assert!(!UpdateSource::GnomeExtensions.requires_root());
        assert!(!UpdateSource::Homebrew.requires_root());
        assert!(!UpdateSource::Rustup.requires_root());
        assert!(!UpdateSource::Mise.requires_root());
        assert!(!UpdateSource::Distrobox.requires_root());
        assert!(!UpdateSource::Toolbx.requires_root());
        assert!(!UpdateSource::CustomHook.requires_root());
        assert!(UpdateSource::Firmware.requires_root());

        assert!(UpdateSource::Dnf.installed_by_default());
        assert!(!UpdateSource::RpmOstree.installed_by_default());
        assert!(!UpdateSource::Dnf.installed_by_default_for_host(true));
        assert!(UpdateSource::RpmOstree.installed_by_default_for_host(true));
        assert!(UpdateSource::FlatpakUser.installed_by_default());
        assert!(UpdateSource::FlatpakSystem.installed_by_default());
        assert!(!UpdateSource::GnomeExtensions.installed_by_default());
        assert!(!UpdateSource::Homebrew.installed_by_default());
        assert!(!UpdateSource::Rustup.installed_by_default());
        assert!(!UpdateSource::Mise.installed_by_default());
        assert!(!UpdateSource::Distrobox.installed_by_default());
        assert!(!UpdateSource::Toolbx.installed_by_default());
        assert!(!UpdateSource::CustomHook.installed_by_default());
        assert!(UpdateSource::Distrobox.is_opt_in());
        assert!(UpdateSource::Toolbx.is_opt_in());
        assert!(!UpdateSource::CustomHook.is_opt_in());
        assert!(!UpdateSource::Homebrew.is_opt_in());
        assert!(!UpdateSource::Rustup.is_opt_in());
        assert!(!UpdateSource::Mise.is_opt_in());
        assert!(UpdateSource::pref_key_is_opt_in(" Distrobox "));
        assert!(UpdateSource::pref_key_is_opt_in("toolbx"));
        assert!(!UpdateSource::pref_key_is_opt_in("homebrew"));
        assert!(!UpdateSource::pref_key_is_opt_in("rustup"));
        assert!(!UpdateSource::pref_key_is_opt_in("mise"));
        assert!(UpdateSource::Firmware.installed_by_default());

        assert_eq!(
            SourceCategory::System.sidebar_label_for_host(false),
            "System · DNF"
        );
        assert_eq!(
            SourceCategory::System.sidebar_label_for_host(true),
            "System · Atomic"
        );
        assert_eq!(SourceCategory::System.dot_css_class_for_host(false), "src-dnf");
        assert_eq!(
            SourceCategory::System.dot_css_class_for_host(true),
            "src-ostree"
        );
        assert_eq!(UpdateSource::RpmOstree.badge(), "OSTREE");
        assert_eq!(UpdateSource::RpmOstree.css_class(), "src-ostree");
        assert_eq!(UpdateSource::RpmOstree.console_tag(), "rpm-ostree");
        assert_eq!(UpdateSource::RpmOstree.apply_order(), 0);

        for &src in UpdateSource::all_sources() {
            assert_eq!(src.supports_selective_apply(), src != UpdateSource::RpmOstree);
            assert!(!src.pref_title().is_empty());
        }

        assert_eq!(
            UpdateSource::pref_sources(),
            vec![
                UpdateSource::Dnf,
                UpdateSource::FlatpakUser,
                UpdateSource::GnomeExtensions,
                UpdateSource::Homebrew,
                UpdateSource::Rustup,
                UpdateSource::Mise,
                UpdateSource::Distrobox,
                UpdateSource::Toolbx,
                UpdateSource::Firmware,
            ]
        );
        assert_eq!(
            UpdateSource::pref_sources_for_host(true),
            vec![
                UpdateSource::RpmOstree,
                UpdateSource::FlatpakUser,
                UpdateSource::GnomeExtensions,
                UpdateSource::Homebrew,
                UpdateSource::Rustup,
                UpdateSource::Mise,
                UpdateSource::Distrobox,
                UpdateSource::Toolbx,
                UpdateSource::Firmware,
            ]
        );

        let ext_pkg = Package::new_gnome_extension(
            "blur-my-shell@aunetx",
            "Blur my Shell",
            Some("v66".into()),
            Some("Adds blur to GNOME Shell".into()),
            Some("https://github.com/aunetx/blur-my-shell".into()),
        );
        assert_eq!(ext_pkg.id, "gnome-ext:blur-my-shell@aunetx");
        assert_eq!(ext_pkg.repo, "extensions.gnome.org");
        assert_eq!(ext_pkg.old_version.as_deref(), Some("v66"));
        assert_eq!(ext_pkg.version, "update available");
        assert_eq!(ext_pkg.detail, "v66 → update available");
        assert_eq!(
            ext_pkg.url.as_deref(),
            Some("https://github.com/aunetx/blur-my-shell")
        );

        let ext_minimal = Package::new_gnome_extension("caffeine@patapon.info", "", None, None, None);
        assert_eq!(ext_minimal.id, "gnome-ext:caffeine@patapon.info");
        assert_eq!(ext_minimal.name, "caffeine@patapon.info");
        assert!(ext_minimal.old_version.is_none());
        assert!(ext_minimal.url.is_none());
        assert_eq!(ext_minimal.detail, "update available · caffeine@patapon.info");

        let mut fp_sys = Package::new_dnf("Steam", "", "1", "flathub");
        fp_sys.source = UpdateSource::FlatpakSystem;
        let mut fp_user = Package::new_dnf("Signal", "", "1", "flathub");
        fp_user.source = UpdateSource::FlatpakUser;
        let brew_pkg = Package::new_homebrew("ripgrep", Some("14.1.0".into()), "14.1.1", "", false);
        assert_eq!(brew_pkg.id, "brew:ripgrep:14.1.1");
        assert_eq!(brew_pkg.repo, "homebrew/core");
        assert_eq!(brew_pkg.old_version.as_deref(), Some("14.1.0"));
        assert_eq!(brew_pkg.detail, "14.1.1 · homebrew/core · formula");
        assert_eq!(brew_pkg.version_line(), "14.1.0 → 14.1.1");

        let brew_cask = Package::new_homebrew("font-jetbrains-mono", None, "3.3.0", "", true);
        assert_eq!(brew_cask.id, "brew:font-jetbrains-mono:3.3.0");
        assert_eq!(brew_cask.repo, "homebrew/cask");
        assert_eq!(brew_cask.detail, "3.3.0 · homebrew/cask · cask");

        let rust_pkg = Package::new_toolchain(
            UpdateSource::Rustup,
            "rustup:stable-x86_64-unknown-linux-gnu",
            "stable-x86_64-unknown-linux-gnu",
            Some("1.86.0".into()),
            "1.87.0",
            "",
            "",
            None,
            None,
        );
        assert_eq!(rust_pkg.repo, "rustup");
        assert_eq!(rust_pkg.detail, "1.87.0 · rustup");
        assert_eq!(rust_pkg.version_line(), "1.86.0 → 1.87.0");

        let mise_pkg = Package::new_toolchain(
            UpdateSource::Mise,
            "mise:node",
            "node",
            None,
            "22.15.0",
            "mise",
            "22.15.0 · requested 22",
            Some("Configured in mise.toml".into()),
            Some("https://nodejs.org".into()),
        );
        assert_eq!(mise_pkg.detail, "22.15.0 · requested 22");
        assert_eq!(mise_pkg.summary.as_deref(), Some("Configured in mise.toml"));
        assert_eq!(mise_pkg.url.as_deref(), Some("https://nodejs.org"));

        let dbox = Package::new_container(
            UpdateSource::Distrobox,
            "a1b2c3d4e5f6ffff",
            "arch-dev",
            "Up 2 hours",
            "docker.io/library/archlinux:latest",
            None,
        );
        assert_eq!(dbox.id, "distrobox:arch-dev");
        assert_eq!(dbox.repo, "docker.io/library/archlinux:latest");
        assert_eq!(dbox.version, "Up 2 hours");
        assert_eq!(
            dbox.detail,
            "Container · docker.io/library/archlinux:latest · check & update on apply"
        );
        assert_eq!(dbox.summary.as_deref(), Some("Rootless container a1b2c3d4e5f6"));

        let tbx = Package::new_container(
            UpdateSource::Toolbx,
            "0123456789ab",
            "fedora-toolbox-44",
            "exited",
            "registry.fedoraproject.org/fedora-toolbox:44",
            Some("2 weeks ago".into()),
        );
        assert_eq!(tbx.id, "toolbx:fedora-toolbox-44");
        assert_eq!(
            tbx.summary.as_deref(),
            Some("Rootless container 0123456789ab · Created 2 weeks ago")
        );

        let pkgs = vec![
            Package::new_dnf("bash", "x86_64", "1", "updates"),
            fp_user,
            fp_sys,
            brew_pkg,
            rust_pkg,
            mise_pkg,
            dbox,
            tbx,
            firmware_pkg("UEFI"),
        ];
        let by_cat = count_by_category(&pkgs);
        assert_eq!(by_cat.get(&SourceCategory::System), Some(&1));
        assert_eq!(by_cat.get(&SourceCategory::Apps), Some(&2));
        assert_eq!(by_cat.get(&SourceCategory::DevAndContainers), Some(&5));
        assert_eq!(by_cat.get(&SourceCategory::Firmware), Some(&1));
        assert_eq!(by_cat.get(&SourceCategory::Extensions), None);
        assert_eq!(count_for_category(&pkgs, SourceCategory::Apps), 2);
        assert_eq!(
            categories_present(&pkgs),
            vec![
                SourceCategory::System,
                SourceCategory::Apps,
                SourceCategory::DevAndContainers,
                SourceCategory::Firmware,
            ]
        );
    }
}
