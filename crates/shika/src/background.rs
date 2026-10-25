//! The background image: one decode per picked file, one effect raster per
//! effect, theme side, and blur, built off the UI thread, then painted under
//! the translucent column (and the terminal, when frost covers it) in place
//! of the desktop. Framing is applied at paint time, so dragging or zooming
//! never rebuilds the raster. A new raster crossfades in over the old one.
//!
//! The decoding contract, the 2048px working copy, and the five effects are
//! ported from Zeron's new-thread background (`crates/ui/src/
//! new_thread_background_effects.rs`, MIT, Copyright (c) 2026 Wing; notice in
//! `assets/licenses/Zeron-MIT.txt`), keeping its fixed values.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, Context, Corners, IntoElement, Pixels, Point, RenderImage, Styled, Window, div,
};
use gpui::{ParentElement, point, px, size};
use shika_core::{BackgroundEffect, BackgroundFraming};

/// The longest side of the working copy the effects read.
const WORKING_SIDE: u32 = 2048;
/// A window width the blur radius is measured against. The raster covers
/// the window, so a radius in points becomes pixels of the working copy at
/// roughly this scale; the exact window width would rebuild on every resize.
const BLUR_REFERENCE_WIDTH: f32 = 1440.;
/// How long a new raster takes to replace the old one. Instant under Reduce
/// Motion.
const CROSSFADE: Duration = Duration::from_millis(280);

/// Decode the exact bytes that will be stored, by their content rather than
/// the file's extension.
pub fn decode(bytes: &[u8]) -> image::ImageResult<image::DynamicImage> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()?
        .decode()
}

/// The words a refused file gets.
pub const UNREADABLE: &str =
    "This image could not be read. Choose a PNG, JPEG, GIF, WebP, BMP, or TIFF file.";

/// What one raster is built from. Dither and None look the same on both
/// theme sides, so they ignore `light` and share one raster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub file: String,
    pub effect: BackgroundEffect,
    pub light: bool,
    pub blur: u8,
}

impl Key {
    pub fn new(file: String, effect: BackgroundEffect, light: bool, blur: u8) -> Self {
        let light = light && !matches!(effect, BackgroundEffect::None | BackgroundEffect::Dither);
        Self {
            file,
            effect,
            light,
            blur,
        }
    }
}

/// The working copy: each pixel's RGBA and its luminance.
struct Source {
    width: u32,
    height: u32,
    luma: Box<[u8]>,
    colors: Box<[[u8; 4]]>,
}

impl Source {
    fn new(image: &image::DynamicImage, blur: u8, side: u32) -> Self {
        let mut working = image.thumbnail(side, side).to_rgba8();
        if blur > 0 {
            let sigma = f32::from(blur) * working.width() as f32 / BLUR_REFERENCE_WIDTH / 2.;
            working = image::imageops::fast_blur(&working, sigma);
        }
        let luma = image::DynamicImage::ImageRgba8(working.clone()).to_luma8();
        Self {
            width: working.width(),
            height: working.height(),
            luma: luma.into_raw().into_boxed_slice(),
            colors: working.pixels().map(|p| p.0).collect(),
        }
    }

    fn at(&self, x: u32, y: u32) -> usize {
        (y.min(self.height - 1) * self.width + x.min(self.width - 1)) as usize
    }
}

/// Build one raster. RenderImage takes BGRA, so red and blue swap here.
fn raster(image: &image::DynamicImage, key: &Key) -> Arc<RenderImage> {
    raster_at(image, key, WORKING_SIDE)
}

fn raster_at(image: &image::DynamicImage, key: &Key, side: u32) -> Arc<RenderImage> {
    let source = Source::new(image, key.blur, side);
    let pixels = match key.effect {
        BackgroundEffect::None => image::RgbaImage::from_fn(source.width, source.height, |x, y| {
            let [r, g, b, a] = source.colors[source.at(x, y)];
            image::Rgba([b, g, r, a])
        }),
        BackgroundEffect::Dither => dither(&source),
        BackgroundEffect::Ascii => ascii(&source, key.light),
        BackgroundEffect::Halftone => halftone(&source, key.light),
        BackgroundEffect::Scanlines => scanlines(&source, key.light),
    };
    Arc::new(RenderImage::new([image::Frame::new(pixels)]))
}

