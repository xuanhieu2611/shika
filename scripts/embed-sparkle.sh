#!/bin/sh
# Embed Sparkle.framework in a bundle from bundle-app.sh and sign its code
# inside out, as Sparkle documents for builds made without Xcode:
#   scripts/embed-sparkle.sh target/release/Shika.app "Developer ID Application"
# The app itself is signed afterwards, without --deep. Only release-app.sh
# embeds Sparkle, so local bundles never replace themselves with a release.
set -eu
cd "$(dirname "$0")/.."

app=$1
identity=$2
sparkle=$(./scripts/sparkle.sh)
framework="$app/Contents/Frameworks/Sparkle.framework"

rm -rf "$framework"
mkdir -p "$app/Contents/Frameworks"
ditto "$sparkle/Sparkle.framework" "$framework"
# The XPC services are for sandboxed apps. Shika is not sandboxed and does not
# set SUEnableInstallerLauncherService or SUEnableDownloaderService.
rm -rf "$framework/XPCServices" "$framework/Versions/B/XPCServices"
cp assets/licenses/Sparkle-MIT.txt "$app/Contents/Resources/licenses/"

sign() {
    codesign --force --options runtime --timestamp --sign "$identity" "$1"
}
sign "$framework/Versions/B/Autoupdate"
sign "$framework/Versions/B/Updater.app"
sign "$framework"
