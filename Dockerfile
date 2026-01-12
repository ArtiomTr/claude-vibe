# Minimal Dockerfile for Claude Code
FROM debian:bookworm-slim

ARG USER_ID=1000
ARG GROUP_ID=1000

# Install essential tools
RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    curl \
    ca-certificates \
    sudo \
    && rm -rf /var/lib/apt/lists/*

# Create user 'claude' with host UID/GID and add to sudoers with passwordless sudo
RUN groupadd -g $GROUP_ID claude && \
    useradd -m -s /bin/bash -u $USER_ID -g $GROUP_ID claude && \
    usermod -aG sudo claude && \
    echo "claude ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/claude && \
    chmod 0440 /etc/sudoers.d/claude

# Set up working directory
WORKDIR /workspace
RUN chown claude:claude /workspace

# Switch to claude user before installing Claude Code
USER claude

# Install Claude Code natively as claude user
RUN curl -fsSL https://claude.ai/install.sh | bash

# Add Claude to PATH
ENV PATH="/home/claude/.local/bin:$PATH"

# Default command
CMD ["claude"]
