#!/bin/sh
# Regenerate assets/licenses/RUST_CRATES.txt, the license notices of the Rust
# crates compiled into Shika.app, from Cargo.lock. --check exits non-zero when
# the committed file is out of date. Needs cargo-about:
#   cargo install --locked --features cli cargo-about
set -eu
cd "$(dirname "$0")/.."

notices=assets/licenses/RUST_CRATES.txt
if ! cargo about --version >/dev/null 2>&1; then
    echo "cargo-about is not installed: cargo install --locked --features cli cargo-about" >&2
    exit 1
fi

out=$(mktemp)
trap 'rm -f "$out"' EXIT
# --frozen reads license text from the fetched crate sources only, so the
# output does not depend on a network lookup.
cargo fetch --locked --quiet
cargo about generate --frozen --fail \
    -m crates/shika/Cargo.toml \
    -c assets/licenses/about.toml \
    -o "$out" \
    assets/licenses/rust-crates.hbs

if [ "${1:-}" = "--check" ]; then
    if ! cmp -s "$out" "$notices"; then
        echo "$notices is out of date. Run scripts/update-notices.sh and commit it." >&2
        exit 1
    fi
else
    cp "$out" "$notices"
    printf '%s\n' "$notices"
fi
