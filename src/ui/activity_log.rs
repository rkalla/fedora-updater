//! Embedded searchable/filterable Activity Log view over `ConsoleBuffer`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use fedora_updater::console::{ConsoleBuffer, ConsoleDelta};
use fedora_updater::presenter::activity_log_subtitle;

/// Source toggle options shown in the Activity Log header strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogSourceFilter {
    #[default]
    All,
    Dnf,
    Flatpak,
    Extensions,
    Brew,
    Fwupd,
}

impl LogSourceFilter {
    #[cfg(test)]
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Dnf => "dnf",
            Self::Flatpak => "flatpak",
            Self::Extensions => "gnome-ext",
            Self::Brew => "brew",
            Self::Fwupd => "fwupd",
        }
    }

    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("dnf") => Self::Dnf,
            Some("flatpak") => Self::Flatpak,
            Some("gnome-ext") => Self::Extensions,
            Some("brew") => Self::Brew,
            Some("fwupd") => Self::Fwupd,
            _ => Self::All,
        }
    }
}

/// Extracts the leading source bracket `[tag]` when valid (`dnf`, `flatpak-user`,
/// `flatpak-system`, `gnome-ext`, `brew`, `fwupd`), ignoring bracketed progress
/// counters like `[1/4]`.
pub fn parse_log_tag(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    let tag = &rest[..end];
    if tag.is_empty()
        || !tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return None;
    }
    Some(tag)
}

/// True when `line` belongs to `filter`.
pub fn line_matches_source(line: &str, filter: LogSourceFilter) -> bool {
    match filter {
        LogSourceFilter::All => true,
        LogSourceFilter::Dnf => matches!(parse_log_tag(line), Some("dnf")),
        LogSourceFilter::Flatpak => {
            matches!(
                parse_log_tag(line),
                Some("flatpak" | "flatpak-user" | "flatpak-system")
            )
        }
        LogSourceFilter::Extensions => matches!(parse_log_tag(line), Some("gnome-ext")),
        LogSourceFilter::Brew => matches!(parse_log_tag(line), Some("brew")),
        LogSourceFilter::Fwupd => matches!(parse_log_tag(line), Some("fwupd")),
    }
}

/// Combined source toggle + case-insensitive substring predicate for Activity Log lines.
pub fn line_matches_filter(line: &str, source: LogSourceFilter, query: &str) -> bool {
    if !line_matches_source(line, source) {
        return false;
    }
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower_line = line.to_ascii_lowercase();
    trimmed
        .split_whitespace()
        .all(|token| lower_line.contains(&token.to_ascii_lowercase()))
}

/// Filters lines from `console` and returns `(joined_text, matched_count, total_count)`.
pub fn filter_console_lines(
    console: &ConsoleBuffer,
    source: LogSourceFilter,
    query: &str,
) -> (String, usize, usize) {
    let total = console.len();
    if source == LogSourceFilter::All && query.trim().is_empty() {
        return (console.full_text(), total, total);
    }

    let mut matched = 0usize;
    let mut out = String::new();
    for line in console.lines() {
        if line_matches_filter(line, source, query) {
            if matched > 0 {
                out.push('\n');
            }
            out.push_str(line);
            matched += 1;
        }
    }
    (out, matched, total)
}

fn append_text_line(buffer: &gtk::TextBuffer, line: &str) {
    let mut end = buffer.end_iter();
    if buffer.char_count() > 0 {
        buffer.insert(&mut end, "\n");
    }
    buffer.insert(&mut end, line);
}

/// Incrementally mirrors [`ConsoleDelta`] operations onto a `Gtk.TextBuffer`.
pub(super) fn apply_text_buffer_delta(buffer: &gtk::TextBuffer, delta: ConsoleDelta, line: &str) {
    match delta {
        ConsoleDelta::Appended => append_text_line(buffer, line),
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
            append_text_line(buffer, line);
        }
    }
}

