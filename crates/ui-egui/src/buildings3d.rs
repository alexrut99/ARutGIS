//! 3D Buildings (View › Scene): OpenStreetMap's buildings where the map is, asked for off the
//! frame (a thread natively, fetch on the web) and handed to `buildings.add3d`, which raises them
//! by their heights; then the map is tilted to show them.

use rustgis_engine::render::rustgis_tiles::cache;
use serde_json::{Value, json};

use crate::{RustGisApp, Slot, i18n};

type Answer = Slot<Result<Vec<u8>, String>>;

/// The request under way: its parameters and the answer to come.
pub struct Job {
    params: Value,
    answer: Answer,
}

/// Whether buildings are being downloaded.
pub fn busy(app: &RustGisApp) -> bool {
    app.ui.buildings.is_some()
}

/// Asks for the buildings in the map's view (narrowed to about 3 km × 3 km about its centre).
pub fn start(app: &mut RustGisApp) {
    let mut p = json!({"name": i18n::t("3D Buildings")});
    let url = rustgis_engine::cmd::portals::buildings_query(&app.session, &p).and_then(|(q, narrowed)| {
        // The box the request was for, so the answer is read for the same one.
        p["bbox"] = json!(q.bbox);
        if narrowed {
            app.session.notify(
                i18n::t("3D Buildings: the view is large, so the buildings around its centre (about 3 km × 3 km) are downloaded."),
                false,
            );
        }
        q.url(None)
    });
    match url {
        Ok(url) => {
            let slot: Answer = Slot::default();
            let land = slot.clone();
            cache::download(&url, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
            app.session.notify(i18n::t("3D Buildings: downloading buildings from OpenStreetMap…"), false);
            app.ui.buildings = Some(Job { params: p, answer: slot });
        }
        Err(e) => app.session.notify(i18n::fmt("3D Buildings: {error}", &[("error", &e)]), true),
    }
}

/// Lands the answer: the buildings are added, raised and shown tilted.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(job) = &app.ui.buildings else { return };
    let Some(r) = job.answer.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
        return;
    };
    let Some(Job { mut params, .. }) = app.ui.buildings.take() else { return };
    let added = r.and_then(|b| String::from_utf8(b).map_err(|_| "the answer isn't UTF-8 text".to_string())).and_then(|t| {
        params["answer"] = json!(t);
        app.run("buildings.add3d", params)
    });
    match added {
        Ok(v) => {
            show_tilted(app);
            app.session.notify(summary(&v), false);
            app.map.invalidate();
        }
        Err(e) => app.session.notify(i18n::fmt("3D Buildings: {error}", &[("error", &e)]), true),
    }
    ctx.request_repaint();
}

/// Tilts the map to show the buildings standing (off the globe, which draws them flat).
fn show_tilted(app: &mut RustGisApp) {
    if app.session.map().projection.is_globe() {
        let _ = app.run("map.projection", json!({"type": "mercator"}));
    }
    if app.session.map().camera.pitch < 20.0 {
        let _ = app.run("map.setPitch", json!({"pitch": 60}));
        let _ = app.run("map.setBearing", json!({"bearing": -20}));
    }
}

/// How many buildings came, and where their heights came from.
pub fn summary(v: &Value) -> String {
    let n = |k: &str| v[k].as_u64().unwrap_or(0).to_string();
    i18n::fmt(
        "3D Buildings: {n} buildings, the tallest {tallest} m. Heights: {tagged} from OpenStreetMap's height, {floors} from the number of floors, {estimated} estimated from the kind of building (the height_src field says which).",
        &[
            ("n", &n("buildings")),
            ("tallest", &format!("{:.0}", v["tallest"].as_f64().unwrap_or(0.0))),
            ("tagged", &n("fromHeight")),
            ("floors", &n("fromFloors")),
            ("estimated", &n("estimated")),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_says_where_the_heights_came_from() {
        let v = json!({"buildings": 1200, "fromHeight": 40, "fromFloors": 160, "estimated": 1000, "tallest": 314.2});
        let s = summary(&v);
        assert!(s.contains("1200 buildings") && s.contains("314 m") && s.contains("40 from") && s.contains("1000 estimated"), "{s}");
    }
}
