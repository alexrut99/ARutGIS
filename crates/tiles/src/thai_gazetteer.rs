//! Finding Thailand's subdistricts, districts and provinces (ตำบล อำเภอ จังหวัด) by name or by
//! postcode, in Thai or in English: `ต.สุเทพ อ.เมือง จ.เชียงใหม่`, `เชียงใหม่`, `50200`,
//! `Suthep, Chiang Mai`.
//!
//! The names, postcodes and extents come from the Department of Provincial Administration's
//! subdistricts (OpenGISData-Thailand, CC BY 4.0), the file Thai boundaries add as a layer
//! ([`crate::thai_boundaries`]), downloaded the first time a search needs it and kept for the
//! session; nothing is built into the app.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::geocode::Place;
use crate::thai_boundaries::{self, Level, Source};

/// Where the names are read from.
pub fn source_url() -> String {
    thai_boundaries::url(Source::Dopa, Level::Subdistrict)
}

/// Who made them, for the place found.
pub const ATTRIBUTION: &str = "Department of Provincial Administration (DOPA), via OpenGISData-Thailand, CC BY 4.0";

/// A subdistrict.
#[derive(Clone, Debug, PartialEq)]
pub struct Tambon {
    pub tam_th: String,
    pub tam_en: String,
    pub amp_th: String,
    pub amp_en: String,
    pub pro_th: String,
    pub pro_en: String,
    pub pro_code: String,
    pub postcodes: Vec<String>,
    /// Its extent: west, south, east, north.
    pub bbox: [f64; 4],
    /// A point inside it.
    pub lon: f64,
    pub lat: f64,
    /// The names as they are compared: lower case, without spaces.
    keys: [String; 6],
}

/// The subdistricts read.
#[derive(Debug, Default)]
pub struct Gazetteer {
    pub tambons: Vec<Tambon>,
    /// Features left out, having no Thai names or no outline: they can't be found.
    pub skipped: usize,
    /// Subdistricts with no postcode: a postcode search can't find them.
    pub without_postcode: usize,
}

impl Gazetteer {
    /// What the file lacks, said once when it is read: `None` when it is whole.
    pub fn notice(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.skipped > 0 {
            parts.push(format!("{} subdistricts in the file have no Thai name or no outline and can't be found", self.skipped));
        }
        if self.without_postcode > 0 {
            parts.push(format!("{} have no postcode, so a postcode search doesn't find them", self.without_postcode));
        }
        (!parts.is_empty()).then(|| format!("Thai place search: {}", parts.join("; ")))
    }
}

/// The names as compared: lower case, without spaces, dots or the abbreviation mark ฯ.
fn key(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && !matches!(c, '.' | 'ฯ' | '-' | '\'')).flat_map(char::to_lowercase).collect()
}

