//! The 3D Scene view (ARutGIS): a map's scene (`scene.*` commands) drawn by
//! `rustgis_render::scene3d` in place of the map, with its settings above it, the layers it
//! stands up beside it, and the mouse to turn it: drag to orbit, right-drag to move along the
//! ground, Ctrl+drag to go up or down (under the ground too), the wheel to come nearer.
//!
//! The scene is built (the heights read, the map draped, the features stood up) off the frame
//! natively, again whenever the scene or the map changes; turning it only draws it again, also
//! off the frame natively, the last picture shown until the next is ready.

use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::map::scene::{Scene, SceneCamera, SceneKind};
use rustgis_engine::render::scene3d::{self, Built, Detail, Look};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, regular, semibold};

/// What the view keeps between frames.
#[derive(Default)]
pub struct SceneView {
    /// The view shows in place of the map.
    pub shown: bool,
    /// The scene as last built, and what it was built from ([`Key`]).
    built: Option<(Key, Arc<Built>)>,
    /// A build running off the frame, and what it builds.
    #[cfg(not(target_arch = "wasm32"))]
    building: Option<Job<Key, Result<Built, String>>>,
    /// Why the last build failed, and what it was built from.
    error: Option<String>,
    failed: Option<Key>,
    /// The picture last drawn, and what it was drawn from.
    texture: Option<(Drawn, egui::TextureHandle)>,
    /// A picture being drawn off the frame, and what it draws.
    #[cfg(not(target_arch = "wasm32"))]
    drawing: Option<Job<Drawn, Option<(egui::ColorImage, f32)>>>,
    /// A picture that couldn't be drawn: not tried again until what it draws changes.
    undrawn: Option<Drawn>,
    /// The camera while the mouse moves it, kept to the map's scene when the drag ends.
    live: Option<SceneCamera>,
    /// The last picture's drawing time (seconds): a slow one draws coarser while turning.
    last_draw: f32,
    /// What the commands said last (and whether it is an error).
    message: Option<(String, bool)>,
    /// What the last build left out, each said in the notices once.
    said: Vec<String>,
    /// What is under the mouse, and the picture and point it was looked for on.
    hover: Option<(Drawn, [u32; 2], Option<String>)>,
    /// What is under the mouse being looked for off the frame, and on which picture and point.
    #[cfg(not(target_arch = "wasm32"))]
    picking: Option<Job<(Drawn, [u32; 2]), Result<Option<scene3d::Picked>, String>>>,
    /// The project the view was for: another one drops what it kept.
    opened: u64,
}

/// Work running on a thread of its own: what it is for, where its answer lands, and its Cancel,
/// pressed when the job is dropped (left behind by another, or the view closed).
#[cfg(not(target_arch = "wasm32"))]
struct Job<K, T> {
    what: K,
    slot: crate::Slot<T>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<K, T> Drop for Job<K, T> {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A job's Cancel, as the scene's building and drawing look at it.
#[cfg(not(target_arch = "wasm32"))]
struct Stop(Arc<std::sync::atomic::AtomicBool>);

#[cfg(not(target_arch = "wasm32"))]
impl rustgis_engine::data::progress::Probe for Stop {
    fn cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn begin(&self, _: u64) {}
    fn advance(&self, _: u64) {}
}

#[cfg(not(target_arch = "wasm32"))]
impl<K, T: Send + 'static> Job<K, T> {
    /// Runs `work` on a thread with its Cancel installed, the frame asked again when it lands.
    fn spawn(what: K, ctx: &egui::Context, work: impl FnOnce() -> T + Send + 'static) -> Job<K, T> {
        let (slot, stop): (crate::Slot<T>, _) = (Default::default(), Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let (out, probe, repaint) = (slot.clone(), Stop(stop.clone()), ctx.clone());
        std::thread::spawn(move || {
            let _probe = rustgis_engine::data::progress::install(Some(Arc::new(probe)));
            let r = work();
            if let Ok(mut o) = out.lock() {
                *o = Some(r);
            }
            repaint.request_repaint();
        });
        Job { what, slot, stop }
    }

    /// Its answer, once it has landed.
    fn landed(&self) -> Option<T> {
        self.slot.lock().ok()?.take()
    }
}

/// Stops the scene's building and drawing: the view isn't shown.
pub fn idle(app: &mut RustGisApp) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.scene3d.building = None;
        app.scene3d.drawing = None;
        app.scene3d.picking = None;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = app;
}

/// What a build is made from: the scene without its camera and opacity (which only change the
/// drawing), and the project's revision then.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    scene: String,
    revision: u64,
    map: u64,
}

/// What a picture was drawn from.
#[derive(Clone, Debug, PartialEq)]
struct Drawn {
    built: usize,
    camera: [u64; 6],
    opacity: u64,
    ruler: bool,
    ruler_step: Option<u64>,
    ruler_range: [Option<u64>; 3],
    grid: Option<[Option<u64>; 2]>,
    size: [u32; 2],
}

impl Key {
    fn of(app: &RustGisApp, scene: &Scene) -> Key {
        let mut s = scene.clone();
        s.camera = SceneCamera::default();
        s.ground_opacity = 1.0;
        s.ruler = true;
        (s.ruler_step, s.ruler_min, s.ruler_max, s.ruler_from) = (None, None, None, None);
        (s.grid, s.grid_step, s.grid_z_step) = (false, None, None);
        Key { scene: serde_json::to_string(&s).unwrap_or_default(), revision: app.session.revision, map: app.session.map().instance_id() }
    }
}

/// Runs a command for the view, keeping what it says to show.
fn run(app: &mut RustGisApp, id: &str, p: Value) -> Option<Value> {
    match app.run(id, p) {
        Ok(v) => {
            app.scene3d.message = None;
            Some(v)
        }
        Err(e) => {
            app.scene3d.message = Some((e, true));
            None
        }
    }
}

/// Runs a command that changes only how the scene is drawn (its opacity, its camera): the built
/// scene stays, though the project's revision moves on.
fn run_drawing_only(app: &mut RustGisApp, id: &str, p: Value) {
    let before = app.session.revision;
    if run(app, id, p).is_some()
        && let Some((key, _)) = app.scene3d.built.as_mut()
        && key.revision == before
    {
        key.revision = app.session.revision;
    }
}

/// Opens the view (`ui.scene`), first making the map's scene when it has none: over the selected
/// raster layer (or the map's first) as its ground and that raster's extent, or the view.
pub fn open(app: &mut RustGisApp, p: &Value) -> Result<Value, String> {
    if p.get("show").and_then(Value::as_bool) == Some(false) {
        app.scene3d.shown = false;
        return Ok(Value::Null);
    }
    if app.session.map().scene.is_none() {
        let map = app.session.map();
        let selected = app.session.active_layer.and_then(|id| map.layer(id)).filter(|l| l.raster.is_some());
        let dem =
            selected.or_else(|| map.layers.iter().find(|l| l.raster.as_ref().is_some_and(|r| r.data.is_some()))).map(|l| l.name.clone());
        let args = match dem {
            Some(d) => json!({"dem": d, "extent": "dem"}),
            None => json!({"extent": "view"}),
        };
        let made = app.run("scene.set", args.clone());
        // A DEM too large for a scene (a whole country): the view instead.
        if made.is_err() && args.get("dem").is_some() {
            app.run("scene.set", json!({"dem": args["dem"], "extent": "view"}))?;
        } else {
            made?;
        }
    }
    app.mb.active = false;
    let _ = app.run("layout.close", Value::Null);
    app.scene3d.shown = true;
    Ok(Value::Null)
}

/// Whether the active map's scene shows.
pub fn shown(app: &RustGisApp) -> bool {
    app.scene3d.shown && app.session.map().scene.is_some() && !app.mb.shown() && app.session.active_layout.is_none()
}

