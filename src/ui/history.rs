//! Embedded searchable/filterable `Update History` timeline over merged DNF5 + `fwupd` records.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use libadwaita::prelude::*;

use fedora_updater::backend::dnf_dbus::{HistoryEntry, HistorySource};
use fedora_updater::presenter::history_subtitle;

/// Source toggle options shown in the Update History header bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistorySourceFilter {
    #[default]
    All,
    System,
    Firmware,
}

impl HistorySourceFilter {
    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("dnf") => Self::System,
            Some("fwupd") => Self::Firmware,
            _ => Self::All,
        }
    }
}

/// Pure source + multi-token case-insensitive query predicate for history rows.
pub fn entry_matches_filter(
    entry: &HistoryEntry,
    source: HistorySourceFilter,
    query: &str,
) -> bool {
    let source_ok = match source {
        HistorySourceFilter::All => true,
        HistorySourceFilter::System => entry.source == HistorySource::Dnf,
        HistorySourceFilter::Firmware => entry.source == HistorySource::Firmware,
    };
    if !source_ok {
        return false;
    }

    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }

    trimmed.split_whitespace().all(|raw| {
        let tok = raw.to_ascii_lowercase();
        if entry.title.to_ascii_lowercase().contains(&tok)
            || entry.description.to_ascii_lowercase().contains(&tok)
            || entry.status.to_ascii_lowercase().contains(&tok)
            || entry.source.badge().to_ascii_lowercase().contains(&tok)
        {
            return true;
        }
        entry.changes.iter().any(|ch| {
            ch.name.to_ascii_lowercase().contains(&tok)
                || ch.evr.to_ascii_lowercase().contains(&tok)
                || ch
                    .original_evr
                    .as_deref()
                    .is_some_and(|old| old.to_ascii_lowercase().contains(&tok))
        })
    })
}

/// Format a UNIX seconds timestamp using local GLib time (`YYYY-MM-DD HH:MM`).
pub fn format_history_timestamp(ts_secs: u64) -> String {
    if ts_secs == 0 {
        return "Unknown time".into();
    }
    if let Ok(sec) = i64::try_from(ts_secs) {
        if let Ok(dt) = glib::DateTime::from_unix_local(sec) {
            if let Ok(text) = dt.format("%Y-%m-%d %H:%M") {
                return text.to_string();
            }
        }
    }
    format!("{ts_secs}s")
}

/// Embedded Update History timeline workspace rendered inside `DetailView::History`.
#[derive(Clone)]
pub struct HistoryView {
    root: gtk::Box,
    source_toggles: libadwaita::ToggleGroup,
    search_entry: gtk::SearchEntry,
    refresh_btn: gtk::Button,
    content_stack: gtk::Stack,
    list_group: libadwaita::PreferencesGroup,
    empty_page: libadwaita::StatusPage,
    status_label: gtk::Label,
    entries: Rc<RefCell<Vec<HistoryEntry>>>,
    rendered_rows: Rc<RefCell<Vec<libadwaita::ExpanderRow>>>,
    active_filter: Rc<Cell<HistorySourceFilter>>,
    query: Rc<RefCell<String>>,
    matched_count: Rc<Cell<usize>>,
    total_count: Rc<Cell<usize>>,
}

impl HistoryView {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        bar.set_margin_top(10);
        bar.set_margin_bottom(10);
        bar.set_margin_start(14);
        bar.set_margin_end(14);

        let source_toggles = libadwaita::ToggleGroup::new();
        for (name, label) in [("all", "All"), ("dnf", "System"), ("fwupd", "Firmware")] {
            let toggle = libadwaita::Toggle::builder().name(name).label(label).build();
            source_toggles.add(toggle);
        }
        source_toggles.set_active_name(Some("all"));

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Filter history…")
            .hexpand(true)
            .search_delay(80)
            .build();

        let refresh_btn = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh History")
            .valign(gtk::Align::Center)
            .build();
        refresh_btn.update_property(&[gtk::accessible::Property::Label("Refresh Update History")]);

        bar.append(&source_toggles);
        bar.append(&search_entry);
        bar.append(&refresh_btn);

        let loading_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        loading_box.set_valign(gtk::Align::Center);
        loading_box.set_halign(gtk::Align::Center);
        loading_box.set_vexpand(true);
        let spinner = libadwaita::Spinner::builder()
            .width_request(24)
            .height_request(24)
            .halign(gtk::Align::Center)
            .build();
        let loading_lbl = gtk::Label::builder()
            .label("Loading update history…")
            .css_classes(["dimmed"])
            .build();
        loading_box.append(&spinner);
        loading_box.append(&loading_lbl);

        let empty_page = libadwaita::StatusPage::builder()
            .icon_name("document-open-recent-symbolic")
            .title("No recorded update history")
            .description("Completed system RPM and firmware updates appear here.")
            .vexpand(true)
            .build();

