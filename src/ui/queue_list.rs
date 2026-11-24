//! Virtualized `Gtk.ListView` package queue backed by `gio::ListStore` + `Gtk.FilterListModel`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;

use fedora_updater::backend::dnf::package_implies_reboot;
use fedora_updater::model::{Package, PackageStatus, UpdateSource};

use super::sidebar::SourceFilter;

/// Display density mode for virtualized queue rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueRowMode {
    Review,
    Running,
    Finished,
}

/// True when updating `package` triggers a restart recommendation badge/callout.
pub fn package_needs_reboot(package: &Package) -> bool {
    package.source == UpdateSource::RpmOstree
        || (package.source == UpdateSource::Dnf && package_implies_reboot(&package.name))
}

/// Compact display name for a Flatpak related extension (e.g. `GL32.nvidia-570-133-07`).
pub fn short_extension_name(ext: &str) -> &str {
    let trimmed = ext.trim();
    if let Some(rest) = trimmed.strip_prefix("org.freedesktop.Platform.") {
        return rest;
    }
    if let Some(rest) = trimmed.strip_prefix("org.gnome.Platform.") {
        return rest;
    }
    if let Some(rest) = trimmed.strip_prefix("org.kde.Platform.") {
        return rest;
    }
    trimmed
        .rsplit('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
}

/// Micro-tag text for Flatpak related extensions attached to a parent app/runtime.
pub fn extension_micro_tag(extensions: &[String]) -> Option<String> {
    let first = extensions.first()?;
    let short = short_extension_name(first);
    if short.is_empty() {
        return None;
    }
    if extensions.len() == 1 {
        Some(format!("+ {short}"))
    } else {
        Some(format!("+ {short} (+{})", extensions.len() - 1))
    }
}

/// Subtitle under a package row: `old → new` when `old_version` is present, else
/// `detail` (without trailing `installs ...`) or `version · repo` — never appending
/// a redundant `· System` / `· Apps` / `· Firmware` suffix.
pub fn queue_row_subtitle(package: &Package) -> String {
    if let Some(old) = package.old_version.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return format!("{old} → {}", package.version);
    }
    let clean = package.clean_detail_without_installs();
    if !clean.is_empty() {
        return clean;
    }
    if !package.version.is_empty() && !package.repo.is_empty() {
        return format!("{} · {}", package.version, package.repo);
    }
    package.version.clone()
}

/// Right-aligned live status label shown while a transaction is running.
pub fn running_status_label(
    status: PackageStatus,
    is_current: bool,
    item_progress: Option<f64>,
) -> String {
    match status {
        PackageStatus::Pending => "Queued".to_string(),
        PackageStatus::Downloading => match item_progress.filter(|_| is_current) {
            Some(frac) => format!("Downloading · {:.0}%", frac.clamp(0.0, 1.0) * 100.0),
            None => "Downloading".to_string(),
        },
        PackageStatus::Installing => match item_progress.filter(|_| is_current) {
            Some(frac) => format!("Installing · {:.0}%", frac.clamp(0.0, 1.0) * 100.0),
            None => "Installing".to_string(),
        },
        PackageStatus::Completed => "Done".to_string(),
        PackageStatus::Failed => "Failed".to_string(),
    }
}

/// True when `source` matches the active sidebar `SourceFilter`.
pub fn matches_source_filter(source: UpdateSource, filter: SourceFilter) -> bool {
    match filter {
        SourceFilter::All | SourceFilter::Security => true,
        SourceFilter::Category(cat) => source.category() == cat,
    }
}

/// Multi-token case-insensitive query across package name, repo, versions, arch,
/// detail, optional summary, and advisories/CVEs.
pub fn matches_package_query(package: &Package, query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    let hay = [
        package.name.as_str(),
        package.repo.as_str(),
        package.arch.as_str(),
        package.version.as_str(),
        package.old_version.as_deref().unwrap_or(""),
        package.detail.as_str(),
        package.summary.as_deref().unwrap_or(""),
        package.url.as_deref().unwrap_or(""),
        package.id.strip_prefix("gnome-ext:").unwrap_or(""),
        package.source.badge(),
    ];
    trimmed.split_whitespace().all(|raw_token| {
        let token = raw_token.to_ascii_lowercase();
        if hay
            .iter()
            .any(|field| field.to_ascii_lowercase().contains(&token))
        {
            return true;
        }
        package.advisories.iter().any(|adv| {
            adv.id.to_ascii_lowercase().contains(&token)
                || adv
                    .cves
                    .iter()
                    .any(|cve| cve.to_ascii_lowercase().contains(&token))
        })
    })
}