/// Draws the view.
pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    if app.scene3d.opened != app.session.opened {
        app.scene3d = SceneView { shown: app.scene3d.shown, opened: app.session.opened, ..SceneView::default() };
    }
    let Some(scene) = app.session.map().scene.clone() else {
        app.scene3d.shown = false;
        return;
    };
    keep_built(app, &scene, ui.ctx());
    toolbar(app, ui, &scene, &t);
    let full = ui.available_rect_before_wrap();
    let side_w = 290.0f32.min(full.width() * 0.4);
    let canvas = Rect::from_min_max(full.min, pos2(full.max.x - side_w, full.max.y));
    let side = Rect::from_min_max(pos2(canvas.max.x, full.min.y), full.max);
    picture(app, ui, canvas, &scene, &t);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(side.shrink2(vec2(10.0, 8.0))));
    ui.painter().rect_filled(side, 0.0, t.pane);
    ui.painter().line_segment([side.left_top(), side.left_bottom()], Stroke::new(1.0, t.border));
    egui::ScrollArea::vertical().id_salt("scene3d_layers").show(&mut child, |ui| layers_pane(app, ui, &scene, &t));
    ui.allocate_rect(full, Sense::hover());
}

/// Builds the scene again when it or the map changed, off the frame natively; lands a finished
/// build. A build that failed isn't tried again until what it is built from changes.
fn keep_built(app: &mut RustGisApp, scene: &Scene, ctx: &egui::Context) {
    let key = Key::of(app, scene);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let landed = app.scene3d.building.as_ref().and_then(|j| j.landed().map(|r| (j.what.clone(), r)));
        if let Some((k, r)) = landed {
            app.scene3d.building = None;
            land(app, k, r);
        }
        // One for what the scene was is cancelled: the scene changed meanwhile.
        if app.scene3d.building.as_ref().is_some_and(|j| j.what != key) {
            app.scene3d.building = None;
        }
        let current = app.scene3d.built.as_ref().is_some_and(|(k, _)| *k == key);
        if !current && app.scene3d.failed.as_ref() != Some(&key) && app.scene3d.building.is_none() {
            let map = app.session.map().detached();
            let s = scene.clone();
            app.scene3d.building = Some(Job::spawn(key, ctx, move || scene3d::build(map, &s, Detail::SCREEN)));
        }
        if app.scene3d.building.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = ctx;
        let current = app.scene3d.built.as_ref().is_some_and(|(k, _)| *k == key);
        if !current && app.scene3d.failed.as_ref() != Some(&key) {
            // No threads in the browser: built on the frame, a little coarser.
            let r = scene3d::build(app.session.map().detached(), scene, Detail { drape_px: 1536, cells: 160 });
            land(app, key, r);
        }
    }
}

/// A finished build, kept; or why it failed.
fn land(app: &mut RustGisApp, key: Key, r: Result<Built, String>) {
    match r {
        Ok(b) => {
            say_notes(app, &b);
            app.scene3d.built = Some((key, Arc::new(b)));
            app.scene3d.error = None;
            app.scene3d.failed = None;
        }
        Err(e) => {
            app.scene3d.error = Some(e);
            app.scene3d.failed = Some(key);
        }
    }
}

/// Says once what a build left out: above the picture the first (with a count of the rest), and
/// each in the notices, a note the build before said too not again.
fn say_notes(app: &mut RustGisApp, b: &Built) {
    for n in b.notes.iter().filter(|n| !app.scene3d.said.contains(n)) {
        app.session.warn(format!("3D Scene: {n}"));
    }
    app.scene3d.said.clone_from(&b.notes);
    if let Some(first) = b.notes.first() {
        let more = b.notes.len() - 1;
        let text = if more == 0 { first.clone() } else { format!("{first} (+{more})") };
        app.scene3d.message = Some((text, false));
    }
}

/// The settings above the picture.
/// In two rows: the ground and the block, then how it is shown and what can be done with it.
fn toolbar(app: &mut RustGisApp, ui: &mut Ui, scene: &Scene, t: &Tokens) {
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 76.0), Sense::hover());
    ui.painter().rect_filled(bar, 0.0, t.ribbon);
    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.border));
    let inner = bar.shrink2(vec2(10.0, 4.0));
    let (top, bottom) = inner.split_top_bottom_at_fraction(0.5);
    for (rect, first) in [(top, true), (bottom, false)] {
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        row.set_clip_rect(rect.intersect(ui.clip_rect()));
        row.spacing_mut().item_spacing.x = 10.0;
        if first { ground_row(app, &mut row, scene) } else { shown_row(app, &mut row, scene) }
    }
}

/// The toolbar's first row: the ground, its stretch, the block, and the study area.
fn ground_row(app: &mut RustGisApp, ui: &mut Ui, scene: &Scene) {
    // The ground: a raster layer of heights, or flat.
    ui.label(tl!("Ground"));
    let rasters: Vec<String> = app.session.map().layers.iter().filter(|l| l.raster.is_some()).map(|l| l.name.clone()).collect();
    let shown = scene.dem.clone().unwrap_or_else(|| tl!("Flat (no DEM)").to_string());
    let mut pick: Option<Value> = None;
    egui::ComboBox::from_id_salt("scene3d_dem").selected_text(shown).width(170.0).show_ui(ui, |ui| {
        if ui.selectable_label(scene.dem.is_none(), tl!("Flat (no DEM)")).clicked() {
            pick = Some(Value::Null);
        }
        for r in &rasters {
            if ui.selectable_label(scene.dem.as_deref() == Some(r.as_str()), r).clicked() {
                pick = Some(json!(r));
            }
        }
    });
    if let Some(d) = pick {
        run(app, "scene.set", json!({"dem": d}));
    }
    ui.separator();
    ui.label(tl!("Vertical exaggeration"));
    let mut ex = scene.exaggeration;
    let r = ui.add(egui::DragValue::new(&mut ex).range(0.1..=50.0).speed(0.05).max_decimals(2).suffix("×")).on_hover_text(tl!(
        "Stretches the relief over the lowest ground (the DEM); below it the block and the wells are stretched by the depth exaggeration"
    ));
    if r.changed() && ex != scene.exaggeration {
        run(app, "scene.set", json!({"exaggeration": ex}));
    }
    // The block below the lowest ground, in true metres: the scene's, or the default the last
    // build took.
    ui.label(tl!("Block depth"));
    let built = app.scene3d.built.as_ref().map(|(_, b)| b.depth);
    let mut depth = scene.depth.or(built).unwrap_or(0.0);
    let r = ui
        .add(egui::DragValue::new(&mut depth).range(0.0..=rustgis_engine::map::scene::MAX_DEPTH).speed(5.0).max_decimals(0).suffix(" m"))
        .on_hover_text(tl!("True metres from the lowest ground, or the deepest well, down to the block's base"));
    if r.changed() && Some(depth) != scene.depth {
        run(app, "scene.set", json!({"depth": depth}));
    }
    if scene.depth.is_some()
        && ui.small_button(tl!("Auto")).on_hover_text(tl!("A quarter of the ground's relief as drawn, at least 50 m")).clicked()
    {
        run(app, "scene.set", json!({"depth": null}));
    }
    // What is below the lowest ground, stretched on its own; its heights still read true.
    ui.label(tl!("Depth exaggeration"));
    let mut dex = scene.depth_exaggeration;
    let r = ui
        .add(
            egui::DragValue::new(&mut dex)
                .range(0.1..=rustgis_engine::map::scene::MAX_EXAGGERATION)
                .speed(0.05)
                .max_decimals(2)
                .suffix("×"),
        )
        .on_hover_text(tl!(
            "Stretches the block, and the wells below the lowest ground, on its own; the ruler and the readout stay in true metres"
        ));
    if r.changed() && dex != scene.depth_exaggeration {
        run(app, "scene.set", json!({"depthExaggeration": dex}));
    }
    // The block's sides and base.
    ui.label(tl!("Block colour"));
    let w = scene.walls;
    let mut c = Color32::from_rgb(w.r(), w.g(), w.b());
    if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque)
        .on_hover_text(tl!("The colour of the block's sides and base"))
        .changed()
    {
        run(app, "scene.set", json!({"walls": format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())}));
    }
    if w != rustgis_engine::map::scene::WALLS
        && ui.small_button(tl!("Default")).on_hover_text(tl!("The soil colour of a block diagram")).clicked()
    {
        let d = rustgis_engine::map::scene::WALLS;
        run(app, "scene.set", json!({"walls": format!("#{:02X}{:02X}{:02X}", d.r(), d.g(), d.b())}));
    }
    ui.separator();
    ui.menu_button(tl!("Study Area"), |ui| {
        if let Some(d) = scene.dem.clone()
            && ui.button(tl!("The DEM's Extent")).clicked()
        {
            run(app, "scene.set", json!({"extent": "dem", "dem": d}));
            ui.close();
        }
        if ui.button(tl!("The Map's Current View")).clicked() {
            run(app, "scene.set", json!({"extent": "view"}));
            ui.close();
        }
        let names: Vec<String> = app.session.map().layers.iter().map(|l| l.name.clone()).collect();
        ui.menu_button(tl!("A Layer's Extent"), |ui| {
            for n in names {
                if ui.button(&n).clicked() {
                    run(app, "scene.set", json!({"extentOf": n}));
                    ui.close();
                }
            }
        });
    });
}

