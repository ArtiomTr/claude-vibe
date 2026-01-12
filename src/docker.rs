//! Docker utility functions for building images and running containers.

use anyhow::{Context, Result, bail};
use nix::unistd::{Gid, Uid};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Get the current user's UID and GID
fn get_host_uid_gid() -> (u32, u32) {
    (Uid::current().as_raw(), Gid::current().as_raw())
}

use crate::git;

/// Maximum number of output lines to display
const MAX_OUTPUT_LINES: usize = 5;

/// Box drawing characters
const BOX_VERTICAL: &str = "│";
const BOX_CORNER_BOTTOM: &str = "╰";
const BOX_HORIZONTAL: &str = "─";

/// Spinner characters (braille pattern)
const SPINNER_CHARS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Get terminal width, defaulting to 80 if unavailable
fn get_terminal_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80)
}

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
    Result {
        result: String,
        cost_usd: Option<f64>,
    },
    /// System events - can have either a message string or a subtype (like "init")
    System {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        subtype: Option<String>,
    },
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
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    /// Tool results can have content as string or array of objects
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

/// Collected output line with its display content
struct OutputLine {
    /// The formatted content (without color codes for gradient application)
    content: String,
    /// Whether this is a tool use line (uses yellow) or text line (uses cyan)
    is_tool: bool,
}

/// State for streaming output display
struct StreamingDisplay {
    lines: Vec<OutputLine>,
    displayed_count: usize,
    spinner_idx: usize,
    header_printed: bool,
    final_result: Option<String>,
    finished: bool,
}

impl StreamingDisplay {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            displayed_count: 0,
            spinner_idx: 0,
            header_printed: false,
            final_result: None,
            finished: false,
        }
    }

    /// Advance spinner and redraw (only if not finished)
    fn tick(&mut self) {
        if !self.finished {
            self.spinner_idx = (self.spinner_idx + 1) % SPINNER_CHARS.len();
            self.redraw();
        }
    }

    /// Add a line and redraw the display
    fn add_line(&mut self, line: OutputLine) {
        if !self.finished {
            self.lines.push(line);
            self.redraw();
        }
    }

    /// Set the final result and redraw in finished state
    fn set_final_result(&mut self, result: String) {
        self.final_result = Some(result);
        self.finished = true;
        self.redraw();
    }

    /// Mark as finished without a result message
    fn finish(&mut self) {
        self.finished = true;
        self.redraw();
    }

    /// Truncate a string to fit within terminal width (accounting for prefix)
    fn truncate_to_width(s: &str, max_width: usize) -> String {
        if s.chars().count() <= max_width {
            s.to_string()
        } else {
            let truncated: String = s.chars().take(max_width.saturating_sub(3)).collect();
            format!("{}...", truncated)
        }
    }

    /// Redraw the header, visible lines, and closing line
    fn redraw(&mut self) {
        let width = get_terminal_width();
        // Reserve space for "│ " prefix (2 chars) + some margin
        let content_width = width.saturating_sub(4);

        // Calculate how many lines to move up (header + output lines + closing line)
        let lines_to_clear = if self.header_printed {
            1 + self.displayed_count + 1 // header + output lines + closing line
        } else {
            0
        };

        // Move cursor up and clear lines
        for _ in 0..lines_to_clear {
            print!("\x1b[A\x1b[2K"); // Move up, clear line
        }

        if self.finished {
            // Finished state: checkmark + collapsed view
            println!("\x1b[32m✓ Claude analyzed your project\x1b[0m");
            self.header_printed = true;

            // Show final result if available
            if let Some(ref result) = self.final_result {
                // Truncate result to single line if needed
                let display_result = Self::truncate_to_width(result, content_width);
                println!(
                    "\x1b[90m{}\x1b[0m \x1b[36m{}\x1b[0m",
                    BOX_VERTICAL, display_result
                );
                self.displayed_count = 1;
            } else {
                self.displayed_count = 0;
            }

            // Print closing line
            let padding: String = BOX_HORIZONTAL.repeat(width.saturating_sub(1));
            println!("\x1b[90m{}{}\x1b[0m", BOX_CORNER_BOTTOM, padding);
        } else {
            // Active state: spinner + streaming lines
            let spinner_char = SPINNER_CHARS[self.spinner_idx];

            println!(
                "\x1b[36m{} Claude is analyzing your project...\x1b[0m",
                spinner_char
            );
            self.header_printed = true;

            // Print visible output lines
            let total = self.lines.len();
            let start = total.saturating_sub(MAX_OUTPUT_LINES);
            let visible_lines = &self.lines[start..];

            for (i, line) in visible_lines.iter().enumerate() {
                let gradient_intensity = if visible_lines.len() > 2 {
                    match i {
                        0 => 2, // Darkest (first line)
                        1 => 1, // Medium dark (second line)
                        _ => 0, // Normal (rest)
                    }
                } else {
                    0 // No gradient if 2 or fewer lines
                };

                let (prefix_color, text_color) = match gradient_intensity {
                    2 => ("\x1b[38;5;238m", "\x1b[38;5;240m"), // Very dark gray
                    1 => ("\x1b[38;5;243m", "\x1b[38;5;245m"), // Medium gray
                    _ => {
                        if line.is_tool {
                            ("\x1b[90m", "\x1b[33m") // Normal: gray pipe, yellow text for tools
                        } else {
                            ("\x1b[90m", "\x1b[36m") // Normal: gray pipe, cyan text for messages
                        }
                    }
                };

                // Truncate content to fit terminal width
                let truncated_content = Self::truncate_to_width(&line.content, content_width);

                println!(
                    "{}{}\x1b[0m {}{}\x1b[0m",
                    prefix_color, BOX_VERTICAL, text_color, truncated_content
                );
            }

            // Print closing line
            let padding: String = BOX_HORIZONTAL.repeat(width.saturating_sub(1));
            println!("\x1b[90m{}{}\x1b[0m", BOX_CORNER_BOTTOM, padding);

            self.displayed_count = visible_lines.len();
        }

        // Flush to ensure output is displayed immediately
        let _ = std::io::stdout().flush();
    }
}

