//! Main application window — thin view over the pure state machine.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use gtk::gio;
use gtk::glib;
use gtk::glib::object::IsA;
use gtk::prelude::*;
use gtk::{Builder, Button, Label, ProgressBar, Revealer, Stack, TextView};
use libadwaita::prelude::*;
use libadwaita::{
    Application, ApplicationWindow, Banner, Breakpoint, ExpanderRow, PreferencesGroup, Spinner,
    StatusPage, StyleManager, Toast, ToastOverlay, Window as AdwWindow, WindowTitle,
};

use fedora_updater::backend::dnf::{self, InstallCheck};
use fedora_updater::model::{
    about_time_left, cache_activity_note, check_progress, count_by_source, idle_chrome,
    now_playing_subtitle, now_playing_title, quiet_ui, run_heading, running_progress_captions,
    source_summary_line, AuthPurpose, IdleScreen, Package, RetryAction, RunPass, StageCount,
    UpdateSource, WorkerEvent,
};
use fedora_updater::orchestrator::{
    can_skip_auth_ui, cancel_background_work, release_privileges, run_apply_all, run_check_all,
    run_systemctl_reboot, session_started_event, worker_to_state_events,
};
use fedora_updater::privilege::{
    dnf_daemon_is_running, helper_activity, kill_recorded_helper, run_local_command, HelperActivity,
};
use fedora_updater::state::{self, session_started_purpose, AppState, Event, Phase, RecoveryLatch};

use super::{preview, rows};

const WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/window.ui"));
const LOG_WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/log_window.ui"));
const DEFAULT_WIDTH: i32 = 760;
const DEFAULT_HEIGHT: i32 = 660;
const ABOUT_NAME: &str = "Fedora Updater";
const ABOUT_COMMENTS: &str = "A desktop front-end for Fedora system, Flatpak, and firmware updates";
const AUTH_DESCRIPTION: &str =
    "A system dialog asks for an administrator password. The password is not stored in this window.";

/// Title and subtitle for the one shared header. `narrow` blanks the subtitle on every phase.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeaderChrome {
    title: String,
    subtitle: String,
}

#[derive(Clone, Copy)]
enum HeaderView<'a> {
    Idle,
    Authenticating(AuthPurpose),
    Checking { footnote: &'a str },
    Ready { count: usize, summary: &'a str },
    Running { heading: &'a str, footnote: &'a str },
    Done { failed: bool, subtitle: &'a str },
    Failed,
}

fn window_header(view: HeaderView<'_>, narrow: bool) -> HeaderChrome {
    let (title, subtitle): (String, String) = match view {
        HeaderView::Idle | HeaderView::Failed => ("Fedora Updater".into(), String::new()),
        HeaderView::Authenticating(AuthPurpose::Check) => {
            ("Fedora Updater".into(), "Checking for updates".into())
        }
        HeaderView::Authenticating(AuthPurpose::Apply) => {
            ("Fedora Updater".into(), "Applying updates".into())
        }
        HeaderView::Checking { footnote } => ("Checking for Updates".into(), footnote.to_string()),
        HeaderView::Ready { count, summary } => {
            let title = if count == 1 {
                "1 Update Available".into()
            } else {
                format!("{count} Updates Available")
            };
            (title, summary.to_string())
        }
        HeaderView::Running { heading, footnote } => (heading.to_string(), footnote.to_string()),
        HeaderView::Done { failed, subtitle } => {
            let title = if failed {
                "Updates Finished With Problems"
            } else {
                "All Updates Applied"
            };
            (title.into(), subtitle.to_string())
        }
    };
    HeaderChrome {
        title,
        subtitle: if narrow { String::new() } else { subtitle },
    }
}

fn checking_footnote(groups_done: u32, eta: Option<&str>) -> String {
    match eta {
        Some(eta) if !eta.is_empty() => format!("{eta} · {groups_done} of 3 sources"),
        _ => format!("{groups_done} of 3 sources"),
    }
}

fn done_header_subtitle(
    failed: usize,
    upgraded: usize,
    dnf: usize,
    flatpak: usize,
    firmware: usize,
    duration: &str,
) -> String {
    if failed > 0 {
        format!("{upgraded} applied · {failed} failed · {duration}")
    } else {
        format!("{upgraded} upgraded · {dnf} system / {flatpak} apps / {firmware} firmware · {duration}")
    }
}

fn soft_error_banner_title(count: usize) -> String {
    if count == 1 {
        "1 source could not be checked".into()
    } else {
        format!("{count} sources could not be checked")
    }
}

fn fail_retry_label(retry: RetryAction) -> &'static str {
    match retry {
        RetryAction::CheckAgain => "Check Again",
        RetryAction::Reboot => "Restart",
    }
}

/// Copy the two alert dialogs pass to `Adw.AlertDialog`. Response ids stay stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DialogCopy {
    heading: &'static str,
    body: &'static str,
    close_id: &'static str,
    close_label: &'static str,
    confirm_id: &'static str,
    confirm_label: &'static str,
    confirm_destructive: bool,
    confirm_suggested: bool,
    default_id: &'static str,
}

