//! A scene's grid of eastings, northings and heights (ARutGIS, [`Scene::grid`]): lines on the
//! box's far walls up to its top, on the block's near sides up to the ground, on its base and over
//! the ground, each drawn only where nothing nearer hides it, and numbered along the box's near
//! edges. Eastings and northings are UTM metres in the WGS 84 zone of the scene's middle; heights
//! are metres above sea level, not exaggerated.

use super::{Built, Eye, Target, fmt_metres_to, ruler_corner, step_decimals};
use crate::canvas::Canvas;
use rustgis_geom::utm::{to_utm_in_zone, utm_zone};
use rustgis_geom::{Coord, merc_to_lonlat};
use rustgis_map::Rgba;

/// How a scene's grid is drawn: metres between its eastings and northings, and true metres
/// between its heights; None for a round step that fits.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridLook {
    /// Metres between its eastings and northings ([`Scene::grid_step`]).
    pub step: Option<f64>,
    /// True metres between its heights ([`Scene::grid_z_step`]).
    pub z_step: Option<f64>,
}

impl GridLook {
    /// Its heights at the ruler's step when it asks none of its own and the ruler asks one, so
    /// the two mark the same heights.
    pub fn or_ruler(self, ruler_step: Option<f64>) -> GridLook {
        GridLook { z_step: self.z_step.or(ruler_step), ..self }
    }
}

/// About how many lines a grid of a round step draws across the box's longer side, and up it.
const LINES_ACROSS: f64 = 6.0;
const LINES_UP: f64 = 5.0;
/// The most lines it draws each way: a step asked too fine for the box is drawn at a multiple.
const MAX_LINES: f64 = 200.0;
/// Points a line along the ground or the base is followed at, across the box.
const SAMPLES: usize = 32;
/// How much nearer than a line what is drawn there may be and still not hide it: a line on a
/// face is as near as the face.
const HIDES: f32 = 1.02;

/// The scene's ground metres, from its middle, in UTM metres of the WGS 84 zone there.
struct Utm {
    zone: u8,
    north: bool,
    origin: (f64, f64, f64),
}

impl Utm {
    fn of(b: &Built) -> Option<Utm> {
        let (cx, cy, k) = b.origin;
        let middle = merc_to_lonlat(Coord::new(cx, cy));
        let zone = utm_zone(middle)?;
        (k > 0.0).then_some(Utm { zone, north: middle.y >= 0.0, origin: b.origin })
    }

    /// The easting and northing of the scene's point (`x`, `y`).
    fn en(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (cx, cy, k) = self.origin;
        to_utm_in_zone(merc_to_lonlat(Coord::new(cx + x / k, cy + y / k)), self.zone, self.north)
    }

    /// Where along the line `fixed` (the scene's y, or with `along_y` its x) the easting (or,
    /// with `along_y`, the northing) is `v`: found from the line's ends, then by Newton's steps,
    /// as the zone's lines are nearly straight over a scene.
    fn at(&self, fixed: f64, v: f64, half: f64, along_y: bool) -> Option<f64> {
        let read = |t: f64| {
            let (e, n) = if along_y { self.en(fixed, t)? } else { self.en(t, fixed)? };
            Some(if along_y { n } else { e })
        };
        let (a, z) = (read(-half)?, read(half)?);
        if (z - a).abs() < 1e-9 {
            return None;
        }
        let mut t = -half + (v - a) / (z - a) * 2.0 * half;
        for _ in 0..3 {
            let (r, r1) = (read(t)?, read(t + 1.0)?);
            let d = r1 - r;
            if d.abs() < 1e-12 {
                break;
            }
            t += (v - r) / d;
        }
        t.is_finite().then_some(t)
    }
}

/// The round step (1, 2 or 5 times a power of ten) at least `least`.
fn round_step(least: f64) -> f64 {
    let least = least.max(1e-3);
    let ten = 10f64.powf(least.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * ten).find(|s| *s >= least * (1.0 - 1e-9)).unwrap_or(10.0 * ten)
}

