//! Observatory navigation sidebar frame and Fedora release-upgrade cards.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

use fedora_updater::model::{count_by_source, DetailView};
use fedora_updater::presenter::{check_progress, run_heading};
use fedora_updater::release_upgrade::{
    banner_title, os_release_version, release_age_caption, upgrade_button_name,
};
use fedora_updater::state::{AppState, Phase};
use fedora_updater::IdleScreen;

use super::charts::ThroughputSparkline;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceFilter {
    #[default]
    All,
    System,
    Apps,
    Firmware,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PulseTone {
    Ready,
    Attention,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineUpgradeSlot {
    None,
    Idle,
    Ready,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarPulse {
    pub tone: PulseTone,
    pub headline: String,
    pub detail: String,
    pub timestamp: Option<String>,
    pub spinning: bool,
}

/// Extract a compact host label from `/etc/os-release` contents (for example
/// `"Fedora Linux 44"`).
pub fn format_host_label(os_release: &str) -> String {
    for line in os_release.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("PRETTY_NAME=") else {
            continue;
        };
        let stripped = rest.trim().trim_matches('"').trim_matches('\'');
        let short = stripped.split(" (").next().unwrap_or(stripped).trim();
        if !short.is_empty() {
            return short.to_string();
        }
    }
    if let Some(version) = os_release_version(os_release) {
        return format!("Fedora Linux {version}");
    }
    "Fedora Linux".to_string()
}

pub fn installed_host_label() -> String {
    std::fs::read_to_string("/etc/os-release")
        .map(|text| format_host_label(&text))
        .unwrap_or_else(|_| "Fedora Linux".to_string())
}

pub fn release_upgrade_allowed_in_sidebar(phase: &Phase) -> bool {
    !matches!(phase, Phase::Authenticating { .. } | Phase::Running(_))
}

pub fn release_upgrade_inline_slot(phase: &Phase) -> InlineUpgradeSlot {
    match phase {
        Phase::Idle(_) => InlineUpgradeSlot::Idle,
        Phase::Ready { .. } => InlineUpgradeSlot::Ready,
        Phase::Done { .. } => InlineUpgradeSlot::Done,
        Phase::Authenticating { .. }
        | Phase::Checking { .. }
        | Phase::Running(_)
        | Phase::Failed { .. } => InlineUpgradeSlot::None,
    }
}

pub fn pulse_summary(state: &AppState, host_label: &str) -> SidebarPulse {
    match &state.phase {
        Phase::Idle(IdleScreen::Fresh) => SidebarPulse {
            tone: PulseTone::Attention,
            headline: host_label.to_string(),
            detail: "Not checked this session".to_string(),
            timestamp: None,
            spinning: false,
        },
        Phase::Idle(IdleScreen::Current { checked_at, detail }) => SidebarPulse {
            tone: PulseTone::Ready,
            headline: host_label.to_string(),
            detail: detail
                .clone()
                .unwrap_or_else(|| "System is up to date".to_string()),
            timestamp: Some(format!("Checked {checked_at}")),
            spinning: false,
        },
        Phase::Idle(IdleScreen::Warning {
            title, checked_at, ..
        }) => SidebarPulse {
            tone: PulseTone::Attention,
            headline: (*title).to_string(),
            detail: "Review Activity Log for details".to_string(),
            timestamp: checked_at
                .as_ref()
                .map(|checked| format!("Checked {checked}")),
            spinning: false,
        },
        Phase::Authenticating { purpose, .. } => SidebarPulse {
            tone: PulseTone::Attention,
            headline: "Authorizing…".to_string(),
            detail: format!("Waiting to {purpose}"),
            timestamp: None,
            spinning: true,
        },
        Phase::Checking {
            packages_so_far,
            checked_sources,
            ..
        } => {
            let (_, groups_done, _) = check_progress(checked_sources);
            let found = packages_so_far.len();
            let detail = if found == 0 {
                format!("{groups_done} of 3 sources complete")
            } else {
                format!("{groups_done} of 3 sources · {found} found")
            };
            SidebarPulse {
                tone: PulseTone::Attention,
                headline: "Checking Sources".to_string(),
                detail,
                timestamp: None,
                spinning: true,
            }
        }
        Phase::Ready {
            packages,
            soft_errors,
        } => {
            let count = packages.len();
            let count_text = if count == 1 {
                "1 update".to_string()
            } else {
                format!("{count} updates")
            };
            let detail = match fedora_updater::total_download_bytes(packages) {
                Some(bytes) if bytes > 0 => {
                    format!("{count_text} · {}", fedora_updater::format_bytes(bytes))
                }
                _ => count_text,
            };
            let warning_count = soft_errors.len();
            let timestamp = if warning_count > 0 {
                let noun = if warning_count == 1 {
                    "warning"
                } else {
                    "warnings"
                };
                Some(format!("{warning_count} {noun}"))
            } else if count == 0 {
                None
            } else {
                Some("Ready to review".to_string())
            };
            SidebarPulse {
                tone: if count == 0 && soft_errors.is_empty() {
                    PulseTone::Ready
                } else {
                    PulseTone::Attention
                },
                headline: if count == 0 && !soft_errors.is_empty() {
                    "Check Problem".to_string()
                } else {
                    host_label.to_string()
                },
                detail,
                timestamp,
                spinning: false,
            }
        }
        Phase::Running(running) => {
            let (done, total) = running.done_and_total();
            let pct = (running.overall_progress.clamp(0.0, 1.0) * 100.0).round() as u32;
            SidebarPulse {
                tone: PulseTone::Attention,
                headline: run_heading(&running.phase_label, running.pass).to_string(),
                detail: format!("{pct}% · {done} of {total}"),
                timestamp: running
                    .current_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
                spinning: true,
            }
        }
        Phase::Done {
            packages,
            duration,
            needs_reboot,
            failed,
            ..
        } => {
            let upgraded = packages
                .iter()
                .filter(|p| p.status == fedora_updater::PackageStatus::Completed)
                .count();
            let headline = if *failed > 0 {
                "Finished With Problems"
            } else if *needs_reboot {
                "Restart Pending"
            } else {
                "Updates Complete"
            };
            let detail = if *failed > 0 {
                format!("{upgraded} applied · {failed} failed · {duration}")
            } else {
                format!("{upgraded} upgraded in {duration}")
            };
            SidebarPulse {
                tone: if *failed > 0 || *needs_reboot {
                    PulseTone::Attention
                } else {
                    PulseTone::Ready
                },
                headline: headline.to_string(),
                detail,
                timestamp: (*needs_reboot).then(|| "Restart recommended".to_string()),
                spinning: false,
            }
        }
        Phase::Failed { title, .. } => SidebarPulse {
            tone: PulseTone::Error,
            headline: title.clone(),
            detail: "No changes committed".to_string(),
            timestamp: None,
            spinning: false,
        },
    }
}

/// Shared release-upgrade card rendered either in the sidebar footer or tucked
/// inside the `680px` content clamp in Focused "Lumen" mode.
pub struct UpgradeCard {
    root: gtk::Box,
    title: gtk::Label,
    age: gtk::Label,
    action: gtk::Button,
}

impl UpgradeCard {
    pub fn new_sidebar() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("card");
        root.add_css_class("upgrade-card");
        root.set_visible(false);

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 8);
        inner.set_margin_top(14);
        inner.set_margin_bottom(14);
        inner.set_margin_start(16);
        inner.set_margin_end(16);

        let title = gtk::Label::builder()
            .label("A new Fedora Linux release is available")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["heading"])
            .build();

        let age = gtk::Label::builder()
            .label("")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .visible(false)
            .css_classes(["dimmed", "caption"])
            .build();

        let action = gtk::Button::builder()
            .label("How to Upgrade")
            .hexpand(true)
            .margin_top(2)
            .build();

        inner.append(&title);
        inner.append(&age);
        inner.append(&action);
        root.append(&inner);

        Self {
            root,
            title,
            age,
            action,
        }
    }

    pub fn new_inline() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("card");
        root.add_css_class("upgrade-card");
        root.set_visible(false);

        let inner = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        inner.set_margin_top(14);
        inner.set_margin_bottom(14);
        inner.set_margin_start(18);
        inner.set_margin_end(18);

        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        copy.set_valign(gtk::Align::Center);

        let title = gtk::Label::builder()
            .label("A new Fedora Linux release is available")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["heading"])
            .build();

        let age = gtk::Label::builder()
            .label("")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .visible(false)
            .css_classes(["dimmed", "caption"])
            .build();

        copy.append(&title);
        copy.append(&age);

        let btn_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        btn_content.append(&gtk::Label::new(Some("How to Upgrade")));
        btn_content.append(&gtk::Image::from_icon_name("go-next-symbolic"));

        let action = gtk::Button::builder()
            .child(&btn_content)
            .valign(gtk::Align::Center)
            .build();

        inner.append(&copy);
        inner.append(&action);
        root.append(&inner);

        Self {
            root,
            title,
            age,
            action,
        }
    }

    pub fn paint(&self, version: Option<u32>, days: Option<u32>, visible: bool) {
        let Some(version) = version else {
            self.root.set_visible(false);
            return;
        };
        let heading = banner_title(version);
        let button_name = upgrade_button_name(version);
        self.title.set_label(&heading);
        self.action.set_tooltip_text(Some(&button_name));
        self.action
            .update_property(&[gtk::accessible::Property::Label(&button_name)]);
        match days.map(release_age_caption) {
            Some(caption) => {
                self.age.set_label(&caption);
                self.age.set_visible(true);
            }
            None => self.age.set_visible(false),
        }
        self.root.set_visible(visible);
    }

    pub fn root(&self) -> &gtk::Box {
        &self.root
    }

    pub fn action(&self) -> &gtk::Button {
        &self.action
    }

    #[cfg(test)]
    pub fn title(&self) -> &gtk::Label {
        &self.title
    }

    #[cfg(test)]
    pub fn age(&self) -> &gtk::Label {
        &self.age
    }
}

