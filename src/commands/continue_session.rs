//! Continue an existing Claude Code session.

use anyhow::{Result, bail};
use std::io::{self, Write};

use crate::{WORKTREE_PREFIX, docker, git, tui};

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

            // Show loading message
            print!("Loading worktree status...");
            io::stdout().flush()?;

            // Fetch statuses and summaries in parallel using tokio
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
                    tui::WorktreeItem { branch, status, summary }
                }));
            }

            // Collect results
            let mut items = Vec::with_capacity(handles.len());
            for handle in handles {
                items.push(handle.await?);
            }

            // Clear loading message
            print!("\r\x1b[K");
            io::stdout().flush()?;

            // Run interactive selection
            let selection = tui::run_single_selection(items)?;

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
