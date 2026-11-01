//! Google Open Buildings 2.5D Temporal: building heights from satellite imagery, a year at a time
//! (2016–2023), over Africa, South and South-East Asia, Latin America and the Caribbean
//! (https://sites.research.google/gr/open-buildings/temporal, CC BY 4.0).
//!
//! The heights are GeoTIFFs of 12.5 km squares in UTM, 0.5 m pixels with overviews, three bands
//! (fractional building count, building height in metres above the ground, building presence),
//! in Google's public bucket. The bucket's manifests (one per S2 cell, UTM zone and year) list
//! every file with its place, so an area needs the manifests of its UTM zones, then the files
//! that cover it, read in place at about 4 m a pixel (an overview), only the window over the
//! area. A building's height is then read from the pixels inside its outline.
//!
//! The bucket answers no browser's cross-origin requests, so this is for the desktop app and the
//! command line.

use serde_json::Value;

/// The dataset's files.
pub const BUCKET: &str = "https://storage.googleapis.com/open-buildings-temporal-data/";
/// The manifests' folder in the bucket.
const MANIFESTS: &str = "v1/manifests/";
/// The files' folder, which a manifest's `uriPrefix` (`gs://open-buildings-temporal-data/v1/geotiffs/3`)
/// names as a `gs://` address.
const GS: &str = "gs://open-buildings-temporal-data/";
/// The latest year.
pub const YEAR: u16 = 2023;
/// The pixel size read (metres): the overview nearest it.
pub const CELL_M: f64 = 4.0;
/// The band of building heights (0-based).
pub const HEIGHT_BAND: usize = 1;
/// The most files an area reads (a 3 km square meets four at most).
pub const MAX_FILES: usize = 6;
/// The most manifests and files a manifest may list.
const MAX_LISTED: usize = 50_000;
/// Heights below this (metres) aren't a building's.
const MIN_HEIGHT: f32 = 1.0;
/// The credit the licence asks for.
pub const ATTRIBUTION: &str = "Google Open Buildings 2.5D Temporal (CC BY 4.0)";

/// The bucket's listing of manifests, a page at a time (the JSON API).
pub fn list_url(page: Option<&str>) -> String {
    let mut u = format!(
        "https://storage.googleapis.com/storage/v1/b/open-buildings-temporal-data/o?prefix={MANIFESTS}&maxResults=1000&fields=nextPageToken,items(name)"
    );
    if let Some(p) = page.filter(|p| !p.is_empty() && p.len() < 1024 && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_=".contains(&b)))
    {
        u.push_str("&pageToken=");
        u.push_str(p);
    }
    u
}

/// A page of the listing: the manifests' names (`31_EPSG_32647_2023_06_30.json`) and the next
/// page's token.
pub fn parse_list(bytes: &[u8]) -> Result<(Vec<String>, Option<String>), String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| "Google's listing of the 2.5D manifests isn't JSON".to_string())?;
    let names = v
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_LISTED)
        .filter_map(|i| i.get("name")?.as_str()?.strip_prefix(MANIFESTS).map(str::to_string))
        .filter(|n| n.ends_with(".json"))
        .collect();
    Ok((names, v.get("nextPageToken").and_then(Value::as_str).map(str::to_string)))
}

/// A manifest's UTM zone (as its EPSG code) and year, from its name.
pub fn manifest_key(name: &str) -> Option<(u32, u16)> {
    let mut parts = name.trim_end_matches(".json").split('_');
    let _cell = parts.next()?;
    (parts.next()? == "EPSG").then_some(())?;
    let epsg: u32 = parts.next()?.parse().ok()?;
    let year: u16 = parts.next()?.parse().ok()?;
    ((32601..=32660).contains(&epsg) || (32701..=32760).contains(&epsg)).then_some((epsg, year))
}

/// A manifest's address.
pub fn manifest_url(name: &str) -> String {
    format!("{BUCKET}{MANIFESTS}{name}")
}

/// The UTM zones (as EPSG codes) an area (west, south, east, north) meets.
pub fn utm_zones(area: [f64; 4]) -> Vec<u32> {
    let [w, s, e, n] = area;
    let zone = |lon: f64| ((((lon + 180.0) / 6.0).floor() as i64).clamp(0, 59) + 1) as u32;
    let mut out = Vec::new();
    for z in zone(w)..=zone(e.max(w)) {
        if n >= 0.0 {
            out.push(32600 + z);
        }
        if s < 0.0 {
            out.push(32700 + z);
        }
    }
    out
}

