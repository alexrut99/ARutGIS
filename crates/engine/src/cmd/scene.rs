//! 3D Scene (ARutGIS): a study area's ground cut out as a block with the map draped over it,
//! and layers stood up in it (wells down to their depth, points at their heights, polygons
//! raised). The scene is kept with its map ([`rustgis_map::scene::Scene`]) and drawn by
//! `rustgis_render::scene3d`: in the app's 3D Scene view, or to an image or an animation here.

use rustgis_map::scene::{MAX_LAYERS, Scene, SceneCamera, SceneKind, SceneLayer};
use rustgis_map::{Map, Rgba};
use serde_json::{Value, json};

use super::{b, f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("scene.set", "3D Scene", "View › Scene", set).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r##"{"extent"?: "dem" (the DEM's extent; the default with a DEM) | "view" (the map's view; the default without one) | [xmin, ymin, xmax, ymax] in Web Mercator metres, "bbox"?: [west, south, east, north] in degrees, "extentOf"?: a layer whose extent the scene takes, "dem"?: a raster layer of heights in metres, or null for flat ground, "exaggeration"?: above 0, at most 50 (default 1), "groundOpacity"?: 0 (clear) to 1 (solid, the default), "depth"?: metres from the lowest ground or deepest well to the base, or null (a tenth of the area's longer side), "walls"?: "#RRGGBB", "layers"?: [layers as scene.layer takes them] (replaces them all), "camera"?: as scene.camera takes it}"##,
        ),
        C::new("scene.layer", "Scene Layer", "View › Scene", layer).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r##"{"layer": a layer of the map, "kind"?: "well" (points as tubes down to their depth) | "point" (points as balls at their height) | "extrude" (polygons raised from the ground) (default: well for points, extrude for polygons), "depth"?: field of a well's depth below its head (metres; needed for wells), "top"?: field of a wellhead's height above sea level (default: the ground's), "water"?: field of a well's water level, its depth below the head, "elevation"?: field of a point's height above sea level (default: the ground's), "height"?: field of a polygon's height, "heightM"?: metres a polygon is raised where no field gives it (default 10), "offset"?: metres above the ground a point or polygon starts, "size"?: a well's or point's width in metres (default: to show at the area's scale), "color"?: "#RRGGBB" (default: the layer's symbology), "label"?: field written beside each, "remove"?: true (takes the layer out of the scene)}"##,
        ),
        C::new("scene.camera", "Scene Camera", "View › Scene", camera).roles(&[]).effects(crate::policy::Effects::NONE).view().params(
            r#"{"heading"?: degrees clockwise from north the eye looks, "turn"?: degrees added to the heading, "tilt"?: degrees from looking straight down, 0 to 175 (90 looks level; past it, up from below the ground), "zoom"?: the eye's distance as a share of the area's diagonal, 0.02 to 20, "target"?: [east, north, up] metres from the middle of the ground the view turns about, "reset"?: true}"#,
        ),
        C::new("scene.remove", "Remove 3D Scene", "View › Scene", |s, _| {
            let had = s.map_mut().scene.take().is_some();
            Ok(json!({"removed": had}))
        })
        .roles(&[])
        .effects(crate::policy::Effects::NONE)
        .when(need_scene)
        .params("{}"),
        C::new("scene.export", "Export Scene", "View › Scene", |s, p| super::exports::write_here(s, p, "scene.export"))
            .inputs(scene_inputs)
            .roles(&[])
            .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .when(need_scene)
            .params(r#"{"path": "*.png", "width"?, "height"?: pixels, 64 to 8192 (default 1600 × 1000), "smooth"?: 1 to 3 (samples a pixel each way; default 2)}"#),
        C::new("scene.exportOrbit", "Export Scene Orbit", "View › Scene", |s, p| super::exports::write_here(s, p, "scene.exportOrbit"))
            .inputs(scene_inputs)
            .roles(&[])
            .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .when(need_scene)
            .params(r#"{"path": "*.gif" | "*.zip" (PNG frames), "frames"?: 2 to 720 (default 72), "turn"?: degrees the heading turns by over the frames (default 360; negative turns the other way), "width"?, "height"?: pixels, 64 to 2048 (default 800 × 500), "speed"?: milliseconds a frame shows for (default 60)}"#),
    ]
}

