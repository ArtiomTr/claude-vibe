# Minimal Dockerfile for Claude Code
FROM node:22-slim

# Install essential tools
RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    curl \
    ca-certificates \
    sudo \
    && rm -rf /var/lib/apt/lists/*

# Install Claude Code globally
RUN npm install -g @anthropic-ai/claude-code

# Create user 'claude' and add to sudoers with passwordless sudo
RUN useradd -m -s /bin/bash claude && \
    usermod -aG sudo claude && \
    echo "claude ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/claude && \
    chmod 0440 /etc/sudoers.d/claude

# Set up working directory
WORKDIR /workspace
RUN chown claude:claude /workspace

# Switch to claude user
USER claude

# Default command
CMD ["claude"]
