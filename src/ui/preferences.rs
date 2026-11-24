//! Native Libadwaita Preferences dialog (`Ctrl+,`) for enabling/disabling update sources.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gtk::prelude::*;
use libadwaita::prelude::*;

use fedora_updater::backend::hooks::{CustomHook, HOOK_TEMPLATES};
use fedora_updater::model::{SourceCategory, UpdateSource};
use fedora_updater::orchestrator::{SourceAvailability, SourceConfig};

use super::prefs;

type ToggleSourceCb = Rc<dyn Fn(&str, bool)>;
type HooksChangedCb = Rc<dyn Fn(&[CustomHook])>;

pub struct PreferencesHandle {
    pub dialog: libadwaita::PreferencesDialog,
    #[cfg(test)]
    pub switches: BTreeMap<String, libadwaita::SwitchRow>,
    #[cfg(test)]
    pub all_disabled_group: libadwaita::PreferencesGroup,
    #[cfg(test)]
    pub custom_hooks_group: libadwaita::PreferencesGroup,
    #[cfg(test)]
    pub add_hook_button: gtk::Button,
    #[cfg(test)]
    pub hook_switches: Rc<RefCell<Vec<libadwaita::SwitchRow>>>,
    #[cfg(test)]
    pub hook_delete_buttons: Rc<RefCell<Vec<gtk::Button>>>,
}

pub fn build_preferences_dialog(
    current_prefs: &prefs::UiPrefs,
    availability: &BTreeMap<String, SourceAvailability>,
    on_toggle: impl Fn(&str, bool) + 'static,
    on_hooks_changed: impl Fn(&[CustomHook]) + 'static,
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

    let on_hooks_changed: HooksChangedCb = Rc::new(on_hooks_changed);
    let hooks_state: Rc<RefCell<Vec<CustomHook>>> =
        Rc::new(RefCell::new(current_prefs.custom_hooks.clone()));
    let rendered_hook_rows: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));
    let hook_switches: Rc<RefCell<Vec<libadwaita::SwitchRow>>> =
        Rc::new(RefCell::new(Vec::new()));
    let hook_delete_buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));

    let add_hook_content = libadwaita::ButtonContent::builder()
        .icon_name("list-add-symbolic")
        .label("Add Hook")
        .build();
    let add_hook_button = gtk::Button::builder()
        .name("add-hook-button")
        .child(&add_hook_content)
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .build();
    add_hook_button.set_tooltip_text(Some("Add an unprivileged custom update hook"));
    add_hook_button.update_property(&[gtk::accessible::Property::Label("Add Hook")]);

    let custom_hooks_group = libadwaita::PreferencesGroup::builder()
        .title("Custom Hooks")
        .description("Custom hooks run strictly as your user account and never request administrator privileges.")
        .header_suffix(&add_hook_button)
        .build();

    let hook_ctx = HookUiContext {
        dialog: dialog.clone(),
        group: custom_hooks_group.clone(),
        rendered_rows: rendered_hook_rows,
        hook_switches: hook_switches.clone(),
        hook_delete_buttons: hook_delete_buttons.clone(),
        hooks_state: hooks_state.clone(),
        config_state,
        all_disabled_group: all_disabled_group.clone(),
        on_hooks_changed,
        ostree_booted,
    };

    {
        let hook_ctx = hook_ctx.clone();
        add_hook_button.connect_clicked(move |_| {
            present_hook_editor(&hook_ctx, None);
        });
    }

    rebuild_hook_rows(&hook_ctx);
    page.add(&custom_hooks_group);

    dialog.add(&page);

    PreferencesHandle {
        dialog,
        #[cfg(test)]
        switches,
        #[cfg(test)]
        all_disabled_group,
        #[cfg(test)]
        custom_hooks_group,
        #[cfg(test)]
        add_hook_button,
        #[cfg(test)]
        hook_switches,
        #[cfg(test)]
        hook_delete_buttons,
    }
}

pub fn present_preferences_dialog(
    parent: &impl IsA<gtk::Widget>,
    current_prefs: &prefs::UiPrefs,
    availability: &BTreeMap<String, SourceAvailability>,
    on_toggle: impl Fn(&str, bool) + 'static,
    on_hooks_changed: impl Fn(&[CustomHook]) + 'static,
) -> PreferencesHandle {
    let handle = build_preferences_dialog(current_prefs, availability, on_toggle, on_hooks_changed);
    handle.dialog.present(Some(parent));
    handle
}

#[derive(Clone)]
struct HookUiContext {
    dialog: libadwaita::PreferencesDialog,
    group: libadwaita::PreferencesGroup,
    rendered_rows: Rc<RefCell<Vec<gtk::Widget>>>,
    hook_switches: Rc<RefCell<Vec<libadwaita::SwitchRow>>>,
    hook_delete_buttons: Rc<RefCell<Vec<gtk::Button>>>,
    hooks_state: Rc<RefCell<Vec<CustomHook>>>,
    config_state: Rc<RefCell<SourceConfig>>,
    all_disabled_group: libadwaita::PreferencesGroup,
    on_hooks_changed: HooksChangedCb,
    ostree_booted: bool,
}