/// The scene's map is the active map: its sources are what the scene drapes.
fn scene_inputs(input: &super::InputContext<'_>, _: &Value) -> Result<super::Inputs, String> {
    Ok(super::tiles::render_inputs(input.map()))
}

pub(crate) fn need_scene(s: &Session) -> Option<&'static str> {
    if s.map().scene.is_some() { None } else { Some("the map has no 3D scene: make one first (View › 3D Scene)") }
}

/// The DEM's box in Web Mercator metres, when it is a raster layer of the map.
fn dem_box(map: &Map, name: &str) -> Result<[f64; 4], String> {
    let r = map
        .layer_by_name(name)
        .and_then(|l| l.raster.as_ref())
        .and_then(|r| r.data.clone())
        .ok_or_else(|| format!("'{name}' isn't a raster layer in the map (a DEM of heights in metres)"))?;
    rustgis_render::terrain::raster_box(&r).ok_or_else(|| format!("{name}: its extent can't be placed on the map"))
}

/// A box of four numbers.
fn four(v: &Value, what: &str) -> Result<[f64; 4], String> {
    let a = v.as_array().filter(|a| a.len() == 4).ok_or_else(|| format!("{what} is four numbers"))?;
    let n: Vec<f64> = a.iter().filter_map(Value::as_f64).filter(|v| v.is_finite()).collect();
    match n[..] {
        [x0, y0, x1, y1] if x0 < x1 && y0 < y1 => Ok([x0, y0, x1, y1]),
        _ => Err(format!("{what} is four numbers, its minimums before its maximums")),
    }
}

/// The extent of a layer of the map, in Web Mercator metres.
fn layer_box(map: &Map, name: &str) -> Result<[f64; 4], String> {
    let l = map.layer_by_name(name).ok_or_else(|| format!("'{name}' isn't a layer of the map"))?;
    if let Some(r) = l.raster.as_ref().and_then(|r| r.data.clone()) {
        return rustgis_render::terrain::raster_box(&r).ok_or_else(|| format!("{name}: its extent can't be placed on the map"));
    }
    // A streamed layer's whole extent, not only the part read.
    let e = l.extent();
    if !(e.min_x.is_finite() && e.max_x >= e.min_x) {
        return Err(format!("{name} has no features to take an extent from"));
    }
    let (a, c) = (
        rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(e.min_x, e.min_y)),
        rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(e.max_x, e.max_y)),
    );
    // A little room round features at its edge (a single point, some).
    let pad = ((c.x - a.x).max(c.y - a.y) * 0.05).max(250.0);
    Ok([a.x - pad, a.y - pad, c.x + pad, c.y + pad])
}

