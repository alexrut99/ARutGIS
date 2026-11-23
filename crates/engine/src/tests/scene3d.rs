//! The 3D scene's commands (ARutGIS): `scene.set`, `scene.layer`, `scene.camera`,
//! `scene.remove` and its exports.

use serde_json::json;

use super::{dem_b64, world};
use crate::Session;

/// The world's sample with a DEM (about 3 km a side at 45° N) and a layer of two wells on it.
fn with_wells() -> (Session, String) {
    let mut s = world();
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": dem_b64()})).unwrap();
    let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
    let wells = json!({"type": "FeatureCollection", "features": [
        {"type": "Feature", "properties": {"ชื่อ": "บ่อ 1", "ลึก": 80, "น้ำ": 12.5}, "geometry": {"type": "Point", "coordinates": [10.01, 45.02]}},
        {"type": "Feature", "properties": {"ชื่อ": "บ่อ 2", "ลึก": null, "น้ำ": null}, "geometry": {"type": "Point", "coordinates": [10.03, 45.02]}}]});
    s.run("layer.addDataText", &json!({"fileName": "บ่อ.geojson", "text": wells.to_string()})).unwrap();
    (s, dem)
}

#[test]
fn a_scene_is_set_over_a_dem_and_stands_wells_up() {
    let (mut s, dem) = with_wells();
    // No scene yet: the layer and camera commands say so.
    assert!(s.run("scene.layer", &json!({"layer": "บ่อ"})).unwrap_err().contains("no 3D scene"));
    assert!(s.run("scene.camera", &json!({"turn": 10})).unwrap_err().contains("no 3D scene"));
    let v = s.run("scene.set", &json!({"dem": dem, "exaggeration": 2})).unwrap();
    // Over the DEM's extent by default, about 3.2 × 4.5 km... at 45° N, 0.04° a side.
    let size = v["groundSize"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!((3_000.0..3_300.0).contains(&size[0]) && (4_300.0..4_600.0).contains(&size[1]), "{size:?}");
    assert_eq!(v["exaggeration"], 2.0);
    // A well layer finds its fields, whatever their case, and needs a depth.
    assert!(s.run("scene.layer", &json!({"layer": "บ่อ"})).unwrap_err().contains("depth field"));
    assert!(s.run("scene.layer", &json!({"layer": "บ่อ", "depth": "nope"})).unwrap_err().contains("no field 'nope'"));
    assert!(s.run("scene.layer", &json!({"layer": "บ่อ", "kind": "extrude", "height": "ลึก"})).unwrap_err().contains("of polygons"));
    let v = s.run("scene.layer", &json!({"layer": "บ่อ", "depth": "ลึก", "water": "น้ำ", "label": "ชื่อ", "color": "#c03030"})).unwrap();
    assert_eq!(
        v["layers"][0],
        json!({"layer": "บ่อ", "kind": "well", "depth": "ลึก", "water": "น้ำ", "label": "ชื่อ", "color": [192, 48, 48, 255], "heightM": 10.0})
    );
    // Changed again, the rest kept.
    let v = s.run("scene.layer", &json!({"layer": "บ่อ", "size": 30, "color": null})).unwrap();
    assert_eq!(v["layers"][0]["size"], 30.0);
    assert!(v["layers"][0].get("color").is_none() && v["layers"][0]["water"] == "น้ำ");
    // The camera keeps to its ranges, and resets.
    let c = s.run("scene.camera", &json!({"heading": 350, "turn": 20, "tilt": 120})).unwrap();
    assert_eq!((c["heading"].as_f64().unwrap().round(), c["tilt"].as_f64().unwrap()), (10.0, 120.0));
    assert!(s.run("scene.camera", &json!({"tilt": 200})).unwrap_err().contains("tilt"));
    assert!(s.run("scene.camera", &json!({"spin": 1})).unwrap_err().contains("no 'spin'"));
    let c = s.run("scene.camera", &json!({"reset": true})).unwrap();
    assert_eq!(c["heading"], 30.0);
    // Wrong settings are refused, the scene as it was.
    for (bad, said) in [
        (json!({"exaggeration": 0}), "exaggeration"),
        (json!({"groundOpacity": 2}), "groundOpacity"),
        (json!({"dem": "บ่อ"}), "isn't a raster layer"),
        (json!({"bbox": [10, 45, 10.0000001, 45.0000001]}), "degrees"),
        (json!({"extent": "view", "bbox": [10, 45, 11, 46]}), "one of"),
        (json!({"walls": "#12345678"}), "opaque"),
        (json!({"colour": "#ffffff"}), "no 'colour'"),
    ] {
        assert!(s.run("scene.set", &bad).unwrap_err().contains(said), "{bad}");
    }
    assert_eq!(s.map().scene.as_ref().unwrap().exaggeration, 2.0);
    // A layer taken out of the scene, then the scene off the map.
    let v = s.run("scene.layer", &json!({"layer": "บ่อ", "remove": true})).unwrap();
    assert!(v.get("layers").is_none());
    assert_eq!(s.run("scene.remove", &json!({})).unwrap()["removed"], true);
    assert!(s.map().scene.is_none());
}

#[test]
fn a_scene_follows_its_layers_renamed_and_is_saved_with_its_map() {
    let (mut s, dem) = with_wells();
    s.run("scene.set", &json!({"dem": dem})).unwrap();
    s.run("scene.layer", &json!({"layer": "บ่อ", "depth": "ลึก"})).unwrap();
    s.run("layer.rename", &json!({"layer": "บ่อ", "name": "บ่อบาดาล"})).unwrap();
    s.run("layer.rename", &json!({"layer": dem, "name": "ความสูง"})).unwrap();
    let scene = s.map().scene.clone().unwrap();
    assert_eq!((scene.dem.as_deref(), scene.layers[0].layer.as_str()), (Some("ความสูง"), "บ่อบาดาล"));
    // Saved, the project requires what keeps it; opened, it is there.
    let (text, _) = s.project.saved_text(None).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(doc["requires"].as_array().unwrap().iter().any(|r| r == "scene-3d"), "{}", doc["requires"]);
    let mut t = Session::new(crate::Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(t.map().scene.as_ref(), Some(&scene));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_scene_is_written_as_an_image_and_an_orbit() {
    let (mut s, dem) = with_wells();
    let dir = std::env::temp_dir().join(format!("rustgis-scene3d-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Not without a scene.
    let png = dir.join("ฉาก.png").display().to_string();
    assert!(s.run("scene.export", &json!({"path": png})).unwrap_err().contains("no 3D scene"));
    s.run("scene.set", &json!({"dem": dem})).unwrap();
    s.run("scene.layer", &json!({"layer": "บ่อ", "depth": "ลึก"})).unwrap();
    assert!(s.run("scene.export", &json!({"path": dir.join("a.jpg").display().to_string()})).unwrap_err().contains(".png"));
    assert!(s.run("scene.export", &json!({"path": png, "width": 10})).unwrap_err().contains("width"));
    let v = s.run("scene.export", &json!({"path": png, "width": 200, "height": 120, "smooth": 1})).unwrap();
    assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(200), Some(120)));
    let img = image::load_from_memory(&std::fs::read(&png).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (200, 120));
    // The well without a depth is said.
    assert!(
        s.notices.iter().any(|n| n.text.contains("1 wells of บ่อ have no depth")),
        "{:?}",
        s.notices.iter().map(|n| &n.text).collect::<Vec<_>>()
    );
    let gif = dir.join("orbit.gif").display().to_string();
    let v = s.run("scene.exportOrbit", &json!({"path": gif, "frames": 4, "width": 96, "height": 64})).unwrap();
    assert_eq!(v["frames"], 4);
    assert!(std::fs::read(&gif).unwrap().starts_with(b"GIF89a"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A saved project's scene is read back whole (the schema fixture `scene_3d.rgis`), its
/// layers as they were named though the map doesn't hold them: drawing leaves them out, said.
#[test]
fn a_saved_scene_is_read_back() {
    let p = crate::Project::from_json(include_str!("../../../../schema/fixtures/scene_3d.rgis")).unwrap();
    let scene = p.maps[0].scene.clone().unwrap();
    assert_eq!((scene.dem.as_deref(), scene.exaggeration, scene.ground_opacity, scene.depth), (Some("dem"), 2.0, 0.5, Some(120.0)));
    assert_eq!(scene.layers.iter().map(|l| l.kind.code()).collect::<Vec<_>>(), ["well", "extrude", "point"]);
    assert_eq!(scene.camera.tilt, 110.0);
    assert!(!scene.ruler, "its ruler turned off");
    assert_eq!(scene.depth_exaggeration, 3.0, "what is under the ground stretched");
    assert_eq!(scene.ruler_step, Some(25.0), "its ruler marked every 25 m");
    assert_eq!(
        (scene.ruler_min, scene.ruler_max, scene.ruler_from),
        (Some(-200.0), Some(1500.0), Some(10.0)),
        "from -200 m to 1,500 m, counted from 10 m"
    );
    assert_eq!((scene.grid, scene.grid_step, scene.grid_z_step), (true, Some(500.0), Some(25.5)), "its grid, every 500 m and 25.5 m");
    // Saved again, it requires what the fixture says.
    let mut again = crate::Project::from_json(include_str!("../../../../schema/fixtures/scene_3d.rgis")).unwrap();
    let (text, _) = again.saved_text(None).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(doc["requires"].as_array().is_some_and(|r| r.contains(&json!("scene-grid"))), "{}", doc["requires"]);
    assert!(scene.check().is_ok());
}

/// A saved scene that can't be drawn (a hand-edited file) is left out on reading, and said.
#[test]
fn a_saved_scene_that_cant_be_drawn_is_left_out_and_said() {
    let mut doc: serde_json::Value = serde_json::from_str(include_str!("../../../../schema/fixtures/scene_3d.rgis")).unwrap();
    doc["maps"][0]["scene"]["exaggeration"] = json!(-2);
    let p = crate::Project::from_json(&doc.to_string()).unwrap();
    assert!(p.maps[0].scene.is_none());
    let said: Vec<String> = p.read_notices.iter().map(|(_, n)| n.english()).collect();
    assert!(said.iter().any(|n| n.contains("its 3D scene can't be drawn") && n.contains("exaggeration")), "{said:?}");
    // A good one says nothing.
    let p = crate::Project::from_json(include_str!("../../../../schema/fixtures/scene_3d.rgis")).unwrap();
    assert!(!p.read_notices.iter().any(|(_, n)| n.english().contains("3D scene")));
}

/// A scene over the map's view is placed in Web Mercator whatever the map is drawn in.
#[test]
fn a_scene_over_the_view_of_a_projected_map_is_where_the_view_is() {
    let mut s = world();
    s.view_size = (800.0, 600.0);
    s.run("map.goToXY", &json!({"lon": 100.5, "lat": 13.75, "scale": 200000})).unwrap();
    s.run("map.viewMode", &json!({"mode": "local"})).unwrap();
    assert_ne!(s.map().projection, rustgis_map::Projection::WebMercator);
    let v = s.run("scene.set", &json!({"extent": "view"})).unwrap();
    let e: Vec<f64> = v["extent"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    let mid = rustgis_geom::merc_to_lonlat(rustgis_geom::Coord::new((e[0] + e[2]) / 2.0, (e[1] + e[3]) / 2.0));
    assert!((mid.x - 100.5).abs() < 0.05 && (mid.y - 13.75).abs() < 0.05, "{mid:?} from {e:?}");
}

/// A streamed layer whose source can't be read for the scene's export has its failure shown on
/// the live layer, as Export Map's does.
#[test]
fn a_scene_export_marks_a_streamed_layer_it_couldnt_read() {
    let (mut s, dem) = with_wells();
    let dir = std::env::temp_dir().join(format!("rustgis-scene3d-gone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    s.run("scene.set", &json!({"dem": dem, "extent": "dem"})).unwrap();
    s.run("scene.layer", &json!({"layer": "บ่อ", "depth": "ลึก"})).unwrap();
    let li = s.map().layers.iter().position(|l| l.name == "บ่อ").unwrap();
    let mut src = rustgis_map::source::SourceLayer::default();
    src.path = Some(dir.join("gone.fgb").display().to_string());
    s.map_mut().layers[li].source = Some(src);
    let png = dir.join("ฉาก.png").display().to_string();
    s.run("scene.export", &json!({"path": png, "width": 120, "height": 80, "smooth": 1})).unwrap();
    // The OS's own words for a missing file differ (Windows: "cannot find the file").
    let error = s.map().layers[li].source.as_ref().and_then(|src| src.error.clone());
    assert!(error.is_some_and(|e| !e.is_empty()), "the failure shows on the layer");
    let said: Vec<&String> = s.notices.iter().map(|n| &n.text).collect();
    assert!(said.iter().any(|n| n.contains("บ่อ may be missing from the scene")), "{said:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A study area over a streamed layer's extent takes the whole of its source, not the part read.
#[test]
fn a_scene_over_a_streamed_layers_extent_takes_its_whole_source() {
    use rustgis_data::source::{Batch, FeatureSource, Request, SourceInfo};
    struct Nothing(SourceInfo);
    impl FeatureSource for Nothing {
        fn info(&self) -> &SourceInfo {
            &self.0
        }
        fn read(&self, _: &Request) -> rustgis_data::Result<Batch> {
            Ok(Batch::default())
        }
    }
    let (mut s, _) = with_wells();
    let li = s.map().layers.iter().position(|l| l.name == "บ่อ").unwrap();
    let ds = s.map().layers[li].dataset.clone();
    let info = SourceInfo {
        name: ds.name.clone(),
        format: "test",
        geometry_type: ds.geometry_type,
        fields: ds.fields.clone(),
        extent: rustgis_geom::Extent::new(10.0, 45.0, 10.2, 45.1),
        count: Some(1000),
        crs: "EPSG:4326".into(),
        indexed: true,
        notes: Vec::new(),
        zm_dropped: false,
    };
    // Nothing read of it yet.
    s.map_mut().layers[li].dataset.features = Vec::new().into();
    s.map_mut().layers[li].source = Some(rustgis_map::source::SourceLayer::from_path("บ่อ.fgb", None, Box::new(Nothing(info))));
    let v = s.run("scene.set", &json!({"extentOf": "บ่อ"})).unwrap();
    let e: Vec<f64> = v["extent"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    let (a, c) = (
        rustgis_geom::merc_to_lonlat(rustgis_geom::Coord::new(e[0], e[1])),
        rustgis_geom::merc_to_lonlat(rustgis_geom::Coord::new(e[2], e[3])),
    );
    assert!(a.x < 10.0 && a.y < 45.0 && c.x > 10.2 && c.y > 45.1, "{a:?} {c:?}");
}

/// A scene's ruler of heights: on by default, turned off and on by `scene.set`, kept with the
/// map (only when off: a scene saved before it had one shows it), and drawn on an exported image
/// as on the screen.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_scene_ruler_is_set_kept_and_exported() {
    let (mut s, dem) = with_wells();
    let dir = std::env::temp_dir().join(format!("rustgis-scene3d-ruler-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let v = s
        .run("scene.set", &json!({"dem": dem, "exaggeration": 5, "depth": 1500, "camera": {"heading": 30, "tilt": 75, "zoom": 1.5}}))
        .unwrap();
    assert_eq!(v["ruler"], true);
    let shot = |s: &mut Session, name: &str| {
        let p = dir.join(name).display().to_string();
        s.run("scene.export", &json!({"path": p, "width": 400, "height": 300, "smooth": 1})).unwrap();
        image::load_from_memory(&std::fs::read(&p).unwrap()).unwrap().to_rgba8().into_raw()
    };
    let on = shot(&mut s, "on.png");
    assert!(s.run("scene.set", &json!({"ruler": "no"})).unwrap_err().contains("true or false"));
    assert_eq!(s.run("scene.set", &json!({"ruler": false})).unwrap()["ruler"], false);
    let off = shot(&mut s, "off.png");
    let differ = on.chunks(4).zip(off.chunks(4)).filter(|(a, b)| a != b).count();
    assert!(differ > 100, "the ruler is on the image: {differ} pixels");
    let (text, _) = s.project.saved_text(None).unwrap();
    assert!(text.contains("\"ruler\":false"));
    // Off, the project and a map file require what keeps it: 0.7.1 knew scenes but not their
    // ruler, and would save the scene with it on again.
    let requires = |text: &str| -> Vec<String> {
        let doc: serde_json::Value = serde_json::from_str(text).unwrap();
        doc["requires"].as_array().map(|r| r.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    assert!(requires(&text).contains(&"scene-ruler".to_string()), "{:?}", requires(&text));
    let map_file = crate::project::map_file_bytes(s.map()).unwrap();
    assert!(requires(std::str::from_utf8(&map_file).unwrap()).contains(&"scene-ruler".to_string()));
    assert!(crate::project::CAPABILITIES.contains(&"scene-ruler"), "this version reads what it writes");
    let mut t = Session::new(crate::Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    assert!(!t.map().scene.as_ref().unwrap().ruler);
    s.run("scene.set", &json!({"ruler": true})).unwrap();
    let (text, _) = s.project.saved_text(None).unwrap();
    assert!(!text.contains("\"ruler\""), "on, as a scene saved before it, it isn't written");
    assert!(!requires(&text).contains(&"scene-ruler".to_string()) && requires(&text).contains(&"scene-3d".to_string()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// What is under the lowest ground stretched: the block drawn deeper, its depth still the
/// metres asked; saved, a project and a map file require what keeps it (0.7.2 would draw it in
/// true metres and save it so).
#[test]
fn what_is_under_the_ground_is_stretched_and_kept() {
    let (mut s, dem) = with_wells();
    s.run("scene.set", &json!({"dem": dem, "exaggeration": 2, "depth": 200})).unwrap();
    for bad in [json!(0), json!(51), json!("two")] {
        assert!(s.run("scene.set", &json!({"depthExaggeration": bad})).unwrap_err().contains("depthExaggeration"));
    }
    let v = s.run("scene.set", &json!({"depthExaggeration": 4})).unwrap();
    assert_eq!((v["depthExaggeration"].as_f64(), v["depth"].as_f64()), (Some(4.0), Some(200.0)));
    let (text, _) = s.project.saved_text(None).unwrap();
    let requires = |text: &str| -> Vec<String> {
        let doc: serde_json::Value = serde_json::from_str(text).unwrap();
        doc["requires"].as_array().map(|r| r.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    assert!(requires(&text).contains(&"scene-depth-exaggeration".to_string()), "{:?}", requires(&text));
    let map_file = crate::project::map_file_bytes(s.map()).unwrap();
    assert!(requires(std::str::from_utf8(&map_file).unwrap()).contains(&"scene-depth-exaggeration".to_string()));
    assert!(crate::project::CAPABILITIES.contains(&"scene-depth-exaggeration"), "this version reads what it writes");
    let mut t = Session::new(crate::Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(t.map().scene.as_ref().unwrap().depth_exaggeration, 4.0);
    // Back to true metres: not written, nor required.
    s.run("scene.set", &json!({"depthExaggeration": 1})).unwrap();
    let (text, _) = s.project.saved_text(None).unwrap();
    assert!(!text.contains("depthExaggeration") && !requires(&text).contains(&"scene-depth-exaggeration".to_string()));
}

/// The ruler marked at a step asked, and the block's colour: set, saved, read back; the step
/// requires what keeps it (0.7.3 would mark its own and save the scene without it). Null is
/// automatic again, and nothing is written for it.
#[test]
fn the_rulers_step_and_the_blocks_colour_are_set_and_kept() {
    let (mut s, dem) = with_wells();
    s.run("scene.set", &json!({"dem": dem})).unwrap();
    for bad in [json!(0), json!(-5), json!(30_000), json!("ten")] {
        assert!(s.run("scene.set", &json!({"rulerStep": bad})).unwrap_err().contains("rulerStep"), "{bad}");
    }
    assert!(s.run("scene.set", &json!({"walls": "#11223380"})).unwrap_err().contains("opaque"));
    let v = s.run("scene.set", &json!({"rulerStep": 25, "walls": "#6B4F3A"})).unwrap();
    assert_eq!(v["rulerStep"].as_f64(), Some(25.0));
    let (text, _) = s.project.saved_text(None).unwrap();
    let requires = |text: &str| -> Vec<String> {
        let doc: serde_json::Value = serde_json::from_str(text).unwrap();
        doc["requires"].as_array().map(|r| r.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    assert!(requires(&text).contains(&"scene-ruler-step".to_string()), "{:?}", requires(&text));
    let map_file = crate::project::map_file_bytes(s.map()).unwrap();
    assert!(requires(std::str::from_utf8(&map_file).unwrap()).contains(&"scene-ruler-step".to_string()));
    assert!(crate::project::CAPABILITIES.contains(&"scene-ruler-step"), "this version reads what it writes");
    let mut t = Session::new(crate::Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let back = t.map().scene.clone().unwrap();
    assert_eq!((back.ruler_step, back.walls), (Some(25.0), rustgis_map::Rgba::rgb(0x6B, 0x4F, 0x3A)));
    // Undone, the scene is as it was.
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.map().scene.as_ref().unwrap().ruler_step, None);
    s.run("edit.redo", &json!({})).unwrap();
    s.run("scene.set", &json!({"rulerStep": null})).unwrap();
    let (text, _) = s.project.saved_text(None).unwrap();
    assert!(!text.contains("rulerStep") && !requires(&text).contains(&"scene-ruler-step".to_string()));
}

/// The ruler between heights asked and counted from one: set, saved, read back, required (0.7.4
/// would stand it on the block and count from 0 m); a foot not below its top is refused, and
/// null is automatic again, nothing written for it.
#[test]
fn the_rulers_heights_are_set_and_kept() {
    let (mut s, dem) = with_wells();
    s.run("scene.set", &json!({"dem": dem})).unwrap();
    for key in ["rulerMin", "rulerMax", "rulerFrom"] {
        for bad in [json!(25_000), json!(-25_000), json!("high")] {
            assert!(s.run("scene.set", &json!({key: bad})).unwrap_err().contains(key), "{key} {bad}");
        }
    }
    assert!(s.run("scene.set", &json!({"rulerMin": 900, "rulerMax": 400})).unwrap_err().contains("below"));
    assert!(s.map().scene.as_ref().unwrap().ruler_min.is_none(), "nothing set by a refused change");
    let v = s.run("scene.set", &json!({"rulerMin": -150, "rulerMax": 1200.5, "rulerFrom": 37})).unwrap();
    assert_eq!((v["rulerMin"].as_f64(), v["rulerMax"].as_f64(), v["rulerFrom"].as_f64()), (Some(-150.0), Some(1200.5), Some(37.0)));
    let (text, _) = s.project.saved_text(None).unwrap();
    let requires = |text: &str| -> Vec<String> {
        let doc: serde_json::Value = serde_json::from_str(text).unwrap();
        doc["requires"].as_array().map(|r| r.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    assert!(requires(&text).contains(&"scene-ruler-range".to_string()), "{:?}", requires(&text));
    let map_file = crate::project::map_file_bytes(s.map()).unwrap();
    assert!(requires(std::str::from_utf8(&map_file).unwrap()).contains(&"scene-ruler-range".to_string()));
    assert!(crate::project::CAPABILITIES.contains(&"scene-ruler-range"), "this version reads what it writes");
    let mut t = Session::new(crate::Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let back = t.map().scene.clone().unwrap();
    assert_eq!((back.ruler_min, back.ruler_max, back.ruler_from), (Some(-150.0), Some(1200.5), Some(37.0)));
    // Undone, the scene is as it was.
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.map().scene.as_ref().unwrap().ruler_max, None);
    s.run("edit.redo", &json!({})).unwrap();
    // One alone is enough to require it.
    s.run("scene.set", &json!({"rulerMin": null, "rulerMax": null})).unwrap();
    let (text, _) = s.project.saved_text(None).unwrap();
    assert!(requires(&text).contains(&"scene-ruler-range".to_string()));
    s.run("scene.set", &json!({"rulerFrom": null})).unwrap();
    let (text, _) = s.project.saved_text(None).unwrap();
    assert!(!text.contains("rulerFrom") && !text.contains("rulerMin") && !requires(&text).contains(&"scene-ruler-range".to_string()));
}

/// The scene's grid of eastings, northings and heights: off by default, turned on with its
/// steps in range or automatic, kept in the project (which then requires `scene-grid`), undone,
/// and drawn on an exported picture.
#[test]
fn the_scenes_grid_is_set_kept_and_exported() {
    let (mut s, dem) = with_wells();
    s.run("scene.set", &json!({"dem": dem})).unwrap();
    assert!(!s.map().scene.as_ref().unwrap().grid, "off by default");
    for (key, bad) in [("gridStep", json!(0.5)), ("gridStep", json!(3_000_000)), ("gridZStep", json!(0.01)), ("gridZStep", json!("tall"))] {
        assert!(s.run("scene.set", &json!({key: bad})).unwrap_err().contains(key), "{key} {bad}");
    }
    assert!(s.run("scene.set", &json!({"grid": "yes"})).unwrap_err().contains("grid is true or false"));
    let v = s.run("scene.set", &json!({"grid": true, "gridStep": 500, "gridZStep": 25.5})).unwrap();
    assert_eq!((v["grid"].as_bool(), v["gridStep"].as_f64(), v["gridZStep"].as_f64()), (Some(true), Some(500.0), Some(25.5)));
    let requires = |s: &mut Session| -> Vec<String> {
        let (text, _) = s.project.saved_text(None).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        doc["requires"].as_array().map(|r| r.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    assert!(requires(&mut s).contains(&"scene-grid".to_string()), "{:?}", requires(&mut s));
    assert!(crate::project::CAPABILITIES.contains(&"scene-grid"), "this version reads what it writes");
    let (text, _) = s.project.saved_text(None).unwrap();
    let mut t = Session::new(crate::Project::default());
    t.run("project.openText", &json!({"text": text})).unwrap();
    let back = t.map().scene.clone().unwrap();
    assert_eq!((back.grid, back.grid_step, back.grid_z_step), (true, Some(500.0), Some(25.5)));
    // Drawn on an export: the picture differs from one without it.
    let dir = std::env::temp_dir().join(format!("arutgis-scene-grid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let export = |s: &mut Session, name: &str| {
        let path = dir.join(name);
        s.run("scene.export", &json!({"path": path.display().to_string(), "width": 320, "height": 240})).unwrap();
        std::fs::read(path).unwrap()
    };
    let with = export(&mut s, "with.png");
    s.run("edit.undo", &json!({})).unwrap();
    assert!(!s.map().scene.as_ref().unwrap().grid, "undone");
    assert!(!requires(&mut s).contains(&"scene-grid".to_string()));
    let without = export(&mut s, "without.png");
    assert_ne!(with, without, "the grid is drawn on the picture");
    // Automatic steps given back, then off: nothing of it saved.
    s.run("edit.redo", &json!({})).unwrap();
    s.run("scene.set", &json!({"grid": false, "gridStep": null, "gridZStep": null})).unwrap();
    let (text, _) = s.project.saved_text(None).unwrap();
    assert!(!text.contains("gridStep") && !text.contains("\"grid\"") && !requires(&mut s).contains(&"scene-grid".to_string()));
    std::fs::remove_dir_all(&dir).ok();
}
