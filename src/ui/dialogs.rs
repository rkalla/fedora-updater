//! Secondary dialogs (About, Release Upgrade, and alerts).

use std::time::Duration;

use gtk::glib;
use gtk::glib::object::IsA;
use gtk::prelude::*;
use gtk::Label;
use libadwaita::prelude::*;

use fedora_updater::backend::cleanup::CleanupPlan;
use fedora_updater::model::format_bytes;
use fedora_updater::presenter::{
    stale_update_dialog, stop_update_dialog, DialogCopy, ABOUT_COMMENTS, ABOUT_NAME,
};

pub(super) fn present_about(parent: &impl IsA<gtk::Widget>) {
    let dialog = libadwaita::AboutDialog::new();
    dialog.set_application_name(ABOUT_NAME);
    dialog.set_version(env!("CARGO_PKG_VERSION"));
    dialog.set_comments(ABOUT_COMMENTS);
    dialog.present(Some(parent));
}

pub(super) fn build_upgrade_dialog(version: u32) -> (libadwaita::Dialog, Vec<gtk::Box>) {
    use fedora_updater::release_upgrade::{
        upgrade_command_lines, upgrade_dialog_body, UPGRADE_DIALOG_HEADING,
    };

    let body = Label::new(Some(upgrade_dialog_body()));
    body.set_wrap(true);
    body.set_xalign(0.0);
    body.set_halign(gtk::Align::Start);

    let commands = gtk::Box::new(gtk::Orientation::Vertical, 10);
    commands.set_margin_top(18);
    let mut rows = Vec::new();
    let copy_names = ["Copy download command", "Copy reboot command"];
    for (command, copy_name) in upgrade_command_lines(version).into_iter().zip(copy_names) {
        let row = command_row(&command, copy_name);
        commands.append(&row);
        rows.push(row);
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_margin_top(6);
    content.set_margin_bottom(24);
    content.append(&body);
    content.append(&commands);

    let toolbar = libadwaita::ToolbarView::new();
    toolbar.add_top_bar(&libadwaita::HeaderBar::new());
    toolbar.set_content(Some(&content));

    let dialog = libadwaita::Dialog::new();
    dialog.set_title(UPGRADE_DIALOG_HEADING);
    dialog.set_content_width(560);
    dialog.set_follows_content_size(true);
    dialog.set_child(Some(&toolbar));
    (dialog, rows)
}

pub(super) fn present_upgrade_dialog(parent: &impl IsA<gtk::Widget>, version: u32) {
    let (dialog, _) = build_upgrade_dialog(version);
    dialog.present(Some(parent));
}

/// One command, padded, with its own copy button.
/// The clipboard text has no trailing newline, so pasting does not run it.
pub(super) fn command_row(command: &str, copy_name: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("upgrade-command");

    let label = Label::new(Some(command));
    label.set_selectable(true);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_margin_top(14);
    label.set_margin_bottom(14);
    label.set_margin_start(14);
    label.set_can_focus(false);
    label.select_region(0, 0);
    label.add_css_class("monospace");

    let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
    copy.set_tooltip_text(Some(copy_name));
    copy.add_css_class("flat");
    copy.set_valign(gtk::Align::Center);
    copy.set_margin_top(4);
    copy.set_margin_bottom(4);
    copy.set_margin_end(6);
    copy.update_property(&[gtk::accessible::Property::Label(copy_name)]);

    let text = command.to_string();
    let idle_tip = copy_name.to_string();
    copy.connect_clicked(move |btn| {
        btn.clipboard().set_text(&text);
        btn.set_icon_name("object-select-symbolic");
        btn.set_tooltip_text(Some("Copied"));
        let btn = btn.clone();
        let idle_tip = idle_tip.clone();
        glib::timeout_add_local_once(Duration::from_millis(1600), move || {
            btn.set_icon_name("edit-copy-symbolic");
            btn.set_tooltip_text(Some(&idle_tip));
        });
    });

    row.append(&label);
    row.append(&copy);
    row
}

pub(super) fn build_dialog(copy: &DialogCopy) -> libadwaita::AlertDialog {
    let dialog = libadwaita::AlertDialog::new(Some(copy.heading), Some(copy.body));
    dialog.add_response(copy.close_id, copy.close_label);
    dialog.add_response(copy.confirm_id, copy.confirm_label);
    if copy.confirm_destructive {
        dialog
            .set_response_appearance(copy.confirm_id, libadwaita::ResponseAppearance::Destructive);
    }
    if copy.confirm_suggested {
        dialog.set_response_appearance(copy.confirm_id, libadwaita::ResponseAppearance::Suggested);
    }
    dialog.set_default_response(Some(copy.default_id));
    dialog.set_close_response(copy.close_id);
    dialog
}

pub(super) fn build_confirm_dialog(
    copy: DialogCopy,
    on_confirm: impl Fn() + 'static,
) -> libadwaita::AlertDialog {
    let dialog = build_dialog(&copy);
    let confirm_id = copy.confirm_id;
    dialog.connect_response(None, move |_dialog, response| {
        if response == confirm_id {
            on_confirm();
        }
    });
    dialog
}

fn present_confirm(
    parent: &impl IsA<gtk::Widget>,
    copy: DialogCopy,
    on_confirm: impl Fn() + 'static,
) {
    let dialog = build_confirm_dialog(copy, on_confirm);
    dialog.present(Some(parent));
}

/// Ask before applying a list that has been sitting for more than 24 hours.
/// Yes refreshes, then applies whatever that check finds. No leaves the list
/// untouched — Update All asks again until the user accepts a refresh.
pub(super) fn prompt_stale_update_list(
    parent: &impl IsA<gtk::Widget>,
    on_confirm: impl Fn() + 'static,
) {
    present_confirm(parent, stale_update_dialog(), on_confirm);
}

pub(super) fn prompt_stop_during_transaction(
    parent: &impl IsA<gtk::Widget>,
    on_confirm: impl Fn() + 'static,
) {
    present_confirm(parent, stop_update_dialog(), on_confirm);
}

pub(crate) fn build_cleanup_confirm_dialog(
    plan: &CleanupPlan,
    on_confirm: impl Fn() + 'static,
) -> libadwaita::AlertDialog {
    let count = plan.len();
    let noun = if count == 1 { "item" } else { "items" };
    let heading = format!("Clean up {count} unused {noun}?");
    let body = match plan.formatted_size() {
        Some(size) => format!(
            "Removing these unused items can reclaim about {size}. Review the list below; nothing is deleted until you confirm."
        ),
        None => "Review the unused items below; nothing is deleted until you confirm.".to_string(),
    };

    let dialog = libadwaita::AlertDialog::new(Some(&heading), Some(&body));

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for item in &plan.items {
        let size_label = item
            .size_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "Size unknown".to_string());
        let escaped_title = glib::markup_escape_text(&item.name);
        let escaped_sub = glib::markup_escape_text(&format!(
            "{} · {} · {}",
            item.source.label(),
            item.id,
            size_label
        ));
        let row = libadwaita::ActionRow::builder()
            .title(escaped_title.as_str())
            .subtitle(escaped_sub.as_str())
            .activatable(false)
            .build();
        let badge = gtk::Label::builder()
            .label(item.source.short_badge())
            .valign(gtk::Align::Center)
            .css_classes(["caption", "dimmed"])
            .build();
        row.add_suffix(&badge);
        list.append(&row);
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(260)
        .child(&list)
        .build();
    dialog.set_extra_child(Some(&scroller));

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove Unused");
    dialog.set_response_appearance("remove", libadwaita::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |_dlg, response| {
        if response == "remove" {
            on_confirm();
        }
    });
    dialog
}

pub(crate) fn present_cleanup_confirm_dialog(
    parent: &impl IsA<gtk::Widget>,
    plan: &CleanupPlan,
    on_confirm: impl Fn() + 'static,
) -> libadwaita::AlertDialog {
    let dialog = build_cleanup_confirm_dialog(plan, on_confirm);
    dialog.present(Some(parent));
    dialog
}
