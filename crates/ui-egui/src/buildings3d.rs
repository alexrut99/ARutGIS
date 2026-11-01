//! 3D Buildings (View › Scene): the buildings where the map is, from the source the user picks
//! (OpenStreetMap, or Overture Maps), asked for off the frame (a thread natively, fetch on the
//! web) and handed to `buildings.add3d`, which raises them by the heights the user allows; then
//! the map is tilted to show them.

use rustgis_engine::render::rustgis_tiles::buildings3d::{Heights, Source};
use rustgis_engine::render::rustgis_tiles::{cache, overture};
use serde_json::{Value, json};

use crate::{RustGisApp, Slot, i18n};

type Answer = Slot<Result<Vec<u8>, String>>;

/// Whether Google's 2.5D heights can be read here: its server doesn't answer web pages.
pub const GOOGLE: bool = cfg!(not(target_arch = "wasm32"));
type Read = Slot<Result<Vec<Value>, String>>;

/// The user's choices: where the buildings come from and which heights raise them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub source: Source,
    pub heights: Heights,
}

/// Where a request is.
enum Step {
    /// OpenStreetMap's answer from the Overpass server.
    Osm(Answer),
    /// Overture's catalog, for its latest release.
    Latest(Answer),
    /// The release's buildings collection: every file's extent.
    Collection(Answer),
    /// The files' STAC items, for their addresses.
    Items(Vec<Answer>),
    /// The buildings in the area, read from each file.
    Reading(Vec<Read>),
    /// Google's 2.5D heights over the area, read on a thread (the desktop app's); then the
    /// command finds them kept.
    Google(Slot<Result<(), String>>),
}

/// The request under way: its parameters and where it is.
pub struct Job {
    params: Value,
    step: Step,
}

/// Whether buildings are being downloaded.
pub fn busy(app: &RustGisApp) -> bool {
    app.ui.buildings.is_some()
}