/// The toolbar's second row: how the scene is shown, and what can be done with it.
fn shown_row(app: &mut RustGisApp, ui: &mut Ui, scene: &Scene) {
    ui.label(tl!("Ground opacity"));
    let mut op = scene.ground_opacity * 100.0;
    let r = ui.add(egui::Slider::new(&mut op, 0.0..=100.0).suffix("%").integer());
    if r.changed() {
        run_drawing_only(app, "scene.set", json!({"groundOpacity": (op / 100.0).clamp(0.0, 1.0)}));
    }
    let mut ruler = scene.ruler;
    if ui
        .checkbox(&mut ruler, tl!("Height Ruler"))
        .on_hover_text(tl!("Heights in metres above sea level up the block's nearest corner"))
        .changed()
    {
        run_drawing_only(app, "scene.set", json!({"ruler": ruler}));
    }
    // Its marks and the heights it stands between, in a menu beside it.
    ui.add_enabled_ui(scene.ruler, |ui| ruler_settings(app, ui, scene));
    let mut grid = scene.grid;
    if ui
        .checkbox(&mut grid, tl!("3D Grid"))
        .on_hover_text(tl!("Eastings and northings (UTM metres) and heights on the box's walls, the block's sides and the ground"))
        .changed()
    {
        run_drawing_only(app, "scene.set", json!({"grid": grid}));
    }
    ui.add_enabled_ui(scene.grid, |ui| grid_settings(app, ui, scene));
    ui.separator();
    if ui.button(tl!("Reset View")).clicked() {
        app.scene3d.live = None;
        run_drawing_only(app, "scene.camera", json!({"reset": true}));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if ui.button(tl!("Export Image…")).clicked()
            && let Err(e) = app.run("ui.sceneExport", Value::Null)
        {
            app.scene3d.message = Some((e, true));
        }
        if ui.button(tl!("Export Orbit…")).clicked()
            && let Err(e) = app.run("ui.sceneOrbit", Value::Null)
        {
            app.scene3d.message = Some((e, true));
        }
    }
    if ui.button(tl!("Remove Scene")).on_hover_text(tl!("Take the 3D scene off the map")).clicked() {
        run(app, "scene.remove", Value::Null);
        app.scene3d.shown = false;
    }
}

/// The ruler's marks every so many true metres, the heights it stands between and the one its
/// marks are counted from: each typed in metres, or left empty for its automatic one (a round step
/// that fits, the block's base, the ground at its corner, 0 m), shown greyed in the box. Marked ●
/// when any is its own.
fn ruler_settings(app: &mut RustGisApp, ui: &mut Ui, scene: &Scene) {
    let asked = scene.ruler_step.is_some() || scene.ruler_min.is_some() || scene.ruler_max.is_some() || scene.ruler_from.is_some();
    let label = if asked { format!("{} ●", tl!("Ruler Settings")) } else { tl!("Ruler Settings").to_string() };
    // What the automatic foot is now: the block's base (its top, the ground at the corner it
    // stands up, moves with the eye).
    let base = app.scene3d.built.as_ref().map(|(_, b)| b.height_of(b.z_range.0));
    let metres =
        |v: Option<f64>| v.map(|v| format!("{} {}", tl!("Auto:"), plain_metres(v.round()))).unwrap_or_else(|| tl!("Auto").to_string());
    ui.menu_button(label, |ui| {
        let rows: [(&str, &str, &str, Option<f64>, String); 4] = [
            (
                "rulerStep",
                tl!("Ruler step"),
                tl!("True metres between the ruler's marks; where numbers don't fit, only every few are numbered. Empty: a round step that fits the ruler"),
                scene.ruler_step,
                tl!("Auto").to_string(),
            ),
            (
                "rulerMin",
                tl!("Ruler bottom"),
                tl!("The height its foot stands at, metres above sea level. Empty: the block's base"),
                scene.ruler_min,
                metres(base),
            ),
            (
                "rulerMax",
                tl!("Ruler top"),
                tl!("The height of its top, metres above sea level. Empty: the ground at its corner"),
                scene.ruler_max,
                tl!("Auto").to_string(),
            ),
            (
                "rulerFrom",
                tl!("Marks counted from"),
                tl!("A mark at this height and every step up and down from it. Empty: 0 m"),
                scene.ruler_from,
                format!("{} 0", tl!("Auto:")),
            ),
        ];
        metres_settings(app, ui, "scene3d_ruler", &rows, asked);
    });
}

/// The grid's steps: metres between its eastings and northings, and between its heights, each
/// typed or left empty for a round one that fits (the heights' then the ruler's step, when it
/// asks one).
fn grid_settings(app: &mut RustGisApp, ui: &mut Ui, scene: &Scene) {
    let asked = scene.grid_step.is_some() || scene.grid_z_step.is_some();
    let label = if asked { format!("{} ●", tl!("Grid Settings")) } else { tl!("Grid Settings").to_string() };
    ui.menu_button(label, |ui| {
        let z_auto = scene.ruler_step.map_or_else(|| tl!("Auto").to_string(), |st| format!("{} {}", tl!("Auto:"), plain_metres(st)));
        let rows: [(&str, &str, &str, Option<f64>, String); 2] = [
            (
                "gridStep",
                tl!("E/N step"),
                tl!("Metres between the grid's eastings and northings (UTM). Empty: a round step that fits the scene"),
                scene.grid_step,
                tl!("Auto").to_string(),
            ),
            (
                "gridZStep",
                tl!("Height step"),
                tl!("True metres between the grid's heights. Empty: the ruler's step when it has one, else a round step that fits"),
                scene.grid_z_step,
                z_auto,
            ),
        ];
        metres_settings(app, ui, "scene3d_grid", &rows, asked);
    });
}

