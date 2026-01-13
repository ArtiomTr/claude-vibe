//! Claude Code session manager with git worktrees.
//!
//! Manages isolated development sessions using git worktrees and Docker containers,
//! enabling parallel Claude Code sessions without branch conflicts.

mod commands;
mod docker;
mod git;
mod style;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Worktree prefix for Claude sessions
pub const WORKTREE_PREFIX: &str = "claude/";

#[derive(Parser)]
#[command(name = "vibe")]
#[command(about = "Claude Code session manager with git worktrees")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Clone a repository as bare repo with worktree support
    Clone {
        /// Repository URL to clone
        url: String,
        /// Directory name (defaults to repository name)
        directory: Option<String>,
    },

    /// Create a new session with a fresh git worktree
    New,

    /// Attach to an existing session
    Continue {
        /// Name of the worktree to continue
        worktree_name: Option<String>,
    },

    /// Remove worktrees that are synced with remote or unused
    Cleanup {
        /// Interactive mode: select worktrees to delete with TUI
        #[arg(short, long)]
        interactive: bool,
    },

    /// Initialize Dockerfile.vibes for a project
    Setup,

    /// Show status of all worktrees
    #[command(visible_aliases = ["stat", "ls"])]
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Clone { url, directory }) => commands::clone::run(&url, directory),
        Some(Commands::New) => commands::new::run(),
        Some(Commands::Continue { worktree_name }) => {
            commands::continue_session::run(worktree_name).await
        }
        Some(Commands::Cleanup { interactive }) => commands::cleanup::run(interactive).await,
        Some(Commands::Setup) => commands::setup::run(),
        Some(Commands::Status) => commands::status::run().await,
        None => {
            // Default to help
            use clap::CommandFactory;
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}