/// Every third row darkened (lightened on a light theme).
fn scanlines(source: &Source, light: bool) -> image::RgbaImage {
    image::RgbaImage::from_fn(source.width, source.height, |x, y| {
        let [r, g, b, a] = source.colors[source.at(x, y)];
        let gain = if y % 3 == 0 { 0.52 } else { 1.0 };
        let channel = |value: u8| {
            if light {
                (value as f32 + (255.0 - value as f32) * (1.0 - gain)) as u8
            } else {
                (value as f32 * gain) as u8
            }
        };
        image::Rgba([channel(b), channel(g), channel(r), a])
    })
}

/// Bitmap glyphs in 6x8 cells, denser where the image is brighter (darker
/// on a light theme), over a dimmed copy of the image.
fn ascii(source: &Source, light: bool) -> image::RgbaImage {
    // Five-column glyphs, one column and row of spacing, lightest first.
    const GLYPHS: [[u8; 7]; 10] = [
        [0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 4, 0],
        [0, 4, 0, 0, 4, 0, 0],
        [0, 0, 0, 14, 0, 0, 0],
        [0, 0, 14, 0, 14, 0, 0],
        [0, 4, 4, 31, 4, 4, 0],
        [0, 21, 14, 31, 14, 21, 0],
        [10, 10, 31, 10, 31, 10, 10],
        [17, 2, 4, 4, 8, 16, 17],
        [14, 17, 23, 21, 23, 16, 14],
    ];
    image::RgbaImage::from_fn(source.width, source.height, |x, y| {
        let sample = source.at(x / 6 * 6 + 3, y / 8 * 8 + 4);
        let density = if light {
            255 - source.luma[sample]
        } else {
            source.luma[sample]
        };
        let index = ((density as f32 / 255.0).sqrt() * 9.0) as usize;
        let ink = x % 6 < 5 && y % 8 < 7 && GLYPHS[index][y as usize % 8] & (1 << (4 - x % 6)) != 0;
        let [r, g, b, a] = source.colors[source.at(x, y)];
        let [cr, cg, cb, _] = source.colors[sample];
        let paper = if light { 255.0 } else { 0.0 };
        let mix = |base: u8, glyph: u8| {
            (base as f32 * 0.60
                + if ink {
                    glyph as f32 * 0.40
                } else {
                    paper * 0.40
                }) as u8
        };
        image::Rgba([mix(b, cb), mix(g, cg), mix(r, cr), a])
    })
}

/// Round dots on a 4px grid, larger where the image is brighter (darker on
/// a light theme), over a dimmed copy of the image.
fn halftone(source: &Source, light: bool) -> image::RgbaImage {
    let (width, height) = (source.width, source.height);
    let paper = if light { 255 } else { 0 };
    let mut pixels =
        image::RgbaImage::from_pixel(width, height, image::Rgba([paper, paper, paper, 255]));
    for y in (0..height).step_by(4) {
        for x in (0..width).step_by(4) {
            let luma = source.luma[source.at(x, y)];
            let luma = if light { 255 - luma } else { luma };
            let radius = 2.0 * (0.3 + 0.7 * (luma as f32 / 255.0).sqrt());
            let [r, g, b, a] = source.colors[source.at(x + 2, y + 2)];
            for dy in 0..4.min(height - y) {
                for dx in 0..4.min(width - x) {
                    let distance = ((dx as f32 - 1.5).powi(2) + (dy as f32 - 1.5).powi(2)).sqrt();
                    let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0) * a as f32 / 255.0;
                    let [sr, sg, sb, sa] = source.colors[source.at(x + dx, y + dy)];
                    let blend = |under: u8, dot: u8| {
                        (under as f32 * 0.60
                            + (dot as f32 * coverage + paper as f32 * (1.0 - coverage)) * 0.40)
                            as u8
                    };
                    pixels.put_pixel(
                        x + dx,
                        y + dy,
                        image::Rgba([blend(sb, b), blend(sg, g), blend(sr, r), sa]),
                    );
                }
            }
        }
    }
    pixels
}