pub struct Sidebar {
    root: gtk::Box,
    host_label: String,
    pulse_indicator_stack: gtk::Stack,
    pulse_dot: gtk::Box,
    pulse_headline: gtk::Label,
    pulse_detail: gtk::Label,
    pulse_timestamp: gtk::Label,
    #[cfg_attr(not(test), allow(dead_code))] // Kept hidden until Phase 2 (#35) enables throughput samples.
    sparkline_card: gtk::Box,
    #[allow(dead_code)] // Phase 2 (#35) drives live samples into the sparkline.
    sparkline: ThroughputSparkline,
    sources_list: gtk::ListBox,
    row_all: gtk::ListBoxRow,
    row_system: gtk::ListBoxRow,
    row_apps: gtk::ListBoxRow,
    row_firmware: gtk::ListBoxRow,
    count_all: gtk::Label,
    count_system: gtk::Label,
    count_apps: gtk::Label,
    count_firmware: gtk::Label,
    views_list: gtk::ListBox,
    view_updates_row: gtk::ListBoxRow,
    #[allow(dead_code)] // Kept for Phase 2 (#35) Update History reveal and tests.
    view_history_row: gtk::ListBoxRow,
    view_log_row: gtk::ListBoxRow,
    upgrade_footer: gtk::Box,
    upgrade_card: UpgradeCard,
    selected_source: Rc<Cell<SourceFilter>>,
    ignore_nav: Rc<Cell<bool>>,
}