/// A menu's rows of settings in metres (`scene.set` key, name, tip, value, what empty means),
/// each typed in a box and set when Enter is pressed or the box is left: empty sets it back to
/// automatic, and a number that isn't one is said under them. With `asked`, a button sets them all
/// back.
fn metres_settings(app: &mut RustGisApp, ui: &mut Ui, id: &str, rows: &[(&str, &str, &str, Option<f64>, String)], asked: bool) {
    let mut set: Option<Value> = None;
    let wrong = egui::Id::unique((id, "wrong"));
    egui::Grid::new(id).num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
        for (key, name, tip, now, auto) in rows {
            ui.label(*name).on_hover_text(*tip);
            ui.horizontal(|ui| {
                match metres_field(ui, egui::Id::unique((id, *key)), *now, auto, name) {
                    Some(Ok(v)) => {
                        ui.data_mut(|d| {
                            d.remove::<String>(wrong);
                        });
                        set = Some(json!({ *key: v }));
                    }
                    Some(Err(e)) => {
                        ui.data_mut(|d| d.insert_temp(wrong, e));
                    }
                    None => {}
                }
                ui.label("m");
            })
            .response
            .on_hover_text(*tip);
            if now.is_some() && ui.small_button(tl!("Auto")).on_hover_text(tl!("Empty the box: its automatic value")).clicked() {
                set = Some(json!({ *key: null }));
            }
            ui.end_row();
        }
    });
    if let Some(e) = ui.data(|d| d.get_temp::<String>(wrong)) {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    if asked && ui.button(tl!("All Automatic")).clicked() {
        let all: serde_json::Map<String, Value> = rows.iter().map(|r| (r.0.to_string(), Value::Null)).collect();
        set = Some(Value::Object(all));
    }
    if let Some(p) = set {
        run_drawing_only(app, "scene.set", p);
    }
}

/// A box for a number of metres: `now` written plainly while it isn't being typed in, empty for
/// automatic, which `auto` says greyed. Read when Enter is pressed or the box is left: `Some(Ok)`
/// with the number (None: empty, automatic) when it changed, `Some(Err)` with why when it isn't a
/// number. Commas between thousands, spaces and a trailing "m" are let through.
fn metres_field(ui: &mut Ui, id: egui::Id, now: Option<f64>, auto: &str, name: &str) -> Option<Result<Option<f64>, String>> {
    let shown = now.map(plain_metres).unwrap_or_default();
    let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| shown.clone());
    let r = ui.add(egui::TextEdit::singleline(&mut text).id(id).hint_text(auto).desired_width(110.0));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, name));
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
        return None;
    }
    // Left (Enter, Tab or a click elsewhere): read, then shown as it is set.
    let typed = ui.data_mut(|d| d.remove_temp::<String>(id));
    let typed = if r.lost_focus() { Some(text) } else { typed };
    let typed = typed?;
    match read_metres(&typed) {
        Ok(v) if v.map(f64::to_bits) == now.map(f64::to_bits) => None,
        Ok(v) => Some(Ok(v)),
        Err(()) => Some(Err(format!("{}: {}", name, tl!("type a number of metres, or leave it empty for Auto")))),
    }
}

/// A number of metres as typed: empty (or Auto) for none; commas, spaces and a unit after it
/// ("m", "ม.", "เมตร") let through.
fn read_metres(text: &str) -> Result<Option<f64>, ()> {
    let t = text.trim();
    let t = ["เมตร", "ม.", "ม", "m"].iter().find_map(|u| t.strip_suffix(u)).unwrap_or(t).trim();
    if t.is_empty() || t.eq_ignore_ascii_case("auto") || t == tl!("Auto") {
        return Ok(None);
    }
    let digits: String = t.chars().filter(|c| !matches!(c, ',' | ' ' | '\u{a0}' | '\u{202f}' | '_')).collect();
    let digits = digits.replace('\u{2212}', "-");
    digits.parse::<f64>().ok().filter(|v| v.is_finite()).map(Some).ok_or(())
}

/// `v` metres written as typed back: no thousands apart, no needless decimals.
fn plain_metres(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".to_string() } else { s.to_string() }
}

/// The picture, turned by the mouse.
fn picture(app: &mut RustGisApp, ui: &mut Ui, rect: Rect, scene: &Scene, t: &Tokens) {
    let resp = ui.interact(rect, egui::Id::unique("scene3d_canvas"), Sense::click_and_drag());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, tl!("3D Scene")));
    let built = app.scene3d.built.as_ref().map(|(_, b)| b.clone());
    let mut cam = app.scene3d.live.unwrap_or(scene.camera);
    // The mouse.
    let mut moved = false;
    if let Some(b) = &built {
        let d = resp.drag_delta();
        let ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
        if resp.dragged_by(egui::PointerButton::Primary) && !ctrl && d != egui::Vec2::ZERO {
            cam.heading -= f64::from(d.x) * 0.3;
            cam.tilt = (cam.tilt - f64::from(d.y) * 0.3).clamp(0.0, rustgis_engine::map::scene::MAX_TILT);
            moved = true;
        } else if (resp.dragged_by(egui::PointerButton::Secondary)) && d != egui::Vec2::ZERO {
            scene3d::pan(b, &mut cam, f64::from(d.x), f64::from(d.y), f64::from(rect.height()));
            moved = true;
        } else if (resp.dragged_by(egui::PointerButton::Middle) || (resp.dragged_by(egui::PointerButton::Primary) && ctrl))
            && d != egui::Vec2::ZERO
        {
            // Up and down, as far as the camera's distance per pixel.
            let per_px = cam.zoom * b_diag(b, scene) / f64::from(rect.height()) * 1.2;
            cam.target[2] -= f64::from(d.y) * per_px;
            moved = true;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + (i.zoom_delta() - 1.0) * 400.0);
            if scroll != 0.0 {
                cam.zoom *= (-f64::from(scroll) * 0.002).exp();
                moved = true;
            }
        }
    }
    if moved {
        cam.clamp();
        app.scene3d.live = Some(cam);
    }
    let dragging = resp.dragged();
    if !dragging
        && let Some(c) = app.scene3d.live.take()
        && let Some(s) = app.session.map_mut().scene.as_mut()
    {
        // Kept with the map's scene, as the map's own camera is: no step to undo.
        s.camera = c;
    }
    // The picture: coarser while it turns, when drawing it is slow.
    let ppp = ui.ctx().pixels_per_point();
    let coarse = (dragging || moved) && app.scene3d.last_draw > 0.03;
    let scale = if coarse { 0.5 } else { 1.0 };
    let (w, h) = (((rect.width() * ppp * scale).round() as u32).max(1), ((rect.height() * ppp * scale).round() as u32).max(1));
    ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(0xB8, 0xCF, 0xE8));
    if let Some(b) = &built {
        let drawn = Drawn {
            built: Arc::as_ptr(b) as usize,
            camera: [cam.heading, cam.tilt, cam.zoom, cam.target[0], cam.target[1], cam.target[2]].map(f64::to_bits),
            opacity: scene.ground_opacity.to_bits(),
            ruler: scene.ruler,
            ruler_step: scene.ruler_step.map(f64::to_bits),
            ruler_range: [scene.ruler_min, scene.ruler_max, scene.ruler_from].map(|v| v.map(f64::to_bits)),
            grid: scene.grid.then(|| [scene.grid_step, scene.grid_z_step].map(|v| v.map(f64::to_bits))),
            size: [w, h],
        };
        // What is under the mouse, looked for again only when it or the picture moves.
        let at = resp.hover_pos().filter(|_| !dragging && !moved).map(|p| p - rect.min);
        match at {
            Some(p) => {
                let point = [p.x.round() as u32, p.y.round() as u32];
                let at = Pointed { drawn: drawn.clone(), point, cam, opacity: scene.ground_opacity, size: rect.size(), pos: p };
                keep_picked(app, ui.ctx(), b, at);
            }
            None => {
                app.scene3d.hover = None;
                #[cfg(not(target_arch = "wasm32"))]
                {
                    app.scene3d.picking = None;
                }
            }
        }
        keep_drawn(app, ui.ctx(), b, drawn, &cam, Look::of(scene), ppp * scale);
    }
    if let Some((_, tex)) = &app.scene3d.texture {
        ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    compass(ui, rect, cam.heading, t);
    // What is going on, and how to turn it.
    let building = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            app.scene3d.building.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    };
    let mut y = rect.min.y + 12.0;
    if building {
        let r = Rect::from_min_size(pos2(rect.min.x + 12.0, y), vec2(150.0, 24.0));
        ui.painter().rect_filled(r, 5.0, t.pane.gamma_multiply(0.92));
        ui.painter().text(r.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, tl!("Building the scene…"), regular(12.0), t.text);
        y += 30.0;
    }
    let said = app.scene3d.error.clone().map(|e| (e, true)).or_else(|| app.scene3d.message.clone());
    if let Some((m, err)) = said {
        let color = if err { Color32::from_rgb(0xB0, 0x20, 0x20) } else { t.text };
        let galley = ui.painter().layout(m, regular(12.0), color, (rect.width() - 40.0).max(100.0));
        let r = Rect::from_min_size(pos2(rect.min.x + 12.0, y), galley.size() + vec2(16.0, 10.0));
        ui.painter().rect_filled(r, 5.0, t.pane.gamma_multiply(0.92));
        ui.painter().galley(r.min + vec2(8.0, 5.0), galley, color);
    }
    // Under the mouse, where and how deep; otherwise how to turn it.
    let hovered = app.scene3d.hover.as_ref().and_then(|(_, _, s)| s.clone());
    let help = hovered.unwrap_or_else(|| tl!("Drag: orbit · Right-drag: move · Ctrl+drag: up or down · Wheel: zoom").to_string());
    let g = ui.painter().layout(help, regular(11.5), t.text, (rect.width() - 40.0).max(100.0));
    let r = Rect::from_min_size(pos2(rect.min.x + 10.0, rect.max.y - g.size().y - 18.0), g.size() + vec2(14.0, 8.0));
    ui.painter().rect_filled(r, 5.0, t.pane.gamma_multiply(0.85));
    ui.painter().galley(r.min + vec2(7.0, 4.0), g, t.text);
}