/// The step lines are drawn at over `span`: the one asked (a multiple of it when that would be
/// more than [`MAX_LINES`]), else a round one giving about `lines`.
fn step_over(span: f64, asked: Option<f64>, lines: f64) -> f64 {
    match asked.filter(|a| a.is_finite() && *a > 0.0) {
        Some(a) => a * (span / a / MAX_LINES).ceil().max(1.0),
        None => round_step(span / lines),
    }
}

/// The multiples of `step` from `lo` to `hi`.
fn values(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut i = (lo / step - 1e-9).ceil();
    while i * step <= hi + 1e-9 && out.len() < MAX_LINES as usize + 1 {
        out.push(i * step);
        i += 1.0;
    }
    out
}

/// The scene's lines in the picture: each a run of points, kept where nothing nearer hides it.
struct Tracer<'a> {
    eye: &'a Eye,
    t: &'a Target,
    ss: usize,
    w: f32,
    h: f32,
}

impl Tracer<'_> {
    /// The parts of the polyline `pts` (the scene's points) that show, in the picture's pixels.
    fn seen(&self, pts: &[[f64; 3]]) -> Vec<Vec<(f32, f32)>> {
        let mut runs: Vec<Vec<(f32, f32)>> = Vec::new();
        let mut run: Vec<(f32, f32)> = Vec::new();
        let end = |run: &mut Vec<(f32, f32)>, runs: &mut Vec<Vec<(f32, f32)>>| {
            if run.len() > 1 {
                runs.push(std::mem::take(run));
            } else {
                run.clear();
            }
        };
        for pair in pts.windows(2) {
            let (a, b) = (self.eye.view(pair[0].map(|c| c as f32)), self.eye.view(pair[1].map(|c| c as f32)));
            let near = self.eye.near;
            if a[2] < near && b[2] < near {
                end(&mut run, &mut runs);
                continue;
            }
            // Cut at the near plane.
            let cut = |p: [f64; 3], q: [f64; 3]| {
                if p[2] >= near {
                    p
                } else {
                    let s = (near - p[2]) / (q[2] - p[2]);
                    [p[0] + (q[0] - p[0]) * s, p[1] + (q[1] - p[1]) * s, near]
                }
            };
            let (a, b) = (cut(a, b), cut(b, a));
            let (pa, pb) = (self.eye.project(a), self.eye.project(b));
            let long = (pb.0 - pa.0).hypot(pb.1 - pa.1);
            let n = ((long / 1.5).ceil() as usize).clamp(1, 4000);
            for i in 0..=n {
                let s = i as f64 / n as f64;
                let v = [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s, a[2] + (b[2] - a[2]) * s];
                let (x, y, iz) = self.eye.project(v);
                if self.shows(x, y, iz) {
                    if i > 0 || run.is_empty() {
                        run.push((x, y));
                    }
                } else {
                    end(&mut run, &mut runs);
                }
            }
        }
        end(&mut run, &mut runs);
        runs
    }

    /// Whether a point of the picture at 1 / depth `iz` is on it and nothing nearer is drawn there.
    fn shows(&self, x: f32, y: f32, iz: f32) -> bool {
        if !(x >= 0.0 && y >= 0.0 && x < self.w && y < self.h) {
            return false;
        }
        let (sx, sy) = ((x as usize * self.ss).min(self.t.w - 1), (y as usize * self.ss).min(self.t.h - 1));
        self.t.depth[sy * self.t.w + sx] <= iz * HIDES
    }
}

/// A label for the picture: its text, where, and which way from there it is written.
struct Number {
    text: String,
    at: (f32, f32),
    align: crate::text::Align,
    /// Moved off `at` this way (a unit on the picture), as far as it is long or high that way
    /// and `gap` pixels more: out from the box's foot. None: written at `at`.
    out: Option<((f32, f32), f32)>,
    /// Placed before those of a higher rank, which are left out where it is: the heights' (0),
    /// the eastings' and northings' (1), the axes' names (2).
    rank: u8,
}

