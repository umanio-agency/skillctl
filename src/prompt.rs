//! Custom multi-select prompt with a live filter, rendered in cliclack's
//! tree-style framing. Drop-in replacement for `cliclack::multiselect` for
//! cases where the candidate list is large enough that scrolling/toggling
//! becomes friction.
//!
//! Key bindings:
//! - Type any char  — append to filter (matches label, case-insensitive substring)
//! - Backspace      — edit filter
//! - ↑ / ↓          — navigate filtered list
//! - Space / Tab    — toggle the focused item
//! - Enter          — confirm
//! - Esc / Ctrl+C   — cancel
//!
//! Filter applies to the label only (skill names) so Space stays free as a
//! toggle. Hint text is shown but not searched.

use std::collections::HashSet;
use std::io::{Stdout, Write, stdout};

use anyhow::{Context as _, Result, anyhow};
use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{
    Clear, ClearType, DisableLineWrap, EnableLineWrap, disable_raw_mode, enable_raw_mode,
    size as terminal_size,
};
use crossterm::{execute, queue};

const WINDOW_SIZE: usize = 12;
const MARK_ACTIVE: &str = "◆";
const MARK_BAR: &str = "│";
const MARK_END: &str = "└";
const MARK_SELECTED: &str = "◼";
const MARK_UNSELECTED: &str = "◻";
const MARK_FOCUS: &str = "❯";
const COLOR_DIM: Color = Color::Grey;
const COLOR_ACCENT: Color = Color::Cyan;
const COLOR_SELECTED: Color = Color::Green;

pub struct FilterMultiSelect<T: Clone> {
    title: String,
    items: Vec<PromptItem<T>>,
    required: bool,
}

struct PromptItem<T> {
    value: T,
    label: String,
    hint: String,
}

pub fn multiselect<T: Clone>(title: impl Into<String>) -> FilterMultiSelect<T> {
    FilterMultiSelect {
        title: title.into(),
        items: Vec::new(),
        required: false,
    }
}

impl<T: Clone> FilterMultiSelect<T> {
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    pub fn item(
        mut self,
        value: T,
        label: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        self.items.push(PromptItem {
            value,
            label: label.into(),
            hint: hint.into(),
        });
        self
    }

    pub fn interact(self) -> Result<Vec<T>> {
        if self.items.is_empty() {
            return Ok(Vec::new());
        }

        let mut state = State {
            title: self.title,
            items: self.items,
            required: self.required,
            query: String::new(),
            focus: 0,
            selected: HashSet::new(),
            visible: Vec::new(),
            last_lines: 0,
        };
        state.refilter();

        let mut out = stdout();
        enable_raw_mode().context("enabling terminal raw mode")?;
        execute!(out, Hide, DisableLineWrap).context("preparing terminal")?;

        let outcome = run_event_loop(&mut state, &mut out);

        let _ = execute!(out, Show, EnableLineWrap, ResetColor);
        let _ = disable_raw_mode();

        match outcome {
            Ok(()) => {
                clear_render(&mut out, state.last_lines)?;
                render_final(&mut out, &state)?;
                Ok(state
                    .selected
                    .iter()
                    .map(|i| state.items[*i].value.clone())
                    .collect())
            }
            Err(LoopError::Cancelled) => {
                clear_render(&mut out, state.last_lines)?;
                render_cancel(&mut out, &state.title)?;
                Err(anyhow!("cancelled"))
            }
            Err(LoopError::Other(e)) => Err(e),
        }
    }
}

struct State<T> {
    title: String,
    items: Vec<PromptItem<T>>,
    required: bool,
    query: String,
    focus: usize,
    selected: HashSet<usize>,
    visible: Vec<usize>,
    last_lines: u16,
}

impl<T> State<T> {
    fn refilter(&mut self) {
        self.visible = filter_indices(&self.query, &self.items);
        if self.focus >= self.visible.len() {
            self.focus = self.visible.len().saturating_sub(1);
        }
    }
}

impl<T> Default for State<T> {
    fn default() -> Self {
        Self {
            title: String::new(),
            items: Vec::new(),
            required: false,
            query: String::new(),
            focus: 0,
            selected: HashSet::new(),
            visible: Vec::new(),
            last_lines: 0,
        }
    }
}

