//! Main application window — thin view over the pure state machine.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Builder, Button, Label, ProgressBar, Stack, TextView, ToggleButton};
use libadwaita::prelude::*;
use libadwaita::{
    Application, ApplicationWindow, Banner, BottomSheet, Breakpoint, ExpanderRow,
    NavigationSplitView, PreferencesGroup, StatusPage, StyleManager, Toast, ToastOverlay,
    ToolbarStyle, ToolbarView, WindowTitle,
};

use fedora_updater::backend::{dnf, dnf_dbus, fwupd_dbus};
use fedora_updater::console::ConsoleBuffer;
use fedora_updater::model::{
    count_by_source, ApplyMode, AuthPurpose, DetailView, IdleScreen, Package, PackageStatus,
    RetryAction, RunPass, UpdateSource,
};
use fedora_updater::orchestrator::{
    can_skip_apply_auth_ui, can_skip_check_auth_ui, cancel_background_work, load_history_system,
    release_privileges, run_apply_all_with_mode, run_check_all, run_systemctl_reboot,
    session_started_event, WorkerMsg,
};
use fedora_updater::presenter::{
    about_time_left, activity_log_subtitle, apply_cta_label, cache_activity_note, check_progress,
    checking_footnote, done_clean_summary, done_header_subtitle, fail_retry_label,
    history_subtitle, idle_chrome, now_playing_subtitle, now_playing_title,
    phase_completion_notification, pipeline_stage, quiet_ui, ready_banner_title,
    reboot_trigger_summary, run_heading, running_progress_captions, selection_summary_text,
    source_summary_line, window_header, DesktopNotification, HeaderView, PipelineStage, QuietInput,
    AUTH_DESCRIPTION, COMPLETION_NOTIFICATION_ID,
};
use fedora_updater::privilege::{
    dnf_daemon_is_running, kill_recorded_helper, kill_recorded_local_command,
};
use fedora_updater::state::{self, session_started_purpose, AppState, Event, Phase, RunningState};

use super::{
    activity_log, charts, dialogs, history, inspector, obj, prefs, preview, queue_list, rows,
    sidebar,
};

const WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/window.ui"));

struct Widgets {
    window: ApplicationWindow,
    split_view: NavigationSplitView,
    detail_toolbar: ToolbarView,
    detail_view: Cell<DetailView>,
    detail_view_stack: Stack,
    back_to_updates_btn: Button,
    inspector_split: libadwaita::OverlaySplitView,
    inspector: inspector::PackageInspector,
    inspected_id: RefCell<Option<String>>,
    activity_log: activity_log::ActivityLogView,
    history_view: history::HistoryView,
    history_loaded: Cell<bool>,
    native_dnf_available: Cell<bool>,
    native_fwupd_available: Cell<bool>,
    apply_mode: Cell<ApplyMode>,
    ignore_select_all: Cell<bool>,
    source_filter: Cell<sidebar::SourceFilter>,
    #[cfg_attr(not(test), allow(dead_code))] // Verified by headless widget E2E tests.
    ready_toolbar: ToolbarView,
    #[cfg_attr(not(test), allow(dead_code))] // Verified by headless widget E2E tests.
    done_toolbar: ToolbarView,
    window_title: WindowTitle,
    /// Un-narrowed subtitle. Header paint reads [`Widgets::narrow`] so a later render keeps it hidden.
    header_subtitle: RefCell<String>,
    compact: Cell<bool>,
    narrow: Cell<bool>,
    sidebar_open: Cell<bool>,
    ignore_sidebar_toggle: Cell<bool>,
    sidebar_toggle: ToggleButton,
    sidebar_close_btn: Button,
    sidebar: sidebar::Sidebar,
    idle_upgrade_slot: gtk::Box,
    ready_upgrade_slot: gtk::Box,
    done_upgrade_slot: gtk::Box,
    idle_upgrade: sidebar::UpgradeCard,
    ready_upgrade: sidebar::UpgradeCard,
    done_upgrade: sidebar::UpgradeCard,
    release_upgrade_version: Cell<Option<u32>>,
    release_upgrade_days: Cell<Option<u32>>,
    sidebar_upgrade_allowed: Cell<bool>,
    inline_upgrade_slot: Cell<sidebar::InlineUpgradeSlot>,
    persist_prefs: Cell<bool>,
    prefs: RefCell<prefs::UiPrefs>,
    prefs_save_source: RefCell<Option<glib::SourceId>>,
    header_refresh: Button,
    stack: Stack,
    toast: ToastOverlay,
    idle_page: StatusPage,
    idle_btn: Button,
    idle_log: Button,
    /// True only on the first-run "Check for Updates" page, so Enter starts a check.
    focus_idle_check: Cell<bool>,
    auth_page: StatusPage,
    auth_cancel: Button,
    check_progress: ProgressBar,
    check_progress_meta: Label,
    check_progress_pct: Label,
    check_list_header_clamp: libadwaita::Clamp,
    check_list_scroll: gtk::ScrolledWindow,
    check_queue: queue_list::QueueList,
    ready_banner: Banner,
    ready_filter_clamp: libadwaita::Clamp,
    ready_selection_bar: libadwaita::Clamp,
    ready_select_all: gtk::CheckButton,
    ready_selection_summary: Label,
    ready_search: gtk::SearchEntry,
    ready_filter_status: Label,
    ready_clear_filter: Button,
    ready_empty_clear_btn: Button,
    ready_list_stack: Stack,
    ready_queue: queue_list::QueueList,
    ready_update: Button,
    ready_split_update: libadwaita::SplitButton,
    run_stop: Button,
    stage_download: Label,
    stage_prepare: Label,
    stage_install: Label,
    stage_verify: Label,
    run_stall: Banner,
    run_progress: ProgressBar,
    run_progress_meta: Label,
    run_progress_pct: Label,
    progress_ring: charts::ProgressRing,
    now_playing_badge: Label,
    now_playing_title: Label,
    now_playing_subtitle: Label,
    now_playing_size: Label,
    now_playing_status_stack: Stack,
    now_playing_status_icon: gtk::Image,
    run_list_scroll: gtk::ScrolledWindow,
    run_list_slot: gtk::Box,
    run_queue: queue_list::QueueList,
    run_completed_group: PreferencesGroup,
    run_completed: ExpanderRow,
    run_completed_rows: RefCell<Vec<gtk::Widget>>,
    run_completed_keys: RefCell<Vec<String>>,
    run_completed_user_open: Cell<bool>,
    ignore_expand: Cell<bool>,
    console_sheet: BottomSheet,
    console_toggle: Button,
    console_chevron: gtk::Image,
    console_line: Label,
    console_full: TextView,
    console_copy_btn: Button,
    console_close_btn: Button,
    done_btn: Button,
    done_retry_btn: Button,
    done_summary_clamp: libadwaita::Clamp,
    done_summary_detail: Label,
    done_reboot_clamp: libadwaita::Clamp,
    done_reboot_detail: Label,
    done_reboot_btn: Button,
    done_problem_banner: Banner,
    done_installed_header_clamp: libadwaita::Clamp,
    done_installed_heading: Label,
    done_installed_scroll: gtk::ScrolledWindow,
    done_queue: queue_list::QueueList,
    fail_page: StatusPage,
    fail_retry: Button,
    /// Last byte from the backend, including a carriage-return redraw.
    /// `None` until this run paints the Running page, so auth time is not quiet time.
    last_output: Cell<Option<Instant>>,
    cache_cursor: RefCell<Option<(PathBuf, u64)>>,
    notification_sent: Cell<bool>,
    reboot_blocker_detail: RefCell<Option<String>>,
}

fn set_accessible_label(widget: &impl IsA<gtk::Accessible>, label: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

fn widgets_from_builder(builder: &Builder, window: ApplicationWindow) -> Rc<Widgets> {
    let sidebar_host: gtk::Box = obj(builder, "sidebar_host");
    let sidebar = sidebar::Sidebar::new();
    sidebar_host.append(sidebar.root());

    let inspector_host: gtk::Box = obj(builder, "inspector_host");
    let inspector = inspector::PackageInspector::new();
    inspector_host.append(inspector.root());

    let activity_log_host: gtk::Box = obj(builder, "activity_log_host");
    let activity_log = activity_log::ActivityLogView::new();
    activity_log_host.append(activity_log.root());

    let history_host: gtk::Box = obj(builder, "history_host");
    let history_view = history::HistoryView::new();
    history_host.append(history_view.root());

    let check_queue = queue_list::QueueList::new(queue_list::QueueRowMode::Review);
    let check_list_clamp: libadwaita::ClampScrollable = obj(builder, "check_list_clamp");
    check_list_clamp.set_child(Some(check_queue.list_view()));

    let ready_queue = queue_list::QueueList::new(queue_list::QueueRowMode::Review);
    let ready_list_clamp: libadwaita::ClampScrollable = obj(builder, "ready_list_clamp");
    ready_list_clamp.set_child(Some(ready_queue.list_view()));

    let run_queue = queue_list::QueueList::new(queue_list::QueueRowMode::Running);
    run_queue.list_view().add_css_class("inline");
    let run_list_slot: gtk::Box = obj(builder, "run_list_slot");
    run_list_slot.append(run_queue.list_view());

    let done_queue = queue_list::QueueList::new(queue_list::QueueRowMode::Finished);
    let done_list_clamp: libadwaita::ClampScrollable = obj(builder, "done_list_clamp");
    done_list_clamp.set_child(Some(done_queue.list_view()));

    let idle_upgrade_slot: gtk::Box = obj(builder, "idle_upgrade_slot");
    let idle_upgrade = sidebar::UpgradeCard::new_inline();
    idle_upgrade_slot.append(idle_upgrade.root());

    let ready_upgrade_slot: gtk::Box = obj(builder, "ready_upgrade_slot");
    let ready_upgrade = sidebar::UpgradeCard::new_inline();
    ready_upgrade_slot.append(ready_upgrade.root());

    let done_upgrade_slot: gtk::Box = obj(builder, "done_upgrade_slot");
    let done_upgrade = sidebar::UpgradeCard::new_inline();
    done_upgrade_slot.append(done_upgrade.root());

    let now_playing_ring_slot: gtk::Box = obj(builder, "now_playing_ring_slot");
    let progress_ring = charts::ProgressRing::new();
    now_playing_ring_slot.append(progress_ring.widget());

    let sidebar_toggle: ToggleButton = obj(builder, "sidebar_toggle");
    let sidebar_close_btn: Button = obj(builder, "sidebar_close_btn");
    let back_to_updates_btn: Button = obj(builder, "back_to_updates_btn");
    let header_refresh: Button = obj(builder, "header_refresh");
    let header_menu: gtk::MenuButton = obj(builder, "header_menu");
    let console_toggle: Button = obj(builder, "console_toggle");
    let console_close_btn: Button = obj(builder, "console_close_btn");
    set_accessible_label(&sidebar_toggle, "Toggle Sidebar");
    set_accessible_label(&sidebar_close_btn, "Return to Updates");
    set_accessible_label(&back_to_updates_btn, "Back to Updates");
    set_accessible_label(&header_refresh, "Refresh");
    set_accessible_label(&header_menu, "Main Menu");
    set_accessible_label(&console_toggle, "Toggle Live Console");
    set_accessible_label(&console_close_btn, "Close Live Console");

    bind_style_manager(&window);

    Rc::new(Widgets {
        window,
        split_view: obj(builder, "split_view"),
        detail_toolbar: obj(builder, "detail_toolbar"),
        detail_view: Cell::new(DetailView::Updates),
        detail_view_stack: obj(builder, "detail_view_stack"),
        back_to_updates_btn,
        inspector_split: obj(builder, "inspector_split"),
        inspector,
        inspected_id: RefCell::new(None),
        activity_log,
        history_view,
        history_loaded: Cell::new(false),
        native_dnf_available: Cell::new(false),
        native_fwupd_available: Cell::new(false),
        apply_mode: Cell::new(ApplyMode::Live),
        ignore_select_all: Cell::new(false),
        source_filter: Cell::new(sidebar::SourceFilter::All),
        ready_toolbar: obj(builder, "ready_toolbar"),
        done_toolbar: obj(builder, "done_toolbar"),
        window_title: obj(builder, "window_title"),
        header_subtitle: RefCell::new(String::new()),
        compact: Cell::new(false),
        narrow: Cell::new(false),
        sidebar_open: Cell::new(true),
        ignore_sidebar_toggle: Cell::new(false),
        sidebar_toggle,
        sidebar_close_btn,
        sidebar,
        idle_upgrade_slot,
        ready_upgrade_slot,
        done_upgrade_slot,
        idle_upgrade,
        ready_upgrade,
        done_upgrade,
        release_upgrade_version: Cell::new(None),
        release_upgrade_days: Cell::new(None),
        sidebar_upgrade_allowed: Cell::new(true),
        inline_upgrade_slot: Cell::new(sidebar::InlineUpgradeSlot::Idle),
        persist_prefs: Cell::new(false),
        prefs: RefCell::new(prefs::UiPrefs::default()),
        prefs_save_source: RefCell::new(None),
        header_refresh,
        stack: obj(builder, "stack"),
        toast: obj(builder, "toast"),
        idle_page: obj(builder, "idle_page"),
        idle_btn: obj(builder, "idle_btn"),
        idle_log: obj(builder, "idle_log"),
        focus_idle_check: Cell::new(false),
        auth_page: obj(builder, "auth_page"),
        auth_cancel: obj(builder, "auth_cancel"),
        check_progress: obj(builder, "check_progress"),
        check_progress_meta: obj(builder, "check_progress_meta"),
        check_progress_pct: obj(builder, "check_progress_pct"),
        check_list_header_clamp: obj(builder, "check_list_header_clamp"),
        check_list_scroll: obj(builder, "check_list_scroll"),
        check_queue,
        ready_banner: obj(builder, "ready_banner"),
        ready_filter_clamp: obj(builder, "ready_filter_clamp"),
        ready_selection_bar: obj(builder, "ready_selection_bar"),
        ready_select_all: obj(builder, "ready_select_all"),
        ready_selection_summary: obj(builder, "ready_selection_summary"),
        ready_search: obj(builder, "ready_search"),
        ready_filter_status: obj(builder, "ready_filter_status"),
        ready_clear_filter: obj(builder, "ready_clear_filter"),
        ready_empty_clear_btn: obj(builder, "ready_empty_clear_btn"),
        ready_list_stack: obj(builder, "ready_list_stack"),
        ready_queue,
        ready_update: obj(builder, "ready_update"),
        ready_split_update: obj(builder, "ready_split_update"),
        run_stop: obj(builder, "run_stop"),
        stage_download: obj(builder, "stage_download"),
        stage_prepare: obj(builder, "stage_prepare"),
        stage_install: obj(builder, "stage_install"),
        stage_verify: obj(builder, "stage_verify"),
        run_stall: obj(builder, "run_stall"),
        run_progress: obj(builder, "run_progress"),
        run_progress_meta: obj(builder, "run_progress_meta"),
        run_progress_pct: obj(builder, "run_progress_pct"),
        progress_ring,
        now_playing_badge: obj(builder, "now_playing_badge"),
        now_playing_title: obj(builder, "now_playing_title"),
        now_playing_subtitle: obj(builder, "now_playing_subtitle"),
        now_playing_size: obj(builder, "now_playing_size"),
        now_playing_status_stack: obj(builder, "now_playing_status_stack"),
        now_playing_status_icon: obj(builder, "now_playing_status_icon"),
        run_list_scroll: obj(builder, "run_list_scroll"),
        run_list_slot,
        run_queue,
        run_completed_group: obj(builder, "run_completed_group"),
        run_completed: obj(builder, "run_completed"),
        run_completed_rows: RefCell::new(Vec::new()),
        run_completed_keys: RefCell::new(Vec::new()),
        run_completed_user_open: Cell::new(false),
        ignore_expand: Cell::new(false),
        console_sheet: obj(builder, "console_sheet"),
        console_toggle,
        console_chevron: obj(builder, "console_chevron"),
        console_line: obj(builder, "console_line"),
        console_full: obj(builder, "console_full"),
        console_copy_btn: obj(builder, "console_copy_btn"),
        console_close_btn,
        done_btn: obj(builder, "done_btn"),
        done_retry_btn: obj(builder, "done_retry_btn"),
        done_summary_clamp: obj(builder, "done_summary_clamp"),
        done_summary_detail: obj(builder, "done_summary_detail"),
        done_reboot_clamp: obj(builder, "done_reboot_clamp"),
        done_reboot_detail: obj(builder, "done_reboot_detail"),
        done_reboot_btn: obj(builder, "done_reboot_btn"),
        done_problem_banner: obj(builder, "done_problem_banner"),
        done_installed_header_clamp: obj(builder, "done_installed_header_clamp"),
        done_installed_heading: obj(builder, "done_installed_heading"),
        done_installed_scroll: obj(builder, "done_installed_scroll"),
        done_queue,
        fail_page: obj(builder, "fail_page"),
        fail_retry: obj(builder, "fail_retry"),
        last_output: Cell::new(None),
        cache_cursor: RefCell::new(None),
        notification_sent: Cell::new(false),
        reboot_blocker_detail: RefCell::new(None),
    })
}

fn sync_dark_class(style_mgr: &StyleManager, window: &ApplicationWindow) {
    if style_mgr.is_dark() {
        window.add_css_class("is-dark");
    } else {
        window.remove_css_class("is-dark");
    }
}

fn bind_style_manager(window: &ApplicationWindow) {
    let style_mgr = StyleManager::default();
    sync_dark_class(&style_mgr, window);
    let win_weak = window.downgrade();
    let handler_id = style_mgr.connect_dark_notify(move |mgr| {
        if let Some(win) = win_weak.upgrade() {
            sync_dark_class(mgr, &win);
        }
    });
    let mgr_for_destroy = style_mgr.clone();
    let handler_cell = Cell::new(Some(handler_id));
    window.connect_destroy(move |_| {
        if let Some(id) = handler_cell.take() {
            mgr_for_destroy.disconnect(id);
        }
    });
}

fn is_compact_viewport(w: &Widgets) -> bool {
    w.compact.get() || w.narrow.get()
}

fn effective_sidebar_collapsed(w: &Widgets) -> bool {
    is_compact_viewport(w) || !w.sidebar_open.get()
}

fn sync_shell_layout(w: &Widgets) {
    let compact = is_compact_viewport(w);
    let collapsed = compact || !w.sidebar_open.get();
    w.split_view.set_collapsed(collapsed);
    if !compact {
        w.split_view.set_show_content(true);
    }

    let toggle_active = if collapsed {
        !w.split_view.shows_content()
    } else {
        true
    };
    w.ignore_sidebar_toggle.set(true);
    w.sidebar_toggle.set_active(toggle_active);
    w.ignore_sidebar_toggle.set(false);
    w.sidebar_toggle.set_tooltip_text(Some(if toggle_active {
        "Hide Sidebar (F9)"
    } else {
        "Show Sidebar (F9)"
    }));
    w.sidebar_close_btn
        .set_visible(collapsed && !w.split_view.shows_content());
    w.inspector_split.set_collapsed(compact);

    sync_release_upgrade_cards(w);
}

fn capture_window_prefs(w: &Widgets) -> prefs::UiPrefs {
    let mut current = *w.prefs.borrow();
    current.sidebar_open = w.sidebar_open.get();
    if !w.window.is_maximized() && !w.window.is_fullscreen() {
        let (default_w, default_h) = w.window.default_size();
        let width = if w.window.width() > 0 {
            w.window.width()
        } else {
            default_w
        };
        let height = if w.window.height() > 0 {
            w.window.height()
        } else {
            default_h
        };
        if width >= prefs::MIN_WINDOW_WIDTH {
            current.window_width = width;
        }
        if height >= prefs::MIN_WINDOW_HEIGHT {
            current.window_height = height;
        }
    }
    let normalized = current.normalized();
    *w.prefs.borrow_mut() = normalized;
    normalized
}

fn schedule_prefs_save(w: &Rc<Widgets>) {
    if !w.persist_prefs.get() {
        return;
    }
    capture_window_prefs(w);
    if let Some(source) = w.prefs_save_source.borrow_mut().take() {
        source.remove();
    }
    let weak = Rc::downgrade(w);
    let source = glib::timeout_add_local_once(Duration::from_millis(350), move || {
        let Some(widgets) = weak.upgrade() else {
            return;
        };
        *widgets.prefs_save_source.borrow_mut() = None;
        if widgets.persist_prefs.get() {
            let latest = capture_window_prefs(&widgets);
            let _ = prefs::save(&latest);
        }
    });
    *w.prefs_save_source.borrow_mut() = Some(source);
}

fn flush_prefs_on_close(w: &Widgets) {
    if let Some(source) = w.prefs_save_source.borrow_mut().take() {
        source.remove();
    }
    if !w.persist_prefs.get() {
        return;
    }
    let latest = capture_window_prefs(w);
    let _ = prefs::save(&latest);
}

fn toggle_sidebar(w: &Rc<Widgets>) {
    if is_compact_viewport(w) {
        w.split_view.set_show_content(!w.split_view.shows_content());
        sync_shell_layout(w);
    } else {
        let next = !w.sidebar_open.get();
        w.sidebar_open.set(next);
        sync_shell_layout(w);
        schedule_prefs_save(w);
    }
}

#[cfg(test)]
impl Widgets {
    fn for_test() -> Rc<Self> {
        let builder = Builder::from_string(WINDOW_UI);
        let window: ApplicationWindow = obj(&builder, "window");
        widgets_from_builder(&builder, window)
    }
}

fn apply_preview_config(cfg: &preview::PreviewConfig, w: &Widgets) {
    w.sidebar_open.set(cfg.sidebar_open);
    w.detail_view.set(cfg.detail_view);
    w.sidebar.select_view(cfg.detail_view);
    if cfg.enable_native_capabilities {
        w.native_dnf_available.set(true);
        w.native_fwupd_available.set(true);
        w.sidebar.set_history_available(true);
    }
    if !cfg.sample_history.is_empty() {
        w.history_view.set_entries(cfg.sample_history.clone());
        w.history_loaded.set(true);
    }
    for (bps, done, total) in &cfg.sample_throughput {
        w.sidebar
            .push_throughput_sample(*bps, Some((*done, *total)));
    }
    if cfg.expand_completed {
        w.ignore_expand.set(true);
        w.run_completed_user_open.set(true);
        w.run_completed.set_enable_expansion(true);
        w.run_completed.set_expanded(true);
        w.ignore_expand.set(false);
    }
}

pub fn build(app: &Application) {
    let preview_cfg = preview::initial_config();
    let is_preview = preview_cfg.is_some();
    let initial_state = preview_cfg
        .as_ref()
        .map(|cfg| cfg.state.clone())
        .unwrap_or_default();
    let state = Rc::new(RefCell::new(initial_state));
    let console = Rc::new(RefCell::new(preview::initial_console()));

    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../style.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let builder = Builder::from_string(WINDOW_UI);
    let window: ApplicationWindow = obj(&builder, "window");
    window.set_application(Some(app));

    let loaded_prefs = prefs::load();
    window.set_default_size(loaded_prefs.window_width, loaded_prefs.window_height);

    let widgets = widgets_from_builder(&builder, window.clone());
    widgets.sidebar_open.set(loaded_prefs.sidebar_open);
    *widgets.prefs.borrow_mut() = loaded_prefs;
    widgets.persist_prefs.set(!prefs::is_persistence_disabled());
    if let Some(cfg) = preview_cfg.as_ref() {
        apply_preview_config(cfg, &widgets);
    }

    let about = gio::SimpleAction::new("about", None);
    {
        let window = window.clone();
        about.connect_activate(move |_, _| dialogs::present_about(&window));
    }
    window.add_action(&about);

    let toggle_sidebar_action = gio::SimpleAction::new("toggle-sidebar", None);
    {
        let widgets = widgets.clone();
        toggle_sidebar_action.connect_activate(move |_, _| toggle_sidebar(&widgets));
    }
    window.add_action(&toggle_sidebar_action);
    app.set_accels_for_action("win.toggle-sidebar", &["F9", "<Primary>backslash"]);

    let focus_search = gio::SimpleAction::new("focus-search", None);
    {
        let state = state.clone();
        let widgets = widgets.clone();
        focus_search.connect_activate(move |_, _| focus_active_search(&state.borrow(), &widgets));
    }
    window.add_action(&focus_search);
    app.set_accels_for_action("win.focus-search", &["<Primary>f"]);

    wire_breakpoints(&widgets, &builder);
    wire_observatory_views(&widgets, &state, &console);

    let idle_btn = widgets.idle_btn.clone();
    let header_refresh = widgets.header_refresh.clone();
    let ready_update = widgets.ready_update.clone();
    let auth_cancel = widgets.auth_cancel.clone();
    let done_btn = widgets.done_btn.clone();
    let done_retry_btn = widgets.done_retry_btn.clone();
    let fail_retry = widgets.fail_retry.clone();

    wire_console(&widgets, &console);
    wire_expander(&widgets);

    let (tx, rx) = async_channel::unbounded::<WorkerMsg>();

    let refresh_action = gio::SimpleAction::new("refresh", None);
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        refresh_action.connect_activate(move |_, _| {
            let can_refresh = matches!(
                state.borrow().phase,
                Phase::Ready { .. }
                    | Phase::Idle(IdleScreen::Current { .. } | IdleScreen::Warning { .. })
            );
            if can_refresh {
                start_check(&state, &console, &widgets, tx.clone(), false);
            }
        });
    }
    window.add_action(&refresh_action);
    app.set_accels_for_action("win.refresh", &["<Primary>r", "F5"]);

    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        idle_btn.connect_clicked(move |_| {
            start_check(&state, &console, &widgets, tx.clone(), false);
        });
    }
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        header_refresh.connect_clicked(move |_| {
            start_check(&state, &console, &widgets, tx.clone(), false);
        });
    }
    wire_upgrade_buttons(&widgets, &state);
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        fail_retry.connect_clicked(move |_| {
            let retry = match &state.borrow().phase {
                Phase::Failed { retry, .. } => *retry,
                _ => return,
            };
            match retry {
                RetryAction::Reboot => {
                    widgets.toast.add_toast(Toast::new("Rebooting…"));
                    run_systemctl_reboot(tx.clone());
                }
                RetryAction::RetryApply => {
                    begin_apply(&state, &console, &widgets, tx.clone());
                }
                RetryAction::CheckAgain => {
                    start_check(&state, &console, &widgets, tx.clone(), false);
                }
            }
        });
    }
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        done_retry_btn.connect_clicked(move |_| {
            start_check(&state, &console, &widgets, tx.clone(), false);
        });
    }
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        ready_update.connect_clicked(move |_| {
            trigger_apply_from_ready(&state, &console, &widgets, tx.clone());
        });
    }
    {
        let state = state.clone();
        let console = console.clone();
        let w_cb = widgets.clone();
        let tx = tx.clone();
        widgets.ready_split_update.connect_clicked(move |_| {
            trigger_apply_from_ready(&state, &console, &w_cb, tx.clone());
        });
    }
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let run_stop = widgets.run_stop.clone();
        run_stop.connect_clicked(move |_| {
            let applying = matches!(
                &state.borrow().phase,
                Phase::Running(running) if running.is_applying()
            );
            let running = matches!(state.borrow().phase, Phase::Running(_));
            if applying {
                let state = state.clone();
                let console = console.clone();
                let widgets_for_cb = widgets.clone();
                dialogs::prompt_stop_during_transaction(&widgets.window, move || {
                    let borrowed = state.borrow();
                    let Phase::Running(running) = &borrowed.phase else {
                        return;
                    };
                    let during = running.is_applying();
                    drop(borrowed);
                    stop_apply(&state, &console, &widgets_for_cb, during);
                });
            } else if running {
                stop_apply(&state, &console, &widgets, false);
            }
        });
    }
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        auth_cancel.connect_clicked(move |_| {
            cancel_background_work();
            clear_console(&console, &widgets);
            dispatch(&state, &console, &widgets, Event::CancelAuth, true);
        });
    }
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        done_btn.connect_clicked(move |_| {
            let clean_done = matches!(state.borrow().phase, Phase::Done { failed: 0, .. });
            if clean_done {
                clear_console(&console, &widgets);
            }
            dispatch(&state, &console, &widgets, Event::ResetToIdle, true);
        });
    }
    {
        let window = widgets.window.clone();
        widgets.done_problem_banner.connect_button_clicked(move |_| {
            let _ = WidgetExt::activate_action(&window, "win.view-log", None);
        });
    }
    {
        let window = widgets.window.clone();
        widgets.ready_banner.connect_button_clicked(move |_| {
            let _ = WidgetExt::activate_action(&window, "win.view-log", None);
        });
    }
    {
        let window = widgets.window.clone();
        widgets.run_stall.connect_button_clicked(move |_| {
            let _ = WidgetExt::activate_action(&window, "win.view-log", None);
        });
    }
    {
        let tx = tx.clone();
        let widgets = widgets.clone();
        let reboot_btn = widgets.done_reboot_btn.clone();
        reboot_btn.connect_clicked(move |_| {
            widgets.toast.add_toast(Toast::new("Rebooting…"));
            run_systemctl_reboot(tx.clone());
        });
    }

    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                handle_worker(&state, &console, &widgets, &tx, ev);
            }
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        glib::timeout_add_local(Duration::from_secs(1), move || {
            refresh_quiet_chrome(&state, &widgets);
            glib::ControlFlow::Continue
        });
    }

    {
        let widgets = widgets.clone();
        window.connect_default_width_notify(move |_| schedule_prefs_save(&widgets));
    }
    {
        let widgets = widgets.clone();
        window.connect_default_height_notify(move |_| schedule_prefs_save(&widgets));
    }
    {
        let widgets = widgets.clone();
        window.connect_is_active_notify(move |win| {
            if win.is_active() {
                withdraw_completion_notification(&widgets);
            }
        });
    }

    {
        let widgets = widgets.clone();
        window.connect_close_request(move |_| {
            flush_prefs_on_close(&widgets);
            cancel_background_work();
            release_privileges();
            glib::Propagation::Proceed
        });
    }

    {
        let hooked = widgets.clone();
        let idle_btn = hooked.idle_btn.clone();
        idle_btn.connect_map(move |btn| {
            if hooked.focus_idle_check.get() {
                btn.grab_focus();
            }
        });
    }

    sync_shell_layout(&widgets);
    render(&state.borrow(), &console.borrow(), &widgets);
    if !is_preview {
        spawn_release_lookup(&state, &console, &widgets);
        spawn_native_capability_probe(&state, &console, &widgets);
    }
    {
        let hooked = widgets.clone();
        glib::idle_add_local_once(move || {
            if hooked.focus_idle_check.get() {
                hooked.idle_btn.grab_focus();
            }
        });
    }
    window.present();
    preview::maybe_schedule_screenshot(&window);
}

