//! Main application window — thin view over the pure state machine.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Builder, Button, Label, ProgressBar, Revealer, Stack, TextView};
use libadwaita::prelude::*;
use libadwaita::{
    Application, ApplicationWindow, Banner, Breakpoint, ExpanderRow, PreferencesGroup, Spinner,
    StatusPage, Toast, ToastOverlay, WindowTitle,
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
    fail_retry_label, idle_chrome, now_playing_subtitle, now_playing_title, quiet_ui, run_heading,
    running_progress_captions, soft_error_banner_title, source_summary_line, window_header,
    HeaderView, QuietInput, AUTH_DESCRIPTION,
};
use fedora_updater::privilege::{dnf_daemon_is_running, kill_recorded_helper};
use fedora_updater::state::{self, session_started_purpose, AppState, Event, Phase, RunningState};

use super::{dialogs, obj, preview, rows};

const WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/window.ui"));
const DEFAULT_WIDTH: i32 = 760;
const DEFAULT_HEIGHT: i32 = 660;

struct Widgets {
    window: ApplicationWindow,
    window_title: WindowTitle,
    /// Un-narrowed subtitle. Header paint reads [`Widgets::narrow`] so a later render keeps it hidden.
    header_subtitle: RefCell<String>,
    narrow: Cell<bool>,
    header_refresh: Button,
    upgrade_notice: gtk::Box,
    upgrade_action: Button,
    upgrade_title: Label,
    upgrade_age: Label,
    stack: Stack,
    toast: ToastOverlay,
    idle_page: StatusPage,
    idle_btn: Button,
    /// True only on the first-run "Check for Updates" page, so Enter starts a check.
    focus_idle_check: Cell<bool>,
    auth_page: StatusPage,
    check_progress: ProgressBar,
    check_progress_meta: Label,
    check_progress_pct: Label,
    check_group: PreferencesGroup,
    check_list_scroll: gtk::ScrolledWindow,
    check_rows: RefCell<Vec<gtk::Widget>>,
    ready_banner: Banner,
    ready_group: PreferencesGroup,
    ready_rows: RefCell<Vec<gtk::Widget>>,
    run_stop: Button,
    run_stall: Banner,
    run_progress: ProgressBar,
    run_progress_meta: Label,
    run_progress_pct: Label,
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
    reboot_banner: Banner,
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

    let widgets = Rc::new(Widgets {
        window: window.clone(),
        window_title: obj(&builder, "window_title"),
        header_subtitle: RefCell::new(String::new()),
        narrow: Cell::new(false),
        header_refresh: obj(&builder, "header_refresh"),
        upgrade_notice: obj(&builder, "upgrade_notice"),
        upgrade_action: obj(&builder, "upgrade_action"),
        upgrade_title: obj(&builder, "upgrade_title"),
        upgrade_age: obj(&builder, "upgrade_age"),
        stack: obj(&builder, "stack"),
        toast: obj(&builder, "toast"),
        idle_page: obj(&builder, "idle_page"),
        idle_btn: obj(&builder, "idle_btn"),
        focus_idle_check: Cell::new(false),
        auth_page: obj(&builder, "auth_page"),
        check_progress: obj(&builder, "check_progress"),
        check_progress_meta: obj(&builder, "check_progress_meta"),
        check_progress_pct: obj(&builder, "check_progress_pct"),
        check_group: obj(&builder, "check_group"),
        check_list_scroll: obj(&builder, "check_list_scroll"),
        check_rows: RefCell::new(Vec::new()),
        ready_banner: obj(&builder, "ready_banner"),
        ready_group: obj(&builder, "ready_group"),
        ready_rows: RefCell::new(Vec::new()),
        run_stop: obj(&builder, "run_stop"),
        run_stall: obj(&builder, "run_stall"),
        run_progress: obj(&builder, "run_progress"),
        run_progress_meta: obj(&builder, "run_progress_meta"),
        run_progress_pct: obj(&builder, "run_progress_pct"),
        now_playing_badge: obj(&builder, "now_playing_badge"),
        now_playing_title: obj(&builder, "now_playing_title"),
        now_playing_subtitle: obj(&builder, "now_playing_subtitle"),
        now_playing_size: obj(&builder, "now_playing_size"),
        now_playing_pct: obj(&builder, "now_playing_pct"),
        now_playing_spinner: obj(&builder, "now_playing_spinner"),
        run_list_group: obj(&builder, "run_list_group"),
        run_list_rows: RefCell::new(Vec::new()),
        run_list_ids: RefCell::new(Vec::new()),
        run_completed_group: obj(&builder, "run_completed_group"),
        run_completed: obj(&builder, "run_completed"),
        run_completed_rows: RefCell::new(Vec::new()),
        run_completed_keys: RefCell::new(Vec::new()),
        run_completed_user_open: Cell::new(false),
        ignore_expand: Cell::new(false),
        console_drawer: obj(&builder, "console_drawer"),
        console_toggle: obj(&builder, "console_toggle"),
        console_reveal: obj(&builder, "console_reveal"),
        console_chevron: obj(&builder, "console_chevron"),
        console_line: obj(&builder, "console_line"),
        console_full: obj(&builder, "console_full"),
        done_btn: obj(&builder, "done_btn"),
        reboot_banner: obj(&builder, "reboot_banner"),
        done_installed_scroll: obj(&builder, "done_installed_scroll"),
        done_installed: obj(&builder, "done_installed"),
        done_installed_rows: RefCell::new(Vec::new()),
        fail_page: obj(&builder, "fail_page"),
        fail_retry: obj(&builder, "fail_retry"),
        last_output: Cell::new(None),
        cache_cursor: RefCell::new(None),
    });

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

