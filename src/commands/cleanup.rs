//! Clean up worktrees that are synced with remote or unused.

use anyhow::Result;

use crate::git;

/// Run the `cleanup` command: remove synced or unused worktrees.
pub fn run() -> Result<()> {
    git::require_bare_repo()?;

    println!("Checking worktrees for cleanup...");

    let worktrees = git::list_claude_worktrees()?;
    let mut cleaned = 0;

    for wt in worktrees {
        println!("Checking: {}", wt.path.display());

        if git::is_worktree_synced(&wt.path)? {
            println!("  Synced with remote, removing...");
            git::remove_worktree(&wt.path, true)?;
            cleaned += 1;
        } else if git::is_worktree_unused(&wt.path)? {
            println!("  Unused (no commits, no changes), removing...");
            git::remove_worktree(&wt.path, true)?;
            cleaned += 1;
        } else {
            println!("  Has local changes or commits, keeping");
        }
    }

    println!();
    println!("Cleaned up {} worktree(s)", cleaned);

    Ok(())
}
