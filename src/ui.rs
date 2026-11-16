//! The window, ported from the upstream interface's `app.tsx` (MIT).
//!
//! Everything is one monospace face at one size, on black, with no borders and no panels:
//! hierarchy comes from colour and weight alone. The page is a top line (what the app is and
//! what the scan is doing), the totals, the dot-matrix meter, then two columns — the category
//! legend with its reclaimable summary, and the detail pane where folders open — and a bottom
//! line carrying the actions that apply right now.
//!
//! The one thing dropped from the port is the terminal: there are no key hints and no key
//! routing. Every action is a control under the pointer, and the signature "press again"
//! confirmation is gone with it, since nothing here deletes anything.

use rune::color::srgb_decode;
use rune::{el, row, text, col, Color, El, TextStyle};

use crate::cats::{self};

/// One terminal cell's worth of text: the only size on screen.
pub const FS: f32 = 12.0;
/// The meter's dot pitch, in points.
pub const CELL: f32 = 7.5;
/// Rows the detail pane's list draws before it stops.

// The table's column widths, in points. The header and every row read these same numbers,
// so a heading can never drift off the column it names.

/// The preview strip under the list: tall enough for verdict, blame and actions.

/// The app's palette, decoded once per view: one graphite ramp for surfaces and text, a single
/// amber accent (the colour of a lit dot-matrix segment) and semantic colours that carry meaning
/// only. Every page reads these, never a literal.
pub(crate) struct Ink {
    /// Primary, secondary and muted text.
    pub(crate) fg: Color,
    pub(crate) muted: Color,
    pub(crate) faint: Color,
    /// Row text in the storage table: between primary and secondary.
    pub(crate) row: Color,
    pub(crate) red: Color,
    pub(crate) green: Color,
    pub(crate) amber: Color,
    /// A row under the pointer, and the row that is picked: the two mouse-only cues.
    pub(crate) hover: Color,
    pub(crate) picked: Color,
    /// Surfaces, darkest to lightest: the sidebar, the page, a card, something raised on a card.
    pub(crate) side: Color,
    pub(crate) bg: Color,
    pub(crate) card: Color,
    pub(crate) raised: Color,
    /// Hairlines: the quiet one between regions, and the one around a control or a picked card.
    pub(crate) line: Color,
    pub(crate) line_strong: Color,
    /// The one accent, the text drawn on it, and a faint wash of it for a selected surface.
    pub(crate) accent: Color,
    pub(crate) on_accent: Color,
    pub(crate) accent_soft: Color,
}

pub(crate) fn rgb(v: [u8; 3]) -> Color {
    Color {
        r: srgb_decode(v[0] as f32 / 255.0),
        g: srgb_decode(v[1] as f32 / 255.0),
        b: srgb_decode(v[2] as f32 / 255.0),
        a: 1.0,
    }
}

pub(crate) fn rgba(v: [u8; 3], a: f32) -> Color {
    let mut c = rgb(v);
    c.a = a;
    c
}

/// The page background as bytes, for the meter's own buffer.
pub(crate) const BG: [u8; 3] = [17, 17, 19];
/// The accent as bytes, for dot bars.
pub(crate) const ACCENT: [u8; 3] = [240, 180, 76];
pub(crate) const WHITE: [u8; 3] = [236, 236, 238];

impl Ink {
    pub(crate) fn new() -> Self {
        Self {
            fg: rgb([236, 236, 238]),
            muted: rgb([160, 160, 166]),
            faint: rgb([112, 112, 120]),
            red: rgb([240, 110, 104]),
            green: rgb([110, 208, 140]),
            amber: rgb(ACCENT),
            row: rgb([196, 196, 202]),
            hover: rgba([255, 255, 255], 0.045),
            picked: rgba([255, 255, 255], 0.075),
            side: rgb([12, 12, 14]),
            bg: rgb(BG),
            card: rgb([24, 24, 27]),
            raised: rgb([33, 33, 37]),
            line: rgba([255, 255, 255], 0.07),
            line_strong: rgba([255, 255, 255], 0.12),
            accent: rgb(ACCENT),
            on_accent: rgb([24, 18, 8]),
            accent_soft: rgba(ACCENT, 0.12),
        }
    }

    /// rune-kit's tokens derived from the same three colours, with this app's radii and type,
    /// so kit buttons, fields, menus and dialogs sit in the same system as the hand-drawn parts.
    pub(crate) fn kit(&self) -> rune_kit::Kit {
        let mut k = rune_kit::Kit::from_palette_focus(
            rune_kit::PaletteSeed { bg: self.bg, fg: self.fg, accent: self.accent },
            rune_kit::PaletteFocus::Accent,
        );
        k.radius.sm = 6.0;
        k.radius.md = R_CTRL;
        k.radius.lg = R_CARD;
        k.c.text_muted = self.muted;
        k.c.text_faint = self.faint;
        // Submit is the page's one primary action: the accent, with dark ink.
        k.c.bg_icon_button_contrast = self.accent;
        k.c.submit_fg = self.on_accent;
        k.c.submit_icon = self.on_accent;
        k.c.submit_highlight = Color::WHITE.alpha(0.25);
        k
    }
}

