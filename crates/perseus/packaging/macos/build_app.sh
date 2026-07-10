#!/usr/bin/env bash
# Usage: build_app.sh <target-triple> <version>
# Builds perseus --features tray for <target-triple>, assembles Perseus.app,
# signs it (APPLE_SIGNING_IDENTITY or ad-hoc), and produces the dmg.
set -euo pipefail
TARGET="$1"; VERSION="$2"
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"   # repo root
PKG="$ROOT/crates/perseus/packaging"
OUT="$ROOT/target/$TARGET/release"
APP="$OUT/bundle/Perseus.app"

cargo build --release -p perseus --features tray --target "$TARGET"

rm -rf "$OUT/bundle"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
sed "s/__VERSION__/$VERSION/g" "$PKG/macos/Info.plist" > "$APP/Contents/Info.plist"
cp "$OUT/perseus" "$APP/Contents/MacOS/perseus"

# icns from the committed icon.png (sips + iconutil are stock macOS tools).
# iconutil reads only the canonical iconset names; 64px is intentionally absent.
ICONSET="$(mktemp -d)/perseus.iconset"; mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s "$PKG/icon.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s*2)) $((s*2)) "$PKG/icon.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/perseus.icns"

IDENTITY="${APPLE_SIGNING_IDENTITY:--}"
codesign --force --options runtime --sign "$IDENTITY" "$APP/Contents/MacOS/perseus"
codesign --force --options runtime --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict "$APP"

ARCH_LABEL=$([ "$TARGET" = "aarch64-apple-darwin" ] && echo arm64 || echo x64)
DMG="$OUT/bundle/perseus-${VERSION}-macos-${ARCH_LABEL}.dmg"
STAGE="$(mktemp -d)"; cp -R "$APP" "$STAGE/"; ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Perseus" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
echo "built: $DMG"
