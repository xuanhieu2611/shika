#!/bin/sh
# Print the path of the Sparkle distribution that release builds embed,
# downloading and verifying it on first use. The framework is not committed;
# the pinned version and checksum are. To upgrade Sparkle, change both, read
# the changelog, and replace assets/licenses/Sparkle-MIT.txt with its LICENSE.
set -eu
cd "$(dirname "$0")/.."

version=2.10.0
sha256=c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c

target_dir=${CARGO_TARGET_DIR:-target}
dir="$target_dir/sparkle/$version"
if [ ! -f "$dir/.verified" ]; then
    rm -rf "$dir"
    mkdir -p "$dir"
    archive="$dir/Sparkle-$version.tar.xz"
    curl -fsSL -o "$archive" \
        "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz"
    if [ "$(shasum -a 256 "$archive" | cut -d ' ' -f 1)" != "$sha256" ]; then
        echo "Sparkle $version does not match its pinned checksum." >&2
        rm -rf "$dir"
        exit 1
    fi
    tar -xJf "$archive" -C "$dir"
    rm "$archive"
    touch "$dir/.verified"
fi
if ! cmp -s "$dir/LICENSE" assets/licenses/Sparkle-MIT.txt; then
    echo "assets/licenses/Sparkle-MIT.txt does not match Sparkle $version's LICENSE." >&2
    exit 1
fi
printf '%s\n' "$dir"
