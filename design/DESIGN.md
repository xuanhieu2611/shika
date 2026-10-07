# Shika design system

Shika is a keyboard-first Mac app for running a few coding agents at once. It is named for Shikamaru: the one who keeps track of everyone on the field while you give directions. Shika creates a git worktree per task, starts the CLI the user already uses (Claude Code, Codex, Cursor CLI, Pi) in a real PTY, and notifies them when an agent needs attention or finishes a turn. Closing the task removes its worktree after the safe-close checks. A push keeps the task open. Kiro is planned.

The user is a developer who does not watch agents work. They start a task, go to the browser, and come back on a notification. The UI is built around that: a wide column for tracking agents, a smaller terminal for the moments they need to read or answer.

**Sources**
- `design/Shika v3.dc.html`: the archived visual reference. Its original runtime and styles are not included beside it, so it is not a standalone runnable page. The browser demo lives in `site/demo/` and uses sample data; the native app and this written spec define current behavior.
- Logo artwork: `design/assets/shika-logo.png`.
- The values that ship are in the app, listed below. This folder is not imported.

---

## Index

`design/` is the written spec and the picture. The CSS token files, JSX components, ui kit, guidelines, loader, and skill are not here. The app is the copy that runs.

- `design/DESIGN.md`: color, type, spacing, layout, focus, motion, copy, and icons.
- `design/Shika v3.dc.html`: the visual reference. It still names the old CSS variables. Those names are the vocabulary in this file. They are not a stylesheet the app loads.
- `design/assets/`: `shika-logo.png` (transparent mark) and `shika-app-icon.png` (1024 tile). The running app uses `assets/macos/Shika.icns` and `assets/macos/shika-app-icon-256.png`. JetBrains Mono is `assets/fonts/`.

**Chrome.** `crates/shika/src/main.rs` draws the window: the top row, project headers, cards, empty projects, footer, terminal header, the CLI picker, the dialogs, settings, the toast, and the shared pieces (`kbd`, `hint`, `secondary_button`, `primary_button`, `segment`, `dialog_shell`). Radii, spacing, and type sizes are the values there. Every chrome color is a field of `Chrome` in `crates/shika/src/appearance.rs`, with solid, glass, light, and dark values, so a token changes in one place. Status words are `Status::label` and `Status::chip` in `crates/shika/src/model.rs`.

**Terminal.** `Palette::shika` and `Palette::shika_dark` in `crates/shika-terminal/src/theme.rs` are the terminal colors, including the ANSI 16: `--term-bg` `#131512` under light chrome and `#10120F` under dark, foreground and cursor `--term-fg` `#D5D9CF`, selection that color at 18%. Font, size, line height, and padding are `TerminalConfig` in `crates/shika-terminal/src/view.rs`.

**Glass.** Opacity, blur, and whether frost covers the sidebar alone or the sidebar and terminal: `crates/shika/src/appearance.rs`, saved by `crates/shika-core/src/settings.rs`. The values are under Glass below. There is no `tokens/glass.css`.

---

## Content fundamentals

Shika talks like a terse colleague. It reports facts and names consequences. It does not cheer, apologize, or explain itself.

- **Sentence case everywhere.** Buttons, titles, statuses: "New agent", "Ready to check", "Delete worktree". The only all-caps text is "SHIKA" in the notification header, matching macOS.
- **Status words are fixed:** Ready to check, Working, Waiting, Asking you. They name the states and are not painted on the card. Asking you is a recognizable blocking permission/question dialog, not a semantic guess from prose. Never invent synonyms ("Needs input", "Done").
- **Address the user as "you", rarely.** "Asking you". Shika never says "I" or "we".
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
- Status is the only color: amber = asking you, green = ready to check, blue = working, grey = waiting. Each has three tokens: `dot`, `text` (meets contrast on the column), `tint` (resting background for asking and ready cards only).
- There is **no brand accent**. Selection and focus use ink (`--focus-ring`), not blue. Primary buttons are ink-filled.
- The terminal uses its own dark palette in both themes (`--term-*`) and a soft ANSI 16 (`--ansi-*`). In dark mode the terminal is a step darker than the column (`#10120F` vs `#1A1C19`).

