//! Git utility functions for worktree and repository management.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::WORKTREE_PREFIX;

/// Default Docker image when no Dockerfile.vibes is found.
pub const DEFAULT_IMAGE: &str = "sirsedev/claude-vibe";

/// Information about a git worktree
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Information about a bare repository setup with worktree support.
pub struct BareRepoInfo {
    /// Path to the .bare directory (the actual git repository)
    #[allow(dead_code)]
    pub bare_path: PathBuf,
    /// Path to the workspace root (directory containing .git file)
    pub workspace_root: PathBuf,
}

/// Check if we're in a bare repository setup with worktree support.
///
/// This detects the setup created by `vibe clone`:
/// - A .git file (not directory) pointing to .bare
/// - A .bare directory containing the actual bare repository
///
/// Returns None if not in such a setup (e.g., regular git repo).
pub fn get_bare_repo_info() -> Result<Option<BareRepoInfo>> {
    // Get the git directory path
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .context("Failed to get git directory")?;

    if !output.status.success() {
        return Ok(None);
    }

    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let git_dir_path = PathBuf::from(&git_dir);

    // Get absolute path of git dir
    let git_dir_abs = fs::canonicalize(&git_dir_path).unwrap_or_else(|_| git_dir_path.clone());

    // Check if git dir ends with .bare (our convention)
    if !git_dir_abs.ends_with(".bare") {
        return Ok(None);
    }

    // The workspace root is the parent of .bare
    let workspace_root = git_dir_abs
        .parent()
        .context("Invalid bare repo structure")?
        .to_path_buf();

    // Verify the .git file exists and points to .bare
    let git_file = workspace_root.join(".git");
    if !git_file.is_file() {
        return Ok(None);
    }

    Ok(Some(BareRepoInfo {
        bare_path: git_dir_abs,
        workspace_root,
    }))
}

/// Ensure we're in a valid bare repository setup.
///
/// Returns BareRepoInfo if valid, or an error with helpful message if not.
pub fn require_bare_repo() -> Result<BareRepoInfo> {
    if !is_git_repo() {
        bail!("Not in a git repository");
    }

    get_bare_repo_info()?.ok_or_else(|| {
        anyhow::anyhow!(
            "This command requires a bare repository setup with worktree support.\n\
             Use 'vibe clone <url>' to clone a repository with the correct structure,\n\
             or convert an existing repository to a bare setup."
        )
    })
}

/// Get the main branch name from remote.
pub fn get_main_branch() -> Result<String> {
    let output = Command::new("git")
        .args(["remote", "show", "origin"])
        .output()
        .context("Failed to query remote")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("HEAD branch")
            && let Some(branch) = line.split_whitespace().last()
        {
            return Ok(branch.to_string());
        }
    }

    Ok("main".to_string())
}

/// Create a new git worktree with the given name.
pub fn create_worktree(repo_root: &Path, worktree_name: &str) -> Result<PathBuf> {
    let worktree_path = repo_root.parent().unwrap().join(worktree_name);

    let status = Command::new("git")
        .args([
            "worktree",
            "add",
            worktree_path.to_str().unwrap(),
            "-b",
            worktree_name,
        ])
        .status()
        .context("Failed to create worktree")?;

    if !status.success() {
        bail!("Failed to create worktree");
    }

    std::fs::canonicalize(&worktree_path).context("Failed to resolve worktree path")
}

/// List all Claude worktrees (those starting with the worktree prefix).
pub fn list_claude_worktrees() -> Result<Vec<Worktree>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .output()
        .context("Failed to list worktrees")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(path));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = Some(branch.to_string());
        } else if line.is_empty() {
            if let (Some(path), Some(branch)) = (current_path.take(), current_branch.take())
                && branch.starts_with(WORKTREE_PREFIX)
            {
                worktrees.push(Worktree { path, branch });
            }
            current_path = None;
            current_branch = None;
        }
    }

    // Handle last entry if no trailing newline
    if let (Some(path), Some(branch)) = (current_path, current_branch)
        && branch.starts_with(WORKTREE_PREFIX)
    {
        worktrees.push(Worktree { path, branch });
    }

    Ok(worktrees)
}

