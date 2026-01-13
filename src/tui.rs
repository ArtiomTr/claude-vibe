//! Interactive TUI selection using ratatui.
//!
//! This module provides terminal UI components for selecting sessions
//! with support for both single and multi-selection modes.

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

use crate::git::WorktreeStatus;

/// Maximum height for the inline viewport
const MAX_VIEWPORT_HEIGHT: u16 = 20;

/// Number of lines each item takes (branch name + status + summary)
const LINES_PER_ITEM: usize = 3;

/// Lines used by borders and title
const BORDER_LINES: usize = 2;

/// Polling interval for keyboard events (milliseconds)
const POLL_INTERVAL_MS: u64 = 100;

/// Guard to ensure terminal raw mode is disabled on drop.
/// This prevents leaving the terminal in a broken state if the code panics.
struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Item in the selection list with status information
pub struct WorktreeItem {
    pub branch: String,
    pub status: WorktreeStatus,
    pub summary: Option<String>,
}

/// Application state for single selection
struct SingleSelectApp {
    items: Vec<WorktreeItem>,
    list_state: ListState,
}

impl SingleSelectApp {
    fn new(items: Vec<WorktreeItem>) -> Self {
        let mut list_state = ListState::default();
        if !items.is_empty() {
            list_state.select(Some(0));
        }

        Self { items, list_state }
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
        self.items
            .iter()
            .map(|item| build_worktree_list_item(&item.branch, &item.status, item.summary.as_deref(), false))
            .collect()
    }
}

/// Application state for multi-selection
struct MultiSelectApp {
    items: Vec<WorktreeItem>,
    list_state: ListState,
    selected: Vec<bool>,
}

impl MultiSelectApp {
    fn new(items: Vec<WorktreeItem>) -> Self {
        let len = items.len();
        let mut list_state = ListState::default();
        if !items.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            items,
            list_state,
            selected: vec![false; len],
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
        self.items
            .iter()
            .enumerate()
            .map(|(i, item)| build_worktree_list_item(&item.branch, &item.status, item.summary.as_deref(), self.selected[i]))
            .collect()
    }
}

/// Build a list item for a worktree with status information
fn build_worktree_list_item(
    branch: &str,
    status: &WorktreeStatus,
    summary: Option<&str>,
    is_checked: bool,
) -> ListItem<'static> {
    // Checkbox for multi-select mode
    let checkbox = if is_checked { "[✓] " } else { "[ ] " };

    // Status indicator based on state
    let (status_icon, status_color) = if status.is_orphaned {
        ("✗", Color::Red) // Orphaned (directory missing)
    } else if status.has_uncommitted && status.has_unpushed {
        ("●", Color::Red) // Both uncommitted and unpushed
    } else if status.has_uncommitted {
        ("●", Color::Yellow) // Uncommitted changes
    } else if status.has_unpushed {
        ("●", Color::Blue) // Unpushed commits
    } else {
        ("●", Color::Green) // Clean
    };

    // Build status details
    let detail_text = if status.is_orphaned {
        "Orphaned - directory missing".to_string()
    } else {
        let mut details = Vec::new();
        if status.modified_files > 0 {
            details.push(format!("{} modified", status.modified_files));
        }
        if status.untracked_files > 0 {
            details.push(format!("{} untracked", status.untracked_files));
        }
        if status.commits_ahead > 0 {
            details.push(format!("{} unpushed", status.commits_ahead));
        }

        if details.is_empty() {
            "Clean - safe to delete".to_string()
        } else {
            details.join(", ")
        }
    };

    let mut lines = vec![
        Line::from(vec![
            Span::raw(checkbox.to_string()),
            Span::styled(format!("{} ", status_icon), Style::default().fg(status_color)),
            Span::raw(branch.to_string()),
        ]),
        Line::from(vec![Span::styled(
            format!("       {}", detail_text),
            Style::default().fg(Color::DarkGray),
        )]),
    ];

    // Add summary line if present
    if let Some(summary_text) = summary {
        lines.push(Line::from(vec![Span::styled(
            format!("       {}", summary_text),
            Style::default().fg(Color::Cyan),
        )]));
    }

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

/// Run interactive single selection.
///
/// Displays a scrollable list of worktrees with their status.
/// The user can navigate with arrow keys or j/k and select with Enter.
///
/// # Arguments
///
/// * `items` - List of worktree items with status information
///
/// # Returns
///
/// * `Ok(Some(index))` - User selected the item at the given index
/// * `Ok(None)` - User cancelled selection (Esc, q, or Ctrl+C)
/// * `Err` - Terminal or I/O error occurred
pub fn run_single_selection(items: Vec<WorktreeItem>) -> io::Result<Option<usize>> {
    let item_count = items.len();
    let viewport_height = calculate_viewport_height(item_count);

    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;

    let mut terminal = setup_inline_terminal(viewport_height)?;
    let mut app = SingleSelectApp::new(items);

    let result = loop {
        let items = app.build_list_items();

        terminal.draw(|frame| {
            let area = frame.area();

            let list = List::new(items)
                .block(
                    Block::default()
                        .title(" Select a session (↑/↓ navigate, Enter select, q quit) ")
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
    };

    // Clear the viewport before exiting
    terminal.clear()?;

    Ok(result)
}

/// Run interactive multi-selection.
///
/// Displays a scrollable list of worktrees with checkboxes.
/// The user can navigate with arrow keys, toggle selection with Space,
/// and confirm with Enter.
///
/// # Arguments
///
/// * `items` - List of worktree items with status information
///
/// # Returns
///
/// * `Ok(Some(indices))` - User confirmed selection with the given indices
/// * `Ok(None)` - User cancelled selection (Esc, q, or Ctrl+C)
/// * `Err` - Terminal or I/O error occurred
pub fn run_multi_selection(items: Vec<WorktreeItem>) -> io::Result<Option<Vec<usize>>> {
    let item_count = items.len();
    let viewport_height = calculate_viewport_height(item_count);

    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;

    let mut terminal = setup_inline_terminal(viewport_height)?;
    let mut app = MultiSelectApp::new(items);

    let result = loop {
        let items = app.build_list_items();

        terminal.draw(|frame| {
            let area = frame.area();

            let list = List::new(items)
                .block(
                    Block::default()
                        .title(" Select worktrees (Space toggle, a all, n none, Enter confirm, q quit) ")
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
                            // No selection, treat as cancel
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
    };

    // Clear the viewport before exiting
    terminal.clear()?;

    Ok(result)
}

/// Ask for confirmation with a yes/no prompt.
///
/// # Arguments
///
/// * `message` - The confirmation message to display
///
/// # Returns
///
/// * `Ok(true)` - User confirmed (y/Y/Enter)
/// * `Ok(false)` - User declined (n/N/Esc/q)
/// * `Err` - I/O error occurred
pub fn confirm(message: &str) -> io::Result<bool> {
    crossterm::terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;

    // Print prompt
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
                    KeyCode::Enter => break false, // Default to No
                    _ => {}
                }
            }
        }
    };

    println!();
    Ok(result)
}
