//! A map's 3D scene (ARutGIS): the ground of a study area cut out as a block, the map draped over
//! it, and features stood up in it (wells down to their depth, points at their heights, polygons
//! raised), seen by a camera that turns about it, from above the ground or from below it.
//!
//! What a scene holds is saved with its map; what it draws is worked out by the renderer
//! (`rustgis_render::scene3d`).

use serde::{Deserialize, Serialize};

use crate::color::Rgba;

/// The least side of a scene's box (Web Mercator metres): shorter, it was most likely given in
/// degrees.
pub const MIN_SIDE: f64 = 10.0;
/// The longest side of a scene's box (Web Mercator metres): a study area, not a country.
pub const MAX_SIDE: f64 = 2_000_000.0;
/// The most a scene's heights are exaggerated by.
pub const MAX_EXAGGERATION: f64 = 50.0;
/// The deepest a scene's base lies below its lowest ground or deepest well (metres).
pub const MAX_DEPTH: f64 = 20_000.0;
/// The most layers a scene stands up.
pub const MAX_LAYERS: usize = 32;
/// The widest a feature stands in a scene (metres): a well's or a point's.
pub const MAX_SIZE: f64 = 10_000.0;
/// The tallest a raised polygon stands, and the deepest a well goes (metres).
pub const MAX_HEIGHT: f64 = 20_000.0;

/// The colour of a scene's sides and base when none is given: the soil of a block diagram.
pub const WALLS: Rgba = crate::view::BLOCK_WALLS;

/// A map's 3D scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    /// The study area, `[xmin, ymin, xmax, ymax]` in Web Mercator metres.
    pub extent: [f64; 4],
    /// The raster layer of ground heights (metres), by name. None: flat ground at 0 m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dem: Option<String>,
    /// How much heights and depths are stretched.
    #[serde(default = "one")]
    pub exaggeration: f64,
    /// How opaque the ground is, 0 to 1: below 1 what is under it shows through.
    #[serde(default = "one")]
    pub ground_opacity: f64,
    /// Metres from the lowest ground, or the deepest well when that is deeper, to the base.
    /// None: a tenth of the box's longer side, at least 50 m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f64>,
    /// The colour of the sides and the base.
    #[serde(default = "walls")]
    pub walls: Rgba,
    /// The layers stood up in the scene, drawn in this order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<SceneLayer>,
    /// Where the scene is seen from.
    #[serde(default)]
    pub camera: SceneCamera,
}

fn one() -> f64 {
    1.0
}

fn walls() -> Rgba {
    WALLS
}

/// How a layer stands in a scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneKind {
    /// Points as wells: tubes from the wellhead down to their depth.
    Well,
    /// Points as balls at their height.
    Point,
    /// Polygons raised from the ground.
    Extrude,
}

impl SceneKind {
    pub fn code(self) -> &'static str {
        match self {
            SceneKind::Well => "well",
            SceneKind::Point => "point",
            SceneKind::Extrude => "extrude",
        }
    }

    pub fn from_code(s: &str) -> Option<SceneKind> {
        match s {
            "well" => Some(SceneKind::Well),
            "point" => Some(SceneKind::Point),
            "extrude" => Some(SceneKind::Extrude),
            _ => None,
        }
    }
}

