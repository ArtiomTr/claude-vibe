//! Continue an existing Claude Code session.

use anyhow::{bail, Result};

use crate::{docker, git, WORKTREE_PREFIX};

/// Run the `continue` command: attach to an existing worktree session.
pub fn run(worktree_name: Option<String>) -> Result<()> {
    git::require_bare_repo()?;

    let name = match worktree_name {
        Some(n) => n,
        None => {
            println!("Error: Please specify a worktree name");
            println!("Usage: vibe continue <worktree-name>");
            println!();
            print_available_worktrees()?;
            bail!("No worktree name provided");
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