    {
        let widgets = widgets.clone();
        let breakpoint: Breakpoint = obj(&builder, "narrow_breakpoint");
        breakpoint.connect_apply(move |_| {
            widgets.narrow.set(true);
            apply_header_subtitle(&widgets);
        });
    }
    {
        let widgets = widgets.clone();
        let breakpoint: Breakpoint = obj(&builder, "narrow_breakpoint");
        breakpoint.connect_unapply(move |_| {
            widgets.narrow.set(false);
            apply_header_subtitle(&widgets);
        });
    }

    let idle_btn = widgets.idle_btn.clone();
    let header_refresh = widgets.header_refresh.clone();
    let ready_update: Button = obj(&builder, "ready_update");
    let auth_cancel: Button = obj(&builder, "auth_cancel");
    let done_btn = widgets.done_btn.clone();
    let fail_retry = widgets.fail_retry.clone();

    wire_console(&widgets, &console);
    {
        let widgets = widgets.clone();
        let expander = widgets.run_completed.clone();
        expander.connect_expanded_notify(move |expander| {
            if widgets.ignore_expand.get() {
                return;
            }
            widgets.run_completed_user_open.set(expander.is_expanded());
        });
    }

    let (tx, rx) = async_channel::unbounded::<WorkerMsg>();

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
    {
        let state = state.clone();
        let window = widgets.window.clone();
        widgets.upgrade_action.connect_clicked(move |_| {
            let Some(version) = state.borrow().release_upgrade else {
                return;
            };
            dialogs::present_upgrade_dialog(&window, version);
        });
    }
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
            if retry == RetryAction::Reboot {
                widgets.toast.add_toast(Toast::new("Rebooting…"));
                run_systemctl_reboot(tx.clone());
                return;
            }
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
            clear_console(&console, &widgets);
            dispatch(&state, &console, &widgets, Event::ResetToIdle, true);
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

    window.connect_close_request(move |_| {
        cancel_background_work();
        release_privileges();
        glib::Propagation::Proceed
    });

