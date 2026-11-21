//! `Adw.OverlaySplitView` Package Inspector pane.

use gtk::prelude::*;

use fedora_updater::model::{AdvisoryKind, Package, PackageAdvisory, PackageStatus, UpdateSource};
use fedora_updater::presenter::format_size_delta;

use super::queue_list::package_needs_reboot;

/// Pure presentation model for the Package Inspector pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectorViewModel {
    pub badge: &'static str,
    pub badge_css: &'static str,
    pub source_title: &'static str,
    pub subtitle: String,
    pub name: String,
    pub repo: Option<String>,
    pub arch: Option<String>,
    pub old_version: Option<String>,
    pub version: String,
    pub version_display: String,
    pub size_label: Option<String>,
    pub install_size_delta: Option<String>,
    pub status_label: Option<&'static str>,
    pub needs_reboot: bool,
    pub reboot_reason: Option<String>,
    pub preflight_warning: Option<String>,
    pub related_extensions: Vec<String>,
    pub summary: Option<String>,
    pub advisories: Vec<PackageAdvisory>,
}

impl InspectorViewModel {
    pub fn from_package(package: &Package) -> Self {
        let source_title = match package.source {
            UpdateSource::Dnf => "System (DNF)",
            UpdateSource::FlatpakUser => "Apps (Flatpak · User)",
            UpdateSource::FlatpakSystem => "Apps (Flatpak · System)",
            UpdateSource::Firmware => "Firmware (LVFS)",
        };
        let repo = (!package.repo.trim().is_empty()).then(|| package.repo.trim().to_string());
        let subtitle = match &repo {
            Some(r) => format!("{} · {r}", package.source.label()),
            None => package.source.label().to_string(),
        };
        let arch = (!package.arch.trim().is_empty()).then(|| package.arch.trim().to_string());
        let old_version = package
            .old_version
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let version_display = match &old_version {
            Some(old) => format!("{old} → {}", package.version),
            None => package.version.clone(),
        };
        let status_label = match package.status {
            PackageStatus::Pending => None,
            PackageStatus::Downloading => Some("Downloading"),
            PackageStatus::Installing => Some("Installing"),
            PackageStatus::Completed => Some("Installed"),
            PackageStatus::Failed => Some("Failed"),
        };
        let needs_reboot = package_needs_reboot(package);
        let reboot_reason = needs_reboot.then(|| {
            format!(
                "Updating {} modifies core system components and requires a restart to take effect.",
                package.name
            )
        });
        let preflight_warning = package
            .preflight_warning
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let summary = package
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let install_size_delta = package
            .install_size_delta
            .filter(|delta| *delta != 0)
            .map(format_size_delta);

        Self {
            badge: package.source.badge(),
            badge_css: package.source.css_class(),
            source_title,
            subtitle,
            name: package.name.clone(),
            repo,
            arch,
            old_version,
            version: package.version.clone(),
            version_display,
            size_label: package.size_label(),
            install_size_delta,
            status_label,
            needs_reboot,
            reboot_reason,
            preflight_warning,
            related_extensions: package.related_extensions(),
            summary,
            advisories: package.advisories.clone(),
        }
    }
}

struct MetadataField {
    row: gtk::Box,
    value: gtk::Label,
}

impl MetadataField {
    fn new(key: &str) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let key_lbl = gtk::Label::builder()
            .label(key)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .css_classes(["caption", "dimmed"])
            .build();
        let value = gtk::Label::builder()
            .label("")
            .xalign(1.0)
            .halign(gtk::Align::End)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .can_focus(false)
            .css_classes(["caption", "numeric"])
            .build();
        row.append(&key_lbl);
        row.append(&value);
        Self { row, value }
    }

    fn set_optional(&self, val: Option<&str>) {
        match val.filter(|s| !s.is_empty()) {
            Some(text) => {
                self.value.set_label(text);
                self.row.set_visible(true);
            }
            None => {
                self.value.set_label("");
                self.row.set_visible(false);
            }
        }
    }
}

/// Right-hand `Adw.OverlaySplitView` inspector rendering details of an activated package.
pub struct PackageInspector {
    root: gtk::Box,
    close_btn: gtk::Button,
    source_badge: gtk::Label,
    name_label: gtk::Label,
    subtitle_label: gtk::Label,
    summary_label: gtk::Label,
    reboot_card: gtk::Box,
    reboot_body: gtk::Label,
    warning_card: gtk::Box,
    warning_body: gtk::Label,
    source_field: MetadataField,
    repo_field: MetadataField,
    arch_field: MetadataField,
    old_version_field: MetadataField,
    version_field: MetadataField,
    size_field: MetadataField,
    disk_delta_field: MetadataField,
    status_field: MetadataField,
    extensions_card: gtk::Box,
    extensions_count: gtk::Label,
    extensions_list: gtk::Box,
    advisories_card: gtk::Box,
    advisories_list: gtk::Box,
}