/// A 4x4 ordered dither in 2px dots: each dot is the image's color at full
/// brightness or nearly black.
fn dither(source: &Source) -> image::RgbaImage {
    const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let (width, height) = (source.width, source.height);
    let mut pixels = image::RgbaImage::new(width, height);
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            // One sample per dot, not four per 2x2 cell.
            let [r, g, b, a] = dither_color(
                source.colors[source.at(x + 1, y + 1)],
                BAYER[y as usize / 2 % 4][x as usize / 2 % 4],
            );
            for dy in 0..2.min(height - y) {
                for dx in 0..2.min(width - x) {
                    pixels.put_pixel(x + dx, y + dy, image::Rgba([b, g, r, a]));
                }
            }
        }
    }
    pixels
}

fn dither_color([r, g, b, a]: [u8; 4], threshold: u8) -> [u8; 4] {
    let peak = r.max(g).max(b) as f32;
    let bright = peak / 255.0 > (threshold as f32 + 0.5) / 16.0;
    let gain = if bright { 255.0 / peak.max(1.0) } else { 0.08 };
    [
        (r as f32 * gain).round() as u8,
        (g as f32 * gain).round() as u8,
        (b as f32 * gain).round() as u8,
        a,
    ]
}

/// Where a `source`-sized image lands to cover `bounds` with `framing`: scaled
/// to cover, then zoomed, its overflow split by the focal point.
pub fn fitted(
    source: (f32, f32),
    bounds: Bounds<Pixels>,
    framing: BackgroundFraming,
) -> Option<Bounds<Pixels>> {
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (source_width, source_height) = source;
    if !(width > 0. && height > 0. && source_width > 0. && source_height > 0.)
        || !(width.is_finite() && height.is_finite())
    {
        return None;
    }
    let (focal_x, focal_y) = framing.focal();
    let cover = (width / source_width).max(height / source_height);
    let fitted_width = (source_width * cover * framing.zoom_factor()).max(width);
    let fitted_height = (source_height * cover * framing.zoom_factor()).max(height);
    Some(Bounds::new(
        point(
            bounds.left() - px((fitted_width - width) * focal_x),
            bounds.top() - px((fitted_height - height) * focal_y),
        ),
        size(px(fitted_width), px(fitted_height)),
    ))
}

/// The framing after dragging the image by `delta` inside `bounds`. An axis
/// with no overflow keeps its focal point.
pub fn pan(
    framing: BackgroundFraming,
    source: (f32, f32),
    bounds: Bounds<Pixels>,
    delta: Point<Pixels>,
) -> BackgroundFraming {
    let Some(fit) = fitted(source, bounds, framing) else {
        return framing;
    };
    let (focal_x, focal_y) = framing.focal();
    let along = |focal: f32, overflow: f32, moved: f32| {
        if overflow > 0.5 {
            focal - moved / overflow
        } else {
            focal
        }
    };
    BackgroundFraming::from_parts(
        along(
            focal_x,
            f32::from(fit.size.width - bounds.size.width),
            f32::from(delta.x),
        ),
        along(
            focal_y,
            f32::from(fit.size.height - bounds.size.height),
            f32::from(delta.y),
        ),
        framing.zoom_factor(),
    )
}

/// The framing at `zoom`, keeping the image point under `anchor` where it is.
/// No anchor zooms around the middle of `bounds`.
pub fn zoom_around(
    framing: BackgroundFraming,
    source: (f32, f32),
    bounds: Bounds<Pixels>,
    zoom: f32,
    anchor: Option<Point<Pixels>>,
) -> BackgroundFraming {
    let anchor = anchor.unwrap_or_else(|| bounds.center());
    let (Some(before), Some(after)) = (
        fitted(source, bounds, framing),
        fitted(
            source,
            bounds,
            BackgroundFraming::from_parts(0.5, 0.5, zoom),
        ),
    ) else {
        return framing;
    };
    let axis = |anchor: Pixels,
                start: Pixels,
                edge: Pixels,
                before: Pixels,
                after: Pixels,
                view: Pixels| {
        // Where the anchor falls in the image, 0 to 1, before the zoom.
        let at = f32::from(anchor - before) / f32::from(edge).max(1.);
        let overflow = f32::from(after - view);
        if overflow > 0.5 {
            (f32::from(start - anchor) + at * f32::from(after)) / overflow
        } else {
            0.5
        }
    };
    BackgroundFraming::from_parts(
        axis(
            anchor.x,
            bounds.left(),
            before.size.width,
            before.left(),
            after.size.width,
            bounds.size.width,
        ),
        axis(
            anchor.y,
            bounds.top(),
            before.size.height,
            before.top(),
            after.size.height,
            bounds.size.height,
        ),
        zoom,
    )
}

