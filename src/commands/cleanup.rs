//! Clean up worktrees that are synced with remote or unused.

use anyhow::Result;
use tokio::sync::mpsc;

use crate::{git, style, tui};

/// Run the `cleanup` command: remove synced or unused worktrees.
///
/// In default mode, automatically removes worktrees that are:
/// - Synced with remote (branch pushed and up-to-date)
/// - Unused (no commits beyond base, no changes)
///
/// In interactive mode (-i), shows a TUI for selecting which worktrees to delete.
pub async fn run(interactive: bool) -> Result<()> {
    git::require_bare_repo()?;

    let worktrees = git::list_claude_worktrees()?;

    if worktrees.is_empty() {
        println!("No claude worktrees found");
        return Ok(());
    }

    if interactive {
        run_interactive(worktrees).await
    } else {
        run_automatic(worktrees)
    }
}

/// Run automatic cleanup (default mode)
fn run_automatic(worktrees: Vec<git::Worktree>) -> Result<()> {
    println!("Checking worktrees for cleanup...\n");

    let mut cleaned = 0;

    for wt in worktrees {
        let status = git::get_worktree_status(&wt.path).unwrap_or_default();

        print!("  {} ", wt.branch);

        if status.is_orphaned {
            style::print_colored("✗", style::indicators::DANGER);
            println!(" orphaned (directory missing), removing...");
            git::remove_worktree_with_branch(&wt.path, &wt.branch, true)?;
            cleaned += 1;
        } else if status.is_safe_to_delete() {
            if git::is_worktree_synced(&wt.path)? {
                style::print_colored("✓", style::indicators::CLEAN);
                println!(" synced, removing...");
                git::remove_worktree_with_branch(&wt.path, &wt.branch, true)?;
                cleaned += 1;
            } else if git::is_worktree_unused(&wt.path)? {
                style::print_colored("✓", style::indicators::CLEAN);
                println!(" unused, removing...");
                git::remove_worktree_with_branch(&wt.path, &wt.branch, true)?;
                cleaned += 1;
            } else {
                style::print_colored("-", style::indicators::DIM);
                println!(" keeping (has commits)");
            }
        } else {
            style::print_colored("!", style::indicators::UNCOMMITTED);
            println!(" keeping (has local changes)");
        }
    }

    println!();
    println!("Cleaned up {} worktree(s)", cleaned);

    Ok(())
}

/// Run interactive cleanup with TUI selection
async fn run_interactive(worktrees: Vec<git::Worktree>) -> Result<()> {
    // Create items with just branch names (status will be loaded async)
    let items: Vec<_> = worktrees
        .iter()
        .map(|wt| tui::WorktreeItem {
            branch: wt.branch.clone(),
            status: None,
            summary_state: tui::SummaryState::None,
        })
        .collect();

    // Create channel for async updates
    let (update_tx, update_rx) = mpsc::unbounded_channel();

    // Spawn background tasks to fetch status and summaries
    for (index, wt) in worktrees.iter().enumerate() {
        let path = wt.path.clone();
        let tx = update_tx.clone();

        tokio::task::spawn_blocking(move || {
            // First fetch status
            let status = git::get_worktree_status(&path).unwrap_or_default();
            let needs_summary = status.has_uncommitted && !status.is_orphaned;
            let _ = tx.send(tui::WorktreeUpdate::Status {
                index,
                status: status.clone(),
            });

            // Then fetch AI summary if needed
            if needs_summary {
                let _ = tx.send(tui::WorktreeUpdate::SummaryStarted { index });
                if let Some(summary) = git::get_ai_summary(&path) {
                    let _ = tx.send(tui::WorktreeUpdate::Summary { index, summary });
                }
            }
        });
    }

    // Drop the original sender so the channel closes when all tasks complete
    drop(update_tx);

    // Run multi-selection TUI with async updates
    let selection = tui::run_multi_selection_async(items, update_rx).await?;

    let Some(indices) = selection else {
        // User cancelled
        return Ok(());
    };

    if indices.is_empty() {
        return Ok(());
    }

    // Collect selected worktrees and check for ones with changes
    let selected_worktrees: Vec<_> = indices.iter().map(|&i| &worktrees[i]).collect();

    let worktrees_with_changes: Vec<_> = selected_worktrees
        .iter()
        .filter(|wt| {
            git::get_worktree_status(&wt.path)
                .map(|s| s.has_local_changes())
                .unwrap_or(false)
        })
        .collect();

    // If any selected worktrees have changes, ask for confirmation
    if !worktrees_with_changes.is_empty() {
        println!();
        style::print_colored("Warning:", style::indicators::UNCOMMITTED);
        println!(
            " {} worktree(s) have uncommitted or unpushed changes:",
            worktrees_with_changes.len()
        );
        for wt in &worktrees_with_changes {
            let status = git::get_worktree_status(&wt.path).unwrap_or_default();
            let mut details = Vec::new();
            let total_added = status.lines_added + status.untracked_files;
            if total_added > 0 {
                details.push(format!("+{}", total_added));
            }
            if status.lines_deleted > 0 {
                details.push(format!("-{}", status.lines_deleted));
            }
            if status.commits_ahead > 0 {
                details.push(format!("↑{}", status.commits_ahead));
            }
            println!("  - {} ({})", wt.branch, details.join(" "));
        }
        println!();

        if !tui::confirm("Delete these worktrees anyway?")? {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Delete selected worktrees
    println!();
    let mut deleted = 0;
    for wt in selected_worktrees {
        print!("Removing {}... ", wt.branch);
        match git::remove_worktree_with_branch(&wt.path, &wt.branch, true) {
            Ok(()) => {
                style::println_colored("done", style::indicators::CLEAN);
                deleted += 1;
            }
            Err(e) => {
                style::print_colored("failed: ", style::indicators::DANGER);
                println!("{}", e);
            }
        }
    }

    println!();
    println!("Deleted {} worktree(s)", deleted);

    Ok(())
}
