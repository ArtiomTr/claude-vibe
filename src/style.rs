//! Terminal styling helpers using crossterm.
//!
//! Provides styled text output without raw ANSI escape codes.

use crossterm::style::{Color, ResetColor, SetForegroundColor};
use std::io;

/// Print colored text to stdout.
pub fn print_colored(text: &str, color: Color) {
    let mut stdout = io::stdout();
    let _ = crossterm::execute!(stdout, SetForegroundColor(color));
    print!("{}", text);
    let _ = crossterm::execute!(stdout, ResetColor);
}

/// Print colored text with newline.
pub fn println_colored(text: &str, color: Color) {
    print_colored(text, color);
    println!();
}

/// Status indicator colors
pub mod indicators {
    use crossterm::style::Color;

    pub const CLEAN: Color = Color::Green;
    pub const UNCOMMITTED: Color = Color::Yellow;
    pub const UNPUSHED: Color = Color::Blue;
    pub const DANGER: Color = Color::Red;
    pub const DIM: Color = Color::DarkGrey;
}

/// Clear the current line (for updating loading messages).
pub fn clear_line() {
    let mut stdout = io::stdout();
    let _ = crossterm::execute!(
        stdout,
        crossterm::cursor::MoveToColumn(0),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
    );
}
