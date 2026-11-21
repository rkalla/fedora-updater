//! Adwaita rows and group refill helpers.

use gtk::prelude::*;
use gtk::{Align, Label};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, ExpanderRow, PreferencesGroup};

use fedora_updater::model::{Package, PackageStatus, UpdateSource};

pub fn source_badge(source: UpdateSource) -> Label {
    Label::builder()
        .label(source.badge())
        .width_chars(3)
        .valign(Align::Center)
        .css_classes(["package-badge", source.css_class()])
        .build()
}

fn leading(package: &Package) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    box_.set_valign(Align::Center);
    box_.append(&source_badge(package.source));
    box_
}

fn caption_suffix(text: &str, extra: &[&str]) -> Label {
    let mut classes = vec!["caption"];
    classes.extend_from_slice(extra);
    Label::builder()
        .label(text)
        .valign(Align::Center)
        .css_classes(classes)
        .build()
}

fn action_row(package: &Package) -> ActionRow {
    let subtitle = format!("{} · {}", package.version_line(), package.source.label());
    let row = ActionRow::builder()
        .title(&package.name)
        .subtitle(&subtitle)
        .title_lines(1)
        .subtitle_lines(2)
        .activatable(false)
        .build();
    row.add_css_class("pkg-row");
    row.add_prefix(&leading(package));
    row
}

fn apply_status_style(
    row: &ActionRow,
    label: &Label,
    status: PackageStatus,
    is_current: bool,
    fade_done: bool,
) {
    for class in ["pending", "active", "done"] {
        row.remove_css_class(class);
    }
    for class in ["dimmed", "accent", "success", "error", "phase-tag"] {
        label.remove_css_class(class);
    }

    match status {
        PackageStatus::Downloading | PackageStatus::Installing => {
            if is_current {
                row.add_css_class("active");
            }
            label.add_css_class("accent");
            label.add_css_class("phase-tag");
            label.set_text(status.label());
        }
        PackageStatus::Pending => {
            row.add_css_class("pending");
            label.add_css_class("dimmed");
            label.set_text("queued");
        }
        PackageStatus::Failed => {
            label.add_css_class("error");
            label.set_text("failed");
        }
        PackageStatus::Completed => {
            if fade_done {
                row.add_css_class("done");
            }
            label.add_css_class("success");
            label.set_text("done");
        }
    }
}

fn add_status_caption(row: &ActionRow, status: PackageStatus) {
    let label = caption_suffix("", &[]);
    apply_status_style(row, &label, status, true, false);
    row.add_suffix(&label);
}

pub fn package_row(package: &Package, show_status: bool) -> ActionRow {
    let row = action_row(package);

    if show_status {
        add_status_caption(&row, package.status);
    } else {
        let trailing = package
            .size_label()
            .unwrap_or_else(|| package.source.badge().into());
        row.add_suffix(&caption_suffix(&trailing, &["dimmed"]));
    }

    row
}

/// Stable apply-list row: status caption is updated in place, no per-row bar.
pub struct ChecklistRow {
    pub row: ActionRow,
    status: Label,
}

pub fn checklist_row(package: &Package) -> ChecklistRow {
    let row = action_row(package);
    let status = caption_suffix("", &[]);
    row.add_suffix(&status);
    let built = ChecklistRow { row, status };
    sync_checklist_row(&built, package, false);
    built
}

pub fn sync_checklist_row(row: &ChecklistRow, package: &Package, is_current: bool) {
    apply_status_style(&row.row, &row.status, package.status, is_current, true);
}

/// Finished package on the Done list and in the running completed expander.
/// Source badge, name, version and source, and size only when known.
///
/// `mark_completed` adds the green "done" word. The running expander uses it.
/// The Installed review does not: the header already says the apply finished,
/// and the word sits in the middle of a wrapped subtitle. Failed rows still
/// name their status.
pub fn finished_row(package: &Package, mark_completed: bool) -> ActionRow {
    let row = action_row(package);
    if mark_completed || package.status != PackageStatus::Completed {
        add_status_caption(&row, package.status);
    }
    if let Some(size) = package.size_label() {
        row.add_suffix(&caption_suffix(&size, &["dimmed"]));
    }
    row
}

pub fn refill_group(
    group: &PreferencesGroup,
    store: &std::cell::RefCell<Vec<gtk::Widget>>,
    rows: impl IntoIterator<Item = ActionRow>,
) {
    for child in store.borrow().iter() {
        group.remove(child);
    }
    store.borrow_mut().clear();
    for row in rows {
        let widget: gtk::Widget = row.upcast();
        group.add(&widget);
        store.borrow_mut().push(widget);
    }
}

pub fn refill_expander(
    expander: &ExpanderRow,
    store: &std::cell::RefCell<Vec<gtk::Widget>>,
    rows: impl IntoIterator<Item = ActionRow>,
) {
    for child in store.borrow().iter() {
        expander.remove(child);
    }
    store.borrow_mut().clear();
    append_expander(expander, store, rows);
}

pub fn append_expander(
    expander: &ExpanderRow,
    store: &std::cell::RefCell<Vec<gtk::Widget>>,
    rows: impl IntoIterator<Item = ActionRow>,
) {
    for row in rows {
        let widget: gtk::Widget = row.upcast();
        expander.add_row(&widget);
        store.borrow_mut().push(widget);
    }
}