/// Where the mouse is over the picture, and the picture it is over.
struct Pointed {
    drawn: Drawn,
    point: [u32; 2],
    cam: SceneCamera,
    opacity: f64,
    size: egui::Vec2,
    pos: egui::Vec2,
}

/// Looks for what is under the mouse when it or the picture moved: natively off the frame (a
/// scene of many features has many triangles to look through), one for a point left behind
/// cancelled, what was said before shown meanwhile; in the browser on the frame.
fn keep_picked(app: &mut RustGisApp, ctx: &egui::Context, b: &Arc<Built>, at: Pointed) {
    let looked = |b: &Built, at: &Pointed| {
        let f = f64::from;
        scene3d::pick(b, &at.cam, at.opacity, f(at.size.x), f(at.size.y), f(at.pos.x), f(at.pos.y))
    };
    let have = |app: &RustGisApp| app.scene3d.hover.as_ref().is_some_and(|(d, q, _)| *d == at.drawn && *q == at.point);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let landed = app.scene3d.picking.as_ref().and_then(|j| j.landed().map(|r| (j.what.clone(), r)));
        if let Some(((d, q), r)) = landed {
            app.scene3d.picking = None;
            if let Ok(p) = r {
                app.scene3d.hover = Some((d, q, p.as_ref().map(said_of)));
            }
        }
        let want = (at.drawn.clone(), at.point);
        if !have(app) && app.scene3d.picking.as_ref().is_none_or(|j| j.what != want) {
            let b = b.clone();
            app.scene3d.picking = Some(Job::spawn(want, ctx, move || looked(&b, &at)));
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = ctx;
        if !have(app) {
            let p = looked(b, &at).ok().flatten();
            app.scene3d.hover = Some((at.drawn.clone(), at.point, p.as_ref().map(said_of)));
        }
    }
}

/// A point of the scene in words: where, how high, how far under the ground, and the feature
/// there (for a well, how far down it from its head).
fn said_of(p: &scene3d::Picked) -> String {
    let (lon, lat) = (p.lonlat.x, p.lonlat.y);
    let place = format!("{:.5}°{} {:.5}°{}", lat.abs(), if lat < 0.0 { "S" } else { "N" }, lon.abs(), if lon < 0.0 { "W" } else { "E" });
    let metres = |v: f64| format!("{v:.1}");
    let mut parts = vec![place, crate::i18n::fmt("elevation {z} m", &[("z", &metres(p.elevation))])];
    // A well whose head is at the ground: its depth down it says it once.
    let same = p.feature.as_ref().and_then(|f| f.below_head).zip(p.below_ground).is_some_and(|(h, g)| (h - g).abs() < 0.5);
    if let Some(d) = p.below_ground.filter(|_| !same) {
        parts.push(crate::i18n::fmt("{d} m below the ground", &[("d", &metres(d))]));
    }
    if let Some(f) = &p.feature {
        let what = match &f.label {
            Some(l) => format!("{}: {l}", f.layer),
            None => f.layer.clone(),
        };
        parts.push(match f.below_head {
            Some(d) => crate::i18n::fmt("{what}, {d} m down from its head", &[("what", &what), ("d", &metres(d))]),
            None => what,
        });
    }
    parts.join(" · ")
}

/// Draws the picture again when what it shows changed: natively off the frame (one at a time,
/// the latest wanted next), the last picture shown meanwhile; in the browser on the frame.
fn keep_drawn(app: &mut RustGisApp, ctx: &egui::Context, b: &Arc<Built>, drawn: Drawn, cam: &SceneCamera, look: Look, ppp: f32) {
    let [w, h] = drawn.size;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let landed = app.scene3d.drawing.as_ref().and_then(|j| j.landed().map(|r| (j.what.clone(), r)));
        if let Some((d, r)) = landed {
            app.scene3d.drawing = None;
            match r {
                Some((img, secs)) => {
                    app.scene3d.last_draw = secs;
                    let tex = ctx.load_texture("scene3d", img, egui::TextureOptions::LINEAR);
                    app.scene3d.texture = Some((d, tex));
                }
                None => app.scene3d.undrawn = Some(d),
            }
        }
        // One of a scene built before is cancelled; one of this scene from another camera
        // finishes, so a picture comes while it turns.
        if app.scene3d.drawing.as_ref().is_some_and(|j| j.what.built != drawn.built) {
            app.scene3d.drawing = None;
        }
        let wanted = app.scene3d.texture.as_ref().is_none_or(|(d, _)| *d != drawn) && app.scene3d.undrawn.as_ref() != Some(&drawn);
        if wanted && app.scene3d.drawing.is_none() {
            let (b, cam) = (b.clone(), *cam);
            app.scene3d.drawing = Some(Job::spawn(drawn, ctx, move || {
                let start = web_time::Instant::now();
                scene3d::draw_with(&b, &cam, look, w, h, ppp, 1).ok().map(|pm| {
                    (egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], pm.data()), start.elapsed().as_secs_f32())
                })
            }));
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let wanted = app.scene3d.texture.as_ref().is_none_or(|(d, _)| *d != drawn) && app.scene3d.undrawn.as_ref() != Some(&drawn);
        if wanted {
            let start = web_time::Instant::now();
            match scene3d::draw_with(b, cam, look, w, h, ppp, 1) {
                Ok(pm) => {
                    app.scene3d.last_draw = start.elapsed().as_secs_f32();
                    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], pm.data());
                    let tex = ctx.load_texture("scene3d", img, egui::TextureOptions::LINEAR);
                    app.scene3d.texture = Some((drawn, tex));
                }
                Err(_) => app.scene3d.undrawn = Some(drawn),
            }
        }
    }
}