/// A raster on screen with the framing of its own file. A replaced image
/// keeps its crop while it fades, even though the settings already hold the
/// new image's framing.
#[derive(Clone)]
struct Shown {
    key: Key,
    image: Arc<RenderImage>,
    framing: BackgroundFraming,
}

/// One frame's layers, each with its framing and opacity, leaving first,
/// plus whether the fade still needs frames.
pub struct Layers {
    pub previous: Option<(Arc<RenderImage>, BackgroundFraming, f32)>,
    pub current: Option<(Arc<RenderImage>, BackgroundFraming, f32)>,
    pub fading: bool,
}

/// What the window paints behind its surfaces, and the one build in flight.
#[derive(Default)]
pub struct Backdrop {
    current: Option<Shown>,
    previous: Option<Shown>,
    fade_from: Option<Instant>,
    /// The raster the settings ask for now, and its framing.
    wanted: Option<(Key, BackgroundFraming)>,
    /// The raster being built.
    building: Option<Key>,
    /// The decoded file, kept so an effect change does not decode again.
    decoded: Option<(String, Arc<image::DynamicImage>)>,
    /// A file that could not be read. It is not tried again until the
    /// settings name another file.
    unreadable: Option<String>,
    /// Rasters that left the screen, to drop from the sprite atlas.
    retired: Vec<Arc<RenderImage>>,
}

impl Backdrop {
    /// True when the named copy could not be read.
    pub fn unreadable(&self, file: &str) -> bool {
        self.unreadable.as_deref() == Some(file)
    }

    /// Keep an image the picker already decoded, so showing it costs no
    /// second decode.
    pub fn seed(&mut self, file: String, image: Arc<image::DynamicImage>) {
        self.decoded = Some((file, image));
    }