impl Sidebar {
    pub fn new() -> Self {
        let host_label = installed_host_label();

        // System Pulse card (padded 14×16 inner box inside .card).
        let pulse_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pulse_card.add_css_class("card");
        pulse_card.add_css_class("pulse-card");

        let pulse_inner = gtk::Box::new(gtk::Orientation::Vertical, 4);
        pulse_inner.set_margin_top(14);
        pulse_inner.set_margin_bottom(14);
        pulse_inner.set_margin_start(16);
        pulse_inner.set_margin_end(16);

        let pulse_top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        pulse_top.set_valign(gtk::Align::Center);

        let pulse_dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pulse_dot.set_size_request(8, 8);
        pulse_dot.set_halign(gtk::Align::Center);
        pulse_dot.set_valign(gtk::Align::Center);
        pulse_dot.add_css_class("pulse-dot");

        let pulse_spinner = libadwaita::Spinner::builder()
            .width_request(14)
            .height_request(14)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();

        let pulse_indicator_stack = gtk::Stack::builder()
            .width_request(16)
            .height_request(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .build();
        pulse_indicator_stack.add_named(&pulse_dot, Some("dot"));
        pulse_indicator_stack.add_named(&pulse_spinner, Some("spinner"));
        pulse_indicator_stack.set_visible_child_name("dot");

        let pulse_headline = gtk::Label::builder()
            .label(&host_label)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();
        pulse_top.append(&pulse_indicator_stack);
        pulse_top.append(&pulse_headline);

        let pulse_detail = gtk::Label::builder()
            .label("Not checked this session")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["caption", "dimmed", "numeric"])
            .build();

        let pulse_timestamp = gtk::Label::builder()
            .label("")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .visible(false)
            .css_classes(["caption", "dimmed"])
            .build();

        pulse_inner.append(&pulse_top);
        pulse_inner.append(&pulse_detail);
        pulse_inner.append(&pulse_timestamp);
        pulse_card.append(&pulse_inner);

        // Hidden Phase-2 ThroughputSparkline card (zero dead UI in Phase 1).
        let sparkline = ThroughputSparkline::new();
        let sparkline_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sparkline_card.add_css_class("card");
        sparkline_card.set_visible(false);
        let sparkline_inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sparkline_inner.set_margin_top(14);
        sparkline_inner.set_margin_bottom(14);
        sparkline_inner.set_margin_start(16);
        sparkline_inner.set_margin_end(16);
        sparkline_inner.append(sparkline.widget());
        sparkline_card.append(&sparkline_inner);

        // Sources navigation list (All, System, Apps, Firmware).
        let sources_label = section_heading("Sources");
        let sources_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(true)
            .css_classes(["navigation-sidebar"])
            .build();

        let (row_all, count_all) =
            source_row("source:all", "All Updates", None, Some("view-list-symbolic"));
        let (row_system, count_system) =
            source_row("source:system", "System · DNF", Some("src-dnf"), None);
        let (row_apps, count_apps) =
            source_row("source:apps", "Apps · Flatpak", Some("src-flatpak"), None);
        let (row_firmware, count_firmware) =
            source_row("source:firmware", "Firmware · LVFS", Some("src-firmware"), None);
        sources_list.append(&row_all);
        sources_list.append(&row_system);
        sources_list.append(&row_apps);
        sources_list.append(&row_firmware);
        sources_list.select_row(Some(&row_all));

        let sources_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
        sources_section.append(&sources_label);
        sources_section.append(&sources_list);

        // Views navigation list (Updates, Activity Log; Update History hidden in Phase 1).
        let views_label = section_heading("Views");
        let views_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .activate_on_single_click(true)
            .css_classes(["navigation-sidebar"])
            .build();

        let view_updates_row = view_row(
            "view:updates",
            "Updates",
            "software-update-available-symbolic",
        );
        let view_history_row = view_row(
            "view:history",
            "Update History",
            "document-open-recent-symbolic",
        );
        view_history_row.set_visible(false);
        let view_log_row = view_row("view:log", "Activity Log", "utilities-terminal-symbolic");
        views_list.append(&view_updates_row);
        views_list.append(&view_history_row);
        views_list.append(&view_log_row);
        views_list.select_row(Some(&view_updates_row));

        let views_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
        views_section.append(&views_label);
        views_section.append(&views_list);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 14);
        body.set_margin_top(10);
        body.set_margin_bottom(12);
        body.set_margin_start(10);
        body.set_margin_end(10);
        body.append(&pulse_card);
        body.append(&sparkline_card);
        body.append(&sources_section);
        body.append(&views_section);

        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .child(&body)
            .build();

