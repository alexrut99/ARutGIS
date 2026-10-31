//! 3D buildings from OpenStreetMap: each building's height in metres, from what its tags say
//! or, failing that, estimated, and where it came from, so a layer can be raised by it.
//!
//! In order: the `height` tag (metres unless it says feet); else `building:levels` times
//! [`METRES_PER_FLOOR`]; else an estimate from the kind of building (`building=house`,
//! `apartments`, `temple`…) for a typical one in Thailand. Few buildings in Thailand have either
//! tag, so most heights are estimates: `height_src` says which each is.

use serde_json::{Map, Value, json};

/// Metres a floor adds.
pub const METRES_PER_FLOOR: f64 = 3.0;

/// The most ground a request covers (km²): a larger view is narrowed to a square of this much
/// about its centre, which the Overpass servers answer in reasonable time.
pub const MAX_AREA_KM2: f64 = 9.0;

/// The credit OpenStreetMap's licence asks for.
pub const ATTRIBUTION: &str = "© OpenStreetMap contributors (ODbL)";

/// Where a building's height comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightSource {
    /// Its `height` tag.
    Tagged,
    /// Its `building:levels` tag.
    Floors,
    /// The kind of building.
    Estimate,
}

impl HeightSource {
    pub fn code(self) -> &'static str {
        match self {
            HeightSource::Tagged => "height",
            HeightSource::Floors => "floors",
            HeightSource::Estimate => "estimate",
        }
    }
}

/// The first number in a tag's value: `12`, `12.5 m`, `~10`, `3;4` (the first of a list).
fn first_number(s: &str) -> Option<f64> {
    let s = s.trim().trim_start_matches(['~', '≈']).trim();
    let s = s.split(';').next()?.trim();
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(s.len());
    let n: f64 = s.get(..end)?.replace(',', ".").parse().ok()?;
    n.is_finite().then_some(n)
}

/// A `height` tag in metres: `12`, `12 m`, `40 ft`, `40'`, `12'6"`.
pub fn parse_height(s: &str) -> Option<f64> {
    let t = s.trim().to_ascii_lowercase();
    let feet = t.contains("ft") || t.contains("feet") || t.contains('\'');
    let mut m = first_number(&t)?;
    if feet {
        m *= 0.3048;
        // 12'6": the inches after the feet.
        if let Some((_, rest)) = t.split_once('\'')
            && let Some(inches) = first_number(rest.trim_end_matches('"'))
        {
            m += inches * 0.0254;
        }
    }
    (m > 0.0 && m < 1000.0).then_some(m)
}

/// A typical height (metres) of a `building=` kind in Thailand.
pub fn estimate(building: &str) -> f64 {
    match building.trim().to_ascii_lowercase().as_str() {
        // Small: sheds, garages, huts, kiosks, roofs over nothing.
        "garage" | "garages" | "shed" | "hut" | "roof" | "carport" | "kiosk" | "toilets" | "cabin" | "service" | "farm_auxiliary"
        | "barn" | "greenhouse" | "ruins" | "container" | "bunker" | "stable" | "sty" => 3.0,
        // Shophouses and townhouses: three or four floors.
        "terrace" | "townhouse" | "shophouse" => 10.0,
        "apartments" | "dormitory" | "condominium" => 15.0,
        "hospital" => 15.0,
        "hotel" => 20.0,
        "commercial" | "retail" | "office" | "supermarket" | "mall" => 12.0,
        "school" | "university" | "college" | "kindergarten" | "public" | "government" | "civic" | "fire_station" => 10.0,
        "temple" | "church" | "mosque" | "shrine" | "monastery" | "religious" | "cathedral" | "chapel" | "pagoda" => 12.0,
        "train_station" | "transportation" | "stadium" | "sports_hall" | "sports_centre" | "grandstand" | "hangar" => 12.0,
        "industrial" | "warehouse" | "factory" | "manufacture" | "storage" => 9.0,
        // Houses and buildings of no stated kind: two floors.
        _ => 6.0,
    }
}