/// The box's diagonal on the ground (metres).
fn b_diag(_b: &Built, scene: &Scene) -> f64 {
    let (w, h) = scene.ground_size();
    w.hypot(h)
}

/// A north arrow at the picture's top right, turned with the camera.
fn compass(ui: &Ui, rect: Rect, heading: f64, t: &Tokens) {
    let c = pos2(rect.max.x - 34.0, rect.min.y + 34.0);
    let p = ui.painter();
    p.circle_filled(c, 20.0, t.pane.gamma_multiply(0.9));
    p.circle_stroke(c, 20.0, Stroke::new(1.0, t.border));
    let a = (-heading).to_radians() as f32;
    let dir = vec2(a.sin(), -a.cos());
    let side = vec2(-dir.y, dir.x);
    let tip = c + dir * 14.0;
    p.add(egui::Shape::convex_polygon(vec![tip, c + side * 5.0, c - side * 5.0], Color32::from_rgb(0xC6, 0x28, 0x28), Stroke::NONE));
    p.add(egui::Shape::convex_polygon(vec![c - dir * 14.0, c - side * 5.0, c + side * 5.0], t.text_dim, Stroke::NONE));
    p.text(c + dir * 25.0, Align2::CENTER_CENTER, "N", semibold(10.0), t.text);
}

/// The layers the scene stands up, each with its settings, and a way to add one.
fn layers_pane(app: &mut RustGisApp, ui: &mut Ui, scene: &Scene, t: &Tokens) {
    ui.label(egui::RichText::new(tl!("Layers in the Scene")).font(semibold(13.0)));
    ui.add_space(4.0);
    let map = app.session.map();
    let addable: Vec<(String, bool)> = map
        .layers
        .iter()
        .filter(|l| l.raster.is_none() && !scene.layers.iter().any(|s| s.layer.eq_ignore_ascii_case(&l.name)))
        .filter_map(|l| match l.dataset.geometry_type {
            rustgis_engine::data::GeometryType::Point => Some((l.name.clone(), true)),
            rustgis_engine::data::GeometryType::Polygon => Some((l.name.clone(), false)),
            _ => None,
        })
        .collect();
    ui.menu_button(format!("➕ {}", tl!("Add a Layer")), |ui| {
        if addable.is_empty() {
            ui.label(tl!("No point or polygon layer to add"));
        }
        for (n, points) in addable {
            if points {
                ui.menu_button(&n, |ui| {
                    if ui.button(tl!("As Wells (down to their depth)")).clicked() {
                        add_wells(app, &n);
                        ui.close();
                    }
                    if ui.button(tl!("As Points (at their height)")).clicked() {
                        run(app, "scene.layer", json!({"layer": n, "kind": "point"}));
                        ui.close();
                    }
                });
            } else if ui.button(format!("{n} · {}", tl!("Raised"))).clicked() {
                run(app, "scene.layer", json!({"layer": n, "kind": "extrude"}));
                ui.close();
            }
        }
    });
    ui.add_space(6.0);
    if scene.layers.is_empty() {
        ui.label(egui::RichText::new(tl!("Add wells, points or polygons to stand them up in the scene.")).color(t.text_dim).small());
    }
    for sl in scene.layers.clone() {
        egui::Frame::NONE.fill(t.canvas).corner_radius(6).inner_margin(8).stroke(Stroke::new(1.0, t.border)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let kind = match sl.kind {
                    SceneKind::Well => tl!("Wells"),
                    SceneKind::Point => tl!("Points"),
                    SceneKind::Extrude => tl!("Raised"),
                };
                ui.label(egui::RichText::new(&sl.layer).font(semibold(12.5)));
                ui.label(egui::RichText::new(kind).color(t.text_dim).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").on_hover_text(tl!("Take Out of the Scene")).clicked() {
                        run(app, "scene.layer", json!({"layer": sl.layer, "remove": true}));
                    }
                });
            });
            let fields = numeric_fields(app, &sl.layer);
            let all = all_fields(app, &sl.layer);
            match sl.kind {
                SceneKind::Well => {
                    field_row(app, ui, &sl.layer, "Depth", "depth", &sl.depth, "", &fields);
                    field_row(app, ui, &sl.layer, "Wellhead height", "top", &sl.top, "The ground's", &fields);
                    field_row(app, ui, &sl.layer, "Water level", "water", &sl.water, "None", &fields);
                }
                SceneKind::Point => {
                    field_row(app, ui, &sl.layer, "Height", "elevation", &sl.elevation, "On the ground", &fields);
                }
                SceneKind::Extrude => {
                    field_row(app, ui, &sl.layer, "Height", "height", &sl.height, "Fixed", &fields);
                    if sl.height.is_none() {
                        ui.horizontal(|ui| {
                            ui.label(tl!("Metres"));
                            let mut m = sl.height_m;
                            if ui.add(egui::DragValue::new(&mut m).range(0.0..=20_000.0).speed(1.0)).changed() {
                                run(app, "scene.layer", json!({"layer": sl.layer, "heightM": m}));
                            }
                        });
                    }
                }
            }
            field_row(app, ui, &sl.layer, "Label", "label", &sl.label, "None", &all);
            if sl.kind != SceneKind::Extrude {
                ui.horizontal(|ui| {
                    ui.label(tl!("Width (m)"));
                    let auto = sl.size.is_none();
                    let mut size = sl.size.unwrap_or_else(|| (b_auto(scene)).round());
                    let r = ui.add_enabled(!auto, egui::DragValue::new(&mut size).range(0.1..=10_000.0).speed(1.0));
                    if r.changed() {
                        run(app, "scene.layer", json!({"layer": sl.layer, "size": size}));
                    }
                    let mut a = auto;
                    if ui.checkbox(&mut a, tl!("Auto")).changed() {
                        run(app, "scene.layer", json!({"layer": sl.layer, "size": if a { Value::Null } else { json!(size) }}));
                    }
                });
            }
            ui.horizontal(|ui| {
                let mut own = sl.color.is_none();
                if ui.checkbox(&mut own, tl!("Symbology colours")).changed() {
                    let c = if own { Value::Null } else { json!("#2E7D32") };
                    run(app, "scene.layer", json!({"layer": sl.layer, "color": c}));
                }
                if let Some(c) = sl.color {
                    let mut c32 = Color32::from_rgb(c.r(), c.g(), c.b());
                    if ui.color_edit_button_srgba(&mut c32).changed() {
                        run(
                            app,
                            "scene.layer",
                            json!({"layer": sl.layer, "color": format!("#{:02X}{:02X}{:02X}", c32.r(), c32.g(), c32.b())}),
                        );
                    }
                }
            });
        });
        ui.add_space(6.0);
    }
}

/// A row choosing the field a scene layer's `key` reads, from `from`; `optional` names what none
/// means (empty: one must be chosen).
#[expect(clippy::too_many_arguments, reason = "one row of the pane, its field and what none means")]
fn field_row(
    app: &mut RustGisApp,
    ui: &mut Ui,
    layer: &str,
    label: &str,
    key: &str,
    value: &Option<String>,
    optional: &str,
    from: &[String],
) {
    ui.horizontal(|ui| {
        ui.label(tl!(label));
        let shown = value.clone().unwrap_or_else(|| tl!(optional).to_string());
        egui::ComboBox::from_id_salt(("scene3d_field", layer, key)).selected_text(shown).width(130.0).show_ui(ui, |ui| {
            if !optional.is_empty() && ui.selectable_label(value.is_none(), tl!(optional)).clicked() {
                run(app, "scene.layer", json!({"layer": layer, key: null}));
            }
            for f in from {
                if ui.selectable_label(value.as_deref() == Some(f.as_str()), f).clicked() {
                    run(app, "scene.layer", json!({"layer": layer, key: f}));
                }
            }
        });
    });
}

