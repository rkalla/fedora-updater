//! Adwaita rows, chips, and group refill helpers.

use gtk::prelude::*;
use gtk::{Align, Label};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, ExpanderRow, PreferencesGroup};

use fedora_updater::model::{AdvisoryKind, Package, PackageStatus, UpdateSource};

pub fn source_badge(source: UpdateSource) -> Label {
    Label::builder()
        .label(source.badge())
        .width_chars(3)
        .valign(Align::Center)
        .css_classes(["package-badge", source.css_class()])
        .build()
}

pub fn chip(text: &str, class: &str) -> Label {
    Label::builder()
        .label(text)
        .valign(Align::Center)
        .css_classes(["chip", class, "caption"])
        .build()
}

fn sev_dot(kind: AdvisoryKind) -> Option<gtk::Box> {
    if kind == AdvisoryKind::Unknown {
        return None;
    }
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("sev-dot");
    dot.add_css_class(kind.css_class());
    dot.set_valign(Align::Center);
    dot.set_halign(Align::Center);
    dot.set_hexpand(false);
    dot.set_vexpand(false);
    dot.set_size_request(7, 7);
    dot.set_overflow(gtk::Overflow::Hidden);
    Some(dot)
}

fn leading(package: &Package) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    box_.set_valign(Align::Center);
    box_.append(&source_badge(package.source));
    if let Some(dot) = sev_dot(package.kind) {
        box_.append(&dot);
    }
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

pub fn package_row(package: &Package, show_status: bool) -> ActionRow {
    let subtitle = format!("{} · {}", package.version_line(), package.source.label());
    let row = ActionRow::builder()
        .title(&package.name)
        .subtitle(&subtitle)
        .title_lines(1)
        .subtitle_lines(1)
        .activatable(false)
        .build();
    row.add_css_class("pkg-row");
    row.add_prefix(&leading(package));

    if show_status {
        match package.status {
            PackageStatus::Downloading | PackageStatus::Installing => {
                row.add_css_class("active");
                row.add_suffix(&caption_suffix(
                    package.status.label(),
                    &["accent", "phase-tag"],
                ));
            }
            PackageStatus::Pending => {
                row.add_css_class("pending");
                row.add_suffix(&caption_suffix("queued", &["dim-label"]));
            }
            PackageStatus::Failed => {
                row.add_suffix(&caption_suffix("failed", &["error"]));
            }
            PackageStatus::Completed => {
                row.add_suffix(&caption_suffix("done", &["success"]));
            }
            PackageStatus::Skipped => {
                row.add_css_class("pending");
                row.add_suffix(&caption_suffix("skipped", &["dim-label"]));
            }
        }
    } else {
        let trailing = package
            .size
            .clone()
            .unwrap_or_else(|| package.source.badge().into());
        row.add_suffix(&caption_suffix(&trailing, &["dim-label"]));
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
        .subtitle_lines(1)
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
    row.status.remove_css_class("dim-label");
    row.status.remove_css_class("accent");
    row.status.remove_css_class("success");
    row.status.remove_css_class("error");
    row.status.remove_css_class("phase-tag");

    let live = is_current && !package.status.is_done();
    if live {
        row.row.add_css_class("active");
        row.status.add_css_class("accent");
        row.status.add_css_class("phase-tag");
        let label = match package.status {
            PackageStatus::Downloading => "downloading",
            _ => "installing",
        };
        row.status.set_text(label);
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
            row.status.add_css_class("dim-label");
            row.status.set_text("skipped");
        }
        _ => {
            row.row.add_css_class("pending");
            row.status.add_css_class("dim-label");
            row.status.set_text("queued");
        }
    }
}

pub fn completed_row(package: &Package) -> ActionRow {
    let row = ActionRow::builder()
        .title(&package.name)
        .subtitle(&package.version_line())
        .title_lines(1)
        .subtitle_lines(1)
        .activatable(false)
        .build();
    let icon = gtk::Image::from_icon_name("object-select-symbolic");
    icon.add_css_class("success");
    row.add_prefix(&icon);
    row.add_prefix(&source_badge(package.source));
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

pub fn refill_chips(chips: &libadwaita::WrapBox, items: &[(&str, &str)]) {
    while let Some(child) = chips.first_child() {
        chips.remove(&child);
    }
    for (text, class) in items {
        chips.append(&chip(text, class));
    }
}
