//! Thai map sheet numbers (เลขระวาง): the 1:50,000 sheet of the Royal Thai Survey Department and
//! the Department of Lands' cadastral UTM sheets at a point, going to a sheet by its number, and
//! a layer of the sheets in an area to map and label. Worked out by `rustgis_map::thai_sheets`;
//! nothing is downloaded.

use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Attr};
use rustgis_geom::{Coord, Extent};
use rustgis_map::thai_sheets::{self as ts, Datum, Scale, Sheet50k, UtmSheet};
use serde_json::{Value, json};

use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// The most sheets a grid layer holds.
pub const MAX_SHEETS: usize = 5000;

pub fn commands() -> Vec<C> {
    vec![
        C::new("map.thaiSheet", "Thai Map Sheet Number", "Home › Navigate › Locate", sheet_at).view().params(
            r#"{"lon"?: number, "lat"?: number (WGS 84; default the map's centre), "datum"?: "wgs84" (default: series L7018 and today's cadastral sheets) | "indian1975" (series L7017 and older cadastral sheets), "zone"?: 47 | 48 (the cadastral sheets' UTM zone; default 47 west of 102°E, 48 east of it)}"#,
        ),
        C::new("map.goToThaiSheet", "Go To Thai Map Sheet", "Home › Navigate › Locate", go_to_sheet).params(
            r#"{"sheet": "a 1:50,000 sheet (5541 I) or a Department of Lands sheet (5137 IV 6888, 5137 IV 6888-5)", "scale"?: 2000 | 1000 | 500 (a numbered part's scale; default 1000, 500 for parts past 16), "datum"?: "wgs84" | "indian1975", "zone"?: 47 | 48}"#,
        ),
        C::new("layer.thaiSheetGrid", "Thai Map Sheet Grid", "Home › Navigate › Locate", sheet_grid).params(
            r#"{"series": "50000" (the 1:50,000 topographic sheets) | "4000" | "2000" | "1000" | "500" (the Department of Lands' cadastral sheets), "bbox"?: [west, south, east, north] (WGS 84; default the map's extent), "datum"?: "wgs84" | "indian1975", "zone"?: 47 | 48, "name"?: text}"#,
        ),
    ]
}

fn datum(p: &Value) -> Result<Datum, String> {
    ps(p, "datum").map(Datum::parse).transpose().map(|d| d.unwrap_or(Datum::Wgs84))
}

fn zone(p: &Value) -> Result<Option<u8>, String> {
    match f(p, "zone") {
        None => Ok(None),
        Some(z) if z == 47.0 || z == 48.0 => Ok(Some(z as u8)),
        Some(z) => Err(format!("zone {z}: Thailand's UTM zones are 47 and 48")),
    }
}

/// The map's centre in WGS 84.
fn centre(s: &Session) -> Result<Coord, String> {
    let e = super::workspace::view_extent(s);
    if e.is_empty() {
        return Err("the map's extent isn't known; give lon and lat".into());
    }
    Ok(Coord::new((e.min_x + e.max_x) / 2.0, (e.min_y + e.max_y) / 2.0))
}

fn sheet50k_json(sh: &Sheet50k, d: Datum) -> Value {
    json!({"sheet": sh.label(), "series": d.series(), "inSeries": sh.in_series, "frame": [sh.west, sh.south, sh.east, sh.north]})
}

fn sheet_at(s: &mut Session, p: &Value) -> CmdResult {
    let c = match (f(p, "lon"), f(p, "lat")) {
        (Some(lon), Some(lat)) => Coord::new(lon, lat),
        (None, None) => centre(s)?,
        _ => return Err("give both lon and lat, or neither (the map's centre)".into()),
    };
    if !(-180.0..=180.0).contains(&c.x) || !(-90.0..=90.0).contains(&c.y) {
        return Err("longitude must be within ±180 and latitude within ±90".into());
    }
    let d = datum(p)?;
    let z = zone(p)?.unwrap_or_else(|| ts::zone_of(c.x));
    let on = ts::to_datum(c, d).ok_or("the point can't be put on that datum")?;
    let fifty = ts::sheet_50k(on);
    let m = ts::to_utm(c, d, z).ok_or("the point is outside the UTM zone")?;
    let mut cadastral = serde_json::Map::new();
    for scale in Scale::ALL {
        if let Some(u) = ts::utm_sheet(c, d, scale, Some(z)) {
            cadastral.insert(scale.label().to_string(), json!(u.label()));
        }
    }
    Ok(json!({
        "lon": c.x, "lat": c.y, "datum": d.code(),
        "sheet50k": sheet50k_json(&fifty, d),
        "zone": z, "easting": (m.x * 100.0).round() / 100.0, "northing": (m.y * 100.0).round() / 100.0,
        "cadastral": cadastral,
        // Outside the series, the regular grid's number names no printed sheet.
        "note": if fifty.in_series { Value::Null } else { json!(format!("no {} sheet is printed here (sea, or another country): the number is the grid's", d.series())) },
    }))
}