fn stale_update_dialog() -> DialogCopy {
    DialogCopy {
        heading: "Refresh Update List?",
        body: "This list is more than a day old. Refresh it before applying these updates.",
        close_id: "no",
        close_label: "_Cancel",
        confirm_id: "yes",
        confirm_label: "_Refresh",
        confirm_destructive: false,
        confirm_suggested: true,
        default_id: "yes",
    }
}

fn stop_update_dialog() -> DialogCopy {
    DialogCopy {
        heading: "Stop This Update?",
        body: "Stopping while packages are being installed can leave the package database incomplete. Check package history before trying again.",
        close_id: "wait",
        close_label: "_Keep Waiting",
        confirm_id: "stop",
        confirm_label: "Stop _Anyway",
        confirm_destructive: true,
        confirm_suggested: false,
        default_id: "wait",
    }
}

struct Widgets {
    window: ApplicationWindow,
    window_title: WindowTitle,
    /// Un-narrowed subtitle. Header paint reads [`Widgets::narrow`] so a later render keeps it hidden.
    header_subtitle: RefCell<String>,
    narrow: Cell<bool>,
    header_refresh: Button,
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
    check_console_line: Label,
    check_console_full: Label,
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
    run_console_line: Label,
    run_console_full: Label,
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
    reconcile_tx: async_channel::Sender<InstallCheck>,
}

