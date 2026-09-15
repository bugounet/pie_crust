#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# Run on macOS with its SDK. Cargo builds for the host architecture by default.
if [[ "$(uname -s)" != "Darwin" ]]; then
  printf '%s\n' 'Run this packaging script on macOS.' >&2
  exit 1
fi
cargo build --release --locked -p pie_crust-desktop
bundle="dist/pie_crust.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp target/release/crusty "$bundle/Contents/MacOS/crusty"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>pie_crust</string>
  <key>CFBundleDisplayName</key><string>pie_crust</string>
  <key>CFBundleExecutable</key><string>crusty</string>
  <key>CFBundleIdentifier</key><string>dev.pie-crust.desktop</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict></plist>
PLIST
plutil -lint "$bundle/Contents/Info.plist"
printf 'Application built: %s\n' "$bundle"
