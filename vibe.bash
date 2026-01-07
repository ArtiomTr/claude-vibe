#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKTREE_PREFIX="claude/"

# Get repository root (works for both regular and bare repos)
get_repo_root() {
    if [[ "$(git rev-parse --is-bare-repository 2>/dev/null)" == "true" ]]; then
        # For bare repos, git-dir is the repo root
        git rev-parse --git-dir 2>/dev/null | xargs -I{} realpath {}
    else
        # For regular repos, use show-toplevel
        git rev-parse --show-toplevel 2>/dev/null
    fi
}

# Generate random string for worktree name
generate_random_name() {
    local length=${1:-8}
    # Use dd to avoid SIGPIPE from head closing the pipe on tr
    dd if=/dev/urandom bs=256 count=1 2>/dev/null | tr -dc 'a-z0-9' | head -c "$length"
}

# Build docker image from appropriate Dockerfile
build_docker_image() {
    local worktree_path="$1"
    local image_name="$2"

    if [[ -f "$worktree_path/Dockerfile.vibes" ]]; then
        echo "Building from Dockerfile.vibes..."
        docker build -t "$image_name" -f "$worktree_path/Dockerfile.vibes" "$worktree_path"
    elif [[ -f "$SCRIPT_DIR/Dockerfile" ]]; then
        echo "Building from default Dockerfile..."
        docker build -t "$image_name" -f "$SCRIPT_DIR/Dockerfile" "$SCRIPT_DIR"
    else
        echo "Error: No Dockerfile found"
        exit 1
    fi
}

# Run docker container with Claude Code
run_container() {
    local worktree_path="$1"
    local image_name="$2"
    local extra_args="${3:-}"
    shift 3 || shift $#  # Remaining args passed to claude

    local mount_args=()
    mount_args+=(-v "$worktree_path:/workspace")

    # Mount Claude Code config as readonly to staging location (copy-on-write pattern)
    local init_script="set -e; "

    # Create non-root user (--dangerously-skip-permissions refuses to run as root)
    init_script+="useradd -m -s /bin/bash claude 2>/dev/null || true; "
    init_script+="chown -R claude:claude /workspace; "

    # Copy Claude config to user home first
    if [[ -d "$HOME/.claude" ]]; then
        mount_args+=(-v "$HOME/.claude:/tmp/.claude-host:ro")
        init_script+="cp -a /tmp/.claude-host /home/claude/.claude; "
        init_script+="chown -R claude:claude /home/claude/.claude; "
    fi
    if [[ -f "$HOME/.claude.json" ]]; then
        mount_args+=(-v "$HOME/.claude.json:/tmp/.claude-host.json:ro")
        init_script+="cp /tmp/.claude-host.json /home/claude/.claude.json; "
        init_script+="chown claude:claude /home/claude/.claude.json; "
    fi

    # Setup Claude user settings with additionalDirectories to pre-trust /workspace
    # (overwrites any copied settings.json)
    init_script+="mkdir -p /home/claude/.claude; "
    init_script+="cat > /home/claude/.claude/settings.json << 'SETTINGS'
{
  \"permissions\": {
    \"additionalDirectories\": [\"/workspace\"],
    \"allow\": [
      \"Bash\",
      \"Read\",
      \"Write\",
      \"Edit\",
      \"Glob\",
      \"Grep\",
      \"WebFetch(domain:*)\",
      \"WebSearch\",
      \"Task\",
      \"TodoWrite\",
      \"mcp__*\"
    ],
    \"deny\": []
  }
}
SETTINGS
"
    init_script+="chown claude:claude /home/claude/.claude/settings.json; "

    init_script+='exec su claude -c "cd /workspace && claude --permission-mode acceptEdits $*" -- "$@"'

    docker run --rm -it \
        "${mount_args[@]}" \
        -w /workspace \
        -e ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-}" \
        $extra_args \
        "$image_name" \
        bash -c "$init_script" -- "$@"
}

# Get the main branch name from remote
get_main_branch() {
    git remote show origin 2>/dev/null | grep 'HEAD branch' | awk '{print $NF}'
}

# Check if worktree is synced with remote
is_worktree_synced() {
    local worktree_path="$1"

    cd "$worktree_path"

    # Get current branch
    local branch
    branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null) || return 1

    # Check if branch exists on remote
    if ! git ls-remote --exit-code --heads origin "$branch" &>/dev/null; then
        return 1
    fi

    # Fetch latest
    git fetch origin "$branch" &>/dev/null || return 1

    # Compare local and remote
    local local_commit remote_commit
    local_commit=$(git rev-parse HEAD)
    remote_commit=$(git rev-parse "origin/$branch" 2>/dev/null) || return 1

    [[ "$local_commit" == "$remote_commit" ]]
}

# Command: new
cmd_new() {
    local random_name
    random_name=$(generate_random_name)
    local worktree_name="${WORKTREE_PREFIX}${random_name}"
    local repo_root
    repo_root=$(get_repo_root) || {
        echo "Error: Not in a git repository"
        exit 1
    }

    local worktree_path="$repo_root/../$worktree_name"
    local image_name="claude-vibe-${random_name}"

    echo "Creating new worktree: $worktree_name"
    git worktree add "$worktree_path" -b "$worktree_name"

    # Resolve absolute path
    worktree_path=$(cd "$worktree_path" && pwd)

    echo "Building Docker image..."
    build_docker_image "$worktree_path" "$image_name"

    echo "Starting Claude Code session..."
    run_container "$worktree_path" "$image_name"
}

