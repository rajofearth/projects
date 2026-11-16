//! The dot-matrix disk meter, ported from the upstream interface's `bar.ts` (MIT).
//!
//! One bordered block of dots per category, sized by its share of everything under the
//! projects root. Shares ease toward their targets, so the bar grows while a scan is running;
//! the leading edge burns white. Drawn into a BGRA buffer and handed to Rune as a native
//! surface, which is this app's equivalent of writing pixels into a terminal.

use std::collections::HashMap;

use rune::native_surface::{ByteFrame, ColorInfo, Format};
use rune::NativeSurface;

use crate::cats::{Cat, Rgb};

pub const BAR_ROWS: usize = 7;
/// Empty rows above the bar where the meter breathes.
pub const HEADROOM_ROWS: usize = 3;

const WHITE: Rgb = [255, 255, 255];

pub struct Segment {
    pub id: Cat,
    pub color: Rgb,
    /// Share of everything under the root, 0..1.
    pub fraction: f32,
}

pub struct BarModel {
    pub segments: Vec<Segment>,
    /// Segment to light up; the others dim.
    pub hover: Option<Cat>,
    /// Shows the bright leading edge while sizes are still coming in.
    pub scanning: bool,
}

impl Default for BarModel {
    fn default() -> Self {
        Self {
            segments: Vec::new(),
            hover: None,
            scanning: false,
        }
    }
}

#[derive(Clone, Copy)]
struct Placed {
    id: Cat,
    x0: f32,
    x1: f32,
    color: Rgb,
    level: f32,
}

/// Stable pseudo-random number in [0, 1) for a grid position.
fn hash(a: usize, b: usize) -> f32 {
    let mut h = (a as u32)
        .wrapping_add(374_761_393)
        .wrapping_mul(668_265_263)
        ^ (b as u32).wrapping_add(1_274_126_177).wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(3_266_489_917);
    ((h ^ (h >> 16)) as f32) / 4_294_967_296.0
}

#[derive(Default)]
pub struct Meter {
    surface: NativeSurface,
    /// Buffer size in pixels, which is the element's size in points times `scale`.
    width: usize,
    height: usize,
    cell: usize,
    scale: f32,
    buffer: Vec<u8>,
    shown: HashMap<Cat, f32>,
    level: HashMap<Cat, f32>,
    placed: Vec<Placed>,
    time: f32,
    edge: f32,
    frame: u64,
    background: Rgb,
}

impl Meter {
    pub fn new() -> Self {
        Self {
            background: [0, 0, 0],
            scale: 1.0,
            cell: 7,
            ..Self::default()
        }
    }

    pub fn surface(&self) -> &NativeSurface {
        &self.surface
    }

    /// Points tall, the height the element asks layout for.
    pub fn height(cell: f32) -> f32 {
        (BAR_ROWS + HEADROOM_ROWS) as f32 * cell
    }

    /// Sizes the buffer to the element's laid-out rect, at `scale` device pixels per point.
    pub fn resize(&mut self, w: f32, h: f32, cell: f32, scale: f32) {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let width = (w * scale).round().max(1.0) as usize;
        let height = (h * scale).round().max(1.0) as usize;
        let cell = (cell * scale).round().max(3.0) as usize;
        if width == self.width && height == self.height && cell == self.cell {
            return;
        }
        self.width = width;
        self.height = height;
        self.cell = cell;
        self.scale = scale;
        self.buffer = vec![0; width * height * 4];
        self.placed.clear();
        self.push();
    }

    fn get_top(&self) -> f32 {
        self.height as f32 - (BAR_ROWS * self.cell) as f32
    }

    /// Id of the segment at a point in the element's own points, if any.
    pub fn hit(&self, x: f32, y: f32) -> Option<Cat> {
        let (x, y) = (x * self.scale, y * self.scale);
        if y < self.get_top() {
            return None;
        }
        self.placed
            .iter()
            .find(|p| x >= p.x0 && x < p.x1)
            .map(|p| p.id)
    }

