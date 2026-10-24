#!/bin/sh
# Build locally, without signing or notarization for distribution.
#
#   --debug         build the debug profile into target/debug/Shika.app
#   --distribution  keep the bundle ID from assets/macos/Info.plist, for
#                   release-app.sh. Without it the bundle ID gets ".dev".
set -eu
cd "$(dirname "$0")/.."
profile=release
distribution=false
for arg in "$@"; do
    case "$arg" in
        --debug) profile=debug ;;
        --distribution) distribution=true ;;
        *)
            echo "usage: $0 [--debug] [--distribution]" >&2
            exit 2
            ;;
    esac
done
if [ "$profile" = debug ]; then
    cargo build -p shika
else
    cargo build --release -p shika
fi
target_dir=${CARGO_TARGET_DIR:-target}
bundle="$target_dir/$profile/Shika.app"
# Start clean, so nothing from an earlier build stays behind, such as the
# Sparkle.framework that release-app.sh embeds.
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources/fonts" "$bundle/Contents/Resources/licenses"
cp "$target_dir/$profile/shika" "$bundle/Contents/MacOS/Shika"
cp assets/macos/Info.plist "$bundle/Contents/Info.plist"
cp assets/macos/Shika.icns "$bundle/Contents/Resources/Shika.icns"
cp assets/fonts/*.ttf assets/fonts/OFL.txt "$bundle/Contents/Resources/fonts/"
cp LICENSE "$bundle/Contents/Resources/"
cp assets/licenses/APP_NOTICES.txt "$bundle/Contents/Resources/THIRD_PARTY_NOTICES.txt"
cp assets/licenses/Heroicons-MIT.txt assets/licenses/Octicons-MIT.txt assets/licenses/RUST_CRATES.txt \
    assets/licenses/Catppuccin-MIT.txt assets/licenses/RosePine-MIT.txt assets/licenses/TokyoNight-Apache-2.0.txt \
    assets/licenses/Dracula-MIT.txt assets/licenses/Gruvbox-MIT.txt assets/licenses/LobeIcons-MIT.txt \
    assets/licenses/Pi-MIT.txt "$bundle/Contents/Resources/licenses/"
cp assets/fonts/OFL.txt "$bundle/Contents/Resources/licenses/JetBrainsMono-OFL.txt"
# macOS keeps one Screen Recording and one Notifications entry per bundle ID,
# tied to the signature it last approved. An ad hoc signature changes on every
# build, so a local bundle with the release's ID would take that entry from
# /Applications/Shika.app and lose it again on the next build. Local bundles
# are com.hieule.shika.dev and ask for their own permissions; the release keeps
# com.hieule.shika. No Developer ID or distribution signing is performed.
identifier=$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$bundle/Contents/Info.plist")
if [ "$distribution" = false ]; then
    identifier="$identifier.dev"
    /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $identifier" "$bundle/Contents/Info.plist"
fi
codesign --force --deep --sign - --identifier "$identifier" "$bundle"
printf '%s\n' "$bundle"
