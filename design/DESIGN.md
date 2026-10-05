# Shika design system

Shika is a keyboard-first Mac app for running a few coding agents at once. It is named for Shikamaru: the one who keeps track of everyone on the field while you give directions. Shika creates a git worktree per task, starts the CLI the user already pays for (Claude Code, Codex, Cursor CLI, Pi, Kiro) in a real PTY, tells them when an agent is asking or done, and deletes the worktree when the task is pushed or closed.

The user is a developer who does not watch agents work. They start a task, go to the browser, and come back on a notification. The UI is built around that: a wide column for tracking agents, a smaller terminal for the moments they need to read or answer.

**Sources**
- `design/Shika v3.dc.html`: the visual reference. Light and dark (`t` key or the theme tweak) and the logo.
- Logo artwork: `design/assets/shika-logo.png`.
- The values that ship are in the app, listed below. This folder is not imported.

---

## Index

`design/` is the written spec and the picture. The CSS token files, JSX components, ui kit, guidelines, loader, and skill are not here. The app is the copy that runs.

- `design/DESIGN.md`: color, type, spacing, layout, focus, motion, copy, and icons.
- `design/Shika v3.dc.html`: the visual reference. It still names the old CSS variables. Those names are the vocabulary in this file. They are not a stylesheet the app loads.
- `design/assets/`: `shika-logo.png` (transparent mark) and `shika-app-icon.png` (1024 tile). The running app uses `assets/macos/Shika.icns` and `assets/macos/shika-app-icon-256.png`. JetBrains Mono is `assets/fonts/`.

**Chrome.** `crates/shika/src/main.rs` draws the window: title bar, project headers, cards, empty projects, the CLI picker, the close dialog, settings, the toast, and `button`. Status words are `Status::label` in `crates/shika/src/model.rs`. Colors, radii, and type sizes are the values in `main.rs`.

**Terminal.** `Palette::shika` and `Palette::shika_dark` in `crates/shika-terminal/src/theme.rs` are the terminal colors, including the ANSI 16. Font, size, line height, and padding are `TerminalConfig` in `crates/shika-terminal/src/view.rs`.

**Translucency.** Opacity, blur, and whether it covers the sidebar alone or the sidebar and terminal: `crates/shika/src/appearance.rs`.

---

## Content fundamentals

Shika talks like a terse colleague. It reports facts and names consequences. It does not cheer, apologize, or explain itself.

- **Sentence case everywhere.** Buttons, titles, statuses: "New agent", "Ready to check", "Delete worktree". The only all-caps text is "SHIKA" in the notification header, matching macOS.
- **Status words are fixed:** Asking you, Ready to check, Working, Waiting. Chips use the short forms: asking, ready, working, waiting. Never invent synonyms ("Needs input", "Done").
- **Address the user as "you", rarely.** "Asking you", "next that needs you". Shika never says "I" or "we".
- **Buttons are verbs naming the outcome:** "Delete worktree", not "OK" or "Yes". Cancel is "Cancel".
- **Confirm dialogs ask a question about the specific task**, then list the facts: "Close “Fix flaky resize test”?" / "The worktree has 2 uncommitted files: src/cli.rs, src/layout.rs." / "This stops the session and force-removes .worktrees/fix-flaky-resize-test."
- **Toasts are past tense, one sentence:** "Pushed fix-flaky-resize-test. Card and worktree removed."
- **Errors state the cause plainly:** "kiro not found on PATH". No "Oops".
- **Use real developer nouns:** branch, worktree, PATH, push. Branch names and paths are always in mono.
- **Key hints are lowercase verbs:** move, new, terminal, shell, close.
- **No emoji. No exclamation marks. No marketing copy inside the app.**
- Card titles are the user's own prompt, shortened. Before the prompt is sent the card reads "New Claude Code" (or whichever CLI).

---

## Visual foundations

