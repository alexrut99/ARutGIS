//! A map's 3D scene (ARutGIS, `rustgis_map::scene`) drawn on the CPU with a depth buffer: the
//! study area's ground as a block, the map draped over it, and features stood up in it. What
//! takes long (the heights, the draped map, the features' shapes) is worked out once by
//! [`build`]; [`draw`] then draws it from any camera, fast enough to turn it with the mouse.
//!
//! The scene's space is metres on the ground: x east and y north of the middle of the box, z up
//! (heights above sea level, exaggerated). Unlike the tilted map, nothing is drawn in order of
//! depth: every pixel keeps the nearest thing drawn on it, so wells under the ground stay under
//! it, and show through it when the ground is made clear.

use rustgis_data::Geometry;
use rustgis_data::raster::Raster;
use rustgis_geom::{Coord, lonlat_to_merc, merc_to_lonlat};
use rustgis_map::scene::{Scene, SceneCamera, SceneKind, SceneLayer};
use rustgis_map::{Camera, Map, Rgba};
use tiny_skia::Pixmap;

use crate::canvas::Canvas;

/// The camera's field of view, top to bottom (degrees).
const FOV: f64 = 40.0;
/// The most cells along the longer side of the ground's mesh.
pub const MAX_CELLS: usize = 400;
/// The fewest.
const MIN_CELLS: usize = 16;
/// The most features of a layer stood up in a scene: past it the rest are left out, said.
pub const MAX_FEATURES: usize = 20_000;
/// The most labels written.
const MAX_LABELS: usize = 2_000;
/// The light: from the north-west, about 38° up (as the map's hillshades are lit).
const LIGHT: [f64; 3] = [-0.5, 0.6, 0.62];
/// How lit the ground's underside is, the map seen through it from below.
const UNDERSIDE: f32 = 0.7;
/// How much darker than the sides the base is.
const BASE_SHADE: f32 = 0.6;
/// A water level's ring.
const WATER: [f32; 3] = [0.12, 0.53, 0.90];

/// How finely a scene is built.
#[derive(Clone, Copy, Debug)]
pub struct Detail {
    /// Pixels along the longer side of the map draped over the ground.
    pub drape_px: u32,
    /// The most cells along the longer side of the ground's mesh (no more than the DEM's own).
    pub cells: usize,
}

impl Detail {
    /// For turning the scene in the app.
    pub const SCREEN: Detail = Detail { drape_px: 2048, cells: 220 };
    /// For an image or an animation written to a file.
    pub const EXPORT: Detail = Detail { drape_px: 4096, cells: MAX_CELLS };
}

/// Triangles of one kind: positions in the scene's space (metres), and per vertex either a lit
/// colour or, draped, a light (first of `col`) and the drape's coordinates.
#[derive(Default)]
struct Mesh {
    pos: Vec<[f32; 3]>,
    col: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    tris: Vec<[u32; 3]>,
}

impl Mesh {
    fn vertex(&mut self, p: [f64; 3], col: [f32; 3]) -> u32 {
        self.pos.push(p.map(|v| v as f32));
        self.col.push(col);
        (self.pos.len() - 1) as u32
    }

    /// A flat-lit quad a, b, c, d (in turn round it), facing `n`.
    fn quad(&mut self, q: [[f64; 3]; 4], n: [f64; 3], color: [f32; 3]) {
        let c = lit(color, n);
        let i = q.map(|p| self.vertex(p, c));
        self.tris.push([i[0], i[1], i[2]]);
        self.tris.push([i[0], i[2], i[3]]);
    }
}

/// A label written beside a feature: at a point of the scene.
struct Label {
    text: String,
    at: [f64; 3],
}

/// What a stood-up feature is, for [`pick`]: its triangles in [`Built::solids`] are those
/// before `end` and after the one before it's.
struct Owner {
    end: usize,
    layer: String,
    label: Option<String>,
    /// A well's head (metres above sea level) and where it is: how deep a point of it is is
    /// measured from there, and how far under the ground from the ground over it.
    head: Option<(f64, (f64, f64))>,
}

/// A scene worked out for drawing: [`build`].
pub struct Built {
    /// Cells across and down the ground's mesh, and its heights (exaggerated metres) at the
    /// cells' corners, row by row from the north.
    cols: usize,
    rows: usize,
    heights: Vec<f32>,
    /// Half the box's width and height on the ground (metres).
    half: (f64, f64),
    /// The map draped over the ground, north up over the box.
    drape: Option<Pixmap>,
    ground: Mesh,
    sides: Mesh,
    solids: Mesh,
    labels: Vec<Label>,
    owners: Vec<Owner>,
    /// The middle of the box (Web Mercator metres), metres on the ground a Web Mercator metre,
    /// and how much heights are stretched: back from the scene's space to the map's.
    origin: (f64, f64, f64),
    ex: f64,
    /// The lowest ground's height above sea level (metres): above it heights are stretched by
    /// `ex`, below it by `dex` (1: true metres).
    datum: f64,
    dex: f64,
    /// How lit the ground's underside is.
    under: f32,
    /// The box's diagonal on the ground (metres): the camera's distance is a share of it.
    diag: f64,
    /// The height the camera turns about by default: the middle of the ground's.
    mid: f64,
    /// The lowest and highest the scene reaches (the scene's metres).
    pub z_range: (f64, f64),
    /// Metres from the lowest ground, or the deepest well when that is deeper, to the base: the
    /// scene's or its default.
    pub depth: f64,
    /// What was left out and why, and parts of the draped map that are missing: for a notice.
    pub notes: Vec<String>,
    /// The credits of the tile layers draped over the ground.
    pub credits: Vec<String>,
}

/// Works out `scene` of `map` for drawing at `detail`: the ground's heights from its DEM, `map`
/// drawn north up over the box (on the map given, whose camera and terrain are changed: pass a
/// copy), and its layers' features stood up. Errors: a scene that fails its check, or a DEM with
/// no heights in the box. A DEM, a layer or a field taken out of the map since the scene named
/// it is left out, said in [`Built::notes`].
pub fn build(mut map: Map, scene: &Scene, detail: Detail) -> Result<Built, String> {
    build_on(&mut map, scene, detail)
}

/// [`build`] on a copy of the map the caller keeps: its streamed layers' sources, read for the
/// scene, carry the failures of those that couldn't be (for an export's notices).
pub fn build_on(map: &mut Map, scene: &Scene, detail: Detail) -> Result<Built, String> {
    scene.check()?;
    // Cancel (an export's) is looked at between the steps.
    go_on()?;
    let [x0, y0, x1, y1] = scene.extent;
    let (cx, cy) = scene.center();
    let k = scene.ground_scale();
    let (gw, gh) = scene.ground_size();
    let half = (gw / 2.0, gh / 2.0);
    let ex = scene.exaggeration;
    let mut notes = Vec::new();

    // The ground's heights.
    // A DEM taken out of the map since leaves the ground flat, said.
    let dem: Option<std::sync::Arc<Raster>> = scene.dem.as_ref().and_then(|name| {
        let r = map.layer_by_name(name).and_then(|l| l.raster.as_ref()).and_then(|r| r.data.clone());
        if r.is_none() {
            notes.push(format!("the scene's DEM '{name}' isn't a raster layer in the map, so the ground is flat"));
        }
        r
    });
    let long = detail.cells.clamp(MIN_CELLS, MAX_CELLS);
    let mut along = long as f64;
    if let Some(r) = &dem
        && let Some([fx0, _, fx1, _]) = crate::terrain::raster_footprint(r)
        && r.width > 0
    {
        // No finer than the DEM's own cells across the box.
        let per_m = f64::from(r.width) / (fx1 - fx0).max(1e-9);
        along = along.min(((x1 - x0).max(y1 - y0) * per_m).ceil());
    }
    let along = (along as usize).clamp(MIN_CELLS, long);
    let (cols, rows) = if gw >= gh {
        (along, ((along as f64 * gh / gw).round() as usize).max(4))
    } else {
        (((along as f64 * gw / gh).round() as usize).max(4), along)
    };
    let mut heights = vec![f32::NAN; (cols + 1) * (rows + 1)];
    if let Some(r) = &dem {
        for j in 0..=rows {
            for i in 0..=cols {
                let m = Coord::new(x0 + (x1 - x0) * i as f64 / cols as f64, y1 - (y1 - y0) * j as f64 / rows as f64);
                if let Some(h) = crate::terrain::dem_height(r, m) {
                    heights[j * (cols + 1) + i] = (h * ex) as f32;
                }
            }
        }
        let known = heights.iter().filter(|h| h.is_finite()).count();
        if known == 0 {
            return Err(format!("{} has no heights inside the scene's extent", scene.dem.as_deref().unwrap_or_default()));
        }
        if known < heights.len() {
            notes.push(format!(
                "{} has no heights over part of the scene, drawn at its lowest there",
                scene.dem.as_deref().unwrap_or_default()
            ));
        }
        let low = heights.iter().copied().filter(|h| h.is_finite()).fold(f32::INFINITY, f32::min);
        for h in &mut heights {
            if !h.is_finite() {
                *h = low;
            }
        }
    } else {
        heights.fill(0.0);
    }
    go_on()?;
    let (g_lo, g_hi) = heights.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), h| (a.min(f64::from(*h)), b.max(f64::from(*h))));

    let mut built = Built {
        cols,
        rows,
        heights,
        half,
        drape: None,
        ground: Mesh::default(),
        sides: Mesh::default(),
        solids: Mesh::default(),
        labels: Vec::new(),
        owners: Vec::new(),
        origin: (cx, cy, k),
        ex,
        datum: g_lo / ex.max(1e-9),
        dex: scene.depth_exaggeration,
        under: UNDERSIDE,
        diag: gw.hypot(gh),
        mid: (g_lo + g_hi) / 2.0,
        z_range: (g_lo, g_hi),
        depth: 0.0,
        notes: Vec::new(),
        credits: Vec::new(),
    };

    // The features, before the sides: the base goes below the deepest well.
    let local = |lonlat: Coord| {
        let m = lonlat_to_merc(lonlat);
        ((m.x - cx) * k, (m.y - cy) * k)
    };
    let inside = |(x, y): (f64, f64)| x.abs() <= half.0 && y.abs() <= half.1;
    let auto_size = built.diag / 250.0;
    // The scale the map is draped at: a layer's features are those the drape draws.
    // Its longer side `drape_px` pixels; a very long, narrow box's shorter side is drawn at
    // least 16 pixels across, round its middle, and cut back to the box after.
    let per_px = (x1 - x0).max(y1 - y0) / f64::from(detail.drape_px);
    let (bw, bh) = ((x1 - x0) / per_px, (y1 - y0) / per_px);
    let (dw, dh) = ((bw.round() as u32).max(16), (bh.round() as u32).max(16));
    let camera = Camera::new(Coord::new(cx, cy), per_px);
    map.view.terrain = None;
    if map.projection != rustgis_map::Projection::WebMercator {
        map.set_projection(rustgis_map::Projection::WebMercator);
    }
    map.camera = camera;
    let view = crate::View { width: f64::from(dw), height: f64::from(dh), pixels_per_point: 1.0, camera, sky: false };
    // Streamed layers read the box first, waited for, as the drape reads them: those the map
    // draws there, and those the scene stands up that it doesn't (hidden, or out of their range).
    let lonlat = crate::view_lonlat(map, &view);
    let scale = camera.scale();
    let mut pending = map.sync_sources(&lonlat, scale, true);
    for l in map
        .layers
        .iter_mut()
        .filter(|l| l.is_streamed() && !(l.visible && l.in_scale_range(scale)) && scene.layers.iter().any(|sl| sl.layer == l.name))
    {
        pending |= l.sync_source(&lonlat, true);
    }
    if pending {
        notes.push("streamed layers were still reading the scene's area, so some features may be missing: build it again once the map has drawn them".to_string());
    }
    go_on()?;
    let at = Placing { local: &local, inside: &inside, auto_size, ex, scale };
    for sl in &scene.layers {
        stand_up(map, sl, &mut built, &at, &mut notes)?;
    }
    notes.extend(unread(map, scene));
    go_on()?;
    let lowest = built.z_range.0.min(g_lo);
    // The block is cut below what is lowest, in true metres (drawn stretched as what is under
    // the lowest ground is): by default a quarter of the relief as drawn, at least 50 m.
    let depth = scene.depth.unwrap_or_else(|| ((g_hi - g_lo) / 4.0).max(50.0));
    let base = lowest - depth * scene.depth_exaggeration;
    built.z_range.0 = base;
    built.depth = depth;
    ground_mesh(&mut built);
    sides_mesh(&mut built, base, scene.walls);

    // The map draped over the ground.
    match crate::render_placed(map, &view) {
        Ok(p) => {
            notes.extend(p.gaps);
            built.credits = p.credits;
            built.drape = Some(box_of(p.pixmap, bw, bh));
        }
        Err(e) => notes.push(format!("the map couldn't be draped over the ground: {e}")),
    }
    go_on()?;
    built.notes = notes;
    Ok(built)
}