/// A sheet number read: the 1:50,000 sheet, and the cadastral sheet's `EENN` and part.
#[derive(Debug, PartialEq)]
struct Asked {
    block: u16,
    quarter: u8,
    square: Option<(u32, u32)>,
    part: Option<u32>,
    /// A scale written with it, `(1:1000)`.
    scale: Option<Scale>,
}

fn parse_sheet(text: &str) -> Result<Asked, String> {
    let bad = || format!("'{text}': a sheet is like 5541 I, 5137 IV 6888 or 5137 IV 6888-5");
    // A scale in brackets, `(1:1,000)` or `(๑ : ๑,๐๐๐)`, with Thai digits as Arabic ones.
    let arabic: String =
        text.chars().map(|c| if ('๐'..='๙').contains(&c) { char::from(b'0' + (c as u32 - '๐' as u32) as u8) } else { c }).collect();
    let (body, scale) = match arabic.split_once('(') {
        Some((b, rest)) => (b.to_string(), Some(Scale::parse(rest.trim_end_matches(')').trim()).map_err(|_| bad())?)),
        None => (arabic.clone(), None),
    };
    let up = body.to_ascii_uppercase().replace(['-', '–'], " - ");
    let mut words = up.split_whitespace().peekable();
    let block: u16 = words.next().filter(|w| w.len() == 4).and_then(|w| w.parse().ok()).ok_or_else(bad)?;
    let quarter = match words.next().ok_or_else(bad)? {
        "I" | "1" => 1,
        "II" | "2" => 2,
        "III" | "3" => 3,
        "IV" | "4" => 4,
        _ => return Err(bad()),
    };
    let square = match words.next() {
        None => None,
        Some(w) if w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()) => {
            let (e, n) = (w[..2].parse::<u32>().map_err(|_| bad())?, w[2..].parse::<u32>().map_err(|_| bad())?);
            if e % 2 != 0 || n % 2 != 0 {
                return Err(format!("'{text}': a 1:4,000 sheet's easting and northing are even kilometres"));
            }
            Some((e, n))
        }
        Some(_) => return Err(bad()),
    };
    let part = match words.next() {
        None => None,
        Some("-") if square.is_some() => {
            let k: u32 = words.next().and_then(|w| w.parse().ok()).ok_or_else(bad)?;
            if !(1..=64).contains(&k) {
                return Err(format!("'{text}': a sheet's part is 1 to 64"));
            }
            Some(k)
        }
        Some(_) => return Err(bad()),
    };
    if words.next().is_some() {
        return Err(bad());
    }
    Ok(Asked { block, quarter, square, part, scale })
}

/// The cadastral sheet a number names, on `d` in zone `z` (by default the 1:50,000 sheet's).
fn find_cadastral(a: &Asked, d: Datum, z: Option<u8>, scale: Scale) -> Result<UtmSheet, String> {
    let fifty = ts::sheets_50k()
        .iter()
        .find(|s| s.block == a.block && s.quarter == a.quarter)
        .copied()
        .ok_or_else(|| format!("{:04}: the series has no sheet {}", a.block, ["I", "II", "III", "IV"][a.quarter as usize - 1]))?;
    let (ee, nn) = a.square.ok_or("no 1:4,000 sheet number")?;
    let mid = ts::from_datum(Coord::new((fifty.west + fifty.east) / 2.0, (fifty.south + fifty.north) / 2.0), d)
        .ok_or("the sheet can't be put on WGS 84")?;
    let z = z.unwrap_or_else(|| ts::zone_of(mid.x));
    let centre = ts::to_utm(mid, d, z).ok_or("the sheet is outside the UTM zone")?;
    // The kilometres ending in EE and NN nearest the 1:50,000 sheet's centre, a sheet being about
    // 27 km across: try the neighbouring hundreds too, keeping the one on that sheet.
    let near = |m: f64, two: u32| {
        let km = (m / 1000.0).floor();
        let base = km - (km.rem_euclid(100.0)) + two as f64;
        [base - 100.0, base, base + 100.0]
    };
    let mut best: Option<(f64, f64, f64)> = None;
    for e in near(centre.x, ee) {
        for n in near(centre.y, nn) {
            let corner = Coord::new(e * 1000.0, n * 1000.0);
            let dist = ((corner.x + 1000.0 - centre.x).powi(2) + (corner.y + 1000.0 - centre.y).powi(2)).sqrt();
            if best.is_none_or(|b| dist < b.2) {
                best = Some((corner.x, corner.y, dist));
            }
        }
    }
    let (x, y, dist) = best.ok_or("no such sheet")?;
    if dist > 40_000.0 {
        return Err(format!(
            "{:04} {}: no 1:4,000 sheet {ee:02}{nn:02} is on it",
            a.block,
            ["I", "II", "III", "IV"][a.quarter as usize - 1]
        ));
    }
    // The part's centre within the 1:4,000 sheet, numbered from the top left.
    let side = scale.side();
    let k = (4000 / scale.denominator()) as f64;
    let (dx, dy) = match a.part {
        Some(p) => {
            if p as f64 > k * k {
                return Err(format!("a 1:4,000 sheet has {} sheets at {}", (k * k) as u32, scale.label()));
            }
            let (row, col) = ((p - 1) as f64 / k, ((p - 1) as f64) % k);
            ((col + 0.5) * side, 2000.0 - (row.floor() + 0.5) * side)
        }
        None => (1000.0, 1000.0),
    };
    let at = ts::from_utm(Coord::new(x + dx, y + dy), d, z).ok_or("the sheet can't be put on WGS 84")?;
    let found = ts::utm_sheet(at, d, if a.part.is_some() { scale } else { Scale::S4000 }, Some(z)).ok_or("no such sheet")?;
    if found.sheet50k.block != a.block || found.sheet50k.quarter != a.quarter {
        return Err(format!(
            "{:04} {}: no 1:4,000 sheet {ee:02}{nn:02} is on it (the nearest is on {})",
            a.block,
            ["I", "II", "III", "IV"][a.quarter as usize - 1],
            found.sheet50k.label()
        ));
    }
    Ok(found)
}

