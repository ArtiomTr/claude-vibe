//! Create a new Claude Code session with a fresh git worktree.

use anyhow::Result;
use rand::Rng;

use crate::{WORKTREE_PREFIX, docker, git};

/// Generate a random alphanumeric string for worktree naming.
fn generate_random_name(length: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();

    (0..length)
        .map(|_| {
            let idx = rng.random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Run the `new` command: create worktree, build image, start session.
pub fn run() -> Result<()> {
    let repo_info = git::require_bare_repo()?;

    let random_name = generate_random_name(8);
    let worktree_name = format!("{}{}", WORKTREE_PREFIX, random_name);
    let image_name = format!("claude-vibe-{}", random_name);

    println!("Creating new worktree: {}", worktree_name);
    let worktree_path = git::create_worktree(&repo_info.workspace_root, &worktree_name)?;

    let image = docker::prepare_image(&worktree_path, &image_name)?;

    println!("Starting Claude Code session...");
    docker::run_container(&worktree_path, &image, None)
}
