//! Flooding a terrain from a point: the cells water standing at a level reaches from where it
//! comes in, and how deep it stands there.
//!
//! Water at `level` fills every cell lower than that which a path of such cells joins to the
//! seed (four-way: water doesn't slip between two corners touching), as a river or a canal
//! overflowing would; a hollow behind higher ground stays dry. This is the "connected bathtub"
//! of flood mapping: it shows where a water level reaches, not how the water flows there or how
//! long it takes.

use std::collections::VecDeque;

/// Where water at a level stands, over a grid of heights.
#[derive(Clone, Debug, PartialEq)]
pub struct Flood {
    /// Depth (metres) in each cell, row by row from the top; NaN where it is dry.
    pub depth: Vec<f32>,
    /// How many cells are under water.
    pub cells: usize,
    /// The deepest water (metres).
    pub max_depth: f64,
    /// Whether the water reaches the grid's edge: it may go on beyond it.
    pub reaches_edge: bool,
}

/// The water standing at `level` that comes in at `seed` (column, row) over `heights`
/// (`cols` × `rows`, row by row from the top; NaN where nothing is known, which water doesn't
/// enter). `None` when the seed is off the grid, on no height, or above the level.
pub fn flood_fill(heights: &[f32], cols: usize, rows: usize, seed: (usize, usize), level: f64) -> Option<Flood> {
    if cols == 0 || rows == 0 || heights.len() != cols * rows || seed.0 >= cols || seed.1 >= rows || !level.is_finite() {
        return None;
    }
    let wet = |i: usize| {
        let h = f64::from(heights[i]);
        h.is_finite() && h <= level
    };
    let start = seed.1 * cols + seed.0;
    if !wet(start) {
        return None;
    }
    let mut depth = vec![f32::NAN; heights.len()];
    let mut seen = vec![false; heights.len()];
    let mut queue = VecDeque::from([start]);
    seen[start] = true;
    let (mut cells, mut max_depth, mut reaches_edge) = (0, 0.0_f64, false);
    while let Some(i) = queue.pop_front() {
        let d = level - f64::from(heights[i]);
        depth[i] = d as f32;
        cells += 1;
        max_depth = max_depth.max(d);
        let (c, r) = (i % cols, i / cols);
        if c == 0 || r == 0 || c + 1 == cols || r + 1 == rows {
            reaches_edge = true;
        }
        let near = [(c > 0).then(|| i - 1), (c + 1 < cols).then(|| i + 1), (r > 0).then(|| i - cols), (r + 1 < rows).then(|| i + cols)];
        for n in near.into_iter().flatten() {
            if !seen[n] && wet(n) {
                seen[n] = true;
                queue.push_back(n);
            }
        }
    }
    Some(Flood { depth, cells, max_depth, reaches_edge })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valley running down the middle column, open at the top, a hollow on the right behind a ridge.
    fn valley() -> Vec<f32> {
        #[rustfmt::skip]
        let h = [
            9.0, 5.0, 2.0, 5.0, 9.0, 1.0, 9.0,
            9.0, 4.0, 1.0, 4.0, 9.0, 1.0, 9.0,
            9.0, 4.0, 0.0, 4.0, 9.0, 1.0, 9.0,
            9.0, 5.0, 3.0, 5.0, 9.0, 9.0, 9.0,
        ];
        h.to_vec()
    }

    #[test]
    fn water_fills_what_joins_the_seed_below_the_level() {
        let h = valley();
        // At 1.5 m: the valley floor only, not the hollow behind the ridge (as low, not joined).
        let f = flood_fill(&h, 7, 4, (2, 2), 1.5).unwrap();
        assert_eq!(f.cells, 2);
        assert!((f.max_depth - 1.5).abs() < 1e-9);
        assert!(f.depth[5].is_nan(), "the hollow stays dry");
        assert!((f64::from(f.depth[2 * 7 + 2]) - 1.5).abs() < 1e-6);
        assert!(!f.reaches_edge);
        // At 4.5 m: up the valley's sides and out at its top and bottom, still not the hollow.
        let f = flood_fill(&h, 7, 4, (2, 2), 4.5).unwrap();
        assert_eq!(f.cells, 8);
        assert!(f.reaches_edge);
        assert!(f.depth[5].is_nan());
        // Over the ridge (9 m): the hollow too.
        let f = flood_fill(&h, 7, 4, (2, 2), 9.0).unwrap();
        assert_eq!(f.cells, 28);
    }

    #[test]
    fn no_flood_from_a_seed_off_the_grid_on_nothing_or_above_the_level() {
        let mut h = valley();
        assert!(flood_fill(&h, 7, 4, (7, 0), 3.0).is_none());
        assert!(flood_fill(&h, 7, 4, (0, 0), 3.0).is_none(), "the seed stands above the water");
        assert!(flood_fill(&h, 7, 4, (2, 2), f64::NAN).is_none());
        assert!(flood_fill(&h, 6, 4, (2, 2), 3.0).is_none(), "the grid's size is the heights'");
        h[2 * 7 + 2] = f32::NAN;
        assert!(flood_fill(&h, 7, 4, (2, 2), 3.0).is_none());
        // Water doesn't enter a cell of no height.
        h[2 * 7 + 2] = 0.0;
        h[7 + 2] = f32::NAN;
        let f = flood_fill(&h, 7, 4, (2, 2), 1.5).unwrap();
        assert_eq!(f.cells, 1);
    }
}
