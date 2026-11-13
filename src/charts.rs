//! Chart geometry: a squarified treemap and donut segments, ported from the mock dashboard.

use rune::{Color, Shape};

pub struct Tile {
    pub name: String,
    pub bytes: u64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

fn worst(row: &[f64], side: f64) -> f64 {
    if row.is_empty() {
        return f64::INFINITY;
    }
    let total: f64 = row.iter().sum();
    let mx = row.iter().cloned().fold(0.0, f64::max);
    let mn = row.iter().cloned().fold(f64::INFINITY, f64::min);
    (side * side * mx / (total * total)).max((total * total) / (side * side * mn))
}

/// Squarified treemap (Bruls, Huizing & van Wijk). Sorts big-to-small itself and skips
/// zero-byte items, so callers can't hand it an order the algorithm can't use.
pub fn squarify(items: &[(String, u64, String)], x: f32, y: f32, w: f32, h: f32) -> Vec<Tile> {
    let total: f64 = items.iter().map(|i| i.1 as f64).sum();
    if total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    // (name, bytes, scaled area)
    let mut rest: Vec<(String, u64, f64)> = items
        .iter()
        .filter(|i| i.1 > 0)
        .map(|(n, v, _)| (n.clone(), *v, *v as f64 / total * (w as f64) * (h as f64)))
        .collect();
    rest.sort_by(|a, b| b.1.cmp(&a.1));
    let mut out = Vec::new();
    let (mut rx, mut ry, mut rw, mut rh) = (x, y, w, h);
    let mut row: Vec<(String, u64, f64)> = Vec::new();

    let flush = |row: &mut Vec<(String, u64, f64)>,
                 out: &mut Vec<Tile>,
                 rx: &mut f32,
                 ry: &mut f32,
                 rw: &mut f32,
                 rh: &mut f32| {
        if row.is_empty() {
            return;
        }
        let s: f64 = row.iter().map(|r| r.2).sum();
        if *rw >= *rh {
            let cw = (s / *rh as f64) as f32;
            let mut cy = *ry;
            for r in row.drain(..) {
                let ih = (r.2 / cw as f64) as f32;
                out.push(Tile {
                    name: r.0,
                    bytes: r.1,
                    x: *rx,
                    y: cy,
                    w: cw,
                    h: ih,
                });
                cy += ih;
            }
            *rx += cw;
            *rw -= cw;
        } else {
            let ch = (s / *rw as f64) as f32;
            let mut cx = *rx;
            for r in row.drain(..) {
                let iw = (r.2 / ch as f64) as f32;
                out.push(Tile {
                    name: r.0,
                    bytes: r.1,
                    x: cx,
                    y: *ry,
                    w: iw,
                    h: ch,
                });
                cx += iw;
            }
            *ry += ch;
            *rh -= ch;
        }
    };

    while let Some(item) = rest.first().cloned() {
        rest.remove(0);
        let side = rw.min(rh) as f64;
        let areas: Vec<f64> = row.iter().map(|r| r.2).collect();
        let mut with = areas.clone();
        with.push(item.2);
        if row.is_empty() || worst(&with, side) <= worst(&areas, side) {
            row.push(item);
        } else {
            rest.insert(0, item);
            flush(&mut row, &mut out, &mut rx, &mut ry, &mut rw, &mut rh);
        }
    }
    flush(&mut row, &mut out, &mut rx, &mut ry, &mut rw, &mut rh);
    out
}

/// One donut segment as a flattened arc polygon (4° steps).
fn arc_points(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<[f32; 2]> {
    let steps = ((a1 - a0).abs() / (4.0f32.to_radians())).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

/// A donut ring segment between radii `ir` and `r`, from angle `a0` to `a1` (radians, y-down).
pub fn ring(
    shape: Shape,
    cx: f32,
    cy: f32,
    ir: f32,
    r: f32,
    a0: f32,
    a1: f32,
    color: Color,
) -> Shape {
    let mut outer = arc_points(cx, cy, r, a0, a1);
    let mut inner = arc_points(cx, cy, ir, a1, a0);
    outer.extend(inner.drain(..));
    shape.fill(outer, color)
}

/// Label for a treemap tile: name if it fits, size under it when there is room.
pub fn tile_label_area(w: f32, h: f32) -> (bool, bool) {
    let area = w * h;
    (area > 3400.0, area > 14000.0)
}

/// A readable per-scan duration.
pub fn scan_time(secs: f64) -> String {
    if secs >= 60.0 {
        let mins = (secs / 60.0) as u64;
        format!("{mins}m {:.0}s", secs % 60.0)
    } else {
        format!("{secs:.1}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<(String, u64, String)> {
        vec![
            ("a".into(), 50, "Web App".into()),
            ("b".into(), 25, "Mobile".into()),
            ("c".into(), 15, "AI / ML".into()),
            ("d".into(), 10, "Learning".into()),
        ]
    }

    #[test]
    fn tiles_fill_the_box_without_gaps() {
        let tiles = squarify(&items(), 0.0, 0.0, 700.0, 280.0);
        let area: f32 = tiles.iter().map(|t| t.w * t.h).sum();
        assert!((area - 700.0 * 280.0).abs() < 1.0, "covered {area}");
        for t in &tiles {
            assert!(t.w >= 0.0 && t.h >= 0.0);
            assert!(t.x + t.w <= 700.0 + 0.01 && t.y + t.h <= 280.0 + 0.01);
        }
    }

    #[test]
    fn bigger_items_get_bigger_tiles() {
        let tiles = squarify(&items(), 0.0, 0.0, 700.0, 280.0);
        let by_name = |n: &str| tiles.iter().find(|t| t.name == n).unwrap();
        assert!(by_name("a").w * by_name("a").h > by_name("d").w * by_name("d").h);
    }

    #[test]
    fn arc_points_trace_the_requested_arc() {
        let pts = arc_points(0.0, 0.0, 10.0, 0.0, std::f32::consts::FRAC_PI_2);
        assert!(
            pts.len() >= 16,
            "4° steps over a quarter circle: {}",
            pts.len()
        );
        assert!(
            (pts[0][0] - 10.0).abs() < 1e-4 && pts[0][1].abs() < 1e-4,
            "starts at angle 0"
        );
        let last = pts.last().unwrap();
        assert!(
            last[0].abs() < 1e-4 && (last[1] - 10.0).abs() < 1e-4,
            "ends at the end angle"
        );
        for p in &pts {
            assert!(
                (p[0] * p[0] + p[1] * p[1] - 100.0).abs() < 1e-3,
                "every point on the r=10 circle"
            );
        }
    }

    #[test]
    fn sizes_format() {
        assert_eq!(crate::model::gb(1073741824), "1.0 GiB");
        assert_eq!(crate::model::gb(0), "0 MiB");
        assert_eq!(crate::model::fmt(1234567), "1,234,567");
        assert_eq!(crate::model::gib(2147483648u64), 2.0);
        assert_eq!(scan_time(90.0), "1m 30s");
        assert_eq!(scan_time(37.2), "37.2s");
    }
}
