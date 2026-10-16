//! A thin vertical scrollbar over a scrolling list, drawn only while the list
//! is longer than its viewport, so a cut-off row is never the only hint that
//! there is more. GPUI has no scrollbar element, so this one reads the list's
//! `ScrollHandle` while painting, after the list has laid out in that frame.

use gpui::{
    Bounds, DispatchPhase, HitboxBehavior, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Rgba, ScrollHandle, Styled, div, point, px, size,
};
use std::{cell::Cell, rc::Rc};

/// Where the pointer held the thumb, measured from the thumb's top, while a
/// drag is in progress. It outlives a frame, so its owner keeps it.
pub type Grab = Rc<Cell<Option<f32>>>;

/// The strip on the list's right edge that takes the pointer.
const STRIP: f32 = 10.;
const THUMB: f32 = 4.;
/// Inset from the list's right edge, inside the rows' 10px padding.
const INSET: f32 = 3.;
const MIN_THUMB: f32 = 24.;

/// The thumb's top and height inside a track, for a viewport `viewport` tall
/// showing content `max` taller, scrolled `scrolled` down. None when it fits.
fn thumb(track: f32, viewport: f32, max: f32, scrolled: f32) -> Option<(f32, f32)> {
    if max <= 0.5 || viewport <= 0. || track <= 0. {
        return None;
    }
    let height = (track * viewport / (viewport + max)).clamp(MIN_THUMB.min(track), track);
    let progress = (scrolled / max).clamp(0., 1.);
    Some(((track - height) * progress, height))
}

/// How far down to scroll so the thumb's top sits at `top` in the track.
fn scrolled_for(top: f32, track: f32, height: f32, max: f32) -> f32 {
    let room = track - height;
    if room <= 0. {
        return 0.;
    }
    (top / room).clamp(0., 1.) * max
}

/// Fills its parent, which must be `relative` and the same size as the list.
/// Dragging the thumb scrolls; a press elsewhere on the strip jumps there.
pub fn vertical(handle: &ScrollHandle, grab: &Grab, color: Rgba, active: Rgba) -> impl IntoElement {
    let handle = handle.clone();
    let grab = grab.clone();
    div().absolute().inset_0().child(
        gpui::canvas(
            |bounds, window, _| {
                let strip = Bounds::new(
                    point(bounds.right() - px(STRIP), bounds.top()),
                    size(px(STRIP), bounds.size.height),
                );
                window.insert_hitbox(strip, HitboxBehavior::Normal)
            },
            move |bounds, hitbox, window, _| {
                let track = f32::from(bounds.size.height);
                let viewport = f32::from(handle.bounds().size.height);
                let max = f32::from(handle.max_offset().y);
                let scrolled = -f32::from(handle.offset().y);
                let Some((top, height)) = thumb(track, viewport, max, scrolled) else {
                    grab.set(None);
                    return;
                };
                let held = grab.get().is_some();
                let fill = if held || hitbox.is_hovered(window) {
                    active
                } else {
                    color
                };
                window.paint_quad(
                    gpui::fill(
                        Bounds::new(
                            point(bounds.right() - px(INSET + THUMB), bounds.top() + px(top)),
                            size(px(THUMB), px(height)),
                        ),
                        fill,
                    )
                    .corner_radii(px(THUMB / 2.)),
                );
                let origin = f32::from(bounds.top());
                let scroll_to = {
                    let handle = handle.clone();
                    move |thumb_top: f32| {
                        let y = scrolled_for(thumb_top, track, height, max);
                        handle.set_offset(point(handle.offset().x, px(-y)));
                    }
                };
                // Capture phase, so the row under the strip does not also
                // take the press as a click.
                {
                    let grab = grab.clone();
                    let hitbox = hitbox.clone();
                    let scroll_to = scroll_to.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture
                            || event.button != MouseButton::Left
                            || !hitbox.is_hovered(window)
                        {
                            return;
                        }
                        let y = f32::from(event.position.y) - origin;
                        let offset = if (top..top + height).contains(&y) {
                            y - top
                        } else {
                            scroll_to(y - height / 2.);
                            height / 2.
                        };
                        grab.set(Some(offset));
                        cx.stop_propagation();
                        window.refresh();
                    });
                }
                {
                    let grab = grab.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        let Some(offset) = grab.get() else {
                            return;
                        };
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        if !event.dragging() {
                            grab.set(None);
                        } else {
                            scroll_to(f32::from(event.position.y) - origin - offset);
                            cx.stop_propagation();
                        }
                        window.refresh();
                    });
                }
                window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && grab.take().is_some() {
                        cx.stop_propagation();
                        window.refresh();
                    }
                });
            },
        )
        .size_full(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_shows_only_when_the_list_overflows_and_tracks_the_scroll() {
        assert_eq!(thumb(400., 400., 0., 0.), None);
        assert_eq!(thumb(400., 400., 0.4, 0.), None);
        // A third more content: the thumb is three quarters of the track.
        assert_eq!(thumb(400., 400., 133.3333, 0.).map(|t| t.0), Some(0.));
        let (_, height) = thumb(400., 400., 400. / 3., 0.).unwrap();
        assert!((height - 300.).abs() < 0.01);
        assert_eq!(thumb(400., 400., 400., 400.), Some((200., 200.)));
        assert_eq!(thumb(400., 400., 400., 200.), Some((100., 200.)));
        // Long content keeps a grabbable thumb, and overscroll stays inside.
        assert_eq!(thumb(400., 400., 100_000., 0.).unwrap().1, MIN_THUMB);
        assert_eq!(thumb(400., 400., 400., 900.), Some((200., 200.)));
        assert_eq!(thumb(400., 400., 400., -50.), Some((0., 200.)));
    }

    #[test]
    fn dragging_the_thumb_maps_back_to_the_scroll_offset() {
        assert_eq!(scrolled_for(0., 400., 200., 400.), 0.);
        assert_eq!(scrolled_for(100., 400., 200., 400.), 200.);
        assert_eq!(scrolled_for(200., 400., 200., 400.), 400.);
        assert_eq!(scrolled_for(-30., 400., 200., 400.), 0.);
        assert_eq!(scrolled_for(900., 400., 200., 400.), 400.);
        assert_eq!(scrolled_for(10., 400., 400., 400.), 0.);
        for scrolled in [0., 120., 400.] {
            let (top, height) = thumb(400., 400., 400., scrolled).unwrap();
            assert!((scrolled_for(top, 400., height, 400.) - scrolled).abs() < 0.001);
        }
    }
}
