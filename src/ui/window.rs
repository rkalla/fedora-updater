//! Main application window — thin view over the pure state machine.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::glib::object::IsA;
use gtk::prelude::*;
use gtk::{Builder, Button, Label, ProgressBar, Revealer, Stack, TextView};
use libadwaita::prelude::*;
use libadwaita::{
    Application, ApplicationWindow, Banner, ButtonContent, ExpanderRow, PreferencesGroup, Spinner,
    StatusPage, StyleManager, Toast, ToastOverlay, Window as AdwWindow, WrapBox,
};

use fedora_updater::model::{
    about_time_left, check_progress, count_by_kind, count_by_source, now_playing_subtitle,
    now_playing_title, source_summary_line, AuthPurpose, Package, UpdateSource, WorkerEvent,
};
use fedora_updater::orchestrator::{
    can_skip_auth_ui, cancel_background_work, release_privileges, run_apply_all, run_check_all,
    run_systemctl_reboot, session_started_event, worker_to_state_events,
};
use fedora_updater::state::{self, AppState, Event, Phase};

use super::{preview, rows};

const WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/window.ui"));
const LOG_WINDOW_UI: &str = include_str!(concat!(env!("OUT_DIR"), "/log_window.ui"));
const DEFAULT_WIDTH: i32 = 760;
const DEFAULT_HEIGHT: i32 = 660;

struct Widgets {
    window: ApplicationWindow,
    stack: Stack,
    toast: ToastOverlay,
    idle_page: StatusPage,
    idle_status: Label,
    idle_btn: Button,
    idle_btn_content: ButtonContent,
    idle_hint: Label,
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
    run_footnote: Label,
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
    done_page: StatusPage,
    done_upgraded: Label,
    done_failed: Label,
    done_failed_card: gtk::Box,
    done_reboot_lbl: Label,
    done_reboot_card: gtk::Box,
    reboot_box: gtk::Box,
    fail_page: StatusPage,
    fail_detail: Label,
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