Names such as `--ink-1` and `--surface-app` are how this document talks about color, type, and space. The app implements them as GPUI values in the files in the Index. Do not add a CSS file beside them.

**Overall feel.** A quiet Mac app in the spirit of Obsidian and the Codex app: warm neutrals, system font, hairlines instead of boxes, and almost no color. The agent column is light (or warm charcoal in dark mode). The terminal is always dark. The only hues are the four status colors, so anything colored on screen means "look here".

**Color.**
- Neutrals carry a faint sage tint taken from the logo (hue ~120, very low chroma). Light: ink `#262824` on column `#F1F2EC`, raised `#FFFFFF`. Dark: ink `#E8EBE3` on column `#1A1C19`, raised `#272A25`.
- Brand tokens (`--brand-ink`, `--brand-sage`, `--brand-cream`, `--brand-mint`, `--brand-tile`) come from the logo and are for the logo, icon and wordmark only, never UI chrome.
- Status is the only color: amber = asking you, green = ready to check, blue = working, hollow grey ring = waiting. Each has four tokens: `dot`, `text` (meets contrast on the column), `chip` (summary pills), `tint` (resting background for asking and ready cards only).
- There is **no brand accent**. Selection and focus use ink (`--focus-ring`), not blue. Primary buttons are ink-filled.
- The terminal uses its own dark palette in both themes (`--term-*`) and a soft ANSI 16 (`--ansi-*`). In dark mode the terminal is a step darker than the column (`#10120F` vs `#1A1C19`).

**Type.**
- Two families. UI: the macOS system font (SF Pro) via `-apple-system`. Code, paths, branches, timers, key caps and the terminal: JetBrains Mono.
- Exact sizes, not a grid: 15 / 14 / 13.5 / 13 / 12.5 / 12 / 11.5 / 11 / 10.5. Weights 400, 500, 600 only.
- Timers use `tabular-nums` so they don't jitter.
- Anything a developer might copy (branch, path, command) is mono.

**Layout.**
- Fixed split, no drag handle. Agent column 540px by default (400-760 setting), terminal takes the rest. One terminal on screen, ever.
- Column: 48px titlebar (traffic lights + New agent), summary (headline + status chips + "a next that needs you"), project groups with up to 3 cards each ("+ N more · j to reach"), footer (Add project + key hints).
- Projects stay visible even with no agents, shown as a dashed empty box.
- Cards inside a project are sorted by attention: asking, ready, working, waiting.

**Cards.** Radius 10, padding 12/14/11. Two lines: status dot + task + time-in-status; then CLI · status · branch (· diff stat when ready), with contextual key hints on the selected card. No agent output on cards. At rest: 45% white (3% white in dark) with a 1px hairline (`--shadow-card`). Asking and ready get a faint status tint. Selected: raised white with a 1.5px ink outline. Selected while the terminal has focus: hairline only, hints hidden.

**Borders and elevation.** Hairlines everywhere (`rgba(0,0,0,.045)` or `#DEDBD5`). Real drop shadows only on floating things: dialogs, picker, toast, notification. Buttons have a 1px shadow in light mode only.

**Corner radii.** 4 key caps · 6 icon buttons and segments · 8 buttons, list rows, toasts · 10 cards · 12 dialogs · 14 notifications · pill for status chips. Inner radii nest: segment 6 inside track 8 with 2px padding.

**Backgrounds.** Flat fills only. No gradients, images, textures or illustrations. Blur is used only on the macOS-style notification (`backdrop-filter: blur(24px)`), matching the OS.

**Motion.** Short and functional. Hover/press 120ms, overlays fade up 4px in 180ms, column width changes in 220ms, all on `cubic-bezier(.2,.8,.2,1)`. The only loops: the working dot pulses (1.6s) and the terminal cursor blinks (1.1s, steps). Nothing bounces. `prefers-reduced-motion` zeroes durations.