    /// Ask for `key`'s raster at `framing`, or for none. Builds it in the
    /// background when it is neither on screen nor being built; one build
    /// runs at a time, and the next starts from the next call. Cheap enough
    /// to call on every paint.
    pub fn want<V: 'static>(
        &mut self,
        wanted: Option<(Key, BackgroundFraming)>,
        path: impl FnOnce(&str) -> Option<PathBuf>,
        backdrop: fn(&mut V) -> &mut Backdrop,
        cx: &mut Context<V>,
    ) {
        let file = wanted.as_ref().map(|(key, _)| key.file.as_str());
        if file != self.unreadable.as_deref() {
            self.unreadable = None;
        }
        if let (Some(current), Some((key, framing))) = (&mut self.current, &wanted)
            && current.key.file == key.file
        {
            current.framing = *framing;
        }
        self.wanted = wanted;
        let Some((key, _)) = self.wanted.clone() else {
            // No image: fade the last one out.
            if let Some(leaving) = self.current.take() {
                self.leave(leaving);
            }
            return;
        };
        if self.current.as_ref().is_some_and(|s| s.key == key)
            || self.building.is_some()
            || self.unreadable(&key.file)
        {
            return;
        }
        let decoded = self
            .decoded
            .as_ref()
            .filter(|(file, _)| *file == key.file)
            .map(|(_, image)| image.clone());
        let Some(path) = path(&key.file) else {
            self.unreadable = Some(key.file);
            return;
        };
        self.building = Some(key.clone());
        cx.spawn(async move |this, cx| {
            let job = key.clone();
            let built = cx
                .background_executor()
                .spawn(async move {
                    let image = match decoded {
                        Some(image) => image,
                        None => Arc::new(decode(&std::fs::read(path).ok()?).ok()?),
                    };
                    let raster = raster(&image, &job);
                    Some((image, raster))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                backdrop(view).finish(key, built);
                cx.notify();
            });
        })
        .detach();
    }

    /// A build finished. It shows when the settings still want it.
    fn finish(&mut self, key: Key, built: Option<(Arc<image::DynamicImage>, Arc<RenderImage>)>) {
        self.building = None;
        let Some((image, raster)) = built else {
            self.unreadable = Some(key.file);
            return;
        };
        self.decoded = Some((key.file.clone(), image));
        let Some(framing) = self
            .wanted
            .as_ref()
            .filter(|(wanted, _)| *wanted == key)
            .map(|(_, framing)| *framing)
        else {
            return;
        };
        if let Some(leaving) = self.current.take() {
            self.leave(leaving);
        }
        self.fade_from = Some(Instant::now());
        self.current = Some(Shown {
            key,
            image: raster,
            framing,
        });
    }

    fn leave(&mut self, leaving: Shown) {
        if let Some(older) = self.previous.replace(leaving) {
            self.retired.push(older.image);
        }
        self.fade_from = Some(Instant::now());
    }

    /// The layers to paint now. A finished fade retires the leaving raster
    /// and frees the atlas tiles of everything retired.
    pub fn layers(&mut self, reduce_motion: bool, window: &mut Window) -> Layers {
        let progress = match self.fade_from {
            Some(_) if reduce_motion => 1.,
            Some(from) => (from.elapsed().as_secs_f32() / CROSSFADE.as_secs_f32()).min(1.),
            None => 1.,
        };
        if progress >= 1. {
            self.fade_from = None;
            if let Some(leaving) = self.previous.take() {
                self.retired.push(leaving.image);
            }
        }
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
        // Ease out: the new image arrives quickly and settles.
        let mix = 1. - (1. - progress).powi(3);
        Layers {
            previous: self
                .previous
                .as_ref()
                .map(|s| (s.image.clone(), s.framing, 1. - mix)),
            current: self
                .current
                .as_ref()
                .map(|s| (s.image.clone(), s.framing, mix)),
            fading: self.fade_from.is_some(),
        }
    }

    /// The raster on screen, for the Adjust preview, when it is the wanted
    /// file's.
    pub fn current(&self) -> Option<Arc<RenderImage>> {
        let wanted = self.wanted.as_ref().map(|(key, _)| key.file.as_str());
        self.current
            .as_ref()
            .filter(|s| Some(s.key.file.as_str()) == wanted)
            .map(|s| s.image.clone())
    }
}

/// A raster's size in pixels, for framing math.
pub fn image_size(image: &RenderImage) -> (f32, f32) {
    let size = image.size(0);
    (size.width.0 as f32, size.height.0 as f32)
}

/// One raster painted to cover its parent with `framing`, at `opacity`.
pub fn layer(
    image: Arc<RenderImage>,
    framing: BackgroundFraming,
    opacity: f32,
) -> impl IntoElement {
    rounded_layer(image, framing, opacity, px(0.))
}

