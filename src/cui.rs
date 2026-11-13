//! The CUI design system, ported to Rune: theme tokens from `cui_theme.c` (dark mode,
//! green accent) and the shared component builders — cards, readouts, badges, bar lists,
//! meters and the small chrome every page reuses.
//!
//! Rules carried over from CUI's design principles: most of the screen is neutral; the
//! accent is spent with intent; status colors mean status; cards rest on a hairline border
//! with no shadow; two weights; sentence case; tabular figures in the mono face.

use rune::color::oklch;
use rune::{text, Align, Color, Cursor, El, TextStyle};

/// Control heights: CUI's control_sm / md / lg.
pub const H_SM: f32 = 30.0;
pub const H_MD: f32 = 36.0;

/// CUI's radius ladder: controls 10, panels 14, surfaces 18.
pub const R_CONTROL: f32 = 10.0;
pub const R_SURFACE: f32 = 18.0;

/// The CUI dark theme. Greys are pure OKLab lightness steps (chroma 0), exactly the
/// levels `cui__theme_build` uses; depth comes from lightness, page darkest.
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub raised: Color,
    pub fg: Color,
    pub text2: Color,
    pub text3: Color,
    pub border: Color,
    pub border_subtle: Color,
    pub track: Color,
    pub control_on: Color,
    pub control_glyph: Color,
    pub accent: Color,
    pub accent_strong: Color,
    pub success: Color,
    pub info: Color,
    pub warning: Color,
    pub danger: Color,
    /// The series palette: blue, orange, aqua, violet, yellow, magenta at one shared
    /// lightness, so no series shouts over another.
    pub series: [Color; 6],
}

pub fn theme() -> Theme {
    Theme {
        bg: oklch(0.18, 0.0, 0.0),
        surface: oklch(0.21, 0.0, 0.0),
        raised: oklch(0.24, 0.0, 0.0),
        fg: oklch(0.96, 0.0, 0.0),
        text2: oklch(0.77, 0.0, 0.0),
        text3: oklch(0.64, 0.0, 0.0),
        border: oklch(0.29, 0.0, 0.0),
        border_subtle: oklch(0.25, 0.0, 0.0),
        track: oklch(0.33, 0.0, 0.0),
        control_on: oklch(0.93, 0.0, 0.0),
        control_glyph: oklch(0.19, 0.0, 0.0),
        accent: rune::color::hex("#53dca6").unwrap(),
        accent_strong: rune::color::hex("#93efd0").unwrap(),
        success: oklch(0.79, 0.14, 162.0),
        info: oklch(0.79, 0.095, 236.0),
        warning: oklch(0.83, 0.12, 78.0),
        danger: oklch(0.70, 0.16, 24.0),
        series: [
            rune::color::hex("#73aff1").unwrap(),
            rune::color::hex("#e7936c").unwrap(),
            rune::color::hex("#5ac299").unwrap(),
            rune::color::hex("#a3a0f1").unwrap(),
            rune::color::hex("#d1a24f").unwrap(),
            rune::color::hex("#e18cb5").unwrap(),
        ],
    }
}

impl Theme {
    /// Hover wash: the foreground at 5% alpha, as CUI's CUI_TOKEN_HOVER.
    pub fn hover(&self) -> Color {
        self.fg.alpha(0.05)
    }
    /// Status color for a project's recency, always paired with a label in the UI.
    pub fn status(&self, days: Option<u64>) -> Color {
        match days {
            None => self.text3,
            Some(d) if d <= 30 => self.success,
            Some(d) if d <= 120 => self.info,
            _ => self.danger,
        }
    }
}

// ---- text helpers ----

/// Body text.
/// Small secondary text under a heading.
pub fn caption(s: impl Into<String>, c: Color) -> El {
    text(s.into()).font(TextStyle::new(12.0, 400.0)).color(c)
}
pub fn note(s: impl Into<String>, c: Color) -> El {
    text(s.into()).font(TextStyle::new(11.5, 400.0)).color(c)
}
/// Section label: small, medium weight, sentence case.
pub fn section(s: impl Into<String>, c: Color) -> El {
    text(s.into()).font(TextStyle::new(12.0, 500.0)).color(c)
}
/// Mono figures: tabular by nature, one format everywhere.
pub fn mono(size: f32) -> TextStyle {
    TextStyle::new(size, 400.0).mono()
}
pub fn mono_text(s: impl Into<String>, size: f32, c: Color) -> El {
    text(s.into()).font(mono(size)).color(c)
}

