//! Color math for the chrome: alpha, mixing, compositing, and WCAG
//! luminance and contrast.
use gpui::{Rgba, rgb};
use shika_terminal::Rgb;

pub fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

/// The fill that, painted over `under`, gives the same pixels as `target`
/// painted over nothing, whatever shows through the window. `target` must be
/// at least as opaque as `under`; otherwise `target` is returned as is.
pub fn over_to_match(target: Rgba, under: Rgba) -> Rgba {
    if under.a >= 1.0 || target.a <= under.a {
        return target;
    }
    let a = (target.a - under.a) / (1.0 - under.a);
    let channel = |t: f32, u: f32| ((t * target.a - u * under.a * (1.0 - a)) / a).clamp(0.0, 1.0);
    Rgba {
        r: channel(target.r, under.r),
        g: channel(target.g, under.g),
        b: channel(target.b, under.b),
        a,
    }
}

/// An `rgb` color with an alpha.
pub fn tint(hex: u32, alpha: f32) -> Rgba {
    Rgba {
        a: alpha,
        ..rgb(hex)
    }
}

/// An opaque GPUI color from a terminal color.
pub fn rgba_of(color: Rgb) -> Rgba {
    Rgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: 1.0,
    }
}

/// `from` moved `amount` (0 to 1) of the way to `to`, in sRGB. Opaque.
pub fn mix(from: Rgba, to: Rgba, amount: f32) -> Rgba {
    let t = amount.clamp(0.0, 1.0);
    let channel = |a: f32, b: f32| a + (b - a) * t;
    Rgba {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: 1.0,
    }
}

/// WCAG relative luminance of an opaque color.
pub fn luminance(color: Rgba) -> f32 {
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// Black or white, whichever contrasts more with `background`.
pub fn extreme(background: Rgba) -> Rgba {
    let (black, white) = (rgb(0x000000), rgb(0xFFFFFF));
    if contrast(black, background) > contrast(white, background) {
        black
    } else {
        white
    }
}

/// WCAG contrast ratio between two opaque colors, from 1 to 21.
pub fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `color`, or the first step from it toward `toward` that meets `min`
/// contrast on `background`. If even `toward` falls short, as on a
/// low-contrast theme, it keeps going to black or white, whichever is
/// further from the background; one of them always meets 4.5:1.
pub fn legible(color: Rgba, background: Rgba, toward: Rgba, min: f32) -> Rgba {
    const STEPS: u16 = 20;
    let extreme = extreme(background);
    let path = (0..=STEPS)
        .map(|i| mix(color, toward, f32::from(i) / f32::from(STEPS)))
        .chain((1..=STEPS).map(|i| mix(toward, extreme, f32::from(i) / f32::from(STEPS))));
    for candidate in path {
        if contrast(candidate, background) >= min {
            return candidate;
        }
    }
    extreme
}
