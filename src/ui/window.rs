//! Main application window — thin view over the pure state machine.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Builder, Button, Label, ProgressBar, Revealer, Stack, TextView, ToggleButton};
use libadwaita::prelude::*;
use libadwaita::{
    Application, ApplicationWindow, Banner, Breakpoint, ExpanderRow, NavigationSplitView,
    PreferencesGroup, Spinner, StatusPage, StyleManager, Toast, ToastOverlay, ToolbarStyle,
    ToolbarView, WindowTitle,
};

use fedora_updater::backend::dnf;
use fedora_updater::console::{ConsoleBuffer, ConsoleDelta};
use fedora_updater::model::{
    count_by_source, AuthPurpose, IdleScreen, Package, RetryAction, RunPass, UpdateSource,
};
use fedora_updater::orchestrator::{
    can_skip_auth_ui, cancel_background_work, release_privileges, run_apply_all, run_check_all,
    run_systemctl_reboot, session_started_event, WorkerMsg,
};
use fedora_updater::presenter::{
    about_time_left, cache_activity_note, check_progress, checking_footnote, done_header_subtitle,
    fail_retry_label, idle_chrome, now_playing_subtitle, now_playing_title, quiet_ui,
    ready_banner_title, run_heading, running_progress_captions, source_summary_line, window_header,
    HeaderView, QuietInput, AUTH_DESCRIPTION,
};
use fedora_updater::privilege::{
    dnf_daemon_is_running, kill_recorded_helper, kill_recorded_local_command,
};
use fedora_updater::state::{self, session_started_purpose, AppState, Event, Phase, RunningState};

use super::prefs::{DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH};
use super::{charts, dialogs, obj, prefs, preview, rows, sidebar};

const WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/window.ui"));

struct Widgets {
    window: ApplicationWindow,
    split_view: NavigationSplitView,
    detail_toolbar: ToolbarView,
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
    check_group: PreferencesGroup,
    check_list_scroll: gtk::ScrolledWindow,
    check_rows: RefCell<Vec<gtk::Widget>>,
    ready_banner: Banner,
    ready_group: PreferencesGroup,
    ready_rows: RefCell<Vec<gtk::Widget>>,
    ready_update: Button,
    run_stop: Button,
    run_stall: Banner,
    run_progress: ProgressBar,
    run_progress_meta: Label,
    run_progress_pct: Label,
    progress_ring: charts::ProgressRing,
    now_playing_badge: Label,
    now_playing_title: Label,
    now_playing_subtitle: Label,
    now_playing_size: Label,
    now_playing_pct: Label,
    now_playing_spinner: Spinner,
    run_list_group: PreferencesGroup,
    run_list_rows: RefCell<Vec<rows::ChecklistRow>>,
    run_list_ids: RefCell<Vec<String>>,
    run_completed_group: PreferencesGroup,
    run_completed: ExpanderRow,
    run_completed_rows: RefCell<Vec<gtk::Widget>>,
    run_completed_keys: RefCell<Vec<String>>,
    run_completed_user_open: Cell<bool>,
    ignore_expand: Cell<bool>,
    console_drawer: gtk::Box,
    console_toggle: Button,
    console_reveal: Revealer,
    console_chevron: gtk::Image,
    console_line: Label,
    console_full: TextView,
    done_btn: Button,
    done_retry_btn: Button,
    reboot_banner: Banner,
    done_problem_banner: Banner,
    done_installed_scroll: gtk::ScrolledWindow,
    done_installed: PreferencesGroup,
    done_installed_rows: RefCell<Vec<gtk::Widget>>,
    fail_page: StatusPage,
    fail_retry: Button,
    /// Last byte from the backend, including a carriage-return redraw.
    /// `None` until this run paints the Running page, so auth time is not quiet time.
    last_output: Cell<Option<Instant>>,
    cache_cursor: RefCell<Option<(PathBuf, u64)>>,
}