/// The manifests (names) for an area's zones in a year.
pub fn manifests_for(names: &[String], area: [f64; 4], year: u16) -> Vec<String> {
    let zones = utm_zones(area);
    names.iter().filter(|n| manifest_key(n).is_some_and(|(z, y)| y == year && zones.contains(&z))).cloned().collect()
}

/// One of a manifest's files: its address and its extent in its UTM zone (min x, min y, max x,
/// max y, metres).
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub url: String,
    pub epsg: u32,
    pub extent: [f64; 4],
}

/// A manifest's files.
pub fn parse_manifest(bytes: &[u8]) -> Result<Vec<File>, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| "a Google 2.5D manifest isn't JSON".to_string())?;
    let prefix = v.get("uriPrefix").and_then(Value::as_str).ok_or("the manifest has no uriPrefix")?;
    // Google's bucket only, as a web address.
    let prefix = prefix.strip_prefix(GS).ok_or("the manifest's files aren't in Google's 2.5D bucket")?;
    if prefix.contains("..") || prefix.contains(['?', '#', ' ']) {
        return Err("the manifest's uriPrefix isn't a folder of the bucket".into());
    }
    let mut out = Vec::new();
    for set in v.get("tilesets").and_then(Value::as_array).into_iter().flatten().take(16) {
        let epsg: u32 = set
            .get("crs")
            .and_then(Value::as_str)
            .and_then(|c| c.strip_prefix("EPSG:"))
            .and_then(|c| c.parse().ok())
            .filter(|c| (32601..=32660).contains(c) || (32701..=32760).contains(c))
            .ok_or("the manifest's files aren't in a UTM zone")?;
        for src in set.get("sources").and_then(Value::as_array).into_iter().flatten().take(MAX_LISTED) {
            let Some(uri) = src.pointer("/uris/0").and_then(Value::as_str) else { continue };
            if uri.contains("..") || uri.starts_with('/') || uri.contains(['?', '#', ' ']) || !uri.ends_with(".tif") {
                continue;
            }
            let num = |p: &str| src.pointer(p).and_then(Value::as_f64).filter(|x| x.is_finite());
            let (Some(x0), Some(y0), Some(sx), Some(sy), Some(w), Some(h)) = (
                num("/affineTransform/translateX"),
                num("/affineTransform/translateY"),
                num("/affineTransform/scaleX"),
                num("/affineTransform/scaleY"),
                num("/dimensions/width"),
                num("/dimensions/height"),
            ) else {
                continue;
            };
            let (x1, y1) = (x0 + sx * w, y0 + sy * h);
            out.push(File { url: format!("{BUCKET}{prefix}{uri}"), epsg, extent: [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)] });
        }
    }
    Ok(out)
}

/// An area (west, south, east, north) in a UTM zone: the box of its corners and edge midpoints.
fn utm_box(area: [f64; 4], epsg: u32) -> Option<[f64; 4]> {
    let t = rustgis_data::raster::Crs::from_epsg(epsg)?.transformer().ok()?;
    let [w, s, e, n] = area;
    let (mx, my) = ((w + e) / 2.0, (s + n) / 2.0);
    let pts: Vec<_> = [(w, s), (e, s), (w, n), (e, n), (mx, s), (mx, n), (w, my), (e, my)]
        .into_iter()
        .filter_map(|(x, y)| t.from_lonlat(rustgis_geom::Coord::new(x, y)))
        .collect();
    if pts.is_empty() {
        return None;
    }
    let f = |g: fn(f64, f64) -> f64, k: fn(&rustgis_geom::Coord) -> f64, init: f64| pts.iter().map(k).fold(init, g);
    Some([
        f(f64::min, |c| c.x, f64::INFINITY),
        f(f64::min, |c| c.y, f64::INFINITY),
        f(f64::max, |c| c.x, f64::NEG_INFINITY),
        f(f64::max, |c| c.y, f64::NEG_INFINITY),
    ])
}

/// The files covering an area.
pub fn files_in(files: &[File], area: [f64; 4]) -> Vec<File> {
    let mut boxes = std::collections::HashMap::new();
    files
        .iter()
        .filter(|f| {
            let b = *boxes.entry(f.epsg).or_insert_with(|| utm_box(area, f.epsg));
            b.is_some_and(|[x0, y0, x1, y1]| f.extent[0] <= x1 && f.extent[2] >= x0 && f.extent[1] <= y1 && f.extent[3] >= y0)
        })
        .cloned()
        .collect()
}

