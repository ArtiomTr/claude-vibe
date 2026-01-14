//! Interactive TUI selection using ratatui.
//!
//! This module provides terminal UI components for selecting sessions
//! with support for both single and multi-selection modes, and async
//! status and summary updates.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Terminal, TerminalOptions, Viewport,
};
use std::io::{self, stdout, Stdout};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::git::WorktreeStatus;

/// Maximum height for the inline viewport
const MAX_VIEWPORT_HEIGHT: u16 = 20;

/// Number of lines each item takes (branch name + status + summary)
const LINES_PER_ITEM: usize = 3;

/// Lines used by borders and title
const BORDER_LINES: usize = 2;

/// Polling interval for keyboard events (milliseconds)
const POLL_INTERVAL_MS: u64 = 50;

/// Spinner frames for animation
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Summary loading state
#[derive(Clone, PartialEq)]
pub enum SummaryState {
    /// No summary needed or not yet determined
    None,
    /// Waiting in queue to be summarized
    Queued,
    /// Currently being summarized by Claude
    Summarizing,
    /// Summary complete
    Done(String),
}

/// Guard to ensure terminal raw mode is disabled on drop.
struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Item in the selection list with status information
pub struct WorktreeItem {
    pub branch: String,
    pub status: Option<WorktreeStatus>,
    pub summary_state: SummaryState,
}

/// Async update message for status or summary
pub enum WorktreeUpdate {
    Status { index: usize, status: WorktreeStatus },
    SummaryStarted { index: usize },
    Summary { index: usize, summary: String },
}

/// Application state for single selection with async updates
struct SingleSelectApp {
    items: Vec<WorktreeItem>,
    list_state: ListState,
    pending_status: usize,
    pending_summaries: usize,
    frame: usize,
}

impl SingleSelectApp {
    fn new(items: Vec<WorktreeItem>) -> Self {
        let pending_status = items.iter().filter(|i| i.status.is_none()).count();
        let pending_summaries = items
            .iter()
            .filter(|i| matches!(i.summary_state, SummaryState::Queued | SummaryState::Summarizing))
            .count();

        let mut list_state = ListState::default();
        if !items.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            items,
            list_state,
            pending_status,
            pending_summaries,
            frame: 0,
        }
    }

    fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    fn spinner_char(&self) -> char {
        SPINNER_FRAMES[self.frame % SPINNER_FRAMES.len()]
    }

    fn update_status(&mut self, index: usize, status: WorktreeStatus) {
        if let Some(item) = self.items.get_mut(index) {
            // If this item needs a summary, mark as queued
            if status.has_uncommitted && !status.is_orphaned {
                if item.summary_state == SummaryState::None {
                    item.summary_state = SummaryState::Queued;
                    self.pending_summaries += 1;
                }
            }
            if item.status.is_none() {
                self.pending_status = self.pending_status.saturating_sub(1);
            }
            item.status = Some(status);
        }
    }

    fn update_summary_started(&mut self, index: usize) {
        if let Some(item) = self.items.get_mut(index) {
            item.summary_state = SummaryState::Summarizing;
        }
    }

    fn update_summary(&mut self, index: usize, summary: String) {
        if let Some(item) = self.items.get_mut(index) {
            if matches!(item.summary_state, SummaryState::Queued | SummaryState::Summarizing) {
                self.pending_summaries = self.pending_summaries.saturating_sub(1);
            }
            item.summary_state = SummaryState::Done(summary);
        }
    }

    fn move_up(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.list_state.select(Some(i));
    }

    fn move_down(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let max_idx = self.items.len().saturating_sub(1);
        let i = self
            .list_state
            .selected()
            .map(|i| (i + 1).min(max_idx))
            .unwrap_or(0);
        self.list_state.select(Some(i));
    }

    fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    fn build_list_items(&self) -> Vec<ListItem<'static>> {
        let selected_idx = self.list_state.selected();
        let spinner = self.spinner_char();
        self.items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                build_worktree_list_item(
                    &item.branch,
                    item.status.as_ref(),
                    &item.summary_state,
                    false,
                    false, // no checkbox for single-select
                    selected_idx == Some(i),
                    spinner,
                )
            })
            .collect()
    }

    fn build_title(&self) -> String {
        let base = " Select a session (↑/↓ navigate, Enter select, q quit)";
        let mut indicators = Vec::new();

        if self.pending_status > 0 {
            indicators.push(format!("Loading: {}", self.pending_status));
        }
        if self.pending_summaries > 0 {
            indicators.push(format!("Summarizing: {}", self.pending_summaries));
        }

        if indicators.is_empty() {
            format!("{} ", base)
        } else {
            format!("{} [{}] ", base, indicators.join(", "))
        }
    }
}

