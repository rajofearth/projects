//! CUI chart components ported to Rune: line and bar charts with annotations, donut,
//! sparkline, scatter and bubble, activity rings, radar, calendar heatmap, weekday-hour
//! heatmap and the neutral treemap. Drawing follows CUI's chart rules: faint dashed
//! gridlines over a solid zero line, small muted axis labels, the accent spent with intent.

use rune::TextStyle;
use rune::{canvas, col, el, row, text, Align, Color, Cursor, El, Path, Shape};

use crate::cui::{self, note, Theme};
use crate::model::{fmt, gb};

/// A dashed straight line, drawn as short strokes so it reads as a hairline gridline.
fn dashed(mut shape: Shape, x0: f32, y0: f32, x1: f32, y1: f32, color: Color) -> Shape {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= 0.0 {
        return shape;
    }
    let (ux, uy) = (dx / len, dy / len);
    let (dash, gap) = (3.0, 4.0);
    let mut t = 0.0;
    while t < len {
        let end = (t + dash).min(len);
        shape = shape.line(
            [x0 + ux * t, y0 + uy * t],
            [x0 + ux * end, y0 + uy * end],
            1.0,
            rune::LineCap::Butt,
            color,
        );
        t += dash + gap;
    }
    shape
}

/// A rounded rectangle path (four quads), for canvas-drawn tiles and bubbles.
fn rrect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.min(w / 2.0).min(h / 2.0);
    Path::new()
        .move_to([x + r, y])
        .line_to([x + w - r, y])
        .quad_to([x + w, y], [x + w, y + r])
        .line_to([x + w, y + h - r])
        .quad_to([x + w, y + h], [x + w - r, y + h])
        .line_to([x + r, y + h])
        .quad_to([x, y + h], [x, y + h - r])
        .line_to([x, y + r])
        .quad_to([x, y], [x + r, y])
        .close()
}

/// Hover plumbing shared by every chart: entering names the item in the inspector bar,
/// pressing pins it there.
pub fn hover_name(name: String) -> impl Fn(&mut crate::State, bool, &mut rune::Tx) + 'static {
    move |s: &mut crate::State, entered, _tx| {
        if entered {
            s.hover = Some(name.clone());
        } else if s.hover.as_deref() == Some(name.as_str()) {
            s.hover = None;
        }
    }
}

pub fn pin_name(name: String) -> impl Fn(&mut crate::State) + 'static {
    move |s: &mut crate::State| {
        if s.pinned.as_deref() == Some(name.as_str()) {
            s.pinned = None;
        } else {
            s.pinned = Some(name.clone());
        }
    }
}

/// A chart annotation, matching CUI's three kinds.
pub enum Annotation {
    /// A dashed horizontal line at a y value, labelled in the right gutter.
    Threshold { y: f64, label: String, accent: bool },
    /// A dot at a point, labelled beside it.
    Point { index: usize, y: f64, label: String },
}

/// Nice axis ticks for a 0..max range: 1/2/5 steps.
pub fn nice_ticks(max: f64, target: usize) -> Vec<f64> {
    if max <= 0.0 {
        return vec![];
    }
    let step = max / target as f64;
    let mag = 10f64.powf(step.log10().floor());
    let norm = step / mag;
    let nice = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    let step = nice * mag;
    let mut out = Vec::new();
    let mut v = step;
    while v < max * 1.0001 {
        out.push(v);
        v += step;
    }
    out
}

/// Compact value label: 1.2k / 3.4M style.
pub fn compact(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else {
        format!("{v:.0}")
    }
}

