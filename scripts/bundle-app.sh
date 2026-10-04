#!/bin/sh
# Build locally, without signing or notarization for distribution.
set -eu
cd "$(dirname "$0")/.."
profile=release
if [ "${1:-}" = "--debug" ]; then
    profile=debug
    cargo build -p shika
else
    cargo build --release -p shika
fi
target_dir=${CARGO_TARGET_DIR:-target}
bundle="$target_dir/$profile/Shika.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources/fonts"
cp "$target_dir/$profile/shika" "$bundle/Contents/MacOS/Shika"
cp assets/macos/Info.plist "$bundle/Contents/Info.plist"
cp assets/macos/Shika.icns "$bundle/Contents/Resources/Shika.icns"
cp assets/fonts/*.ttf assets/fonts/OFL.txt "$bundle/Contents/Resources/fonts/"
# Stable ad hoc identity lets macOS attribute native notification permission
# to com.hieule.shika. No Developer ID or distribution signing is performed.
codesign --force --deep --sign - --identifier com.hieule.shika "$bundle"
printf '%s\n' "$bundle"