/// Find a worktree by name (partial match supported).
pub fn find_worktree(name: &str) -> Result<Option<Worktree>> {
    let worktrees = list_claude_worktrees()?;

    for wt in worktrees {
        let path_str = wt.path.to_string_lossy();
        if path_str.contains(name) || wt.branch.contains(name) {
            return Ok(Some(wt));
        }
    }

    Ok(None)
}

/// Check if worktree is synced with remote (branch exists and commits match).
pub fn is_worktree_synced(worktree_path: &Path) -> Result<bool> {
    let branch = get_worktree_branch(worktree_path)?;

    // Check if branch exists on remote
    let remote_check = Command::new("git")
        .current_dir(worktree_path)
        .args(["ls-remote", "--exit-code", "--heads", "origin", &branch])
        .output()?;

    if !remote_check.status.success() {
        return Ok(false);
    }

    // Fetch latest
    let _ = Command::new("git")
        .current_dir(worktree_path)
        .args(["fetch", "origin", &branch])
        .output();

    // Compare local and remote commits
    let local = Command::new("git")
        .current_dir(worktree_path)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let local_commit = String::from_utf8_lossy(&local.stdout).trim().to_string();

    let remote = Command::new("git")
        .current_dir(worktree_path)
        .args(["rev-parse", &format!("origin/{}", branch)])
        .output()?;

    if !remote.status.success() {
        return Ok(false);
    }

    let remote_commit = String::from_utf8_lossy(&remote.stdout).trim().to_string();

    Ok(local_commit == remote_commit)
}

/// Check if worktree is unused (no commits beyond base, no changes).
pub fn is_worktree_unused(worktree_path: &Path) -> Result<bool> {
    // Check for uncommitted changes
    let diff = Command::new("git")
        .current_dir(worktree_path)
        .args(["diff", "--quiet", "HEAD"])
        .status()?;

    if !diff.success() {
        return Ok(false);
    }

    // Check for staged changes
    let staged = Command::new("git")
        .current_dir(worktree_path)
        .args(["diff", "--cached", "--quiet", "HEAD"])
        .status()?;

    if !staged.success() {
        return Ok(false);
    }

    // Check for untracked files (excluding .claude directory)
    let untracked = Command::new("git")
        .current_dir(worktree_path)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()?;

    let untracked_output = String::from_utf8_lossy(&untracked.stdout);
    let has_untracked = untracked_output.lines().any(|f| !f.starts_with(".claude/"));

    if has_untracked {
        return Ok(false);
    }

    // Check if there are commits beyond the main branch
    let main_branch = get_main_branch().unwrap_or_else(|_| "main".to_string());
    let branch = get_worktree_branch(worktree_path)?;

    let commits_ahead = Command::new("git")
        .current_dir(worktree_path)
        .args([
            "rev-list",
            "--count",
            &format!("origin/{}..{}", main_branch, branch),
        ])
        .output()?;

    let count: i32 = String::from_utf8_lossy(&commits_ahead.stdout)
        .trim()
        .parse()
        .unwrap_or(1);

    Ok(count == 0)
}

/// Get the current branch name for a worktree.
pub fn get_worktree_branch(worktree_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(worktree_path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("Failed to get branch name")?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Remove a worktree and optionally its branch.
pub fn remove_worktree(worktree_path: &Path, delete_branch: bool) -> Result<()> {
    let branch = get_worktree_branch(worktree_path)?;

    Command::new("git")
        .args([
            "worktree",
            "remove",
            worktree_path.to_str().unwrap(),
            "--force",
        ])
        .status()
        .context("Failed to remove worktree")?;

    if delete_branch {
        let _ = Command::new("git").args(["branch", "-D", &branch]).status();
    }

    Ok(())
}

/// Check if current directory is a git repository.
pub fn is_git_repo() -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