/// A colour lit from [`LIGHT`] by a face facing `n`.
fn lit(c: [f32; 3], n: [f64; 3]) -> [f32; 3] {
    let s = shade(n, 0.45, 0.62) as f32;
    c.map(|v| (v * s).min(1.0))
}

/// How much light a face facing `n` takes: `ambient` and `diffuse` of the light along it.
fn shade(n: [f64; 3], ambient: f64, diffuse: f64) -> f64 {
    let l = norm(LIGHT);
    let n = norm(n);
    ambient + diffuse * dot(n, l).max(0.0) / l[2].max(0.1) * 0.62
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = dot(a, a).sqrt();
    if l > 0.0 { a.map(|v| v / l) } else { [0.0, 0.0, 1.0] }
}

fn rgb(c: Rgba) -> [f32; 3] {
    [c.r(), c.g(), c.b()].map(|v| f32::from(v) / 255.0)
}

impl Built {
    /// The ground's height (exaggerated metres) at a point of the scene, from its mesh.
    pub fn ground_at(&self, x: f64, y: f64) -> f64 {
        let (hw, hh) = self.half;
        let fx = ((x + hw) / (2.0 * hw) * self.cols as f64).clamp(0.0, self.cols as f64);
        let fy = ((hh - y) / (2.0 * hh) * self.rows as f64).clamp(0.0, self.rows as f64);
        let (i, j) = ((fx.floor() as usize).min(self.cols - 1), (fy.floor() as usize).min(self.rows - 1));
        let (tx, ty) = (fx - i as f64, fy - j as f64);
        let h = |i: usize, j: usize| f64::from(self.heights[j * (self.cols + 1) + i]);
        let top = h(i, j) + (h(i + 1, j) - h(i, j)) * tx;
        let bottom = h(i, j + 1) + (h(i + 1, j + 1) - h(i, j + 1)) * tx;
        top + (bottom - top) * ty
    }

    /// Where `h` metres above sea level lies in the scene: over the lowest ground (the relief),
    /// stretched by the exaggeration; under it (the block, and wells deeper than it), down from
    /// it stretched by the depth exaggeration (true metres at 1).
    pub fn z_of(&self, h: f64) -> f64 {
        let d = self.datum;
        if h >= d { h * self.ex } else { d * self.ex - (d - h) * self.dex }
    }

    /// The height above sea level (metres) of the scene's `z`: [`Built::z_of`] back.
    pub fn height_of(&self, z: f64) -> f64 {
        let (d, ex, dex) = (self.datum, self.ex.max(1e-9), self.dex.max(1e-9));
        let dz = d * self.ex;
        if z >= dz { z / ex } else { d - (dz - z) / dex }
    }

    /// The point of the scene at a corner of the ground's mesh.
    fn corner(&self, i: usize, j: usize) -> [f64; 3] {
        let (hw, hh) = self.half;
        [
            -hw + 2.0 * hw * i as f64 / self.cols as f64,
            hh - 2.0 * hh * j as f64 / self.rows as f64,
            f64::from(self.heights[j * (self.cols + 1) + i]),
        ]
    }
}

/// The part of a drape drawn round the box's middle that is the box, `bw` by `bh` pixels (no
/// less than one) at its middle: all of it unless a side was drawn wider than the box.
fn box_of(pm: Pixmap, bw: f64, bh: f64) -> Pixmap {
    let (w, h) = (pm.width(), pm.height());
    let side = |n: u32, along: f64| {
        let keep = (along.round() as u32).clamp(1, n);
        (((f64::from(n) - along) / 2.0).round().clamp(0.0, f64::from(n - keep)) as u32, keep)
    };
    let ((x, kw), (y, kh)) = (side(w, bw), side(h, bh));
    if (kw, kh) == (w, h) {
        return pm;
    }
    tiny_skia::IntRect::from_xywh(x as i32, y as i32, kw, kh).and_then(|r| pm.clone_rect(r)).unwrap_or(pm)
}

/// The ground's top: its mesh, lit by its slope, the map draped over it.
fn ground_mesh(b: &mut Built) {
    let (cols, rows) = (b.cols, b.rows);
    let (dx, dy) = (2.0 * b.half.0 / cols as f64, 2.0 * b.half.1 / rows as f64);
    let mut m = Mesh::default();
    for j in 0..=rows {
        for i in 0..=cols {
            let p = b.corner(i, j);
            let h = |i: usize, j: usize| f64::from(b.heights[j * (cols + 1) + i]);
            let (il, ir) = (i.saturating_sub(1), (i + 1).min(cols));
            let (ju, jd) = (j.saturating_sub(1), (j + 1).min(rows));
            let gx = (h(ir, j) - h(il, j)) / (dx * (ir - il) as f64);
            // Rows run south.
            let gy = (h(i, ju) - h(i, jd)) / (dy * (jd - ju) as f64);
            let s = shade([-gx, -gy, 1.0], 0.55, 0.47).clamp(0.3, 1.2) as f32;
            m.pos.push(p.map(|v| v as f32));
            m.col.push([s, s, s]);
            m.uv.push([i as f32 / cols as f32, j as f32 / rows as f32]);
        }
    }
    let at = |i: usize, j: usize| (j * (cols + 1) + i) as u32;
    for j in 0..rows {
        for i in 0..cols {
            // Anticlockwise seen from above.
            m.tris.push([at(i, j), at(i, j + 1), at(i + 1, j + 1)]);
            m.tris.push([at(i, j), at(i + 1, j + 1), at(i + 1, j)]);
        }
    }
    b.ground = m;
}

/// The block's four sides, from the ground's edge down to `base`, and its base.
fn sides_mesh(b: &mut Built, base: f64, walls: Rgba) {
    let c = rgb(walls);
    let mut m = Mesh::default();
    let (cols, rows) = (b.cols, b.rows);
    // Each side's corners along it, and the way it faces.
    let edges: [(Vec<(usize, usize)>, [f64; 3]); 4] = [
        ((0..=cols).map(|i| (i, 0)).collect(), [0.0, 1.0, 0.0]),
        ((0..=cols).map(|i| (i, rows)).collect(), [0.0, -1.0, 0.0]),
        ((0..=rows).map(|j| (0, j)).collect(), [-1.0, 0.0, 0.0]),
        ((0..=rows).map(|j| (cols, j)).collect(), [1.0, 0.0, 0.0]),
    ];
    for (edge, n) in edges {
        for w in edge.windows(2) {
            let (a, d) = (b.corner(w[0].0, w[0].1), b.corner(w[1].0, w[1].1));
            m.quad([a, [a[0], a[1], base], [d[0], d[1], base], d], n, c);
        }
    }
    // The base, unlit (the light is from above): the sides' colour, darker.
    let (hw, hh) = b.half;
    let dark = c.map(|v| v * BASE_SHADE);
    let i = [[-hw, hh, base], [-hw, -hh, base], [hw, -hh, base], [hw, hh, base]].map(|p| m.vertex(p, dark));
    m.tris.push([i[0], i[1], i[2]]);
    m.tris.push([i[0], i[2], i[3]]);
    b.sides = m;
}

/// The scene's streamed layers whose source couldn't be read (left out of it, or only partly
/// in it), in one note with a count, naming the first.
fn unread(map: &Map, scene: &Scene) -> Option<String> {
    let failed: Vec<(&str, &str)> = map
        .layers
        .iter()
        .filter(|l| scene.layers.iter().any(|sl| sl.layer == l.name))
        .filter_map(|l| Some((l.name.as_str(), l.source.as_ref()?.error.as_deref()?)))
        .collect();
    let (name, error) = failed.first()?;
    Some(match failed.len() {
        1 => format!("{name} may be missing from the scene, as its source couldn't be read: {error}"),
        n => format!("{n} layers may be missing from the scene, as their sources couldn't be read; the first, {name}: {error}"),
    })
}

/// How features are placed in the box.
struct Placing<'a> {
    /// Longitude and latitude to the box's metres about its middle.
    local: &'a dyn Fn(Coord) -> (f64, f64),
    /// Whether a point of the box's metres is inside it.
    inside: &'a dyn Fn((f64, f64)) -> bool,
    /// A well's or a point's width when its layer gives none (metres).
    auto_size: f64,
    /// The vertical exaggeration.
    ex: f64,
    /// The scale denominator the map is draped at.
    scale: f64,
}

/// Stands up the features of one of a scene's layers: those the map draws (its definition
/// query, the time slider's window and its display filters at the drape's scale). Cancel is
/// looked at between features. Errors: Cancel.
fn stand_up(map: &Map, sl: &SceneLayer, b: &mut Built, at: &Placing, notes: &mut Vec<String>) -> Result<(), String> {
    let Placing { local, inside, auto_size, ex, scale } = *at;
    let name = &sl.layer;
    // A layer or a field taken out of the map since: the layer is left out, said.
    let Some(layer) = map.layer_by_name(name) else {
        notes.push(format!("the scene stands up '{name}', which isn't a layer of the map any more"));
        return Ok(());
    };
    let ds = &layer.dataset;
    let field = |f: &Option<String>| -> Result<Option<usize>, String> {
        f.as_deref()
            .map(|f| ds.field_index(f).ok_or_else(|| format!("{name} has no field '{f}' any more, so it is left out of the scene")))
            .transpose()
    };
    let fields = (|| {
        Ok::<_, String>((
            field(&sl.depth)?,
            field(&sl.top)?,
            field(&sl.water)?,
            field(&sl.elevation)?,
            field(&sl.height)?,
            field(&sl.label)?,
        ))
    })();
    let (depth, top, water, elevation, height, label) = match fields {
        Ok(f) => f,
        Err(e) => {
            notes.push(e);
            return Ok(());
        }
    };
    let number = |row: usize, f: Option<usize>| f.and_then(|f| ds.value(row, f).as_f64()).filter(|v| v.is_finite());
    let size = sl.size.unwrap_or(auto_size);
    let (mut shown, mut missing, mut over, mut roofless, mut unlabelled) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let drawn = layer.drawn_rows(scale);
    for row in 0..ds.features.len() {
        if !drawn.get(row).copied().unwrap_or(true) {
            continue;
        }
        go_on()?;
        let Some(g) = ds.features.get(row).and_then(|f| f.geometry.as_ref()) else { continue };
        let color = sl.color.unwrap_or_else(|| {
            let s = layer.renderer.symbol_for(ds, row);
            if s.fill.a() > 0 { s.fill } else { s.stroke }
        });
        let color = rgb(color);
        let mut placed: Option<[f64; 3]> = None;
        let owned = label.map(|f| ds.value(row, f).to_string()).filter(|t| !t.is_empty());
        match (sl.kind, g) {
            (SceneKind::Well | SceneKind::Point, Geometry::Point(_) | Geometry::MultiPoint(_)) => {
                let pts: Vec<Coord> = match g {
                    Geometry::Point(c) => vec![*c],
                    Geometry::MultiPoint(v) => v.clone(),
                    _ => Vec::new(),
                };
                for p in pts {
                    let at = local(p);
                    if !inside(at) {
                        continue;
                    }
                    if shown >= MAX_FEATURES {
                        over += 1;
                        continue;
                    }
                    let ground = b.ground_at(at.0, at.1);
                    if sl.kind == SceneKind::Well {
                        let Some(d) = number(row, depth).filter(|d| (0.0..=rustgis_map::scene::MAX_HEIGHT).contains(d)) else {
                            missing += 1;
                            continue;
                        };
                        // Down from its head: through the relief stretched as it is, below the
                        // lowest ground in true metres.
                        let top_m = number(row, top).unwrap_or(ground / ex.max(1e-9));
                        let (head, bottom) = (b.z_of(top_m), b.z_of(top_m - d));
                        let r = size / 2.0;
                        // Its casing stands a little above the head, to show where it is from above.
                        tube(&mut b.solids, at, head + r, bottom, r, color, 14);
                        if let Some(w) = number(row, water).filter(|w| *w >= 0.0 && *w <= d) {
                            let z = b.z_of(top_m - w);
                            tube(&mut b.solids, at, z + r * 0.6, z - r * 0.6, r * 1.7, WATER, 14);
                        }
                        b.z_range.0 = b.z_range.0.min(bottom);
                        b.z_range.1 = b.z_range.1.max(head);
                        // Each of a multipoint's wells keeps its own head and middle.
                        b.owners.push(Owner {
                            end: b.solids.tris.len(),
                            layer: name.clone(),
                            label: owned.clone(),
                            head: Some((top_m, at)),
                        });
                        placed = Some([at.0, at.1, head.max(ground) + r * 3.0]);
                    } else {
                        let z = match elevation {
                            Some(_) => match number(row, elevation) {
                                Some(e) => b.z_of(e),
                                None => {
                                    missing += 1;
                                    continue;
                                }
                            },
                            // Above the ground stretched, under the lowest ground true metres.
                            None => b.z_of(b.height_of(ground) + sl.offset) + size / 2.0,
                        };
                        ball(&mut b.solids, [at.0, at.1, z], size / 2.0, color);
                        b.z_range.0 = b.z_range.0.min(z - size / 2.0);
                        b.z_range.1 = b.z_range.1.max(z + size / 2.0);
                        placed = Some([at.0, at.1, z + size]);
                    }
                    shown += 1;
                }
            }
            (SceneKind::Extrude, Geometry::Polygon(polys)) => {
                for poly in polys {
                    let Some(outer) = poly.first() else { continue };
                    let ring = |r: &Vec<Coord>| -> Vec<(f64, f64)> { r.iter().map(|c| local(*c)).collect() };
                    let holes: Vec<Vec<(f64, f64)>> = poly[1..].iter().map(ring).collect();
                    // Cut to the box: one crossing it or round it stands up as far as it goes, a
                    // hole cut by its edge a notch in what is left.
                    for (o, holes) in cut_to_box(&ring(outer), &holes, b.half) {
                        if shown >= MAX_FEATURES {
                            over += 1;
                            continue;
                        }
                        let tall = match height {
                            Some(_) => match number(row, height) {
                                Some(h) if h >= 0.0 => h,
                                _ => {
                                    missing += 1;
                                    break;
                                }
                            },
                            None => sl.height_m,
                        };
                        let low = o.iter().map(|p| b.ground_at(p.0, p.1)).fold(f64::INFINITY, f64::min);
                        let foot = b.height_of(low) + sl.offset;
                        let (z0, z1) = (b.z_of(foot), b.z_of(foot + tall));
                        if !raised(&mut b.solids, &o, &holes, z0, z1, color) {
                            roofless += 1;
                        }
                        b.z_range.1 = b.z_range.1.max(z1);
                        let (sx, sy) = o.iter().fold((0.0, 0.0), |(a, c), p| (a + p.0, c + p.1));
                        placed = Some([sx / o.len() as f64, sy / o.len() as f64, z1 + auto_size]);
                        shown += 1;
                    }
                }
            }
            _ => {}
        }
        if placed.is_some() && b.owners.last().is_none_or(|o| o.end < b.solids.tris.len()) {
            b.owners.push(Owner { end: b.solids.tris.len(), layer: name.clone(), label: owned, head: None });
        }
        if let (Some(at), Some(f)) = (placed, label) {
            let text = ds.value(row, f).to_string();
            // An empty value has no label to show; one over the limit is counted.
            match (text.is_empty(), b.labels.len() < MAX_LABELS) {
                (true, _) => {}
                (false, true) => b.labels.push(Label { text, at }),
                (false, false) => unlabelled += 1,
            }
        }
    }
    let what = match sl.kind {
        SceneKind::Well => "wells",
        SceneKind::Point => "points",
        SceneKind::Extrude => "polygons",
    };
    if missing > 0 {
        let why = match sl.kind {
            SceneKind::Well => "no depth (or one out of range)",
            SceneKind::Point => "no height",
            SceneKind::Extrude => "no height",
        };
        notes.push(format!("{missing} {what} of {name} have {why}, so they are left out"));
    }
    if roofless > 0 {
        notes.push(format!(
            "{roofless} polygons of {name} have too many corners (over {}) or touch themselves or their holes, so they stand up without a roof",
            rustgis_geom::triangulate::MAX_VERTICES
        ));
    }
    if over > 0 {
        notes.push(format!("{name}: only the first {MAX_FEATURES} {what} are stood up; {over} more are left out"));
    }
    if unlabelled > 0 {
        notes.push(format!("the scene shows at most {MAX_LABELS} labels, so {unlabelled} {what} of {name} are without theirs"));
    }
    Ok(())
}

