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

        // Clearing resets live_tail too so the next commit appends cleanly.
        console.push_live_tagged("dnf", "50%");
        console.clear();
        assert!(console.is_empty());
        assert_eq!(console.len(), 0);
        assert_eq!(console.last_line(), "");
        assert_eq!(console.full_text(), "");
        assert_eq!(console.push_committed_tagged("dnf", "done"), ConsoleDelta::Appended);
        assert_eq!(console.last_line(), "[dnf] done");
    }

    #[test]
    fn cap_zero_and_one_handle_live_tail_and_eviction() {
        let mut zero = ConsoleBuffer::new(0);
        for delta in [
            zero.push("raw"),
            zero.push_live_tagged("dnf", "live"),
            zero.push_committed_tagged("dnf", "commit"),
        ] {
            assert_eq!(delta, ConsoleDelta::TrimmedAndAppended);
            assert_eq!(zero.len(), 0);
            assert!(zero.is_empty());
            assert_eq!(zero.last_line(), "");
            assert_eq!(zero.full_text(), "");
        }

        let mut one = ConsoleBuffer::new(1);
        assert_eq!(one.push_live_tagged("dnf", "10%"), ConsoleDelta::Appended);
        assert_eq!(one.push_live_tagged("dnf", "50%"), ConsoleDelta::ReplacedLast);
        assert_eq!(one.len(), 1);
        assert_eq!(one.full_text(), "[dnf] 50%");

        assert_eq!(
            one.push_committed_tagged("dnf", "100%"),
            ConsoleDelta::ReplacedLast
        );
        assert_eq!(one.len(), 1);
        assert_eq!(one.full_text(), "[dnf] 100%");

        assert_eq!(
            one.push_committed_tagged("dnf", "Complete!"),
            ConsoleDelta::TrimmedAndAppended
        );
        assert_eq!(one.len(), 1);
        assert_eq!(one.full_text(), "[dnf] Complete!");

        // Raw push() resets live_tail and evicts at cap=1.
        assert_eq!(
            one.push_live_tagged("dnf", "downloading"),
            ConsoleDelta::TrimmedAndAppended
        );
        assert_eq!(one.push("manual separator"), ConsoleDelta::TrimmedAndAppended);
        assert_eq!(one.len(), 1);
        assert_eq!(one.full_text(), "manual separator");
        assert_eq!(
            one.push_live_tagged("dnf", "after"),
            ConsoleDelta::TrimmedAndAppended
        );
        assert_eq!(one.full_text(), "[dnf] after");
    }

    #[test]
    fn deltas_stay_in_lockstep_with_reference_ring_model() {
        #[derive(Clone, Copy)]
        enum Op {
            Push(&'static str),
            Live(&'static str, &'static str),
            Commit(&'static str, &'static str),
            Clear,
        }

        let ops = [
            Op::Push("boot"),
            Op::Live("dnf", "10%"),
            Op::Live("dnf", "40%"),
            Op::Commit("dnf", "done-1"),
            Op::Live("dnf", "20%"),
            Op::Push("separator"),
            Op::Commit("fp", "done-2"),
            Op::Live("fw", "50%"),
            Op::Commit("fw", "done-3"),
            Op::Commit("fw", "done-4"),
            Op::Clear,
            Op::Live("dnf", "fresh-live"),
            Op::Commit("dnf", "fresh-commit"),
        ];

        let max_lines = 4;
        let mut console = ConsoleBuffer::new(max_lines);
        let mut model: VecDeque<String> = VecDeque::new();

        for (step, op) in ops.into_iter().enumerate() {
            match op {
                Op::Push(s) => {
                    let delta = console.push(s);
                    model.push_back(s.to_string());
                    let trimmed = if model.len() > max_lines {
                        model.pop_front();
                        true
                    } else {
                        false
                    };
                    assert_eq!(
                        delta,
                        if trimmed {
                            ConsoleDelta::TrimmedAndAppended
                        } else {
                            ConsoleDelta::Appended
                        },
                        "step {step}"
                    );
                }
                Op::Live(tag, line) | Op::Commit(tag, line) => {
                    let delta = match op {
                        Op::Live(_, _) => console.push_live_tagged(tag, line),
                        Op::Commit(_, _) => console.push_committed_tagged(tag, line),
                        _ => unreachable!(),
                    };
                    let formatted = format!("[{tag}] {line}");
                    match delta {
                        ConsoleDelta::ReplacedLast => {
                            *model.back_mut().expect("last exists") = formatted;
                        }
                        ConsoleDelta::Appended => {
                            model.push_back(formatted);
                        }
                        ConsoleDelta::TrimmedAndAppended => {
                            model.push_back(formatted);
                            model.pop_front();
                        }
                    }
                }
                Op::Clear => {
                    console.clear();
                    model.clear();
                }
            }

            let expected_full = model.iter().cloned().collect::<Vec<_>>().join("\n");
            let expected_last = model.back().map(String::as_str).unwrap_or("");
            assert_eq!(console.len(), model.len(), "len at step {step}");
            assert_eq!(console.is_empty(), model.is_empty(), "empty at step {step}");
            assert_eq!(console.full_text(), expected_full, "full at step {step}");
            assert_eq!(console.last_line(), expected_last, "last at step {step}");
        }
    }
}