/// A layer stood up in a scene, its fields named (as the layer has them). Which apply depends on
/// its kind: a well's `depth`, `top` and `water`; a point's `elevation` and `offset`; a raised
/// polygon's `height` and `offset`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneLayer {
    /// The layer, by name.
    pub layer: String,
    pub kind: SceneKind,
    /// A well's depth below its head (metres), a field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<String>,
    /// A well's head: its height above sea level (metres), a field. None: the ground's height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<String>,
    /// A well's water level: its depth below the head (metres), a field, marked by a ring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub water: Option<String>,
    /// A point's height above sea level (metres), a field. None: the ground's, raised by `offset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevation: Option<String>,
    /// A raised polygon's height (metres), a field. None: `height_m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<String>,
    /// A raised polygon's height where no field gives it (metres).
    #[serde(default = "ten")]
    pub height_m: f64,
    /// Metres above the ground a point or a raised polygon starts at.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f64,
    /// A well's or a point's width (metres). None: worked out from the box, to show at its scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// Its colour. None: the layer's symbology's, feature by feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgba>,
    /// The field written beside each feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn ten() -> f64 {
    10.0
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl SceneLayer {
    /// A layer of `kind` with nothing else set.
    pub fn new(layer: impl Into<String>, kind: SceneKind) -> SceneLayer {
        SceneLayer {
            layer: layer.into(),
            kind,
            depth: None,
            top: None,
            water: None,
            elevation: None,
            height: None,
            height_m: ten(),
            offset: 0.0,
            size: None,
            color: None,
            label: None,
        }
    }

    /// The fields it reads, by what they are for: checked against the layer's own.
    pub fn fields(&self) -> Vec<(&'static str, &str)> {
        let named = [
            ("depth", &self.depth),
            ("top", &self.top),
            ("water", &self.water),
            ("elevation", &self.elevation),
            ("height", &self.height),
            ("label", &self.label),
        ];
        named.into_iter().filter_map(|(k, v)| v.as_deref().map(|v| (k, v))).collect()
    }
}

/// Where a scene is seen from: the point it turns about and the eye's direction and distance.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneCamera {
    /// The way the eye looks, degrees clockwise from north.
    pub heading: f64,
    /// Degrees from looking straight down: 90 looks level, past 90 up from below the ground.
    pub tilt: f64,
    /// The eye's distance from the point it turns about, as a share of the box's diagonal.
    pub zoom: f64,
    /// The point it turns about: metres east, north and up of the middle of the box's ground.
    #[serde(default)]
    pub target: [f64; 3],
}

/// The steepest a scene is looked up at from below (degrees from looking straight down).
pub const MAX_TILT: f64 = 175.0;
/// The nearest and farthest the eye comes, as a share of the box's diagonal.
pub const ZOOM_RANGE: (f64, f64) = (0.02, 20.0);

impl Default for SceneCamera {
    fn default() -> Self {
        SceneCamera { heading: 30.0, tilt: 60.0, zoom: 1.1, target: [0.0; 3] }
    }
}

impl SceneCamera {
    /// Kept within its ranges: the heading turned to [0, 360), the tilt to 0 to [`MAX_TILT`] and
    /// the zoom to [`ZOOM_RANGE`]; what isn't a number is the default's.
    pub fn clamp(&mut self) {
        let d = SceneCamera::default();
        self.heading = if self.heading.is_finite() { self.heading.rem_euclid(360.0) } else { d.heading };
        self.tilt = if self.tilt.is_finite() { self.tilt.clamp(0.0, MAX_TILT) } else { d.tilt };
        self.zoom = if self.zoom.is_finite() { self.zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1) } else { d.zoom };
        for v in &mut self.target {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
    }
}

impl Scene {
    /// A scene of the box `extent` with nothing stood up in it.
    pub fn new(extent: [f64; 4]) -> Scene {
        Scene {
            extent,
            dem: None,
            exaggeration: 1.0,
            ground_opacity: 1.0,
            depth: None,
            walls: WALLS,
            layers: Vec::new(),
            camera: SceneCamera::default(),
        }
    }