fn wire_breakpoints(widgets: &Rc<Widgets>, builder: &Builder) {
    let compact_bp: Breakpoint = obj(builder, "compact_breakpoint");
    {
        let widgets = widgets.clone();
        compact_bp.connect_apply(move |_| {
            widgets.compact.set(true);
            sync_shell_layout(&widgets);
        });
    }
    {
        let widgets = widgets.clone();
        compact_bp.connect_unapply(move |_| {
            widgets.compact.set(false);
            sync_shell_layout(&widgets);
        });
    }

    let narrow_bp: Breakpoint = obj(builder, "narrow_breakpoint");
    {
        let widgets = widgets.clone();
        narrow_bp.connect_apply(move |_| {
            widgets.narrow.set(true);
            apply_header_subtitle(&widgets);
            sync_shell_layout(&widgets);
        });
    }
    {
        let widgets = widgets.clone();
        narrow_bp.connect_unapply(move |_| {
            widgets.narrow.set(false);
            apply_header_subtitle(&widgets);
            sync_shell_layout(&widgets);
        });
    }
}

fn focus_active_search(state: &AppState, w: &Widgets) {
    match w.detail_view.get() {
        DetailView::ActivityLog => {
            w.activity_log.search_entry().grab_focus();
        }
        DetailView::History => {
            w.history_view.search_entry().grab_focus();
        }
        DetailView::Updates => {
            if matches!(&state.phase, Phase::Ready { packages, .. } if !packages.is_empty()) {
                w.ready_search.grab_focus();
            }
        }
    }
}

fn close_inspector(w: &Widgets) {
    *w.inspected_id.borrow_mut() = None;
    if w.inspector_split.shows_sidebar() {
        w.inspector_split.set_show_sidebar(false);
    }
    for queue in [&w.check_queue, &w.ready_queue, &w.run_queue, &w.done_queue] {
        queue.clear_selection();
    }
}

fn open_or_toggle_inspector(w: &Widgets, pkg: Package) {
    if w.inspected_id.borrow().as_deref() == Some(pkg.id.as_str())
        && w.inspector_split.shows_sidebar()
    {
        close_inspector(w);
        return;
    }
    *w.inspected_id.borrow_mut() = Some(pkg.id.clone());
    w.inspector.paint(&pkg);
    w.inspector_split.set_show_sidebar(true);
    for queue in [&w.check_queue, &w.ready_queue, &w.run_queue, &w.done_queue] {
        queue.select_by_id(&pkg.id);
    }
}

fn sync_inspector_from_packages(w: &Widgets, packages: &[Package], active_queue: &queue_list::QueueList) {
    let inspected = w.inspected_id.borrow().clone();
    let Some(id) = inspected else {
        return;
    };
    if let Some(pkg) = packages.iter().find(|p| p.id == id) {
        w.inspector.paint(pkg);
        active_queue.select_by_id(&id);
    } else {
        close_inspector(w);
    }
}

fn apply_source_filter(w: &Widgets, filter: sidebar::SourceFilter, package_count: usize) {
    w.source_filter.set(filter);
    w.sidebar.select_source(filter);
    for queue in [&w.check_queue, &w.ready_queue, &w.run_queue, &w.done_queue] {
        queue.set_source_filter(filter);
    }
    update_ready_filter_chrome(w, package_count);
}

fn reset_filters(w: &Widgets, package_count: usize) {
    if !w.ready_search.text().is_empty() {
        w.ready_search.set_text("");
    }
    for queue in [&w.check_queue, &w.ready_queue, &w.run_queue, &w.done_queue] {
        queue.set_query("");
    }
    apply_source_filter(w, sidebar::SourceFilter::All, package_count);
}

fn update_ready_filter_chrome(w: &Widgets, total_packages: usize) {
    let has_packages = total_packages > 0;
    w.ready_filter_clamp.set_visible(has_packages);
    w.ready_list_stack.set_visible(has_packages);
    if !has_packages {
        w.ready_filter_status.set_visible(false);
        w.ready_clear_filter.set_visible(false);
        return;
    }

    let matched = w.ready_queue.filtered_count() as usize;
    let query_active = !w.ready_search.text().trim().is_empty();
    let source_active = w.source_filter.get() != sidebar::SourceFilter::All;
    let filtered = query_active || source_active;
    if filtered {
        w.ready_filter_status
            .set_label(&format!("{matched} of {total_packages}"));
        w.ready_filter_status.set_visible(true);
        w.ready_clear_filter.set_visible(true);
    } else {
        w.ready_filter_status.set_visible(false);
        w.ready_clear_filter.set_visible(false);
    }

    w.ready_list_stack.set_visible_child_name(if matched > 0 {
        "list"
    } else {
        "empty-filter"
    });
}

fn update_ready_selection_chrome(w: &Widgets) {
    if !w.native_dnf_available.get() {
        w.ready_selection_bar.set_visible(false);
        w.ready_split_update.set_visible(false);
        w.ready_update.set_visible(true);
        return;
    }
    let (selected, total, bytes) = w.ready_queue.selection_stats();
    let has_packages = total > 0;
    w.ready_selection_bar.set_visible(has_packages);
    w.ready_update.set_visible(false);
    w.ready_split_update.set_visible(true);
    w.ready_split_update.set_sensitive(selected > 0);
    w.ready_split_update
        .set_label(&apply_cta_label(w.apply_mode.get(), selected, total, bytes));
    w.ready_selection_summary
        .set_text(&selection_summary_text(selected, total, bytes));

    w.ignore_select_all.set(true);
    w.ready_select_all
        .set_inconsistent(selected > 0 && selected < total);
    w.ready_select_all
        .set_active(total > 0 && selected == total);
    w.ignore_select_all.set(false);
}

fn paint_activity_log_chrome(w: &Widgets, matched: usize, total: usize) {
    let sub = activity_log_subtitle(matched, total);
    w.detail_toolbar.set_top_bar_style(ToolbarStyle::Raised);
    w.detail_toolbar.set_bottom_bar_style(ToolbarStyle::Flat);
    w.back_to_updates_btn.set_visible(true);
    w.header_refresh.set_visible(false);
    w.run_stop.set_visible(false);
    sync_console_drawer(w, false);
    paint_header(w, HeaderView::ActivityLog { subtitle: &sub });
}

fn paint_history_chrome(w: &Widgets, matched: usize, total: usize) {
    let sub = history_subtitle(matched, total);
    w.detail_toolbar.set_top_bar_style(ToolbarStyle::Raised);
    w.detail_toolbar.set_bottom_bar_style(ToolbarStyle::Flat);
    w.back_to_updates_btn.set_visible(true);
    w.header_refresh.set_visible(false);
    w.run_stop.set_visible(false);
    sync_console_drawer(w, false);
    paint_header(w, HeaderView::History { subtitle: &sub });
}

fn spawn_history_load(widgets: &Rc<Widgets>) {
    if cfg!(test) || preview::initial_config().is_some() {
        return;
    }
    widgets.history_view.set_loading(true);
    paint_history_chrome(widgets, 0, 0);
    let (tx, rx) = async_channel::bounded::<Vec<dnf_dbus::HistoryEntry>>(1);
    std::thread::spawn(move || {
        let entries = load_history_system(50);
        let _ = tx.send_blocking(entries);
    });
    let widgets = widgets.clone();
    glib::spawn_future_local(async move {
        if let Ok(entries) = rx.recv().await {
            widgets.history_loaded.set(true);
            let (matched, total) = widgets.history_view.set_entries(entries);
            if widgets.detail_view.get() == DetailView::History {
                paint_history_chrome(&widgets, matched, total);
            }
        }
    });
}

fn spawn_native_capability_probe(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
) {
    let (tx, rx) = async_channel::bounded::<(bool, bool)>(1);
    std::thread::spawn(move || {
        let dnf = dnf_dbus::is_available();
        let fwupd = fwupd_dbus::is_available();
        let _ = tx.send_blocking((dnf, fwupd));
    });
    let state = state.clone();
    let console = console.clone();
    let widgets = widgets.clone();
    glib::spawn_future_local(async move {
        if let Ok((dnf, fwupd)) = rx.recv().await {
            widgets.native_dnf_available.set(dnf);
            widgets.native_fwupd_available.set(fwupd);
            widgets.sidebar.set_history_available(dnf || fwupd);
            render(&state.borrow(), &console.borrow(), &widgets);
        }
    });
}

fn switch_detail_view(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    w: &Widgets,
    view: DetailView,
) {
    w.detail_view.set(view);
    w.sidebar.select_view(view);
    match view {
        DetailView::ActivityLog => {
            close_inspector(w);
            w.detail_view_stack.set_visible_child_name("activity-log");
            let (matched, total) = w.activity_log.sync_full(&console.borrow());
            paint_activity_log_chrome(w, matched, total);
        }
        DetailView::History => {
            close_inspector(w);
            w.detail_view_stack.set_visible_child_name("history");
            let (matched, total) = w.history_view.counts();
            paint_history_chrome(w, matched, total);
        }
        DetailView::Updates => {
            w.detail_view_stack.set_visible_child_name("updates");
            render(&state.borrow(), &console.borrow(), w);
        }
    }
}

fn wire_shell_controls(widgets: &Rc<Widgets>) {
    {
        let sidebar_toggle = widgets.sidebar_toggle.clone();
        let widgets = widgets.clone();
        sidebar_toggle.connect_toggled(move |btn| {
            if widgets.ignore_sidebar_toggle.get() {
                return;
            }
            if is_compact_viewport(&widgets) {
                widgets.split_view.set_show_content(!btn.is_active());
                sync_shell_layout(&widgets);
            } else {
                widgets.sidebar_open.set(btn.is_active());
                sync_shell_layout(&widgets);
                schedule_prefs_save(&widgets);
            }
        });
    }
    {
        let sidebar_close_btn = widgets.sidebar_close_btn.clone();
        let widgets = widgets.clone();
        sidebar_close_btn.connect_clicked(move |_| {
            widgets.split_view.set_show_content(true);
            sync_shell_layout(&widgets);
        });
    }
    {
        let split_view = widgets.split_view.clone();
        let widgets = widgets.clone();
        split_view.connect_show_content_notify(move |_| {
            sync_shell_layout(&widgets);
        });
    }
    {
        let window = widgets.window.clone();
        let widgets = widgets.clone();
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                if widgets.console_sheet.is_open() {
                    widgets.console_sheet.set_open(false);
                    return glib::Propagation::Stop;
                }
                if widgets.inspector_split.shows_sidebar() {
                    close_inspector(&widgets);
                    return glib::Propagation::Stop;
                }
                if widgets.split_view.is_collapsed() && !widgets.split_view.shows_content() {
                    widgets.split_view.set_show_content(true);
                    sync_shell_layout(&widgets);
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        window.add_controller(key);
    }
}

fn register_view_actions(
    widgets: &Rc<Widgets>,
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
) {
    let view_log = gio::SimpleAction::new("view-log", None);
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        view_log.connect_activate(move |_, _| {
            switch_detail_view(&state, &console, &widgets, DetailView::ActivityLog);
        });
    }
    widgets.window.add_action(&view_log);

    let view_history = gio::SimpleAction::new("view-history", None);
    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        view_history.connect_activate(move |_, _| {
            switch_detail_view(&state, &console, &widgets, DetailView::History);
            if !widgets.history_loaded.get() {
                spawn_history_load(&widgets);
            }
        });
    }
    widgets.window.add_action(&view_history);

    let apply_mode_action = gio::SimpleAction::new("apply-mode", Some(glib::VariantTy::STRING));
    {
        let widgets = widgets.clone();
        apply_mode_action.connect_activate(move |_, param| {
            let Some(name) = param.and_then(|v| v.str()) else {
                return;
            };
            let mode = match name {
                "live-reboot" => ApplyMode::LiveAndReboot,
                "offline" => ApplyMode::OfflineOnReboot,
                "download-only" => ApplyMode::DownloadOnly,
                _ => ApplyMode::Live,
            };
            widgets.apply_mode.set(mode);
            update_ready_selection_chrome(&widgets);
        });
    }
    widgets.window.add_action(&apply_mode_action);
}