/// The width a well or point is given by default (metres): as the renderer works it out.
fn b_auto(scene: &Scene) -> f64 {
    let (w, h) = scene.ground_size();
    w.hypot(h) / 250.0
}

/// Adds a point layer as wells, its depth the field most like one.
fn add_wells(app: &mut RustGisApp, layer: &str) {
    let fields = numeric_fields(app, layer);
    let like = |words: &[&str]| fields.iter().find(|f| words.iter().any(|w| f.to_lowercase().contains(w))).cloned();
    let Some(depth) = like(&["depth", "ลึก", "deep"]).or_else(|| fields.first().cloned()) else {
        app.scene3d.message = Some((tl!("Wells need a field of their depth (a number): this layer has none").to_string(), true));
        return;
    };
    let mut p = json!({"layer": layer, "kind": "well", "depth": depth});
    if let Some(w) = like(&["water", "น้ำ", "swl"]).filter(|w| *w != depth) {
        p["water"] = json!(w);
    }
    if let Some(n) = all_fields(app, layer).into_iter().find(|f| ["name", "ชื่อ", "id", "รหัส"].iter().any(|w| f.to_lowercase().contains(w)))
    {
        p["label"] = json!(n);
    }
    run(app, "scene.layer", p);
}

fn numeric_fields(app: &RustGisApp, layer: &str) -> Vec<String> {
    use rustgis_engine::data::FieldType;
    app.session
        .map()
        .layer_by_name(layer)
        .map(|l| {
            l.dataset.fields.iter().filter(|f| matches!(f.ty, FieldType::Integer | FieldType::Double)).map(|f| f.name.clone()).collect()
        })
        .unwrap_or_default()
}

