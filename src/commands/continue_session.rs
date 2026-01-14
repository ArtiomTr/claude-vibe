//! Continue an existing Claude Code session.

use anyhow::{bail, Result};
use tokio::sync::mpsc;

use crate::{docker, git, tui, WORKTREE_PREFIX};

/// Run the `continue` command: attach to an existing worktree session.
pub async fn run(worktree_name: Option<String>) -> Result<()> {
    git::require_bare_repo()?;

    let name = match worktree_name {
        Some(n) => n,
        None => {
            // Interactive selection
            let worktrees = git::list_claude_worktrees()?;

            if worktrees.is_empty() {
                println!("No claude worktrees found");
                println!("Use 'vibe new' to create a new session");
                bail!("No worktrees available");
            }

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

            // Run interactive selection with async updates
            let selection = tui::run_single_selection_async(items, update_rx).await?;

            match selection {
                Some(idx) => worktrees[idx].branch.clone(),
                None => {
                    // User cancelled selection - exit silently
                    return Ok(());
                }
            }
        }
    };

    let worktree = match git::find_worktree(&name)? {
        Some(wt) => wt,
        None => {
            println!("Error: Worktree '{}' not found", name);
            println!();
            print_available_worktrees()?;
            bail!("Worktree not found");
        }
    };

    // Extract random part from worktree name for image naming
    let random_part = worktree
        .branch
        .strip_prefix(WORKTREE_PREFIX)
        .unwrap_or(&worktree.branch);
    let image_name = format!("claude-vibe-{}", random_part);

    println!("Continuing session in: {}", worktree.path.display());

    let image = docker::prepare_image(&worktree.path, &image_name)?;

    println!("Starting Claude Code session...");
    docker::run_container(&worktree.path, &image, None)
}

/// Print list of available Claude worktrees.
fn print_available_worktrees() -> Result<()> {
    println!("Available worktrees:");
    let worktrees = git::list_claude_worktrees()?;

    if worktrees.is_empty() {
        println!("  No claude worktrees found");
    } else {
        for wt in worktrees {
            println!("  {} ({})", wt.branch, wt.path.display());
        }
    }

    Ok(())
}
