//! Tests for card row geometry and the Working sparkle.
use super::*;

#[test]
fn exiting_row_keeps_its_gap_until_collapse_and_leaves_no_final_jump() {
    let full = CARD_HEIGHT + CARD_GAP;
    let shrinking = model::EXIT_SHRINK.as_secs_f32() / model::EXIT.as_secs_f32();
    assert_eq!(card_row_geometry(model::card_exit(0.).1), (full, CARD_GAP));
    assert_eq!(
        card_row_geometry(model::card_exit(shrinking).1),
        (full, CARD_GAP)
    );
    let mut previous = full;
    for step in 0..=100 {
        let (_, collapse) = model::card_exit(step as f32 / 100.);
        let (height, gap) = card_row_geometry(collapse);
        assert!(height >= 0. && height <= previous);
        assert!(gap >= 0. && gap <= height);
        assert!((height / full - gap / CARD_GAP).abs() < 1e-6);
        previous = height;
    }
    // Removing this slot does not move a follower or a later project
    // header, and an only-card project returns to its header's height.
    assert_eq!(card_row_geometry(model::card_exit(1.).1), (0., 0.));
}

#[test]
fn every_sparkle_pixel_lights_once_per_cycle() {
    let mut slots = SPARKLE_SLOT.to_vec();
    slots.sort_unstable();
    assert_eq!(slots, (0..25).collect::<Vec<u8>>());
}

#[test]
fn sparkle_pixels_that_start_together_are_never_neighbors() {
    for (a, &sa) in SPARKLE_SLOT.iter().enumerate() {
        for (b, &sb) in SPARKLE_SLOT.iter().enumerate() {
            let apart = (i32::from(sa) - i32::from(sb)).rem_euclid(25);
            let near = (1..=4).contains(&apart.min(25 - apart));
            let (ax, ay, bx, by) = (a % 5, a / 5, b % 5, b / 5);
            let touching = ax.abs_diff(bx) <= 1 && ay.abs_diff(by) <= 1;
            assert!(!(near && touching), "pixels {a} and {b}");
        }
    }
}

#[test]
fn sparkle_keeps_about_eight_pixels_lit_in_steps() {
    // The first frame is the still frame under reduced motion.
    let lit = |t: f32| {
        SPARKLE_SLOT
            .iter()
            .filter(|&&s| sparkle_level(t, s) > 0.)
            .count()
    };
    assert!(lit(0.) >= 6);
    for step in 0..300 {
        let t = step as f32 / 300.;
        assert!((7..=10).contains(&lit(t)), "{} lit at {t}", lit(t));
        for &slot in &SPARKLE_SLOT {
            let level = sparkle_level(t, slot);
            assert!([0., 1. / 3., 2. / 3., 1.].contains(&level));
        }
    }
}