        let list_group = libadwaita::PreferencesGroup::new();
        let list_clamp = libadwaita::Clamp::builder()
            .maximum_size(680)
            .tightening_threshold(500)
            .margin_top(12)
            .margin_bottom(14)
            .margin_start(12)
            .margin_end(12)
            .child(&list_group)
            .build();
        let list_scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .child(&list_clamp)
            .build();

        let content_stack = gtk::Stack::builder()
            .vexpand(true)
            .hexpand(true)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        content_stack.add_named(&loading_box, Some("loading"));
        content_stack.add_named(&empty_page, Some("empty"));
        content_stack.add_named(&list_scroll, Some("list"));
        content_stack.set_visible_child_name("empty");

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer.set_margin_top(6);
        footer.set_margin_bottom(6);
        footer.set_margin_start(14);
        footer.set_margin_end(14);

        let status_label = gtk::Label::builder()
            .label("No recorded transactions")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["caption", "dimmed", "numeric"])
            .build();
        footer.append(&status_label);

        root.append(&bar);
        root.append(&content_stack);
        root.append(&footer);

        Self {
            root,
            source_toggles,
            search_entry,
            refresh_btn,
            content_stack,
            list_group,
            empty_page,
            status_label,
            entries: Rc::new(RefCell::new(Vec::new())),
            rendered_rows: Rc::new(RefCell::new(Vec::new())),
            active_filter: Rc::new(Cell::new(HistorySourceFilter::All)),
            query: Rc::new(RefCell::new(String::new())),
            matched_count: Rc::new(Cell::new(0)),
            total_count: Rc::new(Cell::new(0)),
        }
    }

    pub fn root(&self) -> &gtk::Box {
        &self.root
    }

    pub fn search_entry(&self) -> &gtk::SearchEntry {
        &self.search_entry
    }

    pub fn set_loading(&self, loading: bool) {
        self.refresh_btn.set_sensitive(!loading);
        if loading {
            self.content_stack.set_visible_child_name("loading");
            self.status_label.set_label("Loading…");
        }
    }

    pub fn set_entries(&self, entries: Vec<HistoryEntry>) -> (usize, usize) {
        *self.entries.borrow_mut() = entries;
        self.refresh_btn.set_sensitive(true);
        self.rebuild_list()
    }

    pub fn counts(&self) -> (usize, usize) {
        (self.matched_count.get(), self.total_count.get())
    }

    fn rebuild_list(&self) -> (usize, usize) {
        {
            let mut rows = self.rendered_rows.borrow_mut();
            for row in rows.drain(..) {
                self.list_group.remove(&row);
            }
        }

        let entries = self.entries.borrow();
        let total = entries.len();
        let source = self.active_filter.get();
        let query = self.query.borrow();

        let mut matched = 0usize;
        let mut new_rows = Vec::new();
        for entry in entries.iter() {
            if !entry_matches_filter(entry, source, &query) {
                continue;
            }
            matched += 1;
            let row = build_history_row(entry);
            self.list_group.add(&row);
            new_rows.push(row);
        }
        *self.rendered_rows.borrow_mut() = new_rows;

        self.matched_count.set(matched);
        self.total_count.set(total);
        self.status_label.set_label(&history_subtitle(matched, total));

        if total == 0 {
            self.empty_page.set_title("No recorded update history");
            self.empty_page
                .set_description(Some("Completed system RPM and firmware updates appear here."));
            self.content_stack.set_visible_child_name("empty");
        } else if matched == 0 {
            self.empty_page.set_title("No matching history entries");
            self.empty_page
                .set_description(Some("Adjust the source toggle or search query."));
            self.content_stack.set_visible_child_name("empty");
        } else {
            self.content_stack.set_visible_child_name("list");
        }

        (matched, total)
    }

    pub fn wire(
        &self,
        on_refresh: impl Fn() + 'static,
        on_filter_changed: impl Fn(usize, usize) + 'static,
    ) {
        let on_changed = Rc::new(on_filter_changed);

        {
            let view = self.clone();
            let cb = on_changed.clone();
            self.source_toggles.connect_active_name_notify(move |tg| {
                let next = HistorySourceFilter::from_name(tg.active_name().as_deref());
                view.active_filter.set(next);
                let (matched, total) = view.rebuild_list();
                cb(matched, total);
            });
        }

        {
            let view = self.clone();
            let cb = on_changed;
            self.search_entry.connect_search_changed(move |entry| {
                *view.query.borrow_mut() = entry.text().to_string();
                let (matched, total) = view.rebuild_list();
                cb(matched, total);
            });
        }

        {
            let entry = self.search_entry.clone();
            entry.connect_stop_search(move |e| {
                if !e.text().is_empty() {
                    e.set_text("");
                }
            });
        }

        self.refresh_btn.connect_clicked(move |_| on_refresh());
    }

    #[cfg(test)]
    pub fn visible_child_name(&self) -> Option<String> {
        self.content_stack
            .visible_child_name()
            .map(|s| s.to_string())
    }

    #[cfg(test)]
    pub fn active_filter(&self) -> HistorySourceFilter {
        self.active_filter.get()
    }

    #[cfg(test)]
    pub fn select_source_for_test(&self, filter_name: &str) {
        self.source_toggles.set_active_name(Some(filter_name));
    }

    #[cfg(test)]
    pub fn set_search_for_test(&self, text: &str) {
        self.search_entry.set_text(text);
        *self.query.borrow_mut() = text.to_string();
        self.rebuild_list();
    }

    #[cfg(test)]
    pub fn refresh_btn(&self) -> &gtk::Button {
        &self.refresh_btn
    }
}

