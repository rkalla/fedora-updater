//! Native Libadwaita Preferences dialog (`Ctrl+,`) for enabling/disabling update sources.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
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
    all_disabled_group.set_visible(current_prefs.source_config().all_disabled());
    page.add(&all_disabled_group);

    let disabled_state: Rc<RefCell<BTreeSet<String>>> =
        Rc::new(RefCell::new(current_prefs.disabled_sources.clone()));
    let on_toggle: ToggleSourceCb = Rc::new(on_toggle);
    let pref_sources = UpdateSource::pref_sources();
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

        let group = libadwaita::PreferencesGroup::builder()
            .title(category.label())
            .build();

        for src in cat_sources {
            let key = src.pref_key();
            let subtitle = availability
                .get(key)
                .map(|info| info.subtitle.as_str())
                .unwrap_or("Installed");
            let row = libadwaita::SwitchRow::builder()
                .name(format!("pref-source:{key}"))
                .title(src.pref_title())
                .subtitle(subtitle)
                .active(current_prefs.is_source_enabled(key))
                .build();

            let disabled_state = disabled_state.clone();
            let all_disabled_group = all_disabled_group.clone();
            let on_toggle = on_toggle.clone();
            row.connect_active_notify(move |switch_row| {
                let enabled = switch_row.is_active();
                {
                    let mut guard = disabled_state.borrow_mut();
                    if enabled {
                        guard.remove(key);
                    } else {
                        guard.insert(key.to_string());
                    }
                    let cfg = SourceConfig::from_disabled_keys(guard.iter().cloned());
                    all_disabled_group.set_visible(cfg.all_disabled());
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
