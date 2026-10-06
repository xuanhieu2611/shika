# Shika landing page

Static site with no build step. Page assets and fonts are local. The interactive prototype's generated runtime loads React 18.3.1, React DOM 18.3.1, and Babel Standalone 7.29.0 from unpkg, so the demo needs network access.

```sh
cd site
python3 -m http.server 8000
open http://localhost:8000
```

Serve over HTTP: the runtime fetches its own page. No real agents or Git commands run in the browser. All projects, terminal output, notifications, and shell commands are simulated.

## Files

- `index.html`, `styles.css`, `main.js`: landing page.
- `demo/`: interactive prototype derived from the archived `design/Shika v3.dc.html`. It offers Claude Code, Codex, Cursor CLI, and Pi, with a pinned agent tab and independent task-owned shell tabs. `support.js` is the retained generated runtime; app behavior lives in the inline component in `demo/index.html`, and overrides live in `demo/styles.css`. There is no runtime generator source in this repository.
- `img/`: logo, app icon, favicons, the native sample screenshot used as the loading poster, and `og.png` (2800x1760 native sample screenshot for link previews).
- `fonts/`: Geist and JetBrains Mono, both under the SIL Open Font License.
- `licenses/`, `notices.html`: website attribution. See the root `THIRD_PARTY_NOTICES.md` for the full inventory.

## Checking the prototype

Select a card, press Enter, and use `+` to add a shell. Try `git diff`, `git add .`, `git commit -m "Sample change"`, and `git push`; pushing keeps the card open. A second shell has its own input and output, and returning to another task restores that task's selected tab. Close warns about uncommitted work and unpushed commits; Push is available only when clean.

`j` / `k` move between cards, `n` opens the four-agent picker, `a` adds a sample project, and Ctrl+Q returns to cards. The prototype implements native tab shortcuts, but browsers may reserve Cmd+T, Cmd+W, Cmd+1, and Ctrl+Tab; use the visible tab controls in that case. Escape stays in the terminal and cancels open dialogs. `?theme=light` or `?theme=dark` sets its appearance.

## Deployment

The root `wrangler.jsonc` configures the Cloudflare Worker `shika` to serve `site/`, with an empty `previews` block required for PR previews. There is no build step. The existing Cloudflare custom domains serve production at https://useshika.com. The canonical, Open Graph, and Twitter URLs in `index.html` use that domain. Change them if it moves.

Cloudflare Workers Builds settings (Worker > Settings > Build):

- Root directory: repository root.
- Build command: empty.
- Production branch: `main`.
- Deploy command: `npx wrangler@4.147.0 deploy`.
- Preview command: `npx wrangler@4.147.0 preview`.
- Keep non-production branch builds enabled for PR previews.
- Build watch paths: include `site/*` and `wrangler.jsonc`; remove the default `*` include. Leave excludes empty. This skips desktop-only changes, including those in `crates/`.

From the repository root, validate without uploading:

```sh
npx wrangler@4.147.0 deploy --dry-run
```

After `npx wrangler@4.147.0 login`, `npx wrangler@4.147.0 preview` creates a branch preview; `npx wrangler@4.147.0 deploy` updates production. Neither is needed just to edit the site. Wrangler OAuth supports deployment but does not grant Workers Builds log/settings access; those require the dashboard or a separately scoped Workers CI API token. Older branches without `wrangler.jsonc` must incorporate it before their previews can succeed.