fn wire_observatory_views(
    widgets: &Rc<Widgets>,
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
) {
    register_view_actions(widgets, state, console);
    wire_shell_controls(widgets);

    let close_compact_drawer = |w: &Rc<Widgets>| {
        if w.split_view.is_collapsed() {
            w.split_view.set_show_content(true);
            sync_shell_layout(w);
        }
    };

    {
        let on_source = {
            let state = state.clone();
            let console = console.clone();
            let widgets = widgets.clone();
            move |filter: sidebar::SourceFilter| {
                let count = state.borrow().packages().len();
                apply_source_filter(&widgets, filter, count);
                if widgets.detail_view.get() != DetailView::Updates {
                    switch_detail_view(&state, &console, &widgets, DetailView::Updates);
                }
                close_compact_drawer(&widgets);
            }
        };
        let on_updates = {
            let state = state.clone();
            let console = console.clone();
            let widgets = widgets.clone();
            move || {
                close_compact_drawer(&widgets);
                switch_detail_view(&state, &console, &widgets, DetailView::Updates);
            }
        };
        let on_history = {
            let state = state.clone();
            let console = console.clone();
            let widgets = widgets.clone();
            move || {
                close_compact_drawer(&widgets);
                switch_detail_view(&state, &console, &widgets, DetailView::History);
                if !widgets.history_loaded.get() {
                    spawn_history_load(&widgets);
                }
            }
        };
        let on_activity_log = {
            let state = state.clone();
            let console = console.clone();
            let widgets = widgets.clone();
            move || {
                close_compact_drawer(&widgets);
                switch_detail_view(&state, &console, &widgets, DetailView::ActivityLog);
            }
        };
        widgets
            .sidebar
            .wire_navigation(on_source, on_updates, on_history, on_activity_log);
    }

    {
        let w_cb = widgets.clone();
        widgets.ready_queue.connect_selection_changed(move || {
            update_ready_selection_chrome(&w_cb);
        });
    }
    {
        let w_cb = widgets.clone();
        widgets.ready_select_all.connect_toggled(move |btn| {
            if w_cb.ignore_select_all.get() {
                return;
            }
            w_cb.ready_queue.set_all_selected(btn.is_active());
        });
    }

    {
        let state = state.clone();
        let console = console.clone();
        let widgets = widgets.clone();
        let back_btn = widgets.back_to_updates_btn.clone();
        back_btn.connect_clicked(move |_| {
            switch_detail_view(&state, &console, &widgets, DetailView::Updates);
        });
    }

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let search = widgets.ready_search.clone();
        search.connect_changed(move |entry| {
            widgets.ready_queue.set_query(&entry.text());
            let total = state.borrow().packages().len();
            update_ready_filter_chrome(&widgets, total);
        });
    }
    {
        let search = widgets.ready_search.clone();
        search.connect_stop_search(move |entry| {
            if !entry.text().is_empty() {
                entry.set_text("");
            }
        });
    }
    for btn in [
        widgets.ready_clear_filter.clone(),
        widgets.ready_empty_clear_btn.clone(),
    ] {
        let state = state.clone();
        let widgets = widgets.clone();
        btn.connect_clicked(move |_| {
            let total = state.borrow().packages().len();
            reset_filters(&widgets, total);
        });
    }

    for queue in [
        &widgets.check_queue,
        &widgets.ready_queue,
        &widgets.run_queue,
        &widgets.done_queue,
    ] {
        let widgets = widgets.clone();
        queue.connect_package_activated(move |pkg| {
            open_or_toggle_inspector(&widgets, pkg);
        });
    }

    {
        let w_cb = widgets.clone();
        widgets.inspector.connect_close(move || {
            close_inspector(&w_cb);
        });
    }
    {
        let inspector_split = widgets.inspector_split.clone();
        let w_cb = widgets.clone();
        inspector_split.connect_show_sidebar_notify(move |split| {
            if !split.shows_sidebar() && w_cb.inspected_id.borrow().is_some() {
                close_inspector(&w_cb);
            }
        });
    }

    {
        let w_cb = widgets.clone();
        widgets.activity_log.wire(console.clone(), move |matched, total| {
            if w_cb.detail_view.get() == DetailView::ActivityLog {
                paint_activity_log_chrome(&w_cb, matched, total);
            }
        });
    }
    {
        let w_refresh = widgets.clone();
        let w_filter = widgets.clone();
        widgets.history_view.wire(
            move || spawn_history_load(&w_refresh),
            move |matched, total| {
                if w_filter.detail_view.get() == DetailView::History {
                    paint_history_chrome(&w_filter, matched, total);
                }
            },
        );
    }
}

fn wire_upgrade_buttons(widgets: &Rc<Widgets>, state: &Rc<RefCell<AppState>>) {
    for card in [
        widgets.sidebar.upgrade_card(),
        &widgets.idle_upgrade,
        &widgets.ready_upgrade,
        &widgets.done_upgrade,
    ] {
        let state = state.clone();
        let window = widgets.window.clone();
        card.action().connect_clicked(move |_| {
            let Some(version) = state.borrow().release_upgrade else {
                return;
            };
            dialogs::present_upgrade_dialog(&window, version);
        });
    }
}

fn prepare_for_operation(
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Widgets,
) {
    *widgets.reboot_blocker_detail.borrow_mut() = None;
    close_inspector(widgets);
    reset_filters(widgets, 0);
    widgets.detail_view.set(DetailView::Updates);
    widgets.sidebar.select_view(DetailView::Updates);
    widgets.detail_view_stack.set_visible_child_name("updates");
    clear_console(console, widgets);
}

fn start_check(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    tx: async_channel::Sender<WorkerMsg>,
    then_apply: bool,
) {
    prepare_for_operation(console, widgets);
    dispatch(state, console, widgets, Event::StartCheck { then_apply }, true);
    if can_skip_check_auth_ui(widgets.native_dnf_available.get()) {
        dispatch(
            state,
            console,
            widgets,
            session_started_event(AuthPurpose::Check),
            true,
        );
    }
    run_check_all(tx);
}

fn trigger_apply_from_ready(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    tx: async_channel::Sender<WorkerMsg>,
) {
    if state.borrow().update_list_needs_refresh(SystemTime::now()) {
        let state = state.clone();
        let console = console.clone();
        let widgets_for_cb = widgets.clone();
        dialogs::prompt_stale_update_list(&widgets.window, move || {
            start_check(&state, &console, &widgets_for_cb, tx.clone(), true);
        });
        return;
    }
    if widgets.native_dnf_available.get() {
        let selected_ids = widgets.ready_queue.selected_ids();
        if selected_ids.is_empty() {
            return;
        }
        let mode = widgets.apply_mode.get();
        begin_apply_selected(state, console, widgets, tx, selected_ids, mode);
    } else {
        begin_apply(state, console, widgets, tx);
    }
}

fn begin_apply(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    tx: async_channel::Sender<WorkerMsg>,
) {
    let mode = widgets.apply_mode.get();
    if mode != ApplyMode::Live {
        let ids: Vec<String> = state
            .borrow()
            .packages()
            .iter()
            .map(|p| p.id.clone())
            .collect();
        if !ids.is_empty() {
            begin_apply_selected(state, console, widgets, tx, ids, mode);
            return;
        }
    }
    widgets.last_output.set(None);
    *widgets.cache_cursor.borrow_mut() = None;
    widgets.sidebar.reset_throughput();
    prepare_for_operation(console, widgets);
    let packages = state.borrow().packages().to_vec();
    dispatch(state, console, widgets, Event::StartApply, true);
    if can_skip_apply_auth_ui(widgets.native_dnf_available.get()) {
        dispatch(
            state,
            console,
            widgets,
            session_started_event(AuthPurpose::Apply),
            true,
        );
    }
    run_apply_all_with_mode(tx, packages, ApplyMode::Live);
}

fn begin_apply_selected(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    tx: async_channel::Sender<WorkerMsg>,
    selected_ids: Vec<String>,
    mode: ApplyMode,
) {
    widgets.last_output.set(None);
    *widgets.cache_cursor.borrow_mut() = None;
    widgets.sidebar.reset_throughput();
    prepare_for_operation(console, widgets);
    dispatch(
        state,
        console,
        widgets,
        Event::StartSelectiveApply { selected_ids, mode },
        true,
    );
    let packages = state.borrow().packages().to_vec();
    if can_skip_apply_auth_ui(widgets.native_dnf_available.get()) {
        dispatch(
            state,
            console,
            widgets,
            session_started_event(AuthPurpose::Apply),
            true,
        );
    }
    run_apply_all_with_mode(tx, packages, mode);
}

fn can_start_apply(state: &AppState) -> bool {
    matches!(&state.phase, Phase::Ready { packages, .. } if !packages.is_empty())
}

fn dispatch(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    event: Event,
    show_toast: bool,
) {
    let result = state::reduce(&mut state.borrow_mut(), event);
    match result {
        Ok(()) => render(&state.borrow(), &console.borrow(), widgets),
        Err(e) if show_toast => {
            eprintln!("state error: {e}");
            widgets
                .toast
                .add_toast(Toast::new(&format!("Internal error: {e}")));
        }
        Err(e) => {
            eprintln!("ignored worker event: {e}");
        }
    }
}

/// Stores `SessionReady` for a helper `SessionStarted`. Returns whether that
/// event was written. The caller must not hold a `Ref` on `state`: `borrow_mut`
/// panics instead of leaving the window on Authenticating.
fn commit_session_started(state: &Rc<RefCell<AppState>>) -> Result<bool, state::TransitionError> {
    let purpose = {
        let borrowed = state.borrow();
        session_started_purpose(&borrowed.phase)
    };
    let Some(purpose) = purpose else {
        return Ok(false);
    };
    state::reduce(&mut state.borrow_mut(), session_started_event(purpose))?;
    Ok(true)
}

fn maybe_send_completion_notification(
    w: &Widgets,
    notif: Option<DesktopNotification>,
) -> bool {
    let Some(notif) = notif else {
        return false;
    };
    if w.window.is_active() {
        return false;
    }
    if let Some(app) = w.window.application() {
        let g_notif = gio::Notification::new(&notif.title);
        g_notif.set_body(Some(&notif.body));
        app.send_notification(Some(notif.id), &g_notif);
    }
    w.notification_sent.set(true);
    true
}

fn withdraw_completion_notification(w: &Widgets) {
    if let Some(app) = w.window.application() {
        app.withdraw_notification(COMPLETION_NOTIFICATION_ID);
    }
    w.notification_sent.set(false);
}

fn spawn_reboot_preflight(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
) {
    if preview::initial_config().is_some() {
        return;
    }
    let (tx, rx) = async_channel::bounded::<Option<String>>(1);
    std::thread::spawn(move || {
        let detail = match fedora_updater::backend::logind::query_reboot_preflight() {
            fedora_updater::backend::NativeOutcome::Handled(Ok(pf)) => pf.blocking_detail(),
            _ => None,
        };
        let _ = tx.send_blocking(detail);
    });
    let state = state.clone();
    let console = console.clone();
    let widgets = widgets.clone();
    glib::spawn_future_local(async move {
        if let Ok(detail) = rx.recv().await {
            *widgets.reboot_blocker_detail.borrow_mut() = detail;
            if matches!(
                state.borrow().phase,
                Phase::Done {
                    needs_reboot: true,
                    ..
                }
            ) {
                render(&state.borrow(), &console.borrow(), &widgets);
            }
        }
    });
}

fn handle_worker(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    tx: &async_channel::Sender<WorkerMsg>,
    msg: WorkerMsg,
) {
    handle_worker_with_reboot(
        state,
        console,
        widgets,
        msg,
        || begin_apply(state, console, widgets, tx.clone()),
        || spawn_release_lookup(state, console, widgets),
        || spawn_reboot_preflight(state, console, widgets),
        || {
            widgets
                .toast
                .add_toast(Toast::new("Updates installed. Rebooting…"));
            run_systemctl_reboot(tx.clone());
        },
    );
}

#[cfg(test)]
fn handle_worker_with(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    msg: WorkerMsg,
    on_auto_apply: impl FnOnce(),
    on_release_lookup: impl FnOnce(),
    on_reboot_preflight: impl FnOnce(),
) {
    handle_worker_with_reboot(
        state,
        console,
        widgets,
        msg,
        on_auto_apply,
        on_release_lookup,
        on_reboot_preflight,
        || {},
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_worker_with_reboot(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    msg: WorkerMsg,
    on_auto_apply: impl FnOnce(),
    on_release_lookup: impl FnOnce(),
    on_reboot_preflight: impl FnOnce(),
    on_auto_reboot: impl FnOnce(),
) {
    let ev = match msg {
        WorkerMsg::SessionStarted => {
            match commit_session_started(state) {
                Ok(true) => render(&state.borrow(), &console.borrow(), widgets),
                Ok(false) => {}
                Err(e) => eprintln!("ignored worker event: {e}"),
            }
            return;
        }
        WorkerMsg::ConsoleLine { tag, text } => {
            record_console_output(console, widgets, &tag, &text, false);
            return;
        }
        WorkerMsg::ConsoleDraw { tag, text } => {
            record_console_output(console, widgets, &tag, &text, true);
            return;
        }
        WorkerMsg::Throughput {
            bytes_per_sec,
            downloaded_bytes,
            total_bytes,
        } => {
            widgets
                .sidebar
                .push_throughput_sample(bytes_per_sec, Some((downloaded_bytes, total_bytes)));
            return;
        }
        WorkerMsg::Event(ev) => ev,
    };

    let was_running = matches!(state.borrow().phase, Phase::Running(_));
    let check_finished = matches!(ev, Event::CheckComplete { .. });
    let apply_finished =
        matches!(ev, Event::ApplyComplete { .. }) || (was_running && matches!(ev, Event::Fail { .. }));
    if matches!(ev, Event::ApplyProgress { .. }) {
        widgets.last_output.set(Some(Instant::now()));
    }

    dispatch(state, console, widgets, ev, false);

    let apply_now = if check_finished {
        let borrowed = state.borrow();
        borrowed.apply_when_ready && can_start_apply(&borrowed)
    } else {
        false
    };
    if apply_now {
        on_auto_apply();
    } else {
        let notif = {
            let borrowed = state.borrow();
            phase_completion_notification(&borrowed.phase, check_finished, apply_finished)
        };
        maybe_send_completion_notification(widgets, notif);
    }
    if check_finished {
        on_release_lookup();
    }
    let auto_reboot = apply_finished
        && state.borrow().apply_mode == ApplyMode::LiveAndReboot
        && matches!(state.borrow().phase, Phase::Done { failed: 0, .. });
    if auto_reboot {
        on_auto_reboot();
    } else if apply_finished
        && matches!(
            state.borrow().phase,
            Phase::Done {
                needs_reboot: true,
                ..
            }
        )
    {
        on_reboot_preflight();
    }
}

fn clear_console(console: &Rc<RefCell<ConsoleBuffer>>, widgets: &Widgets) {
    console.borrow_mut().clear();
    widgets.console_full.buffer().set_text("");
    widgets.console_line.set_text("Starting…");
    widgets.activity_log.clear();
    if widgets.detail_view.get() == DetailView::ActivityLog {
        paint_activity_log_chrome(widgets, 0, 0);
    }
}

fn sync_console_drawer(w: &Widgets, show: bool) {
    if show {
        w.console_sheet.set_can_open(true);
        w.console_sheet.set_reveal_bottom_bar(true);
    } else {
        w.console_sheet.set_open(false);
        w.console_sheet.set_can_open(false);
        w.console_sheet.set_reveal_bottom_bar(false);
        w.console_chevron.set_icon_name(Some("pan-end-symbolic"));
    }
}

fn record_console_output(
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Widgets,
    tag: &str,
    text: &str,
    live: bool,
) {
    widgets.last_output.set(Some(Instant::now()));
    let (delta, last_line) = {
        let mut buf = console.borrow_mut();
        let delta = if live {
            buf.push_live_tagged(tag, text)
        } else {
            buf.push_committed_tagged(tag, text)
        };
        (delta, buf.last_line().to_string())
    };
    widgets.console_line.set_text(&last_line);
    if widgets.console_sheet.is_open() {
        activity_log::apply_text_buffer_delta(&widgets.console_full.buffer(), delta, &last_line);
    }
    if widgets.detail_view.get() == DetailView::ActivityLog {
        let (matched, total) =
            widgets
                .activity_log
                .apply_delta(&console.borrow(), delta, &last_line);
        paint_activity_log_chrome(widgets, matched, total);
    }
}

fn wire_expander(widgets: &Rc<Widgets>) {
    let widgets = widgets.clone();
    let expander = widgets.run_completed.clone();
    expander.connect_expanded_notify(move |expander| {
        if widgets.ignore_expand.get() {
            return;
        }
        widgets.run_completed_user_open.set(expander.is_expanded());
    });
}

fn wire_console(widgets: &Rc<Widgets>, console: &Rc<RefCell<ConsoleBuffer>>) {
    {
        let sheet = widgets.console_sheet.clone();
        widgets.console_toggle.connect_clicked(move |_| {
            sheet.set_open(!sheet.is_open());
        });
    }
    {
        let sheet = widgets.console_sheet.clone();
        widgets.console_close_btn.connect_clicked(move |_| {
            sheet.set_open(false);
        });
    }
    {
        let full = widgets.console_full.clone();
        widgets.console_copy_btn.connect_clicked(move |btn| {
            let buf = full.buffer();
            let text = buf.text(&buf.start_iter(), &buf.end_iter(), false);
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&text);
            }
            btn.set_label("Copied");
            let btn = btn.clone();
            glib::timeout_add_local_once(Duration::from_millis(1600), move || {
                btn.set_label("Copy");
            });
        });
    }
    {
        let chevron = widgets.console_chevron.clone();
        let full = widgets.console_full.clone();
        let console = console.clone();
        widgets.console_sheet.connect_open_notify(move |sheet| {
            let open = sheet.is_open();
            if open {
                let text = console.borrow().full_text();
                full.buffer().set_text(&text);
            }
            chevron.set_icon_name(Some(if open {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            }));
        });
    }
}

fn paint_header(w: &Widgets, view: HeaderView<'_>) {
    let full = window_header(view, false);
    *w.header_subtitle.borrow_mut() = full.subtitle;
    w.window_title.set_title(&full.title);
    apply_header_subtitle(w);
}

fn apply_header_subtitle(w: &Widgets) {
    let subtitle = if w.narrow.get() {
        String::new()
    } else {
        w.header_subtitle.borrow().clone()
    };
    w.window_title.set_subtitle(&subtitle);
}

fn paint_stop_button(button: &Button, pass: RunPass) {
    if pass == RunPass::Apply {
        button.set_label("Stop…");
        button.add_css_class("destructive-action");
    } else {
        button.set_label("Stop");
        button.remove_css_class("destructive-action");
    }
}

fn paint_source_badge(label: &Label, package: Option<&Package>) {
    label.remove_css_class("package-badge");
    label.remove_css_class("src-dnf");
    label.remove_css_class("src-flatpak");
    label.remove_css_class("src-firmware");
    if let Some(package) = package {
        label.set_label(package.source.badge());
        label.add_css_class("package-badge");
        label.add_css_class(package.source.css_class());
        label.set_visible(true);
    } else {
        label.set_visible(false);
    }
}

fn current_package<'a>(
    packages: &'a [Package],
    active_index: Option<usize>,
    current_name: Option<&str>,
) -> Option<&'a Package> {
    if let Some(package) = active_index.and_then(|index| packages.get(index)) {
        if !package.status.is_done() {
            return Some(package);
        }
    }
    let name = current_name
        .map(str::trim)
        .filter(|name| !name.is_empty())?;
    packages
        .iter()
        .find(|package| package.name == name && !package.status.is_done())
}

fn finished_row(package: &Package) -> libadwaita::ActionRow {
    rows::finished_row(package, true)
}

fn clear_running_lists(w: &Widgets) {
    w.sidebar.reset_throughput();
    if w.run_queue.total_count() == 0 && w.run_completed_rows.borrow().is_empty() {
        return;
    }
    w.run_queue.sync(&[], None, None);
    w.run_list_slot.set_visible(true);
    w.run_list_scroll.set_visible(true);
    w.ignore_expand.set(true);
    rows::refill_expander(&w.run_completed, &w.run_completed_rows, std::iter::empty());
    w.run_completed_keys.borrow_mut().clear();
    w.run_completed_user_open.set(false);
    w.run_completed.set_expanded(false);
    w.run_completed_group.set_visible(false);
    w.ignore_expand.set(false);
}

fn quiet_secs(w: &Widgets) -> u64 {
    match w.last_output.get() {
        Some(at) => at.elapsed().as_secs(),
        None => {
            w.last_output.set(Some(Instant::now()));
            0
        }
    }
}

fn console_preview_line(last_line: &str) -> &str {
    if last_line.is_empty() {
        "Starting…"
    } else {
        last_line
    }
}

fn paint_stage_breadcrumbs(w: &Widgets, active: PipelineStage) {
    let crumbs = [
        (&w.stage_download, PipelineStage::Download),
        (&w.stage_prepare, PipelineStage::Prepare),
        (&w.stage_install, PipelineStage::Install),
        (&w.stage_verify, PipelineStage::Verify),
    ];
    for (label, stage) in crumbs {
        label.remove_css_class("done");
        label.remove_css_class("active");
        if stage < active {
            label.add_css_class("done");
            label.set_text(stage.done_label());
        } else if stage == active {
            label.add_css_class("active");
            label.set_text(stage.label());
        } else {
            label.set_text(stage.label());
        }
    }
}

fn paint_now_playing_status(
    w: &Widgets,
    show_spinner: bool,
    running: &RunningState,
    playing: Option<&Package>,
) {
    for cls in ["accent", "success", "error"] {
        w.now_playing_status_icon.remove_css_class(cls);
    }
    let active_failed = running
        .active_index
        .and_then(|i| running.packages.get(i))
        .is_some_and(|p| p.status == PackageStatus::Failed);
    if active_failed || playing.is_some_and(|p| p.status == PackageStatus::Failed) {
        w.now_playing_status_icon
            .set_icon_name(Some("dialog-error-symbolic"));
        w.now_playing_status_icon.add_css_class("error");
        w.now_playing_status_stack.set_visible_child_name("status");
    } else if show_spinner || running.work_progress.is_none() {
        w.now_playing_status_stack.set_visible_child_name("spinner");
    } else if running.work_progress.is_some_and(|frac| frac >= 0.999) {
        w.now_playing_status_icon
            .set_icon_name(Some("emblem-ok-symbolic"));
        w.now_playing_status_icon.add_css_class("success");
        w.now_playing_status_stack.set_visible_child_name("status");
    } else if running.pass == RunPass::Download {
        w.now_playing_status_icon
            .set_icon_name(Some("folder-download-symbolic"));
        w.now_playing_status_icon.add_css_class("accent");
        w.now_playing_status_stack.set_visible_child_name("status");
    } else {
        w.now_playing_status_icon
            .set_icon_name(Some("software-update-available-symbolic"));
        w.now_playing_status_icon.add_css_class("accent");
        w.now_playing_status_stack.set_visible_child_name("status");
    }
}

