//! Custom multi-select prompt with a live filter, rendered in cliclack's
//! tree-style framing. Drop-in replacement for `cliclack::multiselect` for
//! cases where the candidate list is large enough that scrolling/toggling
//! becomes friction.
//!
//! Key bindings:
//! - Type any char  — append to filter (matches label, case-insensitive substring)
//! - Backspace      — edit filter (or clear an active tag filter when empty)
//! - ↑ / ↓          — navigate filtered list
//! - Space / Tab    — act on the focused row: toggle a skill, or run a meta-action
//! - Enter          — confirm
//! - Esc / Ctrl+C   — cancel (Esc first clears an active tag filter, if any)
//!
//! Filter applies to the label only (skill names) so Space stays free as a
//! toggle. Hint text is shown but not searched. When the query matches a
//! **tag** carried by the items, actionable meta-rows appear above the skill
//! matches (`tag:<name> — filter` narrows to that tag; `tag:<name> — select
//! all` picks every skill carrying it), turning tags into first-class
//! navigation instead of a name-only substring match.

use std::collections::{BTreeMap, HashSet};
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
const COLOR_META: Color = Color::Magenta;
const MARK_META: &str = "▸";
/// Cap on how many distinct matching tags get meta-rows, so a broad query
/// can't bury the skill matches under a wall of tag actions.
const MAX_META_TAGS: usize = 3;

pub struct FilterMultiSelect<T: Clone> {
    title: String,
    items: Vec<PromptItem<T>>,
    required: bool,
}

struct PromptItem<T> {
    value: T,
    label: String,
    hint: String,
    tags: Vec<String>,
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

