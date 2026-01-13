//! Show status of all Claude worktrees.

use anyhow::Result;
use std::io::{self, Write};

use crate::{git, style};

/// Run the `status` command: show all worktrees with their status.
pub async fn run() -> Result<()> {
    git::require_bare_repo()?;

    let worktrees = git::list_claude_worktrees()?;

    if worktrees.is_empty() {
        println!("No claude worktrees found");
        println!("Use 'vibe new' to create a new session");
        return Ok(());
    }

    // Show loading message
    print!("Loading worktree status...");
    io::stdout().flush()?;

    // Fetch statuses and summaries in parallel
    let mut handles = Vec::new();
    for wt in &worktrees {
        let path = wt.path.clone();
        let branch = wt.branch.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            let status = git::get_worktree_status(&path).unwrap_or_default();
            let summary = if status.has_uncommitted && !status.is_orphaned {
                git::get_ai_summary(&path)
            } else {
                None
            };
            (branch, status, summary)
        }));
    }

    // Collect results
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await?);
    }

    // Clear loading message
    style::clear_line();

    println!("Claude worktrees:\n");

    for (branch, status, summary) in &results {
        // Status indicator
        let (icon, color) = if status.is_orphaned {
            ("✗", style::indicators::DANGER)
        } else if status.has_uncommitted && status.has_unpushed {
            ("●", style::indicators::DANGER)
        } else if status.has_uncommitted {
            ("●", style::indicators::UNCOMMITTED)
        } else if status.has_unpushed {
            ("●", style::indicators::UNPUSHED)
        } else {
            ("●", style::indicators::CLEAN)
        };

        style::print_colored(icon, color);
        println!(" {}", branch);

        // Build status details
        if status.is_orphaned {
            print!("    ");
            style::println_colored("Orphaned - directory missing", style::indicators::DANGER);
        } else {
            let mut details = Vec::new();

            if status.modified_files > 0 {
                details.push(format!("{} modified", status.modified_files));
            }
            if status.untracked_files > 0 {
                details.push(format!("{} untracked", status.untracked_files));
            }
            if status.commits_ahead > 0 {
                details.push(format!("{} unpushed commit(s)", status.commits_ahead));
            }

            if details.is_empty() {
                print!("    ");
                style::println_colored("Clean - safe to delete", style::indicators::DIM);
            } else {
                print!("    ");
                style::println_colored(&details.join(", "), style::indicators::DIM);
            }

            // Show AI summary if available
            if let Some(summary) = summary {
                print!("    ");
                style::println_colored(summary, style::indicators::CYAN);
            }
        }

        println!();
    }

    // Legend
    print_legend();

    Ok(())
}

/// Print the color legend.
fn print_legend() {
    style::print_colored("Legend: ", style::indicators::DIM);
    style::print_colored("●", style::indicators::CLEAN);
    style::print_colored(" clean  ", style::indicators::DIM);
    style::print_colored("●", style::indicators::UNCOMMITTED);
    style::print_colored(" uncommitted  ", style::indicators::DIM);
    style::print_colored("●", style::indicators::UNPUSHED);
    style::print_colored(" unpushed  ", style::indicators::DIM);
    style::print_colored("●", style::indicators::DANGER);
    style::print_colored(" both  ", style::indicators::DIM);
    style::print_colored("✗", style::indicators::DANGER);
    style::println_colored(" orphaned", style::indicators::DIM);
}