/// Footnote, spinner, sidebar telemetry, and stall banner for a quiet run.
fn paint_quiet_chrome(running: &RunningState, caption: &str, elapsed: u64, w: &Widgets) {
    let secs = quiet_secs(w);
    let watching_dnf = running.current_source == Some(UpdateSource::Dnf);
    let cache_note = if secs >= 5 && watching_dnf && !running.is_applying() {
        dnf::poll_cache_growth(
            std::path::Path::new(dnf::DNF_CACHE_ROOT),
            &mut w.cache_cursor.borrow_mut(),
        )
        .map(|growth| cache_activity_note(&growth.package, growth.bytes, growth.rising))
    } else {
        None
    };
    paint_quiet_chrome_with(
        running,
        caption,
        elapsed,
        secs,
        w,
        cache_note.as_deref(),
        dnf_daemon_is_running(),
    );
}

fn paint_quiet_chrome_with(
    running: &RunningState,
    caption: &str,
    elapsed: u64,
    secs: u64,
    w: &Widgets,
    cache_note: Option<&str>,
    daemon_running: bool,
) {
    let at_full = running.work_progress.is_some_and(|frac| frac >= 0.999);
    let watching_dnf = running.current_source == Some(UpdateSource::Dnf);
    let quiet = quiet_ui(QuietInput {
        quiet_secs: secs,
        at_full_item: at_full,
        cache_note,
        watching_dnf,
        daemon_running,
    });

    let mut foot = caption.to_string();
    if quiet.show_eta {
        if let Some(eta) = about_time_left(elapsed, running.overall_progress) {
            foot = format!("{eta} · {foot}");
        }
    }
    if let Some(note) = &quiet.footnote {
        foot = note.clone();
    }
    if w.detail_view.get() == DetailView::Updates {
        paint_header(
            w,
            HeaderView::Running {
                heading: &w.window_title.title(),
                footnote: &foot,
            },
        );
    }

    let playing = current_package(
        &running.packages,
        running.active_index,
        running.current_name.as_deref(),
    );
    paint_now_playing_status(w, quiet.show_spinner, running, playing);
    w.sidebar
        .paint_running_telemetry(elapsed, &quiet, running.overall_progress);

    if let Some(text) = quiet.banner.as_deref() {
        w.run_stall.set_title(text);
        w.run_stall.set_revealed(true);
    } else {
        w.run_stall.set_revealed(false);
    }
}

fn refresh_quiet_chrome(state: &Rc<RefCell<AppState>>, w: &Rc<Widgets>) {
    let s = state.borrow();
    let Phase::Running(running) = &s.phase else {
        w.run_stall.set_revealed(false);
        return;
    };
    let elapsed = s
        .stopwatch
        .as_ref()
        .map(|sw| sw.elapsed_secs())
        .unwrap_or(0);
    let (done, total) = running.done_and_total();
    let (caption, _) = running_progress_captions(running.current_source, done, total, running.pass);
    paint_quiet_chrome(running, &caption, elapsed, w);
}

fn stop_apply(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    during_transaction: bool,
) {
    cancel_background_work();
    kill_recorded_helper();
    kill_recorded_local_command();
    dispatch(
        state,
        console,
        widgets,
        Event::StopApply { during_transaction },
        false,
    );
}

fn render_running(running: &RunningState, elapsed: u64, last_console: &str, w: &Widgets) {
    w.stack.set_visible_child_name("running");
    w.run_stop.set_visible(true);
    paint_stop_button(&w.run_stop, running.pass);
    w.window_title
        .set_title(run_heading(&running.phase_label, running.pass));
    w.console_line.set_text(console_preview_line(last_console));
    paint_stage_breadcrumbs(w, pipeline_stage(&running.phase_label, running.pass));
    let (done, total) = running.done_and_total();
    let (caption, meta) =
        running_progress_captions(running.current_source, done, total, running.pass);
    w.run_progress.set_fraction(running.overall_progress);
    w.run_progress_meta.set_text(&meta);
    w.run_progress_pct
        .set_text(&format!("{:.0}%", running.overall_progress * 100.0));

    let packages = &running.packages;
    let current_name = running.current_name.as_deref();
    let title = now_playing_title(
        packages,
        running.active_index,
        current_name,
        &running.phase_label,
    );
    let subtitle = now_playing_subtitle(&title, &running.phase_label, running.current_source);
    w.now_playing_title.set_text(&title);
    w.now_playing_subtitle.set_text(if subtitle.is_empty() {
        "Working"
    } else {
        &subtitle
    });
    w.now_playing_subtitle.set_visible(true);
    let playing = current_package(packages, running.active_index, current_name);
    paint_source_badge(&w.now_playing_badge, playing);
    if let Some(size) = playing.and_then(Package::size_label) {
        w.now_playing_size.set_text(&size);
        w.now_playing_size.set_visible(true);
    } else {
        w.now_playing_size.set_visible(false);
    }
    w.progress_ring.set_progress(running.work_progress, false);

    let remaining: Vec<Package> = packages
        .iter()
        .filter(|p| !p.status.is_done())
        .cloned()
        .collect();
    let active_id = running
        .active_index
        .and_then(|i| packages.get(i))
        .filter(|cur| !cur.status.is_done())
        .map(|cur| cur.id.as_str());
    w.run_queue.sync(&remaining, active_id, running.work_progress);
    w.run_list_slot.set_visible(!remaining.is_empty());

    let completed: Vec<&Package> = packages.iter().filter(|p| p.status.is_done()).collect();
    let completed_keys: Vec<String> = completed.iter().map(|p| p.id.clone()).collect();
    w.ignore_expand.set(true);
    let failed = completed
        .iter()
        .filter(|p| p.status == PackageStatus::Failed)
        .count();
    w.run_completed.set_title(&if failed > 0 {
        format!("{} finished · {failed} failed", completed.len())
    } else {
        format!("{} completed", completed.len())
    });
    if completed_keys != *w.run_completed_keys.borrow() {
        let prev = w.run_completed_keys.borrow().clone();
        if !prev.is_empty() && completed_keys.starts_with(&prev) {
            rows::append_expander(
                &w.run_completed,
                &w.run_completed_rows,
                completed[prev.len()..].iter().copied().map(finished_row),
            );
        } else {
            rows::refill_expander(
                &w.run_completed,
                &w.run_completed_rows,
                completed.iter().copied().map(finished_row),
            );
        }
        *w.run_completed_keys.borrow_mut() = completed_keys;
    }
    let has_completed = !completed.is_empty();
    w.run_completed.set_enable_expansion(has_completed);
    w.run_completed
        .set_expanded(w.run_completed_user_open.get() && has_completed);
    w.run_completed_group.set_visible(has_completed);
    w.run_list_scroll
        .set_visible(!remaining.is_empty() || has_completed);
    w.ignore_expand.set(false);

    sync_inspector_from_packages(w, packages, &w.run_queue);
    paint_quiet_chrome(running, &caption, elapsed, w);
}

fn arm_idle_check_focus(w: &Widgets) {
    w.focus_idle_check.set(true);
    w.window.set_default_widget(Some(&w.idle_btn));
    w.idle_btn.grab_focus();
}

fn clear_idle_check_focus(w: &Widgets) {
    w.focus_idle_check.set(false);
    if w.window.default_widget().as_ref() == Some(w.idle_btn.upcast_ref()) {
        w.window.set_default_widget(None::<&gtk::Widget>);
    }
}

fn sync_release_upgrade_cards(w: &Widgets) {
    let version = w.release_upgrade_version.get();
    let days = w.release_upgrade_days.get();
    let collapsed = effective_sidebar_collapsed(w);
    let sidebar_visible = version.is_some() && w.sidebar_upgrade_allowed.get() && !collapsed;
    w.sidebar.paint_upgrade(version, days, sidebar_visible);

    let slot = if version.is_some() && collapsed {
        w.inline_upgrade_slot.get()
    } else {
        sidebar::InlineUpgradeSlot::None
    };

    w.idle_upgrade
        .paint(version, days, slot == sidebar::InlineUpgradeSlot::Idle);
    w.idle_upgrade_slot
        .set_visible(w.idle_upgrade.root().get_visible());

    w.ready_upgrade
        .paint(version, days, slot == sidebar::InlineUpgradeSlot::Ready);
    w.ready_upgrade_slot
        .set_visible(w.ready_upgrade.root().get_visible());

    w.done_upgrade
        .paint(version, days, slot == sidebar::InlineUpgradeSlot::Done);
    w.done_upgrade_slot
        .set_visible(w.done_upgrade.root().get_visible());
}

fn paint_release_upgrade(w: &Widgets, version: Option<u32>, days: Option<u32>) {
    w.release_upgrade_version.set(version);
    w.release_upgrade_days.set(days);
    sync_release_upgrade_cards(w);
}

fn toolbar_styles_for_phase(phase: &Phase) -> (ToolbarStyle, ToolbarStyle) {
    match phase {
        Phase::Idle(_) | Phase::Authenticating { .. } | Phase::Failed { .. } => {
            (ToolbarStyle::Flat, ToolbarStyle::Flat)
        }
        Phase::Checking { .. }
        | Phase::Ready { .. }
        | Phase::Running(_)
        | Phase::Done { .. } => (ToolbarStyle::Raised, ToolbarStyle::Raised),
    }
}

fn spawn_release_lookup(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
) {
    let (tx, rx) =
        async_channel::bounded::<Option<fedora_updater::release_upgrade::ReleaseOffer>>(1);
    std::thread::spawn(move || {
        let offer = fedora_updater::release_upgrade::lookup_installed();
        let _ = tx.send_blocking(offer);
    });
    let state = state.clone();
    let console = console.clone();
    let widgets = widgets.clone();
    glib::spawn_future_local(async move {
        if let Ok(offer) = rx.recv().await {
            dispatch(
                &state,
                &console,
                &widgets,
                Event::ReleaseNoted {
                    version: offer.as_ref().map(|offer| offer.version),
                    days_since_release: offer.and_then(|offer| offer.days_since_release),
                },
                true,
            );
        }
    });
}

fn render(state: &AppState, console: &ConsoleBuffer, w: &Widgets) {
    w.run_stall.set_revealed(false);
    clear_idle_check_focus(w);
    let (top_style, bottom_style) = toolbar_styles_for_phase(&state.phase);
    w.detail_toolbar.set_top_bar_style(top_style);
    w.detail_toolbar.set_bottom_bar_style(bottom_style);
    w.sidebar_upgrade_allowed
        .set(sidebar::release_upgrade_allowed_in_sidebar(&state.phase));
    w.inline_upgrade_slot
        .set(sidebar::release_upgrade_inline_slot(&state.phase));
    paint_release_upgrade(w, state.release_upgrade, state.release_age_days);
    w.sidebar.paint(state);
    sync_console_drawer(
        w,
        w.detail_view.get() == DetailView::Updates
            && matches!(state.phase, Phase::Checking { .. } | Phase::Running(_)),
    );
    if !matches!(state.phase, Phase::Running(_)) {
        w.progress_ring.set_progress(None, false);
    }
    if !matches!(
        state.phase,
        Phase::Done {
            needs_reboot: true,
            ..
        }
    ) {
        *w.reboot_blocker_detail.borrow_mut() = None;
    }
    let elapsed = state
        .stopwatch
        .as_ref()
        .map(|sw| sw.elapsed_secs())
        .unwrap_or(0);

    w.header_refresh
        .set_visible(matches!(state.phase, Phase::Ready { .. }));
    w.run_stop
        .set_visible(matches!(state.phase, Phase::Running(_)));

    match &state.phase {
        Phase::Idle(screen) => {
            clear_running_lists(w);
            close_inspector(w);
            w.stack
                .set_visible_child_full("idle", gtk::StackTransitionType::None);
            let chrome = idle_chrome(screen);
            w.idle_page.set_title(chrome.title);
            w.idle_page.set_icon_name(Some(chrome.icon));
            w.idle_btn.set_label(chrome.button);
            if chrome.suggested {
                w.idle_btn.add_css_class("suggested-action");
            } else {
                w.idle_btn.remove_css_class("suggested-action");
            }
            if chrome.pill {
                w.idle_btn.add_css_class("pill");
            } else {
                w.idle_btn.remove_css_class("pill");
            }
            if chrome.flat {
                w.idle_btn.add_css_class("flat");
            } else {
                w.idle_btn.remove_css_class("flat");
            }
            w.idle_page.set_description(chrome.description.as_deref());
            w.idle_log.set_visible(chrome.show_view_log);
            paint_header(w, HeaderView::Idle);
            if matches!(screen, IdleScreen::Fresh) && w.detail_view.get() == DetailView::Updates {
                arm_idle_check_focus(w);
            }
        }
        Phase::Authenticating { purpose, .. } => {
            close_inspector(w);
            w.stack.set_visible_child_name("auth");
            w.auth_page.set_title("Authentication Required");
            w.auth_page.set_description(Some(AUTH_DESCRIPTION));
            paint_header(w, HeaderView::Authenticating(*purpose));
        }
        Phase::Checking {
            packages_so_far,
            checked_sources,
            ..
        } => {
            w.stack.set_visible_child_name("checking");
            w.console_line
                .set_text(console_preview_line(console.last_line()));
            let (frac, groups_done, status) = check_progress(checked_sources);
            let eta = about_time_left(elapsed, frac);
            let foot = checking_footnote(groups_done, eta.as_deref());
            paint_header(w, HeaderView::Checking { footnote: &foot });
            w.check_progress.set_fraction(frac);
            w.check_progress_meta.set_text(status);
            w.check_progress_pct
                .set_text(&format!("{:.0}%", frac * 100.0));
            let found = !packages_so_far.is_empty();
            w.check_list_header_clamp.set_visible(found);
            w.check_list_scroll.set_visible(found);
            w.check_queue.sync(packages_so_far, None, None);
            sync_inspector_from_packages(w, packages_so_far, &w.check_queue);
        }
        Phase::Ready {
            packages,
            soft_errors,
        } => {
            w.stack.set_visible_child_name("ready");
            let summary = source_summary_line(packages);
            paint_header(
                w,
                HeaderView::Ready {
                    count: packages.len(),
                    summary: &summary,
                },
            );

            if let Some(title) = ready_banner_title(soft_errors) {
                w.ready_banner.set_title(&title);
                w.ready_banner.set_button_label(Some("View Log"));
                w.ready_banner.set_revealed(true);
            } else {
                w.ready_banner.set_revealed(false);
            }
            w.ready_update.set_sensitive(!packages.is_empty());
            w.ready_queue.set_selectable(w.native_dnf_available.get());
            w.ready_queue.sync(packages, None, None);
            update_ready_filter_chrome(w, packages.len());
            update_ready_selection_chrome(w);
            sync_inspector_from_packages(w, packages, &w.ready_queue);
        }
        Phase::Running(running) => {
            render_running(running, elapsed, console.last_line(), w);
        }
        Phase::Done {
            packages,
            duration,
            needs_reboot,
            failed,
            failure_reasons,
        } => {
            clear_running_lists(w);
            w.stack
                .set_visible_child_full("done", gtk::StackTransitionType::None);
            w.stack.queue_allocate();
            let upgraded = packages
                .iter()
                .filter(|p| p.status == PackageStatus::Completed)
                .count();
            let (dnf, fp, fw) = count_by_source(packages);
            let subtitle = done_header_subtitle(*failed, upgraded, dnf, fp, fw, duration);
            paint_header(
                w,
                HeaderView::Done {
                    failed: *failed > 0,
                    subtitle: &subtitle,
                },
            );
            if *failed > 0 {
                let msg = if failure_reasons.is_empty() {
                    if *failed == 1 {
                        "1 update failed".to_string()
                    } else {
                        format!("{failed} updates failed")
                    }
                } else {
                    failure_reasons.join(" · ")
                };
                w.done_problem_banner.set_title(&msg);
                w.done_problem_banner.set_revealed(true);
            } else {
                w.done_problem_banner.set_revealed(false);
            }
            w.done_summary_detail
                .set_text(&done_clean_summary(upgraded, duration));
            w.done_summary_clamp.set_visible(!*needs_reboot && *failed == 0);
            let base_reboot = reboot_trigger_summary(packages);
            let blocker = w.reboot_blocker_detail.borrow().clone();
            let reboot_text = match blocker {
                Some(b) if *needs_reboot => format!("{base_reboot}\n{b}"),
                _ => base_reboot,
            };
            w.done_reboot_detail.set_text(&reboot_text);
            w.done_reboot_clamp.set_visible(*needs_reboot);

            w.done_retry_btn.set_visible(*failed > 0);
            w.done_reboot_btn.remove_css_class("suggested-action");
            w.done_retry_btn.remove_css_class("suggested-action");
            w.done_btn.remove_css_class("suggested-action");
            w.done_btn.remove_css_class("pill");
            if *needs_reboot {
                w.done_reboot_btn.add_css_class("suggested-action");
            } else if *failed > 0 {
                w.done_retry_btn.add_css_class("suggested-action");
            } else {
                w.done_btn.add_css_class("suggested-action");
            }
            w.done_installed_heading
                .set_text(if *failed > 0 { "Finished" } else { "Installed" });
            w.done_installed_header_clamp
                .set_visible(!packages.is_empty());
            let mut listed: Vec<Package> = packages.to_vec();
            listed.sort_by_key(|package| match package.status {
                PackageStatus::Failed => 0,
                PackageStatus::Completed => 1,
                _ => 2,
            });
            w.done_queue.sync(&listed, None, None);
            w.done_installed_scroll.set_visible(!packages.is_empty());
            sync_inspector_from_packages(w, packages, &w.done_queue);
        }
        Phase::Failed {
            title,
            detail,
            retry,
        } => {
            clear_running_lists(w);
            close_inspector(w);
            w.stack
                .set_visible_child_full("failed", gtk::StackTransitionType::None);
            paint_header(w, HeaderView::Failed);
            w.fail_page.set_title(title);
            w.fail_page.set_description(Some(detail));
            w.fail_retry.set_label(fail_retry_label(*retry));
        }
    }

    match w.detail_view.get() {
        DetailView::ActivityLog => {
            w.detail_view_stack.set_visible_child_name("activity-log");
            let (matched, total) = w.activity_log.sync_full(console);
            paint_activity_log_chrome(w, matched, total);
        }
        DetailView::History => {
            w.detail_view_stack.set_visible_child_name("history");
            let (matched, total) = w.history_view.counts();
            paint_history_chrome(w, matched, total);
        }
        DetailView::Updates => {
            w.back_to_updates_btn.set_visible(false);
            w.detail_view_stack.set_visible_child_name("updates");
        }
    }
}