impl PackageInspector {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.add_css_class("inspector-pane");

        // Header bar inside the OverlaySplitView sidebar.
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_margin_top(10);
        header.set_margin_bottom(10);
        header.set_margin_start(14);
        header.set_margin_end(10);

        let header_title = gtk::Label::builder()
            .label("Package Inspector")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["heading"])
            .build();

        let close_btn = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close Inspector (Esc)")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        close_btn.update_property(&[gtk::accessible::Property::Label("Close Package Inspector")]);

        header.append(&header_title);
        header.append(&close_btn);

        // Scrollable inspector body.
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_top(14);
        body.set_margin_bottom(14);
        body.set_margin_start(14);
        body.set_margin_end(14);

        let identity = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        identity.set_valign(gtk::Align::Start);

        let source_badge = gtk::Label::builder()
            .width_chars(3)
            .valign(gtk::Align::Center)
            .css_classes(["package-badge"])
            .build();

        let title_col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        title_col.set_hexpand(true);

        let name_label = gtk::Label::builder()
            .label("")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .can_focus(false)
            .css_classes(["title-4"])
            .build();

        let subtitle_label = gtk::Label::builder()
            .label("")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["caption", "dimmed"])
            .build();

        title_col.append(&name_label);
        title_col.append(&subtitle_label);
        identity.append(&source_badge);
        identity.append(&title_col);
        body.append(&identity);

        let summary_label = gtk::Label::builder()
            .label("")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .visible(false)
            .css_classes(["caption"])
            .build();
        body.append(&summary_label);

        // Restart warning card (when package_implies_reboot is true).
        let (reboot_card, reboot_body) = build_callout_card(
            "inspector-reboot-card",
            "system-reboot-symbolic",
            "warning",
            "Restart Required",
        );
        body.append(&reboot_card);

        // Pre-flight power safety warning card (when battery/AC requirements are unmet).
        let (warning_card, warning_body) = build_callout_card(
            "inspector-warning-card",
            "battery-caution-symbolic",
            "error",
            "Power Safety Warning",
        );
        body.append(&warning_card);

        // Metadata key-value card.
        let meta_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        meta_card.add_css_class("card");
        let meta_inner = gtk::Box::new(gtk::Orientation::Vertical, 8);
        meta_inner.set_margin_top(12);
        meta_inner.set_margin_bottom(12);
        meta_inner.set_margin_start(14);
        meta_inner.set_margin_end(14);

        let source_field = MetadataField::new("Source");
        let repo_field = MetadataField::new("Repository");
        let arch_field = MetadataField::new("Architecture");
        let old_version_field = MetadataField::new("Installed");
        let version_field = MetadataField::new("Available");
        let size_field = MetadataField::new("Download Size");
        let disk_delta_field = MetadataField::new("Disk Change");
        let status_field = MetadataField::new("Status");

        for field in [
            &source_field,
            &repo_field,
            &arch_field,
            &old_version_field,
            &version_field,
            &size_field,
            &disk_delta_field,
            &status_field,
        ] {
            meta_inner.append(&field.row);
        }
        meta_card.append(&meta_inner);
        body.append(&meta_card);

        // Flatpak related extensions breakdown card.
        let extensions_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        extensions_card.add_css_class("card");
        extensions_card.set_visible(false);

        let ext_inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        ext_inner.set_margin_top(12);
        ext_inner.set_margin_bottom(12);
        ext_inner.set_margin_start(14);
        ext_inner.set_margin_end(14);

        let ext_head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let ext_title = gtk::Label::builder()
            .label("Related Extensions")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let extensions_count = gtk::Label::builder()
            .label("0")
            .valign(gtk::Align::Center)
            .css_classes(["sidebar-count-pill", "caption", "numeric"])
            .build();
        ext_head.append(&ext_title);
        ext_head.append(&extensions_count);

        let ext_caption = gtk::Label::builder()
            .label("Installed alongside this Flatpak update (download size includes shared extensions).")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["caption", "dimmed"])
            .build();

        let extensions_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        extensions_list.set_margin_top(2);

        ext_inner.append(&ext_head);
        ext_inner.append(&ext_caption);
        ext_inner.append(&extensions_list);
        extensions_card.append(&ext_inner);
        body.append(&extensions_card);

        // Advisory / CVE card (hidden cleanly when `advisories` is empty in Phase 1).
        let advisories_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        advisories_card.add_css_class("card");
        advisories_card.set_visible(false);

        let adv_inner = gtk::Box::new(gtk::Orientation::Vertical, 8);
        adv_inner.set_margin_top(12);
        adv_inner.set_margin_bottom(12);
        adv_inner.set_margin_start(14);
        adv_inner.set_margin_end(14);

        let adv_title = gtk::Label::builder()
            .label("Advisories & CVEs")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build();
        let advisories_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        adv_inner.append(&adv_title);
        adv_inner.append(&advisories_list);
        advisories_card.append(&adv_inner);
        body.append(&advisories_card);

        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .child(&body)
            .build();

        root.append(&header);
        root.append(&scroll);

        Self {
            root,
            close_btn,
            source_badge,
            name_label,
            subtitle_label,
            summary_label,
            reboot_card,
            reboot_body,
            warning_card,
            warning_body,
            source_field,
            repo_field,
            arch_field,
            old_version_field,
            version_field,
            size_field,
            disk_delta_field,
            status_field,
            extensions_card,
            extensions_count,
            extensions_list,
            advisories_card,
            advisories_list,
        }
    }

    pub fn root(&self) -> &gtk::Box {
        &self.root
    }

    pub fn connect_close(&self, on_close: impl Fn() + 'static) {
        self.close_btn.connect_clicked(move |_| on_close());
    }

    pub fn paint(&self, package: &Package) {
        let vm = InspectorViewModel::from_package(package);

        for cls in ["src-dnf", "src-flatpak", "src-firmware"] {
            self.source_badge.remove_css_class(cls);
        }
        self.source_badge.set_label(vm.badge);
        self.source_badge.add_css_class(vm.badge_css);

        self.name_label.set_label(&vm.name);
        self.subtitle_label.set_label(&vm.subtitle);

        if let Some(summary) = vm.summary.as_deref() {
            self.summary_label.set_label(summary);
            self.summary_label.set_visible(true);
        } else {
            self.summary_label.set_label("");
            self.summary_label.set_visible(false);
        }

        if let Some(reason) = vm.reboot_reason.as_deref() {
            self.reboot_body.set_label(reason);
            self.reboot_card.set_visible(true);
        } else {
            self.reboot_body.set_label("");
            self.reboot_card.set_visible(false);
        }

        if let Some(warning) = vm.preflight_warning.as_deref() {
            self.warning_body.set_label(warning);
            self.warning_card.set_visible(true);
        } else {
            self.warning_body.set_label("");
            self.warning_card.set_visible(false);
        }

        self.source_field.set_optional(Some(vm.source_title));
        self.repo_field.set_optional(vm.repo.as_deref());
        self.arch_field.set_optional(vm.arch.as_deref());
        self.old_version_field
            .set_optional(vm.old_version.as_deref());
        self.version_field.set_optional(Some(&vm.version));
        self.size_field.set_optional(vm.size_label.as_deref());
        self.disk_delta_field
            .set_optional(vm.install_size_delta.as_deref());
        self.status_field.set_optional(vm.status_label);

        clear_box(&self.extensions_list);
        if vm.related_extensions.is_empty() {
            self.extensions_card.set_visible(false);
        } else {
            self.extensions_count
                .set_label(&vm.related_extensions.len().to_string());
            for ext in &vm.related_extensions {
                let lbl = gtk::Label::builder()
                    .label(ext)
                    .xalign(0.0)
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .selectable(true)
                    .can_focus(false)
                    .css_classes(["monospace", "caption"])
                    .build();
                self.extensions_list.append(&lbl);
            }
            self.extensions_card.set_visible(true);
        }

        clear_box(&self.advisories_list);
        if vm.advisories.is_empty() {
            self.advisories_card.set_visible(false);
        } else {
            for adv in &vm.advisories {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
                let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let id_lbl = gtk::Label::builder()
                    .label(&adv.id)
                    .xalign(0.0)
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .css_classes(["monospace", "caption"])
                    .build();
                head.append(&id_lbl);

                if let Some((sev_text, sev_css)) = advisory_severity_chip(adv) {
                    let sev_lbl = gtk::Label::builder()
                        .label(sev_text)
                        .valign(gtk::Align::Center)
                        .css_classes(["micro-tag", sev_css])
                        .build();
                    head.append(&sev_lbl);
                }
                row.append(&head);

                if !adv.cves.is_empty() || !adv.bugzillas.is_empty() {
                    let tags = libadwaita::WrapBox::builder()
                        .child_spacing(4)
                        .line_spacing(4)
                        .build();
                    for cve in &adv.cves {
                        let chip = gtk::Label::builder()
                            .label(cve)
                            .css_classes(["micro-tag", "tag-sec"])
                            .build();
                        tags.append(&chip);
                    }
                    for bz in &adv.bugzillas {
                        let chip = gtk::Label::builder()
                            .label(bz)
                            .css_classes(["micro-tag", "tag-bz"])
                            .build();
                        tags.append(&chip);
                    }
                    row.append(&tags);
                }
                if let Some(desc) = adv.description.as_deref().filter(|s| !s.trim().is_empty()) {
                    let desc_lbl = gtk::Label::builder()
                        .label(desc)
                        .xalign(0.0)
                        .halign(gtk::Align::Start)
                        .wrap(true)
                        .css_classes(["caption", "dimmed"])
                        .build();
                    row.append(&desc_lbl);
                }
                self.advisories_list.append(&row);
            }
            self.advisories_card.set_visible(true);
        }
    }

    #[cfg(test)]
    pub fn close_btn(&self) -> &gtk::Button {
        &self.close_btn
    }

    #[cfg(test)]
    pub fn name_text(&self) -> String {
        self.name_label.text().to_string()
    }

    #[cfg(test)]
    pub fn badge_text(&self) -> String {
        self.source_badge.text().to_string()
    }

    #[cfg(test)]
    pub fn repo_value(&self) -> Option<String> {
        self.repo_field
            .row
            .get_visible()
            .then(|| self.repo_field.value.text().to_string())
    }

    #[cfg(test)]
    pub fn arch_value(&self) -> Option<String> {
        self.arch_field
            .row
            .get_visible()
            .then(|| self.arch_field.value.text().to_string())
    }

    #[cfg(test)]
    pub fn version_value(&self) -> String {
        self.version_field.value.text().to_string()
    }

    #[cfg(test)]
    pub fn old_version_value(&self) -> Option<String> {
        self.old_version_field
            .row
            .get_visible()
            .then(|| self.old_version_field.value.text().to_string())
    }

    #[cfg(test)]
    pub fn size_value(&self) -> Option<String> {
        self.size_field
            .row
            .get_visible()
            .then(|| self.size_field.value.text().to_string())
    }

    #[cfg(test)]
    pub fn reboot_card_visible(&self) -> bool {
        self.reboot_card.get_visible()
    }

    #[cfg(test)]
    pub fn warning_card_visible(&self) -> bool {
        self.warning_card.get_visible()
    }

    #[cfg(test)]
    pub fn warning_body_text(&self) -> String {
        self.warning_body.text().to_string()
    }

    #[cfg(test)]
    pub fn extensions_card_visible(&self) -> bool {
        self.extensions_card.get_visible()
    }

    #[cfg(test)]
    pub fn extension_items(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut child = self.extensions_list.first_child();
        while let Some(w) = child {
            if let Ok(lbl) = w.clone().downcast::<gtk::Label>() {
                out.push(lbl.text().to_string());
            }
            child = w.next_sibling();
        }
        out
    }

    #[cfg(test)]
    pub fn summary_visible(&self) -> bool {
        self.summary_label.get_visible()
    }

    #[cfg(test)]
    pub fn summary_text(&self) -> String {
        self.summary_label.text().to_string()
    }

    #[cfg(test)]
    pub fn advisories_card_visible(&self) -> bool {
        self.advisories_card.get_visible()
    }

    #[cfg(test)]
    pub fn disk_delta_value(&self) -> Option<String> {
        self.disk_delta_field
            .row
            .get_visible()
            .then(|| self.disk_delta_field.value.text().to_string())
    }

    #[cfg(test)]
    pub fn advisory_summary_text(&self) -> String {
        let mut parts = Vec::new();
        collect_label_texts(self.advisories_list.upcast_ref(), &mut parts);
        parts.join(" ")
    }
}