fn set(s: &mut Session, p: &Value) -> CmdResult {
    let p = p.as_object().ok_or("the parameters are an object")?;
    const KEYS: &[&str] = &["extent", "bbox", "extentOf", "dem", "exaggeration", "groundOpacity", "depth", "walls", "layers", "camera"];
    if let Some(k) = p.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("scene.set has no '{k}' ({})", KEYS.join(", ")));
    }
    let map = s.map();
    // Built on a copy, set only when all of it is good.
    let old = map.scene.clone();
    let dem = match p.get("dem") {
        None => old.as_ref().and_then(|o| o.dem.clone()),
        Some(Value::Null) => None,
        Some(Value::String(n)) => {
            dem_box(map, n)?;
            Some(map.layer_by_name(n).map_or_else(|| n.clone(), |l| l.name.clone()))
        }
        Some(_) => return Err("dem is a raster layer's name, or null".into()),
    };
    let given = [p.get("extent").is_some(), p.get("bbox").is_some(), p.get("extentOf").is_some()].iter().filter(|g| **g).count();
    if given > 1 {
        return Err("give one of extent, bbox and extentOf".into());
    }
    // The view's box in Web Mercator, whatever the map is drawn in (its camera is in the map's
    // own projection): its edges' longitudes and latitudes.
    let view_box = || -> Result<[f64; 4], String> {
        let (w, h) = s.view_size;
        let view = rustgis_render::View { width: w.max(1.0), height: h.max(1.0), pixels_per_point: 1.0, camera: map.camera, sky: false };
        let e = rustgis_render::view_lonlat(map, &view);
        if !(e.min_x.is_finite() && e.max_x > e.min_x && e.max_y > e.min_y) {
            return Err("the map's view can't be placed: give the scene an extent".into());
        }
        let clamp = |y: f64| y.clamp(-85.05, 85.05);
        let (a, c) = (
            rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(e.min_x.max(-180.0), clamp(e.min_y))),
            rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(e.max_x.min(180.0), clamp(e.max_y))),
        );
        Ok([a.x, a.y, c.x, c.y])
    };
    let extent = match (p.get("extent"), p.get("bbox"), ps(&Value::Object(p.clone()), "extentOf")) {
        (Some(Value::String(e)), _, _) if e == "dem" => dem_box(map, dem.as_deref().ok_or("extent \"dem\" needs a DEM")?)?,
        (Some(Value::String(e)), _, _) if e == "view" => view_box()?,
        (Some(e), _, _) => four(e, "extent")?,
        (_, Some(bb), _) => {
            let [w, so, e, n] = four(bb, "bbox")?;
            if w < -180.0 || e > 180.0 || so < -85.06 || n > 85.06 {
                return Err("bbox is [west, south, east, north] in degrees".into());
            }
            let (a, c) = (
                rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(w, so)),
                rustgis_geom::lonlat_to_merc(rustgis_geom::Coord::new(e, n)),
            );
            [a.x, a.y, c.x, c.y]
        }
        (_, _, Some(l)) => layer_box(map, l)?,
        (None, None, None) => match &old {
            Some(o) => o.extent,
            None => match &dem {
                Some(d) => dem_box(map, d)?,
                None => view_box()?,
            },
        },
    };
    let mut scene = old.unwrap_or_else(|| Scene::new(extent));
    scene.extent = extent;
    scene.dem = dem;
    if let Some(v) = p.get("exaggeration") {
        scene.exaggeration = v.as_f64().ok_or("exaggeration is a number")?;
    }
    if let Some(v) = p.get("groundOpacity") {
        scene.ground_opacity = v.as_f64().ok_or("groundOpacity is a number, 0 to 1")?;
    }
    if let Some(v) = p.get("depth") {
        scene.depth = if v.is_null() { None } else { Some(v.as_f64().ok_or("depth is metres, or null")?) };
    }
    if let Some(v) = p.get("walls") {
        scene.walls = v.as_str().and_then(Rgba::from_hex).filter(|c| c.a() == 255).ok_or("walls is an opaque colour, #RRGGBB")?;
    }
    if let Some(v) = p.get("layers") {
        let a = v.as_array().ok_or("layers is a list of layers as scene.layer takes them")?;
        if a.len() > MAX_LAYERS {
            return Err(format!("a scene stands up at most {MAX_LAYERS} layers"));
        }
        scene.layers = a.iter().map(|l| scene_layer(map, l, None)).collect::<Result<_, _>>()?;
    }
    if let Some(c) = p.get("camera") {
        move_camera(&mut scene.camera, c)?;
    }
    scene.check()?;
    s.map_mut().scene = Some(scene.clone());
    Ok(described(&scene))
}

/// What a scene holds, as the commands answer.
fn described(scene: &Scene) -> Value {
    let (w, h) = scene.ground_size();
    let mut v = serde_json::to_value(scene).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("groundSize".into(), json!([w.round(), h.round()]));
    }
    v
}

