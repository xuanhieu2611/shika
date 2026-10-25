//! The background image in the app: its two Settings rows, picking and
//! removing the file, the Adjust dialog, and the backdrop painted under the
//! window's surfaces. `background.rs` builds the rasters.
use super::*;
use background::Key;
use gpui::{MouseDownEvent, Point, point};
use shika_core::{Background, BackgroundEffect, BackgroundFraming};
use std::cell::Cell;
use std::rc::Rc;

/// Picking an image at full opacity would show nothing, so the pick lowers
/// opacity to this once. The opacity row shows it and can put it back.
pub(super) const PICKED_OPACITY: u8 = 70;
/// One arrow-key pan in the Adjust dialog, in preview points.
const PAN_STEP: f32 = 16.;
/// One `-` / `=` or button step of zoom.
const ZOOM_STEP: f32 = 0.1;
/// The Adjust dialog: preview width, padding included in the shell.
const ADJUST_WIDTH: f32 = 600.;
const PREVIEW_WIDTH: f32 = ADJUST_WIDTH - 40.;
const PREVIEW_MAX_HEIGHT: f32 = 340.;

/// The Adjust dialog's state. `draft` is the framing on screen in the
/// preview; Apply saves it, Cancel and Escape drop it.
pub(super) struct Adjust {
    draft: BackgroundFraming,
    /// The pointer's last position while the preview is dragged.
    drag: Option<Point<Pixels>>,
    preview: Rc<Cell<Option<Bounds<Pixels>>>>,
    track: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// The preview drag. It renders nothing; the preview moves instead.
pub(super) struct PanDrag;
impl Render for PanDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// The zoom track drag.
pub(super) struct ZoomDrag;
impl Render for ZoomDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

fn zoom_fraction(zoom: f32) -> f32 {
    let (min, max) = (
        f32::from(BackgroundFraming::MIN_ZOOM) / 100.,
        f32::from(BackgroundFraming::MAX_ZOOM) / 100.,
    );
    ((zoom - min) / (max - min)).clamp(0., 1.)
}

fn zoom_at(track: Bounds<Pixels>, x: Pixels) -> f32 {
    let (min, max) = (
        f32::from(BackgroundFraming::MIN_ZOOM) / 100.,
        f32::from(BackgroundFraming::MAX_ZOOM) / 100.,
    );
    let at = f32::from(x - track.left()) / f32::from(track.size.width).max(1.);
    min + at.clamp(0., 1.) * (max - min)
}

/// The outline of an image tile, drawn over the image, which would cover a
/// border painted under it.
fn ring(radius: Pixels, chrome: &Chrome) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .rounded(radius)
        .border_1()
        .border_color(chrome.line_control)
}

impl Shika {
    /// The raster the settings ask for: the image, its effect on the side
    /// that paints, and the blur. None while glass is off, so a solid window
    /// builds and holds nothing, except under Adjust, whose preview shows it.
    fn wanted_backdrop(&self, window: &Window) -> Option<(Key, BackgroundFraming)> {
        let background = self.background.as_ref()?;
        if !appearance::glass_active(&self.appearance, self.reduce_transparency)
            && !matches!(self.overlay, Some(Overlay::Adjust(_)))
        {
            return None;
        }
        let light = !self.active_theme(window).dark;
        Some((
            Key::new(
                background.file.clone(),
                background.effect,
                light,
                self.appearance.blur,
            ),
            background.framing,
        ))
    }

    /// Bring the backdrop up to date and return its layers, bottom first:
    /// the column's own color, so a loading or missing image still leaves an
    /// opaque window under the translucent surfaces, then the images.
    pub(super) fn backdrop_layers(
        &mut self,
        chrome: &Chrome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let wanted = self.wanted_backdrop(window);
        let core = self.core.clone();
        self.backdrop.want(
            wanted,
            |file| core.background_path(file),
            |this: &mut Self| &mut this.backdrop,
            cx,
        );
        let layers = self.backdrop.layers(cx.reduce_motion(), window);
        if layers.fading {
            window.request_animation_frame();
        }
        if self.background.is_none()
            || !appearance::glass_active(&self.appearance, self.reduce_transparency)
        {
            return None;
        }
        let base = Rgba {
            a: 1.,
            ..chrome.column
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(base)
                .children(
                    layers.previous.map(|(image, framing, opacity)| {
                        background::layer(image, framing, opacity)
                    }),
                )
                .children(
                    layers.current.map(|(image, framing, opacity)| {
                        background::layer(image, framing, opacity)
                    }),
                ),
        )
    }

