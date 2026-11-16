//! The disk meter's model: each category's share of everything under the projects root, and
//! which one the pointer is over. The Storage page draws it as an ordinary segmented bar of
//! Rune elements, so there is no pixel buffer and nothing animates; this keeps the shares and
//! answers which segment sits under a point.

use crate::cats::{Cat, Rgb};

pub struct Segment {
    pub id: Cat,
    pub color: Rgb,
    /// Share of everything under the root, 0..1.
    pub fraction: f32,
}

#[derive(Default)]
pub struct BarModel {
    pub segments: Vec<Segment>,
    /// Segment to light up; the others dim.
    pub hover: Option<Cat>,
    /// Sizes are still coming in.
    pub scanning: bool,
}

/// Points between two segments of the bar.
pub const GAP: f32 = 3.0;

#[derive(Default)]
pub struct Meter {
    /// The bar's width in points, as last laid out.
    width: f32,
    /// Each category and its share, from the last step.
    shares: Vec<(Cat, f32)>,
}

impl Meter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The bar was laid out `w` points wide. The other arguments are kept for the caller's
    /// signature and do not matter to a bar of elements.
    pub fn resize(&mut self, w: f32, _h: f32, _cell: f32, _scale: f32) {
        self.width = w.max(0.0);
    }

    /// Takes the latest shares. Nothing eases, so it never asks for another frame.
    pub fn step(&mut self, _dt: f32, model: &BarModel) -> bool {
        self.shares = model.segments.iter().filter(|s| s.fraction > 0.0).map(|s| (s.id, s.fraction)).collect();
        false
    }

    /// Where each segment with any bytes starts and ends, in points: shares of the width left
    /// after the gaps, the same split the page's flex row makes.
    pub fn spans(&self) -> Vec<(Cat, f32, f32)> {
        let sum: f32 = self.shares.iter().map(|s| s.1).sum();
        if sum <= 0.0 || self.width <= 0.0 {
            return Vec::new();
        }
        let room = (self.width - GAP * (self.shares.len() as f32 - 1.0)).max(0.0);
        let mut x = 0.0;
        self.shares
            .iter()
            .map(|&(id, f)| {
                let w = room * f / sum;
                let span = (id, x, x + w);
                x += w + GAP;
                span
            })
            .collect()
    }

    /// The segment at a point in the bar's own points, if any. A point in a gap counts for the
    /// segment to its left, so the pointer never flickers between two.
    pub fn hit(&self, x: f32, _y: f32) -> Option<Cat> {
        self.spans().into_iter().find(|&(_, _, x1)| x < x1 + GAP).map(|(id, _, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meter(fractions: [f32; 4]) -> Meter {
        let ids = [Cat::Source, Cat::Deps, Cat::Build, Cat::Other];
        let model = BarModel {
            segments: ids
                .iter()
                .zip(fractions)
                .map(|(&id, fraction)| Segment { id, color: [0, 0, 0], fraction })
                .collect(),
            ..BarModel::default()
        };
        let mut m = Meter::new();
        m.resize(400.0 + 3.0 * GAP, 12.0, 0.0, 2.0);
        assert!(!m.step(0.016, &model), "the bar never asks for frames");
        m
    }

    #[test]
    fn each_segment_takes_its_share_of_the_width() {
        let spans = meter([0.5, 0.25, 0.125, 0.125]).spans();
        assert_eq!(spans.len(), 4);
        assert!((spans[0].2 - spans[0].1 - 200.0).abs() < 0.01);
        assert!((spans[1].2 - spans[1].1 - 100.0).abs() < 0.01);
    }

    #[test]
    fn hit_testing_finds_the_segment_under_a_point() {
        let m = meter([0.5, 0.5, 0.0, 0.0]);
        assert_eq!(m.hit(10.0, 0.0), Some(Cat::Source));
        assert_eq!(m.hit(390.0, 0.0), Some(Cat::Deps));
        assert_eq!(meter([0.0; 4]).hit(10.0, 0.0), None);
    }
}
