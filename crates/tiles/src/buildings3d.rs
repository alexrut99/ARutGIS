//! 3D buildings: each building's height in metres and where it came from, so a layer can be
//! raised by it. The buildings are OpenStreetMap's (by their tags) or Overture Maps' (OpenStreetMap's
//! with the buildings Google, Microsoft and Esri found in imagery filling the gaps, by its columns).
//!
//! In order, as [`Heights`] allows: the data's height (OpenStreetMap's `height` tag, metres unless
//! it says feet; Overture's `height`); else its floors (`building:levels`, Overture's
//! `num_floors`) times [`METRES_PER_FLOOR`]; else an estimate from the kind of building
//! (`building=house`, `apartments`, `temple`…; Overture's `class`, else its `subtype`) for a
//! typical one in Thailand; else one floor. Few buildings in Thailand have a height or floors, so
//! most heights are estimates: `height_src` says which each is.

use serde_json::{Map, Value, json};

/// Metres a floor adds.
pub const METRES_PER_FLOOR: f64 = 3.0;

/// The most ground a request covers (km²): a larger view is narrowed to a square of this much
/// about its centre, which the Overpass servers answer in reasonable time.
pub const MAX_AREA_KM2: f64 = 9.0;

/// The credit OpenStreetMap's licence asks for.
pub const ATTRIBUTION: &str = "© OpenStreetMap contributors (ODbL)";

/// The credit Overture's buildings ask for.
pub const OVERTURE_ATTRIBUTION: &str =
    "© Overture Maps Foundation, © OpenStreetMap contributors, Microsoft, Google, Esri Community Maps (ODbL)";

/// Where the buildings come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// OpenStreetMap, through an Overpass server: up to date, named and tagged.
    #[default]
    Osm,
    /// Overture Maps' buildings theme, read in place from its GeoParquet files: OpenStreetMap's,
    /// with buildings found in imagery where OpenStreetMap has none.
    Overture,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Osm, Source::Overture];
    pub fn code(self) -> &'static str {
        match self {
            Source::Osm => "osm",
            Source::Overture => "overture",
        }
    }
    pub fn parse(s: &str) -> Result<Source, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "osm" | "openstreetmap" => Ok(Source::Osm),
            "overture" => Ok(Source::Overture),
            other => Err(format!("'{other}' isn't a source of buildings (osm or overture)")),
        }
    }
    pub fn attribution(self) -> &'static str {
        match self {
            Source::Osm => ATTRIBUTION,
            Source::Overture => OVERTURE_ATTRIBUTION,
        }
    }
}

/// Which heights are used, in their order: the data's own (a height, else floors), then Google's
/// 2.5D heights from imagery (`google25d`, off by default: the desktop app's), then an estimate
/// from the kind of building. A building left with none stands one floor high.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heights {
    pub data: bool,
    pub google: bool,
    pub estimate: bool,
}

impl Default for Heights {
    fn default() -> Self {
        Heights { data: true, google: false, estimate: true }
    }
}

/// Where a building's height comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeightSource {
    /// Its `height` tag.
    Tagged,
    /// Its `building:levels` tag.
    Floors,
    /// The kind of building.
    Estimate,
    /// Google's 2.5D heights, read from imagery.
    Google,
    /// None of the heights allowed: one floor.
    Floor,
}

impl HeightSource {
    pub fn code(self) -> &'static str {
        match self {
            HeightSource::Tagged => "height",
            HeightSource::Floors => "floors",
            HeightSource::Estimate => "estimate",
            HeightSource::Google => "google_2.5d",
            HeightSource::Floor => "one_floor",
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
        // Overture's subtypes, for buildings it knows no class of.
        "agricultural" | "outbuilding" => 3.0,
        "education" => 10.0,
        "medical" => 15.0,
        "entertainment" => 12.0,
        // Houses and buildings of no stated kind: two floors.
        _ => 6.0,
    }
}

/// A height, floors and kind of building as [`Heights`] allows: the height, else the floors, else
/// an estimate from the kind, else one floor.
fn choose(h: Option<f64>, floors: Option<f64>, kind: Option<&str>, allow: Heights) -> (f64, Option<f64>, HeightSource) {
    if allow.data {
        if let Some(h) = h {
            return (h, floors, HeightSource::Tagged);
        }
        if let Some(f) = floors {
            return (f * METRES_PER_FLOOR, Some(f), HeightSource::Floors);
        }
    }
    let floors = floors.filter(|_| allow.data);
    if allow.estimate {
        return (estimate(kind.unwrap_or("yes")), floors, HeightSource::Estimate);
    }
    (METRES_PER_FLOOR, floors, HeightSource::Floor)
}