        // Bottom Fedora release upgrade footer.
        let upgrade_card = UpgradeCard::new_sidebar();
        let upgrade_footer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        upgrade_footer.set_margin_start(10);
        upgrade_footer.set_margin_end(10);
        upgrade_footer.set_margin_bottom(12);
        upgrade_footer.set_margin_top(4);
        upgrade_footer.set_visible(false);
        upgrade_footer.append(upgrade_card.root());

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.append(&scroll);
        root.append(&upgrade_footer);

        Self {
            root,
            host_label,
            pulse_indicator_stack,
            pulse_dot,
            pulse_headline,
            pulse_detail,
            pulse_timestamp,
            sparkline_card,
            sparkline,
            sources_list,
            row_all,
            row_system,
            row_apps,
            row_firmware,
            count_all,
            count_system,
            count_apps,
            count_firmware,
            views_list,
            view_updates_row,
            view_history_row,
            view_log_row,
            upgrade_footer,
            upgrade_card,
            selected_source: Rc::new(Cell::new(SourceFilter::All)),
            ignore_nav: Rc::new(Cell::new(false)),
        }
    }

    pub fn root(&self) -> &gtk::Box {
        &self.root
    }

    pub fn upgrade_card(&self) -> &UpgradeCard {
        &self.upgrade_card
    }

    pub fn wire_navigation(
        &self,
        on_source: impl Fn(SourceFilter) + 'static,
        on_updates: impl Fn() + 'static,
        on_activity_log: impl Fn() + 'static,
    ) {
        let selected_source = self.selected_source.clone();
        let ignore_nav = self.ignore_nav.clone();
        self.sources_list.connect_row_activated(move |_, row| {
            if ignore_nav.get() {
                return;
            }
            let filter = match row.widget_name().as_str() {
                "source:system" => SourceFilter::System,
                "source:apps" => SourceFilter::Apps,
                "source:firmware" => SourceFilter::Firmware,
                _ => SourceFilter::All,
            };
            selected_source.set(filter);
            on_source(filter);
        });

        let ignore_nav = self.ignore_nav.clone();
        self.views_list.connect_row_activated(move |_, row| {
            if ignore_nav.get() {
                return;
            }
            match row.widget_name().as_str() {
                "view:log" => on_activity_log(),
                _ => on_updates(),
            }
        });
    }

    pub fn select_source(&self, filter: SourceFilter) {
        self.selected_source.set(filter);
        let row = match filter {
            SourceFilter::All => &self.row_all,
            SourceFilter::System => &self.row_system,
            SourceFilter::Apps => &self.row_apps,
            SourceFilter::Firmware => &self.row_firmware,
        };
        self.ignore_nav.set(true);
        self.sources_list.select_row(Some(row));
        self.ignore_nav.set(false);
    }

    pub fn select_view(&self, view: DetailView) {
        let row = match view {
            DetailView::Updates => &self.view_updates_row,
            DetailView::ActivityLog => &self.view_log_row,
            DetailView::History => &self.view_history_row,
        };
        self.ignore_nav.set(true);
        self.views_list.select_row(Some(row));
        self.ignore_nav.set(false);
    }

    pub fn paint(&self, state: &AppState) {
        let pulse = pulse_summary(state, &self.host_label);
        self.pulse_headline.set_text(&pulse.headline);
        self.pulse_detail.set_text(&pulse.detail);
        if let Some(ts) = pulse.timestamp {
            self.pulse_timestamp.set_text(&ts);
            self.pulse_timestamp.set_visible(true);
        } else {
            self.pulse_timestamp.set_text("");
            self.pulse_timestamp.set_visible(false);
        }

        for class in ["pulse-warn", "pulse-err"] {
            self.pulse_dot.remove_css_class(class);
        }
        match pulse.tone {
            PulseTone::Ready => {}
            PulseTone::Attention => self.pulse_dot.add_css_class("pulse-warn"),
            PulseTone::Error => self.pulse_dot.add_css_class("pulse-err"),
        }
        self.pulse_indicator_stack
            .set_visible_child_name(if pulse.spinning { "spinner" } else { "dot" });

        let packages = state.packages();
        let (dnf, flatpak, firmware) = count_by_source(packages);
        self.count_all.set_text(&packages.len().to_string());
        self.count_system.set_text(&dnf.to_string());
        self.count_apps.set_text(&flatpak.to_string());
        self.count_firmware.set_text(&firmware.to_string());
    }

    pub fn paint_upgrade(&self, version: Option<u32>, days: Option<u32>, visible: bool) {
        self.upgrade_card.paint(version, days, visible);
        self.upgrade_footer
            .set_visible(self.upgrade_card.root().get_visible());
    }

    #[cfg(test)]
    pub fn sparkline_card(&self) -> &gtk::Box {
        &self.sparkline_card
    }

    #[cfg(test)]
    pub fn sparkline(&self) -> &ThroughputSparkline {
        &self.sparkline
    }

    #[cfg(test)]
    pub fn view_history_row(&self) -> &gtk::ListBoxRow {
        &self.view_history_row
    }

    #[cfg(test)]
    pub fn count_texts(&self) -> (String, String, String, String) {
        (
            self.count_all.text().to_string(),
            self.count_system.text().to_string(),
            self.count_apps.text().to_string(),
            self.count_firmware.text().to_string(),
        )
    }

    #[cfg(test)]
    pub fn pulse_headline_text(&self) -> String {
        self.pulse_headline.text().to_string()
    }

    #[cfg(test)]
    pub fn sources_list(&self) -> &gtk::ListBox {
        &self.sources_list
    }

    #[cfg(test)]
    pub fn selected_source(&self) -> SourceFilter {
        self.selected_source.get()
    }

    #[cfg(test)]
    pub fn views_list(&self) -> &gtk::ListBox {
        &self.views_list
    }

    #[cfg(test)]
    pub fn selected_view_name(&self) -> Option<String> {
        self.views_list
            .selected_row()
            .map(|r| r.widget_name().to_string())
    }
}