// ---- components ----

/// A card: surface + hairline border, no shadow, 24 pt inside. Groups real boundaries only.
pub fn card(t: &Theme, title: &str, desc: &str, content: El) -> El {
    col_card(t)
        .child(section(title, t.fg))
        .child(caption(desc, t.text3))
        .child(content)
}

/// An empty card body for callers that compose their own header.
pub fn col_card(t: &Theme) -> El {
    rune::col()
        .w_full()
        .fill(t.surface)
        .border(1.0, t.border_subtle)
        .radius(R_SURFACE)
        .px(20.0)
        .py(18.0)
        .gap(6.0)
}

/// A primary button: flat control_on color, no gloss, no shadow.
pub fn button_primary(
    t: &Theme,
    label: &str,
    key: &str,
    on_press: impl Fn(&mut crate::State) + 'static,
) -> El {
    rune::el()
        .key(key.to_string())
        .role("button")
        .label(label.to_string())
        .cursor(Cursor::Pointer)
        .h(H_MD)
        .px(14.0)
        .radius(R_CONTROL)
        .fill(t.control_on)
        .items_center()
        .justify_center()
        .on_press(on_press)
        .child(
            text(label.to_string())
                .font(TextStyle::new(12.5, 500.0))
                .color(t.control_glyph),
        )
}

/// A readout: one figure as an instrument — small label, the value in mono with its unit,
/// a line of context. `size` picks 17 / 28 / 44 like CUI's SM / MD / LG.
pub fn readout(
    t: &Theme,
    label: &str,
    value: &str,
    unit: Option<&str>,
    detail: Option<String>,
    size: f32,
    accent: bool,
) -> El {
    let value_color = if accent { t.accent } else { t.fg };
    let mut row = rune::row().items_end().gap(4.0).child(
        text(value.to_string())
            .font(TextStyle::new(size, 500.0).mono())
            .color(value_color),
    );
    if let Some(u) = unit {
        row = row.child(
            text(u.to_string())
                .font(TextStyle::new(size * 0.45, 500.0).mono())
                .color(value_color.alpha(0.75)),
        );
    }
    let mut col = rune::col().gap(2.0).child(
        text(label.to_string())
            .font(TextStyle::new(11.0, 500.0))
            .color(t.text3),
    );
    col = col.child(row);
    if let Some(d) = detail {
        col = col.child(note(d, t.text3));
    }
    col
}

/// A status badge: a dot means a state, always with its label next to it.

/// A small round status dot.
pub fn dot(color: Color, size: f32) -> El {
    rune::el().size(size, size).radius(size / 2.0).fill(color)
}

/// A meter: fully-round track with a fill; `lead` gets the accent, the rest stay neutral.

/// A bar list row: label, growing bar keyed by value, right-aligned mono figure.
/// Ranked by value; rows keyed by label so a value change animates the bar, not the order.
pub fn bar_list_row(
    t: &Theme,
    label: &str,
    frac: f32,
    value: &str,
    lead: bool,
    label_w: f32,
) -> El {
    rune::row()
        .w_full()
        .h(H_SM)
        .items_center()
        .gap(12.0)
        .child(
            text(label.to_string())
                .font(TextStyle::new(12.5, 400.0))
                .color(t.fg)
                .w(label_w),
        )
        .child(
            rune::el().h(8.0).grow(1.0).radius(4.0).fill(t.track).child(
                rune::el()
                    .h(8.0)
                    .w_frac(frac.clamp(0.02, 1.0))
                    .radius(4.0)
                    .fill(if lead { t.accent } else { t.text3 }),
            ),
        )
        .child(mono_text(value, 11.5, t.text3).w(90.0).align(Align::End))
}
