//! Secondary windows and dialogs (About, Full Log, Release Upgrade, and alerts).

use std::time::Duration;

use gtk::glib;
use gtk::glib::object::IsA;
use gtk::prelude::*;
use gtk::{Builder, Button, Label, TextView};
use libadwaita::prelude::*;
use libadwaita::Window as AdwWindow;

use fedora_updater::presenter::{
    stale_update_dialog, stop_update_dialog, DialogCopy, ABOUT_COMMENTS, ABOUT_NAME,
};

use super::obj;

const LOG_WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/log_window.ui"));

pub(super) fn present_about(parent: &impl IsA<gtk::Widget>) {
    let dialog = libadwaita::AboutDialog::new();
    dialog.set_application_name(ABOUT_NAME);
    dialog.set_version(env!("CARGO_PKG_VERSION"));
    dialog.set_comments(ABOUT_COMMENTS);
    dialog.present(Some(parent));
}

pub(super) fn show_log_window(parent: &impl IsA<gtk::Widget>, log: &str) {
    let builder = Builder::from_string(LOG_WINDOW_UI);
    let win: AdwWindow = obj(&builder, "log_window");
    let view: TextView = obj(&builder, "log_view");
    let copy_btn: Button = obj(&builder, "copy_btn");

    if let Some(parent_win) = parent.root().and_downcast::<gtk::Window>() {
        win.set_transient_for(Some(&parent_win));
    }

    view.buffer().set_text(log);
    view.set_can_focus(true);

    let log_owned = log.to_string();
    copy_btn.connect_clicked(move |_| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&log_owned);
        }
    });
    {
        let esc_win = win.clone();
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                esc_win.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win.add_controller(key);
    }

    win.present();
}

pub(super) fn present_upgrade_dialog(parent: &impl IsA<gtk::Widget>, version: u32) {
    use fedora_updater::release_upgrade::{
        upgrade_command_lines, upgrade_dialog_body, UPGRADE_DIALOG_HEADING,
    };

    let body = Label::new(Some(upgrade_dialog_body()));
    body.set_wrap(true);
    body.set_xalign(0.0);
    body.set_halign(gtk::Align::Start);

    let commands = gtk::Box::new(gtk::Orientation::Vertical, 10);
    commands.set_margin_top(18);
    let copy_names = ["Copy download command", "Copy reboot command"];
    for (command, copy_name) in upgrade_command_lines(version).into_iter().zip(copy_names) {
        commands.append(&command_row(&command, copy_name));
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
    dialog.present(Some(parent));
}

/// One command, padded, with its own copy button.
/// The clipboard text has no trailing newline, so pasting does not run it.
fn command_row(command: &str, copy_name: &str) -> gtk::Box {
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
        btn.set_icon_name("emblem-ok-symbolic");
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

fn build_dialog(copy: &DialogCopy) -> libadwaita::AlertDialog {
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

fn present_confirm(
    parent: &impl IsA<gtk::Widget>,
    copy: DialogCopy,
    on_confirm: impl Fn() + 'static,
) {
    let dialog = build_dialog(&copy);
    let confirm_id = copy.confirm_id;
    dialog.connect_response(None, move |_dialog, response| {
        if response == confirm_id {
            on_confirm();
        }
    });
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

#[cfg(test)]
mod tests {
    #[test]
    fn log_template_has_text_view() {
        let xml = super::LOG_WINDOW_UI;
        assert!(xml.contains("id=\"log_view\""));
        assert!(xml.contains("id=\"copy_btn\""));
        assert!(xml.contains("Copy All"));
        assert!(xml.contains("Full Log"));
        assert!(
            !xml.contains("id=\"close_btn\""),
            "header close and Esc close the log window"
        );
        assert!(!xml.contains("Copy all"));
        assert!(!xml.contains("Full log"));
    }
}