    /// Why this scene can't be drawn, when it can't: its box (ordered, within the Web Mercator
    /// world, [`MIN_SIDE`] to [`MAX_SIDE`] a side), its numbers in their ranges and its layers'.
    /// Checked where a scene is set and where a project is read.
    pub fn check(&self) -> Result<(), String> {
        let [x0, y0, x1, y1] = self.extent;
        if !(self.extent.iter().all(|v| v.is_finite()) && x0 < x1 && y0 < y1) {
            return Err("the scene's extent is [xmin, ymin, xmax, ymax], numbers with xmin < xmax and ymin < ymax".into());
        }
        let world = rustgis_geom::MERCATOR_R * std::f64::consts::PI;
        if self.extent.iter().any(|v| v.abs() > world * 1.0001) {
            return Err("the scene's extent is in Web Mercator metres, inside the world".into());
        }
        let side = (x1 - x0).max(y1 - y0);
        if (x1 - x0).min(y1 - y0) < MIN_SIDE {
            return Err(format!("the scene's extent is less than {MIN_SIDE} m a side: it is in Web Mercator metres, not degrees"));
        }
        if side > MAX_SIDE {
            return Err(format!("the scene's extent is {:.0} km a side, more than the {:.0} km a scene shows", side / 1e3, MAX_SIDE / 1e3));
        }
        if !(self.exaggeration.is_finite() && self.exaggeration > 0.0 && self.exaggeration <= MAX_EXAGGERATION) {
            return Err(format!("exaggeration is a number above 0, at most {MAX_EXAGGERATION}"));
        }
        if !(0.0..=1.0).contains(&self.ground_opacity) {
            return Err("groundOpacity is 0 (clear) to 1 (solid)".into());
        }
        if let Some(d) = self.depth
            && !(d.is_finite() && (0.0..=MAX_DEPTH).contains(&d))
        {
            return Err(format!("depth is metres, 0 to {MAX_DEPTH}"));
        }
        if self.walls.a() != 255 {
            return Err("walls is an opaque colour, #RRGGBB".into());
        }
        if self.layers.len() > MAX_LAYERS {
            return Err(format!("a scene stands up at most {MAX_LAYERS} layers"));
        }
        for l in &self.layers {
            l.check()?;
        }
        Ok(())
    }

    /// The box's middle (Web Mercator metres).
    pub fn center(&self) -> (f64, f64) {
        let [x0, y0, x1, y1] = self.extent;
        ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
    }

    /// Ground metres to a Web Mercator metre at the box's middle: Web Mercator stretches the
    /// ground by the secant of the latitude.
    pub fn ground_scale(&self) -> f64 {
        let (_, cy) = self.center();
        let lat = rustgis_geom::merc_to_lonlat(rustgis_geom::Coord::new(0.0, cy)).y;
        lat.to_radians().cos().max(1e-6)
    }

    /// The box's width and height on the ground (metres).
    pub fn ground_size(&self) -> (f64, f64) {
        let [x0, y0, x1, y1] = self.extent;
        let k = self.ground_scale();
        ((x1 - x0) * k, (y1 - y0) * k)
    }

    /// Hands every layer name it holds (its DEM's and its layers') to `rename`, which changes
    /// those a renamed layer had.
    pub fn rename_layer(&mut self, mut rename: impl FnMut(&mut String)) {
        if let Some(d) = &mut self.dem {
            rename(d);
        }
        for l in &mut self.layers {
            rename(&mut l.layer);
        }
    }
}

impl SceneLayer {
    fn check(&self) -> Result<(), String> {
        let who = &self.layer;
        if self.kind == SceneKind::Well && self.depth.is_none() {
            return Err(format!("{who}: a well layer needs its depth field"));
        }
        if !(self.height_m.is_finite() && (0.0..=MAX_HEIGHT).contains(&self.height_m)) {
            return Err(format!("{who}: heightM is metres, 0 to {MAX_HEIGHT}"));
        }
        if !(self.offset.is_finite() && self.offset.abs() <= MAX_HEIGHT) {
            return Err(format!("{who}: offset is metres, -{MAX_HEIGHT} to {MAX_HEIGHT}"));
        }
        if let Some(s) = self.size
            && !(s.is_finite() && s > 0.0 && s <= MAX_SIZE)
        {
            return Err(format!("{who}: size is metres, above 0 and at most {MAX_SIZE}"));
        }
        Ok(())
    }
}

