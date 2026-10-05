# Shika landing page

Static site. No build step, no dependencies. Every asset is local.

```sh
cd site
python3 -m http.server 8000
open http://localhost:8000
```

The demo must be served over HTTP, not opened as a file: its runtime fetches its own page.

## Files

- `index.html`, `styles.css`, `main.js`: the page.
- `demo/`: the designer's v3 prototype (`design/Shika v3.dc.html`), embedded in the hero at 1280x800 and scaled to fit. `support.js` is the prototype runtime, copied unchanged; it loads React and Babel from unpkg when the demo starts. Changes from the original: JetBrains Mono is served from `../fonts`, the app icon uses the new logo, `?theme=light|dark` picks the theme, the sample projects are made up (storefront, routekit, dotfiles), the sample cards use only Claude Code and Cursor CLI, and Codex, Pi and Kiro show as coming soon in the picker.
- `img/`: logo, app icon, favicons, `demo-poster.webp` (shown until the demo loads) and `og.png` (1200x630 share card).
- `fonts/`: Geist and JetBrains Mono, both under the SIL Open Font License.

## Deploying

The site is served at https://useshika.com from Cloudflare Pages, connected to this repo: production branch `main`, no build command, build output directory `site`, build watch paths `site/*`. A push to `main` that touches `site/` deploys, and each pull request gets a preview URL.

The canonical, Open Graph and Twitter URLs in the `<head>` of `index.html` point at that domain. Change them if it moves.
