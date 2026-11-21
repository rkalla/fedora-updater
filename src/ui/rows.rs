//! Adwaita expander row helpers backing `run_completed` (`Adw.ExpanderRow`).

use gtk::prelude::*;
use gtk::{Align, Label};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, ExpanderRow};

use fedora_updater::model::{Package, PackageStatus};

use super::queue_list::queue_row_subtitle;

pub fn source_badge(source: fedora_updater::model::UpdateSource) -> Label {
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
    let subtitle = queue_row_subtitle(package);
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
    let label = caption_suffix("", &[]);
    match status {
        PackageStatus::Downloading | PackageStatus::Installing => {
            label.add_css_class("accent");
            label.add_css_class("phase-tag");
            label.set_text(status.label());
        }
        PackageStatus::Pending => {
            label.add_css_class("dimmed");
            label.set_text("queued");
        }
        PackageStatus::Failed => {
            label.add_css_class("error");
            label.set_text("failed");
        }
        PackageStatus::Completed => {
            label.add_css_class("success");
            label.set_text("done");
        }
    }
    row.add_suffix(&label);
}

/// Finished package shown in the Running completed expander drawer.
pub fn finished_row(package: &Package, mark_completed: bool) -> ActionRow {
    let row = action_row(package);
    if mark_completed || package.status != PackageStatus::Completed {
        add_status_caption(&row, package.status);
    }
    if let Some(size) = package.size_label() {
        row.add_suffix(&caption_suffix(&size, &["dimmed", "numeric"]));
    }
    row
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