/// Heights read over an area from one file: a grid in its UTM zone, row by row from the top.
#[derive(Clone, Debug)]
pub struct Grid {
    pub epsg: u32,
    /// The top-left corner (metres).
    pub x0: f64,
    pub y0: f64,
    /// The pixel size (metres).
    pub cell: f64,
    pub width: usize,
    pub height: usize,
    /// Heights in metres; NaN where none.
    pub values: Vec<f32>,
}

/// Whether a point is inside a ring (even-odd).
fn inside(ring: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut c = false;
    let n = ring.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let ((xi, yi), (xj, yj)) = (ring[i], ring[j % n]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            c = !c;
        }
        j = i;
    }
    c
}

/// A building's height (metres) from the grids, from the pixels whose centres are inside its outline
/// (longitude/latitude, the outer ring): the 75th percentile of the building heights there; else
/// the pixel under its middle, for an outline smaller than a pixel. `None` where the grids have no
/// building there.
pub fn height(grids: &[Grid], outline: &[(f64, f64)]) -> Option<f64> {
    if outline.len() < 3 {
        return None;
    }
    let mut found: Vec<f32> = Vec::new();
    for g in grids {
        let Some(t) = rustgis_data::raster::Crs::from_epsg(g.epsg).and_then(|c| c.transformer().ok()) else { continue };
        let ring: Vec<(f64, f64)> =
            outline.iter().filter_map(|&(x, y)| t.from_lonlat(rustgis_geom::Coord::new(x, y))).map(|c| (c.x, c.y)).collect();
        if ring.len() < 3 {
            continue;
        }
        let (minx, maxx) = ring.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.0), b.max(p.0)));
        let (miny, maxy) = ring.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.1), b.max(p.1)));
        let col = |x: f64| (x - g.x0) / g.cell;
        let row = |y: f64| (g.y0 - y) / g.cell;
        let (c0, c1) = (col(minx).floor().max(0.0) as usize, (col(maxx).ceil().max(0.0) as usize).min(g.width));
        let (r0, r1) = (row(maxy).floor().max(0.0) as usize, (row(miny).ceil().max(0.0) as usize).min(g.height));
        let at = |c: usize, r: usize| g.values.get(r * g.width + c).copied().filter(|v| v.is_finite() && *v >= MIN_HEIGHT);
        let mut here: Vec<f32> = Vec::new();
        for r in r0..r1 {
            for c in c0..c1 {
                let (x, y) = (g.x0 + (c as f64 + 0.5) * g.cell, g.y0 - (r as f64 + 0.5) * g.cell);
                if inside(&ring, x, y)
                    && let Some(v) = at(c, r)
                {
                    here.push(v);
                }
            }
        }
        if here.is_empty() {
            // An outline smaller than a pixel: the pixel under its middle.
            let (mx, my) = ((minx + maxx) / 2.0, (miny + maxy) / 2.0);
            let (c, r) = (col(mx), row(my));
            if c >= 0.0 && r >= 0.0 && (c as usize) < g.width && (r as usize) < g.height {
                here.extend(at(c as usize, r as usize));
            }
        }
        found.extend(here);
    }
    if found.is_empty() {
        return None;
    }
    found.sort_by(f32::total_cmp);
    let p75 = found[((found.len() - 1) as f64 * 0.75).round() as usize];
    Some((f64::from(p75) * 10.0).round() / 10.0)
}

#[cfg(not(target_arch = "wasm32"))]
mod read {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    /// What was read, kept for the session: the manifests' names, the manifests' files, and the
    /// grids read for each area.
    #[derive(Default)]
    struct Kept {
        names: Option<Arc<Vec<String>>>,
        manifests: HashMap<String, Arc<Vec<File>>>,
        grids: HashMap<String, Arc<Vec<Grid>>>,
    }