/// `layer` with rounded corners. A parent's overflow clip is square.
pub fn rounded_layer(
    image: Arc<RenderImage>,
    framing: BackgroundFraming,
    opacity: f32,
    radius: Pixels,
) -> impl IntoElement {
    div().absolute().inset_0().opacity(opacity).child(
        gpui::canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                if let Some(fit) = fitted(image_size(&image), bounds, framing) {
                    let corners = Corners::all(radius);
                    let _ = window.paint_image(bounds, fit, corners, image.clone(), 0, false);
                }
            },
        )
        .size_full(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(width), px(height)))
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn the_image_covers_the_bounds_and_the_focal_point_picks_the_overflow() {
        let view = bounds(400., 200.);
        // A square image covers a 2:1 view by its width.
        let centered = fitted((100., 100.), view, BackgroundFraming::default()).unwrap();
        assert!(close(f32::from(centered.size.width), 400.));
        assert!(close(f32::from(centered.size.height), 400.));
        assert!(close(f32::from(centered.left()), 10.));
        assert!(close(f32::from(centered.top()), 20. - 100.));
        let top = fitted(
            (100., 100.),
            view,
            BackgroundFraming::from_parts(0.5, 0., 1.),
        )
        .unwrap();
        assert!(close(f32::from(top.top()), 20.));
        let zoomed = fitted(
            (100., 100.),
            view,
            BackgroundFraming::from_parts(1., 1., 2.),
        )
        .unwrap();
        assert!(close(f32::from(zoomed.size.width), 800.));
        assert!(close(f32::from(zoomed.right()), 410.));
        assert!(close(f32::from(zoomed.bottom()), 220.));
        assert!(fitted((0., 100.), view, BackgroundFraming::default()).is_none());
        assert!(fitted((100., 100.), bounds(0., 10.), BackgroundFraming::default()).is_none());
    }

    #[test]
    fn dragging_moves_the_image_with_the_pointer_and_stops_at_its_edges() {
        let view = bounds(400., 200.);
        let start = BackgroundFraming::default();
        // Dragging down shows more of the top: the image moves with the pointer.
        let moved = pan(start, (100., 100.), view, point(px(0.), px(50.)));
        let before = fitted((100., 100.), view, start).unwrap();
        let after = fitted((100., 100.), view, moved).unwrap();
        assert!(close(f32::from(after.top() - before.top()), 50.));
        // No horizontal overflow, so the x focal point stays.
        assert_eq!(moved.focal_x, start.focal_x);
        let far = pan(start, (100., 100.), view, point(px(0.), px(10_000.)));
        assert_eq!(far.focal_y, 0);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let view = bounds(400., 200.);
        let start = BackgroundFraming::default();
        let anchor = point(px(110.), px(70.));
        let before = fitted((100., 100.), view, start).unwrap();
        let zoomed = zoom_around(start, (100., 100.), view, 2., Some(anchor));
        let after = fitted((100., 100.), view, zoomed).unwrap();
        let at = |fit: Bounds<Pixels>| {
            (
                f32::from(anchor.x - fit.left()) / f32::from(fit.size.width),
                f32::from(anchor.y - fit.top()) / f32::from(fit.size.height),
            )
        };
        let (bx, by) = at(before);
        let (ax, ay) = at(after);
        assert!(
            (bx - ax).abs() < 0.002 && (by - ay).abs() < 0.002,
            "{bx},{by} vs {ax},{ay}"
        );
        assert_eq!(zoomed.zoom, 200);
        // Back to 1x there is no horizontal overflow, so x recenters.
        let back = zoom_around(zoomed, (100., 100.), view, 1., None);
        assert_eq!(back.zoom, 100);
        assert_eq!(back.focal_x, 5000);
    }

    #[test]
    fn effects_work_on_a_fixed_size_copy_and_swap_to_bgra() {
        // Every image is scaled to the same working size, as in Zeron, so the
        // fixed dot and glyph sizes look alike whatever the original's size.
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(64, 32, |x, _| {
            image::Rgba([200, (x * 4) as u8, 0, 255])
        }));
        for effect in BackgroundEffect::ALL {
            for light in [false, true] {
                let raster = raster_at(&image, &Key::new("f".into(), effect, light, 0), 128);
                let size = raster.size(0);
                assert_eq!((size.width.0, size.height.0), (128, 64), "{effect:?}");
            }
        }
        let plain = raster_at(
            &image,
            &Key::new("f".into(), BackgroundEffect::None, false, 0),
            128,
        );
        // Red 200 lands in the third byte.
        assert_eq!(&plain.as_bytes(0).unwrap()[..4], &[0, 0, 200, 255]);
        let blurred = raster_at(
            &image,
            &Key::new("f".into(), BackgroundEffect::None, false, 40),
            128,
        );
        assert_eq!(blurred.size(0).width.0, 128);
    }

    #[test]
    fn dither_and_none_share_one_raster_on_both_sides() {
        let dark = Key::new("f".into(), BackgroundEffect::Dither, false, 0);
        assert_eq!(
            Key::new("f".into(), BackgroundEffect::Dither, true, 0),
            dark
        );
        assert_ne!(
            Key::new("f".into(), BackgroundEffect::Halftone, true, 0),
            Key::new("f".into(), BackgroundEffect::Halftone, false, 0)
        );
        // A dot is the color at full brightness or nearly black.
        assert_eq!(dither_color([100, 50, 0, 255], 0), [255, 128, 0, 255]);
        assert_eq!(dither_color([100, 50, 0, 255], 15), [8, 4, 0, 255]);
    }

    #[test]
    fn unreadable_bytes_are_refused() {
        assert!(decode(b"not an image").is_err());
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(decode(&png).unwrap().width(), 2);
    }
}