/// Combined sidebar source + search query predicate evaluated by `Gtk.CustomFilter`.
pub fn matches_package_filter(
    package: &Package,
    source_filter: SourceFilter,
    query: &str,
) -> bool {
    let source_ok = match source_filter {
        SourceFilter::Security => package.has_security_advisory(),
        other => matches_source_filter(package.source, other),
    };
    source_ok && matches_package_query(package, query)
}

fn normalized_progress(progress: Option<f64>) -> f64 {
    match progress {
        Some(f) if f.is_finite() => f.clamp(0.0, 1.0),
        _ => -1.0,
    }
}

fn progress_from_raw(raw: f64) -> Option<f64> {
    (raw >= 0.0 && raw.is_finite()).then(|| raw.clamp(0.0, 1.0))
}

fn filter_fields_equal(a: &Package, b: &Package) -> bool {
    a.id == b.id
        && a.name == b.name
        && a.source == b.source
        && a.repo == b.repo
        && a.arch == b.arch
        && a.version == b.version
        && a.old_version == b.old_version
        && a.detail == b.detail
        && a.summary == b.summary
        && a.advisories == b.advisories
}

mod imp {
    use super::*;
    use glib::Properties;

    #[derive(Properties, Default)]
    #[properties(wrapper_type = super::PackageObject)]
    pub struct PackageObject {
        #[property(get, set)]
        pub id: RefCell<String>,
        #[property(get, set)]
        pub name: RefCell<String>,
        #[property(get, set)]
        pub source: RefCell<String>,
        #[property(get, set)]
        pub old_version: RefCell<String>,
        #[property(get, set)]
        pub version: RefCell<String>,
        #[property(get, set)]
        pub repo: RefCell<String>,
        #[property(get, set)]
        pub size_bytes: Cell<u64>,
        #[property(get, set)]
        pub status: RefCell<String>,
        #[property(get, set)]
        pub is_current: Cell<bool>,
        #[property(get, set)]
        pub item_progress: Cell<f64>,
        #[property(get, set)]
        pub needs_reboot: Cell<bool>,
        #[property(get, set)]
        pub extension_label: RefCell<String>,
        #[property(get, set)]
        pub advisory_summary: RefCell<String>,
        #[property(get, set)]
        pub selected: Cell<bool>,
        #[property(get)]
        pub revision: Cell<u64>,
        pub package: RefCell<Option<Package>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PackageObject {
        const NAME: &'static str = "FedoraUpdaterPackageObject";
        type Type = super::PackageObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for PackageObject {}
}

glib::wrapper! {
    /// GObject wrapper representing a [`Package`] inside virtualized `Gtk.ListView` models.
    pub struct PackageObject(ObjectSubclass<imp::PackageObject>);
}

impl Default for PackageObject {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PackageObject {
    pub fn from_package(pkg: &Package, is_current: bool, item_progress: Option<f64>) -> Self {
        let obj: Self = glib::Object::new();
        obj.set_selected(true);
        obj.apply_snapshot(pkg, is_current, item_progress);
        obj
    }

    pub fn bump_revision(&self) {
        let rev = self.imp().revision.get().wrapping_add(1);
        self.imp().revision.set(rev);
        self.notify_revision();
    }

    fn apply_snapshot(&self, pkg: &Package, is_current: bool, item_progress: Option<f64>) {
        self.set_id(pkg.id.clone());
        self.set_name(pkg.name.clone());
        self.set_source(pkg.source.badge().to_string());
        self.set_old_version(pkg.old_version.clone().unwrap_or_default());
        self.set_version(pkg.version.clone());
        self.set_repo(pkg.repo.clone());
        self.set_size_bytes(pkg.size_bytes.unwrap_or(0));
        self.set_status(pkg.status.label().to_string());
        self.set_is_current(is_current);
        self.set_item_progress(normalized_progress(item_progress));
        self.set_needs_reboot(package_needs_reboot(pkg));
        self.set_extension_label(
            extension_micro_tag(&pkg.related_extensions()).unwrap_or_default(),
        );
        self.set_advisory_summary(pkg.advisory_summary());
        *self.imp().package.borrow_mut() = Some(pkg.clone());
    }

    /// Updates this item in place when any displayed/filter field or live progress changed.
    /// Returns `(any_changed, filter_metadata_changed)`.
    pub fn update_from(
        &self,
        pkg: &Package,
        is_current: bool,
        item_progress: Option<f64>,
    ) -> (bool, bool) {
        let next_progress = normalized_progress(item_progress);
        let unchanged = {
            let existing = self.imp().package.borrow();
            existing.as_ref() == Some(pkg)
                && self.is_current() == is_current
                && (self.item_progress() - next_progress).abs() <= f64::EPSILON
        };
        if unchanged {
            return (false, false);
        }

        let filter_changed = {
            let existing = self.imp().package.borrow();
            existing
                .as_ref()
                .is_none_or(|prev| !filter_fields_equal(prev, pkg))
        };

        self.apply_snapshot(pkg, is_current, item_progress);
        self.bump_revision();
        (true, filter_changed)
    }

