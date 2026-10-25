//! Project-group reorder motion. Layout commits at once; only prepaint moves
//! the group, including hitboxes. Scroll offsets are excluded from measurements.
use crate::model;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, ScrollHandle, Window, point, px,
};
use std::time::{Duration, Instant};

const REORDER: Duration = Duration::from_millis(220);

/// A translation from the last painted position to the new layout position.
/// An interrupted reorder starts at its current visual position, not its origin.
struct Position {
    epoch: u64,
    top: f32,
    from: f32,
    started: Instant,
}

impl Position {
    fn offset(&self, now: Instant) -> f32 {
        let progress =
            now.saturating_duration_since(self.started).as_secs_f32() / REORDER.as_secs_f32();
        self.from * (1. - model::ease(progress))
    }

    fn observe(&mut self, top: f32, epoch: u64, now: Instant, reduced: bool) -> f32 {
        if self.epoch != epoch {
            self.from = self.top + self.offset(now) - top;
            self.started = now;
            self.epoch = epoch;
        }
        self.top = top;
        if reduced {
            self.from = 0.;
        }
        self.offset(now)
    }
}

pub struct ProjectMotion {
    id: ElementId,
    child: AnyElement,
    epoch: u64,
    scroll: ScrollHandle,
}

pub fn project_motion(
    id: impl Into<ElementId>,
    child: impl IntoElement,
    epoch: u64,
    scroll: ScrollHandle,
) -> ProjectMotion {
    ProjectMotion {
        id: id.into(),
        child: child.into_any_element(),
        epoch,
        scroll,
    }
}

impl IntoElement for ProjectMotion {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ProjectMotion {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let now = Instant::now();
        let top = (bounds.top() - self.scroll.offset().y - self.scroll.bounds().top()).into();
        let offset = window.with_element_state(global_id.unwrap(), |state: Option<Position>, _| {
            let mut state = state.unwrap_or(Position {
                epoch: self.epoch,
                top,
                from: 0.,
                started: now,
            });
            let offset = state.observe(top, self.epoch, now, cx.reduce_motion());
            (offset, state)
        });
        if offset != 0. {
            window.request_animation_frame();
        }
        window.with_element_offset(point(px(0.), px(offset)), |window| {
            self.child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_starts_at_old_position_and_settles_exactly() {
        let now = Instant::now();
        let mut p = Position {
            epoch: 0,
            top: 200.,
            from: 0.,
            started: now,
        };
        assert_eq!(p.observe(80., 1, now, false), 120.);
        let halfway = p.observe(80., 1, now + REORDER / 2, false);
        assert!(halfway > 0. && halfway < 120.);
        assert_eq!(p.observe(80., 1, now + REORDER, false), 0.);
        // Ordinary layout changes (including close collapse) do not start motion.
        assert_eq!(p.observe(50., 1, now + REORDER, false), 0.);
    }

    #[test]
    fn interrupted_reorder_retargets_without_a_jump() {
        let now = Instant::now();
        let mut p = Position {
            epoch: 0,
            top: 200.,
            from: 0.,
            started: now,
        };
        p.observe(80., 1, now, false);
        let later = now + REORDER / 3;
        let visual = 80. + p.offset(later);
        let offset = p.observe(240., 2, later, false);
        assert!((240. + offset - visual).abs() < 0.001);
        assert_eq!(p.observe(240., 2, later + REORDER, false), 0.);
    }

    #[test]
    fn reduced_motion_settles_and_does_not_resume_an_old_transition() {
        let now = Instant::now();
        let mut p = Position {
            epoch: 0,
            top: 200.,
            from: 0.,
            started: now,
        };
        assert_eq!(p.observe(80., 1, now, true), 0.);
        assert_eq!(p.observe(80., 1, now + REORDER / 2, false), 0.);
        p.observe(200., 2, now + REORDER, false);
        assert_eq!(p.observe(200., 2, now + REORDER, true), 0.);
    }
}
