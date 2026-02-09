#!/usr/bin/env bash
# grove installer - downloads the correct binary for your platform
# Usage: ./install.sh [version]
#   version: Optional version tag (e.g., v0.1.0). Defaults to latest release.
#
# Options:
#   --self-test    Run self-tests to verify script functions work correctly
#   --dry-run      Show what would be done without actually installing

set -euo pipefail

REPO="bivory/grove"
INSTALL_DIR="${GROVE_INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="grove"
DRY_RUN="${DRY_RUN:-false}"
TMP_DIR=""  # Global for cleanup trap

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[grove]${NC} $1"; }
warn() { echo -e "${YELLOW}[grove]${NC} $1"; }
error() { echo -e "${RED}[grove]${NC} $1" >&2; }

# Detect OS
detect_os() {
    local os
    os="$(uname -s)"
    case "$os" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "darwin" ;;
        *)       error "Unsupported OS: $os"; exit 1 ;;
    esac
}

# Detect architecture
detect_arch() {
    local arch
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *)             error "Unsupported architecture: $arch"; exit 1 ;;
    esac
}

# Get the target triple for this platform
get_target() {
    local os arch
    os="$(detect_os)"
    arch="$(detect_arch)"

    case "$os-$arch" in
        linux-x86_64)   echo "x86_64-unknown-linux-gnu" ;;
        linux-aarch64)  echo "aarch64-unknown-linux-gnu" ;;
        darwin-x86_64)  echo "x86_64-apple-darwin" ;;
        darwin-aarch64) echo "aarch64-apple-darwin" ;;
        *)              error "Unsupported platform: $os-$arch"; exit 1 ;;
    esac
}

# Get latest release version from GitHub
get_latest_version() {
    local url="https://api.github.com/repos/$REPO/releases/latest"
    if command -v curl &> /dev/null; then
        curl -fsSL "$url" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/'
    elif command -v wget &> /dev/null; then
        wget -qO- "$url" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/'
    else
        error "Neither curl nor wget found. Please install one."
        exit 1
    fi
}

# Download a file
download() {
    local url="$1"
    local output="$2"

    if command -v curl &> /dev/null; then
        curl -fsSL "$url" -o "$output"
    elif command -v wget &> /dev/null; then
        wget -q "$url" -O "$output"
    else
        error "Neither curl nor wget found. Please install one."
        exit 1
    fi
}

# Verify checksum
verify_checksum() {
    local file="$1"
    local expected="$2"
    local actual

    if command -v sha256sum &> /dev/null; then
        actual="$(sha256sum "$file" | cut -d' ' -f1)"
    elif command -v shasum &> /dev/null; then
        actual="$(shasum -a 256 "$file" | cut -d' ' -f1)"
    else
        warn "No sha256sum or shasum found, skipping checksum verification"
        return 0
    fi

    if [ "$actual" != "$expected" ]; then
        error "Checksum mismatch!"
        error "  Expected: $expected"
        error "  Actual:   $actual"
        return 1
    fi

    info "Checksum verified"
}

# Main installation logic
main() {
    local version="${1:-}"
    local target
    local binary_url
    local checksum_url

    info "Detecting platform..."
    target="$(get_target)"
    info "Platform: $target"

    # Get version
    if [ -z "$version" ]; then
        info "Fetching latest version..."
        version="$(get_latest_version)"
        if [ -z "$version" ]; then
            error "Could not determine latest version"
            exit 1
        fi
    fi
    info "Version: $version"

    # Construct download URLs
    binary_url="https://github.com/$REPO/releases/download/$version/grove-$target"
    checksum_url="https://github.com/$REPO/releases/download/$version/grove-$target.sha256"

    # Create temp directory
    TMP_DIR="$(mktemp -d)"
    trap 'rm -rf "$TMP_DIR"' EXIT TERM INT

    # Download binary
    info "Downloading grove..."
    download "$binary_url" "$TMP_DIR/$BINARY_NAME"

    # Download and verify checksum
    info "Verifying checksum..."
    if download "$checksum_url" "$TMP_DIR/$BINARY_NAME.sha256" 2>/dev/null; then
        expected_checksum="$(cat "$TMP_DIR/$BINARY_NAME.sha256" | cut -d' ' -f1)"
        verify_checksum "$TMP_DIR/$BINARY_NAME" "$expected_checksum"
    else
        warn "Checksum file not found, skipping verification"
    fi

    # Dry run stops here
    if [ "$DRY_RUN" = "true" ]; then
        info "Dry run complete. Would install to: $INSTALL_DIR/$BINARY_NAME"
        info "Binary URL: $binary_url"
        info "Checksum URL: $checksum_url"
        return 0
    fi

    # Create install directory if needed
    mkdir -p "$INSTALL_DIR"

    # Install binary
    info "Installing to $INSTALL_DIR/$BINARY_NAME..."
    mv "$TMP_DIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
    chmod +x "$INSTALL_DIR/$BINARY_NAME"

    # Verify installation
    if "$INSTALL_DIR/$BINARY_NAME" --version &> /dev/null; then
        info "Successfully installed grove $version"
    else
        warn "Binary installed but version check failed"
    fi

    # Check if install dir is in PATH
    if ! echo "$PATH" | tr ':' '\n' | grep -q "^$INSTALL_DIR$"; then
        warn ""
        warn "NOTE: $INSTALL_DIR is not in your PATH"
        warn "Add it to your shell profile:"
        warn ""
        warn "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc"
        warn "  # or for zsh:"
        warn "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc"
        warn ""
    fi

    info "Installation complete!"
}