fn section_heading(title: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .margin_start(10)
        .css_classes(["sidebar-section-label", "caption", "dimmed"])
        .build()
}

fn source_row(
    widget_name: &str,
    label: &str,
    dot_class: Option<&str>,
    icon_name: Option<&str>,
) -> (gtk::ListBoxRow, gtk::Label) {
    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row_box.set_margin_top(6);
    row_box.set_margin_bottom(6);
    row_box.set_margin_start(8);
    row_box.set_margin_end(8);

    if let Some(icon) = icon_name {
        let img = gtk::Image::from_icon_name(icon);
        img.set_valign(gtk::Align::Center);
        row_box.append(&img);
    } else if let Some(dot_cls) = dot_class {
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.set_size_request(8, 8);
        dot.set_valign(gtk::Align::Center);
        dot.set_halign(gtk::Align::Center);
        dot.set_margin_start(4);
        dot.set_margin_end(4);
        dot.add_css_class("source-dot");
        dot.add_css_class(dot_cls);
        row_box.append(&dot);
    }

    let title = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();

    let count = gtk::Label::builder()
        .label("0")
        .valign(gtk::Align::Center)
        .css_classes(["sidebar-count-pill", "caption", "numeric"])
        .build();

    row_box.append(&title);
    row_box.append(&count);

    let row = gtk::ListBoxRow::builder()
        .name(widget_name)
        .child(&row_box)
        .build();
    (row, count)
}

