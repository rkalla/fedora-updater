//! Main application window — thin view over the pure state machine.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use gtk::glib;
use gtk::glib::object::IsA;
use gtk::prelude::*;
use gtk::{Builder, Button, Label, ProgressBar, Revealer, Stack, TextView};
use libadwaita::prelude::*;
use libadwaita::{
    Application, ApplicationWindow, Banner, ButtonContent, ExpanderRow, PreferencesGroup, Spinner,
    StatusPage, StyleManager, Toast, ToastOverlay, Window as AdwWindow, WrapBox,
};

use fedora_updater::backend::dnf::{self, InstallCheck};
use fedora_updater::model::{
    about_time_left, cache_activity_note, check_progress, count_by_kind, count_by_source,
    idle_chrome, now_playing_subtitle, now_playing_title, quiet_ui, run_heading,
    running_progress_captions, source_summary_line, AuthPurpose, IdleScreen, Package, RetryAction,
    RunPass, StageCount, UpdateSource, WorkerEvent,
};
use fedora_updater::orchestrator::{
    can_skip_auth_ui, cancel_background_work, release_privileges, run_apply_all, run_check_all,
    run_systemctl_reboot, session_started_event, worker_to_state_events,
};
use fedora_updater::privilege::{
    dnf_daemon_is_running, helper_activity, kill_recorded_helper, run_local_command, HelperActivity,
};
use fedora_updater::state::{self, session_started_purpose, AppState, Event, Phase};

use super::{preview, rows};

const WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/window.ui"));
const LOG_WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/log_window.ui"));
const DEFAULT_WIDTH: i32 = 760;
const DEFAULT_HEIGHT: i32 = 660;
const STALE_UPDATE_LIST_PROMPT: &str = "The list of updates is older than 24hrs - I need to refresh this list before applying the updates. Do you want to do that now?";