# Self-test mode - tests script functions without downloading
run_self_tests() {
    local passed=0
    local failed=0

    echo "Running install script self-tests..."
    echo ""

    # Test 1: detect_os returns valid value
    echo -n "Test: detect_os returns valid value... "
    local os
    os="$(detect_os)"
    if [[ "$os" == "linux" || "$os" == "darwin" ]]; then
        echo "PASS (got: $os)"
        passed=$((passed + 1))
    else
        echo "FAIL (got: $os)"
        failed=$((failed + 1))
    fi

    # Test 2: detect_arch returns valid value
    echo -n "Test: detect_arch returns valid value... "
    local arch
    arch="$(detect_arch)"
    if [[ "$arch" == "x86_64" || "$arch" == "aarch64" ]]; then
        echo "PASS (got: $arch)"
        passed=$((passed + 1))
    else
        echo "FAIL (got: $arch)"
        failed=$((failed + 1))
    fi

    # Test 3: get_target returns valid triple
    echo -n "Test: get_target returns valid triple... "
    local target
    target="$(get_target)"
    case "$target" in
        x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin)
            echo "PASS (got: $target)"
            passed=$((passed + 1))
            ;;
        *)
            echo "FAIL (got: $target)"
            failed=$((failed + 1))
            ;;
    esac

    # Test 4: verify_checksum works with valid checksum
    echo -n "Test: verify_checksum accepts valid checksum... "
    local tmp_file
    tmp_file="$(mktemp)"
    echo "test content" > "$tmp_file"
    local expected_hash
    if command -v sha256sum &> /dev/null; then
        expected_hash="$(sha256sum "$tmp_file" | cut -d' ' -f1)"
    elif command -v shasum &> /dev/null; then
        expected_hash="$(shasum -a 256 "$tmp_file" | cut -d' ' -f1)"
    else
        echo "SKIP (no sha256sum/shasum)"
        rm -f "$tmp_file"
        passed=$((passed + 1))  # Count as pass since we handle this gracefully
        tmp_file=""
    fi
    if [ -n "$tmp_file" ]; then
        if verify_checksum "$tmp_file" "$expected_hash" > /dev/null 2>&1; then
            echo "PASS"
            passed=$((passed + 1))
        else
            echo "FAIL"
            failed=$((failed + 1))
        fi
        rm -f "$tmp_file"
    fi

    # Test 5: verify_checksum rejects invalid checksum
    echo -n "Test: verify_checksum rejects invalid checksum... "
    tmp_file="$(mktemp)"
    echo "test content" > "$tmp_file"
    local bad_hash="0000000000000000000000000000000000000000000000000000000000000000"
    if ! verify_checksum "$tmp_file" "$bad_hash" > /dev/null 2>&1; then
        echo "PASS"
        passed=$((passed + 1))
    else
        echo "FAIL (should have rejected bad hash)"
        failed=$((failed + 1))
    fi
    rm -f "$tmp_file"

    # Test 6: curl or wget is available
    echo -n "Test: curl or wget is available... "
    if command -v curl &> /dev/null || command -v wget &> /dev/null; then
        echo "PASS"
        passed=$((passed + 1))
    else
        echo "FAIL (neither curl nor wget found)"
        failed=$((failed + 1))
    fi

    # Test 7: can create temp directory
    echo -n "Test: can create temp directory... "
    local test_tmp_dir
    test_tmp_dir="$(mktemp -d)"
    if [ -d "$test_tmp_dir" ]; then
        echo "PASS"
        passed=$((passed + 1))
        rm -rf "$test_tmp_dir"
    else
        echo "FAIL"
        failed=$((failed + 1))
    fi

    # Summary
    echo ""
    echo "================================"
    echo "Tests passed: $passed"
    echo "Tests failed: $failed"
    echo "================================"

    if [ "$failed" -gt 0 ]; then
        return 1
    fi
    return 0
}

# Parse arguments
case "${1:-}" in
    --self-test)
        run_self_tests
        exit $?
        ;;
    --dry-run)
        DRY_RUN=true
        shift
        main "$@"
        ;;
    *)
        main "$@"
        ;;
esac
