#!/bin/bash
# Grove installer for Claude Code plugin
#
# This script downloads and installs the grove binary for the current platform.
# It can be run standalone or as a plugin postinstall hook.
#
# Environment variables:
#   GROVE_VERSION     - Version to install (default: latest)
#   GROVE_INSTALL_DIR - Installation directory (default: ~/.local/bin)
#   GROVE_REPO        - GitHub repository (default: user/grove)

set -euo pipefail

VERSION="${GROVE_VERSION:-latest}"
INSTALL_DIR="${GROVE_INSTALL_DIR:-$HOME/.local/bin}"
REPO="${GROVE_REPO:-user/grove}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() {
    echo -e "${GREEN}[grove]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[grove]${NC} $1"
}

error() {
    echo -e "${RED}[grove]${NC} $1" >&2
}

# Detect platform
detect_platform() {
    local os arch

    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m)

    case "$os-$arch" in
        linux-x86_64)
            echo "grove-linux-x86_64"
            ;;
        linux-aarch64)
            echo "grove-linux-arm64"
            ;;
        darwin-x86_64)
            echo "grove-darwin-x86_64"
            ;;
        darwin-arm64)
            echo "grove-darwin-arm64"
            ;;
        *)
            error "Unsupported platform: $os-$arch"
            error "Grove supports: Linux (x86_64, arm64), macOS (x86_64, arm64)"
            exit 1
            ;;
    esac
}

# Check for required tools
check_dependencies() {
    local missing=()

    if ! command -v curl &> /dev/null; then
        missing+=("curl")
    fi

    if ! command -v tar &> /dev/null; then
        missing+=("tar")
    fi

    if [ ${#missing[@]} -ne 0 ]; then
        error "Missing required tools: ${missing[*]}"
        error "Please install them and try again."
        exit 1
    fi
}

# Create install directory if needed
ensure_install_dir() {
    if [ ! -d "$INSTALL_DIR" ]; then
        info "Creating install directory: $INSTALL_DIR"
        mkdir -p "$INSTALL_DIR"
    fi
}

# Download and install grove
install_grove() {
    local artifact url temp_dir

    artifact=$(detect_platform)

    if [ "$VERSION" = "latest" ]; then
        url="https://github.com/${REPO}/releases/latest/download/${artifact}.tar.gz"
    else
        url="https://github.com/${REPO}/releases/download/v${VERSION}/${artifact}.tar.gz"
    fi

    info "Downloading grove from $url..."

    temp_dir=$(mktemp -d)
    trap 'rm -rf "$temp_dir"' EXIT

    if ! curl -fsSL "$url" -o "$temp_dir/grove.tar.gz"; then
        error "Failed to download grove"
        error "URL: $url"
        exit 1
    fi

    info "Extracting..."
    tar -xzf "$temp_dir/grove.tar.gz" -C "$temp_dir"

    info "Installing to $INSTALL_DIR..."
    mv "$temp_dir/$artifact" "$INSTALL_DIR/grove"
    chmod +x "$INSTALL_DIR/grove"

    info "Grove installed successfully!"
}

# Verify installation
verify_installation() {
    if [ -x "$INSTALL_DIR/grove" ]; then
        local version
        version=$("$INSTALL_DIR/grove" --version 2>/dev/null || echo "unknown")
        info "Installed: $version"
    else
        error "Installation verification failed"
        exit 1
    fi
}

# Check if install directory is in PATH
check_path() {
    if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
        warn ""
        warn "Note: $INSTALL_DIR is not in your PATH"
        warn "Add this to your shell profile:"
        warn ""
        warn "  export PATH=\"\$PATH:$INSTALL_DIR\""
        warn ""
    fi
}

# Main
main() {
    info "Grove installer"
    info "Version: $VERSION"
    info "Install directory: $INSTALL_DIR"
    echo

    check_dependencies
    ensure_install_dir
    install_grove
    verify_installation
    check_path

    echo
    info "Done! Run 'grove --help' to get started."
}

main "$@"
