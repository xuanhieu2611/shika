//! Shika Light and Shika Dark: the hand-tuned chrome table, grouped into the
//! column, popups and buttons, status hues, and terminal-side ink.
use gpui::{Rgba, rgb};

use super::chrome::{Chrome, Frame, StatusColors, diff_colors};
use super::{rgba_of, tint, with_alpha};

/// Shika Light and Shika Dark, hand-tuned for solid and glass.
pub(super) fn shika_chrome(frame: &Frame, dark: bool) -> Chrome {
    let glass = frame.glass;
    // Pick by mode: solid light, solid dark, glass light, glass dark.
    let pick =
        |light: Rgba, dark_solid: Rgba, glass_light: Rgba, glass_dark: Rgba| match (glass, dark) {
            (false, false) => light,
            (false, true) => dark_solid,
            (true, false) => glass_light,
            (true, true) => glass_dark,
        };
    let column = with_alpha(rgba_of(frame.column_base), frame.alpha);
    let hover = pick(
        tint(0x141E0A, 0.045),
        tint(0xFFFFFF, 0.05),
        tint(0x28341E, 0.06),
        tint(0xFFFFFF, 0.06),
    );
    let ink_3 = pick(rgb(0x6C7166), rgb(0x8D9286), rgb(0x5F6459), rgb(0xA6AB9E));
    let term = frame.term_surfaces(rgb(if dark { 0x151714 } else { 0xE7E9E1 }));
    let term_ink = shika_term_ink(dark);
    let popups = shika_popups(dark);
    let status = shika_status(dark, ink_3);
    Chrome {
        column,
        hairline: pick(
            rgb(0xDADDD3),
            rgb(0x2A2D27),
            tint(0x000000, 0.10),
            tint(0xFFFFFF, 0.08),
        ),
        line_2: pick(
            rgb(0xE0E3D9),
            rgb(0x262923),
            tint(0x000000, 0.08),
            tint(0xFFFFFF, 0.07),
        ),
        line_control: pick(
            rgb(0xD3D7CC),
            rgb(0x383C35),
            tint(0x000000, 0.10),
            tint(0xFFFFFF, 0.12),
        ),
        ink_1: if dark { rgb(0xE8EBE3) } else { rgb(0x262824) },
        ink_2: if dark { rgb(0xB6BBAE) } else { rgb(0x4E524A) },
        ink_3,
        ink_4: pick(rgb(0x9EA296), rgb(0x6B7065), rgb(0x868B7E), rgb(0x9A9F92)),
        ink_5: pick(rgb(0xC3C6BC), rgb(0x464A42), rgb(0xADB1A5), rgb(0x5E6359)),
        focus: if dark { rgb(0xE8EBE3) } else { rgb(0x2F332C) },
        card_selected: pick(
            tint(0x141E0A, 0.10),
            tint(0xFFFFFF, 0.18),
            tint(0x28341E, 0.12),
            tint(0xFFFFFF, 0.20),
        ),
        row_selected: if dark { rgb(0x363A33) } else { rgb(0xE8EBE2) },
        overlay: popups.overlay,
        toast_bg: popups.toast_bg,
        toast_fg: popups.toast_fg,
        toast_shadow: popups.toast_shadow,
        sunken: pick(
            rgb(0xE3E6DD),
            rgb(0x30342E),
            tint(0x28341E, 0.08),
            tint(0xFFFFFF, 0.09),
        ),
        raised: if dark { rgb(0x272A25) } else { rgb(0xFFFFFF) },
        raised_hover: if dark { rgb(0x2E322C) } else { rgb(0xFAFBF8) },
        hover,
        control_shadow: tint(0x141E0A, if dark { 0.0 } else { 0.04 }),
        dialog_ring: popups.dialog_ring,
        dialog_shadow: popups.dialog_shadow,
        primary_bg: popups.primary_bg,
        primary_hover: popups.primary_hover,
        primary_fg: popups.primary_fg,
        kbd_inverse: popups.kbd_inverse,
        ready: status.ready,
        asking: status.asking,
        working: status.working,
        waiting: status.waiting,
        failed: status.failed,
        term_header: term.header,
        term_header_alpha: term.header_alpha,
        term_surface: term.surface,
        term_tab: term.tab,
        term_line: term_ink.line,
        term_seg: term_ink.seg,
        term_seg_active: term_ink.seg_active,
        term_hover: term_ink.hover,
        term_fg: term_ink.fg,
        term_white: term_ink.white,
        term_dim: term_ink.dim,
        term_faint: term_ink.faint,
        term_fainter: term_ink.fainter,
        term_empty: term.empty,
        term_empty_alpha: frame.term_alpha,
        term_text: rgba_of(frame.palette.foreground),
        diff_added: diff_colors(&frame.palette, dark, 2),
        diff_removed: diff_colors(&frame.palette, dark, 1),
    }
}

