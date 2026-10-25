# Background image

An optional picture behind Shika's glass, in place of the desktop, with one of five effects and a crop the author sets. This guide is the contributor contract: the decisions, the state, the code map, and how to debug or extend it. The visual spec is the Background image paragraph under Glass in [design/DESIGN.md](../design/DESIGN.md).

## Decisions (2026-10-24, the author's)

- **It is a backdrop for glass, not a new mode.** The image replaces what the window server would show through the transparent window. Every glass rule stays: Background opacity sets how much shows through, Apply to picks the column alone or the column and terminal, and at 100% opacity or under macOS Reduce transparency the window is solid and the image is not painted. There is no "terminal only" placement: the column always frosts first, as glass does.
- **Settings follow Zeron's**, the MIT-licensed GPUI app ([zeronsh/zeron](https://github.com/zeronsh/zeron)) whose new-thread background was the reference: choose, replace, or remove one image; Shika keeps its own copy; five effects (None, Dither, ASCII, Halftone, Scanlines) with fixed values and no dot size or strength; an Adjust dialog to pan and zoom. Zeron's wallpaper folder, Shuffle, and Match wallpaper colors are not ported: Shika's colors come from its themes.
- **Full opacity hides the image**, so choosing an image at 100% lowers opacity to 70% once. The opacity row shows the change and can undo it. No hint text, no second control.
- **Blur blurs the image.** With an image, the window is opaque and the window server blurs nothing, so the existing blur radius blurs the working copy before its effect instead.

## State

| Where | What |
| --- | --- |
| `settings.json` `background` | `file` (the copy's name), `name` (the original file name, for Settings), `effect`, and `framing` (`focalX`, `focalY` in ten-thousandths, `zoom` in hundredths, 100 to 400). Missing means none. A value that is not a usable record, or a `file` that is not a name Shika writes, loads as none. |
| `<app data>/backgrounds/` | Shika's copies, `background-<nanos>-<pid>-<serial>.<ext>`. Only the one settings name survives a pick or removal. |
| Memory (`background::Backdrop`) | The raster on screen with its file's framing, the leaving raster during a crossfade, the one build in flight, the decoded image of the last file, a file that could not be read, and rasters to drop from the sprite atlas. |

Settings name a copy by its file name only. `shika_core::background::path` resolves it inside `backgrounds/` and refuses anything else, so a hand-edited `settings.json` cannot point Shika at another path. Moving or deleting the original changes nothing.

## Lifecycle

1. **Choose** (`↵` on the row, Choose image…, or Replace) opens the system file dialog. Off the UI thread, Shika reads the file, decodes the exact bytes by content (`background::decode`), and only then writes the copy (`Core::store_background`: a temporary file, synced, renamed into place, always a fresh name). A file that does not decode shows "This image could not be read…" and changes nothing.
2. Settings save the new record with centered framing, keeping the previous effect. Opacity at 100% becomes 70%. Other copies are pruned off-thread. `window_backdrop_dirty` makes the next paint set the window opaque (`appearance::apply` with `image: true`, blur radius 0).
3. **Every paint** calls `Backdrop::want` with the raster the settings ask for: the file, its effect, the theme side (ASCII, Halftone, and Scanlines differ on light and dark; None and Dither do not), and the blur. It is cheap when nothing changed. Otherwise one build runs on the background executor: decode (skipped when the picker already decoded it), scale to a 2048px working copy (`thumbnail`, which also scales small images up, as Zeron does, so fixed-size effects look alike), blur, effect, BGRA `RenderImage`. A build that is no longer wanted when it finishes is dropped; the next paint starts the wanted one.
4. **Paint.** Under the column and terminal, the root paints the column's color at full alpha, then the leaving raster and the new one with their opacities. Framing is applied at paint time (`background::fitted`), so dragging, zooming, or resizing never rebuilds. A finished crossfade (280ms, ease-out, instant under Reduce Motion) drops the leaving raster from the atlas with `Window::drop_image`.
5. **Remove** (`⌫` or Remove) clears the record, prunes every copy, and lets the window go back to transparent glass. The last raster fades out.
6. **Glass off** (100% opacity, Reduce transparency) asks for no raster, so a solid window holds no image memory. The Adjust dialog is the exception: its preview needs the raster.

## Adjust

`Overlay::Adjust` replaces Settings and returns to it on the image row; the opening focus saved by Settings is untouched. The draft framing lives only in the overlay until Apply. The preview has the window's aspect ratio, so the normalized framing is the same crop in the window. Drags use GPUI's drag API (`PanDrag`, `ZoomDrag`, both rendering nothing) with the root's `on_drag_move`, so a drag keeps working when the pointer leaves the preview. Pan math (`background::pan`) moves the image with the pointer and stops at its edges; zoom (`background::zoom_around`) keeps the image point under the pointer fixed. Keys: arrows or `h` `j` `k` `l` pan, `-` / `=` zoom, `0` reset, `↵` apply, `esc` cancel. Shifted keys are dropped by the overlay key handler, so zoom in is `=`, not `+`.

## Code map

| File | Owns |
| --- | --- |
| `crates/shika-core/src/settings.rs` | `Background`, `BackgroundEffect`, `BackgroundFraming`, lenient loading |
| `crates/shika-core/src/background.rs` | Managed names, `store`, `path`, `prune` |
| `crates/shika/src/background.rs` | Decoding, effects (ported from Zeron), framing math, `Backdrop` builds and crossfade, `layer` painting |
| `crates/shika/src/background_view.rs` | Settings rows, choose/remove, window opacity sync, the Adjust dialog |
| `crates/shika/src/appearance.rs` | `apply` / `background_for` take `image`: opaque window, no window-server blur |
| `crates/shika/src/main.rs` | Fields, row constants (`IMAGE_ROW`, `EFFECT_ROW`), key routing, the root's backdrop child and drag listeners |

The effects keep Zeron's values and structure; the notice is `assets/licenses/Zeron-MIT.txt`, listed in `THIRD_PARTY_NOTICES.md` and the bundle's notices.

## Debugging

- **Nothing shows.** Check opacity is below 100%, Reduce transparency is off, and Apply to covers the half you are looking at. Check `backgrounds/` holds the file `settings.json` names. A missing or undecodable copy shows "Image unavailable" in Settings and the column's own color in the window.
- **Slow effect changes in a debug build.** Each raster processes a 2048px image without optimizations; release builds take a fraction of that. Tests build at 128px through `raster_at`.
- **Memory.** One raster is 2048 × up to 2048 × 4 bytes. At most the current and the leaving raster are alive, plus the decoded image of the current file.

## Extending

- A new effect is a `BackgroundEffect` variant (label, description, lenient parse), a raster function, and its theme-side rule in `Key::new`. Keep fixed values; Settings has no per-effect controls.
- Do not paint the original file with `img(path)` anywhere: GPUI would decode the full-size original into the atlas. The Settings tile reuses the raster on screen.
- Do not make the window transparent while an image is set, and do not add a CSS-style backdrop blur: GPUI has none, and the image is the backdrop.