    /// Advances by `dt` seconds, draws and presents a frame. True while anything still moves.
    pub fn step(&mut self, dt: f32, model: &BarModel) -> bool {
        if self.width == 0 {
            return false;
        }
        let dt = dt.min(0.1);
        self.time += dt;
        let mut moving = model.scanning;

        let ease = (dt * 9.0).min(1.0);
        let fade = (dt * 12.0).min(1.0);
        let mut cursor = 0.0f32;

        for segment in &model.segments {
            let from = *self.shown.get(&segment.id).unwrap_or(&0.0);
            let mut to = from + (segment.fraction - from) * ease;
            if (segment.fraction - to).abs() < 0.00005 {
                to = segment.fraction;
            } else {
                moving = true;
            }
            self.shown.insert(segment.id, to);

            let wanted = match model.hover {
                None => 1.0,
                Some(id) if id == segment.id => 1.15,
                Some(_) => 0.4,
            };
            let current = *self.level.get(&segment.id).unwrap_or(&1.0);
            let mut next = current + (wanted - current) * fade;
            if (wanted - next).abs() < 0.01 {
                next = wanted;
            } else {
                moving = true;
            }
            self.level.insert(segment.id, next);
            cursor += to;
        }

        let edge_wanted = if model.scanning { 1.0 } else { 0.0 };
        self.edge += (edge_wanted - self.edge) * (dt * 6.0).min(1.0);
        if (edge_wanted - self.edge).abs() > 0.01 {
            moving = true;
        }

        // Only a frame that would look different is worth drawing and handing to the GPU.
        if moving || self.edge > 0.004 {
            self.draw(model);
            self.push();
        }
        // Report movement so the frame loop keeps running until the bar comes to rest.
        moving || self.edge > 0.004 || self.edge < 0.996 && model.scanning
    }

    fn push(&mut self) {
        let planes = [std::mem::take(&mut self.buffer), Vec::new()];
        let frame = ByteFrame {
            width: self.width as u32,
            height: self.height as u32,
            format: Format::Bgra,
            color: ColorInfo::default(),
            planes,
        };
        self.frame += 1;
        // A refused frame means the size is bad; keep the old picture and try again next tick.
        let _ = self.surface.push_bytes(frame, self.frame);
        self.buffer = vec![0; self.width * self.height * 4];
    }

    // ---- drawing ---------------------------------------------------------------