# Command: continue
cmd_continue() {
    local worktree_name="$1"

    if [[ -z "$worktree_name" ]]; then
        echo "Error: Please specify a worktree name"
        echo "Usage: vibe.bash continue <worktree-name>"
        echo ""
        echo "Available worktrees:"
        git worktree list | grep "${WORKTREE_PREFIX}" || echo "  No claude worktrees found"
        exit 1
    fi

    local repo_root
    repo_root=$(get_repo_root) || {
        echo "Error: Not in a git repository"
        exit 1
    }

    # Find worktree path
    local worktree_path
    worktree_path=$(git worktree list --porcelain | grep -A2 "worktree.*${worktree_name}" | head -1 | sed 's/worktree //')

    if [[ -z "$worktree_path" ]] || [[ ! -d "$worktree_path" ]]; then
        echo "Error: Worktree '$worktree_name' not found"
        echo ""
        echo "Available worktrees:"
        git worktree list | grep "${WORKTREE_PREFIX}" || echo "  No claude worktrees found"
        exit 1
    fi

    local random_part
    random_part=$(basename "$worktree_path" | sed "s/^${WORKTREE_PREFIX}//")
    local image_name="claude-vibe-${random_part}"

    echo "Continuing session in: $worktree_path"

    echo "Building Docker image..."
    build_docker_image "$worktree_path" "$image_name"

    echo "Starting Claude Code session..."
    run_container "$worktree_path" "$image_name"
}

# Command: cleanup
cmd_cleanup() {
    local repo_root
    repo_root=$(get_repo_root) || {
        echo "Error: Not in a git repository"
        exit 1
    }

    echo "Checking worktrees for cleanup..."

    local cleaned=0
    while IFS= read -r line; do
        local worktree_path
        worktree_path=$(echo "$line" | awk '{print $1}')

        # Skip if not a claude worktree
        if [[ ! "$worktree_path" =~ ${WORKTREE_PREFIX} ]]; then
            continue
        fi

        echo "Checking: $worktree_path"

        if is_worktree_synced "$worktree_path"; then
            echo "  Synced with remote, removing..."
            local branch
            branch=$(cd "$worktree_path" && git rev-parse --abbrev-ref HEAD)
            git worktree remove "$worktree_path" --force
            git branch -D "$branch" 2>/dev/null || true
            ((cleaned++))
        else
            echo "  Not synced, keeping"
        fi
    done < <(git worktree list | grep "${WORKTREE_PREFIX}")

    echo ""
    echo "Cleaned up $cleaned worktree(s)"
}

# Command: setup
cmd_setup() {
    local repo_root
    repo_root=$(get_repo_root) || {
        echo "Error: Not in a git repository"
        exit 1
    }

    echo "Fetching from origin..."
    git fetch origin

    local main_branch
    main_branch=$(get_main_branch)

    if [[ -z "$main_branch" ]]; then
        echo "Error: Could not determine main branch"
        exit 1
    fi

    echo "Main branch: $main_branch"
    echo "Checking out $main_branch..."
    git checkout "$main_branch"
    git pull origin "$main_branch"

    local image_name="claude-vibe-setup"

    echo "Building Docker image..."
    if [[ -f "$repo_root/Dockerfile.vibes" ]]; then
        docker build -t "$image_name" -f "$repo_root/Dockerfile.vibes" "$repo_root"
    elif [[ -f "$SCRIPT_DIR/Dockerfile" ]]; then
        docker build -t "$image_name" -f "$SCRIPT_DIR/Dockerfile" "$SCRIPT_DIR"
    else
        echo "Error: No Dockerfile found"
        exit 1
    fi

    echo "Starting Claude Code for project setup..."
    run_container "$repo_root" "$image_name" "" \
        --prompt "Analyze this project and create a Dockerfile.vibes file that includes all necessary dependencies and tools for development. The Dockerfile should be based on a minimal image but include everything needed to build and run this project. Please examine the project structure, dependencies, and build system to determine the requirements."
}

# Command: help
cmd_help() {
    cat << 'EOF'
vibe.bash - Claude Code session manager with git worktrees

Usage: vibe.bash <command> [arguments]

Commands:
  new         Create a new session with a fresh git worktree
              - Creates worktree with random name (claude/<random>)
              - Builds Docker image from Dockerfile.vibes or default Dockerfile
              - Launches Claude Code in the container

  continue    Attach to an existing session
              Usage: vibe.bash continue <worktree-name>
              - Continues work in specified worktree
              - Rebuilds Docker image and launches Claude Code

  cleanup     Remove worktrees that are pushed and synced with remote
              - Checks each claude/* worktree
              - Removes only those fully synced with origin

  setup       Initialize Dockerfile.vibes for a project
              - Checks out main branch
              - Launches Claude Code with prompt to analyze project
              - Creates appropriate Dockerfile.vibes

  help        Display this help message

Environment:
  ANTHROPIC_API_KEY    API key for Claude (passed to container)

Examples:
  vibe.bash new                    # Start a new session
  vibe.bash continue claude/abc123 # Continue existing session
  vibe.bash cleanup                # Clean synced worktrees
  vibe.bash setup                  # Setup Dockerfile.vibes

EOF
}

# Main entry point
main() {
    local command="${1:-help}"
    shift || true

    case "$command" in
        new)
            cmd_new "$@"
            ;;
        continue)
            cmd_continue "${1:-}"
            ;;
        cleanup)
            cmd_cleanup "$@"
            ;;
        setup)
            cmd_setup "$@"
            ;;
        help|--help|-h)
            cmd_help
            ;;
        *)
            echo "Unknown command: $command"
            echo ""
            cmd_help
            exit 1
            ;;
    esac
}

main "$@"