/// Embedded Activity Log workspace shown when `DetailView::ActivityLog` is active.
#[derive(Clone)]
pub struct ActivityLogView {
    root: gtk::Box,
    source_toggles: libadwaita::ToggleGroup,
    search_entry: gtk::SearchEntry,
    copy_btn: gtk::Button,
    content_stack: gtk::Stack,
    text_view: gtk::TextView,
    empty_page: libadwaita::StatusPage,
    status_label: gtk::Label,
    active_filter: Rc<Cell<LogSourceFilter>>,
    query: Rc<RefCell<String>>,
    matched_count: Rc<Cell<usize>>,
    total_count: Rc<Cell<usize>>,
}

impl ActivityLogView {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);

        // Top filter & clipboard control bar.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        bar.set_margin_top(10);
        bar.set_margin_bottom(10);
        bar.set_margin_start(14);
        bar.set_margin_end(14);

        let source_toggles = libadwaita::ToggleGroup::new();
        for (name, label) in [
            ("all", "All"),
            ("dnf", "DNF"),
            ("flatpak", "Flatpak"),
            ("gnome-ext", "Ext"),
            ("brew", "Brew"),
            ("fwupd", "fwupd"),
        ] {
            let toggle = libadwaita::Toggle::builder().name(name).label(label).build();
            source_toggles.add(toggle);
        }
        source_toggles.set_active_name(Some("all"));

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Filter log…")
            .hexpand(true)
            .search_delay(80)
            .build();

        let copy_btn = gtk::Button::builder()
            .label("Copy All")
            .valign(gtk::Align::Center)
            .sensitive(false)
            .tooltip_text("Copy visible log lines")
            .build();

        bar.append(&source_toggles);
        bar.append(&search_entry);
        bar.append(&copy_btn);

        let text_view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(14)
            .right_margin(14)
            .top_margin(12)
            .bottom_margin(12)
            .can_focus(true)
            .css_classes(["activity-log-view", "caption"])
            .build();

        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .child(&text_view)
            .build();

        let empty_page = libadwaita::StatusPage::builder()
            .icon_name("utilities-terminal-symbolic")
            .title("No activity logged yet")
            .description("Check or apply updates to capture command output here.")
            .vexpand(true)
            .build();

        let content_stack = gtk::Stack::builder()
            .vexpand(true)
            .hexpand(true)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        content_stack.add_named(&scroll, Some("log"));
        content_stack.add_named(&empty_page, Some("empty"));
        content_stack.set_visible_child_name("empty");

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer.set_margin_top(6);
        footer.set_margin_bottom(6);
        footer.set_margin_start(14);
        footer.set_margin_end(14);
        let status_label = gtk::Label::builder()
            .label("No output yet")
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["caption", "dimmed", "numeric"])
            .build();
        footer.append(&status_label);

        root.append(&bar);
        root.append(&content_stack);
        root.append(&footer);

        Self {
            root,
            source_toggles,
            search_entry,
            copy_btn,
            content_stack,
            text_view,
            empty_page,
            status_label,
            active_filter: Rc::new(Cell::new(LogSourceFilter::All)),
            query: Rc::new(RefCell::new(String::new())),
            matched_count: Rc::new(Cell::new(0)),
            total_count: Rc::new(Cell::new(0)),
        }
    }

    pub fn root(&self) -> &gtk::Box {
        &self.root
    }

    pub fn search_entry(&self) -> &gtk::SearchEntry {
        &self.search_entry
    }

    pub fn wire(
        &self,
        console: Rc<RefCell<ConsoleBuffer>>,
        on_counts_changed: impl Fn(usize, usize) + 'static,
    ) {
        let on_counts = Rc::new(on_counts_changed);

        {
            let this = self.clone();
            let console = console.clone();
            let on_counts = on_counts.clone();
            self.source_toggles.connect_active_name_notify(move |group| {
                let next = LogSourceFilter::from_name(group.active_name().as_deref());
                if this.active_filter.get() == next {
                    return;
                }
                this.active_filter.set(next);
                let (matched, total) = this.sync_full(&console.borrow());
                on_counts(matched, total);
            });
        }

        {
            let this = self.clone();
            let console = console.clone();
            let on_counts = on_counts.clone();
            self.search_entry.connect_changed(move |entry| {
                let next = entry.text().to_string();
                if *this.query.borrow() == next {
                    return;
                }
                *this.query.borrow_mut() = next;
                let (matched, total) = this.sync_full(&console.borrow());
                on_counts(matched, total);
            });
        }

        {
            let search = self.search_entry.clone();
            self.search_entry.connect_stop_search(move |_| {
                if !search.text().is_empty() {
                    search.set_text("");
                }
            });
        }

        {
            let text_view = self.text_view.clone();
            self.copy_btn.connect_clicked(move |btn| {
                let buf = text_view.buffer();
                let full = buf.text(&buf.start_iter(), &buf.end_iter(), false);
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&full);
                }
                btn.set_label("Copied");
                let btn = btn.clone();
                glib::timeout_add_local_once(Duration::from_millis(1600), move || {
                    btn.set_label("Copy All");
                });
            });
        }
    }

    fn update_chrome(&self, matched: usize, total: usize) {
        self.matched_count.set(matched);
        self.total_count.set(total);
        self.copy_btn.set_sensitive(matched > 0);
        let subtitle = activity_log_subtitle(matched, total);
        self.status_label.set_label(&subtitle);
        if matched > 0 {
            self.content_stack.set_visible_child_name("log");
        } else {
            if total == 0 {
                self.empty_page.set_title("No activity logged yet");
                self.empty_page
                    .set_description(Some("Check or apply updates to capture command output here."));
            } else {
                self.empty_page.set_title("No matching log lines");
                self.empty_page
                    .set_description(Some("Clear the filter toggles or search text to see captured output."));
            }
            self.content_stack.set_visible_child_name("empty");
        }
    }

    pub fn sync_full(&self, console: &ConsoleBuffer) -> (usize, usize) {
        let query = self.query.borrow();
        let (text, matched, total) = filter_console_lines(console, self.active_filter.get(), &query);
        self.text_view.buffer().set_text(&text);
        self.update_chrome(matched, total);
        (matched, total)
    }

    /// Applies an incremental [`ConsoleDelta`] when the Embedded Activity Log is visible.
    pub fn apply_delta(
        &self,
        console: &ConsoleBuffer,
        delta: ConsoleDelta,
        last_line: &str,
    ) -> (usize, usize) {
        let source = self.active_filter.get();
        let unfiltered = source == LogSourceFilter::All && self.query.borrow().trim().is_empty();
        if unfiltered {
            apply_text_buffer_delta(&self.text_view.buffer(), delta, last_line);
            let total = console.len();
            self.update_chrome(total, total);
            return (total, total);
        }

        if delta == ConsoleDelta::Appended {
            let matches = line_matches_filter(last_line, source, &self.query.borrow());
            if matches {
                append_text_line(&self.text_view.buffer(), last_line);
            }
            let matched = self.matched_count.get() + usize::from(matches);
            let total = console.len();
            self.update_chrome(matched, total);
            return (matched, total);
        }

        // ReplacedLast (\r redraw) or ring eviction while filtered: resync cleanly.
        self.sync_full(console)
    }

    pub fn clear(&self) {
        self.text_view.buffer().set_text("");
        self.update_chrome(0, 0);
    }

    #[cfg(test)]
    pub fn source_toggles(&self) -> &libadwaita::ToggleGroup {
        &self.source_toggles
    }

    #[cfg(test)]
    pub fn copy_btn(&self) -> &gtk::Button {
        &self.copy_btn
    }

    #[cfg(test)]
    pub fn content_stack(&self) -> &gtk::Stack {
        &self.content_stack
    }

    #[cfg(test)]
    pub fn active_filter(&self) -> LogSourceFilter {
        self.active_filter.get()
    }

    #[cfg(test)]
    pub fn current_text(&self) -> String {
        let buf = self.text_view.buffer();
        buf.text(&buf.start_iter(), &buf.end_iter(), false)
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_tag_recognizes_source_prefixes_and_ignores_stage_counters() {
        assert_eq!(parse_log_tag("[dnf] Repositories loaded."), Some("dnf"));
        assert_eq!(
            parse_log_tag("[flatpak-user] Syncing flathub"),
            Some("flatpak-user")
        );
        assert_eq!(
            parse_log_tag("[flatpak-system] Updating runtime"),
            Some("flatpak-system")
        );
        assert_eq!(parse_log_tag("[fwupd] Successfully installed"), Some("fwupd"));
        assert_eq!(parse_log_tag("[1/4] Installing kernel"), None);
        assert_eq!(parse_log_tag("[] empty"), None);
        assert_eq!(parse_log_tag("plain line"), None);
    }

    #[test]
    fn filter_console_lines_combines_source_query_and_carriage_return_updates() {
        let mut console = ConsoleBuffer::new(8_000);
        console.push_committed_tagged("dnf", "Repositories loaded.");
        console.push_live_tagged("dnf", "[1/2] openssl 40%");
        console.push_committed_tagged("dnf", "[1/2] openssl 100%");
        console.push_committed_tagged("dnf", "Complete!");
        console.push_committed_tagged("flatpak-user", "Syncing flathub");
        console.push_committed_tagged("flatpak-system", "Thunderbird ready");
        console.push_committed_tagged(
            "gnome-ext",
            "Updating Blur my Shell (blur-my-shell@aunetx)",
        );
        console.push_committed_tagged("brew", "==> Upgrading ripgrep 14.1.0 -> 14.1.1");
        console.push_committed_tagged("fwupd", "UEFI dbx 20241101");
        assert_eq!(LogSourceFilter::All.name(), "all");
        assert_eq!(
            LogSourceFilter::from_name(Some("flatpak")),
            LogSourceFilter::Flatpak
        );
        assert_eq!(
            LogSourceFilter::from_name(Some("gnome-ext")),
            LogSourceFilter::Extensions
        );
        assert_eq!(LogSourceFilter::from_name(Some("brew")), LogSourceFilter::Brew);

        let (all_text, all_m, all_t) = filter_console_lines(&console, LogSourceFilter::All, "");
        assert_eq!(all_m, 8);
        assert_eq!(all_t, 8);
        assert!(!all_text.contains("40%"));
        assert!(all_text.contains("[1/2] openssl 100%"));

        let (dnf_text, dnf_m, dnf_t) =
            filter_console_lines(&console, LogSourceFilter::Dnf, "openssl");
        assert_eq!((dnf_m, dnf_t), (1, 8));
        assert_eq!(dnf_text, "[dnf] [1/2] openssl 100%");

        let (fp_text, fp_m, fp_t) =
            filter_console_lines(&console, LogSourceFilter::Flatpak, "");
        assert_eq!((fp_m, fp_t), (2, 8));
        assert!(fp_text.contains("[flatpak-user]"));
        assert!(fp_text.contains("[flatpak-system]"));

        let (ext_text, ext_m, ext_t) =
            filter_console_lines(&console, LogSourceFilter::Extensions, "blur");
        assert_eq!((ext_m, ext_t), (1, 8));
        assert_eq!(
            ext_text,
            "[gnome-ext] Updating Blur my Shell (blur-my-shell@aunetx)"
        );

        let (brew_text, brew_m, brew_t) =
            filter_console_lines(&console, LogSourceFilter::Brew, "ripgrep");
        assert_eq!((brew_m, brew_t), (1, 8));
        assert_eq!(brew_text, "[brew] ==> Upgrading ripgrep 14.1.0 -> 14.1.1");

        let (fw_text, fw_m, fw_t) =
            filter_console_lines(&console, LogSourceFilter::Fwupd, "dbx");
        assert_eq!((fw_m, fw_t), (1, 8));
        assert_eq!(fw_text, "[fwupd] UEFI dbx 20241101");

        let (none_text, none_m, none_t) =
            filter_console_lines(&console, LogSourceFilter::Fwupd, "thunderbird");
        assert_eq!((none_m, none_t), (0, 8));
        assert_eq!(none_text, "");
    }
}
