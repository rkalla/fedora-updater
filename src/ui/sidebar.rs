//! Observatory navigation sidebar frame and Fedora release-upgrade cards.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

use fedora_updater::model::{count_by_source, count_security, DetailView, PackageStatus, UpdateSource};
use fedora_updater::presenter::{about_time_left, format_elapsed, QuietUi};
use fedora_updater::release_upgrade::{banner_title, release_age_caption, upgrade_button_name};
use fedora_updater::state::{AppState, Phase, RunningState};

use super::charts::ThroughputSparkline;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceFilter {
    #[default]
    All,
    Security,
    System,
    Apps,
    Firmware,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineUpgradeSlot {
    None,
    Idle,
    Ready,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRunStatus {
    Skipped,
    Queued,
    Active,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePipelineItem {
    pub label: &'static str,
    pub status: SourceRunStatus,
    pub badge: String,
}

fn build_pipeline_item<F: Fn(UpdateSource) -> bool>(
    running: &RunningState,
    label: &'static str,
    matches_group: F,
) -> SourcePipelineItem {
    let mut total = 0usize;
    let mut done = 0usize;
    let mut failed = 0usize;
    let mut has_live = false;
    for pkg in running.packages.iter().filter(|p| matches_group(p.source)) {
        total += 1;
        match pkg.status {
            PackageStatus::Completed => done += 1,
            PackageStatus::Failed => failed += 1,
            PackageStatus::Downloading | PackageStatus::Installing => has_live = true,
            PackageStatus::Pending => {}
        }
    }
    let is_current = running.current_source.is_some_and(matches_group);
    let (status, badge) = if total == 0 {
        (SourceRunStatus::Skipped, "—".to_string())
    } else if failed > 0 && done + failed == total {
        (SourceRunStatus::Failed, format!("{failed} failed"))
    } else if done == total {
        (SourceRunStatus::Done, format!("{done}/{total} ✓"))
    } else if is_current || has_live {
        (SourceRunStatus::Active, format!("{done}/{total}"))
    } else {
        (SourceRunStatus::Queued, "Queued".to_string())
    };
    SourcePipelineItem {
        label,
        status,
        badge,
    }
}

pub fn running_source_checklist(running: &RunningState) -> [SourcePipelineItem; 3] {
    [
        build_pipeline_item(running, "System · DNF", |s| s == UpdateSource::Dnf),
        build_pipeline_item(running, "Apps · Flatpak", |s| {
            matches!(s, UpdateSource::FlatpakUser | UpdateSource::FlatpakSystem)
        }),
        build_pipeline_item(running, "Firmware · LVFS", |s| s == UpdateSource::Firmware),
    ]
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
    sparkline_card: gtk::Box,
    sparkline: ThroughputSparkline,
    sources_list: gtk::ListBox,
    row_all: gtk::ListBoxRow,
    row_security: gtk::ListBoxRow,
    row_system: gtk::ListBoxRow,
    row_apps: gtk::ListBoxRow,
    row_firmware: gtk::ListBoxRow,
    count_all: gtk::Label,
    count_security: gtk::Label,
    count_system: gtk::Label,
    count_apps: gtk::Label,
    count_firmware: gtk::Label,
    nav_context_stack: gtk::Stack,
    pipeline_rows: [(gtk::Box, gtk::Label); 3],
    telemetry_elapsed: gtk::Label,
    telemetry_eta: gtk::Label,
    views_list: gtk::ListBox,
    view_updates_row: gtk::ListBoxRow,
    view_history_row: gtk::ListBoxRow,
    view_log_row: gtk::ListBoxRow,
    upgrade_footer: gtk::Box,
    upgrade_card: UpgradeCard,
    selected_source: Rc<Cell<SourceFilter>>,
    ignore_nav: Rc<Cell<bool>>,
}

impl Sidebar {
    pub fn new() -> Self {
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
        let (row_security, count_security) =
            source_row("source:security", "Security Fixes", Some("src-sec"), None);
        row_security.set_visible(false);
        let (row_system, count_system) =
            source_row("source:system", "System · DNF", Some("src-dnf"), None);
        let (row_apps, count_apps) =
            source_row("source:apps", "Apps · Flatpak", Some("src-flatpak"), None);
        let (row_firmware, count_firmware) =
            source_row("source:firmware", "Firmware · LVFS", Some("src-firmware"), None);
        sources_list.append(&row_all);
        sources_list.append(&row_security);
        sources_list.append(&row_system);
        sources_list.append(&row_apps);
        sources_list.append(&row_firmware);
        sources_list.select_row(Some(&row_all));

        let sources_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
        sources_section.append(&sources_label);
        sources_section.append(&sources_list);

        // Running Pipeline Telemetry (Execution Queue + Elapsed/ETA card).
        let pipeline_heading = section_heading("Execution Queue");
        let pipeline_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let pipe_sys = pipeline_row("System · DNF", "src-dnf");
        let pipe_apps = pipeline_row("Apps · Flatpak", "src-flatpak");
        let pipe_fw = pipeline_row("Firmware · LVFS", "src-firmware");
        pipeline_list.append(&pipe_sys.0);
        pipeline_list.append(&pipe_apps.0);
        pipeline_list.append(&pipe_fw.0);

        let telemetry_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        telemetry_card.add_css_class("card");
        let telemetry_inner = gtk::Box::new(gtk::Orientation::Vertical, 4);
        telemetry_inner.set_margin_top(14);
        telemetry_inner.set_margin_bottom(14);
        telemetry_inner.set_margin_start(16);
        telemetry_inner.set_margin_end(16);

        let elapsed_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let elapsed_title = gtk::Label::builder()
            .label("Elapsed")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["caption", "dimmed"])
            .build();
        let telemetry_elapsed = gtk::Label::builder()
            .label("0s")
            .xalign(1.0)
            .halign(gtk::Align::End)
            .css_classes(["caption", "numeric", "heading"])
            .build();
        elapsed_row.append(&elapsed_title);
        elapsed_row.append(&telemetry_elapsed);

        let telemetry_eta = gtk::Label::builder()
            .label("")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .visible(false)
            .css_classes(["caption", "dimmed"])
            .build();

        telemetry_inner.append(&elapsed_row);
        telemetry_inner.append(&telemetry_eta);
        telemetry_card.append(&telemetry_inner);

        let pipeline_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        pipeline_section.append(&pipeline_heading);
        pipeline_section.append(&pipeline_list);
        pipeline_section.append(&telemetry_card);

        let nav_context_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .vhomogeneous(false)
            .build();
        nav_context_stack.add_named(&sources_section, Some("sources"));
        nav_context_stack.add_named(&pipeline_section, Some("pipeline"));
        nav_context_stack.set_visible_child_name("sources");

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
        body.append(&sparkline_card);
        body.append(&nav_context_stack);
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
            sparkline_card,
            sparkline,
            sources_list,
            row_all,
            row_security,
            row_system,
            row_apps,
            row_firmware,
            count_all,
            count_security,
            count_system,
            count_apps,
            count_firmware,
            nav_context_stack,
            pipeline_rows: [pipe_sys, pipe_apps, pipe_fw],
            telemetry_elapsed,
            telemetry_eta,
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
        on_history: impl Fn() + 'static,
        on_activity_log: impl Fn() + 'static,
    ) {
        let selected_source = self.selected_source.clone();
        let ignore_nav = self.ignore_nav.clone();
        self.sources_list.connect_row_activated(move |_, row| {
            if ignore_nav.get() {
                return;
            }
            let filter = match row.widget_name().as_str() {
                "source:security" => SourceFilter::Security,
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
                "view:history" => on_history(),
                "view:log" => on_activity_log(),
                _ => on_updates(),
            }
        });
    }

    pub fn select_source(&self, filter: SourceFilter) {
        self.selected_source.set(filter);
        let row = match filter {
            SourceFilter::All => &self.row_all,
            SourceFilter::Security => &self.row_security,
            SourceFilter::System => &self.row_system,
            SourceFilter::Apps => &self.row_apps,
            SourceFilter::Firmware => &self.row_firmware,
        };
        self.ignore_nav.set(true);
        self.sources_list.select_row(Some(row));
        self.ignore_nav.set(false);
    }

    pub fn set_history_available(&self, available: bool) {
        self.view_history_row.set_visible(available);
    }

    pub fn push_throughput_sample(&self, bytes_per_sec: f64, transferred: Option<(u64, u64)>) {
        self.sparkline.set_visible(true);
        self.sparkline_card.set_visible(true);
        self.sparkline.push_sample(bytes_per_sec, transferred);
    }

    pub fn reset_throughput(&self) {
        self.sparkline.clear();
        self.sparkline.set_visible(false);
        self.sparkline_card.set_visible(false);
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
        let packages = state.packages();
        let (dnf, flatpak, firmware) = count_by_source(packages);
        let sec = count_security(packages);
        self.count_all.set_text(&packages.len().to_string());
        self.count_security.set_text(&sec.to_string());
        self.row_security.set_visible(sec > 0);
        if sec == 0 && self.selected_source.get() == SourceFilter::Security {
            self.select_source(SourceFilter::All);
        }
        self.count_system.set_text(&dnf.to_string());
        self.count_apps.set_text(&flatpak.to_string());
        self.count_firmware.set_text(&firmware.to_string());

        if let Phase::Running(running) = &state.phase {
            self.nav_context_stack.set_visible_child_name("pipeline");
            let items = running_source_checklist(running);
            for ((row_box, pill), item) in self.pipeline_rows.iter().zip(items.iter()) {
                row_box.remove_css_class("active");
                for cls in ["pipeline-pill-active", "pipeline-pill-done", "pipeline-pill-error"] {
                    pill.remove_css_class(cls);
                }
                pill.set_text(&item.badge);
                match item.status {
                    SourceRunStatus::Active => {
                        row_box.add_css_class("active");
                        pill.add_css_class("pipeline-pill-active");
                    }
                    SourceRunStatus::Done => pill.add_css_class("pipeline-pill-done"),
                    SourceRunStatus::Failed => pill.add_css_class("pipeline-pill-error"),
                    SourceRunStatus::Skipped | SourceRunStatus::Queued => {}
                }
            }
        } else {
            self.nav_context_stack.set_visible_child_name("sources");
        }
    }

    pub fn paint_running_telemetry(
        &self,
        elapsed_secs: u64,
        quiet: &QuietUi,
        overall_progress: f64,
    ) {
        self.telemetry_elapsed
            .set_text(&format_elapsed(elapsed_secs));
        let eta_text = if let Some(note) = quiet.footnote.as_deref() {
            Some(note.to_string())
        } else if quiet.show_eta {
            about_time_left(elapsed_secs, overall_progress)
        } else {
            None
        };
        if let Some(eta) = eta_text.as_deref().filter(|s| !s.is_empty()) {
            self.telemetry_eta.set_text(eta);
            self.telemetry_eta.set_visible(true);
        } else {
            self.telemetry_eta.set_text("");
            self.telemetry_eta.set_visible(false);
        }
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
    pub fn row_security(&self) -> &gtk::ListBoxRow {
        &self.row_security
    }

    #[cfg(test)]
    pub fn count_security_text(&self) -> String {
        self.count_security.text().to_string()
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
    pub fn nav_context_name(&self) -> Option<String> {
        self.nav_context_stack
            .visible_child_name()
            .map(|s| s.to_string())
    }

    #[cfg(test)]
    pub fn pipeline_status_texts(&self) -> [String; 3] {
        [
            self.pipeline_rows[0].1.text().to_string(),
            self.pipeline_rows[1].1.text().to_string(),
            self.pipeline_rows[2].1.text().to_string(),
        ]
    }

    #[cfg(test)]
    pub fn telemetry_elapsed_text(&self) -> String {
        self.telemetry_elapsed.text().to_string()
    }

    #[cfg(test)]
    pub fn telemetry_eta_text(&self) -> Option<String> {
        self.telemetry_eta
            .get_visible()
            .then(|| self.telemetry_eta.text().to_string())
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

fn pipeline_row(label: &str, dot_class: &str) -> (gtk::Box, gtk::Label) {
    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row_box.set_margin_top(5);
    row_box.set_margin_bottom(5);
    row_box.set_margin_start(8);
    row_box.set_margin_end(8);
    row_box.add_css_class("pipeline-row");

    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.set_size_request(8, 8);
    dot.set_valign(gtk::Align::Center);
    dot.set_halign(gtk::Align::Center);
    dot.set_margin_start(4);
    dot.set_margin_end(4);
    dot.add_css_class("source-dot");
    dot.add_css_class(dot_class);

    let title = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();

    let pill = gtk::Label::builder()
        .label("Queued")
        .valign(gtk::Align::Center)
        .css_classes(["sidebar-count-pill", "caption", "numeric"])
        .build();

    row_box.append(&dot);
    row_box.append(&title);
    row_box.append(&pill);
    (row_box, pill)
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
    use fedora_updater::model::{AuthPurpose, Package, PackageStatus, RetryAction, UpdateSource};
    use fedora_updater::state::RunningState;
    use fedora_updater::IdleScreen;

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
    fn running_source_checklist_reports_active_done_queued_failed_and_skipped() {
        let mut d1 = Package::new_dnf("openssl", "x86_64", "1", "updates");
        d1.status = PackageStatus::Completed;
        let mut d2 = Package::new_dnf("kernel", "x86_64", "2", "updates");
        d2.status = PackageStatus::Installing;

        let mut fp = Package::new_dnf("Thunderbird", "", "128", "flathub");
        fp.source = UpdateSource::FlatpakUser;
        fp.status = PackageStatus::Pending;

        let mut running = RunningState::new(vec![d1.clone(), d2, fp.clone()]);
        running.current_source = Some(UpdateSource::Dnf);
        let items = running_source_checklist(&running);
        assert_eq!(items[0].status, SourceRunStatus::Active);
        assert_eq!(items[0].badge, "1/2");
        assert_eq!(items[1].status, SourceRunStatus::Queued);
        assert_eq!(items[1].badge, "Queued");
        assert_eq!(items[2].status, SourceRunStatus::Skipped);
        assert_eq!(items[2].badge, "—");

        let mut fw_fail = Package::new_dnf("UEFI dbx", "", "2", "LVFS");
        fw_fail.source = UpdateSource::Firmware;
        fw_fail.status = PackageStatus::Failed;

        let mut fp_active = fp;
        fp_active.status = PackageStatus::Installing;
        let mut running2 = RunningState::new(vec![d1, fp_active, fw_fail]);
        running2.current_source = Some(UpdateSource::FlatpakUser);
        let items2 = running_source_checklist(&running2);
        assert_eq!(items2[0].status, SourceRunStatus::Done);
        assert_eq!(items2[0].badge, "1/1 ✓");
        assert_eq!(items2[1].status, SourceRunStatus::Active);
        assert_eq!(items2[1].badge, "0/1");
        assert_eq!(items2[2].status, SourceRunStatus::Failed);
        assert_eq!(items2[2].badge, "1 failed");
    }
}