    fn blend(&mut self, x: f32, y: f32, color: Rgb, alpha: f32) {
        if alpha <= 0.0 || x < 0.0 || y < 0.0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return;
        }
        let alpha = alpha.min(1.0);
        let i = (y * self.width + x) * 4;
        let b = &mut self.buffer;
        b[i] = (b[i] as f32 + (color[2] as f32 - b[i] as f32) * alpha) as u8;
        b[i + 1] = (b[i + 1] as f32 + (color[1] as f32 - b[i + 1] as f32) * alpha) as u8;
        b[i + 2] = (b[i + 2] as f32 + (color[0] as f32 - b[i + 2] as f32) * alpha) as u8;
    }

    /// Rounded rectangle with a translucent fill and a brighter border.
    #[allow(clippy::too_many_arguments)]
    fn block(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Rgb, fill: f32, border: f32) {
        let (w, h) = (x1 - x0, y1 - y0);
        if w < 1.0 || h < 1.0 {
            return;
        }
        let cell = self.cell as f32;
        let radius = (cell * 0.45).min(w / 2.0).min(h / 2.0);
        let thick = (cell / 7.0).round().max(1.0);
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (hx, hy) = (w / 2.0 - radius, h / 2.0 - radius);
        let xa = x0.max(0.0).floor() as usize;
        let xb = x1.ceil().min(self.width as f32) as usize;
        let ya = y0.max(0.0).floor() as usize;
        let yb = y1.ceil().min(self.height as f32) as usize;
        for y in ya..yb {
            let qy = ((y as f32 + 0.5) - cy).abs() - hy;
            for x in xa..xb {
                let qx = ((x as f32 + 0.5) - cx).abs() - hx;
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let distance = outside + qx.max(qy).min(0.0) - radius;
                if distance > 0.5 {
                    continue;
                }
                let coverage = (0.5 - distance).min(1.0);
                let edge = (distance + thick + 0.5).clamp(0.0, 1.0);
                self.blend(
                    x as f32,
                    y as f32,
                    color,
                    coverage * (fill + (border - fill) * edge),
                );
            }
        }
    }

    fn dot(&mut self, cx: f32, cy: f32, radius: f32, color: Rgb, alpha: f32, glow: f32) {
        let reach = if glow > 0.0 { radius * 3.2 } else { radius + 1.0 };
        let xa = (cx - reach).floor();
        let xb = (cx + reach).ceil();
        let ya = (cy - reach).floor();
        let yb = (cy + reach).ceil();
        let mut y = ya;
        while y <= yb {
            let mut x = xa;
            while x <= xb {
                let d = ((x + 0.5 - cx).powi(2) + (y + 0.5 - cy).powi(2)).sqrt();
                let mut a = (radius + 0.5 - d).clamp(0.0, 1.0) * alpha;
                if glow > 0.0 {
                    let g = d / (radius * 1.6);
                    a += glow * (-g * g).exp() * (1.0 - a);
                }
                if a > 0.004 {
                    self.blend(x, y, color, a);
                }
                x += 1.0;
            }
            y += 1.0;
        }
    }

    fn draw(&mut self, model: &BarModel) {
        let (width, height, cell) = (self.width, self.height, self.cell);
        if width == 0 || height == 0 {
            return;
        }
        let top = self.get_top();
        let bg = self.background;

        for pixel in self.buffer.chunks_exact_mut(4) {
            pixel[0] = bg[2];
            pixel[1] = bg[1];
            pixel[2] = bg[0];
            pixel[3] = 255;
        }

        // Where each segment sits this frame. The previous frame's placement is what hit
        // testing reads, so the new one is built aside and swapped in at the end.
        let mut placed = Vec::with_capacity(model.segments.len());
        let mut x = 0.0f32;
        for segment in &model.segments {
            let w = *self.shown.get(&segment.id).unwrap_or(&0.0) * width as f32;
            if w >= 0.5 {
                placed.push(Placed {
                    id: segment.id,
                    x0: x,
                    x1: x + w,
                    color: segment.color,
                    level: *self.level.get(&segment.id).unwrap_or(&1.0),
                });
            }
            x += w;
        }
        let filled = x;
        self.placed = placed;

        for i in 0..self.placed.len() {
            let (x0, x1, color, level) = {
                let p = &self.placed[i];
                (p.x0, p.x1, p.color, p.level)
            };
            // A faint wash behind each segment's dots; no outline.
            self.block(x0, top, x1, height as f32, color, 0.07 * level, 0.07 * level);
        }

        let cellf = cell as f32;
        let columns = width / cell;
        let offset = (width as f32 - (columns * cell) as f32) / 2.0;
        let radius = (cellf * 0.24).max(1.0);
        let inset = cellf * 0.3;
        let mut index = 0usize;
        for column in 0..columns {
            let cx = offset + column as f32 * cellf + cellf / 2.0;
            while index < self.placed.len() && cx >= self.placed[index].x1 {
                index += 1;
            }
            let segment = self.placed.get(index).copied();
            let within = segment.is_some_and(|s| cx >= s.x0 + inset && cx <= s.x1 - inset);
            // Columns near the growing edge of a scan burn white.
            let lead = if self.edge > 0.0 && cx <= filled {
                (1.0 - (filled - cx) / (cellf * 3.0)).max(0.0) * self.edge
            } else {
                0.0
            };
            for row in 0..BAR_ROWS {
                let cy = top + row as f32 * cellf + cellf / 2.0;
                let Some(segment) = segment else {
                    self.dot(cx, cy, radius * 0.8, WHITE, 0.05, 0.0);
                    continue;
                };
                if !within {
                    if cx > segment.x1 || cx < segment.x0 {
                        self.dot(cx, cy, radius * 0.8, WHITE, 0.05, 0.0);
                    }
                    continue;
                }
                let kind = hash(column, row);
                let phase = hash(row + 101, column + 17);
                let twinkle =
                    0.72 + 0.28 * (self.time * (0.8 + phase * 2.6) + phase * 40.0).sin();
                let level = segment.level;
                if lead > 0.0 && kind < 0.35 + lead * 0.65 {
                    self.dot(cx, cy, radius, WHITE, (0.6 + lead * 0.4).min(1.0), 0.3 * lead);
                } else if kind < 0.5 {
                    self.dot(cx, cy, radius, segment.color, 0.45 * twinkle * level, 0.0);
                } else if kind < 0.86 {
                    self.dot(cx, cy, radius, segment.color, 0.95 * twinkle * level, 0.0);
                } else {
                    self.dot(
                        cx,
                        cy,
                        radius,
                        WHITE,
                        (0.95 * twinkle * level).min(1.0),
                        0.22 * twinkle * level.min(1.0),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cats::CATEGORIES;

    fn model(fractions: [f32; 4], hover: Option<Cat>, scanning: bool) -> BarModel {
        BarModel {
            segments: CATEGORIES
                .iter()
                .zip(fractions)
                .map(|(c, f)| Segment {
                    id: c.id,
                    color: c.color,
                    fraction: f,
                })
                .collect(),
            hover,
            scanning,
        }
    }

    /// Lit pixels on one row of the bar, left to right, as (x, colour of the brightest dot).
    /// Columns of the bar's middle row that carry a lit dot. `step` hands its buffer to
    /// the surface, so the picture is drawn here without presenting it.
    fn lit(m: &mut Meter) -> Vec<usize> {
        m.draw(&model([0.0; 4], None, false));
        let top = m.get_top() as usize;
        let row = top + 3 * m.cell;
        (0..m.width)
            .filter(|&x| {
                let i = (row * m.width + x) * 4;
                let px = &m.buffer[i..i + 4];
                // Anything clearly above the black background counts as a lit dot.
                px[0] > 24 || px[1] > 24 || px[2] > 24
            })
            .collect()
    }

    fn bar(sizes: [f32; 4]) -> Meter {
        let mut m = Meter::new();
        m.resize(320.0, Meter::height(7.0), 7.0, 1.0);
        // Settle the easing so shares sit at their targets.
        for _ in 0..40 {
            m.step(0.05, &model(sizes, None, false));
        }
        m
    }

    #[test]
    fn each_segment_lights_its_own_slice_of_the_bar() {
        let mut m = bar([0.25, 0.25, 0.25, 0.25]);
        assert_eq!(m.placed.len(), 4, "all four segments are placed");
        let dots = lit(&mut m);
        assert!(!dots.is_empty(), "the bar has lit dots");
        assert!(
            *dots.iter().max().unwrap() > m.width * 9 / 10,
            "the last quarter is lit: max at {:?} of {}",
            dots.iter().max(),
            m.width
        );
        // The gaps between the segments' ends are unlit: the dots stop at a segment's edge.
        let mut empty = bar([0.0, 0.0, 0.0, 0.0]);
        assert!(lit(&mut empty).is_empty(), "no sizes, no dots");
    }

    #[test]
    fn hit_testing_finds_the_segment_under_a_point() {
        let m = bar([0.5, 0.5, 0.0, 0.0]);
        let y = m.get_top() + 4.0;
        let left = m.hit(m.width as f32 * 0.25, y);
        let right = m.hit(m.width as f32 * 0.75, y);
        assert_eq!(left, Some(Cat::Source));
        assert_eq!(right, Some(Cat::Deps));
        assert_eq!(m.hit(m.width as f32 * 0.25, 0.0), None, "above the bar");
    }

    #[test]
    fn hover_dims_the_others_and_settles() {
        let mut m = Meter::new();
        m.resize(320.0, Meter::height(7.0), 7.0, 2.0);
        let mut moving = true;
        for _ in 0..60 {
            moving = m.step(0.05, &model([0.5, 0.5, 0.0, 0.0], Some(Cat::Source), false));
        }
        assert!(!moving, "an idle hover settles");
        assert!(*m.level.get(&Cat::Source).unwrap() > 1.0);
        assert!(*m.level.get(&Cat::Deps).unwrap() < 0.5);
    }

    #[test]
    fn a_scanning_bar_keeps_asking_for_frames() {
        let mut m = Meter::new();
        m.resize(200.0, Meter::height(7.0), 7.0, 1.0);
        assert!(m.step(0.016, &model([1.0, 0.0, 0.0, 0.0], None, true)));
        for _ in 0..10 {
            m.step(0.016, &model([1.0, 0.0, 0.0, 0.0], None, true));
        }
        assert!(m.edge > 0.5, "the leading edge burns while scanning");
    }

    #[test]
    fn resizing_keeps_the_buffer_square_with_the_element() {
        let mut m = Meter::new();
        m.resize(100.0, Meter::height(6.0), 6.0, 2.0);
        assert_eq!(m.width, 200);
        assert_eq!(m.height, (Meter::height(6.0) * 2.0) as usize);
        m.step(0.016, &model([1.0, 0.0, 0.0, 0.0], None, false));
        assert_eq!(m.buffer.len(), m.width * m.height * 4);
    }
}
