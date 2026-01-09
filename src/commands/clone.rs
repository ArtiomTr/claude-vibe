//! Clone a repository as bare repo with worktree support.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::docker;

const SETUP_PROMPT: &str = "\
Analyze this project and create a Dockerfile.vibes file that includes all necessary \
dependencies and tools for development. The Dockerfile should be based on sirsedev/claude-vibe \
as the base image (which already includes Claude Code). Add any project-specific dependencies \
needed to build and run this project. Please examine the project structure, dependencies, \
and build system to determine the requirements.";

/// Extract repository name from URL.
fn extract_repo_name(url: &str) -> Option<String> {
    // Handle URLs like:
    // - https://github.com/user/repo.git
    // - git@github.com:user/repo.git
    // - /path/to/repo.git
    // - repo.git
    let url = url.trim_end_matches('/');
    let name = url
        .rsplit('/')
        .next()
        .or_else(|| url.rsplit(':').next())?;

    // Remove .git suffix if present
    let name = name.strip_suffix(".git").unwrap_or(name);

    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Run the `clone` command: clone as bare repo, setup worktree structure, run setup.
pub fn run(url: &str, directory: Option<String>) -> Result<()> {
    // Determine target directory name
    let dir_name = match directory {
        Some(d) => d,
        None => extract_repo_name(url).context("Could not determine repository name from URL")?,
    };

    let target_dir = Path::new(&dir_name);

    // Check if directory already exists
    if target_dir.exists() {
        bail!("Directory '{}' already exists", dir_name);
    }

    println!("Cloning {} into {}...", url, dir_name);

    // Create target directory
    fs::create_dir_all(target_dir).context("Failed to create target directory")?;

    let bare_dir = target_dir.join(".bare");

    // Clone as bare repository into .bare subdirectory
    let status = Command::new("git")
        .args(["clone", "--bare", url, bare_dir.to_str().unwrap()])
        .status()
        .context("Failed to run git clone")?;

    if !status.success() {
        // Cleanup on failure
        let _ = fs::remove_dir_all(target_dir);
        bail!("Git clone failed");
    }

    // Create .git file pointing to .bare
    let git_file = target_dir.join(".git");
    fs::write(&git_file, "gitdir: ./.bare\n").context("Failed to create .git file")?;

    // Configure the bare repo to fetch all branches
    let status = Command::new("git")
        .current_dir(target_dir)
        .args(["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"])
        .status()
        .context("Failed to configure remote fetch")?;

    if !status.success() {
        bail!("Failed to configure git remote");
    }

    // Fetch to populate remote refs
    println!("Fetching remote refs...");
    let _ = Command::new("git")
        .current_dir(target_dir)
        .args(["fetch", "origin"])
        .status();

    println!("Repository cloned successfully.");
    println!();

    // Run setup to create Dockerfile.vibes
    println!("Running setup to initialize Dockerfile.vibes...");

    let target_path = fs::canonicalize(target_dir).context("Failed to resolve target path")?;
    let image_name = "claude-vibe-setup";

    // Fresh clone won't have Dockerfile.vibes, so this will use default image
    let image = docker::prepare_image(&target_path, image_name)?;

    println!("Starting Claude Code for project setup...");
    docker::run_container_with_output(&target_path, &image, SETUP_PROMPT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_repo_name() {
        assert_eq!(
            extract_repo_name("https://github.com/user/repo.git"),
            Some("repo".to_string())
        );
        assert_eq!(
            extract_repo_name("git@github.com:user/repo.git"),
            Some("repo".to_string())
        );
        assert_eq!(
            extract_repo_name("https://github.com/user/repo"),
            Some("repo".to_string())
        );
        assert_eq!(extract_repo_name("repo.git"), Some("repo".to_string()));
        assert_eq!(extract_repo_name("repo"), Some("repo".to_string()));
    }
}