enum LoopError {
    Cancelled,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for LoopError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

fn run_event_loop<T: Clone>(state: &mut State<T>, out: &mut Stdout) -> std::result::Result<(), LoopError> {
    loop {
        clear_render(out, state.last_lines).map_err(LoopError::from)?;
        let lines = render(out, state).map_err(LoopError::from)?;
        state.last_lines = lines;
        out.flush().context("flushing terminal").map_err(LoopError::from)?;

        match event::read().context("reading terminal event").map_err(LoopError::from)? {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
                ..
            }) => {
                if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
                    return Err(LoopError::Cancelled);
                }
                match code {
                    KeyCode::Esc => return Err(LoopError::Cancelled),
                    KeyCode::Enter => {
                        if state.required && state.selected.is_empty() {
                            // Don't confirm with empty selection in required mode; ignore.
                            continue;
                        }
                        return Ok(());
                    }
                    KeyCode::Up => {
                        if state.focus > 0 {
                            state.focus -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if state.focus + 1 < state.visible.len() {
                            state.focus += 1;
                        }
                    }
                    KeyCode::Tab | KeyCode::Char(' ') => {
                        if let Some(&idx) = state.visible.get(state.focus) {
                            if state.selected.contains(&idx) {
                                state.selected.remove(&idx);
                            } else {
                                state.selected.insert(idx);
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        if !state.query.is_empty() {
                            state.query.pop();
                            state.refilter();
                        }
                    }
                    KeyCode::Char(c) if !c.is_control() => {
                        state.query.push(c);
                        state.refilter();
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {
                // Re-render on next loop iteration with potentially new dimensions.
            }
            _ => {}
        }
    }
}

fn render<T>(out: &mut Stdout, state: &State<T>) -> Result<u16> {
    let cols = terminal_size().map(|(c, _)| c as usize).unwrap_or(80);
    let mut lines: u16 = 0;

    // Header
    queue!(
        out,
        SetForegroundColor(COLOR_ACCENT),
        Print(MARK_ACTIVE),
        ResetColor,
        Print("  "),
        Print(&state.title),
        Print("\r\n")
    )?;
    lines += 1;

    // Search bar
    queue!(
        out,
        SetForegroundColor(COLOR_DIM),
        Print(MARK_BAR),
        ResetColor,
        Print("  "),
    )?;
    if state.query.is_empty() {
        queue!(
            out,
            SetForegroundColor(COLOR_DIM),
            Print("type to filter…"),
            ResetColor
        )?;
    } else {
        queue!(out, Print(&state.query))?;
    }
    queue!(out, Print("\r\n"))?;
    lines += 1;

    // Items (windowed around focus)
    if state.visible.is_empty() {
        queue!(
            out,
            SetForegroundColor(COLOR_DIM),
            Print(MARK_BAR),
            Print("  no matches"),
            ResetColor,
            Print("\r\n")
        )?;
        lines += 1;
    } else {
        let (start, end) = window_bounds(state.visible.len(), state.focus, WINDOW_SIZE);
        if start > 0 {
            queue!(
                out,
                SetForegroundColor(COLOR_DIM),
                Print(MARK_BAR),
                Print(format!("  ↑ {} more above", start)),
                ResetColor,
                Print("\r\n")
            )?;
            lines += 1;
        }
        for (offset, idx_in_visible) in (start..end).enumerate() {
            let item_idx = state.visible[idx_in_visible];
            let is_focused = idx_in_visible == state.focus;
            let is_selected = state.selected.contains(&item_idx);
            let item = &state.items[item_idx];

            queue!(out, SetForegroundColor(COLOR_DIM), Print(MARK_BAR), ResetColor, Print("  "))?;

            if is_focused {
                queue!(out, SetForegroundColor(COLOR_ACCENT), Print(MARK_FOCUS), ResetColor, Print(" "))?;
            } else {
                queue!(out, Print("  "))?;
            }

            if is_selected {
                queue!(out, SetForegroundColor(COLOR_SELECTED), Print(MARK_SELECTED), ResetColor)?;
            } else {
                queue!(out, SetForegroundColor(COLOR_DIM), Print(MARK_UNSELECTED), ResetColor)?;
            }
            queue!(out, Print(" "))?;

            // Label (truncated to fit)
            let prefix_width = 6; // bar + spaces + focus + select markers
            let max_label_and_hint = cols.saturating_sub(prefix_width).max(20);
            let label_text = if is_focused {
                format!("\x1b[1m{}\x1b[0m", item.label)
            } else {
                item.label.clone()
            };

            let hint_part = if !item.hint.is_empty() {
                format!("  {}", item.hint)
            } else {
                String::new()
            };

            let combined = format!("{}{}", item.label, hint_part);
            let truncated = truncate_to(&combined, max_label_and_hint);

            // Re-print with hint dimmed if present and not truncated past it
            if !item.hint.is_empty() && truncated.len() == combined.len() {
                queue!(out, Print(&item.label))?;
                queue!(
                    out,
                    SetForegroundColor(COLOR_DIM),
                    Print("  "),
                    Print(&item.hint),
                    ResetColor
                )?;
            } else {
                queue!(out, Print(truncated))?;
            }

            queue!(out, Print("\r\n"))?;
            let _ = label_text; // keep unused-warning quiet for now
            let _ = offset;
            lines += 1;
        }
        if end < state.visible.len() {
            queue!(
                out,
                SetForegroundColor(COLOR_DIM),
                Print(MARK_BAR),
                Print(format!("  ↓ {} more below", state.visible.len() - end)),
                ResetColor,
                Print("\r\n")
            )?;
            lines += 1;
        }
    }

    // Footer (truncate to terminal width to avoid wrapping that would break
    // the line count we use to clear on the next redraw)
    let footer = footer_text(state);
    let footer_max = cols.saturating_sub(3).max(10);
    let footer = truncate_to(&footer, footer_max);
    queue!(
        out,
        SetForegroundColor(COLOR_DIM),
        Print(MARK_END),
        Print("  "),
        Print(footer),
        ResetColor,
        Print("\r\n")
    )?;
    lines += 1;

    Ok(lines)
}

fn footer_text<T>(state: &State<T>) -> String {
    let count = state.selected.len();
    format!("{count} selected • space toggle • enter confirm • esc cancel")
}

fn render_final<T>(out: &mut Stdout, state: &State<T>) -> Result<()> {
    // Compact summary line, cliclack style.
    let names: Vec<String> = state
        .selected
        .iter()
        .map(|i| state.items[*i].label.clone())
        .collect();
    let body = if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    };
    queue!(
        out,
        SetForegroundColor(COLOR_ACCENT),
        Print(MARK_ACTIVE),
        ResetColor,
        Print("  "),
        Print(&state.title),
        Print("\r\n"),
        SetForegroundColor(COLOR_DIM),
        Print(MARK_BAR),
        ResetColor,
        Print("  "),
        Print(body),
        Print("\r\n"),
        SetForegroundColor(COLOR_DIM),
        Print(MARK_BAR),
        ResetColor,
        Print("\r\n")
    )?;
    out.flush()?;
    Ok(())
}

fn render_cancel(out: &mut Stdout, title: &str) -> Result<()> {
    queue!(
        out,
        SetForegroundColor(COLOR_DIM),
        Print(MARK_ACTIVE),
        Print("  "),
        Print(title),
        Print(" — cancelled\r\n"),
        ResetColor
    )?;
    out.flush()?;
    Ok(())
}

fn clear_render(out: &mut Stdout, lines: u16) -> Result<()> {
    if lines == 0 {
        return Ok(());
    }
    queue!(out, MoveToColumn(0))?;
    for _ in 0..lines {
        queue!(out, MoveUp(1), Clear(ClearType::CurrentLine))?;
    }
    Ok(())
}

fn filter_indices<T>(query: &str, items: &[PromptItem<T>]) -> Vec<usize> {
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    let q = query.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.label.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

fn window_bounds(total: usize, focus: usize, window: usize) -> (usize, usize) {
    if total <= window {
        return (0, total);
    }
    let half = window / 2;
    let start = focus.saturating_sub(half);
    let end = (start + window).min(total);
    let start = end.saturating_sub(window);
    (start, end)
}

fn truncate_to(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> PromptItem<()> {
        PromptItem {
            value: (),
            label: label.to_string(),
            hint: String::new(),
        }
    }

    #[test]
    fn empty_query_returns_all() {
        let items = vec![item("foo"), item("bar"), item("baz")];
        assert_eq!(filter_indices("", &items), vec![0, 1, 2]);
    }

    #[test]
    fn case_insensitive_substring() {
        let items = vec![item("Foo"), item("bAr"), item("FooBar")];
        assert_eq!(filter_indices("foo", &items), vec![0, 2]);
    }

    #[test]
    fn no_match_returns_empty() {
        let items = vec![item("foo"), item("bar")];
        assert!(filter_indices("xyz", &items).is_empty());
    }

    #[test]
    fn window_smaller_than_total() {
        assert_eq!(window_bounds(10, 0, 5), (0, 5));
        assert_eq!(window_bounds(10, 9, 5), (5, 10));
        assert_eq!(window_bounds(10, 4, 5), (2, 7));
    }

    #[test]
    fn window_when_total_fits() {
        assert_eq!(window_bounds(3, 0, 5), (0, 3));
        assert_eq!(window_bounds(3, 2, 5), (0, 3));
    }

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate_to("abc", 10), "abc");
    }

    #[test]
    fn truncate_long_with_ellipsis() {
        assert_eq!(truncate_to("abcdefghij", 5), "abcd…");
    }
}
