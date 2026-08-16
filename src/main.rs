//! Multi-call binary:
//!   fedora-updater           → GNOME GUI (unprivileged)
//!   fedora-updater --helper  → allowlisted privileged helper (via pkexec)

mod ui;

use gtk::prelude::*;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--helper") | Some("helper") => {
            // Privileged protocol mode — no GTK.
            fedora_updater::helper_mode::run();
        }
        Some("-h") | Some("--help") => {
            print_help();
        }
        Some(other) => {
            eprintln!("Unknown argument: {other}");
            print_help();
            std::process::exit(2);
        }
        None => run_gui(),
    }
}

fn print_help() {
    eprintln!(
        "\
Fedora Updater — desktop updates via dnf / flatpak / fwupd

Usage:
  fedora-updater           Launch the GUI
  fedora-updater --helper  Privileged helper mode (used via pkexec; not for manual use)
  fedora-updater --help    Show this help
"
    );
}

fn run_gui() {
    let app = libadwaita::Application::builder()
        .application_id("dev.fedora.Updater")
        .build();

    app.connect_activate(|app| {
        ui::window::build(app);
    });

    // Ensure unknown CLI args are not passed through to GTK/GApplication
    // (we already consumed our own flags above; rebuild argv as program only).
    let exe = std::env::args()
        .next()
        .unwrap_or_else(|| "fedora-updater".into());
    let app_args = [exe];
    app.run_with_args(&app_args);
}
