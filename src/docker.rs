//! Docker utility functions for building images and running containers.

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::git;

/// Claude stream-json event types
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum ClaudeEvent {
    #[serde(rename = "assistant")]
    Assistant { message: AssistantMessage },
    #[serde(rename = "user")]
    User { message: UserMessage },
    #[serde(rename = "result")]
    Result { result: String, cost_usd: Option<f64> },
    System { message: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UserMessage {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum ContentBlock {
    Text { text: String },
    ToolUse { name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
    #[serde(other)]
    Unknown,
}

/// Display a Claude event in human-readable format
fn display_event(event: &ClaudeEvent, spinner: &ProgressBar) {
    match event {
        ClaudeEvent::Assistant { message } => {
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        spinner.suspend(|| {
                            println!("\x1b[36mClaude:\x1b[0m {}", text);
                        });
                    }
                    ContentBlock::ToolUse { name, input } => {
                        spinner.set_message(format!("Running {}...", name));
                        let input_summary = summarize_tool_input(name, input);
                        spinner.suspend(|| {
                            println!("\x1b[33m> {}\x1b[0m {}", name, input_summary);
                        });
                    }
                    _ => {}
                }
            }
        }
        ClaudeEvent::Result { cost_usd, .. } => {
            if let Some(cost) = cost_usd {
                spinner.suspend(|| {
                    println!("\x1b[90mCost: ${:.4}\x1b[0m", cost);
                });
            }
        }
        ClaudeEvent::System { message } => {
            spinner.suspend(|| {
                println!("\x1b[90m[System] {}\x1b[0m", message);
            });
        }
        _ => {}
    }
}

/// Summarize tool input for display
fn summarize_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "Read" | "Write" | "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default(),
        "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default(),
        "Grep" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default(),
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.len() > 60 {
                    format!("{}...", &s[..60])
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Source of Docker image to use.
pub enum ImageSource {
    /// Build from a Dockerfile.vibes at the given path
    BuildFrom { dockerfile: PathBuf, context: PathBuf },
    /// Use the default pre-built image
    UseDefault,
}

/// Determine the Docker image source based on Dockerfile.vibes location.
///
/// Search order:
/// 1. Dockerfile.vibes in the worktree path
/// 2. Dockerfile.vibes in the bare repo workspace root
/// 3. Fall back to default sirsedev/claude-vibe image
pub fn find_image_source(worktree_path: &Path) -> Result<ImageSource> {
    // First: check worktree path
    let worktree_dockerfile = worktree_path.join("Dockerfile.vibes");
    if worktree_dockerfile.exists() {
        return Ok(ImageSource::BuildFrom {
            dockerfile: worktree_dockerfile,
            context: worktree_path.to_path_buf(),
        });
    }

    // Second: check bare repo workspace root
    if let Some(repo_info) = git::get_bare_repo_info()? {
        let workspace_dockerfile = repo_info.workspace_root.join("Dockerfile.vibes");
        if workspace_dockerfile.exists() {
            return Ok(ImageSource::BuildFrom {
                dockerfile: workspace_dockerfile,
                context: repo_info.workspace_root,
            });
        }
    }

    // Third: use default image
    Ok(ImageSource::UseDefault)
}

/// Build a Docker image if needed, or return the default image name.
///
/// Returns the image name to use for running the container.
pub fn prepare_image(worktree_path: &Path, image_name: &str) -> Result<String> {
    match find_image_source(worktree_path)? {
        ImageSource::BuildFrom { dockerfile, context } => {
            println!("Building from {}...", dockerfile.display());
            build_image_from(&dockerfile, &context, image_name)?;
            Ok(image_name.to_string())
        }
        ImageSource::UseDefault => {
            println!("Using default image: {}", git::DEFAULT_IMAGE);
            Ok(git::DEFAULT_IMAGE.to_string())
        }
    }
}

/// Build a Docker image from a specific Dockerfile.
fn build_image_from(dockerfile: &Path, context: &Path, image_name: &str) -> Result<()> {
    let status = Command::new("docker")
        .args([
            "build",
            "-t", image_name,
            "-f", dockerfile.to_str().unwrap(),
            context.to_str().unwrap(),
        ])
        .status()
        .context("Failed to run docker build")?;

    if !status.success() {
        bail!("Docker build failed");
    }

    Ok(())
}

/// Run a Docker container with Claude Code.
///
/// Mounts the worktree, copies Claude config, and launches an interactive session.
pub fn run_container(worktree_path: &Path, image_name: &str, prompt: Option<&str>) -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();

    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "-it".to_string(),
        "-v".to_string(),
        format!("{}:/workspace", worktree_path.display()),
        "-w".to_string(),
        "/workspace".to_string(),
        "-e".to_string(),
        format!("ANTHROPIC_API_KEY={}", api_key),
    ];

    // Build init script for container startup
    let mut init_script = String::from("set -e; ");

    // Mount and copy Claude config directory if it exists
    let claude_dir = PathBuf::from(&home).join(".claude");
    if claude_dir.exists() {
        args.extend([
            "-v".to_string(),
            format!("{}:/tmp/.claude-host:ro", claude_dir.display()),
        ]);
        init_script.push_str(
            "sudo cp -a /tmp/.claude-host ~/.claude && \
             sudo chown -R $(id -u):$(id -g) ~/.claude; \
             sed -i 's/\"installMethod\":[^,}]*/\"installMethod\":\"npm-global\"/g' ~/.claude/*.json 2>/dev/null || true; "
        );
    }

    // Mount and copy Claude config file if it exists
    let claude_json = PathBuf::from(&home).join(".claude.json");
    if claude_json.exists() {
        args.extend([
            "-v".to_string(),
            format!("{}:/tmp/.claude-host.json:ro", claude_json.display()),
        ]);
        init_script.push_str(
            "sudo cp /tmp/.claude-host.json ~/.claude.json && \
             sudo chown $(id -u):$(id -g) ~/.claude.json; \
             sed -i 's/\"installMethod\":[^,}]*/\"installMethod\":\"npm-global\"/g' ~/.claude.json 2>/dev/null || true; "
        );
    }

    // Setup Claude settings with pre-trusted /workspace directory
    init_script.push_str(
        r#"mkdir -p ~/.claude; cat > ~/.claude/settings.json << 'SETTINGS'
{
  "permissions": {
    "additionalDirectories": ["/workspace"],
    "allow": [
      "Bash",
      "Read",
      "Write",
      "Edit",
      "Glob",
      "Grep",
      "WebFetch(domain:*)",
      "WebSearch",
      "Task",
      "TodoWrite",
      "mcp__*"
    ],
    "deny": []
  }
}
SETTINGS
"#
    );

    // Add prompt via environment variable if provided
    if let Some(p) = prompt {
        args.extend(["-e".to_string(), format!("CLAUDE_PROMPT={}", p)]);
        init_script.push_str(r#"exec claude --permission-mode acceptEdits -p "$CLAUDE_PROMPT""#);
    } else {
        init_script.push_str("exec claude --permission-mode acceptEdits");
    }

    args.extend([
        image_name.to_string(),
        "bash".to_string(),
        "-c".to_string(),
        init_script,
    ]);

    let status = Command::new("docker")
        .args(&args)
        .status()
        .context("Failed to run docker container")?;

    if !status.success() {
        bail!("Docker container exited with error");
    }

    Ok(())
}

/// Run a Docker container with Claude Code and stream output with progress indicator.
///
/// Similar to `run_container` but captures and displays Claude's output in real-time
/// with a spinner to indicate activity. Used for non-interactive prompts.
pub fn run_container_with_output(
    worktree_path: &Path,
    image_name: &str,
    prompt: &str,
) -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();

    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "-v".to_string(),
        format!("{}:/workspace", worktree_path.display()),
        "-w".to_string(),
        "/workspace".to_string(),
        "-e".to_string(),
        format!("ANTHROPIC_API_KEY={}", api_key),
        "-e".to_string(),
        format!("CLAUDE_PROMPT={}", prompt),
    ];

    // Build init script for container startup
    let mut init_script = String::from("set -e; ");

    // Mount and copy Claude config directory if it exists
    let claude_dir = PathBuf::from(&home).join(".claude");
    if claude_dir.exists() {
        args.extend([
            "-v".to_string(),
            format!("{}:/tmp/.claude-host:ro", claude_dir.display()),
        ]);
        init_script.push_str(
            "sudo cp -a /tmp/.claude-host ~/.claude && \
             sudo chown -R $(id -u):$(id -g) ~/.claude; \
             sed -i 's/\"installMethod\":[^,}]*/\"installMethod\":\"npm-global\"/g' ~/.claude/*.json 2>/dev/null || true; ",
        );
    }

    // Mount and copy Claude config file if it exists
    let claude_json = PathBuf::from(&home).join(".claude.json");
    if claude_json.exists() {
        args.extend([
            "-v".to_string(),
            format!("{}:/tmp/.claude-host.json:ro", claude_json.display()),
        ]);
        init_script.push_str(
            "sudo cp /tmp/.claude-host.json ~/.claude.json && \
             sudo chown $(id -u):$(id -g) ~/.claude.json; \
             sed -i 's/\"installMethod\":[^,}]*/\"installMethod\":\"npm-global\"/g' ~/.claude.json 2>/dev/null || true; ",
        );
    }

    // Setup Claude settings with pre-trusted /workspace directory
    init_script.push_str(
        r#"mkdir -p ~/.claude; cat > ~/.claude/settings.json << 'SETTINGS'
{
  "permissions": {
    "additionalDirectories": ["/workspace"],
    "allow": [
      "Bash",
      "Read",
      "Write",
      "Edit",
      "Glob",
      "Grep",
      "WebFetch(domain:*)",
      "WebSearch",
      "Task",
      "TodoWrite",
      "mcp__*"
    ],
    "deny": []
  }
}
SETTINGS
"#,
    );

    // Run Claude with print mode, verbose, and stream-json output for progress display
    init_script.push_str(
        r#"exec claude --permission-mode acceptEdits --verbose --output-format stream-json -p "$CLAUDE_PROMPT""#,
    );

    args.extend([
        image_name.to_string(),
        "bash".to_string(),
        "-c".to_string(),
        init_script,
    ]);

    // Create spinner for progress indication
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.set_message("Claude is analyzing the project...");
    spinner.enable_steady_tick(Duration::from_millis(100));

    // Spawn docker process and capture output
    let mut child = Command::new("docker")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn docker container")?;

    // Read stdout in a separate thread - parse stream-json
    let stdout = child.stdout.take().expect("Failed to capture stdout");
    let stderr = child.stderr.take().expect("Failed to capture stderr");

    let spinner_clone = spinner.clone();
    let stdout_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                // Try to parse as Claude stream-json event
                match serde_json::from_str::<ClaudeEvent>(&line) {
                    Ok(event) => display_event(&event, &spinner_clone),
                    Err(_) => {
                        // Not JSON or unknown format, display as-is if non-empty
                        if !line.trim().is_empty() {
                            spinner_clone.suspend(|| {
                                println!("{}", line);
                            });
                        }
                    }
                }
            }
        }
    });

    let spinner_clone = spinner.clone();
    let stderr_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                // stderr is usually error messages or status, display as-is
                if !line.trim().is_empty() {
                    spinner_clone.suspend(|| {
                        eprintln!("\x1b[31m{}\x1b[0m", line);
                    });
                }
            }
        }
    });

    // Wait for output threads to finish
    stdout_thread.join().expect("stdout thread panicked");
    stderr_thread.join().expect("stderr thread panicked");

    // Wait for process to exit
    let status = child.wait().context("Failed to wait for docker container")?;

    spinner.finish_and_clear();

    if !status.success() {
        bail!("Docker container exited with error");
    }

    println!("\x1b[32mSetup complete!\x1b[0m");
    Ok(())
}