    pub fn package(&self) -> Option<Package> {
        self.imp().package.borrow().clone()
    }
}

type SelectionChangedCb = Rc<RefCell<Option<Box<dyn Fn()>>>>;

/// Reusable widget tree inside a virtualized `Gtk.ListItem`.
#[derive(Clone)]
pub struct QueueRowWidgets {
    root: gtk::Box,
    check_btn: gtk::CheckButton,
    ignore_check: Rc<Cell<bool>>,
    source_badge: gtk::Label,
    name_label: gtk::Label,
    sec_tag: gtk::Label,
    reboot_tag: gtk::Label,
    ext_tag: gtk::Label,
    subtitle_label: gtk::Label,
    trailing_label: gtk::Label,
}

impl QueueRowWidgets {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        root.add_css_class("pkg-row");
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        root.set_margin_start(14);
        root.set_margin_end(14);

        let check_btn = gtk::CheckButton::builder()
            .visible(false)
            .active(true)
            .valign(gtk::Align::Center)
            .build();
        check_btn.update_property(&[gtk::accessible::Property::Label("Select package for update")]);

        let source_badge = gtk::Label::builder()
            .width_chars(3)
            .valign(gtk::Align::Center)
            .css_classes(["package-badge"])
            .build();

        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        title_row.set_valign(gtk::Align::Center);

        let name_label = gtk::Label::builder()
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["pkg-title"])
            .build();

        let sec_tag = gtk::Label::builder()
            .visible(false)
            .valign(gtk::Align::Center)
            .css_classes(["micro-tag", "tag-sec"])
            .build();

        let reboot_tag = gtk::Label::builder()
            .label("Restart")
            .visible(false)
            .valign(gtk::Align::Center)
            .css_classes(["micro-tag", "tag-reboot"])
            .build();

        let ext_tag = gtk::Label::builder()
            .visible(false)
            .valign(gtk::Align::Center)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["micro-tag", "tag-ext"])
            .build();

        title_row.append(&name_label);
        title_row.append(&sec_tag);
        title_row.append(&reboot_tag);
        title_row.append(&ext_tag);

