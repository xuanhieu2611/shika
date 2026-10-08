#!/bin/sh
# Build a Developer ID signed, notarized, stapled Shika.dmg for sharing.
#
# One-time setup on the release Mac:
#   1. A "Developer ID Application" certificate in the login keychain.
#   2. xcrun notarytool store-credentials shika-notary \
#        --apple-id <apple id> --team-id <team id>
#   3. The Sparkle EdDSA private key in the login keychain, imported with
#      generate_keys -f from the backup (see docs/updates.md).
#
# SHIKA_SIGN_IDENTITY overrides the signing identity and
# SHIKA_NOTARY_PROFILE the notarytool keychain profile.
set -eu
cd "$(dirname "$0")/.."

identity=${SHIKA_SIGN_IDENTITY:-Developer ID Application}
profile=${SHIKA_NOTARY_PROFILE:-shika-notary}

if [ "$(uname -m)" != arm64 ]; then
    echo "Build on an Apple Silicon Mac; this release is arm64 only." >&2
    exit 1
fi
# The bundled crate notices must match Cargo.lock.
./scripts/update-notices.sh --check
if ! security find-identity -v -p codesigning | grep -q "\"$identity"; then
    echo "No code signing identity matching \"$identity\" in the keychain." >&2
    exit 1
fi
# An app with a public key that matches no private key could never be updated.
sparkle=$(./scripts/sparkle.sh)
public_key=$(/usr/libexec/PlistBuddy -c "Print SUPublicEDKey" assets/macos/Info.plist)
if [ "$("$sparkle/bin/generate_keys" -p 2>/dev/null)" != "$public_key" ]; then
    echo "The Sparkle key in the keychain does not match SUPublicEDKey in Info.plist." >&2
    exit 1
fi

# bundle-app.sh assembles the bundle and ad hoc signs it; the Developer ID
# signature below replaces that one.
app=$(./scripts/bundle-app.sh | tail -n 1)
release_dir=$(dirname "$app")
version=$(/usr/libexec/PlistBuddy -c "Print CFBundleShortVersionString" "$app/Contents/Info.plist")
dmg="$release_dir/Shika.dmg"
staging="$release_dir/dmg-staging"
notary_result="$release_dir/notary-result.json"

echo "Signing Shika $version"
./scripts/embed-sparkle.sh "$app" "$identity"
codesign --force --options runtime --timestamp --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"

echo "Creating $dmg"
rm -rf "$staging" "$dmg"
mkdir -p "$staging"
ditto "$app" "$staging/Shika.app"
ln -s /Applications "$staging/Applications"
# Sparkle installs from this DMG too; it recommends APFS with lzfse.
hdiutil create -volname Shika -srcfolder "$staging" -ov -fs APFS -format ULFO "$dmg" >/dev/null
rm -rf "$staging"
codesign --force --timestamp --sign "$identity" "$dmg"

echo "Notarizing (usually 1 to 10 minutes)"
xcrun notarytool submit "$dmg" --keychain-profile "$profile" --wait \
    --output-format json >"$notary_result"
status=$(plutil -extract status raw -o - "$notary_result")
if [ "$status" != Accepted ]; then
    id=$(plutil -extract id raw -o - "$notary_result")
    echo "Notarization finished with status: $status" >&2
    xcrun notarytool log "$id" --keychain-profile "$profile" >&2
    exit 1
fi

xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"

printf 'Shika %s ready: %s\n' "$version" "$dmg"