fn text(p: &Value, k: &str) -> String {
    p.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// Every position in a GeoJSON geometry's coordinates.
fn extent(v: &Value, e: &mut [f64; 4]) {
    match v {
        Value::Array(a) if a.len() >= 2 && a[0].is_number() && a[1].is_number() => {
            let (x, y) = (a[0].as_f64().unwrap_or(f64::NAN), a[1].as_f64().unwrap_or(f64::NAN));
            if x.is_finite() && y.is_finite() {
                *e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
            }
        }
        Value::Array(a) => a.iter().for_each(|c| extent(c, e)),
        _ => {}
    }
}

impl Gazetteer {
    /// Reads the subdistricts' GeoJSON.
    pub fn parse(bytes: &[u8]) -> Result<Gazetteer, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|_| "the subdistricts aren't GeoJSON".to_string())?;
        let features = v.get("features").and_then(Value::as_array).ok_or("the subdistricts aren't a GeoJSON feature collection")?;
        let mut tambons = Vec::with_capacity(features.len());
        let mut skipped = 0;
        for f in features {
            let Some(p) = f.get("properties") else {
                skipped += 1;
                continue;
            };
            let mut bbox = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            if let Some(c) = f.get("geometry").and_then(|g| g.get("coordinates")) {
                extent(c, &mut bbox);
            }
            if !bbox.iter().all(|x| x.is_finite()) {
                skipped += 1;
                continue;
            }
            let num = |k: &str| p.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
            let (lon, lat) = match (num("lon"), num("lat")) {
                (Some(x), Some(y)) => (x, y),
                _ => ((bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0),
            };
            let postcodes = match p.get("postcode") {
                Some(Value::Array(a)) => {
                    a.iter().filter_map(|x| x.as_str().map(str::to_string).or_else(|| x.as_u64().map(|n| n.to_string()))).collect()
                }
                Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
                Some(Value::Number(n)) => vec![n.to_string()],
                _ => Vec::new(),
            };
            let (tam_th, tam_en, amp_th, amp_en, pro_th, pro_en) =
                (text(p, "tam_th"), text(p, "tam_en"), text(p, "amp_th"), text(p, "amp_en"), text(p, "pro_th"), text(p, "pro_en"));
            if tam_th.is_empty() || amp_th.is_empty() || pro_th.is_empty() {
                skipped += 1;
                continue;
            }
            let keys = [key(&tam_th), key(&tam_en), key(&amp_th), key(&amp_en), key(&pro_th), key(&pro_en)];
            tambons.push(Tambon {
                tam_th,
                tam_en,
                amp_th,
                amp_en,
                pro_th,
                pro_en,
                pro_code: text(p, "pro_code"),
                postcodes,
                bbox,
                lon,
                lat,
                keys,
            });
        }
        if tambons.is_empty() {
            return Err("the subdistricts file has no subdistricts".into());
        }
        let without_postcode = tambons.iter().filter(|t| t.postcodes.is_empty()).count();
        Ok(Gazetteer { tambons, skipped, without_postcode })
    }
}

/// The subdistricts read this session, with the address they were read from.
static LOADED: Mutex<Option<(String, Arc<Gazetteer>)>> = Mutex::new(None);

/// The subdistricts read from `url`, when they are.
pub fn loaded(url: &str) -> Option<Arc<Gazetteer>> {
    LOADED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(u, _)| u == url).map(|(_, g)| g.clone())
}

/// What the file just read lacks, until it is taken to be shown (once).
static NOTICE: Mutex<Option<String>> = Mutex::new(None);

/// Reads and keeps the subdistricts downloaded from `url`; what they lack waits in
/// [`take_notice`].
pub fn load(url: &str, bytes: &[u8]) -> Result<Arc<Gazetteer>, String> {
    let g = Arc::new(Gazetteer::parse(bytes)?);
    *NOTICE.lock().unwrap_or_else(|e| e.into_inner()) = g.notice();
    *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = Some((url.to_string(), g.clone()));
    Ok(g)
}

