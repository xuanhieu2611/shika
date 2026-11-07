# Shika website

Static site with no build step and no third-party requests. Page assets, fonts, and the prototype's script are local.

```sh
cd site
python3 -m http.server 8000
open http://localhost:8000
```

No real agents or Git commands run in the browser. In the prototype, all projects, terminal output, notifications, and shell commands are simulated.

## Files

- `guide/`: the public how-to (`index.html`) and the story (`story.html`). `/guide/story` rewrites to the story page via `_redirects`. Update these when user-visible behavior changes. Contributor contracts stay in `docs/`.
- `index.html`, `styles.css`, `main.js`: landing page. Light and dark share one set of tokens in `styles.css`; the system appearance decides until the nav toggle saves a choice (`localStorage` key `shika-theme`). The hero is a native capture (`img/shika-window.webp`); Try it in your browser swaps in the prototype at `demo/?embed=1`, scaled from its 1400x880 window, and later theme changes reach it as a `shika-theme` message, so its state survives.
- `field.js`: the paintings, dithered in one ink. Each `canvas.field` redraws `data-src` with Atkinson dithering in `--field-ink` at `--field-alpha`, so the art stays behind the copy. `data-mode="scene"` is the hero landscape: its ground line stands on the demo's top edge (`data-anchor`, `data-ground`), so the deer peeks over the app window, and it thins out around the copy (`data-clear`). `data-mode="frame"` covers its canvas (the closing section). With a mouse, moving the pointer turns the dither back into the painting in color along its path, which fades over a few seconds. Each develops once from the bottom up; under reduced motion there is no animation and no trail. Attributes are documented at the top of the file.
- `demo/`: interactive prototype of the current app with sample data, hand-written with no framework or runtime: `index.html` (the page), `demo.css` (Shika Dark and Shika Light glass values from `crates/shika/src/appearance/shika_chrome.rs`), and `demo.js` (state, CLI-styled terminal output, keys, and rendering). `img/backdrop-*.webp` is the forest painting with Shika's Halftone effect, ported from `crates/shika/src/background.rs` and framed as the native capture is. The sample state mirrors that capture, so the swap from the hero image is seamless; keep them in step when you recapture. Standalone it sits on the painting as a desktop; `?embed=1` drops the desktop, `?theme=light|dark` sets the side, and `?glass=off` paints a solid window.
- `img/`: generated paintings: `forest-center.webp` (hero landscape and the app's glass background in the captures) and `forest-frame-2.webp` (closing section, paper cut out so it works on light and dark); logo, app icon, favicons; native captures with real CLIs on sample projects: `shika-window.webp` (the hero window, 2800x1760 with transparent corners), `cards-dark.webp` and `cards-light.webp` (the feature grid's column crop), `shika-hero.webp` (the README image, the window composited on the painting), and `og.jpg` (2400x1260 link preview). See Recapturing below.
- `fonts/`: Geist and JetBrains Mono, both under the SIL Open Font License.
- `_redirects`: sends `/appcast.xml`, the app's Sparkle feed, to the latest GitHub release's `appcast.xml`. Workers parses it and does not serve it. Every shipped build has the feed URL baked in, so keep the path; see `docs/updates.md`. It also rewrites `/guide/story` to `guide/story.html`.
- `licenses/`, `notices.html`: website attribution. See the root `THIRD_PARTY_NOTICES.md` for the full inventory.

## Checking the prototype

`j` / `k` move between cards, Enter focuses the terminal, Ctrl+Q returns to the cards, and Escape stays in the terminal. Answer the asking card with `1` to `3` or the arrows and Enter; it works, then turns Ready with a notification. Codex finishes about 25 seconds after load. Type into New Pi and press Enter to name and start it. Select the Lead to chat; it answers with sample text.

Cmd+N opens the picker (Space toggles Create worktree, Tab changes project), `+` or Cmd+T adds a shell with `git status`, `git diff`, `git add .`, `git commit -m "..."`, and `git push`, Cmd+Option+B opens the Changes panel, Cmd+B hides the column, and Cmd+Shift+P previews Create PR. Cmd+Shift+W closes a task: Discard is exact Cmd+D and Push is exact Cmd+Enter (only with a clean tree and unpushed commits), after 300ms. Browsers may reserve some chords (Cmd+T, Cmd+W, Cmd+D, Cmd+1); the visible controls do the same.

## Recapturing

Public images must not show personal paths, real project lists, credentials, or private conversations. The current captures came from a release bundle with an isolated `--data-dir`, sample repositories under `~/code` (so CLI banners show `~/code/...`), local bare remotes, Shika Dark, opacity 70, Apply to Sidebar, and `forest-center.webp` as the background image with Halftone at zoom 125% and focus 89% / 50%. Launch Shika with the parent Claude Code session's `CLAUDE_*` variables removed (or Claude Code saves no transcript and shows a warning), and `DISABLE_AUTOUPDATER=1`. Capture the window with `screencapture -l <window id>` (`-o` for no shadow), strip metadata, and export WebP. Pi's startup screen lists your personal skills and extensions, so do not capture a selected Pi card.

## Deployment

The root `wrangler.jsonc` configures the Cloudflare Worker `shika` to serve `site/`, with an empty `previews` block required for PR previews. There is no build step. The existing Cloudflare custom domains serve production at https://useshika.com. The canonical, Open Graph, and Twitter URLs in `index.html` use that domain. Change them if it moves.

Cloudflare Workers Builds settings (Worker > Settings > Build):

- Root directory: repository root.
- Build command: empty.
- Production branch: `main`.
- Deploy command: `npx wrangler@4.147.0 deploy`.
- Preview command: `npx wrangler@4.147.0 preview`.
- Keep non-production branch builds enabled for PR previews.
- Build watch paths: include `site/**` and `wrangler.jsonc`; remove the default `*` include. Leave excludes empty. `site/**` covers the guide under `site/guide/`. This skips desktop-only changes, including those in `crates/`.

From the repository root, validate without uploading:

```sh
npx wrangler@4.147.0 deploy --dry-run
```

After `npx wrangler@4.147.0 login`, `npx wrangler@4.147.0 preview` creates a branch preview; `npx wrangler@4.147.0 deploy` updates production. Neither is needed just to edit the site. Wrangler OAuth supports deployment but does not grant Workers Builds log/settings access; those require the dashboard or a separately scoped Workers CI API token. Older branches without `wrangler.jsonc` must incorporate it before their previews can succeed.