**Type.**
- Two families. UI: the macOS system font (SF Pro) via `-apple-system`. Code, paths, branches, timers, key caps and the terminal: JetBrains Mono.
- Exact sizes, not a grid: 15 / 14 / 13.5 / 13 / 12.5 / 12 / 11.5 / 11 / 10.5. Weights 400, 500, 600 only.
- Timers use `tabular-nums` so they don't jitter.
- Anything a developer might copy (branch, path, command) is mono.

**Layout.**
- Agent column 540px by default, terminal takes the rest. One terminal on screen, ever. The window opens at 1400x880 and stops at 960x600.
- The column's right edge resizes it: an invisible 8px strip centered on the hairline, with the resize cursor. On hover and through a drag a 2px ink-4 line shows in the strip. Width 320 to 800; the terminal keeps at least 420. Double-click restores 540. A drag stops at 320 and never hides the column; only the toggle does.
- The column can hide (Cmd+B). Its toggle is a 24px icon button (radius 6, 15px glyph: a window with its left pane split off, drawn for Shika) before the gear, ink-3, ink-1 on hover, tooltip "Hide agent column ⌘B". Hidden, the terminal header starts with the same button after the traffic lights (at the wordmark's inset, centered in the 48px row) in term-dim, term-white and the term-hover wash on hover, tooltip "Show agent column ⌘B"; over the empty state it sits in the drag strip. Below 400px (560px with Leftover worktrees) the footer drops its key hints. On a card's second line the branch truncates before the diff stat.
- The 48px top row of both halves is the title bar; there is no separate title strip. Column half: the real traffic lights (x 18, centered), the wordmark "Shika" (13.5 semibold), a gear icon button (24px, radius 6, 15px glyph, tooltip "Settings ⌘,"), and New agent `n`. Terminal half: the terminal header, or a plain strip over the empty state. Empty space in both halves drags the window and a double-click zooms.
- No summary: the project list starts right under the top row. Status lives on the cards.
- Project list (padding 8 12 18, 20 between projects, 6 inside). Header (min height 24, padding 0 4 0 8, gap 10): name 15 semibold, path mono 12 ink-3 with `~`, the base branch New starts from (`dev`, or `main` when unset) in mono 12 ink-3 after a 12px git-branch glyph (gap 4), both turning ink-1 on hover and shows the tooltip "Base branch b" (it is hidden while unresolved or when the configured branch is missing), "2 agents" 11.5 ink-3, then `+` (24px icon button, opens the picker for that project). The path truncates first. A click on the base branch, or `b`, opens the Base branch dialog. Remove shows only while the header is hovered. A selected header gets the row-selected wash, radius 6. `j`/`k` moves through cards and skips headers. Up to 3 cards each ("+ N more · j to reach", 12 ink-3).
- Empty project: 1px dashed box, radius 10, padding 12 14, 12.5 ink-3, "No one is on this repo. Press + to start an agent." A click opens the picker for that project.
- Footer (top line-2, padding 10 12 12): "Add project…" `a` (12.5 ink-2, hover wash, radius 6), "Leftover worktrees (N)" when there are leftovers, and on the right `j k` move, `↵` terminal, `c` close (11 ink-3). The hints stay on that one line.
- Terminal header (48px, term-header fill, bottom term-line): connected, task-scoped terminal tabs, not a segmented control. Tabs sit on the bottom of the row, 34 high, radius 7 on the top corners only, padding 0 12, 12px, min 88 and max 180 wide with the label truncated, no gap between them, starting 8px from the left so the label lines up with the terminal text. The active tab is filled with term-tab, has a 1px term-line outline on its top and sides, and term-white text; at the bottom its sides flare outward in concave curves of radius 7 that run into the bottom line, which breaks under it, so the tab opens into the terminal. The flares are painted over the bottom corners of the neighbors, so labels do not move when the active tab changes; a neighbor's bottom line stops where a flare meets it, and the strip keeps 7px after the last tab for its flare. term-tab is the terminal background adjusted so that, painted over the translucent header, it gives exactly the terminal's pixels. Inactive tabs are term-dim text with no fill; hover gives term-fg text and fills the tab's own shape with the term-hover wash: the same top, sides, and flares, standing on the bottom line instead of breaking it, with no outline. Beside the active tab, the hover's bottom corner on that side rounds inward (radius 7) and fits inside the active tab's flare. Only the pinned CLI tab exists initially. Shell tabs are added on request and numbered monotonically (Shell, Shell 2). Each shell has a 16px `×` (radius 4, hover term-hover and term-white, tooltip "Close shell tab (stops its processes) ⌘W"), always shown on the active tab and on hover otherwise, in a fixed slot so the label does not move. The CLI tab has no `×`. `+` (24px, radius 6, 15px glyph, tooltip "New shell tab ⌘T") follows the last tab; when tabs overflow, the strip scrolls and `+` stays at its right edge. Close task `c` (1px term-seg-active border, radius 6, hover term-hover) stays at the right, centered on the tab labels. The bottom line is drawn by each piece of the row, never by the header, since a translucent tab cannot hide a line under it. Selection reveals the tab without pinning manual scrolling. A metadata row below the header (padding 8 20 2, gap 12) shares the terminal background, as part of the active tab's page: the worktree path in mono 11.5 term-dim, `~` for home, ellipsis at the start; the focus hint is 11.5 term-faint-ui, "↵ type here" or "ctrl q back to cards".
- Empty terminal: the app icon 64px, "No agent selected" 13 term-faint-ui, and `j` select, `n` new agent. The toast sits centered over the terminal side, 22 from the bottom.
- Projects stay visible even with no agents, shown as a dashed empty box.
- Cards inside a project are sorted by attention: asking, ready, working, waiting.

**Cards.** Radius 10, padding 12/14/11, gap 6. Line one: the task in 14 medium (line 20, ink-3 while it still reads "New <CLI>"), then, gap 10, the elapsed turn time in mono 11.5 in the working text color, shown only while Working (`42s`, `51m`, `1h 3m`), including blocked time and never reset by drafts, terminal interactions, or status flicker, and the status signal at the right end, a 13px slot so signals line up down the column. Working: three 3px squares in the working dot color, gap 2, rising up to 5px in a staggered wave, offsets snapped to whole points. Ready: an 8px ready dot until the result is seen. Asking: an 8px amber dot, including after the blocker was seen. Waiting: an 8px solid dot in the waiting ring grey. A seen Ready card shows nothing there. There is no dot on the left; the eye reads to the right. A Ready result is seen when its terminal has focus, or when the card stays selected for one second while the window is active, so `j` passing over it does not count. Each turn that ends brings the dot back. Line two (no indent, 12px ink-3, gap 6, `·` in ink-5): CLI · branch (mono 11.5, truncates before the diff stat) · the diff stat while Ready, like `2 files +64 −3` with a real minus. Status on a card is the signal, the tint, the timer, and the diff stat. While setup is running or has failed, the stage or "Setup failed" sits between the CLI and the branch in the status text color (600 Ready/Asking, 500 Working, 400 Waiting). A long setup stage gives way; "Setup failed" stays whole. The status words are not painted on the card. The selected card shows key hints on the right while the cards have focus: Ready `↵ read`, Asking `↵ answer`, Working `↵ watch`, Waiting `↵ write prompt`. Key caps everywhere: mono 10.5, sunken fill, ink-2, radius 4, padding 1 5 (0 5 inside a card). No agent output on cards. At rest: 45% white (3% white in dark) with a 1px hairline (`--shadow-card`). Asking and unseen ready cards get a faint status tint. Selected: raised white with a 1.5px ink outline. Selected while the terminal has focus: hairline only, hints hidden.

**Borders and elevation.** Hairlines everywhere (`rgba(0,0,0,.045)` or `#DEDBD5`). Real drop shadows only on floating things: dialogs, picker, toast, notification. Buttons have a 1px shadow in light mode only.

**Corner radii.** 4 key caps and shell `×` · 5 segments · 6 icon buttons, text buttons, fields, Close · 7 buttons, list rows, segment tracks, terminal tab tops and flares · 8 toasts · 10 cards · 12 dialogs · 14 notifications. Inner radii nest: segment 5 inside track 7 with 2px padding.

**Overlays.** The picker is 420px with padding 8 and sits 18% from the top: "New agent" 14 semibold, "in {project}" 13 ink-3, "tab to change project" 11.5 ink-4; rows (padding 8 10, radius 7, gap 12) with a number key cap, the CLI name 13.5 medium (ink-4 when missing), and the binary path or "<binary> not found on PATH" in mono 11 ink-3 on the right; a footer under line-2: "↵ start", "esc cancel", "branches from `dev`" with the base branch in mono (gap 4), or "creates a branch and a worktree" while the base is unknown. Dialogs are 400px (Settings 440), padding 20 20 16, gap 8, 20% from the top: title 14 semibold, facts 13/19 ink-2, buttons right-aligned with gap 8. Secondary buttons: raised fill, 1px line-control border, radius 7, 12.5px, the control shadow in light only, raised-hover on hover, the key in mono ink-3 after the label. The primary button is ink-filled with the key in `--kbd-inverse-fg`; in the close dialog it is Push when Push is offered, otherwise Discard. The Base branch dialog is a 400px dialog: "Base branch for {project}", the fact "New agents branch from it. Running agents keep their base.", a 360px mono field (the Settings text field: 26 high, radius 6, raised fill, focus border, 1px focus caret) prefilled with the saved branch, or empty with the default branch as an ink-4 placeholder; under the field, known branches in list rows (mono 12, radius 7, at most six visible and then scrolling), filtered to names starting with the field, the highlighted row in the row-selected wash, and "checked out" in ink-3 on the main checkout's branch; Up and Down move the highlight, and a click sets that branch; when the typed name matches nothing and origin exists, ink-3 dialog text reads "Not on this machine. Enter fetches it from origin."; an error under it in dialog text and ink-1, such as "No branch named dev on origin or locally."; then Cancel `esc` and the primary "Set base branch" `↵`, which reads "Use default branch" while the field is empty and no row is highlighted. Settings rows are 13px list rows with mono number fields, `-`/`+` as small secondary buttons, and "Apply to" as a segmented control on a sunken track with a raised active segment. The labels carry the meaning ("Terminal font size"); there is no help paragraph, only the key hints for the selected row above Done.

**Backgrounds.** No gradients, images, textures, or illustrations. At 100% opacity the fills are flat. Below that, the window is glass, described next. The macOS notification may blur to match the OS.

**Motion.** Short and functional. Hover/press 120ms, overlays fade up 4px in 180ms, column width changes in 220ms, all on `cubic-bezier(.2,.8,.2,1)`. The only loops: the working pixels wave (1.2s, each square a sixth of a cycle behind the last) and the terminal cursor blinks (1.1s, steps). Nothing else bounces. Keep the pixels small: Working is the state that does not need the user. `prefers-reduced-motion` zeroes durations.

**Hover and press.** Hover adds a 4% ink wash (`--surface-hover`) or steps the surface lighter (`--surface-raised-hover`). Icon buttons darken their glyph from ink-3 to ink-1. There is no press scale. Cards have no hover; selection is the state that matters.

**Focus model.** Focus is either on the cards or in the terminal. Cards focused: selected card has the ink outline and key hints; terminal cursor is a hollow block. Terminal focused: card outline drops to a hairline; cursor is a solid blinking block; the terminal header reads "ctrl q back to cards". Escape is typed into the terminal. Command shortcuts provide direct transitions from either surface: `⌘]` / `⌘[` move between agents while preserving card versus terminal focus, `⌘N` opens New in the current project, and `⌘B` hides or shows the agent column without moving focus. They skip project headers, wrap, and do not act while an overlay is open or the app is busy. Ctrl+Tab and Ctrl+Shift+Tab cycle the selected task's tabs. Cmd+1 through Cmd+9 jump to a tab, with the agent on Cmd+1. The native Agent menu lists the actions and keys. New's tooltip reads "New agent ⌘N" and terminal add/close controls show their shortcuts using the existing KeyTip component and theme tokens. Cmd+T adds a shell; Cmd+W closes the selected shell and does nothing on the pinned agent, so it never starts Close task. Each task keeps its selected tab. Picker cancellation, Settings dismissal, and Base branch completion/cancellation restore the previous focus. Close cancellation keeps its workflow-specific shell behavior. Selection changes scroll only enough to reveal the selected row; redraws leave manual scrolling alone. Shell startup focuses its view immediately and never reclaims focus on completion.

**Glass.** Frosted, and the look when background opacity is below 100%. At 100% the window is solid and blur does nothing. The saved default is opaque. Settings (`Cmd-,`) sets opacity from 0 to 100, blur radius from 0 to 255, whether frost covers the sidebar alone or the sidebar and the terminal, the terminal font size from 8 to 32 (default 14), and whether a notification plays a sound. Chrome type sizes stay fixed. The title bar uses the sidebar opacity, so the blur shows through it.

The app does this in `crates/shika/src/appearance.rs`. GPUI's window background is `Transparent`, not `Blurred`, because GPUI's blur has one fixed strength. The radius is the private `CGSSetWindowBackgroundBlurRadius`. A CSS `backdrop-filter` cannot see the desktop, so do not add one, and do not add `tokens/glass.css`. The browser mock's `blur(44px) saturate(1.9)` is only a stand-in for that native blur.

When glass is on, `chrome_for` in `crates/shika/src/appearance.rs` paints these tints. The column alpha is the Settings opacity.

- Light column: `#F6F8F2`. Dark column: `#181A17`. Solid mode stays `#F1F2EC` light and `#1A1C19` dark.
- Terminal text never sits on a visible photo. Sidebar-only frost keeps the terminal at `1.0`. When frost covers the terminal, its alpha is the Settings opacity or `0.85`, whichever is higher. CLI-colored cells stay opaque. The header is a step more transparent than that surface, and not below `0.75`.
- Resting cards are 48% white in light glass and 4.5% white in dark glass, with a 1px top highlight. The selected card stays solid. Unseen ready cards use the ready tint at 78% in light glass and 62% in dark glass.
- Dialogs, the picker, the toast, and tooltips stay solid: light `#FAFAF7`, dark `#242722` for the overlay, and the toast's solid color. The window-server blur only reaches what is behind the window, and GPUI has no backdrop blur, so a translucent popup would show the cards and terminal text under it, sharp. No overlay dims the window: Settings previews opacity and blur on it, and the picker and dialogs match. The panel's shadow and ring separate it.
- Hairlines and meta ink shift so they stay visible on the frost. Light glass meta ink `#5F6459`, hairline 10% black. Dark glass meta ink `#A6AB9E`, hairline 8% white.
- macOS Reduce transparency turns glass off: solid colors, no blur, terminal alpha `1.0`. The saved opacity is left as it is.

Hover washes and the notification stay translucent in both modes. Ink-5 separators shift too: `#ADB1A5` on light glass, `#5E6359` on dark glass.

GPUI's Metal blending adds alpha (source one, destination one), so a translucent fill painted over the translucent column comes out opaque: resting cards and washes in glass do not show the desktop through themselves. In light glass a resting card reads as a light grey rather than white frost. Fixing that needs a GPUI change.

**Status values in the app.** The oklch tokens converted to sRGB. Light: ready dot `#45B164`, text `#21763C`, tint `#EEFBF0`; asking `#ED8725` / `#AB5200` / `#FFF5E7`; working dot `#4493D0`, text `#266EA4`; waiting dot `#A3A79B`. Dark: ready `#68CA80` / `#89DA9B` / `#18241A`; asking `#F8A13F` / `#F6B669` / `#2B1F11`; working `#66ABE5` / `#8CC4F4`; waiting dot `#6B7065`. Asking values restore the existing archived/demo oklch tokens. Glass asking tint is `#FFF4DA` at 78% in light and `#3E290F` at 62% in dark. Asking cards retain their tint until resolved. Waiting text is ink-3.

**Elevation in the app.** Selected card: 1.5px focus ring plus `0 2 6` at 7% ink (dark `0 2 8` at 30% black). Selected while the terminal has focus: a 1px `--line-selected-dim` ring. A resting card's 1px ring is a border, since a GPUI drop shadow would also paint under a translucent card. Dialogs and picker: `0 24 60` at 30% (dark 55%) plus a 0.5px ring. Toast: `0 6 20` at 35% black.

**Dark mode.** Follows macOS, from `window.appearance()`. Same names, different values. Status colors get lighter and the tints become low-chroma charcoal. `Palette::shika_dark` is the dark terminal background. Dark chrome and dark glass are painted by `chrome_for`.

---

## Iconography

- **Sources:** The native app embeds Heroicons' solid `cog-6-tooth` as `SETTINGS_ICON` and Octicons' `git-branch-16` as `BRANCH_ICON` in `crates/shika/src/main.rs`. The landing page uses Tabler outline icons. Preserve the notices in `THIRD_PARTY_NOTICES.md` when copying artwork; new icons should fit the existing size, weight, and inherited colors.
- **Style:** 1.5px stroke, 16px standalone, 14px inside buttons, color inherits (ink-3 at rest, ink-1 on hover).
- **Used for:** Plus (new agent on a project, drawn as a 16px "+" like the prototype), FolderPlus (add project), X (close, when there's no room for the word), GitBranch, Terminal, CircleAlert (CLI not on PATH). That is roughly the whole list.
- **Not used for:** status (status is the tint, the dots, and the working pixels), decoration, card contents, section headers.
- **Unicode as glyphs:** `↵` for Enter in key caps, `·` as the meta separator, `❯` and `●` appear inside agent terminals (drawn by the CLIs, not by Shika).
- **No emoji.** No PNG icons. No hand-drawn SVG.
- **Logo:** a deer head (Shikamaru's deer) with a terminal node graph below it. Black, sage, cream, with a mint glow. Use `design/assets/shika-logo.png` as is: don't recolor it, outline it or put it on a busy background.
- **App icon:** `design/assets/shika-app-icon.png`, the logo on a `--brand-tile` rounded square. The bundle icon is `assets/macos/Shika.icns`. Notifications use `assets/macos/shika-app-icon-256.png` (36px). It does not appear in the window chrome. For the macOS `.icns`, redraw on Apple's 824px icon grid inside a 1024 canvas.
- **Wordmark:** "Shika" in SF Pro Semibold, tracking -0.02em, next to the icon.

---

## Rules

Follow this file for color, type, spacing, layout, focus, motion, copy, and icons.

- Reuse the colors in `Chrome` (`crates/shika/src/appearance.rs`), the type sizes, radii, and shadows in `crates/shika/src/main.rs`, and the terminal palette in `crates/shika-terminal/src/theme.rs`. No color is written in `main.rs`. Never hard-code a new color, font size, radius, or shadow. If no value fits, stop and ask. Do not invent one.
- Build new UI from the views in `crates/shika/src/main.rs` and `crates/shika-terminal/src/view.rs` before writing new ones. New views use those same values and the states rest, hover, selected, selected-dimmed, and disabled.
- Status colors (asking, ready, working, waiting) are the only hues in the chrome. No brand accent. Selection and focus use ink.
- Copy is sentence case. Status words stay fixed. Buttons are verbs that name the outcome. No emoji. No exclamation marks. The longer copy rules are under Content fundamentals.
- Every action with a shortcut shows its key. Typing in the terminal or a text field never triggers app shortcuts.
- Every UI change must work in both light and dark mode. Check both before finishing.
- Theme the terminal with `Palette` in `crates/shika-terminal/src/theme.rs`. `shika` is the terminal under light chrome. `shika_dark` is the same palette with the darker background. JetBrains Mono is already bundled under `assets/fonts/` (OFL).
- Glass is the Glass section of this file, applied by `crates/shika/src/appearance.rs`. At 100% opacity the window is solid. Below that, use the glass tints. When frost covers the terminal, keep its alpha at or above 0.85. Do not add a CSS glass file.
- If you add a color, size, radius, or a new view, update this file in the same change.