fn extent_of(ring: &[Coord]) -> Extent {
    let mut e = Extent::EMPTY;
    for c in ring {
        e.expand(*c);
    }
    e
}

fn go_to_sheet(s: &mut Session, p: &Value) -> CmdResult {
    let text = ps(p, "sheet").map(str::trim).filter(|t| !t.is_empty()).ok_or("sheet is required, like 5541 I or 5137 IV 6888")?;
    let a = parse_sheet(text)?;
    let d = datum(p)?;
    let (label, ring) = if a.square.is_none() {
        let sh = ts::sheets_50k()
            .iter()
            .find(|t| t.block == a.block && t.quarter == a.quarter)
            .copied()
            .ok_or_else(|| format!("'{text}': the {} series has no such sheet", d.series()))?;
        (sh.label(), sh.ring_wgs84(d))
    } else {
        let given = match p.get("scale") {
            Some(Value::Number(n)) => Some(Scale::parse(&n.to_string())?),
            Some(Value::String(t)) => Some(Scale::parse(t)?),
            _ => None,
        };
        // A scale written with the number, else the one given, else the likeliest for the part.
        let scale = a.scale.or(given).unwrap_or(if a.part.is_some_and(|k| k > 16) { Scale::S500 } else { Scale::S1000 });
        if a.part.is_some() && scale == Scale::S4000 {
            return Err(format!("'{text}': a numbered part is a 1:2,000, 1:1,000 or 1:500 sheet"));
        }
        let u = find_cadastral(&a, d, zone(p)?, scale)?;
        (u.label(), u.ring_wgs84())
    };
    let e = extent_of(&ring);
    s.zoom_to(&e);
    Ok(json!({"sheet": label, "datum": d.code(), "bbox": [e.min_x, e.min_y, e.max_x, e.max_y]}))
}

fn bbox(s: &Session, p: &Value) -> Result<[f64; 4], String> {
    match p.get("bbox") {
        Some(Value::Array(v)) => {
            let n: Option<Vec<f64>> = v.iter().map(Value::as_f64).collect();
            match n.as_deref() {
                Some([w, so, e, n]) if w < e && so < n => Ok([*w, *so, *e, *n]),
                _ => Err("bbox is [west, south, east, north]".into()),
            }
        }
        Some(Value::Null) | None => {
            let e = super::workspace::view_extent(s);
            if e.is_empty() {
                return Err("the map's extent isn't known; give bbox".into());
            }
            Ok([e.min_x, e.min_y, e.max_x, e.max_y])
        }
        Some(_) => Err("bbox is [west, south, east, north]".into()),
    }
}