    let widgets = Rc::new(Widgets {
        window: window.clone(),
        stack: obj(&builder, "stack"),
        toast: obj(&builder, "toast"),
        idle_page: obj(&builder, "idle_page"),
        idle_status: obj(&builder, "idle_status"),
        idle_btn: obj(&builder, "idle_btn"),
        idle_btn_content: obj(&builder, "idle_btn_content"),
        idle_hint: obj(&builder, "idle_hint"),
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
        run_footnote: obj(&builder, "run_footnote"),
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
        done_page: obj(&builder, "done_page"),
        done_upgraded: obj(&builder, "done_upgraded"),
        done_failed: obj(&builder, "done_failed"),
        done_failed_card: obj(&builder, "done_failed_card"),
        done_reboot_lbl: obj(&builder, "done_reboot_lbl"),
        done_reboot_card: obj(&builder, "done_reboot_card"),
        reboot_box: obj(&builder, "reboot_box"),
        fail_page: obj(&builder, "fail_page"),
        fail_detail: obj(&builder, "fail_detail"),
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
    let pending_purpose = Rc::new(RefCell::new(AuthPurpose::Check));

    {
        let state = state.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        let pending_purpose = pending_purpose.clone();
        idle_btn.connect_clicked(move |_| {
            start_check(&state, &widgets, &pending_purpose, tx.clone());
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        let pending_purpose = pending_purpose.clone();
        ready_refresh.connect_clicked(move |_| {
            start_check(&state, &widgets, &pending_purpose, tx.clone());
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        let pending_purpose = pending_purpose.clone();
        fail_retry.connect_clicked(move |_| {
            start_check(&state, &widgets, &pending_purpose, tx.clone());
        });
    }
    {
        let state = state.clone();
        let widgets = widgets.clone();
        let tx = tx.clone();
        let pending_purpose = pending_purpose.clone();
        ready_update.connect_clicked(move |_| {
            *pending_purpose.borrow_mut() = AuthPurpose::Apply;
            let packages = state.borrow().packages().to_vec();
            dispatch(&state, &widgets, Event::StartApply);
            if can_skip_auth_ui() {
                dispatch(&state, &widgets, session_started_event(AuthPurpose::Apply));
            }
            run_apply_all(tx.clone(), packages);
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
        let pending_purpose = pending_purpose.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                handle_worker(&state, &widgets, &pending_purpose, ev);
            }
        });
    }

    window.connect_close_request(move |_| {
        release_privileges();
        glib::Propagation::Proceed
    });

    render(&state, &widgets);
    maybe_open_preview_completed(&widgets);
    window.present();
    preview::maybe_schedule_screenshot(&window);
}

fn start_check(
    state: &Rc<RefCell<AppState>>,
    widgets: &Rc<Widgets>,
    pending_purpose: &Rc<RefCell<AuthPurpose>>,
    tx: async_channel::Sender<WorkerEvent>,
) {
    *pending_purpose.borrow_mut() = AuthPurpose::Check;
    dispatch(state, widgets, Event::StartCheck);
    if can_skip_auth_ui() {
        dispatch(state, widgets, session_started_event(AuthPurpose::Check));
    }
    run_check_all(tx);
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
    pending_purpose: &Rc<RefCell<AuthPurpose>>,
    ev: WorkerEvent,
) {
    match &ev {
        WorkerEvent::SessionStarted => {
            let purpose = *pending_purpose.borrow();
            dispatch_worker(state, widgets, session_started_event(purpose));
            return;
        }
        WorkerEvent::Line { .. } => {
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
        WorkerEvent::Failed { .. } => {}
        WorkerEvent::ApplyAllFinished { .. } => {}
        WorkerEvent::CheckAllFinished { .. } => {}
        _ => {}
    }

    for e in worker_to_state_events(ev) {
        dispatch_worker(state, widgets, e);
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

fn render_running(
    w: &Widgets,
    packages: &[Package],
    overall_progress: f64,
    phase_label: &str,
    current_source: Option<UpdateSource>,
    active_index: Option<usize>,
    current_name: Option<&str>,
    work_progress: Option<f64>,
    elapsed: u64,
    last_console: &str,
    full_console: &str,
) {
    w.stack.set_visible_child_name("running");
    let done = packages.iter().filter(|p| p.status.is_done()).count();
    let remaining = packages.len().saturating_sub(done);
    let src = current_source.map(|s| s.label()).unwrap_or("all sources");
    let mut foot = format!("{src} · {remaining} of {} remaining", packages.len());
    if let Some(eta) = about_time_left(elapsed, overall_progress) {
        foot = format!("{eta} · {foot}");
    }
    w.run_footnote.set_text(&foot);
    w.run_progress.set_fraction(overall_progress);
    w.run_progress_meta
        .set_text(&format!("{done} done · {remaining} remaining"));
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
}

fn render(state: &Rc<RefCell<AppState>>, w: &Rc<Widgets>) {
    let s = state.borrow();
    let last_console = s.console.last_line().to_string();
    let full_console = s.console.full_text();
    let elapsed = s
        .stopwatch
        .as_ref()
        .map(|sw| sw.elapsed_secs())
        .unwrap_or(0);

    match &s.phase {
        Phase::Idle {
            last_checked,
            message,
        } => {
            clear_running_lists(w);
            restore_default_window_size(&w.window);
            w.stack
                .set_visible_child_full("idle", gtk::StackTransitionType::None);
            let checked_at = last_checked.as_deref();
            let up_to_date = last_checked.is_some() || message.is_some();
            if up_to_date {
                w.idle_page.set_title("System is up to date");
                w.idle_page.set_icon_name(Some("object-select-symbolic"));
                w.idle_btn_content.set_label("Check Again");
                w.idle_btn_content.set_icon_name("view-refresh-symbolic");
                w.idle_btn.remove_css_class("suggested-action");
                w.idle_btn.remove_css_class("pill");
                w.idle_btn.add_css_class("flat");
                w.idle_hint.set_visible(false);
                let subtitle = match message.as_deref() {
                    Some(m) if m.starts_with("No updates") && m.contains('(') => m.to_string(),
                    Some(m) if m == "System updated" => checked_at
                        .map(|t| format!("Last checked {t}"))
                        .unwrap_or_else(|| "Last checked just now".into()),
                    Some(m) if !m.starts_with("No updates") => m.to_string(),
                    _ => checked_at
                        .map(|t| format!("Last checked {t}"))
                        .unwrap_or_else(|| "Last checked just now".into()),
                };
                w.idle_page.set_description(Some(subtitle.as_str()));
            } else {
                w.idle_page.set_title("Check for updates");
                w.idle_page
                    .set_icon_name(Some("software-update-available-symbolic"));
                w.idle_btn_content.set_label("Check for Updates");
                w.idle_btn_content
                    .set_icon_name("software-update-available-symbolic");
                w.idle_btn.remove_css_class("flat");
                w.idle_btn.add_css_class("suggested-action");
                w.idle_btn.add_css_class("pill");
                w.idle_hint.set_visible(true);
                w.idle_page.set_description(Some(
                    "System, apps, and firmware. One password prompt for this session",
                ));
            }
            let footer = checked_at
                .map(|t| format!("Ready · last checked {t}"))
                .unwrap_or_else(|| "Last checked never".into());
            w.idle_status.set_text(&footer);
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
            ..
        } => {
            render_running(
                w,
                packages,
                *overall_progress,
                phase_label,
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
                w.done_page.set_title("Updates finished with problems");
                w.done_page.set_icon_name(Some("dialog-warning-symbolic"));
                w.done_page.set_description(Some(&format!(
                    "{upgraded} applied · {failed} failed · {duration}"
                )));
                w.done_failed.add_css_class("error");
                w.done_failed_card.add_css_class("error");
            } else {
                w.done_page.set_title("All updates applied");
                w.done_page.set_icon_name(Some("object-select-symbolic"));
                w.done_page.set_description(Some(&format!(
                    "{upgraded} upgraded · {dnf} system / {fp} apps / {fw} firmware · {duration}"
                )));
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
        }
        Phase::Failed { title, detail } => {
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
            "now_playing_title",
            "run_list_scroll",
            "run_list_group",
            "run_completed",
            "done_page",
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
            xml.contains("id=\"done_page\"") && xml.contains("vexpand"),
            "status pages must expand to fill the stack"
        );
        let now_at = xml.find("id=\"now_playing\"").expect("now_playing");
        let scroll_at = xml.find("id=\"run_list_scroll\"").expect("run_list_scroll");
        let list_at = xml.find("id=\"run_list_group\"").expect("run_list_group");
        let completed_at = xml.find("id=\"run_completed\"").expect("run_completed");
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