**Hover and press.** Hover adds a 4% ink wash (`--surface-hover`) or steps the surface lighter (`--surface-raised-hover`). Icon buttons darken their glyph from ink-3 to ink-1. There is no press scale. Cards have no hover; selection is the state that matters.

**Focus model.** Focus is either on the cards or in the terminal. Cards focused: selected card has the ink outline and key hints; terminal cursor is a hollow block. Terminal focused: card outline drops to a hairline; cursor is a solid blinking block; the terminal header reads "esc back to cards".

**Transparency.** Used only for resting cards (so the column color shows through), hover washes, the scrim behind overlays (32% ink light, 50% black dark) and the notification.

**Dark mode.** Follows macOS. Same names, different values. Status colors get lighter and the tints become low-chroma charcoal. `Palette::shika_dark` is the dark terminal background. The chrome in `main.rs` currently paints the light values; the dark chrome values are the ones in this file and in `design/Shika v3.dc.html`.

---

## Iconography

- **Set:** [Lucide](https://lucide.dev) (the same set Obsidian uses). The prototype draws those strokes. The app does not load Lucide. Its chrome glyph is the settings gear, `SETTINGS_ICON` in `crates/shika/src/main.rs`. New glyphs follow the same stroke and size.
- **Style:** 1.5px stroke, 16px standalone, 14px inside buttons, color inherits (ink-3 at rest, ink-1 on hover).
- **Used for:** Plus (new agent on a project), FolderPlus (add project), X (close, when there's no room for the word), GitBranch, Terminal, CircleAlert (CLI not on PATH). That is roughly the whole list.
- **Not used for:** status (status is a dot), decoration, card contents, section headers.
- **Unicode as glyphs:** `↵` for Enter in key caps, `·` as the meta separator, `❯` and `●` appear inside agent terminals (drawn by the CLIs, not by Shika).
- **No emoji.** No PNG icons. No hand-drawn SVG.
- **Logo:** a deer head (Shikamaru's deer) with a terminal node graph below it. Black, sage, cream, with a mint glow. Use `design/assets/shika-logo.png` as is: don't recolor it, outline it or put it on a busy background.
- **App icon:** `design/assets/shika-app-icon.png`, the logo on a `--brand-tile` rounded square. The bundle icon is `assets/macos/Shika.icns`. Notifications use `assets/macos/shika-app-icon-256.png` (36px). It does not appear in the window chrome. For the macOS `.icns`, redraw on Apple's 824px icon grid inside a 1024 canvas.
- **Wordmark:** "Shika" in SF Pro Semibold, tracking -0.02em, next to the icon.

---

## Rules

Follow this file for color, type, spacing, layout, focus, motion, copy, and icons.

- Reuse the colors, type sizes, radii, and shadows already in `crates/shika/src/main.rs` and `crates/shika-terminal/src/theme.rs`. Never hard-code a new color, font size, radius, or shadow. If no value fits, stop and ask. Do not invent one.
- Build new UI from the views in `crates/shika/src/main.rs` and `crates/shika-terminal/src/view.rs` before writing new ones. New views use those same values and the states rest, hover, selected, selected-dimmed, and disabled.
- Status colors (asking, ready, working, waiting) are the only hues in the chrome. No brand accent. Selection and focus use ink.
- Copy is sentence case. Status words stay fixed. Buttons are verbs that name the outcome. No emoji. No exclamation marks. The longer copy rules are under Content fundamentals.
- Every action with a shortcut shows its key. Typing in the terminal or a text field never triggers app shortcuts.
- Every UI change must work in both light and dark mode. Check both before finishing.
- Theme the terminal with `Palette` in `crates/shika-terminal/src/theme.rs`. `shika` is the terminal under light chrome. `shika_dark` is the same palette with the darker background. JetBrains Mono is already bundled under `assets/fonts/` (OFL).
- If you add a color, size, radius, or a new view, update this file in the same change.