// ---- the system's scale ----------------------------------------------------
// Space is a 4 pt grid; radii nest (a control inside a card is the card's radius minus its
// padding); type is one interface face for words and a mono for anything you would compare.

pub(crate) const S1: f32 = 4.0;
pub(crate) const S2: f32 = 8.0;
pub(crate) const S3: f32 = 12.0;
pub(crate) const S4: f32 = 16.0;
pub(crate) const S5: f32 = 20.0;
pub(crate) const S6: f32 = 24.0;
pub(crate) const S8: f32 = 32.0;
pub(crate) const R_SM: f32 = 6.0;
pub(crate) const R_CTRL: f32 = 8.0;
pub(crate) const R_CARD: f32 = 12.0;

/// The interface face (Inter) at a size and weight.
pub(crate) fn sans(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight)
}

/// A line of interface text.
pub(crate) fn label(content: impl Into<String>, size: f32, weight: f32, c: Color) -> El {
    text(content).font(sans(size, weight)).color(c)
}

/// A line of data: figures, paths, branches, sizes.
pub(crate) fn data(content: impl Into<String>, size: f32, c: Color) -> El {
    text(content).font(mono(size, 400.0)).color(c)
}

/// A card: the one surface step above the page, with a hairline.
pub(crate) fn card(ink: &Ink) -> El {
    col().fill(ink.card).radius(R_CARD).border(1.0, ink.line)
}

/// A small quiet tag: a stack name, a kind. Mono, hairline, no fill.
pub(crate) fn tag(content: impl Into<String>, ink: &Ink) -> El {
    row()
        .h(20.0)
        .px(6.0)
        .radius(4.0)
        .items_center()
        .border(1.0, ink.line_strong)
        .shrink()
        .child(data(content, 11.0, ink.muted).truncate())
}

/// A keycap: how a shortcut shows wherever one is named.
pub(crate) fn keycap(key: impl Into<String>, ink: &Ink) -> El {
    row()
        .h(18.0)
        .min_w(18.0)
        .px(5.0)
        .radius(4.0)
        .items_center()
        .justify_center()
        .fill(ink.raised)
        .border(1.0, ink.line)
        .child(data(key, 10.5, ink.muted))
}

/// A thin proportional bar on a track: the kind of bar everything outside the storage page uses.
pub(crate) fn bar(fraction: f32, color: Color, ink: &Ink) -> El {
    let f = fraction.clamp(0.0, 1.0);
    row()
        .h(4.0)
        .w_full()
        .radius(2.0)
        .fill(ink.raised)
        .child(el().h_full().w_frac(f).min_w(if f > 0.0 { 2.0 } else { 0.0 }).radius(2.0).fill(color))
}

pub(crate) fn color_of(rgb8: cats::Rgb) -> Color {
    rgb(rgb8)
}

pub(crate) fn tint(rgb8: cats::Rgb, alpha: f32) -> Color {
    let mut c = color_of(rgb8);
    c.a = alpha.clamp(0.0, 1.0);
    c
}

pub(crate) fn mono(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(size, weight).mono()
}

/// Width of n monospace characters at the one size on screen.
pub(crate) fn ch(n: f32) -> f32 {
    FS * 0.6 * n
}

// ---- the dot sparkline ----------------------------------------------------

fn dot_hash(a: usize, b: usize) -> f32 {
    let mut h = (a as u32)
        .wrapping_add(374_761_393)
        .wrapping_mul(668_265_263)
        ^ (b as u32)
            .wrapping_add(1_274_126_177)
            .wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(3_266_489_917);
    ((h ^ (h >> 16)) as f32) / 4_294_967_296.0
}

/// A small dot-matrix bar, the same pixel language as the big meter: lit dots in the category
/// colour with the odd white one, unlit dots for the rest.
pub(crate) fn dots(fraction: f32, color: cats::Rgb, count: usize, seed: usize, dim: bool) -> El {
    let dot = (FS * 0.26).max(2.0).round();
    let gap = (FS * 0.14).max(1.0).round();
    let lit = if fraction > 0.0 {
        ((fraction.clamp(0.0, 1.0) * count as f32).round() as usize).max(1)
    } else {
        0
    };
    row()
        .gap(gap)
        .items_center()
        .children((0..count).map(move |i| {
            let on = i < lit;
            let k = dot_hash(i + seed * 31, seed);
            let fill = if !on {
                rgb([28, 28, 28])
            } else if k > 0.9 && !dim {
                Color::WHITE
            } else {
                tint(color, if dim { 0.35 } else { 0.55 + k * 0.45 })
            };
            el().size(dot, dot).radius(dot / 2.0).fill(fill)
        }))
}

// ---- top line -------------------------------------------------------------

pub(crate) fn clip_path(path: &str, max_chars: usize) -> String {
    // Long absolute paths keep their tail, which is the part that says what is being read.
    let count = path.chars().count();
    if count <= max_chars {
        return path.to_string();
    }
    let tail: String = path.chars().skip(count - max_chars + 1).collect();
    format!("…{tail}")
}