/// Draws the grid of `b` seen by `eye` (the picture's pixels; `target` the drawing it was made
/// on, `ss` times as fine) on `canvas`, `ppp` pixels a point.
pub(super) fn draw(canvas: &mut Canvas, b: &Built, eye: &Eye, (target, ss): (&Target, usize), ppp: f32, look: GridLook) {
    let (w, h) = (eye.centre.0 * 2.0, eye.centre.1 * 2.0);
    let tracer = Tracer { eye, t: target, ss, w: w as f32, h: h as f32 };
    let (hw, hh) = b.half;
    let (base, top) = b.z_range;
    if !(hw > 0.0 && hh > 0.0 && top > base) {
        return;
    }
    // Which of the box's sides face the eye (the near ones, whose block side shows): the others
    // are far walls, drawn up to the box's top.
    let near_x = |sx: f64| sx * eye.at[0] > hw;
    let near_y = |sy: f64| sy * eye.at[1] > hh;
    // The side whose numbers are written along its foot: the nearer of each pair.
    let (fx, fy) = (if eye.at[0] >= 0.0 { hw } else { -hw }, if eye.at[1] >= 0.0 { hh } else { -hh });
    let mut lines: Vec<Vec<[f64; 3]>> = Vec::new();
    let mut numbers: Vec<Number> = Vec::new();
    let px = 11.0 * ppp;
    // Out from the box on the picture: away from the middle of its base.
    let middle_base = {
        let v = eye.view([0.0, 0.0, base as f32]);
        (v[2] >= eye.near).then(|| eye.project(v))
    };
    let outward = |p: (f32, f32, f32)| {
        let (dx, dy) = middle_base.map_or((0.0, 1.0), |m| (p.0 - m.0, p.1 - m.1));
        let l = dx.hypot(dy);
        if l > 1e-3 { (dx / l, dy / l) } else { (0.0, 1.0) }
    };
    let project = |p: [f64; 3]| {
        let v = eye.view(p.map(|c| c as f32));
        (v[2] >= eye.near).then(|| eye.project(v))
    };
    // The ground along a side, split where it dips below `z`: a height's line on a near side runs
    // only on the block, under the ground.
    let under_ground = |pts: Vec<[f64; 3]>| -> Vec<Vec<[f64; 3]>> {
        let mut out: Vec<Vec<[f64; 3]>> = vec![Vec::new()];
        for p in pts {
            if p[2] <= b.ground_at(p[0], p[1]) + 1e-6 {
                if let Some(r) = out.last_mut() {
                    r.push(p);
                }
            } else if out.last().is_some_and(|r| !r.is_empty()) {
                out.push(Vec::new());
            }
        }
        out.into_iter().filter(|r| r.len() > 1).collect()
    };
    let wall_top = |x: f64, y: f64, near: bool| if near { b.ground_at(x, y) } else { top };
    // Eastings and northings.
    if let Some(utm) = Utm::of(b) {
        let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
        let en: Vec<(f64, f64)> = corners.iter().filter_map(|&(x, y)| utm.en(x, y)).collect();
        if en.len() == 4 {
            let (e0, e1) = (en.iter().map(|p| p.0).fold(f64::INFINITY, f64::min), en.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max));
            let (n0, n1) = (en.iter().map(|p| p.1).fold(f64::INFINITY, f64::min), en.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max));
            let step = step_over((e1 - e0).max(n1 - n0), look.step, LINES_ACROSS);
            let decimals = step_decimals(step);
            // The widest number along each foot, eastings' and northings', for the titles to clear.
            let mut widest = [0.0f32; 2];
            for (along_y, lo, hi) in [(false, e0, e1), (true, n0, n1)] {
                // Eastings are lines up the box (fixed x for each y); northings across it.
                let (half_t, half_f) = if along_y { (hh, hw) } else { (hw, hh) };
                let point = |f: f64, t: f64, z: f64| if along_y { [f, t, z] } else { [t, f, z] };
                for v in values(lo, hi, step) {
                    // Over the ground and on the base: followed across the box.
                    let (mut ground, mut floor) = (Vec::new(), Vec::new());
                    for i in 0..=SAMPLES {
                        let f = -half_f + 2.0 * half_f * i as f64 / SAMPLES as f64;
                        let Some(t) = utm.at(f, v, half_t, along_y) else { continue };
                        if t.abs() > half_t {
                            continue;
                        }
                        let [x, y, _] = point(f, t, 0.0);
                        ground.push([x, y, b.ground_at(x, y)]);
                        floor.push([x, y, base]);
                    }
                    lines.push(ground);
                    lines.push(floor);
                    // Up the two sides it meets.
                    for sign in [-1.0, 1.0] {
                        let f = sign * half_f;
                        let Some(t) = utm.at(f, v, half_t, along_y) else { continue };
                        if t.abs() > half_t {
                            continue;
                        }
                        let [x, y, _] = point(f, t, 0.0);
                        let near = if along_y { near_x(sign) } else { near_y(sign) };
                        lines.push(vec![[x, y, base], [x, y, wall_top(x, y, near)]]);
                        // Numbered along the foot of the nearer side.
                        let foot = if along_y { fx } else { fy };
                        if (f - foot).abs() < 1e-9
                            && let Some(p) = project([x, y, base])
                        {
                            let text = fmt_metres_to(v, decimals);
                            let wide = &mut widest[usize::from(along_y)];
                            *wide = wide.max(crate::text::measure(&text, px / 1.33, false));
                            numbers.push(Number {
                                text,
                                at: (p.0, p.1),
                                align: crate::text::Align::Center,
                                out: Some((outward(p), 4.0 * ppp)),
                                rank: 1,
                            });
                        }
                    }
                }
            }
            // What the numbers are, past the middle of each foot.
            let zone = format!("{}{}", utm.zone, if utm.north { "N" } else { "S" });
            for (title, p, wide) in
                [(format!("E (UTM {zone})"), [0.0, fy, base], widest[0]), (format!("N (UTM {zone})"), [fx, 0.0, base], widest[1])]
            {
                if let Some(p) = project(p) {
                    let dir = outward(p);
                    numbers.push(Number {
                        text: title,
                        at: (p.0, p.1),
                        align: crate::text::Align::Center,
                        out: Some((dir, 8.0 * ppp + dir.0.abs() * wide + dir.1.abs() * px * 1.2)),
                        rank: 2,
                    });
                }
            }
        }
    }
    // Heights: round the four sides, up to the ground on the near ones.
    let (lo, hi) = (b.height_of(base), b.height_of(top));
    let z_step = step_over(hi - lo, look.z_step, LINES_UP);
    let z_decimals = step_decimals(z_step);
    let heights = values(lo, hi, z_step);
    let sides = [
        ((-hw, -hh), (hw, -hh), near_y(-1.0)),
        ((hw, -hh), (hw, hh), near_x(1.0)),
        ((hw, hh), (-hw, hh), near_y(1.0)),
        ((-hw, hh), (-hw, -hh), near_x(-1.0)),
    ];
    for &v in &heights {
        let z = b.z_of(v);
        for &((x0, y0), (x1, y1), near) in &sides {
            let pts: Vec<[f64; 3]> =
                (0..=SAMPLES).map(|i| i as f64 / SAMPLES as f64).map(|s| [x0 + (x1 - x0) * s, y0 + (y1 - y0) * s, z]).collect();
            if near {
                lines.extend(under_ground(pts));
            } else {
                lines.push(pts);
            }
        }
    }
    // Numbered up a corner at the side of the box, not the ruler's: one where a near side meets a
    // far wall, its numbers out from the box's middle.
    let ruler_at = ruler_corner(b, eye);
    let side_corners: Vec<(f64, f64)> = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
        .into_iter()
        .filter(|&(x, y)| near_x(x.signum()) != near_y(y.signum()))
        .filter(|c| *c != ruler_at)
        .collect();
    // The one farthest in from the picture's sides, so its numbers fit beside it.
    let inside = |&(x, y): &(f64, f64)| project([x, y, base]).map_or(f32::NEG_INFINITY, |p| p.0.min(w as f32 - p.0));
    if let Some(&(cx, cy)) = side_corners.iter().max_by(|a, b| inside(a).total_cmp(&inside(b))) {
        let middle = project([0.0, 0.0, (base + top) / 2.0]).map_or(eye.centre.0 as f32, |p| p.0);
        let tick = 6.0 * ppp;
        let mut pb = tiny_skia::PathBuilder::new();
        // The corner itself, up to the box's top: the heights' axis.
        if let (Some(p0), Some(p1)) = (project([cx, cy, base]), project([cx, cy, top])) {
            pb.move_to(p0.0, p0.1);
            pb.line_to(p1.0, p1.1);
        }
        let mut last: Option<(f32, f32)> = None;
        for &v in &heights {
            let Some(p) = project([cx, cy, b.z_of(v)]) else { continue };
            let out = if p.0 >= middle { 1.0 } else { -1.0 };
            pb.move_to(p.0, p.1);
            pb.line_to(p.0 + out * tick, p.1);
            if last.is_none_or(|q| (q.1 - p.1).abs() >= px * 1.4) {
                let align = if out > 0.0 { crate::text::Align::Left } else { crate::text::Align::Right };
                numbers.push(Number {
                    out: None,
                    rank: 0,
                    text: format!("{} m", fmt_metres_to(v, z_decimals)),
                    at: (p.0 + out * (tick + 3.0 * ppp), p.1),
                    align,
                });
                last = Some((p.0, p.1));
            }
        }
        if let Some(p) = project([cx, cy, top]) {
            numbers.push(Number { text: "Z".into(), at: (p.0, p.1 - px * 1.2), align: crate::text::Align::Center, out: None, rank: 2 });
        }
        if let Some(path) = pb.finish() {
            stroke(canvas, &path, ppp, 1.2, 255);
        }
    }
    // The lines, thin and grey, then the numbers over them, none on another.
    let mut pb = tiny_skia::PathBuilder::new();
    for l in &lines {
        for run in tracer.seen(l) {
            pb.move_to(run[0].0, run[0].1);
            for p in &run[1..] {
                pb.line_to(p.0, p.1);
            }
        }
    }
    if let Some(path) = pb.finish() {
        stroke(canvas, &path, ppp, 0.8, 150);
    }
    let mut taken: Vec<[f32; 4]> = Vec::new();
    numbers.sort_by_key(|n| n.rank);
    for n in numbers {
        let wide = crate::text::measure(&n.text, px / 1.33, false);
        let at = match n.out {
            Some(((dx, dy), gap)) => {
                let far = gap + dx.abs() * wide / 2.0 + dy.abs() * px * 0.6;
                (n.at.0 + dx * far, n.at.1 + dy * far)
            }
            None => n.at,
        };
        let left = match n.align {
            crate::text::Align::Left => at.0,
            crate::text::Align::Right => at.0 - wide,
            crate::text::Align::Center => at.0 - wide / 2.0,
        };
        let r = [left - 2.0, at.1 - px * 0.6, left + wide + 2.0, at.1 + px * 0.6];
        let on = r[0] >= 0.0 && r[1] >= 0.0 && r[2] <= w as f32 && r[3] <= h as f32;
        if !on || taken.iter().any(|q| r[0] < q[2] && q[0] < r[2] && r[1] < q[3] && q[1] < r[3]) {
            continue;
        }
        taken.push(r);
        crate::text::text_at(canvas, &n.text, at.0, at.1, px, Rgba::rgb(0x20, 0x20, 0x20), false, n.align, true);
    }
}

/// `path` stroked dark grey, `width` points wide, `alpha` opaque.
fn stroke(canvas: &mut Canvas, path: &tiny_skia::Path, ppp: f32, width: f32, alpha: u8) {
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_skia::Color::from_rgba8(0x30, 0x30, 0x30, alpha));
    paint.anti_alias = true;
    let s = tiny_skia::Stroke { width: width * ppp, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
    canvas.stroke_path(path, &paint, &s, tiny_skia::Transform::identity(), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_steps_and_steps_asked_too_fine() {
        assert_eq!(round_step(1300.0), 2000.0);
        assert_eq!(round_step(0.3), 0.5);
        assert_eq!(step_over(6000.0, None, 6.0), 1000.0);
        assert_eq!(step_over(6000.0, Some(250.0), 6.0), 250.0);
        // 1 m over 1,000 km would be a million lines: every 5,000th.
        assert_eq!(step_over(1_000_000.0, Some(1.0), 6.0), 5000.0);
        assert_eq!(values(-120.0, 260.0, 100.0), [-100.0, 0.0, 100.0, 200.0]);
    }
}
