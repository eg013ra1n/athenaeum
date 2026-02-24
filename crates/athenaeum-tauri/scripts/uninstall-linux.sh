#!/bin/bash
#
# Athenaeum Uninstaller for Linux
# Removes the application and all user data.
#

set -euo pipefail

APP_ID="com.vsharifov.athenaeum"
APP_NAME="athenaeum"

# System paths (deb/rpm install)
SYS_BIN="/usr/bin/$APP_NAME"
SYS_DESKTOP="/usr/share/applications/$APP_NAME.desktop"
SYS_ICONS="/usr/share/icons/hicolor"

# User data paths
USER_DATA="${XDG_DATA_HOME:-$HOME/.local/share}/$APP_ID"
USER_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/$APP_ID"
USER_CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/$APP_ID"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    echo "Athenaeum Uninstaller for Linux"
    echo ""
    echo "Removes the Athenaeum application and all associated data."
    echo ""
    echo "System files (requires sudo, installed via deb/rpm):"
    echo "  - $SYS_BIN"
    echo "  - $SYS_DESKTOP"
    echo "  - $SYS_ICONS/*/apps/$APP_NAME.png"
    echo ""
    echo "User data:"
    echo "  - $USER_DATA/"
    echo "  - $USER_CONFIG/"
    echo "  - $USER_CACHE/"
    echo ""
    echo "Usage: $0 [--help]"
    exit 0
fi

echo "=== Athenaeum Uninstaller for Linux ==="
echo ""

# Collect system files
sys_found=()
user_found=()

if [[ -f "$SYS_BIN" ]]; then
    size=$(du -sh "$SYS_BIN" 2>/dev/null | cut -f1)
    echo "  System binary:   $SYS_BIN ($size)"
    sys_found+=("$SYS_BIN")
fi

if [[ -f "$SYS_DESKTOP" ]]; then
    echo "  Desktop entry:   $SYS_DESKTOP"
    sys_found+=("$SYS_DESKTOP")
fi

# Find icon files
while IFS= read -r -d '' icon; do
    echo "  Icon:            $icon"
    sys_found+=("$icon")
done < <(find "$SYS_ICONS" -name "$APP_NAME.png" -print0 2>/dev/null || true)

if [[ -d "$USER_DATA" ]]; then
    size=$(du -sh "$USER_DATA" 2>/dev/null | cut -f1)
    echo "  User data:       $USER_DATA ($size)"
    user_found+=("$USER_DATA")
fi

if [[ -d "$USER_CONFIG" ]]; then
    size=$(du -sh "$USER_CONFIG" 2>/dev/null | cut -f1)
    echo "  User config:     $USER_CONFIG ($size)"
    user_found+=("$USER_CONFIG")
fi

if [[ -d "$USER_CACHE" ]]; then
    size=$(du -sh "$USER_CACHE" 2>/dev/null | cut -f1)
    echo "  User cache:      $USER_CACHE ($size)"
    user_found+=("$USER_CACHE")
fi

if [[ ${#sys_found[@]} -eq 0 && ${#user_found[@]} -eq 0 ]]; then
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

# Remove system files
if [[ ${#sys_found[@]} -gt 0 ]]; then
    if [[ $EUID -eq 0 ]]; then
        for path in "${sys_found[@]}"; do
            echo "Removing $path ..."
            rm -rf "$path"
        done
    else
        echo "System files found but not running as root."
        echo "To remove system files, re-run with: sudo $0"
        echo "Skipping system files for now."
        echo ""
    fi
fi

# Remove user data
if [[ ${#user_found[@]} -gt 0 ]]; then
    for path in "${user_found[@]}"; do
        echo "Removing $path ..."
        rm -rf "$path"
    done
fi

echo ""
echo "Athenaeum has been uninstalled."
