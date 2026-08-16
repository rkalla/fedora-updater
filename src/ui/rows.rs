//! Adwaita rows, chips, and group refill helpers.

use gtk::prelude::*;
use gtk::{Align, Image, Label, ProgressBar};
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

pub struct RowParts {
    pub row: ActionRow,
    pub bar: Option<ProgressBar>,
    pub pct: Option<Label>,
}

pub fn package_row(package: &Package, show_status: bool) -> ActionRow {
    package_row_parts(package, show_status).row
}

/// Same row as [`package_row`], plus handles so progress can be updated in place.
pub fn package_row_parts(package: &Package, show_status: bool) -> RowParts {
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

    let mut bar = None;
    let mut pct = None;

    if show_status {
        match package.status {
            PackageStatus::Downloading | PackageStatus::Installing => {
                row.add_css_class("active");
                row.add_suffix(&caption_suffix(
                    package.status.label(),
                    &["accent", "phase-tag"],
                ));
                let progress = ProgressBar::builder()
                    .fraction(package.progress.clamp(0.0, 1.0))
                    .valign(Align::Center)
                    .width_request(72)
                    .build();
                progress.add_css_class("row-progress");
                let pct_label = caption_suffix(
                    &format!("{:.0}%", package.progress * 100.0),
                    &["dim-label", "numeric"],
                );
                row.add_suffix(&progress);
                row.add_suffix(&pct_label);
                bar = Some(progress);
                pct = Some(pct_label);
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

    RowParts { row, bar, pct }
}

pub fn completed_row(package: &Package) -> ActionRow {
    let row = ActionRow::builder()
        .title(&package.name)
        .subtitle(&package.version)
        .title_lines(1)
        .subtitle_lines(1)
        .activatable(false)
        .build();
    let icon = Image::from_icon_name("object-select-symbolic");
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