fn advisory_severity_chip(adv: &PackageAdvisory) -> Option<(&str, &'static str)> {
    if let Some(sev) = adv.severity.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let lower = sev.to_ascii_lowercase();
        let css = match lower.as_str() {
            "critical" | "important" => "tag-sec",
            "moderate" => "tag-reboot",
            _ => "tag-bz",
        };
        return Some((sev, css));
    }
    match adv.kind {
        AdvisoryKind::Security => Some(("Security", "tag-sec")),
        AdvisoryKind::Bugfix => Some(("Bugfix", "tag-bz")),
        AdvisoryKind::Enhancement => Some(("Enhancement", "tag-bz")),
        AdvisoryKind::NewPackage => Some(("New", "tag-bz")),
    }
}

#[cfg(test)]
fn collect_label_texts(widget: &gtk::Widget, out: &mut Vec<String>) {
    if let Ok(lbl) = widget.clone().downcast::<gtk::Label>() {
        let t = lbl.text().to_string();
        if !t.is_empty() {
            out.push(t);
        }
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        collect_label_texts(&c, out);
        child = c.next_sibling();
    }
}

fn build_callout_card(
    css_class: &str,
    icon_name: &str,
    icon_css_class: &str,
    title: &str,
) -> (gtk::Box, gtk::Label) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    card.add_css_class(css_class);
    card.set_visible(false);

    let inner = gtk::Box::new(gtk::Orientation::Vertical, 4);
    inner.set_margin_top(12);
    inner.set_margin_bottom(12);
    inner.set_margin_start(14);
    inner.set_margin_end(14);

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class(icon_css_class);
    let title_lbl = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    head.append(&icon);
    head.append(&title_lbl);

    let body_lbl = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["caption", "dimmed"])
        .build();

    inner.append(&head);
    inner.append(&body_lbl);
    card.append(&inner);
    (card, body_lbl)
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_view_model_formats_dnf_reboot_and_omits_empty_phase2_fields() {
        let mut kernel = Package::new_dnf("kernel", "x86_64", "6.14.11-300.fc44", "updates");
        kernel.old_version = Some("6.14.9-300.fc44".into());
        kernel.size_bytes = Some(98 * 1024 * 1024);

        let vm = InspectorViewModel::from_package(&kernel);
        assert_eq!(vm.badge, "DNF");
        assert_eq!(vm.source_title, "System (DNF)");
        assert_eq!(vm.repo.as_deref(), Some("updates"));
        assert_eq!(vm.arch.as_deref(), Some("x86_64"));
        assert_eq!(vm.old_version.as_deref(), Some("6.14.9-300.fc44"));
        assert_eq!(vm.version, "6.14.11-300.fc44");
        assert_eq!(vm.version_display, "6.14.9-300.fc44 → 6.14.11-300.fc44");
        assert_eq!(vm.size_label.as_deref(), Some("98 MB"));
        assert!(vm.needs_reboot);
        assert!(vm.reboot_reason.is_some());
        assert!(vm.related_extensions.is_empty());
        assert!(vm.summary.is_none());
        assert!(vm.advisories.is_empty());
    }

    #[test]
    fn inspector_view_model_extracts_flatpak_extensions_and_hides_missing_firmware_fields() {
        let mut steam = Package::new_dnf("Steam", "x86_64", "1.0.0.82", "flathub");
        steam.source = UpdateSource::FlatpakSystem;
        steam.size_bytes = Some(251 * 1024 * 1024);
        steam.summary = Some("  Launcher for the Steam software distribution service  ".into());
        steam.detail = "1.0.0.82 · flathub · system · installs org.freedesktop.Platform.GL32.nvidia-570-133-07, org.freedesktop.Platform.GL.nvidia-570-133-07".into();

        let fp_vm = InspectorViewModel::from_package(&steam);
        assert_eq!(fp_vm.badge, "FP");
        assert_eq!(fp_vm.source_title, "Apps (Flatpak · System)");
        assert!(!fp_vm.needs_reboot);
        assert_eq!(
            fp_vm.summary.as_deref(),
            Some("Launcher for the Steam software distribution service")
        );
        assert_eq!(
            fp_vm.related_extensions,
            vec![
                "org.freedesktop.Platform.GL32.nvidia-570-133-07",
                "org.freedesktop.Platform.GL.nvidia-570-133-07",
            ]
        );

        let mut fw = Package::new_dnf("UEFI dbx", "", "20241101", "LVFS");
        fw.source = UpdateSource::Firmware;
        fw.status = PackageStatus::Completed;
        let fw_vm = InspectorViewModel::from_package(&fw);
        assert_eq!(fw_vm.badge, "FW");
        assert_eq!(fw_vm.source_title, "Firmware (LVFS)");
        assert!(fw_vm.arch.is_none());
        assert!(fw_vm.size_label.is_none());
        assert_eq!(fw_vm.status_label, Some("Installed"));
        assert!(fw_vm.preflight_warning.is_none());

        fw.preflight_warning =
            Some("System Firmware requires AC power to install safely. Connect AC power and try again.".into());
        let warn_vm = InspectorViewModel::from_package(&fw);
        assert!(warn_vm
            .preflight_warning
            .as_deref()
            .unwrap()
            .contains("AC power"));
    }
}