/// A polygon (its outer ring and holes, open or closed) cut to the box `half` (half its width
/// and height, about the middle): the pieces of it inside the box, each its outer ring and holes,
/// open. A hole the box's edge cuts becomes part of a piece's outline.
fn cut_to_box(outer: &[(f64, f64)], holes: &[Vec<(f64, f64)>], half: (f64, f64)) -> Vec<(Vec<(f64, f64)>, Vec<Vec<(f64, f64)>>)> {
    use geo::BooleanOps;
    let line = |r: &[(f64, f64)]| geo::LineString::from(r.to_vec());
    if outer.len() < 3 || !outer.iter().chain(holes.iter().flatten()).all(|p| p.0.is_finite() && p.1.is_finite()) {
        return Vec::new();
    }
    let poly = geo::Polygon::new(line(outer), holes.iter().filter(|h| h.len() >= 3).map(|h| line(h)).collect());
    let bx = geo::Rect::new(geo::coord! { x: -half.0, y: -half.1 }, geo::coord! { x: half.0, y: half.1 }).to_polygon();
    let open = |r: &geo::LineString<f64>| -> Vec<(f64, f64)> {
        let mut v: Vec<(f64, f64)> = r.0.iter().map(|c| (c.x, c.y)).collect();
        if v.len() > 1 && v.first() == v.last() {
            v.pop();
        }
        v
    };
    poly.intersection(&bx)
        .0
        .iter()
        .map(|p| (open(p.exterior()), p.interiors().iter().map(open).filter(|h| h.len() >= 3).collect()))
        .filter(|(o, _)| o.len() >= 3)
        .collect()
}

/// An upright tube of `sides` sides about `at`, from `z_top` down to `z_bottom`, closed.
fn tube(m: &mut Mesh, at: (f64, f64), z_top: f64, z_bottom: f64, r: f64, color: [f32; 3], sides: usize) {
    let p = |a: f64, z: f64| [at.0 + r * a.cos(), at.1 + r * a.sin(), z];
    let step = std::f64::consts::TAU / sides as f64;
    for s in 0..sides {
        let (a, b) = (s as f64 * step, (s + 1) as f64 * step);
        let mid = (a + b) / 2.0;
        m.quad([p(a, z_top), p(a, z_bottom), p(b, z_bottom), p(b, z_top)], [mid.cos(), mid.sin(), 0.0], color);
    }
    for (z, n) in [(z_top, 1.0), (z_bottom, -1.0)] {
        let c = lit(color, [0.0, 0.0, n]);
        let centre = m.vertex([at.0, at.1, z], c);
        for s in 0..sides {
            let (a, b) = (m.vertex(p(s as f64 * step, z), c), m.vertex(p((s + 1) as f64 * step, z), c));
            m.tris.push([centre, a, b]);
        }
    }
}