/// A line chart: dashed gridlines, solid zero line, muted mono axis labels, the first
/// series filled when there are several, and CUI annotations on top.
pub fn line_chart(
    t: &Theme,
    series: &[(String, Vec<f64>, Color)],
    labels: &[String],
    w: f32,
    h: f32,
    annotations: &[Annotation],
    hover: Option<&str>,
) -> El {
    const L: f32 = 46.0;
    const R: f32 = 14.0;
    const TOP: f32 = 14.0;
    const BOT: f32 = 22.0;
    let iw = (w - L - R).max(10.0);
    let ih = (h - TOP - BOT).max(10.0);
    let n = series.first().map(|s| s.1.len()).unwrap_or(0).max(2);
    let max = series
        .iter()
        .flat_map(|s| s.1.iter())
        .cloned()
        .fold(1.0f64, f64::max);

    let x_of = |i: usize| L + iw * i as f32 / (n - 1).max(1) as f32;
    let y_of = |v: f64| TOP as f32 + ih - (v / max * ih as f64) as f32;

    let mut shape = Shape::new();
    for tick in nice_ticks(max, 4) {
        let y = y_of(tick);
        shape = dashed(shape, L, y, L + iw, y, t.border_subtle);
    }
    shape = shape.line(
        [L, TOP + ih],
        [L + iw, TOP + ih],
        1.0,
        rune::LineCap::Butt,
        t.border,
    );
    shape = shape.line([L, TOP], [L, TOP + ih], 1.0, rune::LineCap::Butt, t.border);

    // Annotations under the data.
    for a in annotations {
        match a {
            Annotation::Threshold { y, accent, .. } => {
                let yy = y_of(*y);
                shape = dashed(
                    shape,
                    L,
                    yy,
                    L + iw,
                    yy,
                    if *accent { t.accent } else { t.fg.alpha(0.5) },
                );
            }
            Annotation::Point { .. } => {}
        }
    }

    for (si, (_, values, color)) in series.iter().enumerate() {
        let pts: Vec<[f32; 2]> = values
            .iter()
            .enumerate()
            .map(|(i, &v)| [x_of(i), y_of(v)])
            .collect();
        if si == 0 && series.len() > 1 {
            let mut area = pts.clone();
            area.push([x_of(values.len().saturating_sub(1)), TOP + ih]);
            area.push([L, TOP + ih]);
            shape = shape.fill(area, color.alpha(0.18));
        }
        shape = shape.stroke(pts, 1.75, *color);
    }

    // Annotation labels and point dots over the data.
    let mut labels_out: Vec<El> = Vec::new();
    for a in annotations {
        match a {
            Annotation::Threshold { y, label, accent } => {
                let yy = y_of(*y);
                labels_out.push(
                    text(label.clone())
                        .font(TextStyle::new(9.0, 500.0).mono())
                        .color(if *accent { t.accent } else { t.text2 })
                        .absolute(L + iw + 2.0, yy - 6.0),
                );
            }
            Annotation::Point { index, y, label } => {
                let (px, py) = (x_of(*index), y_of(*y));
                shape = shape.fill(circle_points(px, py, 3.0), t.fg);
                labels_out.push(
                    text(label.clone())
                        .font(TextStyle::new(9.0, 500.0).mono())
                        .color(t.fg)
                        .absolute(px + 6.0, py - 6.0),
                );
            }
        }
    }

    // Axis labels.
    let mut parts: Vec<El> = vec![el()
        .key("line-chart")
        .w_full()
        .h(h)
        .child(canvas(shape).size(w, h))];
    for tick in nice_ticks(max, 4) {
        parts.push(
            text(compact(tick))
                .font(TextStyle::new(9.0, 400.0).mono())
                .color(t.text3)
                .absolute(0.0, y_of(tick) - 5.0)
                .w(L - 6.0)
                .align(Align::End),
        );
    }
    let skip = (labels.len() as f32 / (iw / 56.0).max(1.0)).ceil().max(1.0) as usize;
    for (i, l) in labels.iter().enumerate() {
        if i % skip != 0 && i != labels.len() - 1 {
            continue;
        }
        parts.push(
            text(l.clone())
                .font(TextStyle::new(9.0, 400.0).mono())
                .color(t.text3)
                .absolute(x_of(i) - 30.0, TOP + ih + 4.0)
                .w(60.0)
                .align(Align::Center),
        );
    }
    parts.extend(labels_out);
    if let Some(name) = hover {
        parts.push(
            text(name.to_string())
                .font(TextStyle::new(10.0, 500.0))
                .color(t.fg)
                .absolute(L + 4.0, TOP + 2.0),
        );
    }
    el().w_full().h(h).children(parts)
}

