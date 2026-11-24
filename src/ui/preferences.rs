//! Native Libadwaita Preferences dialog (`Ctrl+,`) for enabling/disabling update sources.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gtk::prelude::*;
use libadwaita::prelude::*;

use fedora_updater::model::{SourceCategory, UpdateSource};
use fedora_updater::orchestrator::{SourceAvailability, SourceConfig};

use super::prefs;

type ToggleSourceCb = Rc<dyn Fn(&str, bool)>;

pub struct PreferencesHandle {
    pub dialog: libadwaita::PreferencesDialog,
    #[cfg(test)]
    pub switches: BTreeMap<String, libadwaita::SwitchRow>,
    #[cfg(test)]
    pub all_disabled_group: libadwaita::PreferencesGroup,
}

pub fn build_preferences_dialog(
    current_prefs: &prefs::UiPrefs,
    availability: &BTreeMap<String, SourceAvailability>,
    on_toggle: impl Fn(&str, bool) + 'static,
) -> PreferencesHandle {
    let dialog = libadwaita::PreferencesDialog::builder()
        .title("Preferences")
        .build();

    let page = libadwaita::PreferencesPage::builder()
        .title("Update Sources")
        .icon_name("system-software-install-symbolic")
        .build();

    let all_disabled_group = libadwaita::PreferencesGroup::new();
    let warning_row = libadwaita::ActionRow::builder()
        .title("All update sources are disabled")
        .subtitle("Enable at least one update source to check for updates.")
        .activatable(false)
        .build();
    warning_row.add_css_class("inspector-warning-card");
    let warn_icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    warn_icon.set_valign(gtk::Align::Center);
    warning_row.add_prefix(&warn_icon);
    all_disabled_group.add(&warning_row);
    let ostree_booted = availability.contains_key("rpm-ostree") && !availability.contains_key("dnf");
    all_disabled_group.set_visible(
        current_prefs
            .source_config()
            .all_disabled_for_host(ostree_booted),
    );
    page.add(&all_disabled_group);

    let config_state: Rc<RefCell<SourceConfig>> =
        Rc::new(RefCell::new(current_prefs.source_config()));
    let on_toggle: ToggleSourceCb = Rc::new(on_toggle);
    let pref_sources = UpdateSource::pref_sources_for_host(ostree_booted);
    #[cfg(test)]
    let mut switches = BTreeMap::new();

    for &category in SourceCategory::ALL {
        let cat_sources: Vec<UpdateSource> = pref_sources
            .iter()
            .copied()
            .filter(|src| src.category() == category)
            .collect();
        if cat_sources.is_empty() {
            continue;
        }

        let escaped_title = gtk::glib::markup_escape_text(category.label());
        let group = libadwaita::PreferencesGroup::builder()
            .title(escaped_title.as_str())
            .build();

        for src in cat_sources {
            let key = src.pref_key();
            let subtitle = availability
                .get(key)
                .map(|info| info.subtitle.as_str())
                .unwrap_or_else(|| {
                    if src.is_opt_in() {
                        "Updates installed containers when enabled"
                    } else {
                        "Installed"
                    }
                });
            let escaped_pref_title = gtk::glib::markup_escape_text(src.pref_title());
            let escaped_subtitle = gtk::glib::markup_escape_text(subtitle);
            let row = libadwaita::SwitchRow::builder()
                .name(format!("pref-source:{key}"))
                .title(escaped_pref_title.as_str())
                .subtitle(escaped_subtitle.as_str())
                .active(current_prefs.is_source_enabled(key))
                .build();

            let config_state = config_state.clone();
            let all_disabled_group = all_disabled_group.clone();
            let on_toggle = on_toggle.clone();
            row.connect_active_notify(move |switch_row| {
                let enabled = switch_row.is_active();
                {
                    let mut guard = config_state.borrow_mut();
                    if enabled {
                        guard.enable(key);
                    } else {
                        guard.disable(key);
                    }
                    all_disabled_group.set_visible(guard.all_disabled_for_host(ostree_booted));
                }
                on_toggle(key, enabled);
            });

            #[cfg(test)]
            switches.insert(key.to_string(), row.clone());
            group.add(&row);
        }

        page.add(&group);
    }

    dialog.add(&page);

    PreferencesHandle {
        dialog,
        #[cfg(test)]
        switches,
        #[cfg(test)]
        all_disabled_group,
    }
}

pub fn present_preferences_dialog(
    parent: &impl IsA<gtk::Widget>,
    current_prefs: &prefs::UiPrefs,
    availability: &BTreeMap<String, SourceAvailability>,
    on_toggle: impl Fn(&str, bool) + 'static,
) -> PreferencesHandle {
    let handle = build_preferences_dialog(current_prefs, availability, on_toggle);
    handle.dialog.present(Some(parent));
    handle
}