/// Reads a map's scene as a project file holds it: one that [`Scene::check`] refuses is left
/// out (a project file is hostile input), and its camera is kept within its ranges.
pub(crate) fn de_scene<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Scene>, D::Error> {
    let Some(mut scene) = Option::<Scene>::deserialize(d)? else { return Ok(None) };
    scene.camera.clamp();
    Ok(scene.check().is_ok().then_some(scene))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bkk() -> [f64; 4] {
        [11_190_000.0, 1_540_000.0, 11_200_000.0, 1_548_000.0]
    }

    #[test]
    fn a_scene_round_trips_and_leaves_out_what_is_unset() {
        let mut s = Scene::new(bkk());
        let mut w = SceneLayer::new("บ่อบาดาล", SceneKind::Well);
        w.depth = Some("ความลึก".into());
        s.layers.push(w);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["layers"][0], serde_json::json!({"layer": "บ่อบาดาล", "kind": "well", "depth": "ความลึก", "heightM": 10.0}));
        assert!(v.get("dem").is_none() && v.get("depth").is_none());
        let back: Scene = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
        // Only the box is needed.
        let least: Scene = serde_json::from_value(serde_json::json!({"extent": bkk()})).unwrap();
        assert_eq!(least, Scene::new(bkk()));
    }

    #[test]
    fn check_refuses_what_cant_be_drawn() {
        assert!(Scene::new(bkk()).check().is_ok());
        let bad = |f: &dyn Fn(&mut Scene)| {
            let mut s = Scene::new(bkk());
            f(&mut s);
            s.check().unwrap_err()
        };
        assert!(bad(&|s| s.extent = [100.0, 13.0, 101.0, 14.0]).contains("degrees"));
        assert!(bad(&|s| s.extent = [1.0, 0.0, 0.0, 1.0]).contains("xmin < xmax"));
        assert!(bad(&|s| s.extent = [0.0, 0.0, 5e6, 5e6]).contains("km"));
        assert!(bad(&|s| s.exaggeration = 0.0).contains("exaggeration"));
        assert!(bad(&|s| s.ground_opacity = 1.5).contains("groundOpacity"));
        assert!(bad(&|s| s.depth = Some(-1.0)).contains("depth"));
        assert!(bad(&|s| s.walls = Rgba::new(1, 2, 3, 4)).contains("opaque"));
        assert!(bad(&|s| s.layers.push(SceneLayer::new("w", SceneKind::Well))).contains("depth field"));
        assert!(
            bad(&|s| {
                let mut p = SceneLayer::new("p", SceneKind::Point);
                p.size = Some(0.0);
                s.layers.push(p);
            })
            .contains("size")
        );
    }

    #[test]
    fn a_saved_scene_that_cant_be_drawn_is_left_out() {
        let mut m = crate::Map::new("m");
        m.scene = Some(Scene::new(bkk()));
        m.scene.as_mut().unwrap().camera.tilt = 30.0;
        let mut v = serde_json::to_value(&m).unwrap();
        let back: crate::Map = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back.scene, m.scene);
        v["scene"]["camera"]["tilt"] = serde_json::json!(999.0);
        let back: crate::Map = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back.scene.unwrap().camera.tilt, MAX_TILT);
        v["scene"]["exaggeration"] = serde_json::json!(-3.0);
        let back: crate::Map = serde_json::from_value(v).unwrap();
        assert_eq!(back.scene, None);
    }

    #[test]
    fn the_camera_keeps_to_its_ranges() {
        let mut c = SceneCamera { heading: -30.0, tilt: 400.0, zoom: 0.0, target: [f64::NAN, 1.0, 2.0] };
        c.clamp();
        assert_eq!(c, SceneCamera { heading: 330.0, tilt: MAX_TILT, zoom: ZOOM_RANGE.0, target: [0.0, 1.0, 2.0] });
    }

    #[test]
    fn the_ground_is_measured_in_metres() {
        let s = Scene::new(bkk());
        let (w, h) = s.ground_size();
        // About 14° N: Web Mercator stretches the ground by about 3 %.
        assert!((w - 10_000.0 * s.ground_scale()).abs() < 1e-6 && (9_650.0..9_750.0).contains(&w), "{w}");
        assert!((h / w - 0.8).abs() < 1e-9);
    }
}