fn fetch(url: &str) -> Answer {
    let slot: Answer = Slot::default();
    let land = slot.clone();
    cache::download(url, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
    slot
}

fn take<T>(slot: &Slot<T>) -> Option<T> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Every slot's answer once all are in (taken), else `None` (left).
fn all_in<T>(slots: &[Slot<T>]) -> Option<Vec<T>> {
    if slots.iter().all(|s| s.lock().unwrap_or_else(|e| e.into_inner()).is_some()) { slots.iter().map(take).collect() } else { None }
}

/// Asks for the buildings in the map's view (narrowed to about 3 km × 3 km about its centre), from
/// the chosen source.
pub fn start(app: &mut RustGisApp) {
    let Choice { source, heights } = app.ui.buildings_choice;
    let mut p = json!({
        "name": i18n::t("3D Buildings"),
        "source": source.code(),
        "heights": {"data": heights.data, "google": heights.google && GOOGLE, "estimate": heights.estimate},
    });
    let asked = rustgis_engine::cmd::portals::buildings_query(&app.session, &p).and_then(|(q, narrowed)| {
        // The box the request was for, so the answer is read for the same one.
        p["bbox"] = json!(q.bbox);
        if narrowed {
            app.session.notify(
                i18n::t("3D Buildings: the view is large, so the buildings around its centre (about 3 km × 3 km) are downloaded."),
                false,
            );
        }
        match source {
            Source::Osm => q.url(None).map(|url| Step::Osm(fetch(&url))),
            // The release the catalogs found already, else the catalog's latest.
            Source::Overture => Ok(match app.ui.catalogs.release.clone() {
                Some(r) => Step::Collection(fetch(&overture::collection_url(&r, "buildings", "building"))),
                None => Step::Latest(fetch(&overture::catalog_url())),
            }),
        }
    });
    match asked {
        Ok(step) => {
            app.session.notify(
                match source {
                    Source::Osm => i18n::t("3D Buildings: downloading buildings from OpenStreetMap…"),
                    Source::Overture => i18n::t("3D Buildings: reading buildings from Overture Maps…"),
                },
                false,
            );
            app.ui.buildings = Some(Job { params: p, step });
        }
        Err(e) => app.session.notify(i18n::fmt("3D Buildings: {error}", &[("error", &e)]), true),
    }
}

/// The area a job asks for.
fn area(params: &Value) -> Result<[f64; 4], String> {
    let n: Option<Vec<f64>> = params["bbox"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect());
    <[f64; 4]>::try_from(n.unwrap_or_default().as_slice()).map_err(|_| "the area isn't known".to_string())
}

/// Reads the buildings of Overture's files in the area, each off the frame.
fn read_files(urls: Vec<String>, area: [f64; 4]) -> Step {
    let [w, s, e, n] = area;
    let req = rustgis_engine::data::source::Request {
        extent: Some(rustgis_engine::data::Extent::new(w, s, e, n)),
        limit: rustgis_engine::data::source::MAX_LIMIT,
    };
    Step::Reading(
        urls.into_iter()
            .map(|url| {
                let slot: Read = Slot::default();
                let land = slot.clone();
                rustgis_engine::data::source::query_url(
                    &url,
                    vec!["names.primary".into()],
                    req,
                    Box::new(move |r| {
                        let r = r
                            .map(|(fields, batch)| rustgis_engine::cmd::portals::overture_features(&fields, &batch.features))
                            .map_err(|e| e.to_string());
                        *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                    }),
                );
                slot
            })
            .collect(),
    )
}

/// The next step once `step`'s answers are in: another step, or the buildings found (`None` while
/// waiting).
fn advance(app: &mut RustGisApp, params: &mut Value, step: Step) -> Result<Result<Step, ()>, String> {
    Ok(match step {
        Step::Osm(a) => match take(&a) {
            None => Ok(Step::Osm(a)),
            Some(r) => {
                let t = r.and_then(|b| String::from_utf8(b).map_err(|_| "the answer isn't UTF-8 text".to_string()))?;
                params["answer"] = json!(t);
                then_google(params)?
            }
        },
        Step::Latest(a) => match take(&a) {
            None => Ok(Step::Latest(a)),
            Some(r) => {
                let release = r.and_then(|b| overture::parse_latest(&b))?;
                app.ui.catalogs.release = Some(release.clone());
                Ok(Step::Collection(fetch(&overture::collection_url(&release, "buildings", "building"))))
            }
        },
        Step::Collection(a) => match take(&a) {
            None => Ok(Step::Collection(a)),
            Some(r) => {
                // A retired release: the next try reads the catalog again.
                if r.as_ref().err().is_some_and(|e| cache::http_status(e) == Some(404)) {
                    app.ui.catalogs.release = None;
                }
                let files = r.and_then(|b| overture::parse_collection(&b))?;
                let hits: Vec<String> = overture::files_in(&files, area(params)?).iter().map(|f| f.item.clone()).collect();
                if hits.is_empty() {
                    return Err(i18n::t("Overture has no buildings here").to_string());
                }
                if hits.len() > overture::MAX_FILES {
                    return Err(i18n::t("The area takes too many of Overture's files: zoom in").to_string());
                }
                Ok(Step::Items(hits.iter().map(|h| fetch(h)).collect()))
            }
        },
        Step::Items(items) => match all_in(&items) {
            None => Ok(Step::Items(items)),
            Some(done) => {
                let urls = done.into_iter().map(|r| r.and_then(|b| overture::parse_item(&b))).collect::<Result<Vec<_>, _>>()?;
                Ok(read_files(urls, area(params)?))
            }
        },
        Step::Reading(reads) => match all_in(&reads) {
            None => Ok(Step::Reading(reads)),
            Some(done) => {
                let mut all = Vec::new();
                for r in done {
                    all.extend(r?);
                }
                params["features"] = Value::Array(all);
                then_google(params)?
            }
        },
        Step::Google(slot) => match take(&slot) {
            None => Ok(Step::Google(slot)),
            Some(r) => {
                // Not read: the buildings come without them, rather than the command waiting.
                if let Err(e) = r {
                    params["heights"]["google"] = json!(false);
                    app.session.notify(i18n::fmt("3D Buildings: Google's 2.5D heights weren't read: {error}", &[("error", &e)]), true);
                }
                Err(())
            }
        },
    })
}

/// Once the buildings are in: Google's heights read first when they are asked for, else done.
fn then_google(params: &Value) -> Result<Result<Step, ()>, String> {
    if !params["heights"]["google"].as_bool().unwrap_or(false) {
        return Ok(Err(()));
    }
    let slot: Slot<Result<(), String>> = Slot::default();
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_engine::render::rustgis_tiles::google25d;
        let (land, area) = (slot.clone(), area(params)?);
        std::thread::spawn(move || {
            let r = google25d::grids_blocking(area, google25d::YEAR).map(|_| ());
            *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err("the web app can't read them".into()));
    }
    Ok(Ok(Step::Google(slot)))
}

/// Moves the request along; once the buildings are in, they are added, raised and shown tilted.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(Job { mut params, step }) = app.ui.buildings.take() else { return };
    let added = match advance(app, &mut params, step) {
        Ok(Ok(next)) => {
            app.ui.buildings = Some(Job { params, step: next });
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            return;
        }
        Ok(Err(())) => app.run("buildings.add3d", params),
        Err(e) => Err(e),
    };
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
        "3D Buildings: {n} buildings, the tallest {tallest} m. Heights: {tagged} from the data's height, {floors} from the number of floors, {google} from Google 2.5D, {estimated} estimated from the kind of building, {one} one floor (the height_src field says which).",
        &[
            ("n", &n("buildings")),
            ("tallest", &format!("{:.0}", v["tallest"].as_f64().unwrap_or(0.0))),
            ("tagged", &n("fromHeight")),
            ("floors", &n("fromFloors")),
            ("google", &n("fromGoogle")),
            ("estimated", &n("estimated")),
            ("one", &n("oneFloor")),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_says_where_the_heights_came_from() {
        let v = json!({"buildings": 1200, "fromHeight": 40, "fromFloors": 160, "fromGoogle": 500, "estimated": 490, "oneFloor": 10, "tallest": 314.2});
        let s = summary(&v);
        assert!(
            s.contains("1200 buildings")
                && s.contains("314 m")
                && s.contains("40 from")
                && s.contains("500 from Google")
                && s.contains("490 estimated")
                && s.contains("10 one floor"),
            "{s}"
        );
    }

    #[test]
    fn the_area_is_read_back_from_the_request() {
        assert_eq!(area(&json!({"bbox": [100.5, 13.7, 100.53, 13.73]})), Ok([100.5, 13.7, 100.53, 13.73]));
        assert!(area(&json!({})).is_err());
    }
}
