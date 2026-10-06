# Third-party notices

Shika's code is [MIT licensed](LICENSE). The following artwork and fonts retain their own copyright and license notices.

## Native app

| Material | Source | License notice |
| --- | --- | --- |
| Settings gear (`cog-6-tooth`, 24px solid), embedded in `crates/shika/src/main.rs` | [Heroicons](https://github.com/tailwindlabs/heroicons) | [MIT, Tailwind Labs, Inc.](assets/licenses/Heroicons-MIT.txt) |
| Branch icon (`git-branch-16`), embedded in `crates/shika/src/main.rs` | [Octicons](https://github.com/primer/octicons) | [MIT, GitHub Inc.](assets/licenses/Octicons-MIT.txt) |
| JetBrains Mono v2.304, bundled terminal font | [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono/tree/v2.304) | [SIL Open Font License 1.1](assets/fonts/OFL.txt) |

The Rust crates compiled into the app, including GPUI and `alacritty_terminal`, are listed with the full text of their licenses in [`assets/licenses/RUST_CRATES.txt`](assets/licenses/RUST_CRATES.txt). `scripts/update-notices.sh` regenerates it from `Cargo.lock` with [cargo-about](https://github.com/EmbarkStudios/cargo-about); run it after changing dependencies. `scripts/release-app.sh` refuses to build a release while the file is out of date.

`scripts/bundle-app.sh` includes the native notices in `Shika.app/Contents/Resources/licenses/`, with a native attribution index (`THIRD_PARTY_NOTICES.txt`) and Shika's license in `Contents/Resources/`.

## Website

| Material | Source | License notice |
| --- | --- | --- |
| Outline icons embedded in `site/index.html` | [Tabler Icons](https://github.com/tabler/tabler-icons) | [MIT, Paweł Kuna](site/licenses/Tabler-MIT.txt) |
| Geist, locally served font | [Geist](https://github.com/vercel/geist-font) | [SIL Open Font License 1.1](site/fonts/Geist-OFL.txt) |
| JetBrains Mono, locally served font | [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) | [SIL Open Font License 1.1](site/fonts/JetBrainsMono-OFL.txt) |

The browser demo's generated runtime loads React 18.3.1, React DOM 18.3.1, and Babel Standalone 7.29.0 from unpkg. Their distributed bundles retain upstream notices. The site also serves a [local notices page](site/notices.html).

When adding copied artwork or bundled dependencies, preserve the source's copyright and license notices. Shika's MIT license does not replace upstream notices.