struct Widgets {
    window: ApplicationWindow,
    stack: Stack,
    toast: ToastOverlay,
    idle_page: StatusPage,
    idle_status: Label,
    idle_btn: Button,
    idle_btn_content: ButtonContent,
    idle_hint: Label,
    /// True only on the first-run "Check for Updates" page, so Enter starts a check.
    focus_idle_check: Cell<bool>,
    auth_page: StatusPage,
    check_footnote: Label,
    check_progress: ProgressBar,
    check_progress_meta: Label,
    check_progress_pct: Label,
    check_group: PreferencesGroup,
    check_list_scroll: gtk::ScrolledWindow,
    check_rows: RefCell<Vec<gtk::Widget>>,
    check_console_line: Label,
    check_console_full: Label,
    ready_title: Label,
    ready_footnote: Label,
    ready_banner: Banner,
    ready_chips: WrapBox,
    ready_group: PreferencesGroup,
    ready_rows: RefCell<Vec<gtk::Widget>>,
    run_title: Label,
    run_footnote: Label,
    run_stop: Button,
    run_stall: Banner,
    run_progress: ProgressBar,
    run_progress_meta: Label,
    run_progress_pct: Label,
    now_playing_title: Label,
    now_playing_subtitle: Label,
    now_playing_pct: Label,
    now_playing_bar: ProgressBar,
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
    done_icon: gtk::Image,
    done_title: Label,
    done_subtitle: Label,
    done_installed_scroll: gtk::ScrolledWindow,
    done_installed: PreferencesGroup,
    done_installed_rows: RefCell<Vec<gtk::Widget>>,
    done_upgraded: Label,
    done_failed: Label,
    done_failed_card: gtk::Box,
    done_reboot_lbl: Label,
    done_reboot_card: gtk::Box,
    reboot_box: gtk::Box,
    fail_page: StatusPage,
    fail_detail: Label,
    /// Last byte from the backend, including a carriage-return redraw.
    /// `None` until this run paints the Running page, so auth time is not quiet time.
    last_output: Cell<Option<Instant>>,
    cache_cursor: RefCell<Option<(PathBuf, u64)>>,
    reconcile_inflight: Cell<bool>,
    reconcile_not_before: Cell<Option<Instant>>,
    dnf_reconciled: Cell<bool>,
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
        stack: obj(&builder, "stack"),
        toast: obj(&builder, "toast"),
        idle_page: obj(&builder, "idle_page"),
        idle_status: obj(&builder, "idle_status"),
        idle_btn: obj(&builder, "idle_btn"),
        idle_btn_content: obj(&builder, "idle_btn_content"),
        idle_hint: obj(&builder, "idle_hint"),
        focus_idle_check: Cell::new(false),
        auth_page: obj(&builder, "auth_page"),
        check_footnote: obj(&builder, "check_footnote"),
        check_progress: obj(&builder, "check_progress"),
        check_progress_meta: obj(&builder, "check_progress_meta"),
        check_progress_pct: obj(&builder, "check_progress_pct"),
        check_group: obj(&builder, "check_group"),
        check_list_scroll: obj(&builder, "check_list_scroll"),
        check_rows: RefCell::new(Vec::new()),
        check_console_line: obj(&builder, "check_console_line"),
        check_console_full: obj(&builder, "check_console_full"),
        ready_title: obj(&builder, "ready_title"),
        ready_footnote: obj(&builder, "ready_footnote"),
        ready_banner: obj(&builder, "ready_banner"),
        ready_chips: obj(&builder, "ready_chips"),
        ready_group: obj(&builder, "ready_group"),
        ready_rows: RefCell::new(Vec::new()),
        run_title: obj(&builder, "run_title"),
        run_footnote: obj(&builder, "run_footnote"),
        run_stop: obj(&builder, "run_stop"),
        run_stall: obj(&builder, "run_stall"),
        run_progress: obj(&builder, "run_progress"),
        run_progress_meta: obj(&builder, "run_progress_meta"),
        run_progress_pct: obj(&builder, "run_progress_pct"),
        now_playing_title: obj(&builder, "now_playing_title"),
        now_playing_subtitle: obj(&builder, "now_playing_subtitle"),
        now_playing_pct: obj(&builder, "now_playing_pct"),
        now_playing_bar: obj(&builder, "now_playing_bar"),
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
        done_icon: obj(&builder, "done_icon"),
        done_title: obj(&builder, "done_title"),
        done_subtitle: obj(&builder, "done_subtitle"),
        done_installed_scroll: obj(&builder, "done_installed_scroll"),
        done_installed: obj(&builder, "done_installed"),
        done_installed_rows: RefCell::new(Vec::new()),
        done_upgraded: obj(&builder, "done_upgraded"),
        done_failed: obj(&builder, "done_failed"),
        done_failed_card: obj(&builder, "done_failed_card"),
        done_reboot_lbl: obj(&builder, "done_reboot_lbl"),
        done_reboot_card: obj(&builder, "done_reboot_card"),
        reboot_box: obj(&builder, "reboot_box"),
        fail_page: obj(&builder, "fail_page"),
        fail_detail: obj(&builder, "fail_detail"),
        last_output: Cell::new(None),
        cache_cursor: RefCell::new(None),
        reconcile_inflight: Cell::new(false),
        reconcile_not_before: Cell::new(None),
        dnf_reconciled: Cell::new(false),
        reconcile_tx,
    });

    let idle_btn: Button = obj(&builder, "idle_btn");
    let ready_refresh: Button = obj(&builder, "ready_refresh");
    let ready_update: Button = obj(&builder, "ready_update");
    let auth_cancel: Button = obj(&builder, "auth_cancel");
    let reboot_btn: Button = obj(&builder, "reboot_btn");
    let done_btn: Button = obj(&builder, "done_btn");
    let done_log_btn: Button = obj(&builder, "done_log_btn");
    let fail_retry: Button = obj(&builder, "fail_retry");
    let fail_log: Button = obj(&builder, "fail_log");
    let fail_back: Button = obj(&builder, "fail_back");
    let check_console_toggle: Button = obj(&builder, "check_console_toggle");
    let check_console_reveal: Revealer = obj(&builder, "check_console_reveal");
    let check_console_chevron: Label = obj(&builder, "check_console_chevron");
    let run_console_toggle: Button = obj(&builder, "run_console_toggle");
    let run_console_reveal: Revealer = obj(&builder, "run_console_reveal");
    let run_console_chevron: Label = obj(&builder, "run_console_chevron");

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
        ready_refresh.connect_clicked(move |_| {
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
            if pass_is_apply {
                prompt_stop_during_transaction(&widgets, &state);
            } else if matches!(state.borrow().phase, Phase::Running { .. }) {
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
        reboot_btn.connect_clicked(move |_| {
            widgets.toast.add_toast(Toast::new("Rebooting…"));
            run_systemctl_reboot(tx.clone());
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        fail_back.connect_clicked(move |_| {
            dispatch(&state, &widgets, Event::ResetToIdle { message: None });
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
    widgets.reconcile_inflight.set(false);
    widgets.reconcile_not_before.set(None);
    widgets.dnf_reconciled.set(false);
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
    let dialog = libadwaita::AlertDialog::new(Some(STALE_UPDATE_LIST_PROMPT), None);
    dialog.set_prefer_wide_layout(true);
    dialog.add_response("no", "No");
    dialog.add_response("yes", "Yes");
    dialog.set_response_appearance("yes", libadwaita::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("yes"));
    dialog.set_close_response("no");

    let state = state.clone();
    let widgets_for_cb = widgets.clone();
    dialog.connect_response(None, move |_dialog, response| {
        if response == "yes" {
            start_check(&state, &widgets_for_cb, tx.clone(), true);
        }
    });
    dialog.present(Some(&widgets.window));
}

fn show_log_window(parent: &impl IsA<gtk::Widget>, log: &str) {
    let builder = Builder::from_string(LOG_WINDOW_UI);
    let win: AdwWindow = obj(&builder, "log_window");
    let view: TextView = obj(&builder, "log_view");
    let copy_btn: Button = obj(&builder, "copy_btn");
    let close_btn: Button = obj(&builder, "close_btn");

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
        let win = win.clone();
        close_btn.connect_clicked(move |_| {
            win.close();
        });
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
            if let Some(purpose) = session_started_purpose(&state.borrow().phase) {
                dispatch_worker(state, widgets, session_started_event(purpose));
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

    if check_finished && state.borrow().apply_when_ready && can_start_apply(&state.borrow()) {
        begin_apply(state, widgets, tx.clone());
    }
}

fn wire_console(toggle: &Button, reveal: &Revealer, chevron: &Label) {
    let reveal = reveal.clone();
    let chevron = chevron.clone();
    toggle.connect_clicked(move |_| {
        let open = !reveal.reveals_child();
        reveal.set_reveal_child(open);
        chevron.set_text(if open { "▾" } else { "▸" });
        if open {
            chevron.add_css_class("console-open");
        } else {
            chevron.remove_css_class("console-open");
        }
    });
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
    if package.status == fedora_updater::PackageStatus::Completed {
        rows::completed_row(package)
    } else {
        rows::package_row(package, true)
    }
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
    w.run_footnote.set_text(&foot);

    if quiet.show_spinner || work_progress.is_none() {
        w.now_playing_spinner.set_opacity(1.0);
    } else {
        w.now_playing_spinner.set_opacity(0.0);
    }

    let banner = quiet.banner.as_ref().map(|text| {
        if text.starts_with("No output") && dnf_daemon_is_running() {
            format!("{text} The update is waiting on GNOME Software's dnf daemon.")
        } else {
            text.clone()
        }
    });
    if let Some(text) = banner {
        w.run_stall.set_title(&text);
        w.run_stall.set_revealed(true);
    } else {
        w.run_stall.set_revealed(false);
    }

    if quiet.reconcile && watching_dnf {
        maybe_reconcile_dnf(w, packages);
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

fn maybe_reconcile_dnf(w: &Widgets, packages: &[Package]) {
    if w.reconcile_inflight.get() || w.dnf_reconciled.get() {
        return;
    }
    if w.reconcile_not_before
        .get()
        .is_some_and(|at| Instant::now() < at)
    {
        return;
    }
    let dnf_pkgs: Vec<Package> = packages
        .iter()
        .filter(|p| p.source == UpdateSource::Dnf)
        .cloned()
        .collect();
    if dnf_pkgs.is_empty() {
        return;
    }
    w.reconcile_inflight.set(true);
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
    widgets.reconcile_inflight.set(false);
    let (packages, current_source, needs_reboot) = {
        let borrowed = state.borrow();
        let Phase::Running {
            packages,
            current_source,
            needs_reboot,
            ..
        } = &borrowed.phase
        else {
            return;
        };
        (packages.clone(), *current_source, *needs_reboot)
    };
    if current_source != Some(UpdateSource::Dnf) {
        return;
    }
    match check {
        InstallCheck::Unknown => {
            widgets
                .reconcile_not_before
                .set(Some(Instant::now() + Duration::from_secs(30)));
        }
        InstallCheck::AllInstalled => {
            if has_sources_after_dnf(&packages) {
                if widgets.dnf_reconciled.replace(true) {
                    return;
                }
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
    let dialog = libadwaita::AlertDialog::new(
        Some("Stop this update?"),
        Some("Stopping dnf while it is applying packages can leave the rpm database half-applied. Check dnf history before running Update All again."),
    );
    dialog.add_response("wait", "Keep waiting");
    dialog.add_response("stop", "Stop anyway");
    dialog.set_response_appearance("stop", libadwaita::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("wait"));
    dialog.set_close_response("wait");

    let state = state.clone();
    let widgets_for_cb = widgets.clone();
    dialog.connect_response(None, move |_dialog, response| {
        if response != "stop" {
            return;
        }
        let during = matches!(
            state.borrow().phase,
            Phase::Running {
                pass: RunPass::Apply,
                ..
            }
        );
        if matches!(state.borrow().phase, Phase::Running { .. }) {
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
    w.run_title.set_text(run_heading(phase_label, pass));
    if pass == RunPass::Apply {
        w.run_progress.add_css_class("stage-apply");
    } else {
        w.run_progress.remove_css_class("stage-apply");
    }
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
    w.now_playing_bar.set_visible(true);
    if let Some(frac) = work_progress {
        let frac = frac.clamp(0.0, 1.0);
        w.now_playing_spinner.set_opacity(0.0);
        w.now_playing_bar.set_fraction(frac);
        w.now_playing_pct.set_text(&format!("{:.0}%", frac * 100.0));
    } else {
        w.now_playing_spinner.set_opacity(1.0);
        w.now_playing_bar.set_fraction(0.0);
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

    match &s.phase {
        Phase::Idle(screen) => {
            clear_running_lists(w);
            restore_default_window_size(&w.window);
            w.stack
                .set_visible_child_full("idle", gtk::StackTransitionType::None);
            let chrome = idle_chrome(screen);
            w.idle_page.set_title(chrome.title);
            w.idle_page.set_icon_name(Some(chrome.icon));
            w.idle_btn_content.set_label(chrome.button);
            w.idle_btn_content.set_icon_name(chrome.button_icon);
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
            w.idle_hint.set_visible(chrome.hint_visible);
            w.idle_page.set_description(chrome.description.as_deref());
            w.idle_status.set_text(&chrome.footer);
            if matches!(screen, IdleScreen::Fresh) {
                arm_idle_check_focus(w);
            }
        }
        Phase::Authenticating { purpose, .. } => {
            w.stack.set_visible_child_name("auth");
            w.auth_page.set_description(Some(&format!(
                "A system dialog will ask for your password once for this app session so it can {purpose} via a privileged helper"
            )));
        }
        Phase::Checking {
            packages_so_far,
            checked_sources,
            ..
        } => {
            w.stack.set_visible_child_name("checking");
            let (frac, groups_done, status) = check_progress(checked_sources);
            let mut foot = format!("{groups_done} of 3 sources");
            if let Some(eta) = about_time_left(elapsed, frac) {
                foot = format!("{eta} · {foot}");
            }
            w.check_footnote.set_text(&foot);
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
            let n = packages.len();
            w.ready_title.set_text(&format!(
                "{n} update{} available",
                if n == 1 { "" } else { "s" }
            ));
            let (dnf, fp, fw) = count_by_source(packages);
            w.ready_footnote.set_text(&source_summary_line(packages));

            if soft_errors.is_empty() {
                w.ready_banner.set_revealed(false);
            } else {
                w.ready_banner.set_title(&soft_errors.join(" · "));
                w.ready_banner.set_button_label(Some("View log"));
                w.ready_banner.set_revealed(true);
            }

            let (sec, bug, enh) = count_by_kind(packages);
            let mut chips = Vec::new();
            let dnf_s = format!("{dnf} system");
            let fp_s = format!("{fp} apps");
            let fw_s = format!("{fw} firmware");
            let sec_s = format!("{sec} security");
            let bug_s = format!("{bug} bugfix");
            let enh_s = format!("{enh} enhancement");
            if dnf > 0 {
                chips.push((dnf_s.as_str(), "src-dnf"));
            }
            if fp > 0 {
                chips.push((fp_s.as_str(), "src-flatpak"));
            }
            if fw > 0 {
                chips.push((fw_s.as_str(), "src-firmware"));
            }
            if sec > 0 {
                chips.push((sec_s.as_str(), "security"));
            }
            if bug > 0 {
                chips.push((bug_s.as_str(), "bugfix"));
            }
            if enh > 0 {
                chips.push((enh_s.as_str(), "enhance"));
            }
            rows::refill_chips(&w.ready_chips, &chips);

            rows::refill_group(
                &w.ready_group,
                &w.ready_rows,
                packages.iter().map(|p| rows::package_row(p, false)),
            );
        }
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
        } => {
            render_running(
                w,
                packages,
                *overall_progress,
                phase_label,
                *pass,
                *stage,
                *current_source,
                *active_index,
                current_name.as_deref(),
                *work_progress,
                elapsed,
                &last_console,
                &full_console,
            );
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
            if *failed > 0 {
                w.done_title.set_text("Updates finished with problems");
                w.done_icon.set_icon_name(Some("dialog-warning-symbolic"));
                w.done_subtitle.set_text(&format!(
                    "{upgraded} applied · {failed} failed · {duration}"
                ));
                w.done_failed.add_css_class("error");
                w.done_failed_card.add_css_class("error");
            } else {
                w.done_title.set_text("All updates applied");
                w.done_icon.set_icon_name(Some("object-select-symbolic"));
                w.done_subtitle.set_text(&format!(
                    "{upgraded} upgraded · {dnf} system / {fp} apps / {fw} firmware · {duration}"
                ));
                w.done_failed.remove_css_class("error");
                w.done_failed_card.remove_css_class("error");
            }
            w.done_upgraded.set_text(&upgraded.to_string());
            w.done_failed.set_text(&failed.to_string());
            w.done_reboot_lbl
                .set_text(if *needs_reboot { "1" } else { "0" });
            if *needs_reboot {
                w.done_reboot_card.add_css_class("warn");
            } else {
                w.done_reboot_card.remove_css_class("warn");
            }
            w.reboot_box.set_visible(*needs_reboot);
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
                listed.into_iter().map(finished_row),
            );
            w.done_installed_scroll.set_visible(!packages.is_empty());
        }
        Phase::Failed { title, detail, .. } => {
            clear_running_lists(w);
            restore_default_window_size(&w.window);
            w.stack
                .set_visible_child_full("failed", gtk::StackTransitionType::None);
            w.fail_page.set_title(title);
            w.fail_page.set_description(None);
            w.fail_detail.set_text(detail);
            w.fail_detail.set_visible(!detail.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn window_template_has_adwaita_pages() {
        let xml = super::WINDOW_UI;
        for id in [
            "idle_page",
            "auth_page",
            "check_group",
            "ready_banner",
            "ready_chips",
            "ready_group",
            "now_playing",
            "run_title",
            "run_stop",
            "run_stall",
            "now_playing_title",
            "run_list_scroll",
            "run_list_group",
            "run_completed",
            "done_title",
            "done_icon",
            "done_subtitle",
            "done_installed",
            "done_installed_scroll",
            "fail_page",
            "reboot_btn",
        ] {
            assert!(
                xml.contains(&format!("id=\"{id}\"")),
                "window.ui missing id={id}"
            );
        }
        assert!(xml.contains("AdwPreferencesGroup"));
        assert!(xml.contains("AdwBanner"));
        assert!(xml.contains("AdwStatusPage"));
        assert!(xml.contains("AdwSpinner"));
        assert!(
            xml.contains("vhomogeneous"),
            "stack must size every page to the window so Done/Idle fill after apply"
        );
        assert!(
            xml.contains("id=\"idle_page\"") && xml.contains("vexpand"),
            "idle status page must expand to fill the stack"
        );
        assert!(
            !xml.contains("One password prompt"),
            "the idle page hint already covers what the check looks at"
        );
        let installed_at = xml.find("id=\"done_installed\"").expect("done_installed");
        let done_btn_at = xml.find("id=\"done_btn\"").expect("done_btn");
        assert!(
            installed_at < done_btn_at,
            "installed list sits above the summary actions"
        );
        let now_at = xml.find("id=\"now_playing\"").expect("now_playing");
        let stall_at = xml.find("id=\"run_stall\"").expect("run_stall");
        let scroll_at = xml.find("id=\"run_list_scroll\"").expect("run_list_scroll");
        let list_at = xml.find("id=\"run_list_group\"").expect("run_list_group");
        let completed_at = xml.find("id=\"run_completed\"").expect("run_completed");
        assert!(
            stall_at < now_at,
            "stall banner sits above the now-playing card"
        );
        assert!(
            now_at < scroll_at,
            "now-playing card must stay sticky above the checklist scroll"
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
    }

    #[test]
    fn log_template_has_text_view() {
        let xml = super::LOG_WINDOW_UI;
        assert!(xml.contains("id=\"log_view\""));
        assert!(xml.contains("id=\"copy_btn\""));
    }
}