/// A building's height (metres), its floors when tagged, and where the height comes from.
pub fn height(tags: &[(String, String)]) -> (f64, Option<f64>, HeightSource) {
    let get = |k: &str| tags.iter().find(|(t, _)| t == k).map(|(_, v)| v.as_str());
    let floors = get("building:levels").and_then(first_number).filter(|n| *n > 0.0 && *n <= 200.0);
    if let Some(h) = get("height").or_else(|| get("building:height")).and_then(parse_height) {
        return (h, floors, HeightSource::Tagged);
    }
    if let Some(f) = floors {
        return (f * METRES_PER_FLOOR, Some(f), HeightSource::Floors);
    }
    (estimate(get("building").unwrap_or("yes")), None, HeightSource::Estimate)
}

/// The properties a building's polygon gets: `height_m`, `floors` and `height_src`.
pub fn properties(tags: &[(String, String)]) -> Map<String, Value> {
    let (h, floors, src) = height(tags);
    let mut m = Map::new();
    m.insert("height_m".into(), json!((h * 10.0).round() / 10.0));
    m.insert("floors".into(), floors.map_or(Value::Null, |f| json!(f)));
    m.insert("height_src".into(), json!(src.code()));
    m
}

/// `bbox` (west, south, east, north), narrowed to a square of [`MAX_AREA_KM2`] about its centre
/// when it covers more; and whether it was.
pub fn narrow(bbox: [f64; 4]) -> ([f64; 4], bool) {
    let [w, s, e, n] = bbox;
    let mid = (s + n) / 2.0;
    let km_lon = 111.32 * mid.to_radians().cos().max(0.01);
    let area = ((e - w) * km_lon) * ((n - s) * 110.57);
    if area.is_nan() || area <= MAX_AREA_KM2 {
        return (bbox, false);
    }
    let half_km = MAX_AREA_KM2.sqrt() / 2.0;
    let (cx, cy) = ((w + e) / 2.0, mid);
    let (dx, dy) = (half_km / km_lon, half_km / 110.57);
    ([cx - dx, cy - dy, cx + dx, cy + dy], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(t: &[(&str, &str)]) -> Vec<(String, String)> {
        t.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn heights_come_from_the_height_tag_then_floors_then_the_kind() {
        assert_eq!(
            height(&tags(&[("building", "yes"), ("height", "45 m"), ("building:levels", "12")])),
            (45.0, Some(12.0), HeightSource::Tagged)
        );
        assert_eq!(height(&tags(&[("building", "apartments"), ("building:levels", "8")])), (24.0, Some(8.0), HeightSource::Floors));
        assert_eq!(height(&tags(&[("building", "temple")])), (12.0, None, HeightSource::Estimate));
        assert_eq!(height(&tags(&[("building", "yes")])).0, 6.0);
        // A height that isn't one falls through to the floors.
        assert_eq!(height(&tags(&[("height", "tall"), ("building:levels", "2")])).2, HeightSource::Floors);
        let p = properties(&tags(&[("building", "house")]));
        assert_eq!(p["height_m"], 6.0);
        assert!(p["floors"].is_null());
        assert_eq!(p["height_src"], "estimate");
    }

    #[test]
    fn height_tags_are_read_in_metres() {
        assert_eq!(parse_height("12"), Some(12.0));
        assert_eq!(parse_height("12.5m"), Some(12.5));
        assert_eq!(parse_height("~10"), Some(10.0));
        assert_eq!(parse_height("3;4"), Some(3.0));
        assert_eq!(parse_height("12,5"), Some(12.5));
        assert!((parse_height("100 ft").unwrap() - 30.48).abs() < 1e-9);
        assert!((parse_height("12'6\"").unwrap() - (12.0 * 0.3048 + 6.0 * 0.0254)).abs() < 1e-9);
        assert_eq!(parse_height("0"), None);
        assert_eq!(parse_height("tall"), None);
    }

    #[test]
    fn a_large_view_is_narrowed_to_a_square_about_its_centre() {
        // About 1 km × 1 km in Bangkok: kept.
        let small = [100.52, 13.72, 100.529, 13.729];
        assert_eq!(narrow(small), (small, false));
        // Bangkok's whole width: about 3 km × 3 km about the middle.
        let (b, narrowed) = narrow([100.3, 13.5, 100.9, 14.0]);
        assert!(narrowed);
        let km = (b[2] - b[0]) * 111.32 * 13.75_f64.to_radians().cos();
        assert!((km - 3.0).abs() < 0.01, "{km}");
        assert!(((b[0] + b[2]) / 2.0 - 100.6).abs() < 1e-9 && ((b[1] + b[3]) / 2.0 - 13.75).abs() < 1e-9);
    }
}
