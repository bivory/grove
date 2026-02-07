#!/bin/bash
# Lightweight setup for runtime configuration
set -e

echo "Configuring environment..."

cd /workspaces/grove

# Trust mise for this workspace
mise trust

# Fix volume directory permissions if needed
sudo chown -R vscode:vscode /workspaces/grove/target 2>/dev/null || true

# Initialize tissue if not already done
if [ ! -d .tissue ]; then
    echo "Initializing tissue issue tracking..."
    tissue init || true
fi

echo "Setup complete!"
echo "  Run: mise run test     # to run tests"
echo "  Run: mise run ci       # to run all CI checks"
