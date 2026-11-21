//! Bounded scrollback buffer and incremental update deltas for the live console.

use std::collections::VecDeque;

/// Structural change produced by appending or updating a line in [`ConsoleBuffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleDelta {
    Appended,
    ReplacedLast,
    TrimmedAndAppended,
}

/// Fixed-capacity line buffer that tracks an open `\r` progress line.
#[derive(Debug, Default, Clone)]
pub struct ConsoleBuffer {
    lines: VecDeque<String>,
    max_lines: usize,
    /// Last entry is an in-progress `\r` update and the next one replaces it.
    live_tail: bool,
}

impl ConsoleBuffer {
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines,
            live_tail: false,
        }
    }

    pub fn push(&mut self, line: impl Into<String>) -> ConsoleDelta {
        self.push_entry(line.into(), false)
    }

    /// Carriage-return progress. Replaces the open line instead of appending.
    pub fn push_live_tagged(&mut self, tag: &str, line: &str) -> ConsoleDelta {
        self.push_tagged_impl(tag, line, true)
    }

    /// Newline-terminated output. A following commit replaces the open `\r` line.
    pub fn push_committed_tagged(&mut self, tag: &str, line: &str) -> ConsoleDelta {
        self.push_tagged_impl(tag, line, false)
    }

    fn push_tagged_impl(&mut self, tag: &str, line: &str, live: bool) -> ConsoleDelta {
        let formatted = format!("[{tag}] {line}");
        if self.live_tail {
            if let Some(last) = self.lines.back_mut() {
                *last = formatted;
                self.live_tail = live;
                return ConsoleDelta::ReplacedLast;
            }
        }
        self.push_entry(formatted, live)
    }

    fn push_entry(&mut self, entry: String, live: bool) -> ConsoleDelta {
        self.lines.push_back(entry);
        self.live_tail = live;
        if self.trim() {
            ConsoleDelta::TrimmedAndAppended
        } else {
            ConsoleDelta::Appended
        }
    }

    fn trim(&mut self) -> bool {
        let mut trimmed = false;
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
            trimmed = true;
        }
        trimmed
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.live_tail = false;
    }

    pub fn last_line(&self) -> &str {
        self.lines.back().map(String::as_str).unwrap_or("")
    }

    pub fn full_text(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let capacity = self.lines.iter().map(String::len).sum::<usize>() + self.lines.len() - 1;
        let mut out = String::with_capacity(capacity);
        for (idx, line) in self.lines.iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(line);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carriage_return_replaces_the_open_console_line() {
        let mut console = ConsoleBuffer::new(20);
        assert_eq!(
            console.push_live_tagged("dnf", "[26/26] grok-bot  10%"),
            ConsoleDelta::Appended
        );
        assert_eq!(
            console.push_live_tagged("dnf", "[26/26] grok-bot  40%"),
            ConsoleDelta::ReplacedLast
        );
        assert_eq!(console.len(), 1);
        assert!(console.last_line().contains("40%"));
        assert_eq!(
            console.push_committed_tagged("dnf", "[26/26] grok-bot 100%"),
            ConsoleDelta::ReplacedLast
        );
        assert_eq!(console.len(), 1);
        assert!(console.last_line().contains("100%"));
        assert_eq!(
            console.push_committed_tagged("dnf", "Complete!"),
            ConsoleDelta::Appended
        );
        assert_eq!(console.len(), 2);
        assert!(console.last_line().contains("Complete!"));
    }

    #[test]
    fn console_buffer_caps() {
        let mut console = ConsoleBuffer::new(10);
        for i in 0..50 {
            let delta = console.push_committed_tagged("t", &format!("{i}"));
            let expected = if i < 10 {
                ConsoleDelta::Appended
            } else {
                ConsoleDelta::TrimmedAndAppended
            };
            assert_eq!(delta, expected);
        }
        assert_eq!(console.len(), 10);
        let expected_full: String = (40..50)
            .map(|i| format!("[t] {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(console.full_text(), expected_full);
    }

    #[test]
    fn push_and_clear_track_deltas_and_emptiness() {
        let mut console = ConsoleBuffer::new(2);
        assert!(console.is_empty());
        assert_eq!(console.full_text(), "");
        assert_eq!(console.last_line(), "");

        assert_eq!(console.push("one"), ConsoleDelta::Appended);
        assert_eq!(console.push("two"), ConsoleDelta::Appended);
        assert_eq!(console.push("three"), ConsoleDelta::TrimmedAndAppended);
        assert_eq!(console.len(), 2);
        assert!(!console.is_empty());
        assert_eq!(console.full_text(), "two\nthree");
        assert_eq!(console.last_line(), "three");

        console.clear();
        assert!(console.is_empty());
        assert_eq!(console.len(), 0);
        assert_eq!(console.last_line(), "");
        assert_eq!(console.full_text(), "");
    }
}