/// A building's height (metres), its floors when tagged, and where the height comes from.
pub fn height(tags: &[(String, String)]) -> (f64, Option<f64>, HeightSource) {
    height_with(tags, Heights::default())
}

/// [`height`], using only the heights `allow`s.
pub fn height_with(tags: &[(String, String)], allow: Heights) -> (f64, Option<f64>, HeightSource) {
    let get = |k: &str| tags.iter().find(|(t, _)| t == k).map(|(_, v)| v.as_str());
    let floors = get("building:levels").and_then(first_number).filter(|n| *n > 0.0 && *n <= 200.0);
    let h = get("height").or_else(|| get("building:height")).and_then(parse_height);
    choose(h, floors, get("building"), allow)
}

/// The properties a building's polygon gets: `height_m`, `floors` and `height_src`.
pub fn properties(tags: &[(String, String)]) -> Map<String, Value> {
    properties_with(tags, Heights::default())
}

/// [`properties`], using only the heights `allow`s.
pub fn properties_with(tags: &[(String, String)], allow: Heights) -> Map<String, Value> {
    fields(height_with(tags, allow))
}

fn fields((h, floors, src): (f64, Option<f64>, HeightSource)) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("height_m".into(), json!((h * 10.0).round() / 10.0));
    m.insert("floors".into(), floors.map_or(Value::Null, |f| json!(f)));
    m.insert("height_src".into(), json!(src.code()));
    m
}

/// An Overture building's height (metres), floors and where the height comes from, from its
/// `height`, `num_floors`, `class` and `subtype`.
pub fn overture_height(
    height: Option<f64>,
    floors: Option<f64>,
    class: Option<&str>,
    subtype: Option<&str>,
    allow: Heights,
) -> (f64, Option<f64>, HeightSource) {
    let h = height.filter(|h| h.is_finite() && *h > 0.0 && *h < 1000.0);
    let floors = floors.filter(|n| n.is_finite() && *n > 0.0 && *n <= 200.0);
    let kind = class.or(subtype).map(str::trim).filter(|k| !k.is_empty());
    choose(h, floors, kind, allow)
}

/// The properties an Overture building gets: [`properties`]' fields, from [`overture_height`].
pub fn overture_properties(
    height: Option<f64>,
    floors: Option<f64>,
    class: Option<&str>,
    subtype: Option<&str>,
    allow: Heights,
) -> Map<String, Value> {
    fields(overture_height(height, floors, class, subtype, allow))
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
    fn only_the_heights_allowed_are_used() {
        let t = tags(&[("building", "apartments"), ("building:levels", "8")]);
        let no_data = Heights { data: false, estimate: true, ..Heights::default() };
        assert_eq!(height_with(&t, no_data), (15.0, None, HeightSource::Estimate));
        let nothing = Heights { data: false, estimate: false, ..Heights::default() };
        assert_eq!(height_with(&t, nothing), (3.0, None, HeightSource::Floor));
        let data_only = Heights { data: true, estimate: false, ..Heights::default() };
        assert_eq!(height_with(&t, data_only), (24.0, Some(8.0), HeightSource::Floors));
        assert_eq!(height_with(&tags(&[("building", "temple")]), data_only), (3.0, None, HeightSource::Floor));
        assert_eq!(properties_with(&tags(&[("building", "house")]), nothing)["height_src"], "one_floor");
    }

    #[test]
    fn overture_heights_come_from_its_columns_then_its_class_or_subtype() {
        let all = Heights::default();
        assert_eq!(overture_height(Some(52.0), Some(14.0), Some("office"), None, all), (52.0, Some(14.0), HeightSource::Tagged));
        assert_eq!(overture_height(None, Some(4.0), None, None, all), (12.0, Some(4.0), HeightSource::Floors));
        assert_eq!(overture_height(None, None, Some("temple"), Some("religious"), all), (12.0, None, HeightSource::Estimate));
        assert_eq!(overture_height(None, None, None, Some("medical"), all), (15.0, None, HeightSource::Estimate));
        assert_eq!(overture_height(None, None, None, None, all), (6.0, None, HeightSource::Estimate));
        // Nonsense heights fall through.
        assert_eq!(overture_height(Some(-1.0), Some(0.0), Some("house"), None, all).2, HeightSource::Estimate);
        let p = overture_properties(Some(30.0), None, None, None, Heights { data: false, estimate: false, google: false });
        assert_eq!((p["height_m"].as_f64(), p["height_src"].as_str()), (Some(3.0), Some("one_floor")));
    }

    #[test]
    fn sources_are_named_by_their_codes() {
        for s in Source::ALL {
            assert_eq!(Source::parse(s.code()), Ok(s));
        }
        assert_eq!(Source::parse("OpenStreetMap"), Ok(Source::Osm));
        assert!(Source::parse("google").is_err());
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