fn view_row(widget_name: &str, label: &str, icon_name: &str) -> gtk::ListBoxRow {
    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row_box.set_margin_top(6);
    row_box.set_margin_bottom(6);
    row_box.set_margin_start(8);
    row_box.set_margin_end(8);

    let img = gtk::Image::from_icon_name(icon_name);
    img.set_valign(gtk::Align::Center);
    let title = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();

    row_box.append(&img);
    row_box.append(&title);

    gtk::ListBoxRow::builder()
        .name(widget_name)
        .child(&row_box)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedora_updater::model::{AuthPurpose, Package, PackageStatus, RetryAction, RunPass, UpdateSource};
    use fedora_updater::state::RunningState;

    #[test]
    fn format_host_label_extracts_fedora_name_and_version_cleanly() {
        let os_ws = "NAME=\"Fedora Linux\"\nVERSION_ID=44\nPRETTY_NAME=\"Fedora Linux 44 (Workstation Edition)\"\n";
        assert_eq!(format_host_label(os_ws), "Fedora Linux 44");

        let os_fallback = "NAME=\"Fedora Linux\"\nVERSION_ID=45\n";
        assert_eq!(format_host_label(os_fallback), "Fedora Linux 45");

        assert_eq!(format_host_label(""), "Fedora Linux");
    }

    #[test]
    fn release_upgrade_visibility_rules_never_leak_into_auth_or_running() {
        let idle = Phase::Idle(IdleScreen::Fresh);
        let auth = Phase::Authenticating {
            purpose: AuthPurpose::Check,
            packages: vec![],
            last_checked: None,
        };
        let checking = Phase::Checking {
            packages_so_far: vec![],
            soft_errors: vec![],
            checked_sources: vec![],
        };
        let ready = Phase::Ready {
            packages: vec![],
            soft_errors: vec![],
        };
        let running = Phase::Running(RunningState::new(vec![]));
        let done = Phase::Done {
            packages: vec![],
            duration: "10s".into(),
            needs_reboot: false,
            failed: 0,
            failure_reasons: vec![],
        };
        let failed = Phase::Failed {
            title: "Err".into(),
            detail: "Detail".into(),
            retry: RetryAction::CheckAgain,
        };

        assert!(release_upgrade_allowed_in_sidebar(&idle));
        assert!(!release_upgrade_allowed_in_sidebar(&auth));
        assert!(release_upgrade_allowed_in_sidebar(&checking));
        assert!(release_upgrade_allowed_in_sidebar(&ready));
        assert!(!release_upgrade_allowed_in_sidebar(&running));
        assert!(release_upgrade_allowed_in_sidebar(&done));
        assert!(release_upgrade_allowed_in_sidebar(&failed));

        assert_eq!(release_upgrade_inline_slot(&idle), InlineUpgradeSlot::Idle);
        assert_eq!(release_upgrade_inline_slot(&auth), InlineUpgradeSlot::None);
        assert_eq!(
            release_upgrade_inline_slot(&checking),
            InlineUpgradeSlot::None
        );
        assert_eq!(
            release_upgrade_inline_slot(&ready),
            InlineUpgradeSlot::Ready
        );
        assert_eq!(
            release_upgrade_inline_slot(&running),
            InlineUpgradeSlot::None
        );
        assert_eq!(release_upgrade_inline_slot(&done), InlineUpgradeSlot::Done);
        assert_eq!(
            release_upgrade_inline_slot(&failed),
            InlineUpgradeSlot::None
        );
    }

    #[test]
    fn pulse_summary_covers_all_phases() {
        let fresh = pulse_summary(&AppState::default(), "Fedora Linux 44");
        assert_eq!(fresh.tone, PulseTone::Attention);
        assert_eq!(fresh.headline, "Fedora Linux 44");
        assert!(!fresh.spinning);

        let current = AppState {
            phase: Phase::Idle(IdleScreen::Current {
                checked_at: "just now".into(),
                detail: None,
            }),
            ..Default::default()
        };
        let p = pulse_summary(&current, "Fedora Linux 44");
        assert_eq!(p.tone, PulseTone::Ready);
        assert_eq!(p.detail, "System is up to date");
        assert_eq!(p.timestamp.as_deref(), Some("Checked just now"));

        let mut pkg = Package::new_dnf("openssl", "x86_64", "3.2.2", "updates");
        pkg.size_bytes = Some(2 * 1024 * 1024);
        let ready = AppState {
            phase: Phase::Ready {
                packages: vec![pkg.clone()],
                soft_errors: vec![],
            },
            ..Default::default()
        };
        let p = pulse_summary(&ready, "Fedora Linux 44");
        assert_eq!(p.tone, PulseTone::Attention);
        assert_eq!(p.detail, "1 update · 2.0 MB");

        let mut running = RunningState::new(vec![pkg.clone()]);
        running.overall_progress = 0.5;
        running.pass = RunPass::Apply;
        running.phase_label = "Installing".into();
        running.current_name = Some("openssl".into());
        let p = pulse_summary(
            &AppState {
                phase: Phase::Running(running),
                ..Default::default()
            },
            "Fedora Linux 44",
        );
        assert!(p.spinning);
        assert_eq!(p.headline, "Applying");
        assert_eq!(p.detail, "50% · 0 of 1");
        assert_eq!(p.timestamp.as_deref(), Some("openssl"));

        let mut done_pkg = pkg;
        done_pkg.status = PackageStatus::Completed;
        let done_reboot = AppState {
            phase: Phase::Done {
                packages: vec![done_pkg],
                duration: "1m 12s".into(),
                needs_reboot: true,
                failed: 0,
                failure_reasons: vec![],
            },
            ..Default::default()
        };
        let p = pulse_summary(&done_reboot, "Fedora Linux 44");
        assert_eq!(p.headline, "Restart Pending");
        assert_eq!(p.timestamp.as_deref(), Some("Restart recommended"));

        let failed = AppState {
            phase: Phase::Failed {
                title: "All updates failed".into(),
                detail: "dnf failed".into(),
                retry: RetryAction::CheckAgain,
            },
            ..Default::default()
        };
        let p = pulse_summary(&failed, "Fedora Linux 44");
        assert_eq!(p.tone, PulseTone::Error);
        assert_eq!(p.headline, "All updates failed");

        let checking = AppState {
            phase: Phase::Checking {
                packages_so_far: vec![Package::new_dnf("firefox", "x86_64", "140", "updates")],
                soft_errors: vec![],
                checked_sources: vec![UpdateSource::Dnf],
            },
            ..Default::default()
        };
        let p = pulse_summary(&checking, "Fedora Linux 44");
        assert!(p.spinning);
        assert_eq!(p.detail, "1 of 3 sources · 1 found");
    }
}