    fn kept() -> &'static Mutex<Kept> {
        static K: OnceLock<Mutex<Kept>> = OnceLock::new();
        K.get_or_init(Mutex::default)
    }

    fn lock() -> std::sync::MutexGuard<'static, Kept> {
        kept().lock().unwrap_or_else(|e| e.into_inner())
    }

    fn names() -> Result<Arc<Vec<String>>, String> {
        if let Some(n) = lock().names.clone() {
            return Ok(n);
        }
        let mut all = Vec::new();
        let mut page: Option<String> = None;
        for _ in 0..20 {
            let (names, next) = parse_list(&crate::cache::download_blocking(&list_url(page.as_deref()))?)?;
            all.extend(names);
            match next {
                Some(n) => page = Some(n),
                None => break,
            }
        }
        let all = Arc::new(all);
        lock().names = Some(all.clone());
        Ok(all)
    }

    fn manifest(name: &str) -> Result<Arc<Vec<File>>, String> {
        if let Some(m) = lock().manifests.get(name).cloned() {
            return Ok(m);
        }
        let files = Arc::new(parse_manifest(&crate::cache::download_blocking(&manifest_url(name))?)?);
        lock().manifests.insert(name.to_string(), files.clone());
        Ok(files)
    }

    /// One file's heights over an area, at the overview nearest [`CELL_M`].
    fn grid(file: &File, area: [f64; 4]) -> Result<Option<Grid>, String> {
        let info = crate::cog::info_blocking(&file.url)?;
        let layout = &info.layout;
        let level = (0..layout.levels.len())
            .min_by(|&a, &b| {
                let size = |i: usize| (layout.transform_of(&layout.levels[i]).pixel_width.abs() - CELL_M).abs();
                size(a).total_cmp(&size(b))
            })
            .ok_or("the file has no images")?;
        let gt = layout.transform_of(&layout.levels[level]);
        let l = &layout.levels[level];
        let Some([x0, y0, x1, y1]) = utm_box(area, file.epsg) else { return Ok(None) };
        let (pw, ph) = (gt.pixel_width.abs().max(1e-9), gt.pixel_height.abs().max(1e-9));
        let c0 = ((x0 - gt.origin_x) / pw).floor().clamp(0.0, f64::from(l.width)) as u32;
        let c1 = ((x1 - gt.origin_x) / pw).ceil().clamp(0.0, f64::from(l.width)) as u32;
        let r0 = ((gt.origin_y - y1) / ph).floor().clamp(0.0, f64::from(l.height)) as u32;
        let r1 = ((gt.origin_y - y0) / ph).ceil().clamp(0.0, f64::from(l.height)) as u32;
        if c1 <= c0 || r1 <= r0 {
            return Ok(None);
        }
        let values = crate::cog::read_values_blocking(&info, &file.url, level, HEIGHT_BAND, (c0, c1), (r0, r1))?;
        Ok(Some(Grid {
            epsg: file.epsg,
            x0: gt.origin_x + f64::from(c0) * pw,
            y0: gt.origin_y - f64::from(r0) * ph,
            cell: pw,
            width: (c1 - c0) as usize,
            height: (r1 - r0) as usize,
            values,
        }))
    }

    /// The heights over an area (west, south, east, north) in a year, waiting for them; kept, so
    /// asking again for the same area answers at once. No grids where the dataset has no data.
    pub fn grids_blocking(area: [f64; 4], year: u16) -> Result<Arc<Vec<Grid>>, String> {
        let key = format!("{year}:{:?}", area);
        if let Some(g) = lock().grids.get(&key).cloned() {
            return Ok(g);
        }
        let names = names()?;
        let mut files = Vec::new();
        for m in manifests_for(&names, area, year) {
            files.extend(files_in(&manifest(&m)?, area));
        }
        if files.len() > MAX_FILES {
            return Err(format!("the area takes {} of Google's 2.5D files (up to {MAX_FILES}): zoom in", files.len()));
        }
        let mut grids = Vec::new();
        for f in &files {
            grids.extend(grid(f, area)?);
        }
        let grids = Arc::new(grids);
        lock().grids.insert(key, grids.clone());
        Ok(grids)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use read::grids_blocking;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_are_picked_by_zone_and_year() {
        let names: Vec<String> = [
            "31_EPSG_32647_2023_06_30.json",
            "31_EPSG_32648_2023_06_30.json",
            "31_EPSG_32647_2016_06_30.json",
            "01_EPSG_32723_2023_06_30.json",
            "x.json",
        ]
        .map(str::to_string)
        .to_vec();
        assert_eq!(manifest_key("31_EPSG_32647_2023_06_30.json"), Some((32647, 2023)));
        assert_eq!(manifest_key("x.json"), None);
        // Bangkok: zone 47 north.
        assert_eq!(manifests_for(&names, [100.5, 13.7, 100.53, 13.73], 2023), ["31_EPSG_32647_2023_06_30.json"]);
        // Across the 102° line: both zones.
        assert_eq!(utm_zones([101.9, 17.0, 102.1, 17.1]), [32647, 32648]);
        assert_eq!(utm_zones([-43.0, -23.0, -42.9, -22.9]), [32723]);
    }

    #[test]
    fn a_listing_and_a_manifest_are_read() {
        let (names, next) = parse_list(
            br#"{"nextPageToken": "abc", "items": [{"name": "v1/manifests/"}, {"name": "v1/manifests/31_EPSG_32647_2023_06_30.json"}]}"#,
        )
        .unwrap();
        assert_eq!((names, next.as_deref()), (vec!["31_EPSG_32647_2023_06_30.json".to_string()], Some("abc")));
        assert!(list_url(Some("abc")).ends_with("&pageToken=abc"));
        assert!(!list_url(Some("a&b")).contains("pageToken"));
        let m = br#"{"uriPrefix": "gs://open-buildings-temporal-data/v1/geotiffs/3", "tilesets": [{"crs": "EPSG:32647", "sources": [
            {"uris": ["0eec_2023_06_30/tile_a.tif"], "affineTransform": {"scaleX": 0.5, "translateX": 218112.0, "scaleY": -0.5, "translateY": 1579220.0}, "dimensions": {"width": 25000, "height": 25000}},
            {"uris": ["../x.tif"], "affineTransform": {"scaleX": 0.5, "translateX": 0, "scaleY": -0.5, "translateY": 0}, "dimensions": {"width": 1, "height": 1}}
        ]}]}"#;
        let files = parse_manifest(m).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].url, "https://storage.googleapis.com/open-buildings-temporal-data/v1/geotiffs/30eec_2023_06_30/tile_a.tif");
        assert_eq!(files[0].extent, [218112.0, 1566720.0, 230612.0, 1579220.0]);
        assert!(parse_manifest(br#"{"uriPrefix": "gs://elsewhere/x", "tilesets": []}"#).is_err());
        // The file covers 218–231 km E, 1567–1579 km N in zone 47: about 99.6°E, 14.2°N.
        let t = rustgis_data::raster::Crs::from_epsg(32647).unwrap().transformer().unwrap();
        let c = t.to_lonlat(rustgis_geom::Coord::new(224000.0, 1573000.0)).unwrap();
        assert_eq!(files_in(&files, [c.x - 0.01, c.y - 0.01, c.x + 0.01, c.y + 0.01]).len(), 1);
        assert!(files_in(&files, [100.5, 13.7, 100.53, 13.73]).is_empty());
    }

    #[test]
    fn a_buildings_height_is_read_from_the_pixels_inside_it() {
        let t = rustgis_data::raster::Crs::from_epsg(32647).unwrap().transformer().unwrap();
        // A 40 m × 40 m grid of 4 m pixels: 12 m everywhere, 30 m in a 12 m square in the middle.
        let (x0, y0) = (660000.0, 1520000.0);
        let mut values = vec![f32::NAN; 100];
        for r in 0..10 {
            for c in 0..10 {
                values[r * 10 + c] = if (3..6).contains(&r) && (3..6).contains(&c) { 30.0 } else { 12.0 };
            }
        }
        let g = Grid { epsg: 32647, x0, y0, cell: 4.0, width: 10, height: 10, values };
        let ll = |x: f64, y: f64| {
            let c = t.to_lonlat(rustgis_geom::Coord::new(x, y)).unwrap();
            (c.x, c.y)
        };
        let square = |a: f64, b: f64, size: f64| vec![ll(a, b), ll(a + size, b), ll(a + size, b - size), ll(a, b - size), ll(a, b)];
        // The tall square: 30 m.
        assert_eq!(height(&[g.clone()], &square(x0 + 12.0, y0 - 12.0, 12.0)), Some(30.0));
        // A corner of low pixels: 12 m.
        assert_eq!(height(&[g.clone()], &square(x0, y0, 8.0)), Some(12.0));
        // Smaller than a pixel: the pixel under it.
        assert_eq!(height(&[g.clone()], &square(x0 + 13.0, y0 - 13.0, 1.0)), Some(30.0));
        // Off the grid: none.
        assert_eq!(height(&[g], &square(x0 + 500.0, y0, 8.0)), None);
    }
}