/// A scene layer from its parameters, over `was` (the scene's entry for the layer, when it has
/// one), its layer and fields checked against the map's.
fn scene_layer(map: &Map, p: &Value, was: Option<&SceneLayer>) -> Result<SceneLayer, String> {
    let o = p.as_object().ok_or("a scene layer is an object")?;
    const KEYS: &[&str] =
        &["layer", "kind", "depth", "top", "water", "elevation", "height", "heightM", "offset", "size", "color", "label", "remove"];
    if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("a scene layer has no '{k}' ({})", KEYS.join(", ")));
    }
    let name = ps(p, "layer").ok_or("layer is required")?;
    let l = map.layer_by_name(name).ok_or_else(|| format!("'{name}' isn't a layer of the map"))?;
    if l.raster.is_some() {
        return Err(format!("{name} is a raster: a scene stands up points and polygons (a DEM is its ground: dem)"));
    }
    let gt = l.dataset.geometry_type;
    let kind = match ps(p, "kind") {
        Some(k) => SceneKind::from_code(k).ok_or_else(|| format!("unknown kind '{k}' (well, point, extrude)"))?,
        None => match was {
            Some(w) => w.kind,
            None if gt == rustgis_data::GeometryType::Polygon => SceneKind::Extrude,
            None => SceneKind::Well,
        },
    };
    let fits = match kind {
        SceneKind::Well | SceneKind::Point => gt == rustgis_data::GeometryType::Point,
        SceneKind::Extrude => gt == rustgis_data::GeometryType::Polygon,
    };
    if !fits {
        let wants = if kind == SceneKind::Extrude { "polygons" } else { "points" };
        return Err(format!("{name}: a {} layer is of {wants}", kind.code()));
    }
    let mut sl = was.cloned().filter(|w| w.kind == kind).unwrap_or_else(|| SceneLayer::new(&l.name, kind));
    sl.layer = l.name.clone();
    let field = |k: &str, slot: &mut Option<String>| -> Result<(), String> {
        match o.get(k) {
            None => Ok(()),
            Some(Value::Null) => {
                *slot = None;
                Ok(())
            }
            Some(Value::String(f)) if f.is_empty() => {
                *slot = None;
                Ok(())
            }
            Some(Value::String(f)) => {
                let i = l.dataset.field_index(f).ok_or_else(|| format!("{name} has no field '{f}'"))?;
                *slot = Some(l.dataset.fields[i].name.clone());
                Ok(())
            }
            Some(_) => Err(format!("{k} is a field's name")),
        }
    };
    field("depth", &mut sl.depth)?;
    field("top", &mut sl.top)?;
    field("water", &mut sl.water)?;
    field("elevation", &mut sl.elevation)?;
    field("height", &mut sl.height)?;
    field("label", &mut sl.label)?;
    if o.contains_key("heightM") {
        sl.height_m = f(p, "heightM").ok_or("heightM is metres")?;
    }
    if o.contains_key("offset") {
        sl.offset = f(p, "offset").ok_or("offset is metres")?;
    }
    match o.get("size") {
        None => {}
        Some(Value::Null) => sl.size = None,
        Some(_) => sl.size = Some(f(p, "size").ok_or("size is metres, or null")?),
    }
    match o.get("color") {
        None => {}
        Some(Value::Null) => sl.color = None,
        Some(v) => {
            sl.color =
                Some(v.as_str().and_then(Rgba::from_hex).filter(|c| c.a() == 255).ok_or("color is an opaque colour, #RRGGBB, or null")?)
        }
    }
    if kind == SceneKind::Well && sl.depth.is_none() {
        return Err(format!("{name}: a well layer needs its depth field (depth)"));
    }
    Ok(sl)
}

