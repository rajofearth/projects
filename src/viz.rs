//! CUI chart components ported to Rune: the scatter-and-bubble map, the donut and the
//! neutral treemap. Drawing follows CUI's chart rules: faint dashed gridlines over a solid
//! zero line, small muted axis labels, the accent spent with intent. Details live in
//! native tooltips (`El::help`), never in hover state.

use rune::TextStyle;
use rune::{canvas, col, el, row, text, Align, Color, El, Shape};

use crate::cui::{self, Theme};
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

fn circle_points(cx: f32, cy: f32, r: f32) -> Vec<[f32; 2]> {
    (0..24)
        .map(|i| {
            let a = i as f32 / 24.0 * std::f32::consts::TAU;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

/// A thin donut: segments parted by real gaps, the leading share in the middle, and a
/// legend whose names carry a thin bar sized against the largest share.
/// Which segment a point in the donut square lands in, by angle and radius band.
fn seg_at(segments: &[(String, u64, Color)], size: f32, x: f32, y: f32) -> Option<usize> {
    let (dx, dy) = (x - size / 2.0, y - size / 2.0);
    let dist = (dx * dx + dy * dy).sqrt();
    let r = size / 2.0 - 6.0;
    let ir = r * 0.62;
    if !(ir..=r).contains(&dist) {
        return None;
    }
    let mut a = dy.atan2(dx) + std::f32::consts::FRAC_PI_2; // 0 at 12 o'clock
    if a < 0.0 {
        a += std::f32::consts::TAU;
    }
    let total: u64 = segments.iter().map(|s| s.1).sum();
    let mut ang = 0.0f32;
    for (i, (_, n, _)) in segments.iter().enumerate() {
        let span = (*n as f32 / total.max(1) as f32) * std::f32::consts::TAU;
        if a >= ang && a < ang + span {
            return Some(i);
        }
        ang += span;
    }
    None
}

/// Hover plumbing for the donut: entering a wedge cell or legend row names that segment
/// in the ring's middle; leaving clears it only if it is still the one named.
pub fn seg_hover(i: usize) -> impl Fn(&mut crate::State, bool, &mut rune::Tx) + Clone + 'static {
    move |s: &mut crate::State, entered, _tx| {
        if entered {
            s.hover_seg = Some(i);
        } else if s.hover_seg == Some(i) {
            s.hover_seg = None;
        }
    }
}

pub fn donut(
    t: &Theme,
    segments: &[(String, u64, Color)],
    size: f32,
    hover_seg: Option<usize>,
) -> El {
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
    let mut seg_index = 0usize;
    for (name, n, color) in segments {
        if *n == 0 {
            continue;
        }
        let span = (*n as f32 / total.max(1) as f32) * std::f32::consts::TAU;
        let a1 = ang + (span - gap).max(0.01);
        let on = hover_seg == Some(seg_index);
        // The hovered wedge grows a little outward at full strength; the rest dim.
        let (r_in, r_out) = if on { (ir - 2.0, r + 4.0) } else { (ir, r) };
        shape = crate::charts::ring(
            shape,
            cx,
            cy,
            r_in,
            r_out,
            ang,
            a1,
            color.alpha(if on { 1.0 } else { 0.5 }),
        );
        seg_index += 1;
        let is_lead = match &lead {
            Some((_, ln, _)) => *n > *ln,
            None => true,
        };
        if is_lead {
            lead = Some((name.clone(), *n, *color));
        }
        ang += span;
    }
    // The middle names the hovered segment, or the leader at rest; the key swap plus the
    // fade transition crossfades the figures as the pointer moves between wedges.
    let shown = hover_seg
        .and_then(|i| segments.get(i))
        .map(|(name, n, _)| (name.clone(), *n))
        .or_else(|| lead.as_ref().map(|(name, n, _)| (name.clone(), *n)));
    let center = col()
        .absolute(cx - 60.0, cy - 16.0)
        .w(120.0)
        .items_center()
        .gap(1.0)
        .child(
            el().key(format!("segcenter/{}", hover_seg.unwrap_or(usize::MAX)))
                .transition(rune::Transition::opacity())
                .child(
                    col()
                        .items_center()
                        .gap(1.0)
                        .child(match &shown {
                            Some((_, n)) => text(fmt(*n))
                                .font(TextStyle::new(24.0, 500.0).mono())
                                .color(t.fg),
                            None => text("0")
                                .font(TextStyle::new(24.0, 500.0).mono())
                                .color(t.text3),
                        })
                        .child(match &shown {
                            Some((name, _)) => text(name.clone())
                                .font(TextStyle::new(9.5, 400.0))
                                .color(t.text3),
                            None => text("nothing yet")
                                .font(TextStyle::new(9.5, 400.0))
                                .color(t.text3),
                        }),
                ),
        );
    // Wedge hit cells: a grid over the ring; each cell knows which segment its middle
    // lands in, so hovering anywhere on a wedge names it.
    const CELLS: usize = 11;
    let cell = size / CELLS as f32;
    let mut wedge_cells: Vec<El> = Vec::new();
    for row_i in 0..CELLS {
        let mut cells = row();
        for col_i in 0..CELLS {
            let seg = seg_at(
                segments,
                size,
                cell * (col_i as f32 + 0.5),
                cell * (row_i as f32 + 0.5),
            );
            cells = cells.child(match seg {
                Some(i) => el()
                    .key(format!("seg/{i}/{row_i}/{col_i}"))
                    .w(cell)
                    .h(cell)
                    .on_hover_change(seg_hover(i)),
                None => el().w(cell).h(cell),
            });
        }
        wedge_cells.push(cells);
    }
    let mut out = col().gap(10.0).child(
        el().size(size, size)
            .self_center()
            .child(canvas(shape).size(size, size))
            .child(center)
            .child(col().children(wedge_cells)),
    );
    // Legend: name, thin bar sized against the largest share, then count and share in
    // their own right-aligned columns. Hovering a row lights its wedge too.
    let max_share = segments.iter().map(|s| s.1).max().unwrap_or(1).max(1) as f32;
    let total_f = total.max(1) as f32;
    let mut legend = col().w_full().gap(2.0);
    for (i, (name, n, color)) in segments.iter().enumerate() {
        let on = hover_seg == Some(i);
        legend = legend.child(
            row()
                .key(format!("segrow/{name}"))
                .w_full()
                .h(22.0)
                .gap(8.0)
                .items_center()
                .px(6.0)
                .radius(cui::R_CONTROL)
                .hover(t.hover())
                .on_hover_change(seg_hover(i))
                .child(cui::dot(*color, 7.0))
                .child(
                    text(name.clone())
                        .font(TextStyle::new(11.0, if on { 500.0 } else { 400.0 }))
                        .color(if on { t.fg } else { t.text2 })
                        .w(104.0),
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
                    cui::mono_text(fmt(*n), 10.5, t.text2)
                        .w(76.0)
                        .align(Align::End),
                )
                .child(
                    cui::mono_text(
                        format!("{:.0}%", *n as f32 / total_f * 100.0),
                        10.5,
                        t.text3,
                    )
                    .w(38.0)
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
    // An annotation line at an x value with its label, drawn in the danger tone.
    marker: Option<(f64, String)>,
    x_max: f64,
    y_max: f64,
    log_x: bool,
    log_y: bool,
    // One tooltip line per point, same order as `points`.
    tips: &[String],
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

    for (_, x, y, size, color) in points {
        let (px, py) = (xf(*x), yf(*y));
        let r = (*size).clamp(2.5, 16.0) as f32;
        shape = shape.fill(circle_points(px, py, r), color.alpha(0.55));
        shape = shape.stroke(circle_points(px, py, r), 1.5, t.surface);
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
    // Tooltip cells: a grid over the plot; each cell names the bubble whose center falls
    // in it, so every point has a tooltip no matter where its ring is drawn.
    const COLS: usize = 16;
    const ROWS: usize = 7;
    let cw = iw / COLS as f32;
    let ch = ih / ROWS as f32;
    let mut grid: Vec<El> = Vec::new();
    for row_i in 0..ROWS {
        let mut cells = row();
        for col_i in 0..COLS {
            let (cx, cy) = (
                L + cw * (col_i as f32 + 0.5),
                TOP + ch * (row_i as f32 + 0.5),
            );
            let hit = points
                .iter()
                .zip(tips)
                .enumerate()
                .filter(|(_, (p, _))| {
                    (xf(p.1) - cx).abs() <= cw / 2.0 && (yf(p.2) - cy).abs() <= ch / 2.0
                })
                .min_by_key(|(_, (p, _))| ((xf(p.1) - cx).powi(2) + (yf(p.2) - cy).powi(2)) as u64)
                .map(|(i, _)| i);
            cells = cells.child(match hit {
                Some(i) => el()
                    .key(format!("sc/{i}"))
                    .w(cw)
                    .h(ch)
                    .help(tips[i].clone()),
                None => el().w(cw).h(ch),
            });
        }
        grid.push(cells);
    }
    parts.push(col().absolute(L, TOP).w(iw).h(ih).children(grid));
    el().w_full().h(h).children(parts)
}

/// The CUI treemap: squarified tiles that stay neutral, a little lighter for larger items,
/// a soft accent tint under the cursor (painted, no view commit), and a tooltip per tile.
pub fn treemap(
    t: &Theme,
    tiles: &[crate::charts::Tile],
    w: f32,
    h: f32,
    tip: impl Fn(&crate::charts::Tile) -> String,
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
        let mut inner = col().gap(1.0);
        if show_name {
            let maxc = ((tw - 10.0) / (fs * 0.58)).max(3.0) as usize;
            inner = inner.child(
                text(crate::model::fit(&tile.name, maxc))
                    .font(TextStyle::new(fs, 500.0))
                    .color(t.text2),
            );
        }
        if show_size {
            inner = inner.child(
                text(gb(tile.bytes))
                    .font(cui::mono((fs - 3.0).max(7.0)))
                    .color(t.text3),
            );
        }
        els.push(
            el().key(format!("tile/{}", tile.name))
                .absolute(tile.x + 1.5, tile.y + 1.5)
                .size(tw, th)
                .radius((10.0 * (tw.min(th) / 60.0)).clamp(2.0, 10.0))
                .fill(t.surface.mix(t.fg, shade))
                .hover(t.accent.alpha(0.18))
                .border(1.0, t.border_subtle)
                .help(tip(tile))
                .child(inner.px(6.0).py(4.0)),
        );
    }
    el().key("treemap").w(w).h(h).self_center().children(els)
}
