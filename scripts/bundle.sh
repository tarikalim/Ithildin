#!/bin/sh
# Build release and install "/Applications/Ithildin.app".
# scripts/bundle.sh <binary> <out dir> packages a prebuilt binary without installing.
# Icon: assets/icon.svg is the source; scripts/render-icon.swift renders it, and AppIcon.icns and
# ithildin.png are made from that (iconutil, sips).
# The app's binary is the only one: Claude Code's MCP entry and the `ithildin` CLI (a symlink in
# ~/.local/bin) run it too, so one bundle updates all three.
set -e
cd "$(dirname "$0")/.."
bin="${1:-target/release/ithildin}"
app="${2:-/Applications}/Ithildin.app"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
[ -n "$1" ] || cargo build --release -j 4
# Updated in place, not deleted: deleting it dropped it from Spotlight until macOS indexed it again.
# The binary is moved over, not copied over, so a running `ithildin mcp` keeps its old file.
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/ithildin.new"
mv -f "$app/Contents/MacOS/ithildin.new" "$app/Contents/MacOS/ithildin"
cp assets/AppIcon.icns "$app/Contents/Resources/"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Ithildin</string>
  <key>CFBundleDisplayName</key><string>Ithildin</string>
  <key>CFBundleIdentifier</key><string>com.tarikalim.ithildin</string>
  <key>CFBundleExecutable</key><string>ithildin</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)</string>
  <key>CFBundleVersion</key><string>$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict></plist>
PLIST
codesign --force --deep -s - "$app"
# Updated in place, the bundle keeps its date and macOS keeps the cached icon; a new date refreshes it.
touch "$app"
[ -n "$1" ] || { "$lsregister" -f "$app"; mdimport "$app"; }
echo "installed: $app"
