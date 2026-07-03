#!/bin/bash
#
# Athenaeum Uninstaller for macOS
# Removes the application and all user data.
#

set -euo pipefail

APP_ID="com.vsharifov.athenaeum"
APP_PATH="/Applications/Athenaeum.app"
DATA_DIR="$HOME/Library/Application Support/$APP_ID"
CACHE_DIR="$HOME/Library/Caches/$APP_ID"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    echo "Athenaeum Uninstaller for macOS"
    echo ""
    echo "Removes the Athenaeum application and all associated user data:"
    echo "  - $APP_PATH"
    echo "  - ~/Library/Application Support/$APP_ID/ (includes logs/, catalog DB)"
    echo "  - ~/Library/Caches/$APP_ID/"
    echo ""
    echo "Usage: $0 [--help]"
    exit 0
fi

echo "=== Athenaeum Uninstaller for macOS ==="
echo ""
echo "This will remove Athenaeum and all associated data."
echo ""

# Collect what exists
found=()

if [[ -d "$APP_PATH" ]]; then
    size=$(du -sh "$APP_PATH" 2>/dev/null | cut -f1)
    echo "  Application:  $APP_PATH ($size)"
    found+=("$APP_PATH")
fi

if [[ -d "$DATA_DIR" ]]; then
    size=$(du -sh "$DATA_DIR" 2>/dev/null | cut -f1)
    echo "  User data:    $DATA_DIR ($size, includes logs/)"
    found+=("$DATA_DIR")
fi

if [[ -d "$CACHE_DIR" ]]; then
    size=$(du -sh "$CACHE_DIR" 2>/dev/null | cut -f1)
    echo "  Cache:        $CACHE_DIR ($size)"
    found+=("$CACHE_DIR")
fi

if [[ ${#found[@]} -eq 0 ]]; then
    echo "Nothing to remove. Athenaeum does not appear to be installed."
    exit 0
fi

echo ""
read -r -p "Proceed with removal? [y/N] " confirm
if [[ ! "$confirm" =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 0
fi

echo ""

for path in "${found[@]}"; do
    echo "Removing $path ..."
    rm -rf "$path"
done

echo ""
echo "Athenaeum has been uninstalled."