fn layer(s: &mut Session, p: &Value) -> CmdResult {
    let map = s.map();
    let mut scene = map.scene.clone().ok_or("the map has no 3D scene: make one first (scene.set)")?;
    let name = ps(p, "layer").ok_or("layer is required")?;
    let at = scene.layers.iter().position(|l| l.layer.eq_ignore_ascii_case(name));
    if b(p, "remove").unwrap_or(false) {
        let i = at.ok_or_else(|| format!("the scene doesn't stand up '{name}'"))?;
        scene.layers.remove(i);
    } else {
        let sl = scene_layer(map, p, at.map(|i| &scene.layers[i]))?;
        match at {
            Some(i) => scene.layers[i] = sl,
            None if scene.layers.len() >= MAX_LAYERS => return Err(format!("a scene stands up at most {MAX_LAYERS} layers")),
            None => scene.layers.push(sl),
        }
    }
    scene.check()?;
    s.map_mut().scene = Some(scene.clone());
    Ok(described(&scene))
}

/// Moves a scene's camera as `scene.camera`'s parameters say.
fn move_camera(c: &mut SceneCamera, p: &Value) -> Result<(), String> {
    let o = p.as_object().ok_or("camera is an object")?;
    const KEYS: &[&str] = &["heading", "turn", "tilt", "zoom", "target", "reset"];
    if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("the camera has no '{k}' ({})", KEYS.join(", ")));
    }
    if b(p, "reset").unwrap_or(false) {
        *c = SceneCamera::default();
    }
    let num = |k: &str| -> Result<Option<f64>, String> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(_) => f(p, k).map(Some).ok_or_else(|| format!("{k} is a number")),
        }
    };
    if let Some(v) = num("heading")? {
        c.heading = v;
    }
    if let Some(v) = num("turn")? {
        c.heading += v;
    }
    if let Some(v) = num("tilt")? {
        if !(0.0..=rustgis_map::scene::MAX_TILT).contains(&v) {
            return Err(format!("tilt is degrees, 0 to {}", rustgis_map::scene::MAX_TILT));
        }
        c.tilt = v;
    }
    if let Some(v) = num("zoom")? {
        let (lo, hi) = rustgis_map::scene::ZOOM_RANGE;
        if !(lo..=hi).contains(&v) {
            return Err(format!("zoom is {lo} to {hi}"));
        }
        c.zoom = v;
    }
    if let Some(t) = o.get("target") {
        let a = t.as_array().filter(|a| a.len() == 3).ok_or("target is [east, north, up] in metres")?;
        let n: Vec<f64> = a.iter().filter_map(Value::as_f64).filter(|v| v.is_finite()).collect();
        c.target = n.try_into().map_err(|_| "target is [east, north, up] in metres")?;
    }
    c.clamp();
    Ok(())
}

fn camera(s: &mut Session, p: &Value) -> CmdResult {
    let scene = s.map_mut().scene.as_mut().ok_or("the map has no 3D scene: make one first (scene.set)")?;
    move_camera(&mut scene.camera, p)?;
    Ok(serde_json::to_value(scene.camera).unwrap_or(Value::Null))
}

/// A whole number of pixels from `lo` to `hi`, `default` when not given.
fn pixels(p: &Value, k: &str, default: u32, (lo, hi): (u32, u32)) -> Result<u32, String> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(default),
        Some(_) => f(p, k)
            .filter(|v| v.fract() == 0.0 && (f64::from(lo)..=f64::from(hi)).contains(v))
            .map(|v| v as u32)
            .ok_or_else(|| format!("{k} is a whole number from {lo} to {hi}")),
    }
}

/// The path to write, ending in one of `exts`.
fn path_of(p: &Value, exts: &[&str]) -> Result<String, String> {
    let path = ps(p, "path").ok_or_else(|| format!("path is required ({})", exts.join(" or ")))?;
    let lower = path.to_ascii_lowercase();
    if !exts.iter().any(|e| lower.ends_with(e)) {
        return Err(format!("path must end in {}", exts.join(" or ")));
    }
    Ok(path.to_string())
}