fn sheet_grid(s: &mut Session, p: &Value) -> CmdResult {
    let series = match p.get("series") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(t)) => t.clone(),
        _ => return Err("series is required: 50000, 4000, 2000, 1000 or 500".into()),
    };
    let d = datum(p)?;
    let area = bbox(s, p)?;
    let fifty = matches!(series.trim().to_ascii_lowercase().as_str(), "50000" | "1:50000" | "1:50,000" | "50k" | "l7018" | "l7017");
    let fields = vec![Field::new("sheet", FieldType::Text), Field::new("scale", FieldType::Text), Field::new("datum", FieldType::Text)];
    let (name, ds) = if fifty {
        // The extent on the sheets' datum: corners and edge midpoints.
        let [w, so, e, n] = area;
        let mut on = Extent::EMPTY;
        for c in [
            Coord::new(w, so),
            Coord::new(e, so),
            Coord::new(e, n),
            Coord::new(w, n),
            Coord::new((w + e) / 2.0, so),
            Coord::new((w + e) / 2.0, n),
        ] {
            on.expand(ts::to_datum(c, d).ok_or("the area can't be put on that datum")?);
        }
        let sheets = ts::sheets_50k_in([on.min_x, on.min_y, on.max_x, on.max_y]);
        if sheets.is_empty() {
            return Err(format!("no {} sheet is in the area: the series covers Thailand", d.series()));
        }
        let name = ps(p, "name").map(str::to_string).unwrap_or_else(|| format!("ระวาง {} 1:50,000 ({})", d.series(), d.name()));
        let mut ds = Dataset::new(&name, GeometryType::Polygon, fields);
        for sh in &sheets {
            ds.push_next(
                Some(Geometry::Polygon(vec![vec![sh.ring_wgs84(d)]])),
                vec![Attr::Text(sh.label()), Attr::Text("1:50,000".into()), Attr::Text(d.name().into())],
            );
        }
        (name, ds)
    } else {
        let scale = Scale::parse(&series)?;
        let sheets = ts::utm_sheets_in(area, d, scale, zone(p)?, MAX_SHEETS)?;
        let z = sheets.first().map(|u| u.zone).unwrap_or(47);
        let name = ps(p, "name").map(str::to_string).unwrap_or_else(|| format!("ระวาง UTM {} ({}, โซน {z})", scale.label(), d.name()));
        let mut ds = Dataset::new(&name, GeometryType::Polygon, fields);
        for u in &sheets {
            ds.push_next(
                Some(Geometry::Polygon(vec![vec![u.ring_wgs84()]])),
                vec![Attr::Text(u.label()), Attr::Text(scale.label().into()), Attr::Text(d.name().into())],
            );
        }
        (name, ds)
    };
    let n = ds.features.len();
    let id = super::layer::add_dataset(s, ds);
    // Outlines labelled with their numbers, over whatever is under them.
    s.run("layer.noFill", &json!({"layer": id}))?;
    s.run("layer.outline", &json!({"layer": id, "color": "#DC2626", "width": 1.2}))?;
    s.run("labels.field", &json!({"layer": id, "field": "sheet"}))?;
    s.run("labels.enable", &json!({"layer": id, "on": true}))?;
    Ok(json!({"layer": id, "name": name, "sheets": n, "datum": d.code()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_numbers_read() {
        let a = parse_sheet("5137 IV 6888-5").unwrap();
        assert_eq!(a, Asked { block: 5137, quarter: 4, square: Some((68, 88)), part: Some(5), scale: None });
        assert_eq!(parse_sheet("5541 i").unwrap().square, None);
        assert_eq!(parse_sheet("5638 II 2606 - 5 (๑ : ๑,๐๐๐)").unwrap().scale, Some(Scale::S1000));
        assert_eq!(parse_sheet("5638 II 2606 - 5 (๑ : ๑,๐๐๐)").unwrap().part, Some(5));
        assert!(parse_sheet("5137 V").is_err());
        assert!(parse_sheet("5137 IV 6788").is_err());
        assert!(parse_sheet("5137 IV 6888-65").is_err());
        assert!(parse_sheet("513 IV").is_err());
    }

    #[test]
    fn a_cadastral_number_finds_its_square() {
        let a = parse_sheet("5137 IV 6888").unwrap();
        let u = find_cadastral(&a, Datum::Wgs84, None, Scale::S4000).unwrap();
        assert_eq!((u.e4000, u.n4000, u.zone), (668_000.0, 1_588_000.0, 47));
        let a = parse_sheet("5137 IV 6888-16").unwrap();
        let u = find_cadastral(&a, Datum::Wgs84, None, Scale::S1000).unwrap();
        assert_eq!(u.label(), "5137 IV 6888-16");
        assert_eq!((u.x, u.y), (669_500.0, 1_588_000.0));
        let a = parse_sheet("5638 II 2606-1").unwrap();
        let u = find_cadastral(&a, Datum::Indian1975, None, Scale::S2000).unwrap();
        assert_eq!((u.x, u.y, u.zone), (326_000.0, 1_607_000.0, 48));
        // A square that is on another 1:50,000 sheet.
        assert!(find_cadastral(&parse_sheet("5137 IV 2020").unwrap(), Datum::Wgs84, None, Scale::S4000).is_err());
    }
}
