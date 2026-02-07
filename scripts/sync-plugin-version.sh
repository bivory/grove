#!/bin/bash
# sync-plugin-version.sh
#
# Synchronizes the version number from Cargo.toml to plugin.json.
# Called as a cargo-release pre-release hook.
#
# Usage:
#   ./scripts/sync-plugin-version.sh <version>
#
# Example:
#   ./scripts/sync-plugin-version.sh 0.2.0

set -euo pipefail

if [ $# -ne 1 ]; then
    echo "Usage: $0 <version>" >&2
    exit 1
fi

VERSION="$1"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
PLUGIN_JSON="$PROJECT_DIR/.claude-plugin/plugin.json"

# Check if jq is available
if ! command -v jq &> /dev/null; then
    echo "Error: jq is required but not installed" >&2
    exit 1
fi

# Check if plugin.json exists
if [ ! -f "$PLUGIN_JSON" ]; then
    echo "Error: $PLUGIN_JSON not found" >&2
    exit 1
fi

echo "Updating plugin.json version to $VERSION..."

# Update plugin.json
jq --arg v "$VERSION" '.version = $v' "$PLUGIN_JSON" > tmp.json
mv tmp.json "$PLUGIN_JSON"

# Update marketplace.json if it exists
MARKETPLACE_JSON="$PROJECT_DIR/.claude-plugin/marketplace.json"
if [ -f "$MARKETPLACE_JSON" ]; then
    echo "Updating marketplace.json version to $VERSION..."
    jq --arg v "$VERSION" '.version = $v' "$MARKETPLACE_JSON" > tmp.json
    mv tmp.json "$MARKETPLACE_JSON"
fi

# Stage the changes
git add "$PROJECT_DIR/.claude-plugin/"*.json

echo "Version sync complete!"