    {
        let hooked = widgets.clone();
        let idle_btn = hooked.idle_btn.clone();
        idle_btn.connect_map(move |btn| {
            if hooked.focus_idle_check.get() {
                btn.grab_focus();
            }
        });
    }

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
        begin_apply(state, console, widgets, tx.clone());
    }
    if check_finished {
        spawn_release_lookup(state, console, widgets);
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
    if height > DEFAULT_HEIGHT + 24 {
        window.set_default_size(
            if width > 0 { width } else { DEFAULT_WIDTH },
            DEFAULT_HEIGHT,
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
    let at_full = running.work_progress.is_some_and(|frac| frac >= 0.999);
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
    let quiet = quiet_ui(QuietInput {
        quiet_secs: secs,
        at_full_item: at_full,
        cache_note: cache_note.as_deref(),
        watching_dnf,
        daemon_running: dnf_daemon_is_running(),
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

fn paint_release_upgrade(w: &Widgets, version: Option<u32>, days: Option<u32>) {
    let Some(version) = version else {
        w.upgrade_notice.set_visible(false);
        return;
    };
    let title = fedora_updater::release_upgrade::banner_title(version);
    let name = fedora_updater::release_upgrade::upgrade_button_name(version);
    w.upgrade_title.set_label(&title);
    w.upgrade_action.set_tooltip_text(Some(&name));
    w.upgrade_action
        .update_property(&[gtk::accessible::Property::Label(&name)]);
    match days.map(fedora_updater::release_upgrade::release_age_caption) {
        Some(caption) => {
            w.upgrade_age.set_label(&caption);
            w.upgrade_age.set_visible(true);
        }
        None => w.upgrade_age.set_visible(false),
    }
    w.upgrade_notice.set_visible(true);
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
    paint_release_upgrade(w, state.release_upgrade, state.release_age_days);
    sync_console_drawer(
        w,
        matches!(state.phase, Phase::Checking { .. } | Phase::Running(_)),
    );
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

            if soft_errors.is_empty() {
                w.ready_banner.set_revealed(false);
            } else {
                w.ready_banner
                    .set_title(&soft_error_banner_title(soft_errors.len()));
                w.ready_banner.set_button_label(Some("View Log"));
                w.ready_banner.set_revealed(true);
            }

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
            if *needs_reboot {
                w.reboot_banner
                    .set_button_style(libadwaita::BannerButtonStyle::Suggested);
                w.reboot_banner.set_revealed(true);
                w.done_btn.remove_css_class("suggested-action");
                w.done_btn.remove_css_class("pill");
            } else {
                w.reboot_banner
                    .set_button_style(libadwaita::BannerButtonStyle::Default);
                w.reboot_banner.set_revealed(false);
                w.done_btn.add_css_class("suggested-action");
                w.done_btn.remove_css_class("pill");
            }
            w.done_installed
                .set_title(if *failed > 0 { "Finished" } else { "Installed" });
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
            w.done_installed_scroll.set_visible(!packages.is_empty());
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
    #[test]
    fn window_template_has_adwaita_pages() {
        let xml = super::WINDOW_UI;
        for id in [
            "window_title",
            "header_menu",
            "header_refresh",
            "upgrade_notice",
            "upgrade_action",
            "upgrade_title",
            "upgrade_age",
            "idle_page",
            "auth_page",
            "check_group",
            "console_drawer",
            "console_toggle",
            "console_full",
            "ready_banner",
            "ready_group",
            "ready_update",
            "current_package",
            "run_stop",
            "run_stall",
            "now_playing_title",
            "run_list_scroll",
            "run_list_group",
            "run_completed",
            "done_installed",
            "done_installed_scroll",
            "done_btn",
            "done_log_btn",
            "reboot_banner",
            "fail_page",
            "narrow_breakpoint",
        ] {
            assert!(
                xml.contains(&format!("id=\"{id}\"")),
                "window.ui missing id={id}"
            );
        }
        for gone in [
            "ready_chips",
            "done_icon",
            "reboot_btn",
            "now_playing",
            "close_btn",
            "fail_back",
            "idle_hint",
            "upgrade_info",
            "upgrade_banner",
        ] {
            assert!(
                !xml.contains(&format!("id=\"{gone}\"")),
                "window.ui still has id={gone}"
            );
        }
        assert!(xml.contains("AdwPreferencesGroup"));
        assert!(xml.contains("AdwBanner"));
        assert!(xml.contains("AdwStatusPage"));
        assert!(xml.contains("AdwSpinner"));
        assert!(xml.contains("max-width: 500px"));
        assert!(xml.contains("About Fedora Updater"));
        assert!(xml.contains("Main Menu"));
        assert!(
            !xml.contains("dialog-information-symbolic"),
            "the release reminder is not a header icon"
        );
        assert!(xml.contains("How to upgrade"));
        assert!(xml.contains("go-next-symbolic"));
        let blp_src = include_str!("window.blp");
        let age_at = blp_src.find("Gtk.Label upgrade_age").expect("upgrade_age");
        let age_chunk = &blp_src[age_at..age_at + 220];
        assert!(
            age_chunk.contains("dimmed") && age_chunk.contains("caption"),
            "the release age is a quiet caption under the title"
        );
        let action_at = xml.find("id=\"upgrade_action\"").expect("upgrade_action");
        let header_at = xml.find("id=\"header_menu\"").expect("header_menu");
        let stack_at = xml.find("id=\"stack\"").expect("stack");
        assert!(
            header_at < action_at && action_at < stack_at,
            "the release offer sits under the header and above the page stack"
        );
        // AdwHeaderBar.pack_end prepends. The first end child is the trailing
        // app button, so the menu is emitted before Stop.
        let menu_at = xml.find("id=\"header_menu\"").expect("header_menu");
        let stop_at = xml.find("id=\"run_stop\"").expect("run_stop");
        assert!(
            menu_at < stop_at,
            "pack_end prepends: list the menu before Stop so Stop is the start-side button"
        );
        assert!(xml.contains("Update All"));
        assert!(xml.contains("View Full Log"));
        assert!(xml.contains("View Log"));
        assert!(xml.contains("Waiting for authentication"));
        assert!(xml.contains("Nothing is applied until Update All is chosen"));
        assert!(
            !xml.contains("Waiting for polkit"),
            "auth spinner line is Waiting for authentication"
        );
        assert!(!xml.contains("Keyboard Shortcuts"));
        assert!(!xml.contains("One password prompt"));
        assert!(
            xml.contains("vhomogeneous"),
            "stack must size every page to the window so Done/Idle fill after apply"
        );
        assert!(
            xml.contains("id=\"idle_page\"") && xml.contains("vexpand"),
            "idle status page must expand to fill the stack"
        );
        let installed_at = xml.find("id=\"done_installed\"").expect("done_installed");
        let done_btn_at = xml.find("id=\"done_btn\"").expect("done_btn");
        assert!(
            installed_at < done_btn_at,
            "installed list sits above the summary actions"
        );
        let card_at = xml.find("id=\"current_package\"").expect("current_package");
        let stall_at = xml.find("id=\"run_stall\"").expect("run_stall");
        let scroll_at = xml.find("id=\"run_list_scroll\"").expect("run_list_scroll");
        let list_at = xml.find("id=\"run_list_group\"").expect("run_list_group");
        let completed_at = xml.find("id=\"run_completed\"").expect("run_completed");
        assert!(
            stall_at < card_at,
            "stall banner sits above the current-package card"
        );
        assert!(
            card_at < scroll_at,
            "current-package card must stay sticky above the checklist scroll"
        );
        assert!(
            scroll_at < list_at && list_at < completed_at,
            "remaining work stays in the scroll; completed stays pinned below it"
        );
        let scroll_chunk = &xml[scroll_at..list_at];
        assert!(
            scroll_chunk.contains("propagate-natural-height"),
            "run_list_scroll must not propagate the package list height to the window"
        );
        assert!(
            completed_at > xml.find("id=\"run_list_scroll\"").unwrap(),
            "completed expander must not live inside the remaining-work scroll"
        );
        let blp = include_str!("window.blp");
        assert!(!blp.contains("margin-start: 16"));
        assert!(!blp.contains("margin-end: 16"));
        assert!(blp.contains("margin-start: 12"));
        assert!(blp.contains("margin-end: 12"));
    }

    #[test]
    fn rows_and_stylesheet_drop_removed_chrome() {
        let rows = include_str!("rows.rs");
        let css = include_str!("../style.css");
        for token in [
            "sev-dot",
            "dim-label",
            "stage-apply",
            ".chip",
            ".stat-card",
            ".reboot-card",
            ".now-playing",
            ".status-footer",
            ".status-dot",
        ] {
            assert!(!rows.contains(token), "rows.rs still has {token}");
            assert!(!css.contains(token), "style.css still has {token}");
        }
        assert!(css.contains(".package-badge.src-flatpak"));
        assert!(css.contains("@purple_1"));
        assert!(css.contains("opacity: 0.72"));
        assert!(css.contains("opacity: 0.82"));
        assert!(css.contains("margin: 0 12px 12px"));
        assert!(rows.contains("package-badge"));
        assert!(!rows.contains("object-select-symbolic"));
    }

    #[test]
    fn session_started_commit_leaves_auth_and_panics_if_a_borrow_is_held() {
        use fedora_updater::model::AuthPurpose;
        use fedora_updater::state::Phase;
        use std::cell::RefCell;
        use std::panic::{catch_unwind, AssertUnwindSafe};
        use std::rc::Rc;

        let authenticating = || {
            let mut auth = fedora_updater::state::AppState::default();
            auth.phase = Phase::Authenticating {
                purpose: AuthPurpose::Check,
                packages: vec![],
                last_checked: None,
            };
            Rc::new(RefCell::new(auth))
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
}