#[cfg(test)]
mod tests {
    fn object_block<'a>(xml: &'a str, id: &str) -> &'a str {
        let needle = format!("id=\"{id}\"");
        let id_pos = xml
            .find(&needle)
            .unwrap_or_else(|| panic!("window.ui missing id={id}"));
        let start = xml[..id_pos]
            .rfind("<object")
            .unwrap_or_else(|| panic!("missing <object> opening tag before id={id}"));
        let mut depth = 0usize;
        let mut i = start;
        while let Some(rel) = xml[i..].find('<') {
            let tag_start = i + rel;
            let rest = &xml[tag_start..];
            if rest.starts_with("</object>") {
                depth -= 1;
                let tag_end = tag_start + "</object>".len();
                if depth == 0 {
                    return &xml[start..tag_end];
                }
                i = tag_end;
            } else if rest.starts_with("<object")
                && rest.as_bytes().get("<object".len()).is_some_and(|b| {
                    matches!(b, b' ' | b'\n' | b'\r' | b'\t' | b'>' | b'/')
                })
            {
                let close_rel = rest.find('>').expect("unclosed <object tag");
                let tag_end = tag_start + close_rel + 1;
                if !xml[tag_start..tag_end].ends_with("/>") {
                    depth += 1;
                }
                i = tag_end;
            } else {
                i = tag_start + 1;
            }
        }
        panic!("unclosed <object> block for id={id}");
    }

    #[test]
    fn window_template_has_adwaita_pages() {
        let xml = super::WINDOW_UI;
        let ids = super::super::builder_ids_in_source(include_str!("window.rs"));
        assert!(!ids.is_empty());
        for id in ids {
            assert!(
                xml.contains(&format!("id=\"{id}\"")),
                "window.ui missing id={id}"
            );
        }

        let compact_at = xml
            .find("id=\"compact_breakpoint\"")
            .expect("compact_breakpoint");
        let narrow_at = xml
            .find("id=\"narrow_breakpoint\"")
            .expect("narrow_breakpoint");
        assert!(
            compact_at < narrow_at,
            "widest breakpoint (720px) must precede narrower breakpoint (500px)"
        );
        assert!(object_block(xml, "compact_breakpoint").contains("max-width: 720px"));
        assert!(object_block(xml, "narrow_breakpoint").contains("max-width: 500px"));

        let split_block = object_block(xml, "split_view");
        assert!(split_block.contains("AdwNavigationSplitView"));
        assert!(split_block.contains("min-sidebar-width"));
        assert!(split_block.contains("max-sidebar-width"));
        assert!(split_block.contains("sidebar-width-fraction"));

        assert!(object_block(xml, "sidebar_toggle").contains("sidebar-show-left-symbolic"));
        assert!(
            !xml.contains("id=\"upgrade_notice\""),
            "persistent top toolbar upgrade_notice must be removed in Observatory"
        );
        assert!(
            object_block(xml, "ready_toolbar")
                .contains("<property name=\"bottom-bar-style\">1</property>")
        );
        assert!(
            object_block(xml, "done_toolbar")
                .contains("<property name=\"bottom-bar-style\">1</property>")
        );

        assert!(
            object_block(xml, "stack").contains("vhomogeneous"),
            "stack must size every page to the window so Done/Idle fill after apply"
        );
        assert!(
            object_block(xml, "idle_page").contains("vexpand"),
            "idle status page must expand to fill the stack"
        );

        let header_at = xml.find("id=\"header_menu\"").expect("header_menu");
        // AdwHeaderBar.pack_end prepends. The first end child is the trailing
        // app button, so the menu is emitted before Stop.
        let stop_at = xml.find("id=\"run_stop\"").expect("run_stop");
        assert!(
            header_at < stop_at,
            "pack_end prepends: list the menu before Stop so Stop is the start-side button"
        );

        let installed_at = xml
            .find("id=\"done_installed_heading\"")
            .expect("done_installed_heading");
        let done_btn_at = xml.find("id=\"done_btn\"").expect("done_btn");
        assert!(
            installed_at < done_btn_at,
            "installed list sits above the summary actions"
        );

        let stall_at = xml.find("id=\"run_stall\"").expect("run_stall");
        let card_at = xml.find("id=\"current_package\"").expect("current_package");
        let scroll_at = xml.find("id=\"run_list_scroll\"").expect("run_list_scroll");
        let list_at = xml.find("id=\"run_list_slot\"").expect("run_list_slot");
        let completed_at = xml.find("id=\"run_completed\"").expect("run_completed");
        assert!(
            stall_at < card_at
                && card_at < scroll_at
                && scroll_at < list_at
                && list_at < completed_at,
            "running page layout order: run_stall < current_package < run_list_scroll < run_list_slot < run_completed"
        );

        let scroll_block = object_block(xml, "run_list_scroll");
        assert!(
            scroll_block.contains("propagate-natural-height"),
            "run_list_scroll must not propagate the package list height to the window"
        );
        assert!(scroll_block.contains("id=\"run_list_slot\""));
        assert!(
            scroll_block.contains("id=\"run_completed\""),
            "completed expander must live inside run_list_scroll alongside remaining work"
        );

        // Observatory 1.2, 1.3 & 2.3 additions: BottomSheet console, status stack, stage breadcrumbs, Done closure, History & selective UI.
        assert!(object_block(xml, "detail_view_stack").contains("activity-log"));
        assert!(object_block(xml, "detail_view_stack").contains("history"));
        assert!(xml.contains("id=\"ready_selection_bar\""));
        assert!(object_block(xml, "ready_select_all").contains("GtkCheckButton"));
        let sheet_block = object_block(xml, "console_sheet");
        assert!(sheet_block.contains("AdwBottomSheet"));
        assert!(sheet_block.contains("id=\"console_toggle\""));
        assert!(sheet_block.contains("id=\"console_full\""));
        assert!(!xml.contains("id=\"console_drawer\""));
        assert!(!xml.contains("id=\"reboot_banner\""));

        let status_stack = object_block(xml, "now_playing_status_stack");
        assert!(status_stack.contains("AdwSpinner"));
        assert!(status_stack.contains("id=\"now_playing_status_icon\""));
        assert!(status_stack.contains("<property name=\"transition-type\">1</property>"));
        assert!(status_stack.contains("<property name=\"transition-duration\">150</property>"));
        assert!(status_stack.contains("<property name=\"width-request\">22</property>"));
        assert!(status_stack.contains("<property name=\"height-request\">22</property>"));
        for stage_id in [
            "stage_download",
            "stage_prepare",
            "stage_install",
            "stage_verify",
        ] {
            assert!(xml.contains(&format!("id=\"{stage_id}\"")));
        }
        assert!(xml.contains("id=\"done_summary_clamp\""));
        assert!(xml.contains("id=\"done_reboot_clamp\""));

        let insp_block = object_block(xml, "inspector_split");
        assert!(insp_block.contains("AdwOverlaySplitView"));
        assert!(insp_block.contains("max-sidebar-width"));
        for clamp_id in [
            "check_list_clamp",
            "ready_list_clamp",
            "done_list_clamp",
        ] {
            assert!(object_block(xml, clamp_id).contains("AdwClampScrollable"));
        }
        assert!(object_block(xml, "ready_search").contains("GtkSearchEntry"));
        assert!(object_block(xml, "ready_split_update").contains("AdwSplitButton"));
    }

    #[test]
    fn session_started_commit_leaves_auth_and_panics_if_a_borrow_is_held() {
        use fedora_updater::model::AuthPurpose;
        use fedora_updater::state::Phase;
        use std::cell::RefCell;
        use std::panic::{catch_unwind, AssertUnwindSafe};
        use std::rc::Rc;

        let authenticating = || {
            Rc::new(RefCell::new(fedora_updater::state::AppState {
                phase: Phase::Authenticating {
                    purpose: AuthPurpose::Check,
                    packages: vec![],
                    last_checked: None,
                },
                ..Default::default()
            }))
        };

        let state = authenticating();
        assert!(super::commit_session_started(&state).expect("SessionReady"));
        assert!(
            matches!(state.borrow().phase, Phase::Checking { .. }),
            "a finished polkit session must leave Authenticating"
        );

        let state = authenticating();
        let held = state.borrow();
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            let _ = super::commit_session_started(&state);
        }));
        std::panic::set_hook(prev_hook);
        assert!(
            panicked.is_err(),
            "a held Ref must panic on the session write, not skip it"
        );
        drop(held);
        assert!(
            matches!(
                state.borrow().phase,
                Phase::Authenticating {
                    purpose: AuthPurpose::Check,
                    ..
                }
            ),
            "the panicked write must leave the phase on Authenticating"
        );
    }

    fn collect_descendants<T: gtk::glib::object::IsA<gtk::Widget>>(
        root: &impl gtk::glib::object::IsA<gtk::Widget>,
        out: &mut Vec<T>,
    ) {
        use gtk::prelude::*;

        let mut child = root.as_ref().first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            if let Ok(typed) = widget.clone().downcast::<T>() {
                out.push(typed);
            }
            collect_descendants(&widget, out);
            child = next;
        }
    }

    fn verify_rows_widgets() {
        use super::super::rows;
        use fedora_updater::model::{Package, PackageStatus, UpdateSource};
        use gtk::prelude::*;
        use libadwaita::prelude::*;

        for (source, badge, cls) in [
            (UpdateSource::Dnf, "DNF", "src-dnf"),
            (UpdateSource::FlatpakUser, "FP", "src-flatpak"),
            (UpdateSource::FlatpakSystem, "FP", "src-flatpak"),
            (UpdateSource::Firmware, "FW", "src-firmware"),
        ] {
            let lbl = rows::source_badge(source);
            assert_eq!(lbl.label().as_str(), badge);
            assert_eq!(lbl.width_chars(), 3);
            assert!(lbl.has_css_class("package-badge"));
            assert!(lbl.has_css_class(cls));
        }

        // finished_row: omits redundant `· System` / `· Apps`, marks completed when requested, keeps Failed & size.
        let mut done_pkg = Package::new_dnf("openssl", "x86_64", "3.2.2", "updates");
        done_pkg.size_bytes = Some(2 * 1024 * 1024);
        done_pkg.status = PackageStatus::Completed;
        let done_unmarked = rows::finished_row(&done_pkg, false);
        assert!(!done_unmarked.subtitle().unwrap_or_default().contains("· System"));
        let mut done_labels = Vec::new();
        collect_descendants::<gtk::Label>(&done_unmarked, &mut done_labels);
        assert!(!done_labels.iter().any(|l| l.label().as_str() == "done"));
        assert!(done_labels.iter().any(|l| l.label().as_str() == "2.0 MB"));

        let done_marked = rows::finished_row(&done_pkg, true);
        let mut marked_labels = Vec::new();
        collect_descendants::<gtk::Label>(&done_marked, &mut marked_labels);
        assert!(marked_labels.iter().any(|l| l.label().as_str() == "done"));
        assert!(marked_labels.iter().any(|l| l.label().as_str() == "2.0 MB"));

        let mut failed_pkg = done_pkg.clone();
        failed_pkg.status = PackageStatus::Failed;
        let failed_finished = rows::finished_row(&failed_pkg, false);
        let mut failed_labels = Vec::new();
        collect_descendants::<gtk::Label>(&failed_finished, &mut failed_labels);
        assert!(failed_labels
            .iter()
            .any(|l| l.label().as_str() == "failed" && l.has_css_class("error")));

        // refill_expander / append_expander store tracking for the Running completed drawer.
        let exp = libadwaita::ExpanderRow::new();
        let exp_store = std::cell::RefCell::new(Vec::new());
        rows::refill_expander(&exp, &exp_store, vec![rows::finished_row(&done_pkg, true)]);
        assert_eq!(exp_store.borrow().len(), 1);
        rows::append_expander(&exp, &exp_store, vec![rows::finished_row(&failed_pkg, true)]);
        assert_eq!(exp_store.borrow().len(), 2);
        rows::refill_expander(&exp, &exp_store, std::iter::empty());
        assert!(exp_store.borrow().is_empty());
    }

    fn verify_dialogs_widgets() {
        use super::super::dialogs;
        use fedora_updater::presenter::{stale_update_dialog, stop_update_dialog};
        use gtk::prelude::*;
        use libadwaita::prelude::*;
        use std::cell::Cell;
        use std::rc::Rc;

        // 1) Stale update AlertDialog properties & response wiring.
        let stale = stale_update_dialog();
        let confirmed = Rc::new(Cell::new(0usize));
        let confirmed_cb = confirmed.clone();
        let stale_dlg = dialogs::build_confirm_dialog(stale, move || {
            confirmed_cb.set(confirmed_cb.get() + 1);
        });
        assert_eq!(stale_dlg.heading().as_deref(), Some("Refresh Update List?"));
        assert_eq!(
            stale_dlg.body().as_str(),
            "This list is more than a day old. Refresh it before applying these updates."
        );
        assert!(stale_dlg.has_response("no"));
        assert!(stale_dlg.has_response("yes"));
        assert_eq!(stale_dlg.response_label("no").as_str(), "_Cancel");
        assert_eq!(stale_dlg.response_label("yes").as_str(), "_Refresh");
        assert_eq!(stale_dlg.default_response().as_deref(), Some("yes"));
        assert_eq!(stale_dlg.close_response().as_str(), "no");
        assert_eq!(
            stale_dlg.response_appearance("yes"),
            libadwaita::ResponseAppearance::Suggested
        );
        stale_dlg.emit_by_name::<()>("response", &[&"no"]);
        assert_eq!(confirmed.get(), 0, "cancel must not trigger confirm callback");
        stale_dlg.emit_by_name::<()>("response", &[&"yes"]);
        assert_eq!(confirmed.get(), 1, "confirm response must invoke callback once");

        // 2) Stop update AlertDialog properties & destructive appearance.
        let stop = stop_update_dialog();
        let stopped = Rc::new(Cell::new(0usize));
        let stopped_cb = stopped.clone();
        let stop_dlg = dialogs::build_confirm_dialog(stop, move || {
            stopped_cb.set(stopped_cb.get() + 1);
        });
        assert_eq!(stop_dlg.heading().as_deref(), Some("Stop This Update?"));
        assert!(stop_dlg.body().contains("package database"));
        assert_eq!(stop_dlg.default_response().as_deref(), Some("wait"));
        assert_eq!(stop_dlg.close_response().as_str(), "wait");
        assert_eq!(
            stop_dlg.response_appearance("stop"),
            libadwaita::ResponseAppearance::Destructive
        );
        stop_dlg.emit_by_name::<()>("response", &[&"wait"]);
        assert_eq!(stopped.get(), 0);
        stop_dlg.emit_by_name::<()>("response", &[&"stop"]);
        assert_eq!(stopped.get(), 1);

        // 3) Release-upgrade dialog & copyable command_row without trailing newline.
        let (upgrade_dlg, cmd_rows) = dialogs::build_upgrade_dialog(45);
        assert_eq!(upgrade_dlg.title().as_str(), "Upgrade from the terminal");
        assert_eq!(upgrade_dlg.content_width(), 560);
        assert!(upgrade_dlg.follows_content_size());
        assert_eq!(cmd_rows.len(), 2);

        let expected_cmds = [
            (
                "sudo dnf system-upgrade download --releasever=45",
                "Copy download command",
            ),
            ("sudo dnf system-upgrade reboot", "Copy reboot command"),
        ];
        for (row, (expected_cmd, expected_tip)) in cmd_rows.iter().zip(expected_cmds) {
            assert!(row.has_css_class("upgrade-command"));
            let mut labels = Vec::new();
            collect_descendants::<gtk::Label>(row, &mut labels);
            let cmd_label = labels
                .iter()
                .find(|l| l.has_css_class("monospace"))
                .expect("command label");
            assert_eq!(cmd_label.label().as_str(), expected_cmd);
            assert!(
                !cmd_label.label().ends_with('\n'),
                "copy command must not end in newline"
            );
            assert!(cmd_label.is_selectable());
            assert!(!cmd_label.can_focus());

            let mut buttons = Vec::new();
            collect_descendants::<gtk::Button>(row, &mut buttons);
            assert_eq!(buttons.len(), 1);
            assert_eq!(buttons[0].tooltip_text().as_deref(), Some(expected_tip));
        }

        // Direct command_row strip check.
        let single_row = dialogs::command_row("sudo dnf --help", "Copy help");
        let mut single_labels = Vec::new();
        collect_descendants::<gtk::Label>(&single_row, &mut single_labels);
        assert_eq!(single_labels[0].label().as_str(), "sudo dnf --help");
    }

    fn verify_all_phases_narrow_and_recovery() {
        use fedora_updater::console::ConsoleBuffer;
        use fedora_updater::model::{
            AuthPurpose, IdleScreen, Package, PackageStatus, RetryAction, RunPass, StageCount,
            Stopwatch, UpdateSource,
        };
        use fedora_updater::state::{reduce, AppState, Event, Phase, RunningState};
        use gtk::prelude::*;
        use libadwaita::prelude::*;

        let widgets = super::Widgets::for_test();
        super::wire_expander(&widgets);
        let console = ConsoleBuffer::new(100);

        // 1) Idle::Fresh + default_widget + Release Upgrade banner (with age, without age, cleared).
        let fresh_with_upgrade = AppState {
            phase: Phase::Idle(IdleScreen::Fresh),
            release_upgrade: Some(45),
            release_age_days: Some(3),
            ..Default::default()
        };
        super::render(&fresh_with_upgrade, &console, &widgets);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("idle"));
        assert_eq!(widgets.idle_page.title().as_str(), "Check for Updates");
        assert_eq!(widgets.idle_btn.label().as_deref(), Some("Check for Updates"));
        assert!(widgets.idle_btn.has_css_class("suggested-action"));
        assert!(widgets.idle_btn.has_css_class("pill"));
        assert!(!widgets.idle_log.get_visible());
        assert!(widgets.focus_idle_check.get());
        assert_eq!(
            widgets.window.default_widget().as_ref(),
            Some(widgets.idle_btn.upcast_ref()),
            "Fresh idle must arm default_widget on Check CTA"
        );
        assert!(!widgets.header_refresh.get_visible());
        assert!(!widgets.run_stop.get_visible());
        assert!(!widgets.console_sheet.reveals_bottom_bar());
        assert_eq!(
            widgets.sidebar.nav_context_name().as_deref(),
            Some("sources")
        );
        assert!(widgets.sidebar.upgrade_card().root().get_visible());
        assert!(widgets
            .sidebar
            .upgrade_card()
            .title()
            .label()
            .contains("Fedora Linux 45"));
        assert!(widgets.sidebar.upgrade_card().age().get_visible());
        assert!(widgets
            .sidebar
            .upgrade_card()
            .age()
            .label()
            .contains("3 days"));
        assert!(!widgets.idle_upgrade.root().get_visible());

        // Release notice without age hides age label; None hides whole card.
        super::paint_release_upgrade(&widgets, Some(45), None);
        assert!(widgets.sidebar.upgrade_card().root().get_visible());
        assert!(!widgets.sidebar.upgrade_card().age().get_visible());
        super::paint_release_upgrade(&widgets, None, None);
        assert!(!widgets.sidebar.upgrade_card().root().get_visible());

        // 2) Idle::Warning vs Idle::Current (neither arms default_widget nor hides problems).
        let warning_state = AppState {
            phase: Phase::Idle(IdleScreen::Warning {
                title: "Could not check all sources",
                detail: "Some sources could not be checked: dnf check-update failed (exit 1)".into(),
                checked_at: Some("just now".into()),
            }),
            ..Default::default()
        };
        super::render(&warning_state, &console, &widgets);
        assert_eq!(
            widgets.idle_page.title().as_str(),
            "Could not check all sources"
        );
        assert_eq!(
            widgets.idle_page.icon_name().as_deref(),
            Some("dialog-warning-symbolic")
        );
        assert_eq!(widgets.idle_btn.label().as_deref(), Some("Check Again"));
        assert!(widgets.idle_btn.has_css_class("suggested-action"));
        assert!(widgets.idle_log.get_visible());
        assert!(!widgets.focus_idle_check.get());
        assert!(widgets.window.default_widget().is_none());

        let current_state = AppState {
            phase: Phase::Idle(IdleScreen::Current {
                checked_at: "just now".into(),
                detail: None,
            }),
            ..Default::default()
        };
        super::render(&current_state, &console, &widgets);
        assert_eq!(widgets.idle_page.title().as_str(), "System is up to date");
        assert!(widgets.idle_btn.has_css_class("flat"));
        assert!(!widgets.idle_btn.has_css_class("suggested-action"));
        assert!(!widgets.idle_log.get_visible());
        assert!(widgets.window.default_widget().is_none());

        // 3) Authenticating (Apply with packages) -> CancelAuth returns cleanly to Ready.
        let mut dnf_pkg = Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates");
        dnf_pkg.size_bytes = Some(112_000_000);
        let mut auth_apply = AppState {
            phase: Phase::Authenticating {
                purpose: AuthPurpose::Apply,
                packages: vec![dnf_pkg.clone()],
                last_checked: None,
            },
            ..Default::default()
        };
        super::render(&auth_apply, &console, &widgets);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("auth"));
        assert_eq!(widgets.auth_page.title().as_str(), "Authentication Required");
        assert_eq!(widgets.window_title.title().as_str(), "Fedora Updater");
        assert_eq!(
            widgets.window_title.subtitle().as_str(),
            "Applying updates"
        );
        assert!(widgets.auth_cancel.get_visible());
        assert_eq!(widgets.auth_cancel.label().as_deref(), Some("Cancel"));

        reduce(&mut auth_apply, Event::CancelAuth).expect("CancelAuth returns to Ready");
        assert!(matches!(auth_apply.phase, Phase::Ready { ref packages, .. } if packages.len() == 1));
        super::render(&auth_apply, &console, &widgets);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("ready"));
        assert_eq!(widgets.ready_queue.total_count(), 1);

        widgets.narrow.set(true);
        super::apply_header_subtitle(&widgets);
        assert_eq!(widgets.window_title.subtitle().as_str(), "");
        widgets.narrow.set(false);
        super::apply_header_subtitle(&widgets);

        // 4) Checking: progress bar, footnote, incoming package list visibility, BottomSheet bottom bar.
        let checking_empty = AppState {
            phase: Phase::Checking {
                packages_so_far: vec![],
                soft_errors: vec![],
                checked_sources: vec![],
            },
            ..Default::default()
        };
        super::render(&checking_empty, &console, &widgets);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("checking"));
        assert!((widgets.check_progress.fraction() - 0.05).abs() < 1e-6);
        assert_eq!(widgets.check_progress_pct.label().as_str(), "5%");
        assert!(!widgets.check_list_scroll.get_visible());
        assert!(widgets.console_sheet.reveals_bottom_bar());
        assert!(widgets.console_sheet.can_open());

        let checking_populated = AppState {
            phase: Phase::Checking {
                packages_so_far: vec![dnf_pkg.clone()],
                soft_errors: vec![],
                checked_sources: vec![UpdateSource::Dnf],
            },
            ..Default::default()
        };
        super::render(&checking_populated, &console, &widgets);
        assert!((widgets.check_progress.fraction() - 0.25).abs() < 1e-6);
        assert_eq!(widgets.check_progress_pct.label().as_str(), "25%");
        assert!(widgets.check_list_scroll.get_visible());
        assert_eq!(widgets.check_queue.total_count(), 1);

        // 5) Ready: clean, pre-install Stop via reducer, soft error, and empty-list sensitivity guard.
        let mut fp_pkg = Package::new_dnf("Steam", "", "1.0.0.82", "flathub");
        fp_pkg.source = UpdateSource::FlatpakSystem;
        fp_pkg.size_bytes = Some(251_000_000);

        let ready_clean = AppState {
            phase: Phase::Ready {
                packages: vec![dnf_pkg.clone(), fp_pkg.clone()],
                soft_errors: vec![],
            },
            ..Default::default()
        };
        super::render(&ready_clean, &console, &widgets);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("ready"));
        assert!(widgets.header_refresh.get_visible());
        assert!(!widgets.console_sheet.reveals_bottom_bar());
        assert!(!widgets.ready_banner.is_revealed());
        assert!(widgets.ready_update.get_sensitive());
        assert_eq!(widgets.ready_queue.total_count(), 2);
        assert_eq!(widgets.window_title.title().as_str(), "2 Updates Available");
        assert!(widgets.window_title.subtitle().contains("1 system · 1 apps"));

        // Pre-install Stop returns via reducer to Ready with exact Stop banner (NOT "1 source could not be checked").
        let mut stop_download_state = AppState {
            phase: Phase::Running(RunningState::new(vec![dnf_pkg.clone()])),
            ..Default::default()
        };
        reduce(
            &mut stop_download_state,
            Event::StopApply {
                during_transaction: false,
            },
        )
        .expect("pre-install Stop transitions to Ready");
        super::render(&stop_download_state, &console, &widgets);
        assert!(widgets.ready_banner.is_revealed());
        assert_eq!(
            widgets.ready_banner.title().as_str(),
            "Stopped before install. Downloaded packages were not applied."
        );
        assert_eq!(
            widgets.ready_banner.button_label().as_deref(),
            Some("View Log")
        );

        // Ready with backend check error reveals "1 source could not be checked"; empty list disables Update All.
        let ready_err_empty = AppState {
            phase: Phase::Ready {
                packages: vec![],
                soft_errors: vec!["dnf check-update failed (exit 1)".into()],
            },
            ..Default::default()
        };
        super::render(&ready_err_empty, &console, &widgets);
        assert!(widgets.ready_banner.is_revealed());
        assert_eq!(
            widgets.ready_banner.title().as_str(),
            "1 source could not be checked"
        );
        assert!(!widgets.ready_update.get_sensitive());

        // 6) Running: 4-stage breadcrumbs, ProgressRing, 22x22 Gtk.Stack, sidebar pipeline + telemetry, expander user-open, quiet & stall modes.
        let mut p_done = dnf_pkg.clone();
        p_done.status = PackageStatus::Completed;
        let mut p_dl = Package::new_dnf("kernel-core", "x86_64", "7.1.12", "updates");
        p_dl.size_bytes = Some(21 * 1024 * 1024);
        p_dl.status = PackageStatus::Downloading;

        let running_dl = AppState {
            phase: Phase::Running(RunningState {
                packages: vec![p_dl.clone(), fp_pkg.clone()],
                active_index: Some(0),
                overall_progress: 0.5,
                phase_label: "Downloading packages".into(),
                current_source: Some(UpdateSource::Dnf),
                current_name: Some("kernel-core".into()),
                work_progress: Some(0.65),
                pass: RunPass::Download,
                stage: Some(StageCount {
                    current: 1,
                    total: 2,
                }),
                needs_reboot: false,
                failed_sources: vec![],
                failure_reasons: vec![],
            }),
            stopwatch: Some(Stopwatch::start()),
            ..Default::default()
        };
        super::render(&running_dl, &console, &widgets);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("running"));
        assert!(widgets.run_stop.get_visible());
        assert_eq!(widgets.run_stop.label().as_deref(), Some("Stop"));
        assert!(!widgets.run_stop.has_css_class("destructive-action"));
        assert_eq!(widgets.window_title.title().as_str(), "Downloading");
        assert!(widgets.stage_download.has_css_class("active"));
        assert!(!widgets.stage_prepare.has_css_class("active"));
        assert_eq!(widgets.now_playing_title.label().as_str(), "kernel-core");
        assert_eq!(widgets.progress_ring.label_text(), "65%");
        assert_eq!(
            widgets.now_playing_status_stack.transition_type(),
            gtk::StackTransitionType::Crossfade
        );
        assert_eq!(
            widgets.now_playing_status_stack.transition_duration(),
            150
        );
        assert_eq!(
            widgets.now_playing_status_stack.visible_child_name().as_deref(),
            Some("status")
        );
        assert!(widgets.now_playing_size.get_visible());
        assert_eq!(widgets.now_playing_size.label().as_str(), "21 MB");
        assert_eq!(widgets.run_queue.total_count(), 2);
        assert!(!widgets.run_completed_group.get_visible());
        assert_eq!(
            widgets.sidebar.nav_context_name().as_deref(),
            Some("pipeline")
        );
        assert_eq!(
            widgets.sidebar.pipeline_status_texts(),
            ["0/1".to_string(), "Queued".to_string(), "—".to_string()]
        );
        assert!(!widgets.sidebar.sparkline_card().get_visible());

        // Prepare stage breadcrumb transition.
        let running_prepare = AppState {
            phase: Phase::Running(RunningState {
                packages: vec![p_dl.clone(), fp_pkg.clone()],
                active_index: Some(0),
                overall_progress: 0.5,
                phase_label: "Running transaction".into(),
                current_source: Some(UpdateSource::Dnf),
                current_name: Some("kernel-core".into()),
                work_progress: Some(0.9),
                pass: RunPass::Download,
                stage: None,
                needs_reboot: false,
                failed_sources: vec![],
                failure_reasons: vec![],
            }),
            ..Default::default()
        };
        super::render(&running_prepare, &console, &widgets);
        assert!(widgets.stage_download.has_css_class("done"));
        assert_eq!(widgets.stage_download.label().as_str(), "Download ✓");
        assert!(widgets.stage_prepare.has_css_class("active"));

        // Switch to Apply pass with 1 completed row, expand run_completed, then advance to Verifying + add a 2nd completed row.
        let mut p_inst = p_dl.clone();
        p_inst.status = PackageStatus::Installing;
        let running_apply_1 = RunningState {
            packages: vec![p_done.clone(), p_inst.clone(), fp_pkg.clone()],
            active_index: Some(1),
            overall_progress: 0.33,
            phase_label: "Installing".into(),
            current_source: Some(UpdateSource::Dnf),
            current_name: Some("kernel-core".into()),
            work_progress: Some(0.4),
            pass: RunPass::Apply,
            stage: None,
            needs_reboot: false,
            failed_sources: vec![],
            failure_reasons: vec![],
        };
        let state_apply_1 = AppState {
            phase: Phase::Running(running_apply_1.clone()),
            ..Default::default()
        };
        super::render(&state_apply_1, &console, &widgets);
        assert_eq!(widgets.run_stop.label().as_deref(), Some("Stop…"));
        assert!(widgets.run_stop.has_css_class("destructive-action"));
        assert_eq!(widgets.window_title.title().as_str(), "Applying");
        assert!(widgets.stage_download.has_css_class("done"));
        assert!(widgets.stage_prepare.has_css_class("done"));
        assert!(widgets.stage_install.has_css_class("active"));
        assert!(!widgets.stage_verify.has_css_class("active"));
        assert!(widgets.run_completed_group.get_visible());
        assert_eq!(widgets.run_completed.title().as_str(), "1 completed");
        assert!(!widgets.run_completed.is_expanded());

        // Simulate user opening the completed expander; subsequent re-render must preserve it + append row.
        widgets.run_completed.set_expanded(true);
        assert!(widgets.run_completed_user_open.get());

        let mut p_inst_done = p_inst.clone();
        p_inst_done.status = PackageStatus::Completed;
        let mut fp_active = fp_pkg.clone();
        fp_active.status = PackageStatus::Installing;
        let state_apply_2 = AppState {
            phase: Phase::Running(RunningState {
                packages: vec![p_done.clone(), p_inst_done, fp_active],
                active_index: Some(2),
                overall_progress: 0.66,
                phase_label: "Verifying".into(),
                current_source: Some(UpdateSource::FlatpakSystem),
                current_name: Some("Steam".into()),
                work_progress: None,
                pass: RunPass::Apply,
                stage: None,
                needs_reboot: false,
                failed_sources: vec![],
                failure_reasons: vec![],
            }),
            ..Default::default()
        };
        super::render(&state_apply_2, &console, &widgets);
        assert_eq!(widgets.window_title.title().as_str(), "Verifying");
        assert!(widgets.stage_install.has_css_class("done"));
        assert!(widgets.stage_verify.has_css_class("active"));
        assert_eq!(
            widgets.now_playing_status_stack.visible_child_name().as_deref(),
            Some("spinner")
        );
        assert_eq!(
            widgets.sidebar.pipeline_status_texts(),
            ["2/2 ✓".to_string(), "0/1".to_string(), "—".to_string()]
        );
        assert_eq!(widgets.run_completed.title().as_str(), "2 completed");
        assert!(
            widgets.run_completed.is_expanded(),
            "user-expanded completed drawer must stay open across updates"
        );
        assert_eq!(widgets.run_completed_rows.borrow().len(), 2);

        // Deterministic quiet telemetry: <15s ETA -> >=15s ETA suppressed -> 5s at_full_item -> 120s GNOME Software stall banner.
        super::paint_quiet_chrome_with(
            &running_apply_1,
            "System · 1 of 3 done",
            20,
            6,
            &widgets,
            Some("Downloading kernel-core — 21 MB and rising"),
            false,
        );
        assert_eq!(
            widgets.now_playing_status_stack.visible_child_name().as_deref(),
            Some("spinner")
        );
        assert!(!widgets.run_stall.is_revealed());
        assert_eq!(widgets.sidebar.telemetry_elapsed_text(), "20s");
        assert_eq!(
            widgets.sidebar.telemetry_eta_text().as_deref(),
            Some("Downloading kernel-core — 21 MB and rising")
        );
        assert!(widgets
            .window_title
            .subtitle()
            .contains("Downloading kernel-core — 21 MB and rising"));

        // 15s quiet hides unreliable ETA in both header and sidebar.
        super::paint_quiet_chrome_with(
            &running_apply_1,
            "System · 1 of 3 done",
            70,
            15,
            &widgets,
            None,
            false,
        );
        assert!(widgets.sidebar.telemetry_eta_text().is_none());
        assert_eq!(widgets.sidebar.telemetry_elapsed_text(), "1m 10s");
        assert_eq!(
            widgets.now_playing_status_stack.visible_child_name().as_deref(),
            Some("status")
        );

        // 5s with at_full_item switches fixed 22x22 stack to spinner without layout shift.
        let mut full_running = running_apply_1.clone();
        full_running.work_progress = Some(1.0);
        super::paint_quiet_chrome_with(
            &full_running,
            "System · 1 of 3 done",
            25,
            5,
            &widgets,
            None,
            false,
        );
        assert_eq!(
            widgets.now_playing_status_stack.visible_child_name().as_deref(),
            Some("spinner")
        );

        // 120s quiet + dnf5daemon-server running surfaces exact GNOME Software stall banner.
        super::paint_quiet_chrome_with(
            &running_apply_1,
            "System · 1 of 3 done",
            200,
            180,
            &widgets,
            None,
            true,
        );
        assert!(widgets.run_stall.is_revealed());
        assert_eq!(
            widgets.run_stall.title().as_str(),
            "No output for 3 minutes. Waiting on GNOME Software."
        );
        assert_eq!(
            widgets.sidebar.telemetry_eta_text().as_deref(),
            Some("Waiting on dnf")
        );

        // 7) Done: all 4 sub-cases + sole suggested-action invariant + Failed-first sort.
        let done_clean = AppState {
            phase: Phase::Done {
                packages: vec![p_done.clone()],
                duration: "45s".into(),
                needs_reboot: false,
                failed: 0,
                failure_reasons: vec![],
            },
            ..Default::default()
        };
        super::render(&done_clean, &console, &widgets);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("done"));
        assert!(!widgets.run_stall.is_revealed());
        assert!(!widgets.console_sheet.reveals_bottom_bar());
        assert_eq!(
            widgets.sidebar.nav_context_name().as_deref(),
            Some("sources")
        );
        assert!(widgets.done_summary_clamp.get_visible());
        assert_eq!(
            widgets.done_summary_detail.label().as_str(),
            "1 update installed in 45s. No restart needed."
        );
        assert!(!widgets.done_reboot_clamp.get_visible());
        assert!(!widgets.done_problem_banner.is_revealed());
        assert!(!widgets.done_retry_btn.get_visible());
        assert!(widgets.done_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_reboot_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_retry_btn.has_css_class("suggested-action"));
        assert_eq!(widgets.done_installed_heading.label().as_str(), "Installed");

        let mut kernel_done = Package::new_dnf("kernel", "x86_64", "7.1.12", "updates");
        kernel_done.status = PackageStatus::Completed;
        let done_reboot = AppState {
            phase: Phase::Done {
                packages: vec![kernel_done.clone(), p_done.clone()],
                duration: "2m 10s".into(),
                needs_reboot: true,
                failed: 0,
                failure_reasons: vec![],
            },
            ..Default::default()
        };
        super::render(&done_reboot, &console, &widgets);
        assert!(!widgets.done_summary_clamp.get_visible());
        assert!(widgets.done_reboot_clamp.get_visible());
        assert_eq!(
            widgets.done_reboot_detail.label().as_str(),
            "kernel requires a restart to take effect."
        );
        assert!(widgets.done_reboot_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_retry_btn.has_css_class("suggested-action"));

        // Partial Done (no reboot): Failed package listed BEFORE Completed package + sole suggested Check Again.
        let mut dnf_failed = Package::new_dnf("kernel", "x86_64", "7.1.12", "updates");
        dnf_failed.status = PackageStatus::Failed;
        let done_partial = AppState {
            phase: Phase::Done {
                packages: vec![p_done.clone(), dnf_failed.clone()],
                duration: "4s".into(),
                needs_reboot: false,
                failed: 1,
                failure_reasons: vec!["System: RPM database is locked".into()],
            },
            ..Default::default()
        };
        super::render(&done_partial, &console, &widgets);
        assert_eq!(widgets.done_installed_heading.label().as_str(), "Finished");
        assert!(!widgets.done_summary_clamp.get_visible());
        assert!(!widgets.done_reboot_clamp.get_visible());
        assert!(widgets.done_problem_banner.is_revealed());
        assert!(widgets
            .done_problem_banner
            .title()
            .contains("RPM database is locked"));
        assert!(widgets.done_retry_btn.get_visible());
        assert!(widgets.done_retry_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_reboot_btn.has_css_class("suggested-action"));
        assert_eq!(
            widgets.done_queue.filtered_ids(),
            vec![dnf_failed.id.clone(), p_done.id.clone()]
        );

        let done_partial_reboot = AppState {
            phase: Phase::Done {
                packages: vec![dnf_failed],
                duration: "4s".into(),
                needs_reboot: true,
                failed: 1,
                failure_reasons: vec!["Firmware: battery too low".into()],
            },
            ..Default::default()
        };
        super::render(&done_partial_reboot, &console, &widgets);
        assert!(!widgets.done_summary_clamp.get_visible());
        assert!(widgets.done_reboot_clamp.get_visible());
        assert!(widgets.done_problem_banner.is_revealed());
        assert!(widgets.done_reboot_btn.has_css_class("suggested-action"));
        assert!(widgets.done_retry_btn.get_visible());
        assert!(!widgets.done_retry_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_btn.has_css_class("suggested-action"));

        // 8) Failed: RetryAction labels (CheckAgain / RetryApply / Reboot) + retry_apply_packages resume.
        for (retry, expected_btn) in [
            (RetryAction::CheckAgain, "Check Again"),
            (RetryAction::RetryApply, "Retry Update"),
            (RetryAction::Reboot, "Restart"),
        ] {
            let failed_state = AppState {
                phase: Phase::Failed {
                    title: "Failure title".into(),
                    detail: "Failure detail".into(),
                    retry,
                },
                retry_apply_packages: if retry == RetryAction::RetryApply {
                    vec![dnf_pkg.clone()]
                } else {
                    vec![]
                },
                ..Default::default()
            };
            super::render(&failed_state, &console, &widgets);
            assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("failed"));
            assert_eq!(widgets.fail_page.title().as_str(), "Failure title");
            assert_eq!(
                widgets.fail_page.description().as_deref(),
                Some("Failure detail")
            );
            assert_eq!(widgets.fail_retry.label().as_deref(), Some(expected_btn));
        }

        let mut retry_flow = AppState {
            phase: Phase::Authenticating {
                purpose: AuthPurpose::Apply,
                packages: vec![dnf_pkg.clone()],
                last_checked: None,
            },
            ..Default::default()
        };
        reduce(
            &mut retry_flow,
            Event::Fail {
                title: "Authorization Failed".into(),
                detail: "Dismissed".into(),
                retry: RetryAction::RetryApply,
            },
        )
        .expect("Fail(RetryApply)");
        assert_eq!(retry_flow.retry_apply_packages.len(), 1);
        reduce(&mut retry_flow, Event::StartApply).expect("StartApply resumes retry_apply_packages");
        assert!(matches!(
            retry_flow.phase,
            Phase::Authenticating {
                purpose: AuthPurpose::Apply,
                ref packages,
                ..
            } if packages.len() == 1
        ));

        gtk::prelude::GtkWindowExt::destroy(&widgets.window);
    }

    fn verify_console_drawer_and_deltas() {
        use fedora_updater::console::ConsoleBuffer;
        use gtk::prelude::*;
        use std::cell::RefCell;
        use std::rc::Rc;

        let widgets = super::Widgets::for_test();
        let console = Rc::new(RefCell::new(ConsoleBuffer::new(3)));
        super::wire_console(&widgets, &console);
        super::sync_console_drawer(&widgets, true);
        assert!(widgets.console_sheet.reveals_bottom_bar());
        assert!(widgets.console_sheet.can_open());

        // Log lines while collapsed update console_line preview; opening BottomSheet populates console_full.
        super::record_console_output(&console, &widgets, "dnf", "Loading metadata", false);
        super::record_console_output(&console, &widgets, "dnf", "[1/2] pkg 10%", true);
        assert!(!widgets.console_sheet.is_open());
        assert_eq!(widgets.console_line.label().as_str(), "[dnf] [1/2] pkg 10%");
        assert_eq!(
            widgets.console_chevron.icon_name().as_deref(),
            Some("pan-end-symbolic")
        );

        widgets.console_toggle.emit_clicked();
        assert!(widgets.console_sheet.is_open());
        assert_eq!(
            widgets.console_chevron.icon_name().as_deref(),
            Some("pan-down-symbolic")
        );
        let buf = widgets.console_full.buffer();
        let text = || {
            let b = widgets.console_full.buffer();
            b.text(&b.start_iter(), &b.end_iter(), false).to_string()
        };
        assert_eq!(text(), console.borrow().full_text());

        // ReplacedLast live draw while open.
        super::record_console_output(&console, &widgets, "dnf", "[1/2] pkg 50%", true);
        assert_eq!(text(), console.borrow().full_text());
        assert!(!text().contains("10%"));

        // Appended + TrimmedAndAppended ring eviction while open stay exact with ConsoleBuffer.
        super::record_console_output(&console, &widgets, "dnf", "[1/2] pkg 100%", false);
        assert_eq!(text(), console.borrow().full_text());
        super::record_console_output(&console, &widgets, "dnf", "Complete!", false);
        assert_eq!(text(), console.borrow().full_text());
        super::record_console_output(&console, &widgets, "fwupd", "Done fw", false);
        assert_eq!(text(), console.borrow().full_text());
        assert!(!text().contains("Loading metadata"));
        assert_eq!(buf.line_count(), 3);

        widgets.console_close_btn.emit_clicked();
        assert!(!widgets.console_sheet.is_open());
        assert_eq!(
            widgets.console_chevron.icon_name().as_deref(),
            Some("pan-end-symbolic")
        );

        gtk::prelude::GtkWindowExt::destroy(&widgets.window);
    }

    fn verify_handle_worker_full_replay() {
        use fedora_updater::console::ConsoleBuffer;
        use fedora_updater::model::{Package, PackageStatus, UpdateSource};
        use fedora_updater::orchestrator::WorkerMsg;
        use fedora_updater::state::{AppState, Event};
        use gtk::prelude::*;
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        use std::time::SystemTime;

        let widgets = super::Widgets::for_test();
        let console = Rc::new(RefCell::new(ConsoleBuffer::new(100)));
        let state = Rc::new(RefCell::new(AppState::default()));
        super::wire_console(&widgets, &console);
        super::wire_expander(&widgets);

        let dnf_pkg = Package::new_dnf("kernel-core", "x86_64", "7.1.12-200.fc44", "updates");
        let fp_pkg = {
            let mut p = Package::new_dnf("Signal", "", "7.50.0", "flathub");
            p.source = UpdateSource::FlatpakUser;
            p
        };

        let auto_apply_calls = Rc::new(Cell::new(0usize));
        let release_lookup_calls = Rc::new(Cell::new(0usize));
        let reboot_preflight_calls = Rc::new(Cell::new(0usize));

        let feed = |msg: WorkerMsg| {
            let aa = auto_apply_calls.clone();
            let rl = release_lookup_calls.clone();
            let rp = reboot_preflight_calls.clone();
            super::handle_worker_with(
                &state,
                &console,
                &widgets,
                msg,
                move || aa.set(aa.get() + 1),
                move || rl.set(rl.get() + 1),
                move || rp.set(rp.get() + 1),
            );
        };

        // Normal check + apply replay through handle_worker_with with zero RefCell panics.
        super::dispatch(&state, &console, &widgets, Event::StartCheck { then_apply: false }, false);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("auth"));

        feed(WorkerMsg::SessionStarted);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("checking"));

        widgets.console_toggle.emit_clicked();
        feed(WorkerMsg::ConsoleDraw {
            tag: "dnf".into(),
            text: "Downloading repodata 40%".into(),
        });
        assert_eq!(
            widgets.console_line.label().as_str(),
            "[dnf] Downloading repodata 40%"
        );
        feed(WorkerMsg::ConsoleLine {
            tag: "dnf".into(),
            text: "Repositories loaded.".into(),
        });
        assert!(console.borrow().full_text().contains("Repositories loaded."));

        feed(WorkerMsg::Event(Event::BackendCheckDone {
            source: UpdateSource::Dnf,
            packages: vec![dnf_pkg.clone()],
            soft_error: None,
        }));
        assert_eq!(widgets.check_queue.total_count(), 1);
        feed(WorkerMsg::Event(Event::BackendCheckDone {
            source: UpdateSource::FlatpakUser,
            packages: vec![fp_pkg.clone()],
            soft_error: None,
        }));
        feed(WorkerMsg::Event(Event::BackendCheckDone {
            source: UpdateSource::FlatpakSystem,
            packages: vec![],
            soft_error: None,
        }));
        feed(WorkerMsg::Event(Event::BackendCheckDone {
            source: UpdateSource::Firmware,
            packages: vec![],
            soft_error: None,
        }));

        feed(WorkerMsg::Event(Event::CheckComplete {
            packages: vec![dnf_pkg.clone(), fp_pkg.clone()],
            soft_errors: vec![],
            listed_at: SystemTime::UNIX_EPOCH,
        }));
        assert_eq!(release_lookup_calls.get(), 1);
        assert_eq!(auto_apply_calls.get(), 0);
        assert!(widgets.notification_sent.get());
        super::withdraw_completion_notification(&widgets);
        assert!(!widgets.notification_sent.get());
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("ready"));
        assert_eq!(widgets.ready_queue.total_count(), 2);

        // Apply flow via handle_worker_with.
        super::dispatch(&state, &console, &widgets, Event::StartApply, false);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("auth"));
        feed(WorkerMsg::SessionStarted);
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("running"));

        feed(WorkerMsg::Event(Event::ApplyProgress {
            source: UpdateSource::Dnf,
            package_hint: Some("kernel-core".into()),
            status_hint: Some(PackageStatus::Installing),
            progress: Some(0.5),
            phase_label: "Installing".into(),
            stage_current: Some(1),
            stage_total: Some(1),
        }));
        assert_eq!(widgets.now_playing_title.label().as_str(), "kernel-core");
        assert_eq!(widgets.run_stop.label().as_deref(), Some("Stop…"));

        feed(WorkerMsg::Event(Event::BackendApplyDone {
            source: UpdateSource::Dnf,
            success: true,
            needs_reboot: true,
            error: None,
        }));
        feed(WorkerMsg::Event(Event::BackendApplyDone {
            source: UpdateSource::FlatpakUser,
            success: true,
            needs_reboot: false,
            error: None,
        }));
        feed(WorkerMsg::Event(Event::ApplyComplete { needs_reboot: true }));
        assert_eq!(reboot_preflight_calls.get(), 1);
        assert!(widgets.notification_sent.get());
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("done"));
        assert!(widgets.done_reboot_clamp.get_visible());
        assert!(widgets.done_reboot_detail.label().contains("kernel-core"));
        assert!(widgets.done_reboot_btn.has_css_class("suggested-action"));
        assert_eq!(widgets.done_queue.total_count(), 2);

        // Simulated reboot pre-flight blocker surfaces cleanly on done_reboot_detail.
        *widgets.reboot_blocker_detail.borrow_mut() = Some(
            "Restart is currently blocked by Steam (Finishing cloud sync). Wait for it to finish, then try again."
                .into(),
        );
        super::render(&state.borrow(), &console.borrow(), &widgets);
        assert!(widgets.done_reboot_detail.label().contains("Steam"));
        assert!(widgets
            .done_reboot_detail
            .label()
            .contains("Finishing cloud sync"));

        // Stale refresh replay (then_apply: true) invokes auto-apply callback cleanly on CheckComplete
        // without emitting a premature Ready notification.
        super::withdraw_completion_notification(&widgets);
        super::dispatch(&state, &console, &widgets, Event::ResetToIdle, false);
        assert!(widgets.reboot_blocker_detail.borrow().is_none());
        super::dispatch(&state, &console, &widgets, Event::StartCheck { then_apply: true }, false);
        feed(WorkerMsg::SessionStarted);
        feed(WorkerMsg::Event(Event::CheckComplete {
            packages: vec![dnf_pkg],
            soft_errors: vec![],
            listed_at: SystemTime::UNIX_EPOCH,
        }));
        assert_eq!(auto_apply_calls.get(), 1);
        assert_eq!(release_lookup_calls.get(), 2);
        assert!(!widgets.notification_sent.get());

        gtk::prelude::GtkWindowExt::destroy(&widgets.window);
    }

    fn verify_observatory_shell_theme_and_charts() {
        use super::super::sidebar::SourceFilter;
        use fedora_updater::console::ConsoleBuffer;
        use fedora_updater::model::{AuthPurpose, IdleScreen, Package, RetryAction, RunPass, UpdateSource};
        use fedora_updater::state::{AppState, Phase, RunningState};
        use gtk::prelude::*;
        use std::cell::Cell;
        use std::rc::Rc;

        // 1) All CSS rules in style.css parse with zero errors under GTK4.
        let parse_errors = Rc::new(Cell::new(0usize));
        let provider = gtk::CssProvider::new();
        {
            let parse_errors = parse_errors.clone();
            provider.connect_parsing_error(move |_, section, err| {
                parse_errors.set(parse_errors.get() + 1);
                eprintln!("CSS parse error at {:?}: {err}", section.start_location());
            });
        }
        provider.load_from_string(include_str!("../style.css"));
        assert_eq!(parse_errors.get(), 0, "style.css must parse cleanly in GTK4");

        // 2) StyleManager notify::dark toggles `.is-dark` on ApplicationWindow.
        let widgets = super::Widgets::for_test();
        let state = std::rc::Rc::new(std::cell::RefCell::new(AppState::default()));
        let console_cell = std::rc::Rc::new(std::cell::RefCell::new(ConsoleBuffer::new(100)));
        super::wire_observatory_views(&widgets, &state, &console_cell);
        super::sync_shell_layout(&widgets);
        let style_mgr = libadwaita::StyleManager::default();
        let initial_scheme = style_mgr.color_scheme();
        style_mgr.set_color_scheme(libadwaita::ColorScheme::ForceDark);
        assert!(widgets.window.has_css_class("is-dark"));
        style_mgr.set_color_scheme(libadwaita::ColorScheme::ForceLight);
        assert!(!widgets.window.has_css_class("is-dark"));
        style_mgr.set_color_scheme(initial_scheme);

        let console = ConsoleBuffer::new(100);

        // 3) Dual-mode Workbench <-> Focused Lumen collapse & upgrade card placement.
        let dnf_pkg = Package::new_dnf("firefox", "x86_64", "140.0", "updates");
        let mut fp_pkg = Package::new_dnf("Thunderbird", "", "128.2.0", "flathub");
        fp_pkg.source = UpdateSource::FlatpakUser;
        let mut fw_pkg = Package::new_dnf("UEFI dbx", "", "20241101", "fwupd");
        fw_pkg.source = UpdateSource::Firmware;
        let packages = vec![dnf_pkg.clone(), fp_pkg.clone(), fw_pkg.clone()];

        let idle_upgrade = AppState {
            phase: Phase::Idle(IdleScreen::Fresh),
            release_upgrade: Some(45),
            release_age_days: Some(7),
            ..Default::default()
        };
        super::render(&idle_upgrade, &console, &widgets);
        assert!(!widgets.split_view.is_collapsed());
        assert!(widgets.sidebar_toggle.is_active());
        assert!(widgets.sidebar.upgrade_card().root().get_visible());
        assert!(!widgets.idle_upgrade.root().get_visible());
        assert_eq!(
            widgets.detail_toolbar.top_bar_style(),
            libadwaita::ToolbarStyle::Flat
        );

        // Toggle into Focused "Lumen" mode.
        widgets.sidebar_toggle.set_active(false);
        assert!(!widgets.sidebar_open.get());
        assert!(widgets.split_view.is_collapsed());
        assert!(widgets.split_view.shows_content());
        assert!(!widgets.sidebar.upgrade_card().root().get_visible());
        assert!(widgets.idle_upgrade.root().get_visible());
        assert!(!widgets.ready_upgrade.root().get_visible());
        assert!(!widgets.done_upgrade.root().get_visible());

        // Lumen mode surfaces inline upgrade card inside Ready and Done clamps too.
        let ready_upgrade = AppState {
            phase: Phase::Ready {
                packages: packages.clone(),
                soft_errors: vec![],
            },
            release_upgrade: Some(45),
            ..Default::default()
        };
        super::render(&ready_upgrade, &console, &widgets);
        assert_eq!(
            widgets.detail_toolbar.top_bar_style(),
            libadwaita::ToolbarStyle::Raised
        );
        assert_eq!(
            widgets.detail_toolbar.bottom_bar_style(),
            libadwaita::ToolbarStyle::Raised
        );
        assert_eq!(
            widgets.ready_toolbar.bottom_bar_style(),
            libadwaita::ToolbarStyle::Raised
        );
        assert!(widgets.ready_upgrade.root().get_visible());
        assert!(!widgets.idle_upgrade.root().get_visible());
        assert_eq!(
            widgets.sidebar.count_texts(),
            ("3".into(), "1".into(), "1".into(), "1".into())
        );

        // Phase-2 controls must remain hidden in Phase 1.
        assert!(!widgets.sidebar.view_history_row().get_visible());
        assert!(!widgets.sidebar.sparkline_card().get_visible());
        assert!(!widgets.sidebar.sparkline().widget().get_visible());

        let done_upgrade = AppState {
            phase: Phase::Done {
                packages: packages.clone(),
                duration: "1m".into(),
                needs_reboot: false,
                failed: 0,
                failure_reasons: vec![],
            },
            release_upgrade: Some(45),
            ..Default::default()
        };
        super::render(&done_upgrade, &console, &widgets);
        assert_eq!(
            widgets.done_toolbar.bottom_bar_style(),
            libadwaita::ToolbarStyle::Raised
        );
        assert!(widgets.done_upgrade.root().get_visible());

        // Authenticating & Running hide BOTH sidebar and inline upgrade cards in both modes.
        let auth_upgrade = AppState {
            phase: Phase::Authenticating {
                purpose: AuthPurpose::Apply,
                packages: packages.clone(),
                last_checked: None,
            },
            release_upgrade: Some(45),
            ..Default::default()
        };
        let running_upgrade = AppState {
            phase: Phase::Running(RunningState {
                packages: packages.clone(),
                active_index: Some(0),
                overall_progress: 0.4,
                phase_label: "Installing".into(),
                current_source: Some(UpdateSource::Dnf),
                current_name: Some("firefox".into()),
                work_progress: Some(0.72),
                pass: RunPass::Apply,
                stage: None,
                needs_reboot: false,
                failed_sources: vec![],
                failure_reasons: vec![],
            }),
            release_upgrade: Some(45),
            ..Default::default()
        };
        for st in [&auth_upgrade, &running_upgrade] {
            super::render(st, &console, &widgets);
            assert!(!widgets.sidebar.upgrade_card().root().get_visible());
            assert!(!widgets.idle_upgrade.root().get_visible());
            assert!(!widgets.ready_upgrade.root().get_visible());
            assert!(!widgets.done_upgrade.root().get_visible());
        }

        // ProgressRing reflects Running item fraction and clears outside Running.
        assert!((widgets.progress_ring.displayed_fraction() - 0.72).abs() < 1e-6);
        assert!((widgets.progress_ring.target_fraction() - 0.72).abs() < 1e-6);
        assert_eq!(widgets.progress_ring.label_text(), "72%");

        let failed_state = AppState {
            phase: Phase::Failed {
                title: "Err".into(),
                detail: "Detail".into(),
                retry: RetryAction::CheckAgain,
            },
            release_upgrade: Some(45),
            ..Default::default()
        };
        super::render(&failed_state, &console, &widgets);
        assert_eq!(
            widgets.detail_toolbar.top_bar_style(),
            libadwaita::ToolbarStyle::Flat
        );
        assert_eq!(widgets.sidebar.pulse_headline_text(), "Err");
        assert_eq!(widgets.progress_ring.displayed_fraction(), 0.0);
        assert_eq!(widgets.progress_ring.label_text(), "");

        // 4) Compact (<= 720px) + Narrow (<= 500px) breakpoint precedence preserves collapse.
        widgets.sidebar_toggle.set_active(true);
        assert!(widgets.sidebar_open.get());
        assert!(!widgets.split_view.is_collapsed());
        assert!(!widgets.inspector_split.is_collapsed());

        widgets.compact.set(true);
        super::sync_shell_layout(&widgets);
        assert!(widgets.split_view.is_collapsed());
        assert!(widgets.inspector_split.is_collapsed());

        // Simulates libadwaita unapplying compact_breakpoint when narrow_breakpoint activates.
        widgets.compact.set(false);
        widgets.narrow.set(true);
        super::sync_shell_layout(&widgets);
        assert!(super::is_compact_viewport(&widgets));
        assert!(widgets.split_view.is_collapsed());
        assert!(widgets.inspector_split.is_collapsed());

        // Widening back restores Workbench mode since sidebar_open remained true.
        widgets.narrow.set(false);
        super::sync_shell_layout(&widgets);
        assert!(!widgets.split_view.is_collapsed());
        assert!(!widgets.inspector_split.is_collapsed());

        // 5) Sidebar source navigation and sparkline scaffold API work cleanly.
        if let Some(apps_row) = widgets.sidebar.sources_list().row_at_index(3) {
            apps_row.activate();
            assert_eq!(widgets.sidebar.selected_source(), SourceFilter::Apps);
        }
        widgets
            .sidebar
            .sparkline()
            .push_sample(12.5 * 1024.0 * 1024.0, Some((100, 200)));
        widgets.sidebar.sparkline().clear();

        gtk::prelude::GtkWindowExt::destroy(&widgets.window);
    }

    fn verify_queue_inspector_and_activity_log() {
        use super::super::activity_log::LogSourceFilter;
        use super::super::queue_list::{QueueList, QueueRowMode};
        use super::super::sidebar::SourceFilter;
        use fedora_updater::console::ConsoleBuffer;
        use fedora_updater::model::{
            AdvisoryKind, DetailView, Package, PackageAdvisory, PackageStatus, UpdateSource,
        };
        use fedora_updater::state::{AppState, Phase, RunningState};
        use gtk::prelude::*;
        use std::cell::RefCell;
        use std::rc::Rc;

        let widgets = super::Widgets::for_test();
        let state = Rc::new(RefCell::new(AppState::default()));
        let console = Rc::new(RefCell::new(ConsoleBuffer::new(8_000)));
        super::wire_observatory_views(&widgets, &state, &console);
        super::sync_shell_layout(&widgets);

        // Build representative Fedora packages: kernel (reboot), multi-arch mesa twins,
        // Flatpak with NVIDIA extensions, and firmware without arch/size.
        let mut kernel = Package::new_dnf("kernel", "x86_64", "6.14.11-300.fc44", "updates");
        kernel.old_version = Some("6.14.9-300.fc44".into());
        kernel.size_bytes = Some(98 * 1024 * 1024);

        let mut mesa_32 = Package::new_dnf("mesa-dri-drivers", "i686", "25.0.7", "updates");
        mesa_32.size_bytes = Some(24 * 1024 * 1024);
        let mut mesa_64 = Package::new_dnf("mesa-dri-drivers", "x86_64", "25.0.7", "updates");
        mesa_64.size_bytes = Some(26 * 1024 * 1024);

        let mut thunderbird = Package::new_dnf("Thunderbird", "x86_64", "128.2.0", "flathub");
        thunderbird.id = "flatpak:user:app/org.mozilla.Thunderbird/x86_64/stable".into();
        thunderbird.source = UpdateSource::FlatpakUser;
        thunderbird.size_bytes = Some(94 * 1024 * 1024);
        thunderbird.summary = Some("Manage emails, news feeds, and calendars".into());
        thunderbird.detail =
            "128.2.0 · flathub · user · installs org.freedesktop.Platform.GL.nvidia-570-133-07"
                .into();

        let mut uefi = Package::new_dnf("UEFI dbx", "", "20241101", "LVFS");
        uefi.id = "fwupd:uefi-dbx".into();
        uefi.source = UpdateSource::Firmware;

        // 1) Row widget layout guarantees: 1 source badge, zero trailing duplicate, zero `· System`/`· Apps`, hidden Phase-2 CheckButton.
        let fw_row = QueueList::build_row_for_test(&uefi, QueueRowMode::Review, false, None);
        assert!(!fw_row.check_btn().get_visible());
        assert_eq!(fw_row.source_badge().label().as_str(), "FW");
        assert!(!fw_row.trailing_label().get_visible());
        assert!(!fw_row.subtitle_label().label().contains("· Firmware"));
        assert!(!fw_row.sec_tag().get_visible());
        assert!(!fw_row.reboot_tag().get_visible());

        let kernel_row = QueueList::build_row_for_test(&kernel, QueueRowMode::Review, false, None);
        assert_eq!(kernel_row.name_label().label().as_str(), "kernel");
        assert_eq!(kernel_row.source_badge().label().as_str(), "DNF");
        assert!(kernel_row.reboot_tag().get_visible());
        assert_eq!(kernel_row.reboot_tag().label().as_str(), "Restart");
        assert!(!kernel_row.sec_tag().get_visible());
        assert_eq!(
            kernel_row.subtitle_label().label().as_str(),
            "6.14.9-300.fc44 → 6.14.11-300.fc44"
        );
        assert_eq!(kernel_row.trailing_label().label().as_str(), "98 MB");

        let fp_row = QueueList::build_row_for_test(&thunderbird, QueueRowMode::Review, false, None);
        assert_eq!(fp_row.source_badge().label().as_str(), "FP");
        assert!(fp_row.ext_tag().get_visible());
        assert_eq!(
            fp_row.ext_tag().label().as_str(),
            "+ GL.nvidia-570-133-07"
        );
        assert!(!fp_row.subtitle_label().label().contains("installs"));
        assert!(!fp_row.subtitle_label().label().contains("Apps"));

        // Verify optional Phase-2 CVE micro-tag lights up only when populated.
        let mut with_cve = kernel.clone();
        with_cve.advisories.push(PackageAdvisory {
            id: "FEDORA-2026-1".into(),
            kind: AdvisoryKind::Security,
            severity: Some("Important".into()),
            cves: vec!["CVE-2026-9999".into()],
            bugzillas: vec![],
            description: None,
        });
        let cve_row = QueueList::build_row_for_test(&with_cve, QueueRowMode::Review, false, None);
        assert!(cve_row.sec_tag().get_visible());
        assert_eq!(cve_row.sec_tag().label().as_str(), "CVE-2026-9999");

        // Running row highlights current active row with live progress.
        let mut kernel_dl = kernel.clone();
        kernel_dl.status = PackageStatus::Downloading;
        let run_row =
            QueueList::build_row_for_test(&kernel_dl, QueueRowMode::Running, true, Some(0.65));
        assert!(run_row.root().has_css_class("active"));
        assert_eq!(
            run_row.trailing_label().label().as_str(),
            "Downloading · 65%"
        );

        // 2) Render Ready state and verify in-place `PackageObject` reuse + multi-arch twin distinction.
        *state.borrow_mut() = AppState {
            phase: Phase::Ready {
                packages: vec![
                    kernel.clone(),
                    mesa_32.clone(),
                    mesa_64.clone(),
                    thunderbird.clone(),
                    uefi.clone(),
                ],
                soft_errors: vec![],
            },
            ..Default::default()
        };
        super::render(&state.borrow(), &console.borrow(), &widgets);
        assert!(!widgets.ready_split_update.get_visible());
        assert_eq!(widgets.ready_queue.total_count(), 5);
        assert_eq!(
            widgets.sidebar.count_texts(),
            ("5".into(), "3".into(), "1".into(), "1".into())
        );

        let kernel_ptr_before = widgets
            .ready_queue
            .find_object(&kernel.id)
            .expect("kernel object")
            .as_ptr();
        let mesa32_obj = widgets
            .ready_queue
            .find_object(&mesa_32.id)
            .expect("i686 twin");
        let mesa64_obj = widgets
            .ready_queue
            .find_object(&mesa_64.id)
            .expect("x86_64 twin");
        assert_ne!(mesa32_obj.id(), mesa64_obj.id());

        // Sync again in-place with updated size -> identical PackageObject GObject pointer!
        let mut kernel_updated = kernel.clone();
        kernel_updated.size_bytes = Some(102 * 1024 * 1024);
        widgets
            .ready_queue
            .sync(&[kernel_updated, mesa_32, mesa_64, thunderbird.clone(), uefi.clone()], None, None);
        let kernel_ptr_after = widgets
            .ready_queue
            .find_object(&kernel.id)
            .expect("kernel object after sync")
            .as_ptr();
        assert_eq!(kernel_ptr_before, kernel_ptr_after);

        // 3) Sidebar source filter + Gtk.SearchEntry filtering + empty filter clear.
        widgets
            .sidebar
            .sources_list()
            .row_at_index(3)
            .expect("Apps row")
            .activate();
        assert_eq!(widgets.sidebar.selected_source(), SourceFilter::Apps);
        assert_eq!(
            widgets.ready_queue.filtered_ids(),
            vec![thunderbird.id.clone()]
        );
        assert_eq!(widgets.ready_filter_status.label().as_str(), "1 of 5");
        assert!(widgets.ready_clear_filter.get_visible());
        assert_eq!(
            widgets.ready_list_stack.visible_child_name().as_deref(),
            Some("list")
        );

        widgets.ready_search.set_text("no-such-package");
        assert_eq!(
            widgets.ready_list_stack.visible_child_name().as_deref(),
            Some("empty-filter")
        );
        widgets.ready_empty_clear_btn.emit_clicked();
        assert_eq!(widgets.sidebar.selected_source(), SourceFilter::All);
        assert_eq!(widgets.ready_search.text().as_str(), "");
        assert_eq!(widgets.ready_queue.filtered_count(), 5);
        assert_eq!(
            widgets.ready_list_stack.visible_child_name().as_deref(),
            Some("list")
        );

        // 4) OverlaySplitView Package Inspector: open on activate, toggle off on re-click,
        // show reboot & extension sections cleanly, sync live during Running, close on button.
        assert!(!widgets.inspector_split.shows_sidebar());
        widgets.ready_queue.activate_filtered(0);
        assert!(widgets.inspector_split.shows_sidebar());
        assert_eq!(widgets.inspector.name_text(), "kernel");
        assert_eq!(widgets.inspector.badge_text(), "DNF");
        assert_eq!(widgets.inspector.repo_value().as_deref(), Some("updates"));
        assert_eq!(widgets.inspector.arch_value().as_deref(), Some("x86_64"));
        assert_eq!(
            widgets.inspector.old_version_value().as_deref(),
            Some("6.14.9-300.fc44")
        );
        assert_eq!(widgets.inspector.version_value(), "6.14.11-300.fc44");
        assert_eq!(widgets.inspector.size_value().as_deref(), Some("102 MB"));
        assert!(widgets.inspector.reboot_card_visible());
        assert!(!widgets.inspector.extensions_card_visible());
        assert!(!widgets.inspector.summary_visible());
        assert!(!widgets.inspector.advisories_card_visible());

        // Inspector shows Power Safety Warning card for battery-blocked firmware and hides it otherwise.
        assert!(!widgets.inspector.warning_card_visible());
        let mut warn_fw = uefi.clone();
        warn_fw.preflight_warning = Some(
            "System Firmware requires AC power to install safely. Connect AC power and try again."
                .into(),
        );
        widgets.inspector.paint(&warn_fw);
        assert!(widgets.inspector.warning_card_visible());
        assert!(widgets.inspector.warning_body_text().contains("AC power"));

        // Re-activating active row closes Inspector.
        widgets.ready_queue.activate_filtered(0);
        assert!(!widgets.inspector_split.shows_sidebar());

        // Activate Flatpak Thunderbird (index 3 in All) -> AppStream summary & extension breakdown shown cleanly.
        widgets.ready_queue.activate_filtered(3);
        assert!(widgets.inspector_split.shows_sidebar());
        assert_eq!(widgets.inspector.name_text(), "Thunderbird");
        assert!(widgets.inspector.summary_visible());
        assert_eq!(
            widgets.inspector.summary_text(),
            "Manage emails, news feeds, and calendars"
        );
        assert!(widgets.inspector.extensions_card_visible());
        assert_eq!(
            widgets.inspector.extension_items(),
            vec!["org.freedesktop.Platform.GL.nvidia-570-133-07"]
        );
        assert!(!widgets.inspector.reboot_card_visible());

        // Transitioning into Running keeps Inspector synced on Thunderbird if present, and
        // close button dismisses it cleanly.
        let mut fp_running = thunderbird.clone();
        fp_running.status = PackageStatus::Installing;
        *state.borrow_mut() = AppState {
            phase: Phase::Running(RunningState {
                packages: vec![fp_running.clone(), uefi.clone()],
                active_index: Some(0),
                overall_progress: 0.5,
                phase_label: "Installing".into(),
                current_source: Some(UpdateSource::FlatpakUser),
                current_name: Some("Thunderbird".into()),
                work_progress: Some(0.5),
                pass: fedora_updater::model::RunPass::Apply,
                stage: None,
                needs_reboot: false,
                failed_sources: vec![],
                failure_reasons: vec![],
            }),
            ..Default::default()
        };
        super::render(&state.borrow(), &console.borrow(), &widgets);
        assert!(widgets.inspector_split.shows_sidebar());
        assert_eq!(widgets.run_queue.selected_id().as_deref(), Some(thunderbird.id.as_str()));
        widgets.inspector.close_btn().emit_clicked();
        assert!(!widgets.inspector_split.shows_sidebar());
        assert!(widgets.run_queue.selected_id().is_none());

        // 5) Embedded Activity Log over ConsoleBuffer with \r redraw & ToggleGroup + search filtering.
        super::record_console_output(&console, &widgets, "dnf", "Repositories loaded.", false);
        super::record_console_output(&console, &widgets, "dnf", "[1/2] kernel 25%", true);
        super::record_console_output(&console, &widgets, "dnf", "[1/2] kernel 100%", false);
        super::record_console_output(&console, &widgets, "flatpak-user", "Thunderbird GL.nvidia", false);
        super::record_console_output(&console, &widgets, "fwupd", "UEFI dbx ok", false);

        widgets
            .sidebar
            .views_list()
            .row_at_index(2)
            .expect("Activity Log row")
            .activate();
        assert_eq!(widgets.detail_view.get(), DetailView::ActivityLog);
        assert_eq!(
            widgets.detail_view_stack.visible_child_name().as_deref(),
            Some("activity-log")
        );
        assert_eq!(
            widgets.sidebar.selected_view_name().as_deref(),
            Some("view:log")
        );
        assert_eq!(widgets.window_title.title().as_str(), "Activity Log");
        assert!(widgets.back_to_updates_btn.get_visible());
        assert!(!widgets.console_sheet.reveals_bottom_bar());
        assert!(widgets.activity_log.copy_btn().get_sensitive());
        assert_eq!(widgets.activity_log.active_filter(), LogSourceFilter::All);
        assert!(!widgets.activity_log.current_text().contains("25%"));
        assert!(widgets.activity_log.current_text().contains("100%"));

        // Source ToggleGroup + search filtering.
        widgets
            .activity_log
            .source_toggles()
            .set_active_name(Some("flatpak"));
        assert_eq!(widgets.activity_log.active_filter(), LogSourceFilter::Flatpak);
        assert_eq!(
            widgets.activity_log.current_text(),
            "[flatpak-user] Thunderbird GL.nvidia"
        );
        widgets.activity_log.search_entry().set_text("no-match");
        assert_eq!(
            widgets.activity_log.content_stack().visible_child_name().as_deref(),
            Some("empty")
        );
        assert!(!widgets.activity_log.copy_btn().get_sensitive());

        // Reset Activity Log filter & append live output; background Done render must NOT clobber ActivityLog view.
        widgets.activity_log.search_entry().set_text("");
        widgets
            .activity_log
            .source_toggles()
            .set_active_name(Some("all"));
        super::record_console_output(&console, &widgets, "fwupd", "Done fwupd", false);
        assert!(widgets.activity_log.current_text().contains("Done fwupd"));

        *state.borrow_mut() = AppState {
            phase: Phase::Done {
                packages: vec![uefi],
                duration: "12s".into(),
                needs_reboot: false,
                failed: 0,
                failure_reasons: vec![],
            },
            ..Default::default()
        };
        super::render(&state.borrow(), &console.borrow(), &widgets);
        assert_eq!(widgets.detail_view.get(), DetailView::ActivityLog);
        assert_eq!(widgets.window_title.title().as_str(), "Activity Log");

        // Returning to Updates restores active phase title & hides back button.
        widgets.back_to_updates_btn.emit_clicked();
        assert_eq!(widgets.detail_view.get(), DetailView::Updates);
        assert_eq!(
            widgets.detail_view_stack.visible_child_name().as_deref(),
            Some("updates")
        );
        assert!(!widgets.back_to_updates_btn.get_visible());
        assert_eq!(
            widgets.sidebar.selected_view_name().as_deref(),
            Some("view:updates")
        );
        assert_eq!(widgets.window_title.title().as_str(), "All Updates Applied");

        // `win.view-log` action (used by banners, header menu, and Warning/Failed pages) routes cleanly to ActivityLog.
        assert!(WidgetExt::activate_action(&widgets.window, "win.view-log", None).is_ok());
        assert_eq!(widgets.detail_view.get(), DetailView::ActivityLog);
        assert_eq!(
            widgets.detail_view_stack.visible_child_name().as_deref(),
            Some("activity-log")
        );

        gtk::prelude::GtkWindowExt::destroy(&widgets.window);
    }

    fn verify_preview_presets_render_cleanly() {
        use fedora_updater::console::ConsoleBuffer;
        use fedora_updater::model::DetailView;
        use gtk::prelude::*;
        use libadwaita::prelude::*;

        let presets = [
            "idle",
            "idle-current",
            "idle-warning",
            "auth",
            "checking",
            "ready",
            "ready-native",
            "ready-lumen",
            "ready-warn",
            "running",
            "running-expanded",
            "running-sparkline",
            "done",
            "done-reboot",
            "done-partial",
            "failed",
            "log",
            "history",
        ];

        for preset in presets {
            let widgets = super::Widgets::for_test();
            super::wire_expander(&widgets);
            let mut console = ConsoleBuffer::new(100);
            console.push_committed_tagged("dnf", "Repositories loaded.");
            console.push_committed_tagged("flatpak-user", "Syncing flathub");
            console.push_committed_tagged("fwupd", "UEFI dbx");

            let cfg = super::super::preview::config_for(preset);
            super::apply_preview_config(&cfg, &widgets);
            super::sync_shell_layout(&widgets);
            super::render(&cfg.state, &console, &widgets);

            match preset {
                "ready-native" => {
                    assert!(widgets.native_dnf_available.get());
                    assert!(widgets.ready_split_update.get_visible());
                    assert!(widgets.ready_selection_bar.get_visible());
                    assert!(!widgets.ready_update.get_visible());
                    assert!(widgets.sidebar.row_security().get_visible());
                    assert!(widgets.sidebar.view_history_row().get_visible());
                }
                "ready-lumen" => {
                    assert!(!widgets.sidebar_open.get());
                    assert!(widgets.split_view.is_collapsed());
                    assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("ready"));
                }
                "running-expanded" => {
                    assert!(widgets.run_completed.is_expanded());
                    assert!(widgets.run_list_scroll.get_visible());
                }
                "running-sparkline" => {
                    assert!(widgets.sidebar.sparkline_card().get_visible());
                    assert!(widgets.sidebar.sparkline().widget().get_visible());
                }
                "done" => {
                    assert!(widgets.done_summary_clamp.get_visible());
                    assert!(!widgets.done_reboot_clamp.get_visible());
                    assert!(widgets.done_btn.has_css_class("suggested-action"));
                }
                "done-reboot" => {
                    assert!(!widgets.done_summary_clamp.get_visible());
                    assert!(widgets.done_reboot_clamp.get_visible());
                    assert!(widgets.done_reboot_btn.has_css_class("suggested-action"));
                }
                "done-partial" => {
                    assert!(widgets.done_problem_banner.is_revealed());
                    assert!(widgets.done_reboot_clamp.get_visible());
                    assert!(widgets.done_retry_btn.get_visible());
                }
                "log" => {
                    assert_eq!(widgets.detail_view.get(), DetailView::ActivityLog);
                    assert_eq!(
                        widgets.detail_view_stack.visible_child_name().as_deref(),
                        Some("activity-log")
                    );
                }
                "history" => {
                    assert_eq!(widgets.detail_view.get(), DetailView::History);
                    assert_eq!(
                        widgets.detail_view_stack.visible_child_name().as_deref(),
                        Some("history")
                    );
                    assert!(widgets.history_view.counts().1 > 0);
                }
                _ => {}
            }

            gtk::prelude::GtkWindowExt::destroy(&widgets.window);
        }
    }

    fn verify_phase2_native_controls_and_history() {
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;

        use fedora_updater::backend::dnf_dbus::{
            HistoryEntry, HistoryPackageChange, HistorySource,
        };
        use fedora_updater::console::ConsoleBuffer;
        use fedora_updater::model::{
            AdvisoryKind, ApplyMode, DetailView, Package, PackageAdvisory, PackageStatus,
            StageCount, UpdateSource,
        };
        use fedora_updater::orchestrator::WorkerMsg;
        use fedora_updater::presenter::format_size_delta;
        use fedora_updater::state::{AppState, Event, Phase, RunningState};
        use gtk::prelude::*;

        use super::super::history::HistorySourceFilter;
        use super::super::queue_list::QueueList;
        use super::super::sidebar::SourceFilter;

        let widgets = super::Widgets::for_test();
        let state = Rc::new(RefCell::new(AppState::default()));
        let console = Rc::new(RefCell::new(ConsoleBuffer::new(200)));
        widgets.history_loaded.set(true);
        super::wire_observatory_views(&widgets, &state, &console);

        let mut openssl = Package::new_dnf("openssl", "x86_64", "3.2.2-3.fc44", "updates");
        openssl.old_version = Some("3.2.2-1.fc44".into());
        openssl.size_bytes = Some(2_457_600);
        openssl.install_size_delta = Some(400_000);
        openssl.summary = Some("Utilities from the general purpose cryptography library".into());
        openssl.advisories = vec![PackageAdvisory {
            id: "FEDORA-2026-84a1b2c3".into(),
            kind: AdvisoryKind::Security,
            severity: Some("Important".into()),
            cves: vec!["CVE-2026-2401".into()],
            bugzillas: vec!["RHBZ#2458001".into()],
            description: Some("Fixes TLS 1.3 buffer over-read.".into()),
        }];

        let mut kernel = Package::new_dnf("kernel-core", "x86_64", "6.14.12-300.fc44", "updates");
        kernel.old_version = Some("6.14.11-300.fc44".into());
        kernel.size_bytes = Some(18_874_368);

        let mut fp = Package::new_dnf("Thunderbird", "x86_64", "128.3.0", "flathub");
        fp.id = "flatpak:user:app/org.mozilla.Thunderbird/x86_64/stable".into();
        fp.source = UpdateSource::FlatpakUser;
        fp.size_bytes = Some(8_388_608);

        *state.borrow_mut() = AppState {
            phase: Phase::Ready {
                packages: vec![openssl.clone(), kernel.clone(), fp.clone()],
                soft_errors: vec![],
            },
            ..Default::default()
        };

        // 1) CLI fallback invariants (native capabilities disabled).
        super::render(&state.borrow(), &console.borrow(), &widgets);
        assert!(!widgets.ready_selection_bar.get_visible());
        assert!(!widgets.ready_split_update.get_visible());
        assert!(widgets.ready_update.get_visible());
        assert!(!widgets.ready_queue.is_selectable());
        assert!(!widgets.sidebar.view_history_row().get_visible());
        let fallback_row = QueueList::build_row_for_test(
            &openssl,
            super::super::queue_list::QueueRowMode::Review,
            false,
            None,
        );
        assert!(!fallback_row.check_btn().get_visible());

        // 2) Enable native dnf5daemon + fwupd D-Bus capabilities.
        widgets.native_dnf_available.set(true);
        widgets.native_fwupd_available.set(true);
        widgets.sidebar.set_history_available(true);
        super::render(&state.borrow(), &console.borrow(), &widgets);

        assert!(widgets.ready_selection_bar.get_visible());
        assert!(widgets.ready_split_update.get_visible());
        assert!(!widgets.ready_update.get_visible());
        assert!(widgets.ready_queue.is_selectable());
        assert!(widgets.sidebar.view_history_row().get_visible());
        assert!(widgets.sidebar.row_security().get_visible());
        assert_eq!(widgets.sidebar.count_security_text(), "1");

        let sel_row = QueueList::build_selectable_row_for_test(&openssl, true);
        assert!(sel_row.check_btn().get_visible());
        assert!(sel_row.check_btn().is_active());
        assert!(sel_row.sec_tag().get_visible());
        assert_eq!(sel_row.sec_tag().label().as_str(), "CVE-2026-2401");

        // 3) Selective checkboxes, indeterminate Toggle All Selected, empty selection gating, and SplitButton modes.
        assert!(widgets.ready_queue.set_selected_by_id(&kernel.id, false));
        assert!(widgets.ready_select_all.is_inconsistent());
        assert_eq!(widgets.ready_queue.selected_ids().len(), 2);
        assert!(widgets
            .ready_selection_summary
            .text()
            .as_str()
            .contains("2 of 3 selected"));
        assert!(widgets
            .ready_split_update
            .label()
            .as_deref()
            .unwrap_or("")
            .starts_with("Update 2 of 3"));

        widgets.ready_queue.set_all_selected(false);
        assert!(!widgets.ready_split_update.get_sensitive());
        widgets.ready_queue.set_all_selected(true);
        assert!(widgets.ready_split_update.get_sensitive());
        assert!(!widgets.ready_select_all.is_inconsistent());
        assert!(widgets.ready_select_all.is_active());

        for (mode_str, expected_mode, expected_prefix) in [
            ("offline", ApplyMode::OfflineOnReboot, "Prepare Offline Update"),
            ("download-only", ApplyMode::DownloadOnly, "Download Only"),
            ("live-reboot", ApplyMode::LiveAndReboot, "Update & Restart"),
            ("live", ApplyMode::Live, "Update All"),
        ] {
            assert!(WidgetExt::activate_action(
                &widgets.window,
                "win.apply-mode",
                Some(&mode_str.to_variant()),
            )
            .is_ok());
            assert_eq!(widgets.apply_mode.get(), expected_mode);
            let label = widgets
                .ready_split_update
                .label()
                .map(|g| g.to_string())
                .unwrap_or_default();
            assert!(
                label.starts_with(expected_prefix),
                "mode {mode_str}: expected prefix {expected_prefix}, got {label}"
            );
        }

        // 4) Sidebar Security Fixes filter + Inspector CVE / RHBZ / disk-delta cards.
        widgets.sidebar.row_security().activate();
        assert_eq!(widgets.sidebar.selected_source(), SourceFilter::Security);
        assert_eq!(widgets.ready_queue.filtered_count(), 1);
        widgets.ready_queue.activate_filtered(0);
        assert!(widgets.inspector_split.shows_sidebar());
        assert!(widgets.inspector.advisories_card_visible());
        let adv_text = widgets.inspector.advisory_summary_text();
        assert!(adv_text.contains("CVE-2026-2401"), "{adv_text}");
        assert!(adv_text.contains("RHBZ#2458001"), "{adv_text}");
        assert!(adv_text.contains("Important"), "{adv_text}");
        assert_eq!(
            widgets.inspector.disk_delta_value(),
            Some(format_size_delta(400_000))
        );

        // 5) ThroughputSparkline + ApplyMode::LiveAndReboot auto-reboot via handle_worker_with_reboot.
        *state.borrow_mut() = AppState {
            phase: Phase::Running(RunningState {
                packages: vec![Package {
                    status: PackageStatus::Downloading,
                    ..openssl.clone()
                }],
                active_index: Some(0),
                overall_progress: 0.25,
                phase_label: "Downloading packages".into(),
                current_source: Some(UpdateSource::Dnf),
                current_name: Some("openssl".into()),
                work_progress: Some(0.25),
                pass: fedora_updater::model::RunPass::Download,
                stage: Some(StageCount {
                    current: 1,
                    total: 4,
                }),
                needs_reboot: false,
                failed_sources: vec![],
                failure_reasons: vec![],
            }),
            apply_mode: ApplyMode::LiveAndReboot,
            ..Default::default()
        };
        super::render(&state.borrow(), &console.borrow(), &widgets);
        assert!(!widgets.sidebar.sparkline_card().get_visible());

        super::handle_worker_with_reboot(
            &state,
            &console,
            &widgets,
            WorkerMsg::Throughput {
                bytes_per_sec: 12_500_000.0,
                downloaded_bytes: 12_500_000,
                total_bytes: 50_000_000,
            },
            || {},
            || {},
            || {},
            || {},
        );
        assert!(widgets.sidebar.sparkline_card().get_visible());
        assert!(widgets.sidebar.sparkline().widget().get_visible());

        let auto_reboot_fired = Rc::new(Cell::new(false));
        let flag = auto_reboot_fired.clone();
        super::handle_worker_with_reboot(
            &state,
            &console,
            &widgets,
            WorkerMsg::Event(Event::ApplyComplete {
                needs_reboot: false,
            }),
            || {},
            || {},
            || {},
            move || flag.set(true),
        );
        assert!(auto_reboot_fired.get());

        // 6) Update History page (`DetailView::History`), source toggles & search filtering.
        widgets.history_view.set_entries(vec![
            HistoryEntry {
                id: "dnf-txn-203".into(),
                source: HistorySource::Dnf,
                timestamp: 1_795_224_552,
                title: "openssl + 1 more".into(),
                description: "dnf update -y".into(),
                status: "Ok".into(),
                succeeded: true,
                changes: vec![HistoryPackageChange {
                    name: "openssl".into(),
                    arch: "x86_64".into(),
                    evr: "3.2.2-3.fc44".into(),
                    original_evr: Some("3.2.2-1.fc44".into()),
                    action: "Upgraded".into(),
                }],
            },
            HistoryEntry {
                id: "fwupd-uefi-2026".into(),
                source: HistorySource::Firmware,
                timestamp: 1_795_150_000,
                title: "System Firmware".into(),
                description: "uefi_capsule".into(),
                status: "Restart Required".into(),
                succeeded: true,
                changes: vec![],
            },
        ]);

        assert!(WidgetExt::activate_action(&widgets.window, "win.view-history", None).is_ok());
        assert_eq!(widgets.detail_view.get(), DetailView::History);
        assert_eq!(
            widgets.detail_view_stack.visible_child_name().as_deref(),
            Some("history")
        );
        assert_eq!(
            widgets.history_view.visible_child_name().as_deref(),
            Some("list")
        );
        assert_eq!(widgets.history_view.counts(), (2, 2));
        assert!(widgets.history_view.refresh_btn().get_sensitive());

        widgets.history_view.select_source_for_test("dnf");
        assert_eq!(widgets.history_view.active_filter(), HistorySourceFilter::System);
        assert_eq!(widgets.history_view.counts(), (1, 2));
        widgets.history_view.select_source_for_test("fwupd");
        assert_eq!(widgets.history_view.active_filter(), HistorySourceFilter::Firmware);
        assert_eq!(widgets.history_view.counts(), (1, 2));
        widgets.history_view.select_source_for_test("all");
        assert_eq!(widgets.history_view.active_filter(), HistorySourceFilter::All);
        assert_eq!(widgets.history_view.counts(), (2, 2));

        widgets.history_view.set_search_for_test("no-such-package");
        assert_eq!(widgets.history_view.counts(), (0, 2));
        assert_eq!(
            widgets.history_view.visible_child_name().as_deref(),
            Some("empty")
        );
        widgets.history_view.set_search_for_test("openssl");
        assert_eq!(widgets.history_view.counts(), (1, 2));
        assert_eq!(
            widgets.history_view.visible_child_name().as_deref(),
            Some("list")
        );

        gtk::prelude::GtkWindowExt::destroy(&widgets.window);
    }

    /// Serialized on a single thread because GTK4/Libadwaita asserts main-thread ownership.
    #[test]
    fn gtk_widget_window_rows_and_dialogs_e2e() {
        if let Err(err) = libadwaita::init() {
            if std::env::var_os("FEDORA_UPDATER_REQUIRE_GTK").is_some()
                || std::env::var_os("GDK_BACKEND").is_some()
            {
                panic!("GTK/libadwaita initialization failed under required/explicit backend: {err}");
            }
            eprintln!(
                "note: skipping GTK widget E2E without an active display ({err}); run ./scripts/test-ui-headless.sh"
            );
            return;
        }

        verify_rows_widgets();
        verify_dialogs_widgets();
        verify_all_phases_narrow_and_recovery();
        verify_console_drawer_and_deltas();
        verify_handle_worker_full_replay();
        verify_observatory_shell_theme_and_charts();
        verify_queue_inspector_and_activity_log();
        verify_preview_presets_render_cleanly();
        verify_phase2_native_controls_and_history();
    }
}
