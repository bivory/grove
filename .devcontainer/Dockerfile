# Devcontainer image for grove
# Multi-stage build
FROM rust:1.92 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

# Build: docker build -t grove-devcontainer .devcontainer/
FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04

# Install system dependencies
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
        libssl-dev \
        git \
        jq \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Create workspace directory
RUN mkdir -p /workspaces/grove && chown vscode:vscode /workspaces/grove

# Switch to vscode user for tool installation
USER vscode
WORKDIR /home/vscode

# Set up cargo environment
ENV CARGO_HOME="/home/vscode/.cargo"
ENV RUSTUP_HOME="/home/vscode/.rustup"

ENV PATH="/home/vscode/.local/bin:/usr/local/bin:${PATH}"

# Install mise
RUN curl https://mise.run | sh \
    && echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc \
    && echo 'eval "$(mise activate bash)"' >> ~/.bashrc

# Copy mise.toml for tool installation
RUN mkdir -p ~/.config/mise
COPY --chown=vscode:vscode mise.toml /home/vscode/.config/mise/config.toml
COPY --chown=vscode:vscode mise.toml /workspaces/grove/mise.toml

# Install all tools via mise
RUN mise trust ~/.config/mise/config.toml \
    && cd /workspaces/grove \
    && mise trust \
    && mise install --verbose

# Set up PATH for all tools
ENV PATH="/home/vscode/.local/share/mise/shims:/home/vscode/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH}"

# Install rust-analyzer for LSP support
RUN rustup component add rust-analyzer

# Verify tools are installed
RUN cargo --version \
    && rustc --version \
    && rust-analyzer --version \
    && cargo-nextest --version \
    && typos --version \
    && cargo-machete --version \
    && cargo-audit --version \
    && cargo-deny --version

# Install Claude Code
RUN curl -fsSL https://claude.ai/install.sh | bash \
    && claude install stable

# Verify Claude Code
RUN claude --version

# Install tissue for issue tracking
RUN mkdir -p ~/.local/bin \
    && OS=$(uname -s | tr '[:upper:]' '[:lower:]') \
    && ARCH=$(uname -m) \
    && case "$OS" in darwin) OS="macos" ;; linux) OS="linux" ;; esac \
    && case "$ARCH" in x86_64|amd64) ARCH="x86_64" ;; aarch64|arm64) ARCH="aarch64" ;; esac \
    && VERSION="v26.1.2" \
    && curl -fsSL "https://github.com/evil-mind-evil-sword/tissue/releases/download/${VERSION}/tissue-${ARCH}-${OS}.tar.gz" -o /tmp/tissue.tar.gz \
    && tar -xzf /tmp/tissue.tar.gz -C /tmp \
    && mv /tmp/tissue-${ARCH}-${OS} ~/.local/bin/tissue \
    && chmod +x ~/.local/bin/tissue \
    && rm /tmp/tissue.tar.gz
RUN tissue version

# Force HTTPS for GitHub clones (no SSH keys in container)
RUN git config --global url."https://github.com/".insteadOf "git@github.com:"

RUN claude plugin marketplace add bivory/claude-plugin-marketplace
# Install roz
RUN curl -fsSL https://raw.githubusercontent.com/bivory/roz/main/.claude-plugin/install.sh | bash -s -- v0.1.4
RUN roz --version
RUN claude plugin install roz@bivory
RUN claude plugin list | grep -q roz

# Install grove -- Release version
#RUN curl -fsSL https://raw.githubusercontent.com/bivory/grove/main/.claude-plugin/install.sh | bash -s -- v0.4.0
# Install grove -- Local version (binary + plugin)
COPY --from=builder /app/target/release/grove /usr/local/bin/grove
RUN grove --version
RUN mkdir -p grove-plugin
COPY --chown=vscode:vscode .claude-plugin ./grove-plugin/.claude-plugin
COPY --chown=vscode:vscode agents ./grove-plugin/agents
COPY --chown=vscode:vscode hooks ./grove-plugin/hooks
COPY --chown=vscode:vscode rules ./grove-plugin/rules
COPY --chown=vscode:vscode skills ./grove-plugin/skills
RUN cat > ./grove-plugin/.claude-plugin/marketplace.json << 'EOF'
{
  "name": "local-market",
  "owner": {
    "name": "Bryan Ivory"
  },
  "plugins": [
    {
      "name": "grove",
      "source": "./",
      "description": "Compound learning gate for Claude Code"
    }
  ]
}
EOF
RUN claude plugin marketplace add ./grove-plugin
RUN claude plugin install grove@local-market
RUN claude plugin list | grep -q grove

# Install Total Recall for grove
RUN claude plugin marketplace add davegoldblatt/recall-marketplace
RUN claude plugin install recall@recall-marketplace
# Fails need to login
# RUN claude -m /recall:recall-init
# RUN claude -p "Initialize recall" --append-system-prompt "Follow the recall-init protocol"

# Install additional Claude Code plugins
RUN claude plugin marketplace add anthropics/claude-plugins-official
RUN claude plugin install code-simplifier && \
    claude plugin install rust-analyzer-lsp && \
    claude plugin install context7 && \
    claude plugin install serena

# Install mise-en-place plugin from community fork
# See: https://github.com/jdx/mise/discussions/6575
RUN claude plugin marketplace add richardthe3rd/mise#claude-plugin \
    && claude plugin install mise-en-place@mise-en-place

USER root

ENV CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1