/// What the subdistricts read last lack (features left out, postcodes missing), once: the app
/// and the command line show it as a notice.
pub fn take_notice() -> Option<String> {
    NOTICE.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The level a word asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Want {
    Tambon,
    Amphoe,
    Province,
}

/// Words that say what the next name is: `ต.`, `อำเภอ`, `จังหวัด`, `Tambon`…
const MARKERS: &[(&str, Want)] = &[
    ("กิ่งอำเภอ", Want::Amphoe),
    ("กิ่งอ.", Want::Amphoe),
    ("ตำบล", Want::Tambon),
    ("แขวง", Want::Tambon),
    ("อำเภอ", Want::Amphoe),
    ("จังหวัด", Want::Province),
    ("ต.", Want::Tambon),
    ("อ.", Want::Amphoe),
    ("จ.", Want::Province),
];

const LATIN_MARKERS: &[(&str, Want)] = &[
    ("tambon", Want::Tambon),
    ("subdistrict", Want::Tambon),
    ("khwaeng", Want::Tambon),
    ("amphoe", Want::Amphoe),
    ("district", Want::Amphoe),
    ("khet", Want::Amphoe),
    ("changwat", Want::Province),
    ("province", Want::Province),
];

/// A name or postcode asked for, and the level it was marked as.
#[derive(Clone, Debug, PartialEq)]
struct Word {
    key: String,
    want: Option<Want>,
}

/// The words of a query: names split at spaces and commas (Thai names have no spaces), each
/// with the level a marker before it gave; in English, a name of several words is kept whole
/// between commas.
fn words(query: &str, latin_whole: bool) -> Vec<Word> {
    // The markers set apart from the names they are written against (`ต.สุเทพ`), in one pass so
    // that `อำเภอ` inside `กิ่งอำเภอ` stays one marker.
    let query = query.replace(['،', ';', '/'], ",");
    let mut s = String::with_capacity(query.len() + 16);
    let mut rest = query.as_str();
    while let Some(ch) = rest.chars().next() {
        match MARKERS.iter().find(|(m, _)| rest.starts_with(m)) {
            Some((m, _)) => {
                s.push_str(&format!(" {m} "));
                rest = &rest[m.len()..];
            }
            None => {
                s.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
    }
    // เขต is a district in Bangkok: a marker only at a word's start, before a name.
    let s: String = s
        .split_whitespace()
        .map(|w| match w.strip_prefix("เขต") {
            Some(rest) if rest.chars().count() >= 2 => format!("เขต {rest}"),
            _ => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = Vec::new();
    for part in s.split(',') {
        let mut pending: Option<Want> = None;
        let mut latin: Vec<&str> = Vec::new();
        let flush = |latin: &mut Vec<&str>, pending: &mut Option<Want>, out: &mut Vec<Word>| {
            if !latin.is_empty() {
                out.push(Word { key: key(&latin.join("")), want: pending.take() });
                latin.clear();
            }
        };
        for w in part.split_whitespace() {
            let lower = w.to_lowercase();
            let marker = MARKERS
                .iter()
                .find(|(m, _)| *m == w)
                .map(|(_, l)| *l)
                .or_else(|| (w == "เขต").then_some(Want::Amphoe))
                .or_else(|| LATIN_MARKERS.iter().find(|(m, _)| *m == lower.trim_end_matches('.')).map(|(_, l)| *l));
            if let Some(l) = marker {
                flush(&mut latin, &mut pending, &mut out);
                pending = Some(l);
                continue;
            }
            let is_latin = w.is_ascii();
            if is_latin && latin_whole && !w.chars().all(|c| c.is_ascii_digit()) {
                latin.push(w);
                continue;
            }
            flush(&mut latin, &mut pending, &mut out);
            let k = key(w);
            if !k.is_empty() {
                out.push(Word { key: k, want: pending.take() });
            }
        }
        flush(&mut latin, &mut pending, &mut out);
    }
    // Bangkok's short names.
    for w in &mut out {
        if matches!(w.key.as_str(), "กทม" | "กรุงเทพ" | "bkk" | "krungthep") {
            w.key = "กรุงเทพมหานคร".into();
        }
    }
    out
}

/// Whether a query reads as a Thai administrative name or a postcode (the search box sends
/// those here, and places by name to OpenStreetMap).
pub fn looks_administrative(query: &str) -> bool {
    let q = query.trim();
    let postcode = q.len() == 5 && q.chars().all(|c| c.is_ascii_digit());
    postcode
        || MARKERS.iter().any(|(m, _)| q.starts_with(m) || q.contains(&format!(" {m}")))
        || q.split_whitespace().any(|w| w.len() == 5 && w.chars().all(|c| c.is_ascii_digit()))
}

/// How well a word names a field: 3 the whole name, 1 its start.
fn score(word: &str, name: &str) -> u32 {
    if name.is_empty() {
        0
    } else if word == name {
        3
    } else if word.chars().count() >= 2 && name.starts_with(word) {
        1
    } else {
        0
    }
}

/// A place found: the level, the names and the subdistricts it is made of.
struct Found {
    level: Want,
    score: u32,
    key: (String, String, String),
    bbox: [f64; 4],
    point: (f64, f64),
    first: usize,
}

impl Gazetteer {
    /// The places a query names, best first, at most `limit`.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Place> {
        // Names in English for a query in English (a postcode alone is answered in Thai).
        let latin = query.chars().any(|c| c.is_ascii_alphabetic()) && !query.chars().any(|c| c.is_alphabetic() && !c.is_ascii());
        let mut found = self.find(&words(query, true), limit);
        if found.is_empty() && latin {
            // English names without commas between them: `Suthep Chiang Mai`, the words of a
            // name of several (`Chiang Mai`) joined.
            found = self.find(&self.join_names(words(query, false)), limit);
        }
        found.into_iter().map(|f| self.place(&f, latin)).collect()
    }

    /// Words joined with the ones after them while together they are a name in the file (the
    /// longest such run), so a name of several words is one term.
    fn join_names(&self, words: Vec<Word>) -> Vec<Word> {
        let names: std::collections::HashSet<&str> = self.tambons.iter().flat_map(|t| t.keys.iter().map(String::as_str)).collect();
        let mut out = Vec::with_capacity(words.len());
        let mut i = 0;
        while let Some(first) = words.get(i) {
            let (mut end, mut joined, mut acc) = (i, first.key.clone(), first.key.clone());
            // A word a marker sets apart starts a name of its own.
            for (j, w) in words.iter().enumerate().skip(i + 1).take_while(|(_, w)| w.want.is_none()) {
                acc.push_str(&w.key);
                if names.contains(acc.as_str()) {
                    (end, joined) = (j, acc.clone());
                }
            }
            out.push(Word { key: joined, want: first.want });
            i = end + 1;
        }
        out
    }

    fn find(&self, words: &[Word], limit: usize) -> Vec<Found> {
        if words.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<Found> = Vec::new();
        for (i, t) in self.tambons.iter().enumerate() {
            let mut total = 0;
            let mut level = Want::Province;
            let mut ok = true;
            for w in words {
                let postcode = w.key.len() == 5 && w.key.chars().all(|c| c.is_ascii_digit());
                // The best field the word names, the more local one on a tie.
                let mut best: Option<(u32, Want)> = None;
                if postcode {
                    if w.want.is_none() && t.postcodes.contains(&w.key) {
                        best = Some((3, Want::Tambon));
                    }
                } else {
                    for (k, l) in [
                        (0, Want::Tambon),
                        (1, Want::Tambon),
                        (2, Want::Amphoe),
                        (3, Want::Amphoe),
                        (4, Want::Province),
                        (5, Want::Province),
                    ] {
                        if w.want.is_some_and(|want| want != l) {
                            continue;
                        }
                        let sc = score(&w.key, &t.keys[k]);
                        if sc > 0 && best.is_none_or(|(b, bl)| sc > b || (sc == b && l < bl)) {
                            best = Some((sc, l));
                        }
                    }
                }
                match best {
                    Some((sc, l)) => {
                        total += sc;
                        level = level.min(l);
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let key = match level {
                Want::Tambon => (t.pro_th.clone(), t.amp_th.clone(), t.tam_th.clone()),
                Want::Amphoe => (t.pro_th.clone(), t.amp_th.clone(), String::new()),
                Want::Province => (t.pro_th.clone(), String::new(), String::new()),
            };
            match found.iter_mut().find(|f| f.level == level && f.key == key) {
                Some(f) => {
                    f.score = f.score.max(total);
                    f.bbox = [f.bbox[0].min(t.bbox[0]), f.bbox[1].min(t.bbox[1]), f.bbox[2].max(t.bbox[2]), f.bbox[3].max(t.bbox[3])];
                }
                None => found.push(Found { level, score: total, key, bbox: t.bbox, point: (t.lon, t.lat), first: i }),
            }
        }
        // Best first; on a tie the larger unit (a province before a subdistrict of its name).
        found.sort_by(|a, b| b.score.cmp(&a.score).then(b.level.cmp(&a.level)).then(a.first.cmp(&b.first)));
        found.truncate(limit.max(1));
        for f in &mut found {
            if f.level != Want::Tambon {
                f.point = ((f.bbox[0] + f.bbox[2]) / 2.0, (f.bbox[1] + f.bbox[3]) / 2.0);
            }
        }
        found
    }

    fn place(&self, f: &Found, english: bool) -> Place {
        let t = &self.tambons[f.first];
        let bangkok = t.pro_code == "10" || t.pro_th == "กรุงเทพมหานคร";
        let name = if english {
            match f.level {
                Want::Tambon => format!("{}, {}, {} {}", t.tam_en, t.amp_en, t.pro_en, t.postcodes.join(", ")).trim().to_string(),
                Want::Amphoe => format!("{}, {}", t.amp_en, t.pro_en),
                Want::Province => t.pro_en.clone(),
            }
        } else {
            let (tam, amp, pro) = if bangkok { ("แขวง", "เขต", "") } else { ("ต.", "อ.", "จ.") };
            match f.level {
                Want::Tambon => {
                    format!("{tam}{} {amp}{} {pro}{} {}", t.tam_th, t.amp_th, t.pro_th, t.postcodes.join(", ")).trim().to_string()
                }
                Want::Amphoe => format!("{amp}{} {pro}{}", t.amp_th, t.pro_th),
                Want::Province => format!("{pro}{}", t.pro_th),
            }
        };
        Place { name, lon: f.point.0, lat: f.point.1, bbox: Some(f.bbox) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Gazetteer {
        let f = |tam: &str, tam_en: &str, amp: &str, amp_en: &str, pro: &str, pro_en: &str, code: &str, post: &str, x: f64, y: f64| {
            format!(
                r#"{{"type":"Feature","properties":{{"tam_th":"{tam}","tam_en":"{tam_en}","amp_th":"{amp}","amp_en":"{amp_en}","pro_th":"{pro}","pro_en":"{pro_en}","pro_code":"{code}","postcode":["{post}"],"lon":{x},"lat":{y}}},"geometry":{{"type":"MultiPolygon","coordinates":[[[[{x0},{y0}],[{x1},{y0}],[{x1},{y1}],[{x0},{y0}]]]]}}}}"#,
                x0 = x - 0.01,
                x1 = x + 0.01,
                y0 = y - 0.01,
                y1 = y + 0.01
            )
        };
        let features = [
            f("สุเทพ", "Suthep", "เมืองเชียงใหม่", "Mueang Chiang Mai", "เชียงใหม่", "Chiang Mai", "50", "50200", 98.95, 18.8),
            f("ศรีภูมิ", "Si Phum", "เมืองเชียงใหม่", "Mueang Chiang Mai", "เชียงใหม่", "Chiang Mai", "50", "50200", 98.99, 18.79),
            f("บ้านหลวง", "Ban Luang", "จอมทอง", "Chom Thong", "เชียงใหม่", "Chiang Mai", "50", "50160", 98.6, 18.4),
            f("บ้านหลวง", "Ban Luang", "เมืองน่าน", "Mueang Nan", "น่าน", "Nan", "55", "55000", 100.8, 18.8),
            f(
                "พระบรมมหาราชวัง",
                "Phra Borom Maha Ratchawang",
                "พระนคร",
                "Phra Nakhon",
                "กรุงเทพมหานคร",
                "Bangkok",
                "10",
                "10200",
                100.49,
                13.75,
            ),
        ];
        let json = format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, features.join(","));
        Gazetteer::parse(json.as_bytes()).unwrap()
    }

    fn names(g: &Gazetteer, q: &str) -> Vec<String> {
        g.search(q, 10).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn subdistricts_districts_and_provinces_by_thai_name() {
        let g = sample();
        assert_eq!(g.tambons.len(), 5);
        assert_eq!(names(&g, "ต.สุเทพ อ.เมือง จ.เชียงใหม่"), ["ต.สุเทพ อ.เมืองเชียงใหม่ จ.เชียงใหม่ 50200"]);
        assert_eq!(names(&g, "ตำบลสุเทพ"), ["ต.สุเทพ อ.เมืองเชียงใหม่ จ.เชียงใหม่ 50200"]);
        assert_eq!(names(&g, "สุเทพ เชียงใหม่"), ["ต.สุเทพ อ.เมืองเชียงใหม่ จ.เชียงใหม่ 50200"]);
        // A province, over the extent of its subdistricts.
        let p = g.search("จังหวัดเชียงใหม่", 5);
        assert_eq!(p[0].name, "จ.เชียงใหม่");
        let b = p[0].bbox.unwrap();
        for (got, want) in b.iter().zip([98.59, 18.39, 99.0, 18.81]) {
            assert!((got - want).abs() < 1e-9, "{b:?}");
        }
        assert_eq!(names(&g, "กิ่งอำเภอจอมทอง"), ["อ.จอมทอง จ.เชียงใหม่"]);
        assert_eq!(names(&g, "อ.เมืองเชียงใหม่"), ["อ.เมืองเชียงใหม่ จ.เชียงใหม่"]);
        // Two subdistricts of one name, told apart by their province.
        assert_eq!(names(&g, "บ้านหลวง").len(), 2);
        assert_eq!(names(&g, "ต.บ้านหลวง จ.น่าน"), ["ต.บ้านหลวง อ.เมืองน่าน จ.น่าน 55000"]);
        // Marked levels hold: no district is called บ้านหลวง.
        assert!(names(&g, "อ.บ้านหลวง").is_empty());
        // Bangkok's แขวง and เขต, and its short names.
        assert_eq!(names(&g, "แขวงพระบรมมหาราชวัง เขตพระนคร"), ["แขวงพระบรมมหาราชวัง เขตพระนคร กรุงเทพมหานคร 10200"]);
        assert_eq!(names(&g, "กทม"), ["กรุงเทพมหานคร"]);
        assert_eq!(names(&g, "กรุงเทพฯ"), ["กรุงเทพมหานคร"]);
    }

    #[test]
    fn postcodes_and_english_names() {
        let g = sample();
        assert_eq!(names(&g, "50200"), ["ต.สุเทพ อ.เมืองเชียงใหม่ จ.เชียงใหม่ 50200", "ต.ศรีภูมิ อ.เมืองเชียงใหม่ จ.เชียงใหม่ 50200"]);
        assert_eq!(names(&g, "สุเทพ 50200").len(), 1);
        assert!(names(&g, "10100").is_empty());
        assert_eq!(names(&g, "Suthep, Mueang Chiang Mai, Chiang Mai"), ["Suthep, Mueang Chiang Mai, Chiang Mai 50200"]);
        assert_eq!(names(&g, "chiang mai"), ["Chiang Mai"]);
        assert_eq!(names(&g, "Tambon Suthep"), ["Suthep, Mueang Chiang Mai, Chiang Mai 50200"]);
        assert_eq!(names(&g, "Bangkok"), ["Bangkok"]);
        // Names of several words without commas between them, in either order.
        assert_eq!(names(&g, "Suthep Chiang Mai"), ["Suthep, Mueang Chiang Mai, Chiang Mai 50200"]);
        assert_eq!(names(&g, "Chiang Mai Suthep"), ["Suthep, Mueang Chiang Mai, Chiang Mai 50200"]);
        assert_eq!(names(&g, "Ban Luang Nan"), ["Ban Luang, Mueang Nan, Nan 55000"]);
        assert_eq!(names(&g, "Mueang Chiang Mai"), ["Mueang Chiang Mai, Chiang Mai"]);
        assert!(names(&g, "London").is_empty());
    }

    #[test]
    fn what_the_file_lacks_is_counted() {
        // The sample is whole: nothing to say.
        let g = sample();
        assert_eq!((g.skipped, g.without_postcode), (0, 0));
        assert_eq!(g.notice(), None);
        // A feature without names, one without an outline, one without a postcode.
        let json = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"tam_th":"ก","amp_th":"ข","pro_th":"ค","postcode":[]},"geometry":{"type":"Point","coordinates":[100,14]}},
            {"type":"Feature","properties":{"tam_th":"","amp_th":"ข","pro_th":"ค"},"geometry":{"type":"Point","coordinates":[100,14]}},
            {"type":"Feature","properties":{"tam_th":"ง","amp_th":"ข","pro_th":"ค","postcode":["10000"]},"geometry":null},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[100,14]}}
        ]}"#;
        let g = Gazetteer::parse(json.as_bytes()).unwrap();
        assert_eq!((g.tambons.len(), g.skipped, g.without_postcode), (1, 3, 1));
        let n = g.notice().unwrap();
        assert!(n.contains("3 subdistricts") && n.contains("1 have no postcode"), "{n}");
        // Read through `load`, the notice waits to be shown once.
        load("test://lacking", json.as_bytes()).unwrap();
        assert!(take_notice().is_some_and(|t| t.contains("3 subdistricts")));
        assert_eq!(take_notice(), None);
    }

    #[test]
    fn administrative_queries_are_recognised() {
        assert!(looks_administrative("50200"));
        assert!(looks_administrative("ต.สุเทพ"));
        assert!(looks_administrative("จังหวัดน่าน"));
        assert!(looks_administrative("ร้านกาแฟ 50200"));
        assert!(!looks_administrative("วัดพระแก้ว"));
        assert!(!looks_administrative("Eiffel Tower"));
    }
}