/// A ball of radius `r` about `at`: an icosahedron split once.
fn ball(m: &mut Mesh, at: [f64; 3], r: f64, color: [f32; 3]) {
    let t = (1.0 + 5f64.sqrt()) / 2.0;
    let mut v: Vec<[f64; 3]> = [
        [-1.0, t, 0.0],
        [1.0, t, 0.0],
        [-1.0, -t, 0.0],
        [1.0, -t, 0.0],
        [0.0, -1.0, t],
        [0.0, 1.0, t],
        [0.0, -1.0, -t],
        [0.0, 1.0, -t],
        [t, 0.0, -1.0],
        [t, 0.0, 1.0],
        [-t, 0.0, -1.0],
        [-t, 0.0, 1.0],
    ]
    .into_iter()
    .map(norm)
    .collect();
    let faces: [[usize; 3]; 20] = [
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    let mut tris = Vec::with_capacity(80);
    for [a, b, c] in faces {
        let mut mid = |i: usize, j: usize| {
            v.push(norm([(v[i][0] + v[j][0]) / 2.0, (v[i][1] + v[j][1]) / 2.0, (v[i][2] + v[j][2]) / 2.0]));
            v.len() - 1
        };
        let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
        tris.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
    }
    let first = m.pos.len() as u32;
    for n in &v {
        m.pos.push([at[0] + r * n[0], at[1] + r * n[1], at[2] + r * n[2]].map(|x| x as f32));
        m.col.push(lit(color, *n));
    }
    m.tris.extend(tris.into_iter().map(|t| t.map(|i| first + i as u32)));
}

/// A polygon raised from `z0` to `z1`: its roof and its walls. `false` when the roof couldn't be
/// made (too many corners, or a ring touching itself or a hole), the walls stood up still.
fn raised(m: &mut Mesh, outer: &[(f64, f64)], holes: &[Vec<(f64, f64)>], z0: f64, z1: f64, color: [f32; 3]) -> bool {
    // A ring finished as a fan (touching itself or a hole) can spill out of it: no roof then.
    let roof = rustgis_geom::triangulate::triangulate_reporting(outer, holes).filter(|(_, fan)| !fan).map(|(t, _)| t);
    let roofed = roof.is_some();
    if let Some(tris) = roof {
        let c = lit(color, [0.0, 0.0, 1.0]);
        let first = m.pos.len() as u32;
        for p in outer.iter().chain(holes.iter().flatten()) {
            m.vertex([p.0, p.1, z1], c);
        }
        m.tris.extend(tris.into_iter().map(|t| t.map(|i| first + i as u32)));
    }
    if z1 - z0 <= 0.0 {
        return roofed;
    }
    for ring in std::iter::once(outer).chain(holes.iter().map(Vec::as_slice)) {
        for i in 0..ring.len() {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            // Its normal either way: walls are drawn from both sides.
            let n = [b.1 - a.1, a.0 - b.0, 0.0];
            m.quad([[a.0, a.1, z1], [a.0, a.1, z0], [b.0, b.1, z0], [b.0, b.1, z1]], n, color);
        }
    }
    roofed
}

/// Where the eye is and how it looks, for a picture `w` × `h` pixels.
struct Eye {
    at: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    fwd: [f64; 3],
    /// Pixels a unit of the picture plane at a unit's distance spans.
    focal: f64,
    centre: (f64, f64),
    near: f64,
}

impl Eye {
    fn of(b: &Built, cam: &SceneCamera, w: f64, h: f64) -> Eye {
        let (th, ph) = (cam.heading.to_radians(), cam.tilt.to_radians());
        let fwd = [th.sin() * ph.sin(), th.cos() * ph.sin(), -ph.cos()];
        let right = [th.cos(), -th.sin(), 0.0];
        let up = cross(right, fwd);
        let dist = cam.zoom * b.diag;
        let target = [cam.target[0], cam.target[1], b.mid + cam.target[2]];
        let at = [target[0] - dist * fwd[0], target[1] - dist * fwd[1], target[2] - dist * fwd[2]];
        Eye {
            at,
            right,
            up,
            fwd,
            focal: (h / 2.0) / (FOV.to_radians() / 2.0).tan(),
            centre: (w / 2.0, h / 2.0),
            near: (dist * 0.002).max(0.05),
        }
    }

    /// A point of the scene in the eye's space: right, up and ahead.
    fn view(&self, p: [f32; 3]) -> [f64; 3] {
        let d = [f64::from(p[0]) - self.at[0], f64::from(p[1]) - self.at[1], f64::from(p[2]) - self.at[2]];
        [dot(d, self.right), dot(d, self.up), dot(d, self.fwd)]
    }

    /// A point of the eye's space on the picture (pixels), and 1 / its depth.
    fn project(&self, v: [f64; 3]) -> (f32, f32, f32) {
        let iz = 1.0 / v[2];
        ((self.centre.0 + v[0] * iz * self.focal) as f32, (self.centre.1 - v[1] * iz * self.focal) as f32, iz as f32)
    }
}

/// A vertex on the picture: where, 1 / its depth, and what it carries over 1 / its depth
/// (colour or light, and the drape's coordinates), for perspective.
#[derive(Clone, Copy)]
struct Sv {
    x: f32,
    y: f32,
    iz: f32,
    a: [f32; 5],
}

/// What is being drawn into.
struct Target {
    w: usize,
    h: usize,
    rgb: Vec<[f32; 3]>,
    /// 1 / depth of what is nearest on each pixel (0: nothing).
    depth: Vec<f32>,
    /// 1 / depth of the nearest clear surface on each pixel, the only one laid over it; infinite
    /// once laid, so a pixel on two triangles' shared edge isn't laid twice.
    clear: Vec<f32>,
}

#[derive(Clone, Copy)]
enum How<'a> {
    /// Kept where nearest, and nearest from then on.
    Solid,
    /// Kept as the nearest clear surface where it is ([`Target::clear`]), nothing drawn.
    Mark,
    /// Laid over what is behind it, `alpha` of it, where it is the nearest clear surface,
    /// leaving what is nearest as it was.
    Clear(f32),
    /// Draped: the picture's colours by the light, or by `under` seen from below.
    Draped(&'a Pixmap, f32, Option<f32>),
}

/// Rows of a triangle drawn between looks at Cancel.
const ROWS_PER_LOOK: usize = 64;
/// Triangles drawn between looks at Cancel.
const TRIS_PER_LOOK: usize = 1024;

/// Whether to go on: Cancel (an export's) not pressed.
fn go_on() -> Result<(), String> {
    rustgis_data::progress::check().map_err(|e| e.to_string())
}

impl Target {
    fn tri(&mut self, v: [Sv; 3], how: How) -> Result<(), String> {
        let area = (v[1].x - v[0].x) * (v[2].y - v[0].y) - (v[1].y - v[0].y) * (v[2].x - v[0].x);
        if area.is_nan() || area.abs() <= 1e-9 {
            return Ok(());
        }
        // Anticlockwise in the scene seen from above is clockwise on the picture (y down).
        let below = area > 0.0;
        let minx = v.iter().map(|p| p.x).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
        let maxx = (v.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max).ceil() as isize).min(self.w as isize - 1);
        let miny = v.iter().map(|p| p.y).fold(f32::INFINITY, f32::min).floor().max(0.0) as usize;
        let maxy = (v.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max).ceil() as isize).min(self.h as isize - 1);
        if maxx < 0 || maxy < 0 {
            return Ok(());
        }
        let inv = 1.0 / area;
        let edge = |a: &Sv, b: &Sv, x: f32, y: f32| (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x);
        for py in miny..=maxy as usize {
            if (py - miny) % ROWS_PER_LOOK == ROWS_PER_LOOK - 1 {
                go_on()?;
            }
            let y = py as f32 + 0.5;
            let row = py * self.w;
            for px in minx..=maxx as usize {
                let x = px as f32 + 0.5;
                let w0 = edge(&v[1], &v[2], x, y) * inv;
                let w1 = edge(&v[2], &v[0], x, y) * inv;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let iz = w0 * v[0].iz + w1 * v[1].iz + w2 * v[2].iz;
                let i = row + px;
                if iz <= self.depth[i] {
                    continue;
                }
                let at = |k: usize| (w0 * v[0].a[k] + w1 * v[1].a[k] + w2 * v[2].a[k]) / iz;
                match how {
                    How::Solid => {
                        self.rgb[i] = [at(0), at(1), at(2)];
                        self.depth[i] = iz;
                    }
                    How::Mark => {
                        self.clear[i] = self.clear[i].max(iz);
                    }
                    How::Clear(alpha) => {
                        if iz < self.clear[i] * (1.0 - 1e-5) {
                            continue;
                        }
                        self.clear[i] = f32::INFINITY;
                        let c = [at(0), at(1), at(2)];
                        let d = &mut self.rgb[i];
                        *d = [0, 1, 2].map(|k| d[k] + (c[k] - d[k]) * alpha);
                    }
                    How::Draped(pm, under, alpha) => {
                        // Seen from below, the map shows through, dimmer.
                        let light = if below { under } else { at(0) };
                        let c = sample(pm, at(3), at(4)).map(|c| (c * light).min(1.0));
                        match alpha {
                            None => {
                                self.rgb[i] = c;
                                self.depth[i] = iz;
                            }
                            Some(alpha) => {
                                if iz < self.clear[i] * (1.0 - 1e-5) {
                                    continue;
                                }
                                self.clear[i] = f32::INFINITY;
                                let d = &mut self.rgb[i];
                                *d = [0, 1, 2].map(|k| d[k] + (c[k] - d[k]) * alpha);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// The drape's colour at (`u`, `v`), 0 to 1 across and down it, bilinearly, over white where it
/// is clear.
fn sample(pm: &Pixmap, u: f32, v: f32) -> [f32; 3] {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let (fx, fy) = ((u * w - 0.5).clamp(0.0, w - 1.0), (v * h - 0.5).clamp(0.0, h - 1.0));
    let (x0, y0) = (fx.floor() as u32, fy.floor() as u32);
    let (x1, y1) = ((x0 + 1).min(pm.width() - 1), (y0 + 1).min(pm.height() - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let px = |x: u32, y: u32| {
        let p = pm.pixel(x, y).unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
        // Premultiplied over white.
        let a = 1.0 - f32::from(p.alpha()) / 255.0;
        [p.red(), p.green(), p.blue()].map(|c| f32::from(c) / 255.0 + a)
    };
    let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    [0, 1, 2].map(|k| {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bottom = c[k] + (d[k] - c[k]) * tx;
        top + (bottom - top) * ty
    })
}

/// A mesh's triangles on the picture, cut where they cross the near plane.
fn draw_mesh(t: &mut Target, eye: &Eye, m: &Mesh, how: How, draped: bool) -> Result<(), String> {
    let views: Vec<[f64; 3]> = m.pos.iter().map(|p| eye.view(*p)).collect();
    let attrs = |i: usize| -> [f32; 5] {
        let c = m.col[i];
        let uv = if draped { m.uv[i] } else { [0.0, 0.0] };
        [c[0], c[1], c[2], uv[0], uv[1]]
    };
    for (n, tri) in m.tris.iter().enumerate() {
        if n % TRIS_PER_LOOK == TRIS_PER_LOOK - 1 {
            go_on()?;
        }
        let idx = tri.map(|i| i as usize);
        let vs = idx.map(|i| (views[i], attrs(i)));
        if vs.iter().all(|(v, _)| v[2] < eye.near) {
            continue;
        }
        let poly = clip_near(&vs, eye.near);
        let sv: Vec<Sv> = poly
            .iter()
            .map(|(v, a)| {
                let (x, y, iz) = eye.project(*v);
                Sv { x, y, iz, a: a.map(|c| c * iz) }
            })
            .collect();
        for k in 1..sv.len().saturating_sub(1) {
            t.tri([sv[0], sv[k], sv[k + 1]], how)?;
        }
    }
    Ok(())
}

/// A triangle's part in front of the near plane (depth `near`): none, a triangle or a quad.
fn clip_near(vs: &[([f64; 3], [f32; 5]); 3], near: f64) -> Vec<([f64; 3], [f32; 5])> {
    if vs.iter().all(|(v, _)| v[2] >= near) {
        return vs.to_vec();
    }
    let mut out = Vec::with_capacity(4);
    for i in 0..3 {
        let (a, b) = (vs[i], vs[(i + 1) % 3]);
        let (ina, inb) = (a.0[2] >= near, b.0[2] >= near);
        if ina {
            out.push(a);
        }
        if ina != inb {
            let t = (near - a.0[2]) / (b.0[2] - a.0[2]);
            let p = [0, 1, 2].map(|k| a.0[k] + (b.0[k] - a.0[k]) * t);
            let at = [0, 1, 2, 3, 4].map(|k| a.1[k] + (b.1[k] - a.1[k]) * t as f32);
            out.push((p, at));
        }
    }
    out
}

/// Draws a built scene from `cam`, `w` × `h` pixels at `ppp` pixels a point (for its labels and
/// credits), the ground `opacity` opaque (0 to 1), each pixel worked out from `ss` × `ss`
/// samples (1: none, 2 or 3 for a smooth image to keep). Cancel (an export's) is looked at as it
/// draws. Errors: a picture of no pixels, or Cancel.
pub fn draw(b: &Built, cam: &SceneCamera, opacity: f64, w: u32, h: u32, ppp: f32, ss: u32) -> Result<Pixmap, String> {
    draw_with(b, cam, Look { opacity, ruler: false, ruler_step: None }, w, h, ppp, ss)
}

/// How a scene is looked at, besides from where: [`draw_with`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// How opaque the ground is, 0 to 1.
    pub opacity: f64,
    /// A ruler of heights up the block's corner nearest the eye ([`Scene::ruler`]).
    pub ruler: bool,
    /// True metres between its marks ([`Scene::ruler_step`]). None: [`ruler_step`]'s.
    pub ruler_step: Option<f64>,
}

impl Look {
    /// How `scene` is looked at.
    pub fn of(scene: &Scene) -> Look {
        Look { opacity: scene.ground_opacity, ruler: scene.ruler, ruler_step: scene.ruler_step }
    }
}

/// [`draw`], looked at as `look` says.
pub fn draw_with(b: &Built, cam: &SceneCamera, look: Look, w: u32, h: u32, ppp: f32, ss: u32) -> Result<Pixmap, String> {
    let opacity = look.opacity;
    let ss = ss.clamp(1, 4) as usize;
    let (bw, bh) = (w as usize * ss, h as usize * ss);
    let no_pixels = || "the picture has no pixels".to_string();
    if bw == 0 || bh == 0 {
        return Err(no_pixels());
    }
    let eye = Eye::of(b, cam, bw as f64, bh as f64);
    let mut t = Target { w: bw, h: bh, rgb: vec![[0.0; 3]; bw * bh], depth: vec![0.0; bw * bh], clear: Vec::new() };
    // The sky: pale at the horizon, bluer above.
    for y in 0..bh {
        let s = y as f32 / bh as f32;
        let c = [0.60 + 0.30 * s, 0.74 + 0.20 * s, 0.90 + 0.07 * s];
        t.rgb[y * bw..(y + 1) * bw].fill(c);
    }
    let opacity = opacity.clamp(0.0, 1.0) as f32;
    draw_mesh(&mut t, &eye, &b.solids, How::Solid, false)?;
    let white;
    let drape = match &b.drape {
        Some(d) => d,
        None => {
            white = Pixmap::new(1, 1).ok_or_else(no_pixels)?;
            &white
        }
    };
    if opacity >= 1.0 {
        draw_mesh(&mut t, &eye, &b.ground, How::Draped(drape, b.under, None), true)?;
        draw_mesh(&mut t, &eye, &b.sides, How::Solid, false)?;
    } else if opacity > 0.0 {
        // Clear ground over what is under it: only its nearest surface on each pixel, as glass,
        // not every fold of the ground behind it.
        t.clear = vec![0.0; bw * bh];
        draw_mesh(&mut t, &eye, &b.ground, How::Mark, true)?;
        draw_mesh(&mut t, &eye, &b.ground, How::Draped(drape, b.under, Some(opacity)), true)?;
        // Then the block's nearest side or base, clearer still (what is under the ground is
        // what is looked at), not hiding the ground from an eye below the base.
        t.clear.fill(0.0);
        draw_mesh(&mut t, &eye, &b.sides, How::Mark, false)?;
        draw_mesh(&mut t, &eye, &b.sides, How::Clear(opacity * 0.6), false)?;
    }
    // Down to the picture's pixels.
    let mut pm = Pixmap::new(w, h).ok_or_else(no_pixels)?;
    let n = (ss * ss) as f32;
    let data = pm.data_mut();
    for y in 0..h as usize {
        if y % ROWS_PER_LOOK == ROWS_PER_LOOK - 1 {
            go_on()?;
        }
        for x in 0..w as usize {
            let mut c = [0.0f32; 3];
            for sy in 0..ss {
                for sx in 0..ss {
                    let p = t.rgb[(y * ss + sy) * bw + x * ss + sx];
                    c = [c[0] + p[0], c[1] + p[1], c[2] + p[2]];
                }
            }
            let i = (y * w as usize + x) * 4;
            for k in 0..3 {
                data[i + k] = ((c[k] / n).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            data[i + 3] = 255;
        }
    }
    // The labels that aren't behind something solid.
    if !b.labels.is_empty() {
        let mut canvas = Canvas::from_pixmap(pm);
        let small = Eye::of(b, cam, f64::from(w), f64::from(h));
        let px = 12.0 * ppp;
        for l in &b.labels {
            let v = small.view(l.at.map(|c| c as f32));
            if v[2] < small.near {
                continue;
            }
            let (x, y, iz) = small.project(v);
            if !(x >= 0.0 && y >= 0.0 && x < w as f32 && y < h as f32) {
                continue;
            }
            let (sx, sy) = ((x as usize * ss).min(bw - 1), (y as usize * ss).min(bh - 1));
            // Behind what is nearest there by more than a little: hidden.
            if t.depth[sy * bw + sx] > iz * 1.02 {
                continue;
            }
            crate::text::text_at(
                &mut canvas,
                &l.text,
                x,
                y - px * 0.6,
                px,
                Rgba::rgb(0x20, 0x20, 0x20),
                true,
                crate::text::Align::Center,
                true,
            );
        }
        pm = canvas.into_pixmap();
    }
    if look.ruler {
        let mut canvas = Canvas::from_pixmap(pm);
        ruler(&mut canvas, b, &Eye::of(b, cam, f64::from(w), f64::from(h)), ppp, look.ruler_step);
        pm = canvas.into_pixmap();
    }
    if !b.credits.is_empty() {
        let mut canvas = Canvas::from_pixmap(pm);
        crate::draw_attribution(&mut canvas, &b.credits, ppp, (0.0, 0.0));
        pm = canvas.into_pixmap();
    }
    Ok(pm)
}

/// The fewest pixels between a ruler's marks (at a point a pixel).
const RULER_GAP: f64 = 34.0;

/// The step between a ruler's marks over `span` metres `px` pixels long: 1, 2 or 5 times a power
/// of ten, at least [`RULER_GAP`] pixels (`ppp` a point) and at most eight to the ruler.
fn ruler_step(span: f64, px: f64, ppp: f64) -> f64 {
    let least = (span / 8.0).max(span * RULER_GAP * ppp / px.max(1.0)).max(1e-3);
    let ten = 10f64.powf(least.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * ten).find(|s| *s >= least * (1.0 - 1e-9)).unwrap_or(10.0 * ten)
}

/// How much of the segment `a`–`b` lies on a picture `w` × `h` (pixels).
fn on_picture(a: (f32, f32), b: (f32, f32), w: f64, h: f64) -> f64 {
    let (x0, y0, dx, dy) = (f64::from(a.0), f64::from(a.1), f64::from(b.0 - a.0), f64::from(b.1 - a.1));
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, x0), (dx, w - x0), (-dy, y0), (dy, h - y0)] {
        if p == 0.0 {
            if q < 0.0 {
                return 0.0;
            }
        } else {
            let r = q / p;
            if p < 0.0 { t0 = t0.max(r) } else { t1 = t1.min(r) }
        }
    }
    if t1 <= t0 { 0.0 } else { (t1 - t0) * dx.hypot(dy) }
}

/// The corner of the box a ruler stands up, of the three nearest the eye along the ground (the
/// farthest is behind the block): the one most of whose edge is on the picture, the leftmost of
/// those as much on it.
fn ruler_corner(b: &Built, eye: &Eye) -> (f64, f64) {
    let (hw, hh) = b.half;
    let mut corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let far = |c: &(f64, f64)| (c.0 - eye.at[0]).hypot(c.1 - eye.at[1]);
    corners.sort_by(|p, q| far(p).total_cmp(&far(q)));
    let (w, h) = (eye.centre.0 * 2.0, eye.centre.1 * 2.0);
    let seen = |&(x, y): &(f64, f64)| -> (f64, f64) {
        let at = |z: f64| {
            let v = eye.view([x as f32, y as f32, z as f32]);
            (v[2] >= eye.near).then(|| eye.project(v))
        };
        match (at(b.z_range.0), at(b.ground_at(x, y))) {
            (Some(p0), Some(p1)) => (on_picture((p0.0, p0.1), (p1.0, p1.1), w, h), f64::from(p0.0.min(p1.0))),
            _ => (0.0, 0.0),
        }
    };
    corners[..3]
        .iter()
        .map(|c| (*c, seen(c)))
        .max_by(|(_, (la, xa)), (_, (lb, xb))| la.round().total_cmp(&lb.round()).then(xb.total_cmp(xa)))
        .map_or((-hw, -hh), |(c, _)| c)
}

/// The most marks a ruler of an asked step draws: a step too fine for it is marked at a multiple.
const RULER_MARKS: f64 = 400.0;

/// Which of a ruler's marks `asked` metres apart over `span` metres `px` pixels long are drawn
/// and which numbered: every `draw`th multiple of `asked` (1 unless there would be more than
/// [`RULER_MARKS`]), numbered every `number`th of those, the fewest that keeps numbers
/// [`RULER_GAP`] pixels apart (`ppp` a point): a round height (1, 2 or 5 times a power of ten)
/// when one is a multiple of the step, else 1, 2 or 5 times a power of ten of the steps.
fn ruler_every(asked: f64, span: f64, px: f64, ppp: f64) -> (u64, u64) {
    let draw = (span / asked / RULER_MARKS).ceil().max(1.0);
    let drawn = asked * draw;
    let need = (span * RULER_GAP * ppp / px.max(1.0) / drawn).max(1.0) * (1.0 - 1e-9);
    let nice = |least: f64| {
        let ten = 10f64.powf(least.log10().floor());
        (0..12).flat_map(move |i| [1.0, 2.0, 5.0].map(|m| m * ten * 10f64.powi(i))).filter(move |r| *r >= least)
    };
    let whole = |k: f64| (k - k.round()).abs() < 1e-6 * k.max(1.0);
    let number =
        nice(need * drawn).take(9).map(|r| r / drawn).find(|k| whole(*k)).or_else(|| nice(need).next()).map_or(1.0, f64::round).max(1.0);
    (draw as u64, number as u64)
}

/// A ruler of heights up the block's corner nearest the eye, from its base to the ground there:
/// marked every `asked` true metres, or every [`ruler_step`] when none is asked, numbered in
/// metres above sea level (not exaggerated), on the side of the corner away from the block. An
/// asked step finer than numbers fit has only every few marks numbered ([`ruler_every`]), the
/// rest shorter. Left out when the corner is behind the eye or too short on the picture to mark.
fn ruler(canvas: &mut Canvas, b: &Built, eye: &Eye, ppp: f32, asked: Option<f64>) {
    let (x, y) = ruler_corner(b, eye);
    let (base, top) = (b.z_range.0, b.ground_at(x, y));
    let at = |z: f64| {
        let v = eye.view([x as f32, y as f32, z as f32]);
        (v[2] >= eye.near).then(|| eye.project(v))
    };
    let (Some(p0), Some(p1)) = (at(base), at(top)) else { return };
    let len = f64::from(p1.0 - p0.0).hypot(f64::from(p1.1 - p0.1));
    let ppp64 = f64::from(ppp);
    if on_picture((p0.0, p0.1), (p1.0, p1.1), eye.centre.0 * 2.0, eye.centre.1 * 2.0) < RULER_GAP * ppp64 || top <= base {
        return;
    }
    // Which way is out: away from the middle of the block on the picture.
    let mid = eye.view([0.0, 0.0, b.mid as f32]);
    let mut out = if mid[2] >= eye.near && eye.project(mid).0 > p0.0 { -1.0 } else { 1.0 };
    // Unless its numbers would be off the picture there.
    let room = 70.0 * ppp;
    let left = p0.0.min(p1.0).max(0.0);
    if (out < 0.0 && left < room) || (out > 0.0 && p0.0.max(p1.0) > eye.centre.0 as f32 * 2.0 - room) {
        out = -out;
    }
    // Stretched one way down to the lowest ground, maybe another below it.
    let (hi, lo) = (b.height_of(top), b.height_of(base));
    // Marked as often as its most crowded part allows: the scene's metres a true metre, at
    // their fewest along it.
    let per_m = if lo < b.datum && hi > b.datum {
        b.ex.min(b.dex)
    } else if lo >= b.datum {
        b.ex
    } else {
        b.dex
    };
    let long = len * per_m * (hi - lo) / (top - base).max(1e-9);
    // Marks every `step`, numbered every `number`th, numbers to `precision`.
    let (step, number, precision) = match asked.filter(|a| a.is_finite() && *a > 0.0) {
        Some(a) => {
            let (draw, number) = ruler_every(a, hi - lo, long, ppp64);
            (a * draw as f64, number, a)
        }
        None => {
            let st = ruler_step(hi - lo, long, ppp64);
            (st, 1, st)
        }
    };
    let tick = 7.0 * ppp;
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(p0.0, p0.1);
    pb.line_to(p1.0, p1.1);
    // Its ends, numbered to the metre, and the round heights between not too near them.
    let mut marks = vec![(p0, lo, 1.0, true), (p1, hi, 1.0, true)];
    // A label's height and a little apart, on the picture.
    let near_end = 18.0 * ppp;
    let apart = |p: (f32, f32, f32), q: (f32, f32, f32)| (p.0 - q.0).hypot(p.1 - q.1) > near_end;
    // Counted from 0 m, so the numbered ones are the round ones.
    let mut i = (lo / step - 1e-9).ceil() as i64;
    while (i as f64) * step <= hi + 1e-9 {
        let v = i as f64 * step;
        let numbered = i.rem_euclid(number as i64) == 0;
        if let Some(p) = at(b.z_of(v))
            && (!numbered || (apart(p, p0) && apart(p, p1)))
        {
            marks.push((p, v, precision, numbered));
        }
        i += 1;
    }
    for (p, _, _, numbered) in &marks {
        pb.move_to(p.0, p.1);
        pb.line_to(p.0 + out * if *numbered { tick } else { tick * 0.55 }, p.1);
    }
    let Some(path) = pb.finish() else { return };
    let paint = |c: tiny_skia::Color| {
        let mut p = tiny_skia::Paint::default();
        p.set_color(c);
        p.anti_alias = true;
        p
    };
    let stroke = |w: f32| tiny_skia::Stroke { width: w * ppp, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
    let t = tiny_skia::Transform::identity();
    canvas.stroke_path(&path, &paint(tiny_skia::Color::from_rgba8(255, 255, 255, 220)), &stroke(4.0), t, None);
    canvas.stroke_path(&path, &paint(tiny_skia::Color::from_rgba8(0x20, 0x20, 0x20, 255)), &stroke(1.6), t, None);
    let px = 11.0 * ppp;
    let align = if out < 0.0 { crate::text::Align::Right } else { crate::text::Align::Left };
    for ((x, y, _), v, precision, _) in marks.into_iter().filter(|m| m.3) {
        let text = format!("{} m", fmt_metres(v, precision));
        crate::text::text_at(canvas, &text, x + out * (tick + 4.0 * ppp), y, px, Rgba::rgb(0x20, 0x20, 0x20), false, align, true);
    }
}

/// `v` metres written to the precision of a ruler's `step`, with thousands apart.
fn fmt_metres(v: f64, step: f64) -> String {
    let decimals = if step >= 1.0 { 0 } else { (-step.log10().floor()) as usize };
    let s = format!("{:.*}", decimals, if v.abs() < step * 1e-6 { 0.0 } else { v });
    let (int, frac) = s.split_once('.').map_or((s.as_str(), None), |(i, f)| (i, Some(f)));
    let (sign, digits) = int.strip_prefix('-').map_or(("", int), |d| ("-", d));
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// What is under a point of a scene's picture: [`pick`].
#[derive(Clone, Debug, PartialEq)]
pub struct Picked {
    /// Where it is: longitude and latitude (degrees).
    pub lonlat: Coord,
    /// Its height above sea level (metres, not exaggerated).
    pub elevation: f64,
    /// How far below the ground it is there (metres), when it is under the ground.
    pub below_ground: Option<f64>,
    /// The feature stood up there, when it is one.
    pub feature: Option<PickedFeature>,
}

/// A feature of a scene under a point of its picture.
#[derive(Clone, Debug, PartialEq)]
pub struct PickedFeature {
    /// Its layer.
    pub layer: String,
    /// Its label's text, when the scene labels its layer.
    pub label: Option<String>,
    /// For a well, how far down it the point is from its head (metres).
    pub below_head: Option<f64>,
}

/// The nearest of a mesh's triangles a ray from `o` along `d` meets: how far along, and which.
fn hit(m: &Mesh, o: [f64; 3], d: [f64; 3], near: f64) -> Result<Option<(f64, usize)>, String> {
    let mut best: Option<(f64, usize)> = None;
    for (n, tri) in m.tris.iter().enumerate() {
        if n % TRIS_PER_LOOK == 0 {
            go_on()?;
        }
        let [a, b, c] = tri.map(|i| m.pos[i as usize].map(f64::from));
        let (e1, e2) = ([0, 1, 2].map(|k| b[k] - a[k]), [0, 1, 2].map(|k| c[k] - a[k]));
        let p = cross(d, e2);
        let det = dot(e1, p);
        if det.abs() < 1e-12 {
            continue;
        }
        let inv = 1.0 / det;
        let s = [0, 1, 2].map(|k| o[k] - a[k]);
        let u = dot(s, p) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = cross(s, e1);
        let v = dot(d, q) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let t = dot(e2, q) * inv;
        if t >= near && best.is_none_or(|(bt, _)| t < bt) {
            best = Some((t, n));
        }
    }
    Ok(best)
}

/// What is under the point (`x`, `y`) of a picture `w` × `h` (any unit, the same) of `b` from
/// `cam`, the ground `opacity` opaque: the nearest of the ground, the block's sides and what is
/// stood up, or through ground less than opaque, what is stood up behind it. None: the sky (and
/// the ground and the block, clear). It looks at Cancel as it goes (a scene of many features has
/// many triangles): Err when pressed.
pub fn pick(b: &Built, cam: &SceneCamera, opacity: f64, w: f64, h: f64, x: f64, y: f64) -> Result<Option<Picked>, String> {
    if !(w > 0.0 && h > 0.0 && x.is_finite() && y.is_finite()) {
        return Ok(None);
    }
    let eye = Eye::of(b, cam, w, h);
    let (sx, sy) = ((x - eye.centre.0) / eye.focal, (eye.centre.1 - y) / eye.focal);
    let d = norm([0, 1, 2].map(|k| eye.fwd[k] + eye.right[k] * sx + eye.up[k] * sy));
    // Along the ray, a point's depth grows by d · fwd a unit.
    let near = eye.near / dot(d, eye.fwd).max(1e-9);
    let solid = hit(&b.solids, eye.at, d, near)?;
    // The ground's own surface (true) or the block's sides and base; none when the ground is
    // clear, as none of them is drawn then.
    let ground = if opacity > 0.0 {
        [hit(&b.ground, eye.at, d, near)?.map(|(t, _)| (t, true)), hit(&b.sides, eye.at, d, near)?.map(|(t, _)| (t, false))]
            .into_iter()
            .flatten()
            .min_by(|a, c| a.0.total_cmp(&c.0))
    } else {
        None
    };
    let (t, owner, on_ground) = match (solid, ground) {
        (Some((ts, n)), Some((tg, _))) if ts <= tg || opacity < 1.0 => (ts, Some(n), false),
        (Some((ts, n)), None) => (ts, Some(n), false),
        (_, Some((tg, surface))) => (tg, None, surface),
        (None, None) => return Ok(None),
    };
    let p = [0, 1, 2].map(|k| eye.at[k] + d[k] * t);
    let (cx, cy, k) = b.origin;
    let lonlat = merc_to_lonlat(Coord::new(p[0] / k + cx, p[1] / k + cy));
    let (hw, hh) = b.half;
    let inside = p[0].abs() <= hw * (1.0 + 1e-9) && p[1].abs() <= hh * (1.0 + 1e-9);
    let owner = owner.and_then(|n| b.owners.get(b.owners.partition_point(|o| o.end <= n)));
    // A well's depth under the ground is the ground's over its middle, not beside it.
    let (gx, gy) = owner.and_then(|o| o.head).map_or((p[0], p[1]), |(_, at)| at);
    // Heights in true metres, however the scene stretches them.
    let elevation = b.height_of(p[2]);
    let below = b.height_of(b.ground_at(gx, gy)) - elevation;
    let feature = owner.map(|o| PickedFeature {
        layer: o.layer.clone(),
        label: o.label.clone(),
        below_head: o.head.map(|(h, _)| (h - elevation).max(0.0)),
    });
    // On the ground itself, at it (its mesh and its heights between their corners differ a
    // little); elsewhere under it by more than a metre.
    Ok(Some(Picked { lonlat, elevation, below_ground: (!on_ground && inside && below > 1.0).then_some(below), feature }))
}

/// Moves the point `cam` turns about as a drag of (`dx`, `dy`) pixels over a picture `h` pixels
/// tall moves the ground under it, along the ground (not up or down).
pub fn pan(b: &Built, cam: &mut SceneCamera, dx: f64, dy: f64, h: f64) {
    let focal = (h / 2.0) / (FOV.to_radians() / 2.0).tan();
    let per_px = cam.zoom * b.diag / focal.max(1.0);
    let th = cam.heading.to_radians();
    let (right, ahead) = ([th.cos(), -th.sin()], [th.sin(), th.cos()]);
    // Looking down steeply, a drag up the picture is a drag north; looking level, ahead.
    let along = 1.0 / cam.tilt.to_radians().cos().abs().max(0.35);
    cam.target[0] -= (right[0] * dx - ahead[0] * dy * along) * per_px;
    cam.target[1] -= (right[1] * dx - ahead[1] * dy * along) * per_px;
    let (hw, hh) = b.half;
    cam.target[0] = cam.target[0].clamp(-hw * 1.5, hw * 1.5);
    cam.target[1] = cam.target[1].clamp(-hh * 1.5, hh * 1.5);
}

/// Encodes a picture as PNG.
pub fn png(pm: &Pixmap) -> Result<Vec<u8>, String> {
    pm.encode_png().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform};
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
    use rustgis_map::scene::SceneLayer;

    /// A map with a DEM over a 4 km box near 14° N: a hill in the middle, 100 m at the edges,
    /// 400 m on top; and a layer of two wells.
    fn hill() -> (Map, Scene) {
        let (side, cell) = (80u32, 50.0);
        let (cx, cy) = (11_200_000.0, 1_560_000.0);
        let half = f64::from(side) * cell / 2.0;
        let gt = GeoTransform { origin_x: cx - half, origin_y: cy + half, pixel_width: cell, pixel_height: -cell };
        let mut z = Vec::new();
        for j in 0..side {
            for i in 0..side {
                let (dx, dy) = (f64::from(i) - 40.0, f64::from(j) - 40.0);
                z.push((100.0 + 300.0 * (-(dx * dx + dy * dy) / 300.0).exp()) as f32);
            }
        }
        let dem = Raster::new("dem", side, side, vec![z], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let mut m = Map::new("Map");
        m.add_raster("dem", rustgis_map::RasterLayer::from_path(dem, "dem.tif"));
        let mut wells = Dataset::new(
            "wells",
            GeometryType::Point,
            vec![Field::new("ชื่อ", FieldType::Text), Field::new("ลึก", FieldType::Double), Field::new("น้ำ", FieldType::Double)],
        );
        let ll = |x: f64, y: f64| rustgis_geom::merc_to_lonlat(Coord::new(x, y));
        wells.push(Some(Geometry::Point(ll(cx - 1000.0, cy))), vec![Value::Text("บ่อ ก".into()), Value::Float(120.0), Value::Float(15.0)]);
        wells.push(Some(Geometry::Point(ll(cx + 1000.0, cy))), vec![Value::Text("บ่อ ข".into()), Value::Null, Value::Null]);
        m.add_dataset(wells);
        let mut s = Scene::new([cx - 1500.0, cy - 1500.0, cx + 1500.0, cy + 1500.0]);
        s.dem = Some("dem".into());
        let mut w = SceneLayer::new("wells", SceneKind::Well);
        w.depth = Some("ลึก".into());
        w.water = Some("น้ำ".into());
        w.label = Some("ชื่อ".into());
        w.color = Some(Rgba::rgb(0xE0, 0x30, 0x30));
        s.layers.push(w);
        (m, s)
    }

    fn small() -> Detail {
        Detail { drape_px: 256, cells: 48 }
    }

    #[test]
    fn a_point_of_the_picture_says_where_and_how_deep_it_is() {
        let (m, s) = hill();
        let b = build(m, &s, small()).unwrap();
        let k = b.origin.2;
        let well = -1000.0 * k;
        let look = |cam: &SceneCamera, opacity: f64, x: f64, y: f64| pick(&b, cam, opacity, 400.0, 300.0, x, y).unwrap();
        // Straight down onto the well: the well, at its head, where it is.
        let mut cam = SceneCamera { heading: 0.0, tilt: 0.0, zoom: 0.3, target: [well, 0.0, 0.0] };
        let p = look(&cam, 1.0, 200.0, 150.0).unwrap();
        let f = p.feature.clone().unwrap();
        assert_eq!((f.layer.as_str(), f.label.as_deref()), ("wells", Some("บ่อ ก")));
        assert!(f.below_head.unwrap() < 1.0, "{p:?}");
        let ll = rustgis_geom::merc_to_lonlat(Coord::new(11_200_000.0 - 1000.0, 1_560_000.0));
        assert!((p.lonlat.x - ll.x).abs() < 1e-4 && (p.lonlat.y - ll.y).abs() < 1e-4, "{p:?}");
        // Beside it: the ground, at its height there, not under itself.
        cam.target = [well + 300.0, 0.0, 0.0];
        let p = look(&cam, 1.0, 200.0, 150.0).unwrap();
        assert!(p.feature.is_none() && p.below_ground.is_none(), "{p:?}");
        assert!((p.elevation - b.ground_at(well + 300.0, 0.0)).abs() < 1.0, "{p:?}");
        // From under the ground, looking down: the base, as far below the ground as it lies.
        let (ground, base) = (b.ground_at(well + 300.0, 0.0), b.z_range.0);
        cam.zoom = 0.002;
        cam.target[2] = base + 10.0 - b.mid;
        assert!(base + 10.0 + cam.zoom * b.diag < ground, "the eye is under the ground");
        let p = look(&cam, 1.0, 200.0, 150.0).unwrap();
        assert!((p.elevation - base).abs() < 1.0, "{p:?}");
        assert!((p.below_ground.unwrap() - (ground - base)).abs() < 1.0, "{p:?}");
        // Down the well from beside it, through clear ground: the well, partway down.
        let side = SceneCamera { heading: 90.0, tilt: 90.0, zoom: 0.15, target: [well, 0.0, b.ground_at(well, 0.0) - 60.0 - b.mid] };
        let solid = look(&side, 1.0, 200.0, 150.0).unwrap();
        assert!(solid.feature.is_none(), "opaque ground hides it: {solid:?}");
        let clear = look(&side, 0.3, 200.0, 150.0).unwrap();
        let below = clear.feature.and_then(|f| f.below_head).unwrap();
        assert!((below - 60.0).abs() < 5.0, "{below}");
        assert!((clear.below_ground.unwrap() - 60.0).abs() < 5.0);
        // The sky.
        let level = SceneCamera { heading: 0.0, tilt: 90.0, zoom: 1.0, target: [0.0, 0.0, 0.0] };
        assert!(look(&level, 1.0, 200.0, 2.0).is_none());
        // Ground left clear is not drawn, and so not picked: beside the well the sky; the well
        // still itself.
        let beside = SceneCamera { heading: 0.0, tilt: 0.0, zoom: 0.3, target: [well + 300.0, 0.0, 0.0] };
        assert!(look(&beside, 0.3, 200.0, 150.0).is_some());
        assert!(look(&beside, 0.0, 200.0, 150.0).is_none());
        let onto = SceneCamera { target: [well, 0.0, 0.0], ..beside };
        assert!(look(&onto, 0.0, 200.0, 150.0).and_then(|p| p.feature).is_some());
        // Cancel pressed: it stops (a scene of many features has many triangles to look through).
        struct Cancelled;
        impl rustgis_data::progress::Probe for Cancelled {
            fn cancelled(&self) -> bool {
                true
            }
            fn begin(&self, _: u64) {}
            fn advance(&self, _: u64) {}
        }
        let installed = rustgis_data::progress::install(Some(std::sync::Arc::new(Cancelled)));
        let r = pick(&b, &cam, 1.0, 400.0, 300.0, 200.0, 150.0);
        drop(installed);
        assert!(r.unwrap_err().contains("ancel"));
    }

    #[test]
    fn the_relief_is_stretched_and_what_is_under_it_is_true_metres() {
        let (m, mut s) = hill();
        s.exaggeration = 4.0;
        s.depth = Some(200.0);
        let b = build(m, &s, small()).unwrap();
        let k = b.origin.2;
        let well = -1000.0 * k;
        // The ground stretched: four times its heights; the lowest ground the datum.
        let (head, foot) = (b.ground_at(well, 0.0), b.ground_at(b.half.0, b.half.1));
        let e = head / 4.0;
        assert!((e - 175.0).abs() < 10.0 && (foot / 4.0 - 100.0).abs() < 1.0, "{head} {foot}");
        assert!((b.datum - foot / 4.0).abs() < 1.0);
        // Over the datum stretched, under it true metres; and back.
        let d = b.datum;
        assert!((b.z_of(d + 10.0) - (d + 10.0) * 4.0).abs() < 1e-6);
        assert!((b.z_of(d - 50.0) - (d * 4.0 - 50.0)).abs() < 1e-6);
        for h in [e + 10.0, e, d, d - 50.0, -300.0] {
            assert!((b.height_of(b.z_of(h)) - h).abs() < 1e-6);
        }
        // The 120 m well goes below the datum: the block 200 true metres below its foot.
        let bottom = b.z_of(e - 120.0);
        assert!(e - 120.0 < d && (b.z_range.0 - (bottom - 200.0)).abs() < 1.0 && b.depth == 200.0, "{:?}", b.z_range);
        assert!((b.height_of(b.z_range.0) - (e - 120.0 - 200.0)).abs() < 1.0);
        // Straight down onto the well: at its head.
        let look = |cam: &SceneCamera, opacity: f64| pick(&b, cam, opacity, 400.0, 300.0, 200.0, 150.0).unwrap();
        let down = SceneCamera { heading: 0.0, tilt: 0.0, zoom: 0.3, target: [well, 0.0, 0.0] };
        let p = look(&down, 1.0).unwrap();
        assert!(
            p.feature.as_ref().and_then(|f| f.below_head).unwrap() < 1.0 && (p.elevation - e).abs() < 3.0,
            "its casing a little above: {p:?}"
        );
        // Down the well from beside it through clear ground, 100 m down (below the datum): as
        // deep as it is, however stretched the relief it went through.
        let at = b.z_of(e - 100.0);
        let side = SceneCamera { heading: 90.0, tilt: 90.0, zoom: 0.15, target: [well, 0.0, at - b.mid] };
        let clear = look(&side, 0.3).unwrap();
        let below = clear.feature.as_ref().and_then(|f| f.below_head).unwrap();
        assert!((below - 100.0).abs() < 5.0, "{below}");
        assert!((clear.below_ground.unwrap() - 100.0).abs() < 5.0 && (clear.elevation - (e - 100.0)).abs() < 5.0, "{clear:?}");
        // From under the ground: the base at its true height, as far under the ground there.
        let beside = well + 300.0;
        let (ground, base) = (b.ground_at(beside, 0.0), b.z_range.0);
        let under = SceneCamera { heading: 0.0, tilt: 0.0, zoom: 0.002, target: [beside, 0.0, base + 10.0 - b.mid] };
        let p = look(&under, 1.0).unwrap();
        assert!((p.elevation - b.height_of(base)).abs() < 1.0, "{p:?}");
        assert!((p.below_ground.unwrap() - (ground / 4.0 - b.height_of(base))).abs() < 1.0, "{p:?}");
    }

    #[test]
    fn what_is_under_the_lowest_ground_is_stretched_as_asked_and_read_in_true_metres() {
        let (m, mut s) = hill();
        s.exaggeration = 4.0;
        s.depth_exaggeration = 3.0;
        s.depth = Some(200.0);
        let b = build(m, &s, small()).unwrap();
        let k = b.origin.2;
        let well = -1000.0 * k;
        let (d, e) = (b.datum, b.ground_at(well, 0.0) / 4.0);
        // Over the lowest ground four times, under it three times; and back.
        assert!((b.z_of(d + 10.0) - (d + 10.0) * 4.0).abs() < 1e-6);
        assert!((b.z_of(d - 50.0) - (d * 4.0 - 150.0)).abs() < 1e-6);
        for h in [e, d, d - 50.0, -300.0] {
            assert!((b.height_of(b.z_of(h)) - h).abs() < 1e-6);
        }
        // The block 200 true metres below the 120 m well's foot, drawn 600 down; still 200 m.
        let bottom = b.z_of(e - 120.0);
        assert!((b.z_range.0 - (bottom - 600.0)).abs() < 1.0 && b.depth == 200.0, "{:?}", b.z_range);
        assert!((b.height_of(b.z_range.0) - (e - 120.0 - 200.0)).abs() < 1.0);
        // 100 m down the well through clear ground: 100 m, however it is stretched.
        let at = b.z_of(e - 100.0);
        let side = SceneCamera { heading: 90.0, tilt: 90.0, zoom: 0.15, target: [well, 0.0, at - b.mid] };
        let p = pick(&b, &side, 0.3, 400.0, 300.0, 200.0, 150.0).unwrap().unwrap();
        let below = p.feature.as_ref().and_then(|f| f.below_head).unwrap();
        assert!((below - 100.0).abs() < 5.0 && (p.below_ground.unwrap() - 100.0).abs() < 5.0, "{p:?}");
        // Against true metres below the lowest ground: deeper by twice the well's part under it
        // and twice the block.
        s.depth_exaggeration = 1.0;
        let flat = build(hill().0, &s, small()).unwrap();
        let more = 2.0 * (d - (e - 120.0)) + 400.0;
        assert!((flat.z_range.0 - b.z_range.0 - more).abs() < 1.0, "{:?} {:?}", b.z_range, flat.z_range);
    }

    #[test]
    fn a_point_set_under_the_ground_is_as_far_under_it_in_true_metres() {
        let (m, mut s) = hill();
        s.exaggeration = 4.0;
        let mut p = SceneLayer::new("wells", SceneKind::Point);
        p.offset = -100.0;
        p.size = Some(4.0);
        s.layers = vec![p];
        let b = build(m, &s, small()).unwrap();
        let k = b.origin.2;
        // Its middle, from the ball's corners: 100 true metres under the ground, below the
        // lowest ground, so not stretched.
        let n = b.solids.pos.len() / 2;
        let centre = |v: &[[f32; 3]]| v.iter().map(|p| f64::from(p[2])).sum::<f64>() / v.len() as f64;
        for (v, x) in [(&b.solids.pos[..n], -1000.0 * k), (&b.solids.pos[n..], 1000.0 * k)] {
            let h = b.height_of(b.ground_at(x, 0.0)) - 100.0;
            assert!(h < b.datum && (centre(v) - (b.z_of(h) + 2.0)).abs() < 0.01, "{} {h}", centre(v));
        }
        // Picked from above through clear ground: as deep as it is set, less its half width.
        let cam = SceneCamera { heading: 0.0, tilt: 0.0, zoom: 0.01, target: [1000.0 * k, 0.0, 0.0] };
        let p = pick(&b, &cam, 0.0, 400.0, 300.0, 200.0, 150.0).unwrap().unwrap();
        assert!((p.below_ground.unwrap() - 96.0).abs() < 1.0, "{p:?}");
    }

    #[test]
    fn a_long_narrow_box_is_draped_with_the_box_only() {
        // 400 to 1: drawn 16 pixels across its width, cut back to the one row that is the box.
        let (cx, cy) = (11_200_000.0, 1_560_000.0);
        for extent in [[cx - 10_000.0, cy - 25.0, cx + 10_000.0, cy + 25.0], [cx - 25.0, cy - 10_000.0, cx + 25.0, cy + 10_000.0]] {
            let b = build(Map::new("Map"), &Scene::new(extent), small()).unwrap();
            let d = b.drape.as_ref().unwrap();
            let long = (extent[2] - extent[0]) > (extent[3] - extent[1]);
            assert_eq!(if long { (d.width(), d.height()) } else { (d.height(), d.width()) }, (256, 1));
        }
        // 50 to 1: as drawn, its sides in proportion.
        let b = build(Map::new("Map"), &Scene::new([cx - 5000.0, cy - 100.0, cx + 5000.0, cy + 100.0]), small()).unwrap();
        let d = b.drape.as_ref().unwrap();
        assert_eq!((d.width(), d.height()), (256, 5));
    }

    #[test]
    fn what_is_drawn_round_the_box_is_cut_off() {
        let mut pm = Pixmap::new(40, 16).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(0, 0, 255, 255));
        // The box: 4 rows at the middle, red.
        let red =
            tiny_skia::Paint { shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(255, 0, 0, 255)), ..Default::default() };
        pm.fill_rect(tiny_skia::Rect::from_xywh(0.0, 6.0, 40.0, 4.0).unwrap(), &red, tiny_skia::Transform::identity(), None);
        let cut = box_of(pm.clone(), 40.0, 4.0);
        assert_eq!((cut.width(), cut.height()), (40, 4));
        assert!(cut.pixels().iter().all(|p| (p.red(), p.blue()) == (255, 0)));
        // A box as big as the picture: all of it.
        assert_eq!(box_of(pm.clone(), 40.0, 16.0).data(), pm.data());
        // Less than a pixel: the one at its middle.
        let cut = box_of(pm, 40.0, 0.3);
        assert_eq!((cut.width(), cut.height()), (40, 1));
        assert_eq!(cut.pixels()[0].red(), 255);
    }

    #[test]
    fn each_well_of_a_multipoint_is_picked_with_its_own_head() {
        let (mut m, mut s) = hill();
        let (cx, cy) = (11_200_000.0, 1_560_000.0);
        let ll = |x: f64, y: f64| rustgis_geom::merc_to_lonlat(Coord::new(x, y));
        let mut pair = Dataset::new("pair", GeometryType::Point, vec![Field::new("ลึก", FieldType::Double)]);
        // Low on the hill's foot first, high near its top last.
        pair.push(Some(Geometry::MultiPoint(vec![ll(cx + 1000.0, cy), ll(cx - 200.0, cy)])), vec![Value::Float(100.0)]);
        m.add_dataset(pair);
        let mut w = SceneLayer::new("pair", SceneKind::Well);
        w.depth = Some("ลึก".into());
        s.layers = vec![w];
        let b = build(m, &s, small()).unwrap();
        let k = b.origin.2;
        assert!(b.ground_at(-200.0 * k, 0.0) - b.ground_at(1000.0 * k, 0.0) > 50.0, "the two heads differ");
        for x in [1000.0 * k, -200.0 * k] {
            let cam = SceneCamera { heading: 0.0, tilt: 0.0, zoom: 0.3, target: [x, 0.0, 0.0] };
            let p = pick(&b, &cam, 1.0, 400.0, 300.0, 200.0, 150.0).unwrap().unwrap();
            let f = p.feature.clone().unwrap();
            assert!(f.below_head.unwrap() < 1.0, "at its own head: {p:?}");
            assert!(p.below_ground.is_none(), "{p:?}");
        }
    }

    #[test]
    fn a_ruler_marks_the_corner_in_round_metres() {
        assert_eq!(ruler_step(1000.0, 500.0, 1.0), 200.0);
        assert_eq!(ruler_step(300.0, 1000.0, 1.0), 50.0);
        assert_eq!(ruler_step(300.0, 1000.0, 2.0), 50.0);
        assert_eq!(ruler_step(300.0, 150.0, 1.0), 100.0);
        assert_eq!(fmt_metres(1250.0, 50.0), "1,250");
        assert_eq!(fmt_metres(-40.0, 20.0), "-40");
        assert_eq!(fmt_metres(12.5, 0.5), "12.5");
        assert_eq!(fmt_metres(-0.0, 10.0), "0");
        let (m, mut s) = hill();
        assert!(Look::of(&s).ruler, "on by default");
        // Stretched, so its corner stands tall enough on the picture to mark.
        s.exaggeration = 5.0;
        let b = build(m, &s, small()).unwrap();
        let side = SceneCamera { heading: 30.0, tilt: 80.0, zoom: 1.1, target: [0.0, 0.0, 0.0] };
        let with =
            |ruler: bool, cam: &SceneCamera| draw_with(&b, cam, Look { opacity: 1.0, ruler, ruler_step: None }, 400, 300, 1.0, 1).unwrap();
        let (on, off) = (with(true, &side), with(false, &side));
        let differ = on.data().chunks(4).zip(off.data().chunks(4)).filter(|(a, c)| a != c).count();
        assert!(differ > 200, "the ruler and its numbers are drawn: {differ} pixels");
        assert_eq!(off.data(), draw(&b, &side, 1.0, 400, 300, 1.0, 1).unwrap().data());
        // From straight above the corner is a point: no ruler.
        let above = SceneCamera { tilt: 0.0, ..side };
        assert_eq!(with(true, &above).data(), with(false, &above).data());
    }

    #[test]
    fn a_ruler_is_marked_at_the_step_asked_and_numbered_where_numbers_fit() {
        // 25 m over 1,000 m on 500 px: every mark drawn, numbers 34 px (68 m) apart at least,
        // so every 4th numbered, at the hundreds.
        assert_eq!(ruler_every(25.0, 1000.0, 500.0, 1.0), (1, 4));
        // 20 m: every 5th, at the hundreds again.
        assert_eq!(ruler_every(20.0, 1000.0, 500.0, 1.0), (1, 5));
        // 30 m divides no round height: every 5th, 150 m apart.
        assert_eq!(ruler_every(30.0, 1000.0, 500.0, 1.0), (1, 5));
        // Wide apart: all numbered.
        assert_eq!(ruler_every(100.0, 1000.0, 1000.0, 1.0), (1, 1));
        // Too fine to draw every mark: at a multiple, at most RULER_MARKS.
        let (draw, _) = ruler_every(0.1, 1000.0, 1000.0, 1.0);
        assert_eq!(draw, 25);
        assert!(1000.0 / (0.1 * draw as f64) <= RULER_MARKS);
        let (m, mut s) = hill();
        s.exaggeration = 5.0;
        let b = build(m, &s, small()).unwrap();
        let side = SceneCamera { heading: 30.0, tilt: 80.0, zoom: 1.1, target: [0.0, 0.0, 0.0] };
        let with =
            |ruler_step: Option<f64>| draw_with(&b, &side, Look { opacity: 1.0, ruler: true, ruler_step }, 400, 300, 1.0, 1).unwrap();
        let auto = with(None);
        assert_ne!(with(Some(10.0)).data(), auto.data(), "marked every 10 m");
        // A step that is no number is the automatic one.
        assert_eq!(with(Some(f64::NAN)).data(), auto.data());
        s.ruler_step = Some(10.0);
        assert_eq!(Look::of(&s).ruler_step, Some(10.0));
    }

    #[test]
    fn the_ground_follows_the_dem_and_the_base_goes_below_the_wells() {
        let (m, s) = hill();
        let b = build(m, &s, small()).unwrap();
        // The hill's top in the middle, its foot near the edges.
        assert!((b.ground_at(0.0, 0.0) - 400.0).abs() < 15.0, "{}", b.ground_at(0.0, 0.0));
        assert!(b.ground_at(b.half.0, 0.0) < 160.0);
        // One well stood up, 120 m deep from the ground; the other has no depth, said.
        assert!(b.notes.iter().any(|n| n.starts_with("1 wells of wells have no depth")), "{:?}", b.notes);
        let well_bottom = b.ground_at(-1000.0 * s.ground_scale(), 0.0) - 120.0;
        assert!(b.z_range.0 < well_bottom - 50.0, "{:?} vs {well_bottom}", b.z_range);
        assert_eq!(b.labels.len(), 1);
        assert_eq!(b.labels[0].text, "บ่อ ก");
    }

    #[test]
    fn a_scene_is_drawn_from_above_and_below() {
        let (m, mut s) = hill();
        s.layers[0].size = Some(120.0);
        let b = build(m, &s, small()).unwrap();
        let red = |pm: &Pixmap| {
            pm.pixels()
                .iter()
                .filter(|p| p.red() > 90 && p.red() > p.green().saturating_add(50) && p.red() > p.blue().saturating_add(50))
                .count()
        };
        // From above, opaque ground: the well's head shows, its tube under the ground doesn't.
        let above = SceneCamera { heading: 0.0, tilt: 0.0, zoom: 1.0, target: [0.0; 3] };
        let pm = draw(&b, &above, 1.0, 160, 120, 1.0, 1).unwrap();
        let shown = red(&pm);
        assert!(shown > 0, "the well's head");
        // Level and through clear ground, the whole tube shows: far more of it.
        let side = SceneCamera { heading: 0.0, tilt: 90.0, zoom: 1.0, target: [0.0, 0.0, -100.0] };
        let solid = red(&draw(&b, &side, 1.0, 400, 300, 1.0, 1).unwrap());
        let clear = red(&draw(&b, &side, 0.3, 400, 300, 1.0, 1).unwrap());
        assert!(clear > solid * 2 && clear > 20, "{solid} → {clear}");
        // From below, the base.
        let under = SceneCamera { heading: 0.0, tilt: 170.0, zoom: 1.0, target: [0.0; 3] };
        let pm = draw(&b, &under, 1.0, 64, 48, 1.0, 1).unwrap();
        let mid = pm.pixel(32, 24).unwrap();
        let walls = rgb(s.walls);
        assert!(
            [mid.red(), mid.green(), mid.blue()].iter().zip(walls).all(|(c, w)| (f32::from(*c) / 255.0 - w * BASE_SHADE).abs() < 0.06),
            "{mid:?}"
        );
    }

    #[test]
    fn smoothing_draws_the_same_picture() {
        let (m, s) = hill();
        let b = build(m, &s, small()).unwrap();
        let cam = SceneCamera::default();
        let (a, c) = (draw(&b, &cam, 1.0, 80, 60, 1.0, 1).unwrap(), draw(&b, &cam, 1.0, 80, 60, 1.0, 2).unwrap());
        assert_eq!((c.width(), c.height()), (80, 60));
        let diff: f64 = a.data().iter().zip(c.data()).map(|(x, y)| f64::from((i16::from(*x) - i16::from(*y)).unsigned_abs())).sum::<f64>()
            / a.data().len() as f64;
        assert!(diff < 12.0, "{diff}");
    }

    #[test]
    fn a_polygon_round_the_box_is_cut_to_it() {
        let half = (100.0, 50.0);
        let area = |r: &[(f64, f64)]| {
            ((0..r.len()).map(|i| r[i].0 * r[(i + 1) % r.len()].1 - r[(i + 1) % r.len()].0 * r[i].1).sum::<f64>() / 2.0).abs()
        };
        let big = vec![(-500.0, -500.0), (500.0, -500.0), (500.0, 500.0), (-500.0, 500.0)];
        // All round it: the box.
        let round = cut_to_box(&big, &[], half);
        assert!(round.len() == 1 && (area(&round[0].0) - 200.0 * 100.0).abs() < 1e-6, "{round:?}");
        // Across it, no corner inside: the part inside.
        let across = cut_to_box(&[(-300.0, -10.0), (300.0, -10.0), (300.0, 10.0), (-300.0, 10.0)], &[], half);
        assert!(across.len() == 1 && (area(&across[0].0) - 200.0 * 20.0).abs() < 1e-6, "{across:?}");
        // Outside it: nothing.
        assert!(cut_to_box(&[(200.0, 0.0), (300.0, 0.0), (300.0, 10.0)], &[], half).is_empty());
        // A hole across the box's left edge: a notch in the outline, roofed without a fan.
        let hole = vec![(-150.0, -20.0), (-50.0, -20.0), (-50.0, 20.0), (-150.0, 20.0)];
        let cut = cut_to_box(&big, std::slice::from_ref(&hole), half);
        assert_eq!(cut.len(), 1, "{cut:?}");
        let (o, hs) = &cut[0];
        let holes_area: f64 = hs.iter().map(|h| area(h)).sum();
        assert!((area(o) - holes_area - (200.0 * 100.0 - 50.0 * 40.0)).abs() < 1e-6, "{cut:?}");
        let (tris, fan) = rustgis_geom::triangulate::triangulate_reporting(o, hs).unwrap();
        assert!(!fan && !tris.is_empty());
    }

    #[test]
    fn a_drawing_stops_when_cancelled() {
        let (m, s) = hill();
        let b = build(m, &s, small()).unwrap();
        struct Cancelled;
        impl rustgis_data::progress::Probe for Cancelled {
            fn cancelled(&self) -> bool {
                true
            }
            fn begin(&self, _: u64) {}
            fn advance(&self, _: u64) {}
        }
        let installed = rustgis_data::progress::install(Some(std::sync::Arc::new(Cancelled)));
        let r = draw(&b, &SceneCamera::default(), 1.0, 400, 300, 1.0, 2);
        drop(installed);
        assert!(r.unwrap_err().contains("ancel"));
        assert!(draw(&b, &SceneCamera::default(), 1.0, 0, 300, 1.0, 1).unwrap_err().contains("no pixels"));
    }

    #[test]
    fn panning_moves_along_the_ground() {
        let (m, s) = hill();
        let b = build(m, &s, small()).unwrap();
        let mut cam = SceneCamera { heading: 90.0, tilt: 0.0, zoom: 1.0, target: [0.0; 3] };
        // Facing east, looking down: dragging right moves the ground right (south on the
        // picture's right... the eye goes north).
        pan(&b, &mut cam, 10.0, 0.0, 100.0);
        assert!(cam.target[1] > 0.0 && cam.target[0].abs() < 1e-6, "{:?}", cam.target);
        assert_eq!(cam.target[2], 0.0);
    }

    #[test]
    fn what_the_map_no_longer_has_is_left_out_and_said() {
        let (m, mut s) = hill();
        s.layers[0].depth = Some("nope".into());
        let b = build(m.clone(), &s, small()).unwrap();
        assert!(b.notes.iter().any(|n| n.contains("no field 'nope'")) && b.solids.tris.is_empty(), "{:?}", b.notes);
        s.layers[0].layer = "gone".into();
        assert!(build(m.clone(), &s, small()).unwrap().notes.iter().any(|n| n.contains("'gone', which isn't a layer")));
        s.layers.clear();
        s.dem = Some("wells".into());
        let b = build(m, &s, small()).unwrap();
        assert!(b.notes.iter().any(|n| n.contains("the ground is flat")) && b.z_range.1 == 0.0, "{:?}", b.notes);
    }
    #[test]
    fn only_the_features_the_map_draws_are_stood_up() {
        let (mut m, s) = hill();
        // The well with no depth filtered out by the layer's definition query: not said missing.
        if let Some(l) = m.layers.iter_mut().find(|l| l.name == "wells") {
            l.definition_query = Some("ลึก > 0".into());
        }
        let b = build(m, &s, small()).unwrap();
        assert!(!b.notes.iter().any(|n| n.contains("no depth")), "{:?}", b.notes);
        assert_eq!(b.labels.len(), 1);
    }

    #[test]
    fn a_streamed_layer_reads_the_scene_before_it_stands_up() {
        use rustgis_data::source::{Batch, FeatureSource, Request, SourceInfo};
        struct All(SourceInfo, Dataset);
        impl FeatureSource for All {
            fn info(&self) -> &SourceInfo {
                &self.0
            }
            fn read(&self, _: &Request) -> rustgis_data::Result<Batch> {
                Ok(Batch {
                    features: self.1.features.iter().cloned().collect(),
                    matched: self.1.features.len() as u64,
                    complete: true,
                    ..Default::default()
                })
            }
        }
        let (mut m, s) = hill();
        let Some(l) = m.layers.iter_mut().find(|l| l.name == "wells") else { panic!("no wells") };
        let ds = l.dataset.clone();
        let info = SourceInfo {
            name: ds.name.clone(),
            format: "test",
            geometry_type: ds.geometry_type,
            fields: ds.fields.clone(),
            extent: ds.extent(),
            count: Some(ds.features.len() as u64),
            crs: "EPSG:4326".into(),
            indexed: true,
            notes: Vec::new(),
            zm_dropped: false,
        };
        // Nothing read yet, and hidden in the map (so the drape doesn't read it).
        l.dataset.features = Vec::new().into();
        l.visible = false;
        l.source = Some(rustgis_map::source::SourceLayer::from_path("wells.shp", None, Box::new(All(info, ds))));
        let b = build(m, &s, small()).unwrap();
        assert_eq!(b.labels.len(), 1, "{:?}", b.notes);
        assert!(!b.solids.tris.is_empty());
    }

    #[test]
    fn labels_over_the_limit_are_said() {
        let (mut m, s) = hill();
        let Some(l) = m.layers.iter_mut().find(|l| l.name == "wells") else { panic!("no wells") };
        let (cx, cy) = (11_200_000.0, 1_560_000.0);
        let ll = |x: f64, y: f64| rustgis_geom::merc_to_lonlat(Coord::new(x, y));
        for i in 0..MAX_LABELS + 10 {
            let (dx, dy) = ((i % 60) as f64 * 40.0 - 1200.0, (i / 60) as f64 * 40.0 - 1200.0);
            l.dataset
                .push(Some(Geometry::Point(ll(cx + dx, cy + dy))), vec![Value::Text(format!("บ่อ {i}")), Value::Float(50.0), Value::Null]);
        }
        let b = build(m, &s, small()).unwrap();
        assert_eq!(b.labels.len(), MAX_LABELS);
        assert!(b.notes.iter().any(|n| n.contains(&format!("at most {MAX_LABELS} labels, so 11 wells of wells"))), "{:?}", b.notes);
    }

    #[test]
    fn a_layer_whose_source_failed_is_said() {
        let (mut m, s) = hill();
        let Some(l) = m.layers.iter_mut().find(|l| l.name == "wells") else { panic!("no wells") };
        let mut src = rustgis_map::source::SourceLayer::default();
        src.error = Some("the disk is gone".into());
        l.source = Some(src);
        let b = build(m, &s, small()).unwrap();
        assert!(
            b.notes.iter().any(|n| n.contains("wells may be missing from the scene") && n.contains("the disk is gone")),
            "{:?}",
            b.notes
        );
    }

    #[test]
    fn standing_up_stops_when_cancelled() {
        struct Cancelled;
        impl rustgis_data::progress::Probe for Cancelled {
            fn cancelled(&self) -> bool {
                true
            }
            fn begin(&self, _: u64) {}
            fn advance(&self, _: u64) {}
        }
        let (m, s) = hill();
        let mut b = build(m.clone(), &s, small()).unwrap();
        let before = b.solids.tris.len();
        let (local, inside) = (|c: Coord| (c.x, c.y), |_: (f64, f64)| true);
        let at = Placing { local: &local, inside: &inside, auto_size: 10.0, ex: 1.0, scale: 50_000.0 };
        let installed = rustgis_data::progress::install(Some(std::sync::Arc::new(Cancelled)));
        let r = stand_up(&m, &s.layers[0], &mut b, &at, &mut Vec::new());
        drop(installed);
        assert!(r.unwrap_err().contains("ancel"));
        assert_eq!(b.solids.tris.len(), before, "nothing more stood up");
    }

    #[test]
    fn a_polygon_too_detailed_for_a_roof_is_said() {
        let (mut m, mut s) = hill();
        let (cx, cy) = (11_200_000.0, 1_560_000.0);
        let n = rustgis_geom::triangulate::MAX_VERTICES + 100;
        let ring: Vec<Coord> = (0..=n)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / n as f64;
                rustgis_geom::merc_to_lonlat(Coord::new(cx + 500.0 * a.cos(), cy + 500.0 * a.sin()))
            })
            .collect();
        let mut d = Dataset::new("big", GeometryType::Polygon, vec![]);
        d.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        m.add_dataset(d);
        s.layers = vec![SceneLayer::new("big", SceneKind::Extrude)];
        let b = build(m, &s, small()).unwrap();
        assert!(b.notes.iter().any(|n| n.contains("without a roof")), "{:?}", b.notes);
        // Its walls stood up still.
        assert!(!b.solids.tris.is_empty());
    }
}
