//! Flood Simulation from the map (right-click › Flood From Here, or View › Scene › 3D Terrain):
//! a flood asked for waits here while its elevation tiles come, then runs, says what it found,
//! and is shown over the 3D terrain.

use egui::Ui;
use rustgis_engine::cmd::workflows::flood_loading;
use rustgis_engine::geom::Coord;
use serde_json::{Value, json};

use crate::{RustGisApp, i18n};

/// The rises offered: metres the water stands over the ground where it comes in.
pub const RISES: [f64; 5] = [0.5, 1.0, 2.0, 3.0, 5.0];

/// How long a flood waits for its elevation tiles before giving up.
const WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// A flood asked for, waiting for its elevation.
pub struct Job {
    params: Value,
    since: web_time::Instant,
}

/// "+2 m", "+0.5 m".
pub fn rise_label(rise: f64) -> String {
    i18n::fmt("+{rise} m", &[("rise", &rise.to_string())])
}

/// Ground metres around `c` a flood covers: about what the view shows, 2 to 50 km.
fn radius(app: &RustGisApp, c: Coord) -> f64 {
    let cam = &app.session.map().camera;
    // Web Mercator metres a pixel, at the point's latitude: ground metres.
    let ground_px = cam.res * c.y.to_radians().cos().abs();
    let r = ground_px * 500.0;
    if r.is_finite() { r.clamp(2_000.0, 50_000.0) } else { 10_000.0 }
}

/// Floods the ground from `c` (lon/lat) with water `rise` metres over it: run once its elevation
/// is read (`poll`).
pub fn start(app: &mut RustGisApp, c: Coord, rise: f64) {
    let name = i18n::fmt("Flood {rise}", &[("rise", &rise_label(rise))]);
    let params = json!({"lon": c.x, "lat": c.y, "rise": rise, "radius": radius(app, c).round(), "name": name});
    app.session.notify(i18n::t("Flood Simulation: reading the ground's elevation…"), false);
    app.ui.flood = Some(Job { params, since: web_time::Instant::now() });
}

/// Runs a flood waiting for its elevation once the elevation is read; says what it found.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(job) = app.ui.flood.as_ref() else { return };
    let (params, since) = (job.params.clone(), job.since);
    match flood_loading(&app.session, &params) {
        Ok(Some(_)) if since.elapsed() < WAIT => {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }
        Ok(Some(_)) => {
            app.ui.flood = None;
            app.session
                .notify(i18n::t("The ground's elevation couldn't be read in time: check the internet connection and try again."), true);
            return;
        }
        Ok(None) => {}
        Err(e) => {
            app.ui.flood = None;
            app.session.notify(i18n::fmt("Flood Simulation: {error}", &[("error", &e)]), true);
            return;
        }
    }
    app.ui.flood = None;
    match app.run("analysis.flood", params) {
        Ok(v) => {
            show_in_3d(app);
            app.session.notify(summary(&v), false);
        }
        Err(e) => app.session.notify(i18n::fmt("Flood Simulation: {error}", &[("error", &e)]), true),
    }
    ctx.request_repaint();
}

/// Shows the water over the ground: the world's elevation as the terrain when the map has none,
/// and the map tilted (off the globe, which doesn't tilt).
fn show_in_3d(app: &mut RustGisApp) {
    if app.session.map().view.terrain.is_none() {
        let _ = app.run("map.terrain", json!({"world": true}));
    }
    if app.session.map().projection.is_globe() {
        let _ = app.run("map.projection", json!({"type": "mercator"}));
    }
    if app.session.map().camera.pitch < 20.0 {
        let _ = app.run("map.setPitch", json!({"pitch": 55}));
    }
}

/// What a flood found: its area (km² and rai), the deepest water and the level it stands at.
pub fn summary(v: &Value) -> String {
    let n = |k: &str| v[k].as_f64().unwrap_or(0.0);
    let km2 = n("areaKm2");
    let area = if km2 >= 10.0 { format!("{km2:.0}") } else { format!("{km2:.2}") };
    let rai = format_thousands(n("areaRai").round());
    let mut text = i18n::fmt(
        "{name}: water covers {area} km² ({rai} rai), up to {depth} m deep; it stands at {level} m above sea level, {ground} m where it comes in.",
        &[
            ("name", v["name"].as_str().unwrap_or_default()),
            ("area", &area),
            ("rai", &rai),
            ("depth", &format!("{:.1}", n("maxDepth"))),
            ("level", &format!("{:.1}", n("level"))),
            ("ground", &format!("{:.1}", n("ground"))),
        ],
    );
    if v["reachesEdge"].as_bool() == Some(true) {
        text.push(' ');
        text.push_str(i18n::t("The water reaches the edge of the area worked out: zoom out and run it again to see further."));
    }
    text
}

/// 12345 → "12,345".
fn format_thousands(v: f64) -> String {
    let s = format!("{:.0}", v.abs());
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if v < 0.0 { format!("-{out}") } else { out }
}

/// The rows of the map's right-click menu: a flood from the clicked place at each rise.
pub fn menu_rows(app: &mut RustGisApp, ui: &mut Ui, c: Coord) {
    let waiting = app.ui.flood.is_some();
    crate::widgets::submenu_row(ui, "relief", i18n::t("Flood From Here"), |ui| {
        ui.label(egui::RichText::new(i18n::t("Water over the ground here (a river, a canal):")).small().weak());
        for rise in RISES {
            if ui.add_enabled(!waiting, egui::Button::new(rise_label(rise))).clicked() {
                start(app, c, rise);
                ui.close();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flood_says_its_area_depth_and_level() {
        assert_eq!(format_thousands(1234567.0), "1,234,567");
        assert_eq!(format_thousands(999.0), "999");
        let v = json!({"name": "Flood +2 m", "areaKm2": 12.6, "areaRai": 7875.0, "maxDepth": 3.21, "level": 5.0, "ground": 3.0, "reachesEdge": true});
        let s = summary(&v);
        assert!(s.contains("13 km²") && s.contains("7,875 rai") && s.contains("3.2 m deep"), "{s}");
        assert!(s.contains("edge"), "{s}");
        assert_eq!(rise_label(0.5), "+0.5 m");
    }
}