/// Writes the scene from its camera as a PNG.
pub(crate) fn export_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("exporting a scene needs the desktop app".into());
    }
    let path = path_of(p, &[".png"])?;
    let (w, h) = (pixels(p, "width", 1600, (64, 8192))?, pixels(p, "height", 1000, (64, 8192))?);
    let smooth = pixels(p, "smooth", 2, (1, 3))?;
    if f64::from(w) * f64::from(h) * f64::from(smooth * smooth) > 1.2e8 {
        return Err("the picture is too large: lower its size or its smoothing".into());
    }
    let map = s.map().detached();
    let scene = map.scene.clone().ok_or("the map has no 3D scene: make one first (scene.set)")?;
    Ok(Box::new(move || {
        // Built on a copy that keeps what its streamed layers' sources said, for the notices.
        let mut used = map.clone();
        let (written, notes) = (|| {
            let built = rustgis_render::scene3d::build_on(&mut used, &scene, rustgis_render::scene3d::Detail::EXPORT)?;
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            let ppp = (f64::from(h) / 700.0).clamp(1.0, 4.0) as f32;
            let pm = rustgis_render::scene3d::draw(&built, &scene.camera, scene.ground_opacity, w, h, ppp, smooth)?;
            let png = rustgis_render::scene3d::png(&pm)?;
            rustgis_data::io::write_atomic(std::path::Path::new(&path), &png).map_err(|e| format!("{path}: {e}"))?;
            Ok((json!({"path": path, "width": w, "height": h, "bytes": png.len()}), built.notes))
        })()
        .map_or_else(|e| (Err(e), Vec::new()), |(v, n)| (Ok(v), n));
        let warn = rustgis_data::diagnostics::Severity::Warning;
        super::exports::drawn(std::slice::from_ref(&used), notes.into_iter().map(|n| (warn, n)).collect(), written, path.clone())
    }))
}

/// Turns the scene's camera about its point, a frame a step, as an animated GIF or a zip of PNGs.
pub(crate) fn orbit_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("exporting a scene needs the desktop app".into());
    }
    let path = path_of(p, &[".gif", ".zip"])?;
    let gif = path.to_ascii_lowercase().ends_with(".gif");
    let frames = pixels(p, "frames", 72, (2, 720))? as usize;
    let turn = match p.get("turn") {
        None | Some(Value::Null) => 360.0,
        Some(_) => f(p, "turn").filter(|t| *t != 0.0 && t.abs() <= 3600.0).ok_or("turn is degrees, not 0, at most 3600 either way")?,
    };
    let (w, h) = (pixels(p, "width", 800, (64, 2048))?, pixels(p, "height", 500, (64, 2048))?);
    let speed = pixels(p, "speed", 60, (16, 60_000))?;
    if frames as f64 * f64::from(w) * f64::from(h) > super::time::MAX_PIXELS {
        return Err("the animation is too large: lower its frames or its size".into());
    }
    let map = s.map().detached();
    let scene = map.scene.clone().ok_or("the map has no 3D scene: make one first (scene.set)")?;
    Ok(Box::new(move || {
        // Built on a copy that keeps what its streamed layers' sources said, for the notices.
        let mut used = map.clone();
        let (written, notes) = (|| {
            let built = rustgis_render::scene3d::build_on(&mut used, &scene, rustgis_render::scene3d::Detail::EXPORT)?;
            // A whole turn stops a step short of where it began, so it loops with no frame twice.
            let steps = if turn.rem_euclid(360.0) == 0.0 { frames } else { frames - 1 };
            let bytes = super::time::animate(frames, gif.then_some(speed), |i| {
                let mut cam = scene.camera;
                cam.heading += turn * i as f64 / steps as f64;
                cam.clamp();
                let pm = rustgis_render::scene3d::draw(&built, &cam, scene.ground_opacity, w, h, 1.0, 2)?;
                rustgis_render::scene3d::png(&pm)
            })?;
            rustgis_data::io::write_atomic(std::path::Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok((json!({"path": path, "frames": frames, "turn": turn, "bytes": bytes.len()}), built.notes))
        })()
        .map_or_else(|e| (Err(e), Vec::new()), |(v, n)| (Ok(v), n));
        let warn = rustgis_data::diagnostics::Severity::Warning;
        super::exports::drawn(std::slice::from_ref(&used), notes.into_iter().map(|n| (warn, n)).collect(), written, path.clone())
    }))
}
