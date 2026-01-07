# Minimal Dockerfile for Claude Code
FROM node:22-slim

# Install essential tools
RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install Claude Code globally
RUN npm install -g @anthropic-ai/claude-code

# Set up working directory
WORKDIR /workspace

# Default command
CMD ["claude"]