fn set_accessible_label(widget: &impl IsA<gtk::Accessible>, label: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

fn widgets_from_builder(builder: &Builder, window: ApplicationWindow) -> Rc<Widgets> {
    let sidebar_host: gtk::Box = obj(builder, "sidebar_host");
    let sidebar = sidebar::Sidebar::new();
    sidebar_host.append(sidebar.root());

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
    let header_refresh: Button = obj(builder, "header_refresh");
    let header_menu: gtk::MenuButton = obj(builder, "header_menu");
    set_accessible_label(&sidebar_toggle, "Toggle Sidebar");
    set_accessible_label(&sidebar_close_btn, "Return to Updates");
    set_accessible_label(&header_refresh, "Refresh");
    set_accessible_label(&header_menu, "Main Menu");

    bind_style_manager(&window);

    Rc::new(Widgets {
        window,
        split_view: obj(builder, "split_view"),
        detail_toolbar: obj(builder, "detail_toolbar"),
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
        check_group: obj(builder, "check_group"),
        check_list_scroll: obj(builder, "check_list_scroll"),
        check_rows: RefCell::new(Vec::new()),
        ready_banner: obj(builder, "ready_banner"),
        ready_group: obj(builder, "ready_group"),
        ready_rows: RefCell::new(Vec::new()),
        ready_update: obj(builder, "ready_update"),
        run_stop: obj(builder, "run_stop"),
        run_stall: obj(builder, "run_stall"),
        run_progress: obj(builder, "run_progress"),
        run_progress_meta: obj(builder, "run_progress_meta"),
        run_progress_pct: obj(builder, "run_progress_pct"),
        progress_ring,
        now_playing_badge: obj(builder, "now_playing_badge"),
        now_playing_title: obj(builder, "now_playing_title"),
        now_playing_subtitle: obj(builder, "now_playing_subtitle"),
        now_playing_size: obj(builder, "now_playing_size"),
        now_playing_pct: obj(builder, "now_playing_pct"),
        now_playing_spinner: obj(builder, "now_playing_spinner"),
        run_list_group: obj(builder, "run_list_group"),
        run_list_rows: RefCell::new(Vec::new()),
        run_list_ids: RefCell::new(Vec::new()),
        run_completed_group: obj(builder, "run_completed_group"),
        run_completed: obj(builder, "run_completed"),
        run_completed_rows: RefCell::new(Vec::new()),
        run_completed_keys: RefCell::new(Vec::new()),
        run_completed_user_open: Cell::new(false),
        ignore_expand: Cell::new(false),
        console_drawer: obj(builder, "console_drawer"),
        console_toggle: obj(builder, "console_toggle"),
        console_reveal: obj(builder, "console_reveal"),
        console_chevron: obj(builder, "console_chevron"),
        console_line: obj(builder, "console_line"),
        console_full: obj(builder, "console_full"),
        done_btn: obj(builder, "done_btn"),
        done_retry_btn: obj(builder, "done_retry_btn"),
        reboot_banner: obj(builder, "reboot_banner"),
        done_problem_banner: obj(builder, "done_problem_banner"),
        done_installed_scroll: obj(builder, "done_installed_scroll"),
        done_installed: obj(builder, "done_installed"),
        done_installed_rows: RefCell::new(Vec::new()),
        fail_page: obj(builder, "fail_page"),
        fail_retry: obj(builder, "fail_retry"),
        last_output: Cell::new(None),
        cache_cursor: RefCell::new(None),
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

pub fn build(app: &Application) {
    let state = Rc::new(RefCell::new(preview::initial_state().unwrap_or_default()));
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

    let about = gio::SimpleAction::new("about", None);
    {
        let window = window.clone();
        about.connect_activate(move |_, _| dialogs::present_about(&window));
    }
    window.add_action(&about);

    let view_log = gio::SimpleAction::new("view-log", None);
    {
        let window = window.clone();
        let console = console.clone();
        view_log.connect_activate(move |_, _| {
            dialogs::show_log_window(&window, &console.borrow().full_text());
        });
    }
    window.add_action(&view_log);

    let toggle_sidebar_action = gio::SimpleAction::new("toggle-sidebar", None);
    {
        let widgets = widgets.clone();
        toggle_sidebar_action.connect_activate(move |_, _| toggle_sidebar(&widgets));
    }
    window.add_action(&toggle_sidebar_action);
    app.set_accels_for_action("win.toggle-sidebar", &["F9", "<Primary>backslash"]);

    wire_breakpoints(&widgets, &builder);
    wire_shell_controls(&widgets);

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
            if state.borrow().update_list_needs_refresh(SystemTime::now()) {
                let state = state.clone();
                let console = console.clone();
                let widgets_for_cb = widgets.clone();
                let tx = tx.clone();
                dialogs::prompt_stale_update_list(&widgets.window, move || {
                    start_check(&state, &console, &widgets_for_cb, tx.clone(), true);
                });
                return;
            }
            begin_apply(&state, &console, &widgets, tx.clone());
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
        let tx = tx.clone();
        let widgets = widgets.clone();
        let banner = widgets.reboot_banner.clone();
        banner.connect_button_clicked(move |_| {
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
    spawn_release_lookup(&state, &console, &widgets);
    {
        let hooked = widgets.clone();
        glib::idle_add_local_once(move || {
            if hooked.focus_idle_check.get() {
                hooked.idle_btn.grab_focus();
            }
        });
    }
    maybe_open_preview_completed(&widgets);
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
            if keyval == gtk::gdk::Key::Escape
                && widgets.split_view.is_collapsed()
                && !widgets.split_view.shows_content()
            {
                widgets.split_view.set_show_content(true);
                sync_shell_layout(&widgets);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(key);
    }

    let close_compact_drawer = |w: &Rc<Widgets>| {
        if w.split_view.is_collapsed() {
            w.split_view.set_show_content(true);
            sync_shell_layout(w);
        }
    };

    let on_source = {
        let widgets = widgets.clone();
        move |_filter: sidebar::SourceFilter| {
            close_compact_drawer(&widgets);
        }
    };
    let on_updates = {
        let widgets = widgets.clone();
        move || {
            close_compact_drawer(&widgets);
        }
    };
    let on_activity_log = {
        let widgets = widgets.clone();
        move || {
            close_compact_drawer(&widgets);
            let _ = WidgetExt::activate_action(&widgets.window, "win.view-log", None);
        }
    };
    widgets
        .sidebar
        .wire_navigation(on_source, on_updates, on_activity_log);
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

fn start_check(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    tx: async_channel::Sender<WorkerMsg>,
    then_apply: bool,
) {
    clear_console(console, widgets);
    dispatch(state, console, widgets, Event::StartCheck { then_apply }, true);
    if can_skip_auth_ui() {
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

fn begin_apply(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    tx: async_channel::Sender<WorkerMsg>,
) {
    widgets.last_output.set(None);
    *widgets.cache_cursor.borrow_mut() = None;
    clear_console(console, widgets);
    let packages = state.borrow().packages().to_vec();
    dispatch(state, console, widgets, Event::StartApply, true);
    if can_skip_auth_ui() {
        dispatch(
            state,
            console,
            widgets,
            session_started_event(AuthPurpose::Apply),
            true,
        );
    }
    run_apply_all(tx, packages);
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

fn handle_worker(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    tx: &async_channel::Sender<WorkerMsg>,
    msg: WorkerMsg,
) {
    handle_worker_with(
        state,
        console,
        widgets,
        msg,
        || begin_apply(state, console, widgets, tx.clone()),
        || spawn_release_lookup(state, console, widgets),
    );
}

fn handle_worker_with(
    state: &Rc<RefCell<AppState>>,
    console: &Rc<RefCell<ConsoleBuffer>>,
    widgets: &Rc<Widgets>,
    msg: WorkerMsg,
    on_auto_apply: impl FnOnce(),
    on_release_lookup: impl FnOnce(),
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
        WorkerMsg::Event(ev) => ev,
    };

    let check_finished = matches!(ev, Event::CheckComplete { .. });
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
    }
    if check_finished {
        on_release_lookup();
    }
}

fn clear_console(console: &Rc<RefCell<ConsoleBuffer>>, widgets: &Widgets) {
    console.borrow_mut().clear();
    widgets.console_full.buffer().set_text("");
    widgets.console_line.set_text("Starting…");
}

fn sync_console_drawer(w: &Widgets, show: bool) {
    w.console_drawer.set_visible(show);
    if !show {
        w.console_reveal.set_reveal_child(false);
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
    if widgets.console_reveal.reveals_child() {
        apply_console_delta(&widgets.console_full.buffer(), delta, &last_line);
    }
}

fn append_console_line(buffer: &gtk::TextBuffer, line: &str) {
    let mut end = buffer.end_iter();
    if buffer.char_count() > 0 {
        buffer.insert(&mut end, "\n");
    }
    buffer.insert(&mut end, line);
}

fn apply_console_delta(buffer: &gtk::TextBuffer, delta: ConsoleDelta, line: &str) {
    match delta {
        ConsoleDelta::Appended => append_console_line(buffer, line),
        ConsoleDelta::ReplacedLast => {
            let mut end = buffer.end_iter();
            let mut start = end;
            start.set_line_offset(0);
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, line);
        }
        ConsoleDelta::TrimmedAndAppended => {
            let mut start = buffer.start_iter();
            let mut line1 = start;
            if line1.forward_line() {
                buffer.delete(&mut start, &mut line1);
            } else {
                let mut end = buffer.end_iter();
                buffer.delete(&mut start, &mut end);
            }
            append_console_line(buffer, line);
        }
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
    let reveal = widgets.console_reveal.clone();
    let chevron = widgets.console_chevron.clone();
    let full = widgets.console_full.clone();
    let console = console.clone();
    widgets.console_toggle.connect_clicked(move |_| {
        let open = !reveal.reveals_child();
        if open {
            let text = console.borrow().full_text();
            full.buffer().set_text(&text);
        }
        reveal.set_reveal_child(open);
        chevron.set_icon_name(Some(if open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
    });
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

fn maybe_open_preview_completed(w: &Widgets) {
    if std::env::var("FEDORA_UPDATER_PREVIEW").as_deref() != Ok("running-expanded") {
        return;
    }
    w.ignore_expand.set(true);
    w.run_completed_user_open.set(true);
    w.run_completed.set_enable_expansion(true);
    w.run_completed.set_expanded(true);
    w.ignore_expand.set(false);
}

fn finished_row(package: &Package) -> libadwaita::ActionRow {
    rows::finished_row(package, true)
}

fn restore_default_window_size(window: &ApplicationWindow) {
    if window.is_maximized() || window.is_fullscreen() {
        return;
    }
    let width = window.width();
    let height = window.height();
    if height > DEFAULT_WINDOW_HEIGHT + 24 {
        window.set_default_size(
            if width > 0 { width } else { DEFAULT_WINDOW_WIDTH },
            DEFAULT_WINDOW_HEIGHT,
        );
    }
}

fn clear_running_lists(w: &Widgets) {
    if w.run_list_rows.borrow().is_empty() && w.run_completed_rows.borrow().is_empty() {
        return;
    }
    for child in w.run_list_rows.borrow().iter() {
        w.run_list_group.remove(&child.row);
    }
    w.run_list_rows.borrow_mut().clear();
    w.run_list_ids.borrow_mut().clear();
    w.run_list_group.set_visible(true);
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

/// Footnote, spinner, and stall banner for a quiet run. Does not rebuild the list.
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
    paint_header(
        w,
        HeaderView::Running {
            heading: &w.window_title.title(),
            footnote: &foot,
        },
    );

    if quiet.show_spinner || running.work_progress.is_none() {
        w.now_playing_spinner.set_opacity(1.0);
    } else {
        w.now_playing_spinner.set_opacity(0.0);
    }

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
    if let Some(frac) = running.work_progress {
        let frac = frac.clamp(0.0, 1.0);
        w.now_playing_spinner.set_opacity(0.0);
        w.now_playing_pct.set_text(&format!("{:.0}%", frac * 100.0));
    } else {
        w.now_playing_spinner.set_opacity(1.0);
        w.now_playing_pct.set_text("");
    }

    let remaining: Vec<&Package> = packages.iter().filter(|p| !p.status.is_done()).collect();
    let rem_ids: Vec<String> = remaining.iter().map(|p| p.id.clone()).collect();
    if *w.run_list_ids.borrow() != rem_ids {
        for child in w.run_list_rows.borrow().iter() {
            w.run_list_group.remove(&child.row);
        }
        let built: Vec<rows::ChecklistRow> =
            remaining.iter().copied().map(rows::checklist_row).collect();
        for row in &built {
            w.run_list_group.add(&row.row);
        }
        *w.run_list_rows.borrow_mut() = built;
        *w.run_list_ids.borrow_mut() = rem_ids;
    }
    {
        let list = w.run_list_rows.borrow();
        for (p, row) in remaining.iter().zip(list.iter()) {
            let is_current = running
                .active_index
                .and_then(|i| packages.get(i))
                .is_some_and(|cur| cur.id == p.id && !cur.status.is_done());
            rows::sync_checklist_row(row, p, is_current);
        }
    }
    w.run_list_group.set_visible(!remaining.is_empty());

    let completed: Vec<&Package> = packages.iter().filter(|p| p.status.is_done()).collect();
    let completed_keys: Vec<String> = completed.iter().map(|p| p.id.clone()).collect();
    w.ignore_expand.set(true);
    let failed = completed
        .iter()
        .filter(|p| p.status == fedora_updater::PackageStatus::Failed)
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
    w.ignore_expand.set(false);

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
    let done_upgrade_visible = w.done_upgrade.root().get_visible();
    w.done_upgrade_slot.set_visible(done_upgrade_visible);
    if done_upgrade_visible {
        w.done_installed_scroll.set_visible(true);
    }
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
        matches!(state.phase, Phase::Checking { .. } | Phase::Running(_)),
    );
    if !matches!(state.phase, Phase::Running(_)) {
        w.progress_ring.set_progress(None, false);
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
            restore_default_window_size(&w.window);
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
            if matches!(screen, IdleScreen::Fresh) {
                arm_idle_check_focus(w);
            }
        }
        Phase::Authenticating { purpose, .. } => {
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
            w.check_list_scroll.set_visible(found);
            rows::refill_group(
                &w.check_group,
                &w.check_rows,
                packages_so_far.iter().map(|p| rows::package_row(p, false)),
            );
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
            w.ready_group.set_visible(!packages.is_empty());

            rows::refill_group(
                &w.ready_group,
                &w.ready_rows,
                packages.iter().map(|p| rows::package_row(p, false)),
            );
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
            restore_default_window_size(&w.window);
            w.stack
                .set_visible_child_full("done", gtk::StackTransitionType::None);
            w.stack.queue_allocate();
            let upgraded = packages
                .iter()
                .filter(|p| p.status == fedora_updater::PackageStatus::Completed)
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
            if *failed > 0 && !failure_reasons.is_empty() {
                w.done_problem_banner
                    .set_title(&failure_reasons.join(" · "));
                w.done_problem_banner.set_revealed(true);
            } else {
                w.done_problem_banner.set_revealed(false);
            }
            w.done_retry_btn.set_visible(*failed > 0);
            w.done_retry_btn.remove_css_class("suggested-action");
            w.done_btn.remove_css_class("suggested-action");
            w.done_btn.remove_css_class("pill");
            if *needs_reboot {
                w.reboot_banner
                    .set_button_style(libadwaita::BannerButtonStyle::Suggested);
                w.reboot_banner.set_revealed(true);
            } else {
                w.reboot_banner
                    .set_button_style(libadwaita::BannerButtonStyle::Default);
                w.reboot_banner.set_revealed(false);
                if *failed > 0 {
                    w.done_retry_btn.add_css_class("suggested-action");
                } else {
                    w.done_btn.add_css_class("suggested-action");
                }
            }
            w.done_installed
                .set_title(if *failed > 0 { "Finished" } else { "Installed" });
            w.done_installed.set_visible(!packages.is_empty());
            let mut listed: Vec<&fedora_updater::Package> = packages.iter().collect();
            listed.sort_by_key(|package| match package.status {
                fedora_updater::PackageStatus::Failed => 0,
                fedora_updater::PackageStatus::Completed => 1,
                _ => 2,
            });
            rows::refill_group(
                &w.done_installed,
                &w.done_installed_rows,
                listed
                    .into_iter()
                    .map(|package| rows::finished_row(package, false)),
            );
            w.done_installed_scroll
                .set_visible(!packages.is_empty() || w.done_upgrade_slot.get_visible());
        }
        Phase::Failed {
            title,
            detail,
            retry,
        } => {
            clear_running_lists(w);
            restore_default_window_size(&w.window);
            w.stack
                .set_visible_child_full("failed", gtk::StackTransitionType::None);
            paint_header(w, HeaderView::Failed);
            w.fail_page.set_title(title);
            w.fail_page.set_description(Some(detail));
            w.fail_retry.set_label(fail_retry_label(*retry));
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

        let installed_at = xml.find("id=\"done_installed\"").expect("done_installed");
        let done_btn_at = xml.find("id=\"done_btn\"").expect("done_btn");
        assert!(
            installed_at < done_btn_at,
            "installed list sits above the summary actions"
        );

        let stall_at = xml.find("id=\"run_stall\"").expect("run_stall");
        let card_at = xml.find("id=\"current_package\"").expect("current_package");
        let scroll_at = xml.find("id=\"run_list_scroll\"").expect("run_list_scroll");
        let list_at = xml.find("id=\"run_list_group\"").expect("run_list_group");
        let completed_at = xml.find("id=\"run_completed\"").expect("run_completed");
        assert!(
            stall_at < card_at
                && card_at < scroll_at
                && scroll_at < list_at
                && list_at < completed_at,
            "running page layout order: run_stall < current_package < run_list_scroll < run_list_group < run_completed"
        );

        let scroll_block = object_block(xml, "run_list_scroll");
        assert!(
            scroll_block.contains("propagate-natural-height"),
            "run_list_scroll must not propagate the package list height to the window"
        );
        assert!(scroll_block.contains("id=\"run_list_group\""));
        assert!(
            !scroll_block.contains("id=\"run_completed\""),
            "completed expander must not live inside the remaining-work scroll"
        );
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

        // NVIDIA extension Ready row: show_status = false shows byte size and 2-line wrapping.
        let mut steam = Package::new_dnf("Steam", "", "1.0.0.82", "flathub");
        steam.source = UpdateSource::FlatpakSystem;
        steam.size_bytes = Some(251 * 1024 * 1024);
        steam.detail =
            "1.0.0.81 → 1.0.0.82 · flathub (system) · installs org.freedesktop.Platform.GL32.nvidia-570-133-07".into();
        let ready_row = rows::package_row(&steam, false);
        assert_eq!(ready_row.title().as_str(), "Steam");
        assert!(ready_row.subtitle().unwrap().contains("nvidia-570-133-07"));
        assert_eq!(ready_row.subtitle_lines(), 2);
        let mut labels = Vec::new();
        collect_descendants::<gtk::Label>(&ready_row, &mut labels);
        assert!(labels.iter().any(|l| l.label().as_str() == "251 MB"));

        // Without size_bytes, show_status = false falls back to source badge suffix.
        let firefox = Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates");
        let fallback_row = rows::package_row(&firefox, false);
        let mut fallback_labels = Vec::new();
        collect_descendants::<gtk::Label>(&fallback_row, &mut fallback_labels);
        assert!(fallback_labels
            .iter()
            .filter(|l| l.has_css_class("dimmed"))
            .any(|l| l.label().as_str() == "DNF"));

        // show_status = true shows live status caption.
        let mut installing = firefox.clone();
        installing.status = PackageStatus::Installing;
        let status_row = rows::package_row(&installing, true);
        let mut status_labels = Vec::new();
        collect_descendants::<gtk::Label>(&status_row, &mut status_labels);
        assert!(status_labels
            .iter()
            .any(|l| l.label().as_str() == "installing" && l.has_css_class("phase-tag")));

        // checklist_row + sync_checklist_row CSS transitions across all 5 statuses.
        let mut p = Package::new_dnf("kernel-core", "x86_64", "7.1.12-200.fc44", "updates");
        let check_row = rows::checklist_row(&p);
        assert!(check_row.row.has_css_class("pending"));
        assert_eq!(check_row.status_label().label().as_str(), "queued");
        assert!(check_row.status_label().has_css_class("dimmed"));

        p.status = PackageStatus::Downloading;
        rows::sync_checklist_row(&check_row, &p, true);
        assert!(check_row.row.has_css_class("active"));
        assert!(!check_row.row.has_css_class("pending"));
        assert_eq!(check_row.status_label().label().as_str(), "downloading");
        assert!(check_row.status_label().has_css_class("accent"));
        assert!(check_row.status_label().has_css_class("phase-tag"));

        p.status = PackageStatus::Installing;
        rows::sync_checklist_row(&check_row, &p, false);
        assert!(!check_row.row.has_css_class("active"));
        assert_eq!(check_row.status_label().label().as_str(), "installing");
        assert!(check_row.status_label().has_css_class("accent"));

        p.status = PackageStatus::Completed;
        rows::sync_checklist_row(&check_row, &p, false);
        assert!(check_row.row.has_css_class("done"));
        assert_eq!(check_row.status_label().label().as_str(), "done");
        assert!(check_row.status_label().has_css_class("success"));
        assert!(!check_row.status_label().has_css_class("phase-tag"));

        p.status = PackageStatus::Failed;
        rows::sync_checklist_row(&check_row, &p, false);
        assert!(!check_row.row.has_css_class("done"));
        assert_eq!(check_row.status_label().label().as_str(), "failed");
        assert!(check_row.status_label().has_css_class("error"));

        // finished_row: Done installed (mark_completed = false) omits "done", keeps Failed & size.
        let mut done_pkg = Package::new_dnf("openssl", "x86_64", "3.2.2", "updates");
        done_pkg.size_bytes = Some(2 * 1024 * 1024);
        done_pkg.status = PackageStatus::Completed;
        let done_unmarked = rows::finished_row(&done_pkg, false);
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

        // refill_group / refill_expander / append_expander store tracking.
        let group = libadwaita::PreferencesGroup::new();
        let store = std::cell::RefCell::new(Vec::new());
        rows::refill_group(&group, &store, vec![rows::package_row(&firefox, false)]);
        assert_eq!(store.borrow().len(), 1);
        rows::refill_group(&group, &store, std::iter::empty());
        assert_eq!(store.borrow().len(), 0);

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

        // 4) Full Log window builder populates TextView without presenting.
        let sample_log = "[dnf] Repositories loaded.\n[dnf] Complete!";
        let (log_win, log_view, copy_btn) = dialogs::build_log_window(sample_log);
        assert_eq!(log_win.title().as_deref(), Some("Full Log"));
        let buf = log_view.buffer();
        let actual_text = buf.text(&buf.start_iter(), &buf.end_iter(), true);
        assert_eq!(actual_text.as_str(), sample_log);
        assert!(!log_view.is_editable());
        assert!(log_view.is_monospace());
        assert_eq!(copy_btn.label().as_deref(), Some("Copy All"));
        GtkWindowExt::destroy(&log_win);
    }

    fn verify_all_phases_narrow_and_recovery() {
        use fedora_updater::console::ConsoleBuffer;
        use fedora_updater::model::{
            IdleScreen, Package, PackageStatus, RetryAction, RunPass, StageCount, Stopwatch,
            UpdateSource,
        };
        use fedora_updater::state::{AppState, Phase, RunningState};
        use gtk::prelude::*;
        use libadwaita::prelude::*;

        let widgets = super::Widgets::for_test();
        super::wire_expander(&widgets);
        let console = ConsoleBuffer::new(100);

        // 1) Idle::Fresh + Release Upgrade banner (with age, without age, cleared).
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
        assert!(!widgets.header_refresh.get_visible());
        assert!(!widgets.run_stop.get_visible());
        assert!(!widgets.console_drawer.get_visible());
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

        // 2) Idle::Warning vs Idle::Current + Failed(RetryApply) & Done partial recovery.
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
        assert!(widgets.idle_log.get_visible());
        assert!(!widgets.focus_idle_check.get());

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

        // 3) Authenticating (Check vs Apply) + Cancel affordance + Narrow breakpoint.
        let auth_apply = AppState {
            phase: Phase::Authenticating {
                purpose: fedora_updater::model::AuthPurpose::Apply,
                packages: vec![],
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

        widgets.narrow.set(true);
        super::apply_header_subtitle(&widgets);
        assert_eq!(widgets.window_title.subtitle().as_str(), "");
        widgets.narrow.set(false);
        super::apply_header_subtitle(&widgets);
        assert_eq!(
            widgets.window_title.subtitle().as_str(),
            "Applying updates"
        );

        // 4) Checking: progress bar, footnote, incoming package list visibility, console drawer.
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
        assert!(widgets.console_drawer.get_visible());

        let mut dnf_pkg = Package::new_dnf("firefox", "x86_64", "140.0-1.fc44", "updates");
        dnf_pkg.size_bytes = Some(112_000_000);
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
        assert_eq!(widgets.check_rows.borrow().len(), 1);

        // 5) Ready: clean, soft error, pre-install Stop notice, and empty-list sensitivity guard.
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
        assert!(!widgets.ready_banner.is_revealed());
        assert!(widgets.ready_update.get_sensitive());
        assert_eq!(widgets.ready_rows.borrow().len(), 2);
        assert_eq!(widgets.window_title.title().as_str(), "2 Updates Available");
        assert!(widgets.window_title.subtitle().contains("1 system · 1 apps"));

        // Ready after pre-install Stop must surface explicit Stop banner (NOT "1 source could not be checked").
        let ready_stopped = AppState {
            phase: Phase::Ready {
                packages: vec![dnf_pkg.clone()],
                soft_errors: vec![
                    "Stopped before install. Downloaded packages were not applied.".into(),
                ],
            },
            ..Default::default()
        };
        super::render(&ready_stopped, &console, &widgets);
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

        // 6) Running: Download pass -> Apply pass -> Expander user-open preservation -> Quiet & Stall modes.
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
        assert_eq!(widgets.now_playing_title.label().as_str(), "kernel-core");
        assert_eq!(widgets.now_playing_pct.label().as_str(), "65%");
        assert_eq!(widgets.now_playing_spinner.opacity(), 0.0);
        assert!(widgets.now_playing_size.get_visible());
        assert_eq!(widgets.now_playing_size.label().as_str(), "21 MB");
        assert_eq!(widgets.run_list_rows.borrow().len(), 2);
        assert!(!widgets.run_completed_group.get_visible());

        // Switch to Apply pass with 1 completed row, expand run_completed, then add a 2nd completed row.
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
        assert_eq!(widgets.now_playing_spinner.opacity(), 1.0);
        assert_eq!(widgets.run_completed.title().as_str(), "2 completed");
        assert!(
            widgets.run_completed.is_expanded(),
            "user-expanded completed drawer must stay open across updates"
        );
        assert_eq!(widgets.run_completed_rows.borrow().len(), 2);

        // Deterministic quiet & stall chrome checks without host DNF cache/proc coupling.
        super::paint_quiet_chrome_with(
            &running_apply_1,
            "System · 1 of 3 done",
            20,
            6,
            &widgets,
            Some("Downloading kernel-core — 21 MB and rising"),
            false,
        );
        assert_eq!(widgets.now_playing_spinner.opacity(), 1.0);
        assert!(!widgets.run_stall.is_revealed());
        assert!(widgets
            .window_title
            .subtitle()
            .contains("Downloading kernel-core — 21 MB and rising"));

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

        // 7) Done: clean (no reboot), clean (reboot), partial (Failed sorted before Completed).
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
        assert!(!widgets.reboot_banner.is_revealed());
        assert!(!widgets.done_problem_banner.is_revealed());
        assert!(!widgets.done_retry_btn.get_visible());
        assert!(widgets.done_btn.has_css_class("suggested-action"));
        assert_eq!(widgets.done_installed.title().as_str(), "Installed");

        // Partial Done: Failed package listed BEFORE Completed package regardless of input order.
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
        assert_eq!(widgets.done_installed.title().as_str(), "Finished");
        assert!(widgets.done_problem_banner.is_revealed());
        assert!(widgets
            .done_problem_banner
            .title()
            .contains("RPM database is locked"));
        assert!(widgets.done_retry_btn.get_visible());
        assert!(widgets.done_retry_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_btn.has_css_class("suggested-action"));
        let rows_ref = widgets.done_installed_rows.borrow();
        assert_eq!(rows_ref.len(), 2);
        let first_row = rows_ref[0]
            .clone()
            .downcast::<libadwaita::ActionRow>()
            .expect("ActionRow");
        let second_row = rows_ref[1]
            .clone()
            .downcast::<libadwaita::ActionRow>()
            .expect("ActionRow");
        assert_eq!(first_row.title().as_str(), "kernel");
        assert_eq!(second_row.title().as_str(), "firefox");
        drop(rows_ref);

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
        assert!(widgets.reboot_banner.is_revealed());
        assert_eq!(
            widgets.reboot_banner.button_style(),
            libadwaita::BannerButtonStyle::Suggested
        );
        assert!(widgets.done_retry_btn.get_visible());
        assert!(!widgets.done_retry_btn.has_css_class("suggested-action"));
        assert!(!widgets.done_btn.has_css_class("suggested-action"));

        // 8) Failed: RetryAction labels (CheckAgain / RetryApply / Reboot).
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

        // Log lines while collapsed update console_line preview; opening drawer populates console_full.
        super::record_console_output(&console, &widgets, "dnf", "Loading metadata", false);
        super::record_console_output(&console, &widgets, "dnf", "[1/2] pkg 10%", true);
        assert!(!widgets.console_reveal.reveals_child());
        assert_eq!(widgets.console_line.label().as_str(), "[dnf] [1/2] pkg 10%");
        assert_eq!(
            widgets.console_chevron.icon_name().as_deref(),
            Some("pan-end-symbolic")
        );

        widgets.console_toggle.emit_clicked();
        assert!(widgets.console_reveal.reveals_child());
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

        widgets.console_toggle.emit_clicked();
        assert!(!widgets.console_reveal.reveals_child());
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

        let feed = |msg: WorkerMsg| {
            let aa = auto_apply_calls.clone();
            let rl = release_lookup_calls.clone();
            super::handle_worker_with(
                &state,
                &console,
                &widgets,
                msg,
                move || aa.set(aa.get() + 1),
                move || rl.set(rl.get() + 1),
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
        assert_eq!(widgets.check_rows.borrow().len(), 1);
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
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("ready"));
        assert_eq!(widgets.ready_rows.borrow().len(), 2);

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
        assert_eq!(widgets.stack.visible_child_name().as_deref(), Some("done"));
        assert!(widgets.reboot_banner.is_revealed());
        assert_eq!(widgets.done_installed_rows.borrow().len(), 2);

        // Stale refresh replay (then_apply: true) invokes auto-apply callback cleanly on CheckComplete.
        super::dispatch(&state, &console, &widgets, Event::ResetToIdle, false);
        super::dispatch(&state, &console, &widgets, Event::StartCheck { then_apply: true }, false);
        feed(WorkerMsg::SessionStarted);
        feed(WorkerMsg::Event(Event::CheckComplete {
            packages: vec![dnf_pkg],
            soft_errors: vec![],
            listed_at: SystemTime::UNIX_EPOCH,
        }));
        assert_eq!(auto_apply_calls.get(), 1);
        assert_eq!(release_lookup_calls.get(), 2);

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
        super::wire_shell_controls(&widgets);
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

        widgets.compact.set(true);
        super::sync_shell_layout(&widgets);
        assert!(widgets.split_view.is_collapsed());

        // Simulates libadwaita unapplying compact_breakpoint when narrow_breakpoint activates.
        widgets.compact.set(false);
        widgets.narrow.set(true);
        super::sync_shell_layout(&widgets);
        assert!(super::is_compact_viewport(&widgets));
        assert!(widgets.split_view.is_collapsed());

        // Widening back restores Workbench mode since sidebar_open remained true.
        widgets.narrow.set(false);
        super::sync_shell_layout(&widgets);
        assert!(!widgets.split_view.is_collapsed());

        // 5) Sidebar source navigation and sparkline scaffold API work cleanly.
        if let Some(apps_row) = widgets.sidebar.sources_list().row_at_index(2) {
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
    }
}