/// Popups, the toast, dialogs, and the primary button. Each is the same in
/// solid and glass.
struct Popups {
    overlay: Rgba,
    toast_bg: Rgba,
    toast_fg: Rgba,
    toast_shadow: Rgba,
    dialog_ring: Rgba,
    dialog_shadow: Rgba,
    primary_bg: Rgba,
    primary_hover: Rgba,
    primary_fg: Rgba,
    kbd_inverse: Rgba,
}

fn shika_popups(dark: bool) -> Popups {
    Popups {
        // Solid in glass too. The window-server blur only reaches the desktop
        // and GPUI has no backdrop blur, so a translucent popup would show the
        // cards and terminal text under it, unblurred.
        overlay: if dark { rgb(0x242722) } else { rgb(0xFAFAF7) },
        toast_bg: if dark { rgb(0x363A33) } else { rgb(0x252823) },
        toast_fg: if dark { rgb(0xF1F4EC) } else { rgb(0xE9ECE3) },
        toast_shadow: tint(0x000000, 0.35),
        dialog_ring: if dark {
            tint(0xFFFFFF, 0.10)
        } else {
            tint(0x000000, 0.20)
        },
        dialog_shadow: if dark {
            tint(0x000000, 0.55)
        } else {
            tint(0x0A0E08, 0.30)
        },
        primary_bg: if dark { rgb(0xE8EBE3) } else { rgb(0x262824) },
        primary_hover: if dark { rgb(0xFFFFFF) } else { rgb(0x353A31) },
        primary_fg: if dark { rgb(0x1A1C19) } else { rgb(0xF3F5EE) },
        kbd_inverse: if dark {
            tint(0x1A1C19, 0.70)
        } else {
            tint(0xF3F5EE, 0.72)
        },
    }
}

/// The status hues on cards, and the failed PR checks mark.
struct StatusSet {
    ready: StatusColors,
    asking: StatusColors,
    working: StatusColors,
    waiting: StatusColors,
    failed: StatusColors,
}

fn shika_status(dark: bool, ink_3: Rgba) -> StatusSet {
    StatusSet {
        ready: if dark {
            StatusColors {
                dot: rgb(0x68CA80),
                text: rgb(0x89DA9B),
            }
        } else {
            StatusColors {
                dot: rgb(0x45B164),
                text: rgb(0x21763C),
            }
        },
        asking: if dark {
            StatusColors {
                dot: rgb(0xF8A13F),
                text: rgb(0xF6B669),
            }
        } else {
            StatusColors {
                dot: rgb(0xED8725),
                text: rgb(0xAB5200),
            }
        },
        working: if dark {
            StatusColors {
                dot: rgb(0x66ABE5),
                text: rgb(0x8CC4F4),
            }
        } else {
            StatusColors {
                dot: rgb(0x4493D0),
                text: rgb(0x266EA4),
            }
        },
        waiting: StatusColors {
            dot: if dark { rgb(0x6B7065) } else { rgb(0xA3A79B) },
            text: ink_3,
        },
        failed: if dark {
            StatusColors {
                dot: rgb(0xF07A6B),
                text: rgb(0xF59B8F),
            }
        } else {
            StatusColors {
                dot: rgb(0xDD4B3E),
                text: rgb(0xB42318),
            }
        },
    }
}

/// Terminal-side lines, washes, and text, the same in solid and glass.
struct TermInk {
    line: Rgba,
    seg: Rgba,
    seg_active: Rgba,
    hover: Rgba,
    fg: Rgba,
    white: Rgba,
    dim: Rgba,
    faint: Rgba,
    fainter: Rgba,
}

fn shika_term_ink(dark: bool) -> TermInk {
    // Terminal-side lines and washes are translucent so they read the same
    // over solid and glass.
    let (line, seg, seg_active, hover) = if dark {
        (
            tint(0xFFFFFF, 0.07),
            tint(0xFFFFFF, 0.06),
            tint(0xFFFFFF, 0.14),
            tint(0xFFFFFF, 0.06),
        )
    } else {
        (
            tint(0x000000, 0.10),
            tint(0x28341E, 0.07),
            tint(0x000000, 0.18),
            tint(0x141E0A, 0.06),
        )
    };
    TermInk {
        line,
        seg,
        seg_active,
        hover,
        fg: if dark { rgb(0xC5CABE) } else { rgb(0x4E524A) },
        white: if dark { rgb(0xF2F5EC) } else { rgb(0x262824) },
        dim: if dark { rgb(0x878C80) } else { rgb(0x6C7166) },
        faint: if dark { rgb(0x757A6E) } else { rgb(0x868B7E) },
        fainter: if dark { rgb(0x5A5F55) } else { rgb(0xA3A79B) },
    }
}