fn all_fields(app: &RustGisApp, layer: &str) -> Vec<String> {
    app.session.map().layer_by_name(layer).map(|l| l.dataset.fields.iter().map(|f| f.name.clone()).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(app: &mut RustGisApp, ctx: &egui::Context) {
        frame_with(app, ctx, Vec::new());
    }

    fn frame_with(app: &mut RustGisApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        let screen = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 700.0)));
        let input = egui::RawInput { screen_rect: screen, events, ..Default::default() };
        let mut pass = ctx.run_ui(input, |ui| crate::mapview::show(app, ui));
        pass.textures_delta.clear();
    }

    /// Frames until the scene has been built (on its thread) since `before`, and drawn.
    fn built_after(app: &mut RustGisApp, ctx: &egui::Context, before: Option<usize>) -> usize {
        let end = web_time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            frame(app, ctx);
            if let Some((_, b)) = &app.scene3d.built {
                let now = Arc::as_ptr(b) as usize;
                if Some(now) != before && app.scene3d.building.is_none() && app.scene3d.drawing.is_none() && app.scene3d.texture.is_some() {
                    return now;
                }
            }
            assert!(web_time::Instant::now() < end, "the scene was never built: {:?}", app.scene3d.error);
            std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: the scene is built on a thread of its own
        }
    }

    #[test]
    fn a_point_under_the_mouse_is_said_in_thai() {
        let was = crate::i18n::current();
        crate::i18n::set_current(crate::i18n::Lang::from_code("th").unwrap());
        let place = rustgis_engine::geom::Coord::new(98.42115, 18.56087);
        let ground = scene3d::Picked { lonlat: place, elevation: 512.04, below_ground: None, feature: None };
        assert_eq!(said_of(&ground), "18.56087°N 98.42115°E · ระดับ 512.0 ม. (รทก.)");
        // Down a well whose head is at the ground: its depth once.
        let well = scene3d::PickedFeature { layer: "บ่อ".into(), label: Some("บ่อ 08".into()), below_head: Some(35.0) };
        let down = scene3d::Picked { elevation: 477.0, below_ground: Some(35.2), feature: Some(well.clone()), ..ground.clone() };
        assert_eq!(said_of(&down), "18.56087°N 98.42115°E · ระดับ 477.0 ม. (รทก.) · บ่อ: บ่อ 08 ลึก 35.0 ม. จากปากบ่อ");
        // Its head above the ground (a casing's top from a field): both.
        let raised = scene3d::Picked { below_ground: Some(33.0), ..down };
        assert!(said_of(&raised).contains("ลึก 33.0 ม. จากผิวดิน · บ่อ: บ่อ 08 ลึก 35.0 ม. จากปากบ่อ"));
        // South and west.
        let south = scene3d::Picked { lonlat: rustgis_engine::geom::Coord::new(-43.2, -22.9), ..ground };
        assert!(said_of(&south).starts_with("22.90000°S 43.20000°W"));
        crate::i18n::set_current(was);
    }

    #[test]
    fn what_is_under_the_mouse_is_looked_for_off_the_frame() {
        let mut app = RustGisApp::with_sample();
        let ctx = egui::Context::default();
        app.run("scene.set", json!({"bbox": [100.4, 13.6, 100.7, 13.9]})).unwrap();
        app.run("ui.scene", Value::Null).unwrap();
        built_after(&mut app, &ctx, None);
        // The mouse over the picture's middle: looked for on a thread, said when it lands.
        let end = web_time::Instant::now() + std::time::Duration::from_secs(60);
        frame_with(&mut app, &ctx, vec![egui::Event::PointerMoved(pos2(640.0, 420.0))]);
        assert!(app.scene3d.picking.is_some() || app.scene3d.hover.is_some(), "looked for");
        let first = app.scene3d.picking.as_ref().map(|j| j.stop.clone());
        // Moved on before it landed: the one left behind is cancelled.
        frame_with(&mut app, &ctx, vec![egui::Event::PointerMoved(pos2(600.0, 400.0))]);
        assert!(first.is_none_or(|s| s.load(std::sync::atomic::Ordering::Relaxed) || app.scene3d.hover.is_some()));
        loop {
            frame(&mut app, &ctx);
            if app.scene3d.picking.is_none() && app.scene3d.hover.is_some() {
                break;
            }
            assert!(web_time::Instant::now() < end, "what is under the mouse never landed");
            std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: it is looked for on a thread of its own
        }
        let (_, point, said) = app.scene3d.hover.clone().unwrap();
        assert!(said.is_some_and(|t| t.contains("°N")), "the ground under it");
        assert!(point[0] > 0 && point[1] > 0);
        // Off the picture: nothing said, nothing looked for.
        frame_with(&mut app, &ctx, vec![egui::Event::PointerGone]);
        assert!(app.scene3d.hover.is_none() && app.scene3d.picking.is_none());
    }

    #[test]
    fn every_note_of_a_build_is_in_the_notices_once() {
        let mut app = RustGisApp::with_sample();
        let mut scene = rustgis_engine::map::scene::Scene::new([11_180_000.0, 1_520_000.0, 11_200_000.0, 1_540_000.0]);
        for name in ["ไม่มี ก", "ไม่มี ข"] {
            scene.layers.push(rustgis_engine::map::scene::SceneLayer::new(name, rustgis_engine::map::scene::SceneKind::Point));
        }
        let b = scene3d::build(app.session.map().detached(), &scene, Detail { drape_px: 64, cells: 16 }).unwrap();
        let said = |app: &RustGisApp| app.session.notices.iter().filter(|n| n.text.starts_with("3D Scene: ")).count();
        say_notes(&mut app, &b);
        assert_eq!(said(&app), 2, "{:?}", b.notes);
        assert!(app.scene3d.message.as_ref().is_some_and(|(m, _)| m.ends_with("(+1)")));
        // Built again with the same losses: not said again.
        say_notes(&mut app, &b);
        assert_eq!(said(&app), 2);
    }

    #[test]
    fn the_scene_view_builds_once_and_again_only_when_the_scene_changes() {
        let mut app = RustGisApp::with_sample();
        let ctx = egui::Context::default();
        // No scene, and a whole world in view (too wide for one): said, nothing shown.
        assert!(!shown(&app));
        app.run("scene.set", json!({"bbox": [100.4, 13.6, 100.7, 13.9]})).unwrap();
        app.run("ui.scene", Value::Null).unwrap();
        assert!(shown(&app));
        let first = built_after(&mut app, &ctx, None);
        // Drawn, and not built again frame after frame.
        assert!(app.scene3d.texture.is_some());
        for _ in 0..5 {
            frame(&mut app, &ctx);
        }
        assert!(app.scene3d.building.is_none());
        assert_eq!(app.scene3d.built.as_ref().map(|(_, b)| Arc::as_ptr(b) as usize), Some(first));
        // Its opacity and its camera only draw it again.
        run_drawing_only(&mut app, "scene.set", json!({"groundOpacity": 0.4}));
        run_drawing_only(&mut app, "scene.set", json!({"ruler": false}));
        run_drawing_only(&mut app, "scene.set", json!({"rulerStep": 25}));
        run_drawing_only(&mut app, "scene.set", json!({"rulerMin": -100, "rulerMax": 900, "rulerFrom": 5}));
        run_drawing_only(&mut app, "scene.camera", json!({"turn": 45}));
        frame(&mut app, &ctx);
        assert!(app.scene3d.building.is_none(), "{:?}", app.scene3d.message);
        assert_eq!(app.session.map().scene.as_ref().unwrap().camera.heading, 75.0);
        // Its exaggeration builds it again, and so does the block's colour.
        app.run("scene.set", json!({"exaggeration": 3})).unwrap();
        let second = built_after(&mut app, &ctx, Some(first));
        assert_ne!(first, second);
        app.run("scene.set", json!({"walls": "#6B4F3A"})).unwrap();
        let third = built_after(&mut app, &ctx, Some(second));
        assert_ne!(second, third);
        // The map's tab hides it, a build under way cancelled; its own shows it again.
        app.run("scene.set", json!({"exaggeration": 4})).unwrap();
        frame(&mut app, &ctx);
        let stop = app.scene3d.building.as_ref().map(|j| j.stop.clone());
        app.run("ui.scene", json!({"show": false})).unwrap();
        assert!(!shown(&app));
        frame(&mut app, &ctx);
        assert!(app.scene3d.building.is_none() && app.scene3d.drawing.is_none());
        assert!(stop.is_none_or(|s| s.load(std::sync::atomic::Ordering::Relaxed)), "its Cancel pressed");
        app.run("ui.scene", Value::Null).unwrap();
        assert!(shown(&app));
        // Taken off the map, the view goes.
        app.run("scene.remove", Value::Null).unwrap();
        frame(&mut app, &ctx);
        assert!(!shown(&app));
    }

    #[test]
    fn metres_are_read_as_typed() {
        assert_eq!(read_metres(" 25 "), Ok(Some(25.0)));
        assert_eq!(read_metres("1,200 m"), Ok(Some(1200.0)));
        assert_eq!(read_metres("-50.5ม."), Ok(Some(-50.5)));
        assert_eq!(read_metres("2 500 เมตร"), Ok(Some(2500.0)));
        assert_eq!(read_metres("\u{2212}20"), Ok(Some(-20.0)));
        assert_eq!(read_metres(""), Ok(None));
        assert_eq!(read_metres("Auto"), Ok(None));
        assert_eq!(read_metres("abc"), Err(()));
        assert_eq!(read_metres("1e400"), Err(()));
        assert_eq!((plain_metres(1200.0), plain_metres(0.25), plain_metres(-0.0001)), ("1200".into(), "0.25".into(), "0".into()));
    }

    /// The ruler's and the grid's settings are typed: Enter sets one, an emptied box sets it back
    /// to automatic, a number with thousands apart and its unit is read, and what isn't a number
    /// is said and sets nothing. The grid is turned on beside the ruler.
    #[test]
    fn the_rulers_and_the_grids_settings_are_typed() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("scene.set", json!({"bbox": [100.4, 13.6, 100.7, 13.9]})).unwrap();
        let mut fonts = false;
        // (The ground opacity's slider beside them has no name of its own for a screen reader.)
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 420.0)).with_accessibility_check(false).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let Some(scene) = app.session.map().scene.clone() else { return };
                ui.horizontal_wrapped(|ui| shown_row(app, ui, &scene));
            },
            app,
        );
        h.run_steps(2);
        let scene = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().scene.clone().unwrap();
        let type_in = |h: &mut egui_kittest::Harness<'_, RustGisApp>, field: &str, text: &str| {
            h.get_by_role_and_label(Role::TextInput, field).focus();
            h.run();
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            h.key_press(egui::Key::Backspace);
            if !text.is_empty() {
                h.get_by_role_and_label(Role::TextInput, field).type_text(text);
            }
            h.run();
            h.key_press(egui::Key::Enter);
            h.run_steps(2);
        };
        h.get_by_label("Ruler Settings").click();
        h.run_steps(2);
        type_in(&mut h, "Ruler step", "25");
        assert_eq!(scene(&h).ruler_step, Some(25.0));
        type_in(&mut h, "Ruler top", "1,200 m");
        assert_eq!(scene(&h).ruler_max, Some(1200.0));
        type_in(&mut h, "Ruler step", "");
        assert_eq!(scene(&h).ruler_step, None, "emptied: automatic");
        type_in(&mut h, "Ruler bottom", "lots");
        assert_eq!(scene(&h).ruler_min, None);
        assert!(h.query_by_label_contains("type a number of metres").is_some(), "said under the boxes");
        type_in(&mut h, "Ruler bottom", "-50");
        assert_eq!(scene(&h).ruler_min, Some(-50.0));
        assert!(h.query_by_label_contains("type a number of metres").is_none(), "said no more");
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        // The grid, beside it.
        assert!(!scene(&h).grid);
        h.get_by_label("3D Grid").click();
        h.run_steps(2);
        assert!(scene(&h).grid);
        h.get_by_label("Grid Settings").click();
        h.run_steps(2);
        type_in(&mut h, "E/N step", "250");
        type_in(&mut h, "Height step", "20");
        let s = scene(&h);
        assert_eq!((s.grid_step, s.grid_z_step), (Some(250.0), Some(20.0)));
    }

    #[test]
    fn opening_the_view_makes_a_scene_over_the_selected_dem() {
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let mut app = RustGisApp::with_sample();
        let gt = GeoTransform { origin_x: 11_190_000.0, origin_y: 1_550_000.0, pixel_width: 100.0, pixel_height: -100.0 };
        let dem = Raster::new("ความสูง", 50, 40, vec![vec![10.0; 2000]], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let id = app.session.map_mut().add_raster("ความสูง", rustgis_engine::map::RasterLayer::from_path(dem, "dem.tif"));
        app.session.active_layer = Some(id);
        app.run("ui.scene", Value::Null).unwrap();
        let s = app.session.map().scene.clone().unwrap();
        assert_eq!(s.dem.as_deref(), Some("ความสูง"));
        let want = [11_190_000.0, 1_546_000.0, 11_195_000.0, 1_550_000.0];
        assert!(s.extent.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3), "{:?}", s.extent);
    }
}
