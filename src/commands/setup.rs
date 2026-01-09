//! Initialize Dockerfile.vibes for a project by analyzing it with Claude.

use anyhow::Result;

use crate::{docker, git};

const SETUP_PROMPT: &str = "\
Analyze this project and create a Dockerfile.vibes file that includes all necessary \
dependencies and tools for development. The Dockerfile should be based on sirsedev/claude-vibe \
as the base image (which already includes Claude Code). Add any project-specific dependencies \
needed to build and run this project. Please examine the project structure, dependencies, \
and build system to determine the requirements.";

/// Run the `setup` command: analyze project and create Dockerfile.vibes.
pub fn run() -> Result<()> {
    let repo_info = git::require_bare_repo()?;

    let image_name = "claude-vibe-setup";

    // For setup, we use the workspace root (where Dockerfile.vibes will be created)
    let image = docker::prepare_image(&repo_info.workspace_root, image_name)?;

    println!("Starting Claude Code for project setup...");
    docker::run_container_with_output(&repo_info.workspace_root, &image, SETUP_PROMPT)
}