pub fn build(app: &Application) {
    let state = Rc::new(RefCell::new(preview::initial_state().unwrap_or_default()));

    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../style.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    StyleManager::default().set_color_scheme(libadwaita::ColorScheme::Default);

    let builder = Builder::from_string(WINDOW_UI);
    let window: ApplicationWindow = obj(&builder, "window");
    window.set_application(Some(app));

    window.set_default_size(DEFAULT_WIDTH, DEFAULT_HEIGHT);

    let (reconcile_tx, reconcile_rx) = async_channel::unbounded::<InstallCheck>();

    let widgets = Rc::new(Widgets {
        window: window.clone(),
        window_title: obj(&builder, "window_title"),
        header_subtitle: RefCell::new(String::new()),
        narrow: Cell::new(false),
        header_refresh: obj(&builder, "header_refresh"),
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
        check_console_line: obj(&builder, "check_console_line"),
        check_console_full: obj(&builder, "check_console_full"),
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
        run_console_line: obj(&builder, "run_console_line"),
        run_console_full: obj(&builder, "run_console_full"),
        done_btn: obj(&builder, "done_btn"),
        reboot_banner: obj(&builder, "reboot_banner"),
        done_installed_scroll: obj(&builder, "done_installed_scroll"),
        done_installed: obj(&builder, "done_installed"),
        done_installed_rows: RefCell::new(Vec::new()),
        fail_page: obj(&builder, "fail_page"),
        fail_retry: obj(&builder, "fail_retry"),
        last_output: Cell::new(None),
        cache_cursor: RefCell::new(None),
        reconcile_tx,
    });

    let about = gio::SimpleAction::new("about", None);
    {
        let window = window.clone();
        about.connect_activate(move |_, _| present_about(&window));
    }
    window.add_action(&about);

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
    let done_log_btn: Button = obj(&builder, "done_log_btn");
    let fail_retry = widgets.fail_retry.clone();
    let fail_log: Button = obj(&builder, "fail_log");
    let check_console_toggle: Button = obj(&builder, "check_console_toggle");
    let check_console_reveal: Revealer = obj(&builder, "check_console_reveal");
    let check_console_chevron: gtk::Image = obj(&builder, "check_console_chevron");
    let run_console_toggle: Button = obj(&builder, "run_console_toggle");
    let run_console_reveal: Revealer = obj(&builder, "run_console_reveal");
    let run_console_chevron: gtk::Image = obj(&builder, "run_console_chevron");

    wire_console(
        &check_console_toggle,
        &check_console_reveal,
        &check_console_chevron,
    );
    wire_console(
        &run_console_toggle,
        &run_console_reveal,
        &run_console_chevron,
    );
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

    let (tx, rx) = async_channel::unbounded::<WorkerEvent>();

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        idle_btn.connect_clicked(move |_| {
            start_check(&state, &widgets, tx.clone(), false);
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        header_refresh.connect_clicked(move |_| {
            start_check(&state, &widgets, tx.clone(), false);
        });
    }
    {
        let state = state.clone();
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
            start_check(&state, &widgets, tx.clone(), false);
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        ready_update.connect_clicked(move |_| {
            if state.borrow().update_list_needs_refresh(SystemTime::now()) {
                prompt_stale_update_list(&widgets, &state, tx.clone());
                return;
            }
            begin_apply(&state, &widgets, tx.clone());
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let run_stop = widgets.run_stop.clone();
        run_stop.connect_clicked(move |_| {
            let pass_is_apply = matches!(
                state.borrow().phase,
                Phase::Running {
                    pass: RunPass::Apply,
                    ..
                }
            );
            let running = matches!(state.borrow().phase, Phase::Running { .. });
            if pass_is_apply {
                prompt_stop_during_transaction(&widgets, &state);
            } else if running {
                stop_apply(&state, &widgets, false);
            }
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        auth_cancel.connect_clicked(move |_| {
            cancel_background_work();
            dispatch(&state, &widgets, Event::CancelAuth);
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        done_btn.connect_clicked(move |_| {
            dispatch(
                &state,
                &widgets,
                Event::ResetToIdle {
                    message: Some("System updated".into()),
                },
            );
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        done_log_btn.connect_clicked(move |_| {
            let log = state.borrow().console.full_text();
            show_log_window(&widgets.stack, &log);
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        fail_log.connect_clicked(move |_| {
            let log = state.borrow().console.full_text();
            show_log_window(&widgets.stack, &log);
        });
    }
    {
        let state = state.clone();
        let stack = widgets.stack.clone();
        widgets.ready_banner.connect_button_clicked(move |_| {
            let log = state.borrow().console.full_text();
            show_log_window(&stack, &log);
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
        let widgets = widgets.clone();
        let tx = tx.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                handle_worker(&state, &widgets, &tx, ev);
            }
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        glib::spawn_future_local(async move {
            while let Ok(check) = reconcile_rx.recv().await {
                handle_install_check(&state, &widgets, check);
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

    render(&state, &widgets);
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
    widgets: &Rc<Widgets>,
    tx: async_channel::Sender<WorkerEvent>,
    then_apply: bool,
) {
    dispatch(state, widgets, Event::StartCheck { then_apply });
    if can_skip_auth_ui() {
        dispatch(state, widgets, session_started_event(AuthPurpose::Check));
    }
    run_check_all(tx);
}

fn begin_apply(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    tx: async_channel::Sender<WorkerEvent>,
) {
    widgets.last_output.set(None);
    *widgets.cache_cursor.borrow_mut() = None;
    let packages = state.borrow().packages().to_vec();
    dispatch(state, widgets, Event::StartApply);
    if can_skip_auth_ui() {
        dispatch(state, widgets, session_started_event(AuthPurpose::Apply));
    }
    run_apply_all(tx, packages);
}

fn can_start_apply(state: &AppState) -> bool {
    matches!(&state.phase, Phase::Ready { packages, .. } if !packages.is_empty())
}

/// Ask before applying a list that has been sitting for more than 24 hours.
/// Yes refreshes, then applies whatever that check finds. No leaves the list
/// untouched — Update All asks again until the user accepts a refresh.
fn prompt_stale_update_list(
    widgets: &Rc<Widgets>,
    state: &Rc<RefCell<AppState>>,
    tx: async_channel::Sender<WorkerEvent>,
) {
    let copy = stale_update_dialog();
    let dialog = build_dialog(&copy);

    let state = state.clone();
    let widgets_for_cb = widgets.clone();
    dialog.connect_response(None, move |_dialog, response| {
        if response == copy.confirm_id {
            start_check(&state, &widgets_for_cb, tx.clone(), true);
        }
    });
    dialog.present(Some(&widgets.window));
}

fn present_about(parent: &impl IsA<gtk::Widget>) {
    let dialog = libadwaita::AboutDialog::new();
    dialog.set_application_name(ABOUT_NAME);
    dialog.set_version(env!("CARGO_PKG_VERSION"));
    dialog.set_comments(ABOUT_COMMENTS);
    dialog.present(Some(parent));
}

fn show_log_window(parent: &impl IsA<gtk::Widget>, log: &str) {
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

fn dispatch(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, event: Event) {
    let current = state.borrow().clone();
    match state::reduce(current, event) {
        Ok(next) => {
            *state.borrow_mut() = next;
            render(state, widgets);
        }
        Err(e) => {
            eprintln!("state error: {e}");
            widgets
                .toast
                .add_toast(Toast::new(&format!("Internal error: {e}")));
        }
    }
}

fn dispatch_worker(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, event: Event) {
    let current = state.borrow().clone();
    match state::reduce(current, event) {
        Ok(next) => {
            *state.borrow_mut() = next;
            render(state, widgets);
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
    let current = state.borrow().clone();
    match state::reduce(current, session_started_event(purpose)) {
        Ok(next) => {
            *state.borrow_mut() = next;
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

fn handle_worker(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    tx: &async_channel::Sender<WorkerEvent>,
    ev: WorkerEvent,
) {
    let check_finished = matches!(ev, WorkerEvent::CheckAllFinished { .. });
    if matches!(
        ev,
        WorkerEvent::Line { .. } | WorkerEvent::Draw { .. } | WorkerEvent::ApplyProgress { .. }
    ) {
        widgets.last_output.set(Some(Instant::now()));
    }

    match &ev {
        WorkerEvent::SessionStarted => {
            match commit_session_started(state) {
                Ok(true) => render(state, widgets),
                Ok(false) => {}
                Err(e) => eprintln!("ignored worker event: {e}"),
            }
            return;
        }
        WorkerEvent::Line { .. } | WorkerEvent::Draw { .. } => {
            for e in worker_to_state_events(ev) {
                let current = state.borrow().clone();
                if let Ok(next) = state::reduce(current, e) {
                    *state.borrow_mut() = next;
                }
            }
            let s = state.borrow();
            let last = s.console.last_line().to_string();
            let full = s.console.full_text();
            drop(s);
            widgets.check_console_line.set_text(&last);
            widgets.check_console_full.set_text(&full);
            widgets.run_console_line.set_text(&last);
            widgets.run_console_full.set_text(&full);
            return;
        }
        _ => {}
    }

    for e in worker_to_state_events(ev) {
        dispatch_worker(state, widgets, e);
    }

    let apply_now = if check_finished {
        let borrowed = state.borrow();
        borrowed.apply_when_ready && can_start_apply(&borrowed)
    } else {
        false
    };
    if apply_now {
        begin_apply(state, widgets, tx.clone());
    }
}

fn wire_console(toggle: &Button, reveal: &Revealer, chevron: &gtk::Image) {
    let reveal = reveal.clone();
    let chevron = chevron.clone();
    toggle.connect_clicked(move |_| {
        let open = !reveal.reveals_child();
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

fn obj<T: IsA<glib::Object>>(builder: &Builder, id: &str) -> T {
    builder
        .object(id)
        .unwrap_or_else(|| panic!("missing UI object `{id}`"))
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

fn has_sources_after_dnf(packages: &[Package]) -> bool {
    packages
        .iter()
        .any(|p| p.source.apply_order() > UpdateSource::Dnf.apply_order())
}

/// Footnote, spinner, and stall banner for a quiet run. Does not rebuild the list.
fn paint_quiet_chrome(
    state: &Rc<RefCell<AppState>>,
    w: &Widgets,
    caption: &str,
    applying: bool,
    work_progress: Option<f64>,
    current_source: Option<UpdateSource>,
    last_line: &str,
    elapsed: u64,
    overall_progress: f64,
    packages: &[Package],
) {
    let secs = quiet_secs(w);
    let at_full = work_progress.is_some_and(|frac| frac >= 0.999);
    let watching_dnf = current_source == Some(UpdateSource::Dnf);
    let cache_note = if secs >= 5 && watching_dnf && !applying {
        dnf::poll_cache_growth(
            std::path::Path::new(dnf::DNF_CACHE_ROOT),
            &mut w.cache_cursor.borrow_mut(),
        )
        .map(|growth| cache_activity_note(&growth.package, growth.bytes, growth.rising))
    } else {
        None
    };
    // Absent covers preview and the moment before polkit returns. That is not
    // "dnf exited", so the page must not rpm-reconcile the real system.
    let tool_alive = !watching_dnf || helper_activity() != HelperActivity::Exited;
    let quiet = quiet_ui(
        secs,
        at_full,
        cache_note.as_deref(),
        tool_alive,
        watching_dnf,
        last_line,
        dnf_daemon_is_running(),
    );

    let mut foot = caption.to_string();
    if quiet.show_eta {
        if let Some(eta) = about_time_left(elapsed, overall_progress) {
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

    if quiet.show_spinner || work_progress.is_none() {
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

    if quiet.reconcile && watching_dnf {
        maybe_reconcile_dnf(state, w, packages);
    }
}

fn refresh_quiet_chrome(state: &Rc<RefCell<AppState>>, w: &Rc<Widgets>) {
    let s = state.borrow();
    let Phase::Running {
        packages,
        overall_progress,
        pass,
        current_source,
        work_progress,
        stage,
        ..
    } = &s.phase
    else {
        drop(s);
        w.run_stall.set_revealed(false);
        return;
    };
    let elapsed = s
        .stopwatch
        .as_ref()
        .map(|sw| sw.elapsed_secs())
        .unwrap_or(0);
    let last = s.console.last_line().to_string();
    let package_done = packages.iter().filter(|p| p.status.is_done()).count();
    let (done, total) = match *stage {
        Some(StageCount { current, total }) if total > 0 => (current as usize, total as usize),
        _ => (package_done, packages.len()),
    };
    let (caption, _) = running_progress_captions(*current_source, done, total, *pass);
    let packages = packages.clone();
    let applying = *pass == RunPass::Apply;
    let work_progress = *work_progress;
    let current_source = *current_source;
    let overall_progress = *overall_progress;
    drop(s);
    paint_quiet_chrome(
        state,
        w,
        &caption,
        applying,
        work_progress,
        current_source,
        &last,
        elapsed,
        overall_progress,
        &packages,
    );
}

/// Writes `RecoveryProbeStarted` onto the latch. Returns whether that moved it
/// to `InFlight`. The caller must not hold a `Ref` on `state`: a shared borrow
/// still outstanding panics on `borrow_mut` instead of skipping the probe.
fn commit_recovery_probe_started(state: &Rc<RefCell<AppState>>) -> bool {
    let can_start = {
        let borrowed = state.borrow();
        let recovery = match &borrowed.phase {
            Phase::Running { recovery, .. } => *recovery,
            _ => return false,
        };
        match recovery {
            RecoveryLatch::Idle => true,
            RecoveryLatch::Backoff { not_before } => Instant::now() >= not_before,
            RecoveryLatch::InFlight | RecoveryLatch::Settled => false,
        }
    };
    if !can_start {
        return false;
    }
    let current = state.borrow().clone();
    let Ok(next) = state::reduce(current, Event::RecoveryProbeStarted) else {
        return false;
    };
    let moved = matches!(
        &next.phase,
        Phase::Running {
            recovery: RecoveryLatch::InFlight,
            ..
        }
    );
    *state.borrow_mut() = next;
    moved
}

fn maybe_reconcile_dnf(state: &Rc<RefCell<AppState>>, w: &Widgets, packages: &[Package]) {
    let dnf_pkgs: Vec<Package> = packages
        .iter()
        .filter(|p| p.source == UpdateSource::Dnf)
        .cloned()
        .collect();
    if dnf_pkgs.is_empty() {
        return;
    }
    if !commit_recovery_probe_started(state) {
        return;
    }
    let tx = w.reconcile_tx.clone();
    std::thread::spawn(move || {
        let check = query_dnf_install_state(&dnf_pkgs);
        let _ = tx.send_blocking(check);
    });
}

fn query_dnf_install_state(packages: &[Package]) -> InstallCheck {
    let args = dnf::rpm_query_args(packages);
    // `-q`, `--qf`, and the format are not package specs.
    if args.len() <= 3 {
        return InstallCheck::Unknown;
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut output = String::new();
    match run_local_command("rpm", &arg_refs, |line| {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&line);
    }) {
        Ok(_) => {
            let installed = dnf::parse_rpm_query(&output);
            dnf::check_packages_installed(packages, &installed)
        }
        Err(_) => InstallCheck::Unknown,
    }
}

fn handle_install_check(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, check: InstallCheck) {
    let (packages, current_source, needs_reboot, recovery) = {
        let borrowed = state.borrow();
        let Phase::Running {
            packages,
            current_source,
            needs_reboot,
            recovery,
            ..
        } = &borrowed.phase
        else {
            return;
        };
        (packages.clone(), *current_source, *needs_reboot, *recovery)
    };
    if current_source != Some(UpdateSource::Dnf) {
        return;
    }
    match check {
        InstallCheck::Unknown => {
            dispatch_worker(state, widgets, Event::RecoveryProbeUnknown);
        }
        InstallCheck::AllInstalled => {
            if has_sources_after_dnf(&packages) {
                if matches!(recovery, RecoveryLatch::Settled) {
                    return;
                }
                dispatch_worker(state, widgets, Event::RecoveryProbeSettled);
                dispatch_worker(
                    state,
                    widgets,
                    Event::BackendApplyDone {
                        source: UpdateSource::Dnf,
                        success: true,
                        needs_reboot,
                    },
                );
            } else {
                cancel_background_work();
                kill_recorded_helper();
                dispatch_worker(state, widgets, Event::ApplyComplete { needs_reboot });
            }
        }
        InstallCheck::NotInstalled => {
            cancel_background_work();
            kill_recorded_helper();
            dispatch_worker(
                state,
                widgets,
                Event::Fail {
                    title: "Update did not finish".into(),
                    detail: "dnf exited and the new package versions are not installed. Nothing from this run was committed. You can run Update All again.".into(),
                    retry: RetryAction::CheckAgain,
                },
            );
        }
        InstallCheck::Partial { installed, total } => {
            cancel_background_work();
            kill_recorded_helper();
            dispatch_worker(
                state,
                widgets,
                Event::Fail {
                    title: "Update stopped partway through".into(),
                    detail: format!(
                        "{installed} of {total} packages were installed before dnf exited. Check `dnf history` before running Update All again."
                    ),
                    retry: RetryAction::CheckAgain,
                },
            );
        }
    }
}

fn prompt_stop_during_transaction(widgets: &Rc<Widgets>, state: &Rc<RefCell<AppState>>) {
    let copy = stop_update_dialog();
    let dialog = build_dialog(&copy);

    let state = state.clone();
    let widgets_for_cb = widgets.clone();
    dialog.connect_response(None, move |_dialog, response| {
        if response != copy.confirm_id {
            return;
        }
        let during = matches!(
            state.borrow().phase,
            Phase::Running {
                pass: RunPass::Apply,
                ..
            }
        );
        let running = matches!(state.borrow().phase, Phase::Running { .. });
        if running {
            stop_apply(&state, &widgets_for_cb, during);
        }
    });
    dialog.present(Some(&widgets.window));
}

fn stop_apply(state: &Rc<RefCell<AppState>>, widgets: &Rc<Widgets>, during_transaction: bool) {
    cancel_background_work();
    kill_recorded_helper();
    dispatch_worker(state, widgets, Event::StopApply { during_transaction });
}

fn render_running(
    state: &Rc<RefCell<AppState>>,
    w: &Widgets,
    packages: &[Package],
    overall_progress: f64,
    phase_label: &str,
    pass: RunPass,
    stage: Option<StageCount>,
    current_source: Option<UpdateSource>,
    active_index: Option<usize>,
    current_name: Option<&str>,
    work_progress: Option<f64>,
    elapsed: u64,
    last_console: &str,
    full_console: &str,
) {
    w.stack.set_visible_child_name("running");
    w.header_refresh.set_visible(false);
    w.run_stop.set_visible(true);
    paint_stop_button(&w.run_stop, pass);
    w.window_title.set_title(run_heading(phase_label, pass));
    let package_done = packages.iter().filter(|p| p.status.is_done()).count();
    let (done, total) = match stage {
        Some(StageCount { current, total }) if total > 0 => (current as usize, total as usize),
        _ => (package_done, packages.len()),
    };
    let (caption, meta) = running_progress_captions(current_source, done, total, pass);
    w.run_progress.set_fraction(overall_progress);
    w.run_progress_meta.set_text(&meta);
    w.run_progress_pct
        .set_text(&format!("{:.0}%", overall_progress * 100.0));

    let title = now_playing_title(packages, active_index, current_name, phase_label);
    let subtitle = now_playing_subtitle(&title, phase_label, current_source);
    w.now_playing_title.set_text(&title);
    w.now_playing_subtitle.set_text(if subtitle.is_empty() {
        "Working"
    } else {
        &subtitle
    });
    w.now_playing_subtitle.set_visible(true);
    let playing = current_package(packages, active_index, current_name);
    paint_source_badge(&w.now_playing_badge, playing);
    if let Some(size) = playing.and_then(|package| package.size.as_deref()) {
        w.now_playing_size.set_text(size);
        w.now_playing_size.set_visible(true);
    } else {
        w.now_playing_size.set_visible(false);
    }
    if let Some(frac) = work_progress {
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
            let is_current = active_index
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

    if !last_console.is_empty() {
        w.run_console_line.set_text(last_console);
    }
    w.run_console_full.set_text(full_console);
    paint_quiet_chrome(
        state,
        w,
        &caption,
        pass == RunPass::Apply,
        work_progress,
        current_source,
        last_console,
        elapsed,
        overall_progress,
        packages,
    );
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

fn render(state: &Rc<RefCell<AppState>>, w: &Rc<Widgets>) {
    w.run_stall.set_revealed(false);
    clear_idle_check_focus(w);
    let s = state.borrow();
    let last_console = s.console.last_line().to_string();
    let full_console = s.console.full_text();
    let elapsed = s
        .stopwatch
        .as_ref()
        .map(|sw| sw.elapsed_secs())
        .unwrap_or(0);

    // Quiet reconcile writes the recovery latch. Clone the Running page and
    // drop this Ref before that paint; a shared borrow still held here panics
    // on the write and the rpm probe never starts.
    let running = match &s.phase {
        Phase::Running {
            packages,
            overall_progress,
            phase_label,
            current_source,
            active_index,
            current_name,
            work_progress,
            pass,
            stage,
            ..
        } => Some((
            packages.clone(),
            *overall_progress,
            phase_label.clone(),
            *current_source,
            *active_index,
            current_name.clone(),
            *work_progress,
            *pass,
            *stage,
        )),
        _ => None,
    };
    if let Some((
        packages,
        overall_progress,
        phase_label,
        current_source,
        active_index,
        current_name,
        work_progress,
        pass,
        stage,
    )) = running
    {
        drop(s);
        render_running(
            state,
            w,
            &packages,
            overall_progress,
            &phase_label,
            pass,
            stage,
            current_source,
            active_index,
            current_name.as_deref(),
            work_progress,
            elapsed,
            &last_console,
            &full_console,
        );
        return;
    }

    w.header_refresh
        .set_visible(matches!(s.phase, Phase::Ready { .. }));
    w.run_stop.set_visible(false);

    match &s.phase {
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
            let (_step, frac, groups_done, status) = check_progress(checked_sources);
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
            if !last_console.is_empty() {
                w.check_console_line.set_text(&last_console);
            }
            w.check_console_full.set_text(&full_console);
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
        Phase::Running { .. } => {
            unreachable!("Running is painted after the state borrow is dropped");
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
            "idle_page",
            "auth_page",
            "check_group",
            "check_console_toggle",
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
            "run_console_toggle",
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
    fn header_titles_follow_the_phase_table() {
        use fedora_updater::model::{run_heading, AuthPurpose, RunPass};

        let cases = [
            (super::HeaderView::Idle, "Fedora Updater", ""),
            (
                super::HeaderView::Authenticating(AuthPurpose::Check),
                "Fedora Updater",
                "Checking for updates",
            ),
            (
                super::HeaderView::Authenticating(AuthPurpose::Apply),
                "Fedora Updater",
                "Applying updates",
            ),
            (
                super::HeaderView::Checking {
                    footnote: "2 of 3 sources",
                },
                "Checking for Updates",
                "2 of 3 sources",
            ),
            (
                super::HeaderView::Checking {
                    footnote: "about 1 minute · 1 of 3 sources",
                },
                "Checking for Updates",
                "about 1 minute · 1 of 3 sources",
            ),
            (
                super::HeaderView::Ready {
                    count: 1,
                    summary: "1 system · 0 apps · 0 firmware",
                },
                "1 Update Available",
                "1 system · 0 apps · 0 firmware",
            ),
            (
                super::HeaderView::Ready {
                    count: 4,
                    summary: "2 system · 2 apps · 0 firmware",
                },
                "4 Updates Available",
                "2 system · 2 apps · 0 firmware",
            ),
            (
                super::HeaderView::Running {
                    heading: "Downloading",
                    footnote: "System · 4 of 19 downloaded",
                },
                "Downloading",
                "System · 4 of 19 downloaded",
            ),
            (
                super::HeaderView::Done {
                    failed: false,
                    subtitle: "12 upgraded · 10 system / 2 apps / 0 firmware · 4 minutes",
                },
                "All Updates Applied",
                "12 upgraded · 10 system / 2 apps / 0 firmware · 4 minutes",
            ),
            (
                super::HeaderView::Done {
                    failed: true,
                    subtitle: "8 applied · 1 failed · 4 minutes",
                },
                "Updates Finished With Problems",
                "8 applied · 1 failed · 4 minutes",
            ),
            (super::HeaderView::Failed, "Fedora Updater", ""),
        ];
        for (view, title, subtitle) in cases {
            let wide = super::window_header(view, false);
            assert_eq!(wide.title, title);
            assert_eq!(wide.subtitle, subtitle);
            assert_ne!(wide.subtitle, "Waiting for authentication");
            let narrow = super::window_header(view, true);
            assert_eq!(narrow.title, title);
            assert_eq!(narrow.subtitle, "");
        }

        for pass in [RunPass::Download, RunPass::Apply] {
            for label in [
                "",
                "Downloading",
                "Installing packages",
                "Verifying",
                "transaction",
                "Updating",
            ] {
                let heading = run_heading(label, pass);
                let chrome = super::window_header(
                    super::HeaderView::Running {
                        heading,
                        footnote: "System · 1 of 2 downloaded",
                    },
                    false,
                );
                assert_ne!(chrome.title, "Installing");
                assert_eq!(chrome.title, heading);
            }
        }
        assert_eq!(
            super::done_header_subtitle(0, 12, 10, 2, 0, "4 minutes"),
            "12 upgraded · 10 system / 2 apps / 0 firmware · 4 minutes"
        );
        assert_eq!(
            super::done_header_subtitle(1, 8, 8, 0, 0, "4 minutes"),
            "8 applied · 1 failed · 4 minutes"
        );
        assert_eq!(super::checking_footnote(2, None), "2 of 3 sources");
        assert_eq!(
            super::checking_footnote(1, Some("about 1 minute")),
            "about 1 minute · 1 of 3 sources"
        );
        assert_eq!(
            super::soft_error_banner_title(1),
            "1 source could not be checked"
        );
        assert_eq!(
            super::soft_error_banner_title(3),
            "3 sources could not be checked"
        );
        assert_eq!(
            super::ABOUT_COMMENTS,
            "A desktop front-end for Fedora system, Flatpak, and firmware updates"
        );
        assert_eq!(super::ABOUT_NAME, "Fedora Updater");
        assert!(!super::ABOUT_COMMENTS.contains("polished"));
        assert_eq!(
            super::AUTH_DESCRIPTION,
            "A system dialog asks for an administrator password. The password is not stored in this window."
        );
        assert_ne!(
            super::window_header(super::HeaderView::Authenticating(AuthPurpose::Check), false)
                .subtitle,
            "Waiting for authentication"
        );
    }

    #[test]
    fn alert_dialogs_use_the_accepted_copy() {
        let stale = super::stale_update_dialog();
        assert_eq!(stale.heading, "Refresh Update List?");
        assert_eq!(
            stale.body,
            "This list is more than a day old. Refresh it before applying these updates."
        );
        assert_eq!(stale.close_id, "no");
        assert_eq!(stale.close_label, "_Cancel");
        assert_eq!(stale.confirm_id, "yes");
        assert_eq!(stale.confirm_label, "_Refresh");
        assert!(stale.confirm_suggested);
        assert!(!stale.confirm_destructive);
        assert_eq!(stale.default_id, "yes");
        assert_eq!(stale.default_id, stale.confirm_id);
        assert_ne!(stale.default_id, stale.close_id);

        let stop = super::stop_update_dialog();
        assert_eq!(stop.heading, "Stop This Update?");
        assert_eq!(
            stop.body,
            "Stopping while packages are being installed can leave the package database incomplete. Check package history before trying again."
        );
        assert_eq!(stop.close_id, "wait");
        assert_eq!(stop.close_label, "_Keep Waiting");
        assert_eq!(stop.confirm_id, "stop");
        assert_eq!(stop.confirm_label, "Stop _Anyway");
        assert!(stop.confirm_destructive);
        assert!(!stop.confirm_suggested);
        assert_eq!(stop.default_id, "wait");
        assert_eq!(stop.default_id, stop.close_id);
        assert_ne!(stop.default_id, stop.confirm_id);
    }

    #[test]
    fn fail_retry_label_follows_the_action() {
        use fedora_updater::model::RetryAction;
        assert_eq!(
            super::fail_retry_label(RetryAction::CheckAgain),
            "Check Again"
        );
        assert_eq!(super::fail_retry_label(RetryAction::Reboot), "Restart");
    }

    fn running_after_session_ready() -> fedora_updater::state::AppState {
        use fedora_updater::model::{AuthPurpose, Package};
        use fedora_updater::state::{AppState, Event, Phase};
        let mut s = AppState::default();
        s.phase = Phase::Ready {
            packages: vec![Package::new_dnf("glibc", "x86_64", "1", "updates")],
            soft_errors: vec![],
        };
        let s = fedora_updater::state::reduce(s, Event::StartApply).unwrap();
        fedora_updater::state::reduce(
            s,
            Event::SessionReady {
                purpose: AuthPurpose::Apply,
            },
        )
        .unwrap()
    }

    #[test]
    fn recovery_probe_commit_writes_inflight_and_panics_if_a_borrow_is_held() {
        use fedora_updater::state::{Phase, RecoveryLatch};
        use std::cell::RefCell;
        use std::panic::{catch_unwind, AssertUnwindSafe};
        use std::rc::Rc;

        let state = Rc::new(RefCell::new(running_after_session_ready()));
        assert!(
            matches!(
                state.borrow().phase,
                Phase::Running {
                    recovery: RecoveryLatch::Idle,
                    ..
                }
            ),
            "SessionReady for Apply starts the latch Idle"
        );
        assert!(super::commit_recovery_probe_started(&state));
        assert!(matches!(
            state.borrow().phase,
            Phase::Running {
                recovery: RecoveryLatch::InFlight,
                ..
            }
        ));
        assert!(
            !super::commit_recovery_probe_started(&state),
            "a second start leaves InFlight and does not spawn another probe"
        );

        let state = Rc::new(RefCell::new(running_after_session_ready()));
        let held = state.borrow();
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            super::commit_recovery_probe_started(&state);
        }));
        std::panic::set_hook(prev_hook);
        assert!(
            panicked.is_err(),
            "a held Ref must panic on the latch write, not skip the probe"
        );
        drop(held);
        assert!(
            matches!(
                state.borrow().phase,
                Phase::Running {
                    recovery: RecoveryLatch::Idle,
                    ..
                }
            ),
            "the panicked write must leave the latch Idle"
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