/// Print the closing box line padded to terminal width
fn print_closing_line() {
    let width = get_terminal_width();
    // BOX_CORNER_BOTTOM is 3 bytes but 1 char, BOX_HORIZONTAL is 3 bytes but 1 char
    let padding_count = width.saturating_sub(1); // -1 for the corner
    let padding: String = BOX_HORIZONTAL.repeat(padding_count);
    println!("\x1b[90m{}{}\x1b[0m", BOX_CORNER_BOTTOM, padding);
}

/// Reset terminal colors (used for cleanup on Ctrl+C)
fn reset_terminal() {
    print!("\x1b[0m");
    let _ = std::io::stdout().flush();
}

/// Process a Claude event: collect lines, handle result, return cost if present
fn process_event(event: &ClaudeEvent, display: &Mutex<StreamingDisplay>) -> Option<f64> {
    match event {
        ClaudeEvent::Assistant { message } => {
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        display.lock().unwrap().add_line(OutputLine {
                            content: text.clone(),
                            is_tool: false,
                        });
                    }
                    ContentBlock::ToolUse { name, input } => {
                        let input_summary = summarize_tool_input(name, input);
                        display.lock().unwrap().add_line(OutputLine {
                            content: format!("> {} {}", name, input_summary),
                            is_tool: true,
                        });
                    }
                    _ => {}
                }
            }
            None
        }
        ClaudeEvent::Result { result, cost_usd } => {
            // Set the final result and collapse the display
            let final_text = result.trim().to_string();
            if !final_text.is_empty() {
                display.lock().unwrap().set_final_result(final_text);
            } else {
                display.lock().unwrap().finish();
            }
            *cost_usd
        }
        ClaudeEvent::System { subtype, .. } => {
            // Skip system init events (they're internal setup messages)
            if subtype.as_deref() == Some("init") {
                return None;
            }
            // Skip other system messages for now (they're usually internal)
            None
        }
        _ => None,
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
    BuildFrom {
        dockerfile: PathBuf,
        context: PathBuf,
    },
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
        ImageSource::BuildFrom {
            dockerfile,
            context,
        } => {
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
    let (uid, gid) = get_host_uid_gid();
    let status = Command::new("docker")
        .args([
            "build",
            "-t",
            image_name,
            "--build-arg",
            &format!("USER_ID={}", uid),
            "--build-arg",
            &format!("GROUP_ID={}", gid),
            "-f",
            dockerfile.to_str().unwrap(),
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
            "sudo rm -rf ~/.claude && sudo cp -a /tmp/.claude-host ~/.claude; \
             sudo chown -R claude:claude ~/.claude; \
             sed -i 's/\"installMethod\":[^,}]*/\"installMethod\":\"native\"/g' ~/.claude/*.json 2>/dev/null || true; ",
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
            "sudo cp /tmp/.claude-host.json ~/.claude.json; \
             sudo chown claude:claude ~/.claude.json; \
             sed -i 's/\"installMethod\":[^,}]*/\"installMethod\":\"native\"/g' ~/.claude.json 2>/dev/null || true; ",
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

    // Add prompt via environment variable if provided
    if let Some(p) = prompt {
        args.extend(["-e".to_string(), format!("CLAUDE_PROMPT={}", p)]);
        init_script
            .push_str(r#"exec claude --permission-mode bypassPermissions -p "$CLAUDE_PROMPT""#);
    } else {
        init_script.push_str("exec claude --permission-mode bypassPermissions");
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
            "sudo rm -rf ~/.claude && sudo cp -a /tmp/.claude-host ~/.claude; \
             sudo chown -R claude:claude ~/.claude; \
             sed -i 's/\"installMethod\":[^,}]*/\"installMethod\":\"native\"/g' ~/.claude/*.json 2>/dev/null || true; ",
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
            "sudo cp /tmp/.claude-host.json ~/.claude.json; \
             sudo chown claude:claude ~/.claude.json; \
             sed -i 's/\"installMethod\":[^,}]*/\"installMethod\":\"native\"/g' ~/.claude.json 2>/dev/null || true; ",
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

    // Set up Ctrl+C handler to clean up terminal
    let _ = ctrlc::set_handler(move || {
        reset_terminal();
        println!(); // New line after any partial output
        print_closing_line();
        std::process::exit(130); // Standard exit code for Ctrl+C
    });

    // Streaming display state and cost
    let display = Arc::new(Mutex::new(StreamingDisplay::new()));
    let cost_usd = Arc::new(Mutex::new(None::<f64>));
    let spinner_running = Arc::new(AtomicBool::new(true));

    // Start spinner thread
    let display_spinner = Arc::clone(&display);
    let spinner_flag = Arc::clone(&spinner_running);
    let spinner_thread = std::thread::spawn(move || {
        // Initial draw
        display_spinner.lock().unwrap().redraw();

        while spinner_flag.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(80));
            if spinner_flag.load(Ordering::SeqCst) {
                display_spinner.lock().unwrap().tick();
            }
        }
    });

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

    let display_clone = Arc::clone(&display);
    let cost_clone = Arc::clone(&cost_usd);
    let stdout_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                // Try to parse as Claude stream-json event
                if let Ok(event) = serde_json::from_str::<ClaudeEvent>(&line) {
                    if let Some(cost) = process_event(&event, &display_clone) {
                        *cost_clone.lock().unwrap() = Some(cost);
                    }
                }
                // Silently ignore unparseable JSON lines (internal Claude messages)
            }
        }
    });

    let stderr_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                // stderr is usually error messages or status, display as-is
                if !line.trim().is_empty() {
                    eprintln!("\x1b[31m{}\x1b[0m", line);
                }
            }
        }
    });

    // Wait for output threads to finish
    stdout_thread.join().expect("stdout thread panicked");
    stderr_thread.join().expect("stderr thread panicked");

    // Stop spinner thread
    spinner_running.store(false, Ordering::SeqCst);
    spinner_thread.join().expect("spinner thread panicked");

    // Wait for process to exit
    let status = child
        .wait()
        .context("Failed to wait for docker container")?;

    // Ensure terminal is reset
    reset_terminal();

    // Display cost if available
    if let Some(cost) = *cost_usd.lock().unwrap() {
        println!("\x1b[90m  Cost: ${:.4}\x1b[0m", cost);
    }

    if !status.success() {
        bail!("Docker container exited with error");
    }

    println!("\x1b[32mSetup complete!\x1b[0m");
    Ok(())
}
