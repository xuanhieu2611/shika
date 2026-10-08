#!/bin/sh
# Write target/release/appcast.xml, the Sparkle feed for the DMG that
# release-app.sh just built, signed with the EdDSA key in the login keychain:
#   scripts/appcast.sh release-notes.md
# The Markdown notes are embedded as the update's release notes. Upload the
# feed and the DMG to the same GitHub release; SUFeedURL redirects to
# the latest release's appcast.xml.
#
# SHIKA_DOWNLOAD_URL overrides the enclosure URL, for testing against a local
# server. It defaults to this version's DMG on GitHub.
set -eu
cd "$(dirname "$0")/.."

if [ $# -ne 1 ] || [ ! -f "$1" ]; then
    echo "Usage: scripts/appcast.sh release-notes.md" >&2
    exit 1
fi
notes=$1
repo=https://github.com/xuanhieu2611/shika
target_dir=${CARGO_TARGET_DIR:-target}
app="$target_dir/release/Shika.app"
dmg="$target_dir/release/Shika.dmg"
feed="$target_dir/release/appcast.xml"
plist="$app/Contents/Info.plist"
bin="$(./scripts/sparkle.sh)/bin"

if [ ! -d "$app/Contents/Frameworks/Sparkle.framework" ]; then
    echo "$app has no Sparkle. Build it with scripts/release-app.sh first." >&2
    exit 1
fi
# Stapling rewrites the DMG, so sign only the final, stapled bytes.
if ! xcrun stapler validate -q "$dmg"; then
    echo "$dmg is not stapled. Build it with scripts/release-app.sh first." >&2
    exit 1
fi
if grep -q ']]>' "$notes"; then
    echo "$notes contains ]]>, which would end the CDATA section." >&2
    exit 1
fi

read_plist() {
    /usr/libexec/PlistBuddy -c "Print $1" "$plist"
}
version=$(read_plist CFBundleShortVersionString)
build=$(read_plist CFBundleVersion)
minimum=$(read_plist LSMinimumSystemVersion)
feed_url=$(read_plist SUFeedURL)
url=${SHIKA_DOWNLOAD_URL:-$repo/releases/download/v$version/Shika.dmg}

# Sparkle offers an update only when sparkle:version, the CFBundleVersion,
# is higher than the installed one. Catch a forgotten bump before publishing.
if published=$(curl -fsSL --max-time 15 "$feed_url" 2>/dev/null); then
    previous=$(printf '%s\n' "$published" |
        sed -n 's:.*<sparkle\:version>\([0-9]*\)</sparkle\:version>.*:\1:p' | head -n 1)
    if [ -n "$previous" ] && [ "$build" -le "$previous" ]; then
        echo "CFBundleVersion $build is not above the published build $previous." >&2
        echo "Bump it in assets/macos/Info.plist and rebuild." >&2
        exit 1
    fi
else
    echo "No published feed at $feed_url, so there is no build number to compare."
fi

enclosure=$("$bin/sign_update" "$dmg")
pub_date=$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S +0000')

{
    cat <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
    <channel>
        <title>Shika</title>
        <link>$repo/releases</link>
        <language>en</language>
        <item>
            <title>Shika $version</title>
            <link>$repo/releases/tag/v$version</link>
            <sparkle:version>$build</sparkle:version>
            <sparkle:shortVersionString>$version</sparkle:shortVersionString>
            <sparkle:minimumSystemVersion>$minimum</sparkle:minimumSystemVersion>
            <sparkle:fullReleaseNotesLink>$repo/releases</sparkle:fullReleaseNotesLink>
            <pubDate>$pub_date</pubDate>
            <description sparkle:format="markdown"><![CDATA[
EOF
    cat "$notes"
    cat <<EOF
]]></description>
            <enclosure url="$url" type="application/octet-stream" $enclosure />
        </item>
    </channel>
</rss>
EOF
} >"$feed"

# SURequireSignedFeed makes Sparkle reject a feed without this signature.
"$bin/sign_update" "$feed"
"$bin/sign_update" --verify "$feed"
printf 'Shika %s (build %s) feed ready: %s\n' "$version" "$build" "$feed"