    /// Append a candidate. `tags` powers the tag meta-rows; pass an empty vec
    /// for a plain, tag-less picker.
    pub fn item(
        mut self,
        value: T,
        label: impl Into<String>,
        hint: impl Into<String>,
        tags: Vec<String>,
    ) -> Self {
        self.items.push(PromptItem {
            value,
            label: label.into(),
            hint: hint.into(),
            tags,
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
            active_tag: None,
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

/// Multi-select with a row of switchable **library tabs**, layered on the same
/// live filter as [`FilterMultiSelect`]. Each tab carries its own item list;
/// selection accumulates per tab so one run yields picks across several
/// libraries. Items for every tab are supplied up front (eager discovery
/// happens before the prompt opens — no git work runs inside raw mode).
///
/// Extra key bindings over the single-list prompt:
/// - ← / →  — switch the active library tab (filter + focus reset for the tab)
pub struct TabbedMultiSelect<T: Clone> {
    title: String,
    tabs: Vec<TabData<T>>,
    required: bool,
}

struct TabData<T> {
    label: String,
    items: Vec<PromptItem<T>>,
}

pub fn tabbed<T: Clone>(title: impl Into<String>) -> TabbedMultiSelect<T> {
    TabbedMultiSelect {
        title: title.into(),
        tabs: Vec::new(),
        required: false,
    }
}

impl<T: Clone> TabbedMultiSelect<T> {
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Append a tab and its items as `(value, label, hint)` triples.
    pub fn tab(mut self, label: impl Into<String>, items: Vec<(T, String, String)>) -> Self {
        self.tabs.push(TabData {
            label: label.into(),
            items: items
                .into_iter()
                .map(|(value, label, hint)| PromptItem {
                    value,
                    label,
                    hint,
                    tags: Vec::new(),
                })
                .collect(),
        });
        self
    }

    pub fn interact(self) -> Result<Vec<T>> {
        if self.tabs.iter().all(|t| t.items.is_empty()) {
            return Ok(Vec::new());
        }

        let mut state = TabState {
            title: self.title,
            tabs: self.tabs,
            required: self.required,
            active: 0,
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

        let outcome = run_tab_event_loop(&mut state, &mut out);

        let _ = execute!(out, Show, EnableLineWrap, ResetColor);
        let _ = disable_raw_mode();

        match outcome {
            Ok(()) => {
                clear_render(&mut out, state.last_lines)?;
                render_tab_final(&mut out, &state)?;
                let mut picks = Vec::new();
                for (t, tab) in state.tabs.iter().enumerate() {
                    for (i, item) in tab.items.iter().enumerate() {
                        if state.selected.contains(&(t, i)) {
                            picks.push(item.value.clone());
                        }
                    }
                }
                Ok(picks)
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

struct TabState<T> {
    title: String,
    tabs: Vec<TabData<T>>,
    required: bool,
    active: usize,
    query: String,
    focus: usize,
    selected: HashSet<(usize, usize)>,
    visible: Vec<usize>,
    last_lines: u16,
}

impl<T> TabState<T> {
    fn refilter(&mut self) {
        self.visible = filter_indices(&self.query, &self.tabs[self.active].items);
        if self.focus >= self.visible.len() {
            self.focus = self.visible.len().saturating_sub(1);
        }
    }

    fn switch_to(&mut self, tab: usize) {
        if tab != self.active && tab < self.tabs.len() {
            self.active = tab;
            self.focus = 0;
            self.refilter();
        }
    }

    fn selected_in(&self, tab: usize) -> usize {
        self.selected.iter().filter(|(t, _)| *t == tab).count()
    }
}

/// A rendered/navigable line in the single-list picker: either a real skill or
/// one of the tag meta-actions surfaced above the matches.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// Narrow the list to skills carrying `tag`.
    MetaFilter { tag: String, count: usize },
    /// Select every skill carrying `tag` (and narrow to show them).
    MetaSelectAll { tag: String, count: usize },
    /// A real candidate at `items[item_idx]`.
    Skill { item_idx: usize },
}

struct State<T> {
    title: String,
    items: Vec<PromptItem<T>>,
    required: bool,
    query: String,
    /// When set, the list is narrowed to skills carrying this tag (entered from
    /// a meta-row); the text `query` still refines by label on top of it.
    active_tag: Option<String>,
    focus: usize,
    selected: HashSet<usize>,
    visible: Vec<Row>,
    last_lines: u16,
}

impl<T> State<T> {
    fn refilter(&mut self) {
        let mut rows: Vec<Row> = Vec::new();
        // Meta-rows only in plain mode (no active tag) with a non-empty query.
        if self.active_tag.is_none() && !self.query.is_empty() {
            for (tag, count) in matching_tags(&self.query, &self.items) {
                rows.push(Row::MetaFilter {
                    tag: tag.clone(),
                    count,
                });
                rows.push(Row::MetaSelectAll { tag, count });
            }
        }
        // Skill rows: label substring, intersected with the active tag (if any).
        for idx in filter_indices(&self.query, &self.items) {
            if let Some(tag) = &self.active_tag {
                if !self.items[idx]
                    .tags
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(tag))
                {
                    continue;
                }
            }
            rows.push(Row::Skill { item_idx: idx });
        }
        self.visible = rows;
        if self.focus >= self.visible.len() {
            self.focus = self.visible.len().saturating_sub(1);
        }
    }

    /// Enter tag-filter mode: narrow to `tag`, clear the text query, reset focus.
    fn enter_tag_filter(&mut self, tag: String) {
        self.active_tag = Some(tag);
        self.query.clear();
        self.focus = 0;
        self.refilter();
    }

    /// Leave tag-filter mode (Esc / Backspace-on-empty).
    fn clear_tag_filter(&mut self) {
        self.active_tag = None;
        self.query.clear();
        self.focus = 0;
        self.refilter();
    }

    /// Select every skill carrying `tag`, then narrow the view to show them.
    fn select_all_tagged(&mut self, tag: &str) {
        for (i, it) in self.items.iter().enumerate() {
            if it.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                self.selected.insert(i);
            }
        }
        self.enter_tag_filter(tag.to_string());
    }

    /// Space/Tab on the focused row: toggle a skill, or run its meta-action.
    fn activate_focused(&mut self) {
        match self.visible.get(self.focus).cloned() {
            Some(Row::Skill { item_idx }) => {
                if !self.selected.remove(&item_idx) {
                    self.selected.insert(item_idx);
                }
            }
            Some(Row::MetaFilter { tag, .. }) => self.enter_tag_filter(tag),
            Some(Row::MetaSelectAll { tag, .. }) => self.select_all_tagged(&tag),
            None => {}
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
            active_tag: None,
            focus: 0,
            selected: HashSet::new(),
            visible: Vec::new(),
            last_lines: 0,
        }
    }
}

/// Distinct tags across `items` whose name matches `query` (case-insensitive
/// substring), with the count of items carrying each, ranked exact → prefix →
/// substring, then by descending count, then alphabetically. Capped at
/// [`MAX_META_TAGS`].
fn matching_tags<T>(query: &str, items: &[PromptItem<T>]) -> Vec<(String, usize)> {
    let q = query.to_lowercase();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for it in items {
        let mut seen: HashSet<String> = HashSet::new();
        for t in &it.tags {
            let key = t.to_lowercase();
            if seen.insert(key) {
                *counts.entry(t.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut matched: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(t, _)| t.to_lowercase().contains(&q))
        .collect();
    matched.sort_by(|a, b| {
        tag_rank(&a.0, &q)
            .cmp(&tag_rank(&b.0, &q))
            .then(b.1.cmp(&a.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    matched.truncate(MAX_META_TAGS);
    matched
}

/// 0 = exact match, 1 = prefix, 2 = other substring. `q` is already lowercase.
fn tag_rank(tag: &str, q: &str) -> u8 {
    let t = tag.to_lowercase();
    if t == q {
        0
    } else if t.starts_with(q) {
        1
    } else {
        2
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

fn run_event_loop<T: Clone>(
    state: &mut State<T>,
    out: &mut Stdout,
) -> std::result::Result<(), LoopError> {
    loop {
        clear_render(out, state.last_lines).map_err(LoopError::from)?;
        let lines = render(out, state).map_err(LoopError::from)?;
        state.last_lines = lines;
        out.flush()
            .context("flushing terminal")
            .map_err(LoopError::from)?;

        match event::read()
            .context("reading terminal event")
            .map_err(LoopError::from)?
        {
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
                    KeyCode::Esc => {
                        if state.active_tag.is_some() {
                            state.clear_tag_filter();
                        } else {
                            return Err(LoopError::Cancelled);
                        }
                    }
                    KeyCode::Enter => {
                        if state.required && state.selected.is_empty() {
                            // Don't confirm with empty selection in required mode; ignore.
                            continue;
                        }
                        return Ok(());
                    }
                    KeyCode::Up if state.focus > 0 => {
                        state.focus -= 1;
                    }
                    KeyCode::Down if state.focus + 1 < state.visible.len() => {
                        state.focus += 1;
                    }
                    KeyCode::Tab | KeyCode::Char(' ') => state.activate_focused(),
                    KeyCode::Backspace => {
                        if !state.query.is_empty() {
                            state.query.pop();
                            state.refilter();
                        } else if state.active_tag.is_some() {
                            state.clear_tag_filter();
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

fn run_tab_event_loop<T: Clone>(
    state: &mut TabState<T>,
    out: &mut Stdout,
) -> std::result::Result<(), LoopError> {
    loop {
        clear_render(out, state.last_lines).map_err(LoopError::from)?;
        let lines = render_tabbed(out, state).map_err(LoopError::from)?;
        state.last_lines = lines;
        out.flush()
            .context("flushing terminal")
            .map_err(LoopError::from)?;

        match event::read()
            .context("reading terminal event")
            .map_err(LoopError::from)?
        {
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
                            continue;
                        }
                        return Ok(());
                    }
                    KeyCode::Left if state.active > 0 => state.switch_to(state.active - 1),
                    KeyCode::Right if state.active + 1 < state.tabs.len() => {
                        state.switch_to(state.active + 1)
                    }
                    KeyCode::Up if state.focus > 0 => state.focus -= 1,
                    KeyCode::Down if state.focus + 1 < state.visible.len() => state.focus += 1,
                    KeyCode::Tab | KeyCode::Char(' ') => {
                        if let Some(&idx) = state.visible.get(state.focus) {
                            let key = (state.active, idx);
                            if state.selected.contains(&key) {
                                state.selected.remove(&key);
                            } else {
                                state.selected.insert(key);
                            }
                        }
                    }
                    KeyCode::Backspace if !state.query.is_empty() => {
                        state.query.pop();
                        state.refilter();
                    }
                    KeyCode::Char(c) if !c.is_control() => {
                        state.query.push(c);
                        state.refilter();
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn queue_tab_row<T>(out: &mut Stdout, state: &TabState<T>, max: usize) -> Result<()> {
    let mut width = 0usize;
    for (i, tab) in state.tabs.iter().enumerate() {
        let count = state.selected_in(i);
        let base = if count > 0 {
            format!("{}({count})", tab.label)
        } else {
            tab.label.clone()
        };
        let shown = if i == state.active {
            format!("[{base}]")
        } else {
            base
        };
        let sep = if i > 0 { 2 } else { 0 };
        let w = sep + shown.chars().count();
        if width + w > max {
            queue!(out, SetForegroundColor(COLOR_DIM), Print(" …"), ResetColor)?;
            break;
        }
        if i > 0 {
            queue!(out, Print("  "))?;
        }
        let color = if i == state.active {
            COLOR_ACCENT
        } else {
            COLOR_DIM
        };
        queue!(out, SetForegroundColor(color), Print(&shown), ResetColor)?;
        width += w;
    }
    Ok(())
}

fn render_tabbed<T>(out: &mut Stdout, state: &TabState<T>) -> Result<u16> {
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

    // Tab row
    queue!(
        out,
        SetForegroundColor(COLOR_DIM),
        Print(MARK_BAR),
        ResetColor,
        Print("  ")
    )?;
    queue_tab_row(out, state, cols.saturating_sub(3).max(10))?;
    queue!(out, Print("\r\n"))?;
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

    let items = &state.tabs[state.active].items;
    if state.visible.is_empty() {
        let msg = if items.is_empty() {
            "  (no skills in this library)"
        } else {
            "  no matches"
        };
        queue!(
            out,
            SetForegroundColor(COLOR_DIM),
            Print(MARK_BAR),
            Print(msg),
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
                Print(format!("  ↑ {start} more above")),
                ResetColor,
                Print("\r\n")
            )?;
            lines += 1;
        }
        for idx_in_visible in start..end {
            let item_idx = state.visible[idx_in_visible];
            let is_focused = idx_in_visible == state.focus;
            let is_selected = state.selected.contains(&(state.active, item_idx));
            let item = &items[item_idx];

            queue!(
                out,
                SetForegroundColor(COLOR_DIM),
                Print(MARK_BAR),
                ResetColor,
                Print("  ")
            )?;
            if is_focused {
                queue!(
                    out,
                    SetForegroundColor(COLOR_ACCENT),
                    Print(MARK_FOCUS),
                    ResetColor,
                    Print(" ")
                )?;
            } else {
                queue!(out, Print("  "))?;
            }
            if is_selected {
                queue!(
                    out,
                    SetForegroundColor(COLOR_SELECTED),
                    Print(MARK_SELECTED),
                    ResetColor
                )?;
            } else {
                queue!(
                    out,
                    SetForegroundColor(COLOR_DIM),
                    Print(MARK_UNSELECTED),
                    ResetColor
                )?;
            }
            queue!(out, Print(" "))?;

            let prefix_width = 6;
            let max_label_and_hint = cols.saturating_sub(prefix_width).max(20);
            let hint_part = if !item.hint.is_empty() {
                format!("  {}", item.hint)
            } else {
                String::new()
            };
            let combined = format!("{}{}", item.label, hint_part);
            let truncated = truncate_to(&combined, max_label_and_hint);
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

    let footer = format!(
        "{} selected • ← → library • space toggle • enter confirm • esc cancel",
        state.selected.len()
    );
    let footer = truncate_to(&footer, cols.saturating_sub(3).max(10));
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

fn render_tab_final<T>(out: &mut Stdout, state: &TabState<T>) -> Result<()> {
    let mut groups: Vec<String> = Vec::new();
    for (t, tab) in state.tabs.iter().enumerate() {
        let names: Vec<&str> = tab
            .items
            .iter()
            .enumerate()
            .filter(|(i, _)| state.selected.contains(&(t, *i)))
            .map(|(_, item)| item.label.as_str())
            .collect();
        if !names.is_empty() {
            groups.push(format!("{}: {}", tab.label, names.join(", ")));
        }
    }
    let body = if groups.is_empty() {
        "(none)".to_string()
    } else {
        groups.join("  •  ")
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

    // Search bar (shows the active tag filter, if any)
    queue!(
        out,
        SetForegroundColor(COLOR_DIM),
        Print(MARK_BAR),
        ResetColor,
        Print("  "),
    )?;
    if let Some(tag) = &state.active_tag {
        queue!(
            out,
            SetForegroundColor(COLOR_META),
            Print(format!("{MARK_META} tag:{tag}")),
            ResetColor
        )?;
        if state.query.is_empty() {
            queue!(
                out,
                SetForegroundColor(COLOR_DIM),
                Print("  (esc clears)"),
                ResetColor
            )?;
        } else {
            queue!(out, Print(format!("  {}", state.query)))?;
        }
    } else if state.query.is_empty() {
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
        let prefix_width = 6; // bar + spaces + focus + marker
        let max_body = cols.saturating_sub(prefix_width).max(20);
        for idx_in_visible in start..end {
            let is_focused = idx_in_visible == state.focus;

            queue!(
                out,
                SetForegroundColor(COLOR_DIM),
                Print(MARK_BAR),
                ResetColor,
                Print("  ")
            )?;
            if is_focused {
                queue!(
                    out,
                    SetForegroundColor(COLOR_ACCENT),
                    Print(MARK_FOCUS),
                    ResetColor,
                    Print(" ")
                )?;
            } else {
                queue!(out, Print("  "))?;
            }

            match &state.visible[idx_in_visible] {
                Row::Skill { item_idx } => {
                    let is_selected = state.selected.contains(item_idx);
                    let item = &state.items[*item_idx];
                    if is_selected {
                        queue!(
                            out,
                            SetForegroundColor(COLOR_SELECTED),
                            Print(MARK_SELECTED),
                            ResetColor
                        )?;
                    } else {
                        queue!(
                            out,
                            SetForegroundColor(COLOR_DIM),
                            Print(MARK_UNSELECTED),
                            ResetColor
                        )?;
                    }
                    queue!(out, Print(" "))?;

                    let hint_part = if !item.hint.is_empty() {
                        format!("  {}", item.hint)
                    } else {
                        String::new()
                    };
                    let combined = format!("{}{}", item.label, hint_part);
                    let truncated = truncate_to(&combined, max_body);
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
                }
                Row::MetaFilter { tag, count } => {
                    let text = format!("{MARK_META} tag:{tag} — filter to {count} skill(s)");
                    queue!(
                        out,
                        SetForegroundColor(COLOR_META),
                        Print(truncate_to(&text, max_body)),
                        ResetColor
                    )?;
                }
                Row::MetaSelectAll { tag, count } => {
                    let text = format!("{MARK_META} tag:{tag} — select all {count}");
                    queue!(
                        out,
                        SetForegroundColor(COLOR_META),
                        Print(truncate_to(&text, max_body)),
                        ResetColor
                    )?;
                }
            }
            queue!(out, Print("\r\n"))?;
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
    let esc = if state.active_tag.is_some() {
        "esc clears filter"
    } else {
        "esc cancel"
    };
    format!("{count} selected • space toggle/act • enter confirm • {esc}")
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
            tags: Vec::new(),
        }
    }

    fn tagged(label: &str, tags: &[&str]) -> PromptItem<()> {
        PromptItem {
            value: (),
            label: label.to_string(),
            hint: String::new(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn state_with(items: Vec<PromptItem<()>>) -> State<()> {
        let mut s = State {
            items,
            ..State::default()
        };
        s.refilter();
        s
    }

    fn skill_indices(s: &State<()>) -> Vec<usize> {
        s.visible
            .iter()
            .filter_map(|r| match r {
                Row::Skill { item_idx } => Some(*item_idx),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn matching_tags_ranks_exact_then_prefix_then_substring() {
        let items = vec![
            tagged("a", &["advideo"]),
            tagged("b", &["video-editing"]),
            tagged("c", &["video"]),
        ];
        let names: Vec<String> = matching_tags("video", &items)
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(names, vec!["video", "video-editing", "advideo"]);
    }

    #[test]
    fn matching_tags_counts_distinct_carriers() {
        let items = vec![
            tagged("a", &["video"]),
            tagged("b", &["video"]),
            tagged("c", &["audio"]),
        ];
        assert_eq!(matching_tags("vid", &items), vec![("video".to_string(), 2)]);
    }

    #[test]
    fn refilter_surfaces_meta_rows_above_skill_matches() {
        let items = vec![
            tagged("alpha", &["video"]),
            tagged("beta", &["audio"]),
            tagged("vidtool", &[]),
        ];
        let mut s = state_with(items);
        s.query = "vid".to_string();
        s.refilter();
        assert_eq!(
            s.visible[0],
            Row::MetaFilter {
                tag: "video".to_string(),
                count: 1
            }
        );
        assert_eq!(
            s.visible[1],
            Row::MetaSelectAll {
                tag: "video".to_string(),
                count: 1
            }
        );
        // Only labels containing "vid" become skill rows.
        assert_eq!(skill_indices(&s), vec![2]);
    }

    #[test]
    fn tag_mode_hides_meta_rows_and_filters_by_tag() {
        let items = vec![
            tagged("alpha", &["video"]),
            tagged("beta", &["video"]),
            tagged("gamma", &["audio"]),
        ];
        let mut s = state_with(items);
        s.enter_tag_filter("video".to_string());
        assert_eq!(s.active_tag.as_deref(), Some("video"));
        assert!(s.visible.iter().all(|r| matches!(r, Row::Skill { .. })));
        assert_eq!(skill_indices(&s), vec![0, 1]);
    }

    #[test]
    fn select_all_tagged_selects_carriers_and_narrows() {
        let items = vec![
            tagged("alpha", &["video"]),
            tagged("beta", &["video"]),
            tagged("gamma", &["audio"]),
        ];
        let mut s = state_with(items);
        s.select_all_tagged("video");
        assert!(s.selected.contains(&0) && s.selected.contains(&1));
        assert!(!s.selected.contains(&2));
        assert_eq!(s.active_tag.as_deref(), Some("video"));
    }

    #[test]
    fn clear_tag_filter_restores_full_list() {
        let items = vec![tagged("alpha", &["video"]), tagged("gamma", &["audio"])];
        let mut s = state_with(items);
        s.enter_tag_filter("video".to_string());
        assert_eq!(skill_indices(&s), vec![0]);
        s.clear_tag_filter();
        assert!(s.active_tag.is_none());
        assert_eq!(skill_indices(&s), vec![0, 1]);
    }

    #[test]
    fn tag_matching_is_case_insensitive() {
        let items = vec![tagged("alpha", &["Video"])];
        let mut s = state_with(items);
        s.query = "vid".to_string();
        s.refilter();
        assert!(matches!(s.visible.first(), Some(Row::MetaFilter { .. })));
        s.select_all_tagged("VIDEO");
        assert!(s.selected.contains(&0));
    }

    #[test]
    fn activate_focused_meta_filter_enters_tag_mode() {
        let items = vec![tagged("alpha", &["video"]), tagged("vidx", &[])];
        let mut s = state_with(items);
        s.query = "vid".to_string();
        s.refilter();
        s.focus = 0; // the MetaFilter row
        s.activate_focused();
        assert_eq!(s.active_tag.as_deref(), Some("video"));
    }

    #[test]
    fn activate_focused_skill_toggles_selection() {
        let items = vec![tagged("alpha", &["video"])];
        let mut s = state_with(items); // no query → single skill row at focus 0
        s.activate_focused();
        assert!(s.selected.contains(&0));
        s.activate_focused();
        assert!(!s.selected.contains(&0));
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

    fn tab_state() -> TabState<()> {
        TabState {
            title: "t".to_string(),
            tabs: vec![
                TabData {
                    label: "a".to_string(),
                    items: vec![item("foo"), item("bar")],
                },
                TabData {
                    label: "b".to_string(),
                    items: vec![item("baz")],
                },
            ],
            required: false,
            active: 0,
            query: String::new(),
            focus: 1,
            selected: HashSet::new(),
            visible: Vec::new(),
            last_lines: 0,
        }
    }

    #[test]
    fn tab_switch_resets_focus_and_refilters() {
        let mut s = tab_state();
        s.refilter();
        assert_eq!(s.visible, vec![0, 1]);
        s.switch_to(1);
        assert_eq!(s.active, 1);
        assert_eq!(s.focus, 0);
        assert_eq!(s.visible, vec![0], "tab b has a single item");
    }

    #[test]
    fn tab_filter_applies_to_active_tab_only() {
        let mut s = tab_state();
        s.query = "ba".to_string();
        s.refilter();
        assert_eq!(s.visible, vec![1], "only `bar` matches in tab a");
        s.switch_to(1);
        assert_eq!(
            s.visible,
            vec![0],
            "`baz` matches in tab b under the same query"
        );
    }

    #[test]
    fn selected_count_is_per_tab() {
        let mut s = tab_state();
        s.selected.insert((0, 0));
        s.selected.insert((0, 1));
        s.selected.insert((1, 0));
        assert_eq!(s.selected_in(0), 2);
        assert_eq!(s.selected_in(1), 1);
    }
}