fn circle_points(cx: f32, cy: f32, r: f32) -> Vec<[f32; 2]> {
    (0..24)
        .map(|i| {
            let a = i as f32 / 24.0 * std::f32::consts::TAU;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

/// A bar chart: slim bars with rounded tops, a soft band behind the hovered column and a
/// compact chip over it with the value.
pub fn bar_chart(
    t: &Theme,
    values: &[f64],
    labels: &[String],
    w: f32,
    h: f32,
    color: Color,
    annotations: &[Annotation],
    hover: Option<&str>,
) -> El {
    const L: f32 = 40.0;
    const R: f32 = 10.0;
    const TOP: f32 = 16.0;
    const BOT: f32 = 20.0;
    let iw = (w - L - R).max(10.0);
    let ih = (h - TOP - BOT).max(10.0);
    let n = values.len().max(1);
    let max = values.iter().cloned().fold(1.0f64, f64::max);
    let slot = iw / n as f32;
    let bar_w = (slot * 0.55).clamp(3.0, 18.0);

    let x_of = |i: usize| L + slot * i as f32 + slot / 2.0;
    let y_of = |v: f64| TOP + ih - (v / max * ih as f64) as f32;

    let mut shape = Shape::new();
    for tick in nice_ticks(max, 3) {
        shape = dashed(shape, L, y_of(tick), L + iw, y_of(tick), t.border_subtle);
    }
    shape = shape.line(
        [L, TOP + ih],
        [L + iw, TOP + ih],
        1.0,
        rune::LineCap::Butt,
        t.border,
    );
    for a in annotations {
        if let Annotation::Threshold { y, accent, .. } = a {
            shape = dashed(
                shape,
                L,
                y_of(*y),
                L + iw,
                y_of(*y),
                if *accent { t.accent } else { t.fg.alpha(0.5) },
            );
        }
    }
    for (i, &v) in values.iter().enumerate() {
        let y = y_of(v);
        shape = shape.fill_path(
            &rrect_path(x_of(i) - bar_w / 2.0, y, bar_w, TOP + ih - y, 2.5),
            color.alpha(0.9),
        );
    }
    for a in annotations {
        if let Annotation::Point { index, y, .. } = a {
            shape = shape.fill(circle_points(x_of(*index), y_of(*y), 3.0), t.fg);
        }
    }

    let mut parts: Vec<El> = vec![el()
        .key("bar-chart")
        .w_full()
        .h(h)
        .child(canvas(shape).size(w, h))];
    for tick in nice_ticks(max, 3) {
        parts.push(
            text(compact(tick))
                .font(TextStyle::new(9.0, 400.0).mono())
                .color(t.text3)
                .absolute(0.0, y_of(tick) - 5.0)
                .w(L - 6.0)
                .align(Align::End),
        );
    }
    let skip = (n as f32 / (iw / 40.0).max(1.0)).ceil().max(1.0) as usize;
    for (i, l) in labels.iter().enumerate() {
        if i % skip != 0 && i != n - 1 {
            continue;
        }
        parts.push(
            text(l.clone())
                .font(TextStyle::new(9.0, 400.0).mono())
                .color(t.text3)
                .absolute(x_of(i) - 24.0, TOP + ih + 4.0)
                .w(48.0)
                .align(Align::Center),
        );
    }
    // Annotation labels.
    for a in annotations {
        match a {
            Annotation::Threshold { y, label, accent } => parts.push(
                text(label.clone())
                    .font(TextStyle::new(9.0, 500.0).mono())
                    .color(if *accent { t.accent } else { t.text2 })
                    .absolute(L + iw + 2.0, y_of(*y) - 6.0),
            ),
            Annotation::Point { index, y, label } => parts.push(
                text(label.clone())
                    .font(TextStyle::new(9.0, 500.0).mono())
                    .color(t.fg)
                    .absolute(x_of(*index) + 6.0, y_of(*y) - 6.0),
            ),
        }
    }
    // Hover bands: one transparent strip per column, chip above the hovered bar.
    for (i, &v) in values.iter().enumerate() {
        let name = labels.get(i).cloned().unwrap_or_default();
        let on = hover == Some(name.as_str());
        if on {
            parts.push(
                el().absolute(x_of(i) - slot / 2.0, TOP)
                    .w(slot)
                    .h(ih)
                    .fill(t.fg.alpha(0.05)),
            );
            parts.push(
                text(gb_value(v))
                    .font(TextStyle::new(9.5, 500.0).mono())
                    .color(t.fg)
                    .absolute(x_of(i) - 30.0, y_of(v) - 18.0)
                    .w(60.0)
                    .align(Align::Center),
            );
        }
        parts.push(
            el().key(format!("barcol/{name}"))
                .absolute(x_of(i) - slot / 2.0, TOP)
                .w(slot)
                .h(ih)
                .cursor(Cursor::Pointer)
                .on_hover_change(hover_name(name.clone())),
        );
    }
    el().w_full().h(h).children(parts)
}

fn gb_value(v: f64) -> String {
    if v >= 1.0 {
        format!("{v:.1} GiB")
    } else {
        format!("{:.0} MiB", v * 1024.0)
    }
}

/// A thin donut: segments parted by real gaps, the leading share in the middle, and a
/// legend whose names carry a thin bar sized against the largest share.
pub fn donut(t: &Theme, segments: &[(String, u64, Color)], size: f32, hover: Option<&str>) -> El {
    let total: u64 = segments.iter().map(|s| s.1).sum();
    let (cx, cy) = (size / 2.0, size / 2.0);
    let r = size / 2.0 - 6.0;
    let ir = r * 0.62;
    let mut shape = Shape::new();
    let mut ang = -std::f32::consts::FRAC_PI_2;
    let gap = if segments.iter().filter(|s| s.1 > 0).count() > 1 {
        0.035
    } else {
        0.0
    };
    let mut lead: Option<(String, u64, Color)> = None;
    for (name, n, color) in segments {
        if *n == 0 {
            continue;
        }
        let span = (*n as f32 / total.max(1) as f32) * std::f32::consts::TAU;
        let a1 = ang + (span - gap).max(0.01);
        let on = hover == Some(name.as_str());
        shape = crate::charts::ring(
            shape,
            cx,
            cy,
            ir,
            r,
            ang,
            a1,
            if on {
                color.alpha(1.0)
            } else {
                color.alpha(0.85)
            },
        );
        let is_lead = match &lead {
            Some((_, ln, _)) => *n > *ln,
            None => true,
        };
        if is_lead {
            lead = Some((name.clone(), *n, *color));
        }
        ang += span;
    }
    let mut out = col().gap(10.0).child(
        el().size(size, size)
            .self_center()
            .child(canvas(shape).size(size, size))
            .child(
                col()
                    .absolute(cx - 50.0, cy - 16.0)
                    .w(100.0)
                    .items_center()
                    .gap(1.0)
                    .child(match &lead {
                        Some((_, n, _)) => text(fmt(*n))
                            .font(TextStyle::new(24.0, 500.0).mono())
                            .color(t.fg),
                        None => text("0")
                            .font(TextStyle::new(24.0, 500.0).mono())
                            .color(t.text3),
                    })
                    .child(match &lead {
                        Some((name, _, _)) => text(name.clone())
                            .font(TextStyle::new(9.5, 400.0))
                            .color(t.text3),
                        None => text("nothing yet")
                            .font(TextStyle::new(9.5, 400.0))
                            .color(t.text3),
                    }),
            ),
    );
    // Legend: name, thin bar sized against the largest share, value.
    let max_share = segments.iter().map(|s| s.1).max().unwrap_or(1).max(1) as f32;
    let mut legend = col().w_full().gap(5.0);
    for (name, n, color) in segments {
        legend = legend.child(
            row()
                .w_full()
                .gap(8.0)
                .items_center()
                .child(
                    text(name.clone())
                        .font(TextStyle::new(11.0, 400.0))
                        .color(t.text2)
                        .w(110.0),
                )
                .child(
                    el().h(3.0).grow(1.0).radius(1.5).fill(t.track).child(
                        el().h(3.0)
                            .w_frac(*n as f32 / max_share)
                            .radius(1.5)
                            .fill(*color),
                    ),
                )
                .child(
                    cui::mono_text(fmt(*n), 10.5, t.text3)
                        .w(40.0)
                        .align(Align::End),
                ),
        );
    }
    out = out.child(legend);
    out
}

/// Scatter and bubble: every mark carries a 1.5 pt ring in the surface color so overlaps
/// stay apart; the hovered point gets guide lines to both axes.
pub fn scatter_bubble(
    t: &Theme,
    points: &[(String, f64, f64, f64, Color)], // name, x, y, size, color
    x_ticks: &[(f64, String)],
    y_ticks: &[(f64, String)],
    w: f32,
    h: f32,
    x_label: &str,
    hover: Option<&str>,
    // An annotation line at an x value with its label, drawn in the danger tone.
    marker: Option<(f64, String)>,
    x_max: f64,
    y_max: f64,
    log_x: bool,
    log_y: bool,
) -> El {
    const L: f32 = 44.0;
    const R: f32 = 16.0;
    const TOP: f32 = 12.0;
    const BOT: f32 = 26.0;
    let iw = (w - L - R).max(10.0);
    let ih = (h - TOP - BOT).max(10.0);
    let xf = |v: f64| -> f32 {
        let f = if log_x {
            v.max(1.0).ln() / x_max.max(1.0).ln()
        } else {
            v / x_max.max(1.0)
        };
        L + iw * f.clamp(0.0, 1.0) as f32
    };
    let yf = |v: f64| -> f32 {
        let f = if log_y {
            v.max(1.0).ln() / y_max.max(1.0).ln()
        } else {
            v / y_max.max(1.0)
        };
        TOP + ih - ih * f.clamp(0.0, 1.0) as f32
    };

    let mut shape = Shape::new();
    for (v, _) in x_ticks {
        shape = dashed(shape, xf(*v), TOP, xf(*v), TOP + ih, t.border_subtle);
    }
    for (v, _) in y_ticks {
        shape = dashed(shape, L, yf(*v), L + iw, yf(*v), t.border_subtle);
    }
    shape = shape.line(
        [L, TOP + ih],
        [L + iw, TOP + ih],
        1.0,
        rune::LineCap::Butt,
        t.border,
    );
    shape = shape.line([L, TOP], [L, TOP + ih], 1.0, rune::LineCap::Butt, t.border);

    if let Some((mx, _mlabel)) = &marker {
        let mxx = xf(*mx);
        shape = dashed(shape, mxx, TOP, mxx, TOP + ih, t.danger.alpha(0.7));
    }

    // Guide lines to both axes for the hovered point, drawn under the marks.
    if let Some(name) = hover {
        if let Some((_, x, y, _, _)) = points.iter().find(|p| p.0 == name) {
            let (px, py) = (xf(*x), yf(*y));
            shape = dashed(shape, px, py, px, TOP + ih, t.fg.alpha(0.35));
            shape = dashed(shape, L, py, px, py, t.fg.alpha(0.35));
        }
    }

    for (name, x, y, size, color) in points {
        let (px, py) = (xf(*x), yf(*y));
        let r = (*size).clamp(2.5, 16.0);
        let on = hover == Some(name.as_str());
        shape = shape.fill(
            circle_points(px, py, r as f32),
            if on {
                color.alpha(0.85)
            } else {
                color.alpha(0.55)
            },
        );
        shape = shape.stroke(
            circle_points(px, py, r as f32),
            1.5,
            if on { t.accent } else { t.surface },
        );
    }

    let mut parts: Vec<El> = vec![el()
        .key("scatter")
        .w_full()
        .h(h)
        .child(canvas(shape).size(w, h))];
    for (v, l) in x_ticks {
        parts.push(
            text(l.clone())
                .font(TextStyle::new(9.0, 400.0).mono())
                .color(t.text3)
                .absolute(xf(*v) - 24.0, TOP + ih + 4.0)
                .w(48.0)
                .align(Align::Center),
        );
    }
    for (v, l) in y_ticks {
        parts.push(
            text(l.clone())
                .font(TextStyle::new(9.0, 400.0).mono())
                .color(t.text3)
                .absolute(0.0, yf(*v) - 5.0)
                .w(L - 6.0)
                .align(Align::End),
        );
    }
    parts.push(
        text(x_label.to_string())
            .font(TextStyle::new(9.0, 400.0))
            .color(t.text3)
            .absolute(L, h - 12.0)
            .w(iw)
            .align(Align::Center),
    );
    if let Some((mx, mlabel)) = &marker {
        parts.push(
            text(mlabel.clone())
                .font(TextStyle::new(8.5, 400.0).mono())
                .color(t.danger)
                .absolute(xf(*mx) + 4.0, TOP),
        );
    }
    for (name, x, y, size, _) in points {
        let r = (*size).clamp(2.5, 16.0) as f32;
        let (px, py) = (xf(*x), yf(*y));
        parts.push(
            el().key(format!("dot/{name}"))
                .absolute(px - r - 3.0, py - r - 3.0)
                .size(2.0 * r + 6.0, 2.0 * r + 6.0)
                .radius(r + 3.0)
                .cursor(Cursor::Pointer)
                .on_hover_change(hover_name(name.clone()))
                .on_press(pin_name(name.clone())),
        );
    }
    el().w_full().h(h).children(parts)
}

/// Activity rings: fractions of a goal; past 1 a ring keeps winding, with a shadow under
/// its tip so the overlap reads. Legend lists each ring's name and share, outer first.
pub fn activity_rings(t: &Theme, rings: &[(String, f64, Color)], size: f32) -> El {
    let (cx, cy) = (size / 2.0, size / 2.0);
    let outer = size / 2.0 - 4.0;
    let thickness = (outer / (rings.len().max(1) as f32) * 0.62).clamp(6.0, 14.0);
    let mut shape = Shape::new();
    for (i, (_, frac, color)) in rings.iter().enumerate() {
        let r = outer - thickness * i as f32 - thickness / 2.0;
        let ir = r - thickness / 2.0 + 0.5;
        let rr = r + thickness / 2.0 - 0.5;
        shape = crate::charts::ring(shape, cx, cy, ir, rr, 0.0, std::f32::consts::TAU, t.track);
        let f = frac.clamp(0.0, 2.0);
        let sweep = std::f32::consts::TAU * (f.min(1.0) as f32);
        if sweep > 0.02 {
            shape = crate::charts::ring(
                shape,
                cx,
                cy,
                ir,
                rr,
                -std::f32::consts::FRAC_PI_2,
                -std::f32::consts::FRAC_PI_2 + sweep,
                *color,
            );
        }
        if f > 1.0 {
            // Wound past the goal: a second lap in a dimmer shade over the full ring.
            let lap = std::f32::consts::TAU * (f - 1.0) as f32;
            shape = crate::charts::ring(
                shape,
                cx,
                cy,
                ir,
                rr,
                -std::f32::consts::FRAC_PI_2,
                -std::f32::consts::FRAC_PI_2 + lap,
                color.alpha(0.55),
            );
        }
    }
    let mut legend = col().gap(6.0);
    for (name, frac, color) in rings {
        legend = legend.child(
            row()
                .gap(8.0)
                .items_center()
                .child(cui::dot(*color, 8.0))
                .child(
                    text(name.clone())
                        .font(TextStyle::new(11.5, 400.0))
                        .color(t.text2),
                )
                .child(
                    cui::mono_text(format!("{}%", (frac * 100.0).round() as i64), 11.0, t.text3)
                        .w(44.0)
                        .align(Align::End),
                ),
        );
    }
    row()
        .gap(16.0)
        .items_center()
        .child(canvas(shape).size(size, size))
        .child(legend)
}

/// Radar: hairline rings and spokes, each series filled lightly under a crisp outline,
/// with a legend.
pub fn radar(
    t: &Theme,
    axes: &[String],
    series: &[(String, Vec<f64>, Color)],
    size: f32,
    max: f64,
) -> El {
    let (cx, cy) = (size / 2.0, size / 2.0 + 4.0);
    let r = size / 2.0 - 26.0;
    let n = axes.len().max(3);
    let pt = |axis: usize, frac: f32| -> [f32; 2] {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * axis as f32 / n as f32;
        [cx + r * frac * a.cos(), cy + r * frac * a.sin()]
    };
    let mut shape = Shape::new();
    for level in [0.25, 0.5, 0.75, 1.0] {
        let ring_pts: Vec<[f32; 2]> = (0..n).map(|a| pt(a, level)).collect();
        shape = shape.stroke_with(ring_pts, true, 1.0, rune::LineCap::Butt, t.border_subtle);
    }
    for a in 0..n {
        let [px, py] = pt(a, 1.0);
        shape = shape.line(
            [cx, cy],
            [px, py],
            1.0,
            rune::LineCap::Butt,
            t.border_subtle,
        );
    }
    for (_, values, color) in series {
        let pts: Vec<[f32; 2]> = values
            .iter()
            .enumerate()
            .map(|(i, &v)| pt(i, (v / max.max(1.0)).clamp(0.02, 1.0) as f32))
            .collect();
        let mut poly = Path::new().move_to(pts[0]);
        for q in &pts[1..] {
            poly = poly.line_to(*q);
        }
        shape = shape.fill_path(&poly.close(), color.alpha(0.16));
        shape = shape.stroke_with(pts, true, 1.75, rune::LineCap::Round, *color);
    }
    let mut axis_labels: Vec<El> = Vec::new();
    for (i, name) in axes.iter().enumerate() {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / n as f32;
        let lx = cx + (r + 12.0) * a.cos();
        let ly = cy + (r + 12.0) * a.sin();
        axis_labels.push(
            text(name.clone())
                .font(TextStyle::new(9.5, 400.0))
                .color(t.text3)
                .absolute(lx - 30.0, ly - 6.0)
                .w(60.0)
                .align(if a.cos().abs() < 0.3 {
                    Align::Center
                } else if a.cos() > 0.0 {
                    Align::Start
                } else {
                    Align::End
                }),
        );
    }
    let mut legend = row().w_full().gap(14.0).justify_center();
    for (name, _, color) in series {
        legend = legend.child(
            row()
                .gap(6.0)
                .items_center()
                .child(cui::dot(*color, 8.0))
                .child(
                    text(name.clone())
                        .font(TextStyle::new(11.0, 400.0))
                        .color(t.text2),
                ),
        );
    }
    col()
        .gap(4.0)
        .child(
            el().size(size, size)
                .self_center()
                .child(canvas(shape).size(size, size))
                .children(axis_labels),
        )
        .child(legend)
}

/// Five-step ramp toward the accent: the empty surface for values at the bottom of the
/// range, then four even mixes toward the accent.
fn ramp(t: &Theme, level: usize) -> Color {
    match level {
        0 => t.track,
        1 => t.surface.mix(t.accent, 0.28),
        2 => t.surface.mix(t.accent, 0.52),
        3 => t.surface.mix(t.accent, 0.76),
        _ => t.accent,
    }
}

fn level_of(v: u32, max: u32) -> usize {
    if max == 0 || v == 0 {
        return 0;
    }
    let f = v as f32 / max as f32;
    if f <= 0.25 {
        1
    } else if f <= 0.5 {
        2
    } else if f <= 0.75 {
        3
    } else {
        4
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Calendar heatmap: one square a day in five steps, weeks as columns starting on Monday,
/// month labels where a month begins, and a Less-to-More key. Hovering a day lights it.
pub fn calendar_heatmap(t: &Theme, per_day: &[u32], today_days: i64, hover: Option<&str>) -> El {
    const CELL: f32 = 11.0;
    const GAP: f32 = 2.0;
    const ROWS: usize = 7;
    let days = per_day.len().min(364);
    if days == 0 {
        return col().h(0.0);
    }
    let today_weekday = weekday_of(today_days) as usize;
    let weeks = (days + today_weekday + 6) / 7;
    let max = per_day.iter().cloned().max().unwrap_or(0);
    // Column c, row r: day index counted from the first Monday on or before the start.
    let cell = |week: usize, wd: usize| -> Option<(usize, String)> {
        let idx = week * 7 + wd as usize;
        if idx >= days + (7 - today_weekday) % 7 {
            return None;
        }
        let day = days as i64 - 1 - (idx as i64 - ((7 - today_weekday) % 7) as i64);
        if day < 0 {
            return None;
        }
        Some((day as usize, format!("d/{day}")))
    };
    let mut cols: Vec<El> = Vec::new();
    let mut month_labels: Vec<(usize, String)> = Vec::new();
    let mut last_month = -1i64;
    for week in 0..weeks {
        let mut cells: Vec<El> = Vec::new();
        for wd in 0..ROWS {
            match cell(week, wd) {
                None => cells.push(el().size(CELL, CELL)),
                Some((day, key)) => {
                    let v = per_day.get(day).cloned().unwrap_or(0);
                    let on = hover == Some(key.as_str());
                    let mut square = el()
                        .key(key.clone())
                        .size(CELL, CELL)
                        .radius(2.5)
                        .fill(ramp(t, level_of(v, max)))
                        .on_hover_change(hover_name(key.clone()));
                    if on {
                        square = square.border(1.0, t.fg);
                    }
                    cells.push(square);
                }
            }
        }
        // A month label goes over the column that contains the 1st of a month.
        if let Some((day, _)) = cell(week, 0) {
            let (_, month, _) = civil_from_days(today_days - day as i64);
            if month != last_month {
                last_month = month;
                month_labels.push((week, MONTHS[(month - 1) as usize].to_string()));
            }
        }
        cols.push(col().gap(GAP).children(cells));
    }
    let grid = row().gap(GAP).children(cols);
    // Month labels sit above the columns that begin a month.
    let mut label_parts: Vec<El> = Vec::new();
    for (week, name) in &month_labels {
        label_parts.push(
            text(name.clone())
                .font(TextStyle::new(8.5, 400.0))
                .color(t.text3)
                .absolute(*week as f32 * (CELL + GAP), 0.0)
                .w(CELL * 3.0),
        );
    }
    let key = row()
        .gap(4.0)
        .items_center()
        .child(note("Less", t.text3))
        .children(
            (0..5)
                .map(|l| el().size(CELL, CELL).radius(2.5).fill(ramp(t, l)))
                .collect::<Vec<El>>(),
        )
        .child(note("More", t.text3));
    col()
        .gap(4.0)
        .child(
            el().w(weeks as f32 * (CELL + GAP))
                .h(10.0)
                .children(label_parts),
        )
        .child(grid)
        .child(row().w_full().gap(8.0).child(key))
}

/// Days since 1970-01-01 back to a civil date (inverse of `days_from_civil`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Weekday of a day count since 1970-01-01 (a Thursday): 0 = Monday.
fn weekday_of(epoch_days: i64) -> i64 {
    (epoch_days + 3).rem_euclid(7)
}

/// Weekday-hour heatmap: one cell per weekday and hour in five steps toward the accent,
/// with weekday row labels, hour column labels, and a Less-to-More key.
pub fn hour_heatmap(t: &Theme, by_hour: &[u32], hover: Option<&str>) -> El {
    const CELL: f32 = 13.0;
    const GAP: f32 = 2.0;
    const LABEL_W: f32 = 30.0;
    if by_hour.len() < 168 {
        return col().h(0.0);
    }
    let max = by_hour.iter().cloned().max().unwrap_or(0);
    let weekdays = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut rows: Vec<El> = Vec::new();
    for wd in 0..7 {
        let mut cells = row().gap(GAP);
        for hour in 0..24 {
            let key = format!("h/{wd}/{hour}");
            let v = by_hour[wd * 24 + hour];
            let on = hover == Some(key.as_str());
            let mut square = el()
                .key(key.clone())
                .size(CELL, CELL)
                .radius(3.0)
                .fill(ramp(t, level_of(v, max)))
                .on_hover_change(hover_name(key.clone()));
            if on {
                square = square.border(1.0, t.fg);
            }
            cells = cells.child(square);
        }
        rows.push(
            row()
                .gap(GAP)
                .items_center()
                .child(
                    text(weekdays[wd].to_string())
                        .font(TextStyle::new(9.0, 400.0))
                        .color(t.text3)
                        .w(LABEL_W),
                )
                .child(cells),
        );
    }
    let mut hour_labels = row().gap(GAP).px(LABEL_W);
    for hour in (0..24).step_by(3) {
        hour_labels = hour_labels.child(
            text(format!("{hour:02}"))
                .font(TextStyle::new(8.5, 400.0).mono())
                .color(t.text3)
                .w(CELL),
        );
    }
    let key = row()
        .gap(4.0)
        .items_center()
        .child(note("Less", t.text3))
        .children(
            (0..5)
                .map(|l| el().size(CELL, CELL).radius(3.0).fill(ramp(t, l)))
                .collect::<Vec<El>>(),
        )
        .child(note("More", t.text3));
    col()
        .gap(6.0)
        .children(rows)
        .child(hour_labels)
        .child(row().gap(8.0).child(key))
}

/// The CUI treemap: squarified tiles that stay neutral, a little lighter for larger items,
/// with the accent filling and ringing only the hovered or selected tile.
pub fn treemap(
    t: &Theme,
    tiles: &[crate::charts::Tile],
    h: f32,
    hover: Option<&str>,
    selected: Option<&str>,
) -> El {
    let max_area = tiles.iter().map(|x| x.w * x.h).fold(0.0f32, f32::max);
    let mut els: Vec<El> = Vec::new();
    for tile in tiles {
        let tw = (tile.w - 3.0).max(0.0);
        let th = (tile.h - 3.0).max(0.0);
        if tw < 3.0 || th < 3.0 {
            continue;
        }
        let area = tw * th;
        let shade = if max_area > 0.0 {
            0.05 + 0.13 * (area / max_area)
        } else {
            0.08
        };
        let (show_name, show_size) = crate::charts::tile_label_area(tw, th);
        let fs = (area.sqrt() / 9.0).clamp(7.0, 12.0);
        let on = hover == Some(tile.name.as_str());
        let sel = selected == Some(tile.name.as_str());
        let fill = if on || sel {
            t.accent.alpha(0.22)
        } else {
            t.surface.mix(t.fg, shade)
        };
        let border_c = if on || sel { t.accent } else { t.border_subtle };
        let mut inner = col().gap(1.0);
        if show_name {
            let maxc = ((tw - 10.0) / (fs * 0.58)).max(3.0) as usize;
            inner = inner.child(
                text(crate::model::fit(&tile.name, maxc))
                    .font(TextStyle::new(fs, 500.0))
                    .color(if on || sel { t.accent_strong } else { t.text2 }),
            );
        }
        if show_size {
            inner = inner.child(
                text(gb(tile.bytes))
                    .font(cui::mono((fs - 3.0).max(7.0)))
                    .color(if on || sel { t.accent } else { t.text3 }),
            );
        }
        els.push(
            el().key(format!("tile/{}", tile.name))
                .absolute(tile.x + 1.5, tile.y + 1.5)
                .size(tw, th)
                .radius((10.0 * (tw.min(th) / 60.0)).clamp(2.0, 10.0))
                .fill(fill)
                .border(1.0, border_c)
                .cursor(Cursor::Pointer)
                .on_hover_change(hover_name(tile.name.clone()))
                .on_press(pin_name(tile.name.clone()))
                .child(inner.px(6.0).py(4.0)),
        );
    }
    el().key("treemap").w_full().h(h).children(els)
}