    /// Choose a file, check that it decodes, keep Shika's own copy, and show
    /// it. A file that does not decode changes nothing.
    pub(super) fn pick_background(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose background image".into()),
        });
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let stored = cx
                .background_executor()
                .spawn({
                    let name = name.clone();
                    async move {
                        let bytes =
                            std::fs::read(&path).map_err(|_| background::UNREADABLE.to_string())?;
                        let image = background::decode(&bytes)
                            .map_err(|_| background::UNREADABLE.to_string())?;
                        let file = core
                            .store_background(&bytes, &name)
                            .map_err(|e| e.to_string())?;
                        Ok::<_, String>((file, Arc::new(image)))
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match stored {
                    Ok((file, image)) => this.set_background_file(file, name, image, cx),
                    Err(error) => this.message(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_background_file(
        &mut self,
        file: String,
        name: String,
        image: Arc<image::DynamicImage>,
        cx: &mut Context<Self>,
    ) {
        self.backdrop.seed(file.clone(), image);
        let effect = self
            .background
            .as_ref()
            .map(|b| b.effect)
            .unwrap_or_default();
        self.background = Some(Background {
            file,
            name,
            effect,
            framing: BackgroundFraming::default(),
        });
        if self.appearance.is_opaque() {
            self.appearance = self.appearance.with_opacity(PICKED_OPACITY.into());
        }
        self.background_changed(cx);
    }

    /// Back to the desktop behind glass.
    pub(super) fn remove_background(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.background.is_none() {
            return;
        }
        self.background = None;
        self.background_changed(cx);
    }

    pub(super) fn set_background_effect(
        &mut self,
        effect: BackgroundEffect,
        cx: &mut Context<Self>,
    ) {
        let Some(background) = self.background.as_mut() else {
            return;
        };
        if background.effect == effect {
            return;
        }
        background.effect = effect;
        self.save_settings();
        cx.notify();
    }

    /// Save, then remove the copies settings no longer name. The window
    /// turns opaque or transparent on the next paint, which knows the window.
    fn background_changed(&mut self, cx: &mut Context<Self>) {
        self.window_backdrop_dirty = true;
        self.save_settings();
        let core = self.core.clone();
        let keep = self.background.as_ref().map(|b| b.file.clone());
        cx.background_executor()
            .spawn(async move { core.prune_backgrounds(keep.as_deref()) })
            .detach();
        cx.notify();
    }

    /// Apply a background change to the window itself: opaque with an
    /// image, the glass settings without one.
    pub(super) fn sync_window_backdrop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.window_backdrop_dirty) {
            return;
        }
        self.reduce_transparency = appearance::reduce_transparency();
        appearance::apply(&self.appearance, self.background.is_some(), window);
        self.push_terminal_theme(window, cx);
    }

    /// Keys on the image and effect rows. False leaves the key to the
    /// other Appearance rows' handling.
    pub(super) fn on_background_key(
        &mut self,
        row: usize,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match (row, key) {
            (IMAGE_ROW, "enter") => self.pick_background(cx),
            (IMAGE_ROW, "a") if self.background.is_some() => self.open_adjust(window, cx),
            (IMAGE_ROW, "backspace") => self.remove_background(cx),
            (EFFECT_ROW, "h" | "left" | "l" | "right") => {
                let delta = if matches!(key, "h" | "left") { -1 } else { 1 };
                if let Some(effect) = self.background.as_ref().map(|b| b.effect.step(delta)) {
                    self.set_background_effect(effect, cx);
                }
            }
            (IMAGE_ROW | EFFECT_ROW, "h" | "left" | "l" | "right" | "backspace") => {}
            _ => return false,
        }
        true
    }

    /// The Appearance row `delta` steps from `at`, wrapping, past the effect
    /// row while there is no image to treat.
    pub(super) fn next_appearance_row(&self, at: usize, delta: i64) -> usize {
        let rows = SETTING_ROWS as i64;
        let mut next = (at as i64 + delta).rem_euclid(rows) as usize;
        if next == EFFECT_ROW && self.background.is_none() {
            next = (next as i64 + delta.signum()).rem_euclid(rows) as usize;
        }
        next
    }

    pub(super) fn image_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings { row, .. }) if *row == IMAGE_ROW
        );
        let small = |id: &'static str, label: &'static str| {
            secondary_button(id, chrome)
                .px(px(10.))
                .py(px(3.))
                .rounded(px(6.))
                .text_size(px(12.))
                .child(label)
        };
        let missing = self.background.as_ref().is_some_and(|b| {
            self.backdrop.unreadable(&b.file)
                || !self
                    .core
                    .background_path(&b.file)
                    .is_some_and(|path| path.is_file())
        });
        // The raster on screen doubles as the thumbnail, so Settings never
        // decodes the full-size original into the GPU atlas.
        let thumbnail = self.backdrop.current();
        let mut controls = div().flex().items_center().gap(px(8.)).min_w_0();
        match &self.background {
            Some(background) => {
                let name = if missing {
                    "Image unavailable".to_string()
                } else if background.name.is_empty() {
                    "Image".to_string()
                } else {
                    background.name.clone()
                };
                controls = controls
                    .when(!missing, |d| {
                        d.child(
                            div()
                                .relative()
                                .flex_none()
                                .size(px(26.))
                                .rounded(px(6.))
                                .bg(chrome.sunken)
                                .children(thumbnail.map(|image| {
                                    background::rounded_layer(image, background.framing, 1., px(6.))
                                }))
                                .child(ring(px(6.), chrome)),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(180.))
                            .truncate()
                            .text_color(chrome.ink_3)
                            .child(name),
                    )
                    .when(!missing, |d| {
                        d.child(small("background-adjust", "Adjust").on_click(
                            cx.listener(|this, _, window, cx| this.open_adjust(window, cx)),
                        ))
                    })
                    .child(small("background-replace", "Replace").on_click(cx.listener(
                        |this, _, window, cx| {
                            this.select_setting(IMAGE_ROW, window, cx);
                            this.pick_background(cx);
                        },
                    )))
                    .child(small("background-remove", "Remove").on_click(cx.listener(
                        |this, _, window, cx| {
                            this.select_setting(IMAGE_ROW, window, cx);
                            this.remove_background(cx);
                        },
                    )));
            }
            _ => {
                controls = controls
                    .child(div().text_color(chrome.ink_4).child("None"))
                    .child(
                        small("background-choose", "Choose image…").on_click(cx.listener(
                            |this, _, window, cx| {
                                this.select_setting(IMAGE_ROW, window, cx);
                                this.pick_background(cx);
                            },
                        )),
                    );
            }
        }
        list_row(selected, chrome)
            .id("setting-background-image")
            .justify_between()
            .gap(px(12.))
            .on_click(cx.listener(|this, _, window, cx| this.select_setting(IMAGE_ROW, window, cx)))
            .child(div().flex_none().child("Background image"))
            .child(controls)
    }

    pub(super) fn effect_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        let Some(current) = self.background.as_ref().map(|b| b.effect) else {
            // Kept in the list, hidden, so row numbers stay scroll indices.
            return div().hidden();
        };
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings { row, .. }) if *row == EFFECT_ROW
        );
        let shadow = chrome.control_shadow;
        let mut choices = div()
            .flex()
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(7.))
            .bg(chrome.sunken);
        for effect in BackgroundEffect::ALL {
            let on = effect == current;
            choices = choices.child(
                segment(
                    SharedString::from(format!("background-effect-{}", effect.label())),
                    effect.label(),
                    on,
                    chrome.raised,
                    chrome.ink_1,
                    chrome.ink_3,
                )
                .when(on, |d| {
                    d.shadow(vec![
                        BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                    ])
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_setting(EFFECT_ROW, window, cx);
                    this.set_background_effect(effect, cx);
                })),
            );
        }
        list_row(selected, chrome)
            .justify_between()
            .gap(px(12.))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child("Image effect")
                    .child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(15.))
                            .text_color(chrome.ink_3)
                            .child(current.description()),
                    ),
            )
            .child(choices)
    }

    /// Open the Adjust dialog over Settings, starting from the saved framing.
    pub(super) fn open_adjust(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(background) = self.background.as_ref() else {
            return;
        };
        if self.busy || self.backdrop.unreadable(&background.file) {
            return;
        }
        let draft = background.framing;
        self.commit_setting_edit(window, cx);
        self.overlay = Some(Overlay::Adjust(Box::new(Adjust {
            draft,
            drag: None,
            preview: Rc::default(),
            track: Rc::default(),
        })));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Back to Settings, on the image row. `apply` saves the draft.
    pub(super) fn close_adjust(
        &mut self,
        apply: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::Adjust(adjust)) = &self.overlay else {
            return;
        };
        let draft = adjust.draft;
        if apply
            && let Some(background) = self.background.as_mut()
            && background.framing != draft
        {
            background.framing = draft;
            self.save_settings();
        }
        self.overlay = Some(Overlay::Settings {
            section: SettingsSection::Appearance,
            row: IMAGE_ROW,
            edit: None,
            usage: None,
        });
        self.settings_scroll.scroll_to_item(IMAGE_ROW);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The image's size, the preview's bounds, and the draft, when the
    /// preview has painted once.
    fn adjust_geometry(&self) -> Option<((f32, f32), Bounds<Pixels>)> {
        let Some(Overlay::Adjust(adjust)) = &self.overlay else {
            return None;
        };
        let image = self.backdrop.current()?;
        Some((background::image_size(&image), adjust.preview.get()?))
    }

    fn set_draft(&mut self, draft: BackgroundFraming, cx: &mut Context<Self>) {
        if let Some(Overlay::Adjust(adjust)) = &mut self.overlay {
            adjust.draft = draft;
        }
        cx.notify();
    }

    fn adjust_draft(&self) -> Option<BackgroundFraming> {
        match &self.overlay {
            Some(Overlay::Adjust(adjust)) => Some(adjust.draft),
            _ => None,
        }
    }

    pub(super) fn pan_draft(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some((source, bounds)), Some(draft)) = (self.adjust_geometry(), self.adjust_draft())
        else {
            return;
        };
        self.set_draft(background::pan(draft, source, bounds, delta), cx);
    }

    pub(super) fn zoom_draft(
        &mut self,
        zoom: f32,
        anchor: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let (Some((source, bounds)), Some(draft)) = (self.adjust_geometry(), self.adjust_draft())
        else {
            return;
        };
        self.set_draft(
            background::zoom_around(draft, source, bounds, zoom, anchor),
            cx,
        );
    }

    /// A drag on the preview moved to `position`.
    pub(super) fn drag_preview(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(Overlay::Adjust(adjust)) = &mut self.overlay else {
            return;
        };
        let last = adjust.drag.replace(position);
        if let Some(last) = last {
            self.pan_draft(position - last, cx);
        }
    }

    /// A drag on the zoom track moved to `x`.
    pub(super) fn drag_zoom(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(Overlay::Adjust(adjust)) = &self.overlay else {
            return;
        };
        if let Some(track) = adjust.track.get() {
            self.zoom_draft(zoom_at(track, x), None, cx);
        }
    }

    pub(super) fn adjust_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.adjust_draft() else {
            return;
        };
        let pan = |x: f32, y: f32| point(px(x * PAN_STEP), px(y * PAN_STEP));
        match key {
            "enter" => self.close_adjust(true, window, cx),
            "escape" => self.close_adjust(false, window, cx),
            // Arrows move the image the way a drag would.
            "h" | "left" => self.pan_draft(pan(-1., 0.), cx),
            "l" | "right" => self.pan_draft(pan(1., 0.), cx),
            "k" | "up" => self.pan_draft(pan(0., -1.), cx),
            "j" | "down" => self.pan_draft(pan(0., 1.), cx),
            "-" => self.zoom_draft(draft.zoom_factor() - ZOOM_STEP, None, cx),
            "=" => self.zoom_draft(draft.zoom_factor() + ZOOM_STEP, None, cx),
            "0" => self.set_draft(BackgroundFraming::default(), cx),
            _ => {}
        }
    }

    pub(super) fn adjust_view(
        &self,
        adjust: &Adjust,
        max_h: Pixels,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let viewport = window.viewport_size();
        let aspect = f32::from(viewport.height) / f32::from(viewport.width).max(1.);
        let (mut width, mut height) = (PREVIEW_WIDTH, PREVIEW_WIDTH * aspect);
        if height > PREVIEW_MAX_HEIGHT {
            width *= PREVIEW_MAX_HEIGHT / height;
            height = PREVIEW_MAX_HEIGHT;
        }
        let draft = adjust.draft;
        let image = self.backdrop.current();
        // Where the column falls in the window, so the crop reads in place.
        let column = self
            .pane_widths(window)
            .0
            .map(|w| f32::from(w) / f32::from(viewport.width).max(1.) * width);
        let preview_cell = adjust.preview.clone();
        let preview = div()
            .id("background-preview")
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(height))
            .rounded(px(8.))
            .overflow_hidden()
            .bg(chrome.sunken)
            .cursor_grab()
            .child(
                gpui::canvas(
                    move |bounds, _, _| preview_cell.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .children(image.map(|image| background::rounded_layer(image, draft, 1., px(8.))))
            .children(column.map(|w| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(w))
                    .rounded_l(px(8.))
                    .bg(with_alpha(chrome.column, chrome.column.a.min(0.6)))
                    .border_r_1()
                    .border_color(chrome.hairline)
            }))
            .child(ring(px(8.), chrome))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    if let Some(Overlay::Adjust(adjust)) = &mut this.overlay {
                        adjust.drag = Some(event.position);
                    }
                }),
            )
            .on_drag(PanDrag, |_, _, _, cx| cx.new(|_| PanDrag))
            .on_scroll_wheel(
                cx.listener(|this, event: &gpui::ScrollWheelEvent, window, cx| {
                    let Some(draft) = this.adjust_draft() else {
                        return;
                    };
                    let dy = f32::from(event.delta.pixel_delta(window.line_height()).y);
                    this.zoom_draft(
                        draft.zoom_factor() * (1. + dy * 0.005),
                        Some(event.position),
                        cx,
                    );
                }),
            )
            .on_pinch(cx.listener(|this, event: &gpui::PinchEvent, _, cx| {
                let Some(draft) = this.adjust_draft() else {
                    return;
                };
                this.zoom_draft(
                    draft.zoom_factor() * (1. + event.delta),
                    Some(event.position),
                    cx,
                );
            }));
        let track_cell = adjust.track.clone();
        let fraction = zoom_fraction(draft.zoom_factor());
        let knob = chrome.ink_1;
        let track = div()
            .id("background-zoom")
            .relative()
            .flex_1()
            .h(px(20.))
            .flex()
            .items_center()
            .cursor_pointer()
            .child(
                gpui::canvas(
                    move |bounds, _, _| track_cell.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(4.))
                    .rounded(px(2.))
                    .bg(chrome.sunken)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(gpui::relative(fraction))
                            .rounded(px(2.))
                            .bg(chrome.ink_3),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top(px(3.))
                    .left(gpui::relative(fraction))
                    .ml(px(-7.))
                    .size(px(14.))
                    .rounded_full()
                    .bg(chrome.raised)
                    .border_1()
                    .border_color(knob),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.drag_zoom(event.position.x, cx)
                }),
            )
            .on_drag(ZoomDrag, |_, _, _, cx| cx.new(|_| ZoomDrag));
        let step = |id: &'static str, label: &'static str, delta: f32| {
            step_button(id, label, chrome).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(draft) = this.adjust_draft() {
                    this.zoom_draft(draft.zoom_factor() + delta, None, cx);
                }
            }))
        };
        let percent = format!("{}%", draft.zoom);
        dialog_shell(ADJUST_WIDTH, max_h, chrome)
            .pt(px(20.))
            .px(px(20.))
            .pb(px(16.))
            .gap(px(12.))
            .child(dialog_title(div()).child("Adjust background"))
            .child(dialog_text(chrome).child("Drag to move the image. Scroll or pinch to zoom."))
            .child(div().flex().justify_center().child(preview))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(step("background-zoom-out", "-", -ZOOM_STEP))
                    .child(track)
                    .child(step("background-zoom-in", "+", ZOOM_STEP))
                    .child(
                        div()
                            .w(px(44.))
                            .flex_none()
                            .text_right()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(chrome.ink_3)
                            .child(percent),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .mt(px(4.))
                    .child(
                        dialog_button("background-reset", "Reset", "0", chrome).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.set_draft(BackgroundFraming::default(), cx)
                            }),
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        dialog_button("background-cancel", "Cancel", "esc", chrome).on_click(
                            cx.listener(|this, _, window, cx| this.close_adjust(false, window, cx)),
                        ),
                    )
                    .child(
                        primary_button("background-apply", "Apply", "↵", chrome).on_click(
                            cx.listener(|this, _, window, cx| this.close_adjust(true, window, cx)),
                        ),
                    ),
            )
    }
}