fn build_history_row(entry: &HistoryEntry) -> libadwaita::ExpanderRow {
    let badge = gtk::Label::builder()
        .label(entry.source.badge())
        .width_chars(3)
        .valign(gtk::Align::Center)
        .css_classes(["package-badge", entry.source.css_class()])
        .build();

    let ts = format_history_timestamp(entry.timestamp);
    let subtitle = if entry.description.trim().is_empty() {
        ts
    } else {
        format!("{ts} · {}", entry.description.trim())
    };

    let row = libadwaita::ExpanderRow::builder()
        .title(glib::markup_escape_text(&entry.title))
        .subtitle(glib::markup_escape_text(&subtitle))
        .enable_expansion(!entry.changes.is_empty())
        .build();
    row.add_prefix(&badge);

    if entry.changes.len() > 1 {
        let count_pill = gtk::Label::builder()
            .label(format!("{} pkgs", entry.changes.len()))
            .valign(gtk::Align::Center)
            .css_classes(["sidebar-count-pill", "caption", "numeric"])
            .build();
        row.add_suffix(&count_pill);
    }

    let status_cls = if entry.succeeded { "dimmed" } else { "error" };
    let status_lbl = gtk::Label::builder()
        .label(&entry.status)
        .valign(gtk::Align::Center)
        .css_classes(["caption", "numeric", status_cls])
        .build();
    row.add_suffix(&status_lbl);

    for ch in &entry.changes {
        let sub_parts = [ch.action.as_str(), ch.arch.as_str()]
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let child = libadwaita::ActionRow::builder()
            .title(glib::markup_escape_text(&ch.name))
            .subtitle(glib::markup_escape_text(&sub_parts))
            .build();
        let ver_text = ch.display_version_change();
        if !ver_text.is_empty() {
            let ver_lbl = gtk::Label::builder()
                .label(&ver_text)
                .valign(gtk::Align::Center)
                .css_classes(["caption", "dimmed", "ver-diff", "numeric"])
                .build();
            child.add_suffix(&ver_lbl);
        }
        row.add_row(&child);
    }

    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedora_updater::backend::dnf_dbus::HistoryPackageChange;

    fn sample_entries() -> Vec<HistoryEntry> {
        vec![
            HistoryEntry {
                id: "dnf-txn-203".into(),
                source: HistorySource::Dnf,
                timestamp: 1_795_224_552,
                title: "openssl + 1 more".into(),
                description: "dnf update -y".into(),
                status: "Ok".into(),
                succeeded: true,
                changes: vec![
                    HistoryPackageChange {
                        name: "openssl".into(),
                        arch: "x86_64".into(),
                        evr: "3.2.2-3.fc44".into(),
                        original_evr: Some("3.2.2-1.fc44".into()),
                        action: "Upgraded".into(),
                    },
                    HistoryPackageChange {
                        name: "exfatprogs".into(),
                        arch: "x86_64".into(),
                        evr: "1.4.4-1.fc44".into(),
                        original_evr: Some("1.4.3-1.fc44".into()),
                        action: "Upgraded".into(),
                    },
                ],
            },
            HistoryEntry {
                id: "fwupd-uefi".into(),
                source: HistorySource::Firmware,
                timestamp: 1_795_150_000,
                title: "System Firmware".into(),
                description: "uefi_capsule".into(),
                status: "Restart Required".into(),
                succeeded: true,
                changes: vec![HistoryPackageChange {
                    name: "System Firmware".into(),
                    arch: String::new(),
                    evr: "0.1.33".into(),
                    original_evr: Some("0.1.32".into()),
                    action: "Firmware".into(),
                }],
            },
        ]
    }

    #[test]
    fn history_filtering_and_timestamp_formatting() {
        let entries = sample_entries();
        assert!(entry_matches_filter(
            &entries[0],
            HistorySourceFilter::All,
            ""
        ));
        assert!(entry_matches_filter(
            &entries[0],
            HistorySourceFilter::System,
            "exfat"
        ));
        assert!(!entry_matches_filter(
            &entries[0],
            HistorySourceFilter::Firmware,
            ""
        ));
        assert!(entry_matches_filter(
            &entries[1],
            HistorySourceFilter::Firmware,
            "uefi"
        ));
        assert!(!entry_matches_filter(
            &entries[1],
            HistorySourceFilter::All,
            "openssl"
        ));

        assert_eq!(format_history_timestamp(0), "Unknown time");
        let ts = format_history_timestamp(1_795_224_552);
        assert!(ts.contains('-') && ts.contains(':'), "got {ts:?}");
    }
}