        let subtitle_label = gtk::Label::builder()
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "dimmed", "ver-diff", "numeric"])
            .build();

        let main_col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        main_col.set_hexpand(true);
        main_col.set_valign(gtk::Align::Center);
        main_col.append(&title_row);
        main_col.append(&subtitle_label);

        let trailing_label = gtk::Label::builder()
            .valign(gtk::Align::Center)
            .halign(gtk::Align::End)
            .css_classes(["caption", "numeric"])
            .build();

        root.append(&check_btn);
        root.append(&source_badge);
        root.append(&main_col);
        root.append(&trailing_label);

        Self {
            root,
            check_btn,
            ignore_check: Rc::new(Cell::new(false)),
            source_badge,
            name_label,
            sec_tag,
            reboot_tag,
            ext_tag,
            subtitle_label,
            trailing_label,
        }
    }

    fn paint_from_object(&self, obj: &PackageObject, mode: QueueRowMode, selectable: bool) {
        let Some(pkg) = obj.package() else {
            return;
        };
        self.paint_package(
            &pkg,
            mode,
            obj.is_current(),
            progress_from_raw(obj.item_progress()),
            obj.selected(),
            selectable,
        );
    }

    fn paint_package(
        &self,
        pkg: &Package,
        mode: QueueRowMode,
        is_current: bool,
        item_progress: Option<f64>,
        is_selected: bool,
        selectable: bool,
    ) {
        let is_atomic = !pkg.source.supports_selective_apply();
        let show_check = selectable && mode == QueueRowMode::Review;
        self.ignore_check.set(true);
        self.check_btn.set_active(if is_atomic { true } else { is_selected });
        self.check_btn.set_sensitive(!is_atomic);
        self.check_btn.set_tooltip_text(
            is_atomic.then_some("Atomic system updates apply together"),
        );
        self.check_btn.set_visible(show_check);
        self.ignore_check.set(false);
        for cls in UpdateSource::ALL_CSS_CLASSES {
            self.source_badge.remove_css_class(cls);
        }
        self.source_badge.set_label(pkg.source.badge());
        self.source_badge.add_css_class(pkg.source.css_class());

        self.name_label.set_label(&pkg.name);
        self.subtitle_label.set_label(&queue_row_subtitle(pkg));

        if let Some(tag) = pkg.primary_security_tag() {
            self.sec_tag.set_label(&tag);
            self.sec_tag.set_visible(true);
        } else {
            self.sec_tag.set_visible(false);
        }

        self.reboot_tag.set_visible(package_needs_reboot(pkg));

        if let Some(ext) = extension_micro_tag(&pkg.related_extensions()) {
            self.ext_tag.set_label(&ext);
            self.ext_tag.set_visible(true);
        } else {
            self.ext_tag.set_visible(false);
        }

        for cls in ["active", "pending", "done"] {
            self.root.remove_css_class(cls);
        }
        self.name_label.remove_css_class("dimmed");
        for cls in ["dimmed", "accent", "success", "error", "phase-tag"] {
            self.trailing_label.remove_css_class(cls);
        }

        match mode {
            QueueRowMode::Review => {
                if let Some(size) = pkg.size_label() {
                    self.trailing_label.set_label(&size);
                    self.trailing_label.add_css_class("dimmed");
                    self.trailing_label.set_visible(true);
                } else {
                    self.trailing_label.set_label("");
                    self.trailing_label.set_visible(false);
                }
            }
            QueueRowMode::Running => {
                let text = running_status_label(pkg.status, is_current, item_progress);
                self.trailing_label.set_label(&text);
                self.trailing_label.set_visible(true);
                match pkg.status {
                    PackageStatus::Downloading | PackageStatus::Installing => {
                        if is_current {
                            self.root.add_css_class("active");
                        }
                        self.trailing_label.add_css_class("accent");
                        self.trailing_label.add_css_class("phase-tag");
                    }
                    PackageStatus::Pending => {
                        self.root.add_css_class("pending");
                        self.name_label.add_css_class("dimmed");
                        self.trailing_label.add_css_class("dimmed");
                    }
                    PackageStatus::Completed => {
                        self.root.add_css_class("done");
                        self.name_label.add_css_class("dimmed");
                        self.trailing_label.add_css_class("success");
                    }
                    PackageStatus::Failed => {
                        self.trailing_label.add_css_class("error");
                    }
                }
            }
            QueueRowMode::Finished => {
                if pkg.status == PackageStatus::Failed {
                    self.trailing_label.set_label("Failed");
                    self.trailing_label.add_css_class("error");
                    self.trailing_label.set_visible(true);
                } else if let Some(size) = pkg.size_label() {
                    self.trailing_label.set_label(&size);
                    self.trailing_label.add_css_class("dimmed");
                    self.trailing_label.set_visible(true);
                } else {
                    self.trailing_label.set_label("");
                    self.trailing_label.set_visible(false);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn check_btn(&self) -> &gtk::CheckButton {
        &self.check_btn
    }

    #[cfg(test)]
    pub fn source_badge(&self) -> &gtk::Label {
        &self.source_badge
    }

    #[cfg(test)]
    pub fn name_label(&self) -> &gtk::Label {
        &self.name_label
    }

    #[cfg(test)]
    pub fn sec_tag(&self) -> &gtk::Label {
        &self.sec_tag
    }

    #[cfg(test)]
    pub fn reboot_tag(&self) -> &gtk::Label {
        &self.reboot_tag
    }

    #[cfg(test)]
    pub fn ext_tag(&self) -> &gtk::Label {
        &self.ext_tag
    }

    #[cfg(test)]
    pub fn subtitle_label(&self) -> &gtk::Label {
        &self.subtitle_label
    }

    #[cfg(test)]
    pub fn trailing_label(&self) -> &gtk::Label {
        &self.trailing_label
    }

    #[cfg(test)]
    pub fn root(&self) -> &gtk::Box {
        &self.root
    }
}

struct RowSlot {
    widgets: QueueRowWidgets,
    bound_obj: Rc<RefCell<Option<PackageObject>>>,
    bound: Option<(PackageObject, glib::SignalHandlerId)>,
}

/// Virtualized package list backed by `gio::ListStore` -> `Gtk.FilterListModel` -> `Gtk.ListView`.
pub struct QueueList {
    store: gio::ListStore,
    filter: gtk::CustomFilter,
    filter_model: gtk::FilterListModel,
    selection: gtk::SingleSelection,
    list_view: gtk::ListView,
    source_filter: Rc<Cell<SourceFilter>>,
    query: Rc<RefCell<String>>,
    selectable: Rc<Cell<bool>>,
    on_selection_changed: SelectionChangedCb,
}

impl QueueList {
    pub fn new(mode: QueueRowMode) -> Self {
        let store = gio::ListStore::new::<PackageObject>();
        let source_filter = Rc::new(Cell::new(SourceFilter::All));
        let query = Rc::new(RefCell::new(String::new()));
        let selectable = Rc::new(Cell::new(false));
        let on_selection_changed: SelectionChangedCb = Rc::new(RefCell::new(None));

        let filter = {
            let source_filter = source_filter.clone();
            let query = query.clone();
            gtk::CustomFilter::new(move |obj| {
                let Some(pkg_obj) = obj.downcast_ref::<PackageObject>() else {
                    return false;
                };
                let Some(pkg) = pkg_obj.package() else {
                    return false;
                };
                matches_package_filter(&pkg, source_filter.get(), &query.borrow())
            })
        };

        let filter_model = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::builder()
            .model(&filter_model)
            .autoselect(false)
            .can_unselect(true)
            .build();

        let factory = gtk::SignalListItemFactory::new();
        let slots: Rc<RefCell<HashMap<usize, RowSlot>>> = Rc::new(RefCell::new(HashMap::new()));

        {
            let slots = slots.clone();
            let cb = on_selection_changed.clone();
            factory.connect_setup(move |_, obj| {
                let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let row = QueueRowWidgets::new();
                let bound_obj: Rc<RefCell<Option<PackageObject>>> = Rc::new(RefCell::new(None));
                {
                    let ignore = row.ignore_check.clone();
                    let bound_for_toggle = bound_obj.clone();
                    let cb_for_toggle = cb.clone();
                    row.check_btn.connect_toggled(move |btn| {
                        if ignore.get() {
                            return;
                        }
                        if let Some(pkg_obj) = bound_for_toggle.borrow().as_ref() {
                            let active = btn.is_active();
                            if pkg_obj.selected() != active {
                                pkg_obj.set_selected(active);
                                if let Some(handler) = cb_for_toggle.borrow().as_ref() {
                                    handler();
                                }
                            }
                        }
                    });
                }
                list_item.set_child(Some(&row.root));
                let key = list_item.as_ptr() as usize;
                slots.borrow_mut().insert(
                    key,
                    RowSlot {
                        widgets: row,
                        bound_obj,
                        bound: None,
                    },
                );
            });
        }

        {
            let slots = slots.clone();
            let selectable_bind = selectable.clone();
            factory.connect_bind(move |_, obj| {
                let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let Some(pkg_obj) = list_item.item().and_downcast::<PackageObject>() else {
                    return;
                };
                let key = list_item.as_ptr() as usize;
                let mut slots_ref = slots.borrow_mut();
                let Some(slot) = slots_ref.get_mut(&key) else {
                    return;
                };
                if let Some((prev_obj, prev_id)) = slot.bound.take() {
                    prev_obj.disconnect(prev_id);
                }
                *slot.bound_obj.borrow_mut() = Some(pkg_obj.clone());
                slot.widgets
                    .paint_from_object(&pkg_obj, mode, selectable_bind.get());
                let row_clone = slot.widgets.clone();
                let sel_flag = selectable_bind.clone();
                let handler_id = pkg_obj.connect_revision_notify(move |item| {
                    row_clone.paint_from_object(item, mode, sel_flag.get());
                });
                slot.bound = Some((pkg_obj, handler_id));
            });
        }

        {
            let slots = slots.clone();
            factory.connect_unbind(move |_, obj| {
                let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let key = list_item.as_ptr() as usize;
                if let Some(slot) = slots.borrow_mut().get_mut(&key) {
                    *slot.bound_obj.borrow_mut() = None;
                    if let Some((prev_obj, prev_id)) = slot.bound.take() {
                        prev_obj.disconnect(prev_id);
                    }
                }
            });
        }

        {
            let slots = slots.clone();
            factory.connect_teardown(move |_, obj| {
                let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let key = list_item.as_ptr() as usize;
                if let Some(mut slot) = slots.borrow_mut().remove(&key) {
                    *slot.bound_obj.borrow_mut() = None;
                    if let Some((prev_obj, prev_id)) = slot.bound.take() {
                        prev_obj.disconnect(prev_id);
                    }
                }
            });
        }

        let list_view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .single_click_activate(true)
            .show_separators(false)
            .css_classes(["queue-list"])
            .build();

        Self {
            store,
            filter,
            filter_model,
            selection,
            list_view,
            source_filter,
            query,
            selectable,
            on_selection_changed,
        }
    }

    pub fn list_view(&self) -> &gtk::ListView {
        &self.list_view
    }

    pub fn set_selectable(&self, selectable: bool) {
        if self.selectable.get() == selectable {
            return;
        }
        self.selectable.set(selectable);
        for pos in 0..self.store.n_items() {
            if let Some(obj) = self.store.item(pos).and_downcast::<PackageObject>() {
                obj.bump_revision();
            }
        }
    }

    pub fn is_selectable(&self) -> bool {
        self.selectable.get()
    }

    pub fn connect_selection_changed(&self, on_change: impl Fn() + 'static) {
        *self.on_selection_changed.borrow_mut() = Some(Box::new(on_change));
    }

    fn notify_selection_changed(&self) {
        if let Some(cb) = self.on_selection_changed.borrow().as_ref() {
            cb();
        }
    }

    pub fn set_all_selected(&self, selected: bool) {
        for pos in 0..self.store.n_items() {
            if let Some(obj) = self.store.item(pos).and_downcast::<PackageObject>() {
                let target = if obj
                    .package()
                    .is_some_and(|p| !p.source.supports_selective_apply())
                {
                    true
                } else {
                    selected
                };
                if obj.selected() != target {
                    obj.set_selected(target);
                    obj.bump_revision();
                }
            }
        }
        self.notify_selection_changed();
    }

    #[cfg(test)]
    pub fn set_selected_by_id(&self, id: &str, selected: bool) -> bool {
        for pos in 0..self.store.n_items() {
            let Some(obj) = self.store.item(pos).and_downcast::<PackageObject>() else {
                continue;
            };
            if obj.id() == id {
                if !selected
                    && obj
                        .package()
                        .is_some_and(|p| !p.source.supports_selective_apply())
                {
                    return false;
                }
                if obj.selected() != selected {
                    obj.set_selected(selected);
                    obj.bump_revision();
                    self.notify_selection_changed();
                }
                return true;
            }
        }
        false
    }

    pub fn selected_ids(&self) -> Vec<String> {
        (0..self.store.n_items())
            .filter_map(|pos| self.store.item(pos).and_downcast::<PackageObject>())
            .filter(|obj| obj.selected())
            .map(|obj| obj.id().to_string())
            .collect()
    }

    pub fn selection_stats(&self) -> (usize, usize, Option<u64>) {
        let total = self.store.n_items() as usize;
        let mut selected = 0usize;
        let mut bytes_sum = 0u64;
        let mut any_bytes = false;
        for pos in 0..self.store.n_items() {
            let Some(obj) = self.store.item(pos).and_downcast::<PackageObject>() else {
                continue;
            };
            if obj.selected() {
                selected += 1;
                if let Some(pkg) = obj.package() {
                    if let Some(sz) = pkg.size_bytes {
                        bytes_sum = bytes_sum.saturating_add(sz);
                        any_bytes = true;
                    }
                }
            }
        }
        (selected, total, any_bytes.then_some(bytes_sum))
    }

    pub fn set_source_filter(&self, filter: SourceFilter) {
        if self.source_filter.get() == filter {
            return;
        }
        self.source_filter.set(filter);
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn set_query(&self, query: &str) {
        if *self.query.borrow() == query {
            return;
        }
        *self.query.borrow_mut() = query.to_string();
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn total_count(&self) -> u32 {
        self.store.n_items()
    }

    pub fn filtered_count(&self) -> u32 {
        self.filter_model.n_items()
    }

    pub fn select_by_id(&self, id: &str) {
        for pos in 0..self.filter_model.n_items() {
            let Some(item) = self.filter_model.item(pos).and_downcast::<PackageObject>() else {
                continue;
            };
            if item.id() == id {
                self.selection.set_selected(pos);
                return;
            }
        }
        self.clear_selection();
    }

    pub fn clear_selection(&self) {
        if self.selection.selected() != gtk::INVALID_LIST_POSITION {
            self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        }
    }

    pub fn connect_package_activated(&self, on_activate: impl Fn(Package) + 'static) {
        let filter_model = self.filter_model.clone();
        self.list_view.connect_activate(move |_, pos| {
            let Some(item) = filter_model.item(pos).and_downcast::<PackageObject>() else {
                return;
            };
            if let Some(pkg) = item.package() {
                on_activate(pkg);
            }
        });
    }

    /// Synchronizes `gio::ListStore` in place keyed by `Package::id`, reusing existing
    /// `PackageObject` instances so 100ms progress ticks never reconstruct rows.
    pub fn sync(&self, packages: &[Package], active_id: Option<&str>, active_progress: Option<f64>) {
        let mut any_filter_field_changed = false;

        // Fast path: identical IDs in identical order -> in-place property updates only.
        if self.store.n_items() as usize == packages.len()
            && packages.iter().enumerate().all(|(idx, pkg)| {
                self.store
                    .item(idx as u32)
                    .and_downcast::<PackageObject>()
                    .is_some_and(|obj| obj.id() == pkg.id)
            })
        {
            for (idx, pkg) in packages.iter().enumerate() {
                let is_current = active_id == Some(pkg.id.as_str());
                let prog = if is_current { active_progress } else { None };
                if let Some(obj) = self.store.item(idx as u32).and_downcast::<PackageObject>() {
                    let (_, filter_changed) = obj.update_from(pkg, is_current, prog);
                    any_filter_field_changed |= filter_changed;
                }
            }
            if any_filter_field_changed {
                self.filter.changed(gtk::FilterChange::Different);
            }
            return;
        }

        // General diff path preserving surviving `PackageObject` instances.
        let mut desired_counts: HashMap<&str, usize> = HashMap::with_capacity(packages.len());
        for pkg in packages {
            *desired_counts.entry(pkg.id.as_str()).or_insert(0) += 1;
        }

        let mut keep_counts: HashMap<String, usize> = HashMap::new();
        let mut indices_to_remove = Vec::new();
        for idx in 0..self.store.n_items() {
            let Some(obj) = self.store.item(idx).and_downcast::<PackageObject>() else {
                indices_to_remove.push(idx);
                continue;
            };
            let id = obj.id().to_string();
            let desired = desired_counts.get(id.as_str()).copied().unwrap_or(0);
            let kept = keep_counts.get(&id).copied().unwrap_or(0);
            if kept < desired {
                keep_counts.insert(id, kept + 1);
            } else {
                indices_to_remove.push(idx);
            }
        }
        for idx in indices_to_remove.into_iter().rev() {
            self.store.remove(idx);
        }

        for (target_idx, pkg) in packages.iter().enumerate() {
            let target = target_idx as u32;
            let is_current = active_id == Some(pkg.id.as_str());
            let prog = if is_current { active_progress } else { None };

            if let Some(existing) = self.store.item(target).and_downcast::<PackageObject>() {
                if existing.id() == pkg.id {
                    let (_, filter_changed) = existing.update_from(pkg, is_current, prog);
                    any_filter_field_changed |= filter_changed;
                    continue;
                }
            }

            let found = (target + 1..self.store.n_items()).find(|&idx| {
                self.store
                    .item(idx)
                    .and_downcast::<PackageObject>()
                    .is_some_and(|obj| obj.id() == pkg.id)
            });

            if let Some(found_idx) = found {
                if let Some(moved) = self.store.item(found_idx).and_downcast::<PackageObject>() {
                    self.store.remove(found_idx);
                    let (_, filter_changed) = moved.update_from(pkg, is_current, prog);
                    any_filter_field_changed |= filter_changed;
                    self.store.insert(target, &moved);
                }
            } else {
                let obj = PackageObject::from_package(pkg, is_current, prog);
                self.store.insert(target, &obj);
            }
        }

        while self.store.n_items() as usize > packages.len() {
            let last = self.store.n_items() - 1;
            self.store.remove(last);
        }

        if any_filter_field_changed {
            self.filter.changed(gtk::FilterChange::Different);
        }
    }

    #[cfg(test)]
    pub fn filtered_ids(&self) -> Vec<String> {
        (0..self.filter_model.n_items())
            .filter_map(|i| {
                self.filter_model
                    .item(i)
                    .and_downcast::<PackageObject>()
                    .map(|o| o.id().to_string())
            })
            .collect()
    }

    #[cfg(test)]
    pub fn find_object(&self, id: &str) -> Option<PackageObject> {
        (0..self.store.n_items())
            .filter_map(|i| self.store.item(i).and_downcast::<PackageObject>())
            .find(|obj| obj.id() == id)
    }

    #[cfg(test)]
    pub fn selected_id(&self) -> Option<String> {
        self.selection
            .selected_item()
            .and_downcast::<PackageObject>()
            .map(|o| o.id().to_string())
    }

    #[cfg(test)]
    pub fn activate_filtered(&self, position: u32) {
        self.list_view.emit_by_name::<()>("activate", &[&position]);
    }

    #[cfg(test)]
    pub fn build_row_for_test(
        pkg: &Package,
        mode: QueueRowMode,
        is_current: bool,
        item_progress: Option<f64>,
    ) -> QueueRowWidgets {
        let widgets = QueueRowWidgets::new();
        widgets.paint_package(pkg, mode, is_current, item_progress, true, false);
        widgets
    }

    #[cfg(test)]
    pub fn build_selectable_row_for_test(
        pkg: &Package,
        is_selected: bool,
    ) -> QueueRowWidgets {
        let widgets = QueueRowWidgets::new();
        widgets.paint_package(pkg, QueueRowMode::Review, false, None, is_selected, true);
        widgets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedora_updater::model::{AdvisoryKind, PackageAdvisory};

    fn sample_flatpak_with_ext() -> Package {
        let mut p = Package::new_dnf("Thunderbird", "x86_64", "128.2.0", "flathub");
        p.id = "flatpak:user:app/org.mozilla.Thunderbird/x86_64/stable".into();
        p.source = UpdateSource::FlatpakUser;
        p.detail = "128.2.0 · flathub · user · installs org.freedesktop.Platform.GL.nvidia-570-133-07, org.freedesktop.Platform.VAAPI.Intel".into();
        p
    }

    #[test]
    fn queue_row_subtitle_prefers_old_to_new_and_omits_redundant_source_suffix() {
        let mut dnf = Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates");
        assert_eq!(queue_row_subtitle(&dnf), "140.0-1.fc44 · updates");
        dnf.old_version = Some("139.0-1.fc44".into());
        assert_eq!(queue_row_subtitle(&dnf), "139.0-1.fc44 → 140.0-1.fc44");

        let fp = sample_flatpak_with_ext();
        let fp_sub = queue_row_subtitle(&fp);
        assert_eq!(fp_sub, "128.2.0 · flathub · user");
        assert!(!fp_sub.contains("installs"));
        assert!(!fp_sub.contains("Apps"));
        assert!(!fp_sub.contains("System"));
    }

    #[test]
    fn extension_micro_tag_and_reboot_detection_work_cleanly() {
        let fp = sample_flatpak_with_ext();
        assert_eq!(
            extension_micro_tag(&fp.related_extensions()).as_deref(),
            Some("+ GL.nvidia-570-133-07 (+1)")
        );
        assert!(!package_needs_reboot(&fp));

        let kernel = Package::new_dnf("kernel-core", "x86_64", "6.14.11-300.fc44", "updates");
        assert!(package_needs_reboot(&kernel));
        let kernel_tools =
            Package::new_dnf("kernel-tools", "x86_64", "6.14.11-300.fc44", "updates");
        assert!(!package_needs_reboot(&kernel_tools));

        let ostree = Package::new_rpm_ostree(
            "upgraded",
            "mesa-dri-drivers",
            "x86_64",
            Some("25.0.2-1.fc44".into()),
            "25.0.4-1.fc44",
            "fedora-ostree",
            "Upgraded",
            Some("Atomic system deployment".into()),
        );
        assert!(package_needs_reboot(&ostree));
        assert!(!ostree.source.supports_selective_apply());
    }

    #[test]
    fn package_filter_matches_source_and_multi_token_queries() {
        use fedora_updater::model::SourceCategory;

        let mut openssl = Package::new_dnf("openssl", "x86_64", "3.2.2-3.fc44", "updates");
        openssl.old_version = Some("3.2.2-1.fc44".into());
        openssl.advisories.push(PackageAdvisory {
            id: "FEDORA-2026-84a1".into(),
            kind: AdvisoryKind::Security,
            severity: Some("Important".into()),
            cves: vec!["CVE-2026-2401".into()],
            bugzillas: vec![],
            description: None,
        });
        let fp = sample_flatpak_with_ext();
        let mut fw = Package::new_dnf("UEFI dbx", "", "20241101", "LVFS");
        fw.source = UpdateSource::Firmware;

        assert!(matches_package_filter(&openssl, SourceFilter::All, ""));
        assert!(matches_package_filter(
            &openssl,
            SourceFilter::Category(SourceCategory::System),
            "ssl updates"
        ));
        assert!(matches_package_filter(
            &openssl,
            SourceFilter::Category(SourceCategory::System),
            "cve-2026-2401"
        ));
        assert!(!matches_package_filter(
            &openssl,
            SourceFilter::Category(SourceCategory::Apps),
            "ssl"
        ));
        assert!(matches_package_filter(
            &fp,
            SourceFilter::Category(SourceCategory::Apps),
            "thunder flathub"
        ));
        assert!(!matches_package_filter(
            &fp,
            SourceFilter::Category(SourceCategory::Firmware),
            ""
        ));
        assert!(matches_package_filter(
            &fw,
            SourceFilter::Category(SourceCategory::Firmware),
            "lvfs"
        ));
    }

    #[test]
    fn running_status_label_formats_percentages_only_for_current_item() {
        assert_eq!(
            running_status_label(PackageStatus::Pending, false, None),
            "Queued"
        );
        assert_eq!(
            running_status_label(PackageStatus::Downloading, true, Some(0.42)),
            "Downloading · 42%"
        );
        assert_eq!(
            running_status_label(PackageStatus::Downloading, false, Some(0.42)),
            "Downloading"
        );
        assert_eq!(
            running_status_label(PackageStatus::Installing, true, Some(1.0)),
            "Installing · 100%"
        );
        assert_eq!(
            running_status_label(PackageStatus::Completed, false, None),
            "Done"
        );
        assert_eq!(
            running_status_label(PackageStatus::Failed, false, None),
            "Failed"
        );
    }
}