/// Application state for multi-selection with async updates
struct MultiSelectApp {
    items: Vec<WorktreeItem>,
    list_state: ListState,
    selected: Vec<bool>,
    pending_status: usize,
    pending_summaries: usize,
    frame: usize,
}

impl MultiSelectApp {
    fn new(items: Vec<WorktreeItem>) -> Self {
        let len = items.len();
        let pending_status = items.iter().filter(|i| i.status.is_none()).count();
        let pending_summaries = items
            .iter()
            .filter(|i| matches!(i.summary_state, SummaryState::Queued | SummaryState::Summarizing))
            .count();

        let mut list_state = ListState::default();
        if !items.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            items,
            list_state,
            selected: vec![false; len],
            pending_status,
            pending_summaries,
            frame: 0,
        }
    }

    fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    fn spinner_char(&self) -> char {
        SPINNER_FRAMES[self.frame % SPINNER_FRAMES.len()]
    }

    fn update_status(&mut self, index: usize, status: WorktreeStatus) {
        if let Some(item) = self.items.get_mut(index) {
            if status.has_uncommitted && !status.is_orphaned {
                if item.summary_state == SummaryState::None {
                    item.summary_state = SummaryState::Queued;
                    self.pending_summaries += 1;
                }
            }
            if item.status.is_none() {
                self.pending_status = self.pending_status.saturating_sub(1);
            }
            item.status = Some(status);
        }
    }

    fn update_summary_started(&mut self, index: usize) {
        if let Some(item) = self.items.get_mut(index) {
            item.summary_state = SummaryState::Summarizing;
        }
    }

    fn update_summary(&mut self, index: usize, summary: String) {
        if let Some(item) = self.items.get_mut(index) {
            if matches!(item.summary_state, SummaryState::Queued | SummaryState::Summarizing) {
                self.pending_summaries = self.pending_summaries.saturating_sub(1);
            }
            item.summary_state = SummaryState::Done(summary);
        }
    }

    fn move_up(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.list_state.select(Some(i));
    }

    fn move_down(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let max_idx = self.items.len().saturating_sub(1);
        let i = self
            .list_state
            .selected()
            .map(|i| (i + 1).min(max_idx))
            .unwrap_or(0);
        self.list_state.select(Some(i));
    }

    fn toggle_current(&mut self) {
        if let Some(idx) = self.list_state.selected() {
            self.selected[idx] = !self.selected[idx];
        }
    }

    fn select_all(&mut self) {
        for s in &mut self.selected {
            *s = true;
        }
    }

    fn deselect_all(&mut self) {
        for s in &mut self.selected {
            *s = false;
        }
    }

    fn get_selected_indices(&self) -> Vec<usize> {
        self.selected
            .iter()
            .enumerate()
            .filter_map(|(i, &selected)| if selected { Some(i) } else { None })
            .collect()
    }

    fn build_list_items(&self) -> Vec<ListItem<'static>> {
        let selected_idx = self.list_state.selected();
        let spinner = self.spinner_char();
        self.items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                build_worktree_list_item(
                    &item.branch,
                    item.status.as_ref(),
                    &item.summary_state,
                    self.selected[i],
                    true, // show checkbox for multi-select
                    selected_idx == Some(i),
                    spinner,
                )
            })
            .collect()
    }

    fn build_title(&self) -> String {
        let base = " Select worktrees (Space toggle, a all, n none, Enter confirm, q quit)";
        let mut indicators = Vec::new();

        if self.pending_status > 0 {
            indicators.push(format!("Loading: {}", self.pending_status));
        }
        if self.pending_summaries > 0 {
            indicators.push(format!("Summarizing: {}", self.pending_summaries));
        }

        if indicators.is_empty() {
            format!("{} ", base)
        } else {
            format!("{} [{}] ", base, indicators.join(", "))
        }
    }
}