fn sync_hooks_change(ctx: &HookUiContext) {
    let snapshot = ctx.hooks_state.borrow().clone();
    {
        let mut cfg = ctx.config_state.borrow_mut();
        cfg.custom_hooks = snapshot.clone();
        ctx.all_disabled_group
            .set_visible(cfg.all_disabled_for_host(ctx.ostree_booted));
    }
    (ctx.on_hooks_changed)(&snapshot);
}

fn rebuild_hook_rows(ctx: &HookUiContext) {
    for old_row in ctx.rendered_rows.borrow_mut().drain(..) {
        ctx.group.remove(&old_row);
    }
    ctx.hook_switches.borrow_mut().clear();
    ctx.hook_delete_buttons.borrow_mut().clear();

    let hooks = ctx.hooks_state.borrow().clone();
    if hooks.is_empty() {
        let empty_row = libadwaita::ActionRow::builder()
            .name("custom-hook-empty-row")
            .title("No custom hooks configured")
            .subtitle("Add a user-space command such as cargo install-update or chezmoi update.")
            .activatable(false)
            .build();
        empty_row.add_css_class("dimmed");
        ctx.group.add(&empty_row);
        ctx.rendered_rows.borrow_mut().push(empty_row.upcast());
        return;
    }

    for (idx, hook) in hooks.into_iter().enumerate() {
        let subtitle_plain = match hook.check_command.as_deref() {
            Some(chk) => format!(
                "Check: {chk}  ·  Run: {} (current user only)",
                hook.apply_command
            ),
            None => format!(
                "Run: {}  ·  No check command (current user only)",
                hook.apply_command
            ),
        };
        let escaped_title = gtk::glib::markup_escape_text(&hook.name);
        let escaped_subtitle = gtk::glib::markup_escape_text(&subtitle_plain);

        let row = libadwaita::SwitchRow::builder()
            .name(format!("custom-hook:{idx}"))
            .title(escaped_title.as_str())
            .subtitle(escaped_subtitle.as_str())
            .active(hook.enabled)
            .build();

        let edit_btn = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("Edit custom hook")
            .build();
        edit_btn.update_property(&[gtk::accessible::Property::Label("Edit custom hook")]);

        let delete_btn = gtk::Button::builder()
            .name(format!("delete-hook:{idx}"))
            .icon_name("user-trash-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("Remove custom hook")
            .build();
        delete_btn.update_property(&[gtk::accessible::Property::Label("Remove custom hook")]);

        row.add_suffix(&edit_btn);
        row.add_suffix(&delete_btn);

        {
            let ctx_toggle = ctx.clone();
            row.connect_active_notify(move |switch_row| {
                let enabled = switch_row.is_active();
                if let Some(entry) = ctx_toggle.hooks_state.borrow_mut().get_mut(idx) {
                    entry.enabled = enabled;
                }
                sync_hooks_change(&ctx_toggle);
            });
        }

        {
            let ctx_edit = ctx.clone();
            edit_btn.connect_clicked(move |_| {
                present_hook_editor(&ctx_edit, Some(idx));
            });
        }

        {
            let ctx_del = ctx.clone();
            delete_btn.connect_clicked(move |_| {
                {
                    let mut list = ctx_del.hooks_state.borrow_mut();
                    if idx < list.len() {
                        list.remove(idx);
                    }
                }
                sync_hooks_change(&ctx_del);
                rebuild_hook_rows(&ctx_del);
            });
        }

        ctx.group.add(&row);
        ctx.hook_switches.borrow_mut().push(row.clone());
        ctx.hook_delete_buttons.borrow_mut().push(delete_btn);
        ctx.rendered_rows.borrow_mut().push(row.upcast());
    }
}

pub(crate) struct HookEditorHandles {
    pub dialog: libadwaita::Dialog,
    #[cfg(test)]
    pub template_row: Option<libadwaita::ComboRow>,
    #[cfg(test)]
    pub name_entry: libadwaita::EntryRow,
    #[cfg(test)]
    pub check_entry: libadwaita::EntryRow,
    #[cfg(test)]
    pub apply_entry: libadwaita::EntryRow,
    #[cfg(test)]
    pub save_btn: gtk::Button,
}

pub(crate) fn build_hook_editor_dialog(
    existing: Option<&CustomHook>,
    on_save: impl Fn(CustomHook) + 'static,
) -> HookEditorHandles {
    let title = if existing.is_some() {
        "Edit Custom Hook"
    } else {
        "New Custom Hook"
    };
    let initial_enabled = existing.is_none_or(|h| h.enabled);

    let name_entry = libadwaita::EntryRow::builder()
        .title("Hook Name")
        .text(existing.map(|h| h.name.as_str()).unwrap_or(""))
        .build();
    let check_entry = libadwaita::EntryRow::builder()
        .title("Check Command (optional, exit 0 + output = updates available)")
        .text(
            existing
                .and_then(|h| h.check_command.as_deref())
                .unwrap_or(""),
        )
        .build();
    let apply_entry = libadwaita::EntryRow::builder()
        .title("Update Command (runs as your user account)")
        .text(existing.map(|h| h.apply_command.as_str()).unwrap_or(""))
        .build();

    let group = libadwaita::PreferencesGroup::builder()
        .description("Commands run unprivileged via `sh -c` under your user account and never request root or pkexec.")
        .build();

    #[cfg(test)]
    let mut template_row = None;
    if existing.is_none() {
        let mut labels = Vec::with_capacity(1 + HOOK_TEMPLATES.len());
        labels.push("Custom (blank)");
        for tpl in HOOK_TEMPLATES {
            labels.push(tpl.name);
        }
        let model = gtk::StringList::new(&labels);
        let combo = libadwaita::ComboRow::builder()
            .name("hook-template-combo")
            .title("Template")
            .subtitle("Start from a preset or enter your own command")
            .model(&model)
            .selected(0)
            .build();
        let name_fill = name_entry.clone();
        let check_fill = check_entry.clone();
        let apply_fill = apply_entry.clone();
        combo.connect_selected_notify(move |row| {
            let selected = row.selected();
            if selected == 0 {
                name_fill.set_text("");
                check_fill.set_text("");
                apply_fill.set_text("");
            } else if let Some(tpl) = HOOK_TEMPLATES.get((selected - 1) as usize) {
                name_fill.set_text(tpl.name);
                check_fill.set_text(tpl.check_command.unwrap_or(""));
                apply_fill.set_text(tpl.apply_command);
            }
        });
        group.add(&combo);
        #[cfg(test)]
        {
            template_row = Some(combo);
        }
    }

    group.add(&name_entry);
    group.add(&check_entry);
    group.add(&apply_entry);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.set_margin_top(12);
    content.set_margin_bottom(18);
    content.append(&group);

    let header = libadwaita::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel_btn = gtk::Button::with_label("Cancel");
    let save_btn = gtk::Button::with_label("Save");
    save_btn.add_css_class("suggested-action");
    let valid_now = !name_entry.text().trim().is_empty() && !apply_entry.text().trim().is_empty();
    save_btn.set_sensitive(valid_now);
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);

    let toolbar = libadwaita::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));

    let editor = libadwaita::Dialog::builder()
        .title(title)
        .content_width(480)
        .follows_content_size(true)
        .child(&toolbar)
        .build();

    {
        let name_val = name_entry.clone();
        let apply_val = apply_entry.clone();
        let save_btn = save_btn.clone();
        let update_valid = move || {
            let ok = !name_val.text().trim().is_empty() && !apply_val.text().trim().is_empty();
            save_btn.set_sensitive(ok);
        };
        let update_a = update_valid.clone();
        name_entry.connect_changed(move |_| update_a());
        apply_entry.connect_changed(move |_| update_valid());
    }

    {
        let editor = editor.clone();
        cancel_btn.connect_clicked(move |_| {
            editor.close();
        });
    }

    {
        let editor = editor.clone();
        let name_entry = name_entry.clone();
        let check_entry = check_entry.clone();
        let apply_entry = apply_entry.clone();
        save_btn.connect_clicked(move |_| {
            let check_raw = check_entry.text().to_string();
            let hook = CustomHook {
                name: name_entry.text().to_string(),
                check_command: if check_raw.trim().is_empty() {
                    None
                } else {
                    Some(check_raw)
                },
                apply_command: apply_entry.text().to_string(),
                enabled: initial_enabled,
            }
            .normalized();
            if hook.is_valid() {
                on_save(hook);
                editor.close();
            }
        });
    }

    HookEditorHandles {
        dialog: editor,
        #[cfg(test)]
        template_row,
        #[cfg(test)]
        name_entry,
        #[cfg(test)]
        check_entry,
        #[cfg(test)]
        apply_entry,
        #[cfg(test)]
        save_btn,
    }
}

fn present_hook_editor(ctx: &HookUiContext, edit_index: Option<usize>) {
    let existing = edit_index.and_then(|idx| ctx.hooks_state.borrow().get(idx).cloned());
    let ctx_save = ctx.clone();
    let editor = build_hook_editor_dialog(existing.as_ref(), move |hook| {
        {
            let mut list = ctx_save.hooks_state.borrow_mut();
            if let Some(idx) = edit_index.filter(|&i| i < list.len()) {
                list[idx] = hook;
            } else {
                list.push(hook);
            }
        }
        sync_hooks_change(&ctx_save);
        rebuild_hook_rows(&ctx_save);
    });
    editor.dialog.present(Some(&ctx.dialog));
}
