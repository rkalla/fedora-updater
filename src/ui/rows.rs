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

fn add_status_caption(row: &ActionRow, status: PackageStatus) {
    match status {
        PackageStatus::Downloading | PackageStatus::Installing => {
            row.add_css_class("active");
            row.add_suffix(&caption_suffix(status.label(), &["accent", "phase-tag"]));
        }
        PackageStatus::Pending => {
            row.add_css_class("pending");
            row.add_suffix(&caption_suffix("queued", &["dimmed"]));
        }
        PackageStatus::Failed => {
            row.add_suffix(&caption_suffix("failed", &["error"]));
        }
        PackageStatus::Completed => {
            row.add_suffix(&caption_suffix("done", &["success"]));
        }
        PackageStatus::Skipped => {
            row.add_css_class("pending");
            row.add_suffix(&caption_suffix("skipped", &["dimmed"]));
        }
    }
}

pub fn package_row(package: &Package, show_status: bool) -> ActionRow {
    let row = action_row(package);

    if show_status {
        add_status_caption(&row, package.status);
    } else {
        let trailing = package
            .size
            .clone()
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

    let status = caption_suffix("", &[]);
    row.add_suffix(&status);
    let built = ChecklistRow { row, status };
    sync_checklist_row(&built, package, false);
    built
}

pub fn sync_checklist_row(row: &ChecklistRow, package: &Package, is_current: bool) {
    row.row.remove_css_class("pending");
    row.row.remove_css_class("active");
    row.row.remove_css_class("done");
    row.status.remove_css_class("dimmed");
    row.status.remove_css_class("accent");
    row.status.remove_css_class("success");
    row.status.remove_css_class("error");
    row.status.remove_css_class("phase-tag");

    let live = is_current
        && matches!(
            package.status,
            PackageStatus::Downloading | PackageStatus::Installing
        );
    if live {
        row.row.add_css_class("active");
        row.status.add_css_class("accent");
        row.status.add_css_class("phase-tag");
        row.status.set_text(package.status.label());
        return;
    }

    match package.status {
        PackageStatus::Completed => {
            row.row.add_css_class("done");
            row.status.add_css_class("success");
            row.status.set_text("done");
        }
        PackageStatus::Failed => {
            row.status.add_css_class("error");
            row.status.set_text("failed");
        }
        PackageStatus::Skipped => {
            row.row.add_css_class("pending");
            row.status.add_css_class("dimmed");
            row.status.set_text("skipped");
        }
        PackageStatus::Installing => {
            row.status.add_css_class("accent");
            row.status.add_css_class("phase-tag");
            row.status.set_text("installing");
        }
        PackageStatus::Downloading => {
            row.status.add_css_class("accent");
            row.status.add_css_class("phase-tag");
            row.status.set_text("downloading");
        }
        PackageStatus::Pending => {
            row.row.add_css_class("pending");
            row.status.add_css_class("dimmed");
            row.status.set_text("queued");
        }
    }
}

/// Finished package on the Done list and in the running completed expander.
/// Source badge, name, version and source, and size only when known.
///
/// `mark_completed` adds the green "done" word. The running expander uses it.
/// The Installed review does not: the header already says the apply finished,
/// and the word sits in the middle of a wrapped subtitle. Failed and skipped
/// rows still name their status.
pub fn finished_row(package: &Package, mark_completed: bool) -> ActionRow {
    let row = action_row(package);
    if mark_completed || package.status != PackageStatus::Completed {
        add_status_caption(&row, package.status);
    }
    if let Some(size) = package.size.as_deref() {
        row.add_suffix(&caption_suffix(size, &["dimmed"]));
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