/// Build a list item for a worktree with status information
fn build_worktree_list_item(
    branch: &str,
    status: Option<&WorktreeStatus>,
    summary_state: &SummaryState,
    is_checked: bool,
    show_checkbox: bool,
    is_selected: bool,
    spinner: char,
) -> ListItem<'static> {
    // Checkbox only for multi-select mode
    let prefix = if show_checkbox {
        if is_checked { "[✓] " } else { "[ ] " }
    } else {
        ""
    };
    let indent = if show_checkbox { "      " } else { "  " };

    // Status indicator based on state
    let (status_icon, status_color, show_summary_line) = match status {
        None => ("◌", Color::DarkGray, false),
        Some(s) if s.is_orphaned => ("✗", Color::Red, false),
        Some(s) => {
            let icon_color = if s.has_uncommitted && s.has_unpushed {
                ("●", Color::Red)
            } else if s.has_uncommitted {
                ("●", Color::Yellow)
            } else if s.has_unpushed {
                ("●", Color::Blue)
            } else {
                ("●", Color::Green)
            };
            let show_summary = s.has_uncommitted && !s.is_orphaned;
            (icon_color.0, icon_color.1, show_summary)
        }
    };

    // First line: branch name with status icon
    let mut lines = vec![Line::from(vec![
        Span::raw(prefix.to_string()),
        Span::styled(
            format!("{} ", status_icon),
            Style::default().fg(status_color),
        ),
        Span::raw(branch.to_string()),
    ])];

    // Second line: description/summary with spinner
    if show_summary_line {
        let (summary_text, color) = match summary_state {
            SummaryState::None => ("".to_string(), Color::DarkGray),
            SummaryState::Queued => (
                format!("{} Queued", spinner),
                if is_selected { Color::White } else { Color::DarkGray },
            ),
            SummaryState::Summarizing => (
                format!("{} Summarizing...", spinner),
                if is_selected { Color::White } else { Color::DarkGray },
            ),
            SummaryState::Done(text) => (
                text.clone(),
                if is_selected { Color::White } else { Color::DarkGray },
            ),
        };
        if !summary_text.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                format!("{}{}", indent, summary_text),
                Style::default().fg(color),
            )]));
        }
    }

    // Third line: git status details
    let status_line = match status {
        None => Line::from(vec![Span::styled(
            format!("{}{} Loading...", indent, spinner),
            Style::default().fg(if is_selected { Color::White } else { Color::DarkGray }),
        )]),
        Some(s) if s.is_orphaned => Line::from(vec![Span::styled(
            format!("{}Orphaned - directory missing", indent),
            Style::default().fg(Color::Red),
        )]),
        Some(s) => {
            let mut spans = vec![Span::raw(indent.to_string())];

            // Count untracked files as added lines
            let total_added = s.lines_added + s.untracked_files;
            let has_changes = total_added > 0 || s.lines_deleted > 0;
            let has_unpushed = s.commits_ahead > 0;

            if !has_changes && !has_unpushed {
                spans.push(Span::styled(
                    "Clean",
                    Style::default().fg(if is_selected { Color::White } else { Color::DarkGray }),
                ));
            } else {
                // Dimmed green for additions (including untracked)
                if total_added > 0 {
                    spans.push(Span::styled(
                        format!("+{}", total_added),
                        Style::default().fg(Color::Rgb(80, 160, 80)),
                    ));
                    if s.lines_deleted > 0 || has_unpushed {
                        spans.push(Span::styled(" ", Style::default()));
                    }
                }

                // Dimmed red for deletions
                if s.lines_deleted > 0 {
                    spans.push(Span::styled(
                        format!("-{}", s.lines_deleted),
                        Style::default().fg(Color::Rgb(180, 80, 80)),
                    ));
                    if has_unpushed {
                        spans.push(Span::styled(" ", Style::default()));
                    }
                }

                // Unpushed commits
                if has_unpushed {
                    spans.push(Span::styled(
                        format!("↑{}", s.commits_ahead),
                        Style::default().fg(Color::Rgb(100, 140, 180)),
                    ));
                }
            }

            Line::from(spans)
        }
    };
    lines.push(status_line);

    ListItem::new(lines)
}

/// Calculate viewport height based on item count.
fn calculate_viewport_height(item_count: usize) -> u16 {
    let needed = item_count
        .saturating_mul(LINES_PER_ITEM)
        .saturating_add(BORDER_LINES);
    (needed as u16).min(MAX_VIEWPORT_HEIGHT)
}

/// Setup terminal with inline viewport.
fn setup_inline_terminal(height: u16) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    let backend = CrosstermBackend::new(stdout());
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )
}

/// Run interactive single selection with async status and summary updates.
///
/// Shows the TUI immediately and updates as data arrives.
pub async fn run_single_selection_async(
    items: Vec<WorktreeItem>,
    mut update_rx: mpsc::UnboundedReceiver<WorktreeUpdate>,
) -> io::Result<Option<usize>> {
    let item_count = items.len();
    let viewport_height = calculate_viewport_height(item_count);

    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;

    let mut terminal = setup_inline_terminal(viewport_height)?;
    let mut app = SingleSelectApp::new(items);

    let result = loop {
        app.tick();
        let list_items = app.build_list_items();
        let title = app.build_title();

        terminal.draw(|frame| {
            let area = frame.area();

            let list = List::new(list_items)
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                )
                .highlight_style(
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .bg(Color::DarkGray),
                )
                .highlight_symbol("> ");

            frame.render_stateful_widget(list, area, &mut app.list_state);
        })?;

        // Check for keyboard events (non-blocking)
        if event::poll(Duration::from_millis(POLL_INTERVAL_MS))? {
            if let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = event::read()?
            {
                match code {
                    KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                    KeyCode::Enter => break app.selected(),
                    KeyCode::Esc | KeyCode::Char('q') => break None,
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break None,
                    _ => {}
                }
            }
        }

        // Check for updates (non-blocking)
        while let Ok(update) = update_rx.try_recv() {
            match update {
                WorktreeUpdate::Status { index, status } => app.update_status(index, status),
                WorktreeUpdate::SummaryStarted { index } => app.update_summary_started(index),
                WorktreeUpdate::Summary { index, summary } => app.update_summary(index, summary),
            }
        }
    };

    terminal.clear()?;
    Ok(result)
}

/// Run interactive multi-selection with async status and summary updates.
///
/// Shows the TUI immediately and updates as data arrives.
pub async fn run_multi_selection_async(
    items: Vec<WorktreeItem>,
    mut update_rx: mpsc::UnboundedReceiver<WorktreeUpdate>,
) -> io::Result<Option<Vec<usize>>> {
    let item_count = items.len();
    let viewport_height = calculate_viewport_height(item_count);

    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;

    let mut terminal = setup_inline_terminal(viewport_height)?;
    let mut app = MultiSelectApp::new(items);

    let result = loop {
        app.tick();
        let list_items = app.build_list_items();
        let title = app.build_title();

        terminal.draw(|frame| {
            let area = frame.area();

            let list = List::new(list_items)
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                )
                .highlight_style(
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .bg(Color::DarkGray),
                )
                .highlight_symbol("> ");

            frame.render_stateful_widget(list, area, &mut app.list_state);
        })?;

        // Check for keyboard events (non-blocking)
        if event::poll(Duration::from_millis(POLL_INTERVAL_MS))? {
            if let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = event::read()?
            {
                match code {
                    KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                    KeyCode::Char(' ') => app.toggle_current(),
                    KeyCode::Char('a') => app.select_all(),
                    KeyCode::Char('n') => app.deselect_all(),
                    KeyCode::Enter => {
                        let selected = app.get_selected_indices();
                        if selected.is_empty() {
                            break None;
                        } else {
                            break Some(selected);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => break None,
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break None,
                    _ => {}
                }
            }
        }

        // Check for updates (non-blocking)
        while let Ok(update) = update_rx.try_recv() {
            match update {
                WorktreeUpdate::Status { index, status } => app.update_status(index, status),
                WorktreeUpdate::SummaryStarted { index } => app.update_summary_started(index),
                WorktreeUpdate::Summary { index, summary } => app.update_summary(index, summary),
            }
        }
    };

    terminal.clear()?;
    Ok(result)
}

/// Ask for confirmation with a yes/no prompt.
pub fn confirm(message: &str) -> io::Result<bool> {
    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;

    print!("{} [y/N] ", message);
    io::Write::flush(&mut stdout())?;

    let result = loop {
        if event::poll(Duration::from_millis(POLL_INTERVAL_MS))? {
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => break true,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                        break false
                    }
                    KeyCode::Enter => break false,
                    _ => {}
                }
            }
        }
    };

    println!();
    Ok(result)
}
