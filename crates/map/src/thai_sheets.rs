//! Thai map sheet numbers (เลขระวาง), worked out from where a point is rather than read from a
//! data file:
//!
//! - **1:50,000 topographic sheets** of the Royal Thai Survey Department (กรมแผนที่ทหาร), series
//!   L7018 (WGS 84) and its forerunner L7017 (Indian 1975), numbered the same way. A sheet is
//!   15′ × 15′. Its number is a four-digit block `CCRR` of 30′ × 30′ (`CC`, the column, from
//!   75°E in half degrees; `RR`, the row, from 4°30′S) and a quarter of it: I north-east,
//!   II south-east, III south-west, IV north-west. Khon Kaen (16.44°N, 102.84°E) is on 5541 I.
//!   The 830 sheets the series has, and the few along the coast of the south and the
//!   Myanmar border that are shifted or widened to hold the land, follow the series index
//!   (cross-checked against Mahidol University's index of L7018 sheets and Andreas
//!   Hoerstemeier's index from RTSD's catalogue).
//! - **Cadastral UTM sheets** of the Department of Lands (กรมที่ดิน), by its regulation on map
//!   sheets (ระเบียบกรมที่ดินว่าด้วยการสร้างและการใช้ระวางแผนที่ พ.ศ. 2547): a 1:4,000 sheet is a
//!   2 km square of the UTM grid, numbered by the 1:50,000 sheet its lower-left corner is on and
//!   the tens and units of that corner's easting and northing in kilometres (even numbers):
//!   `5638 II 2606`. It is split into 4 sheets at 1:2,000, 16 at 1:1,000 and 64 at 1:500,
//!   numbered from the top left, row by row: `5638 II 2606-5` is the first sheet of the second
//!   row at 1:1,000.
//!
//! Both are worked out on WGS 84 (today's sheets) or on Indian 1975 (older ones), with EPSG's
//! transformation between them, which the app's Indian 1975 systems use too.

use rustgis_geom::Coord;

/// The datum sheets are numbered on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Datum {
    /// WGS 84: series L7018 and today's cadastral sheets.
    Wgs84,
    /// Indian 1975: series L7017 and older cadastral sheets.
    Indian1975,
}

/// Indian 1975's ellipsoid (Everest 1830) and EPSG's shift to WGS 84 for Thailand, as the app's
/// Indian 1975 / UTM presets have them.
const INDIAN_1975: &str = "+a=6377276.345 +b=6356075.41314024 +towgs84=210,814,289,0,0,0,0";

impl Datum {
    /// Reads `wgs84` or `indian1975`.
    pub fn parse(s: &str) -> Result<Datum, String> {
        let k: String = s.trim().to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        match k.as_str() {
            "" | "wgs84" | "4326" | "l7018" => Ok(Datum::Wgs84),
            "indian1975" | "indian" | "4240" | "l7017" => Ok(Datum::Indian1975),
            _ => Err(format!("'{s}': a datum is wgs84 or indian1975")),
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Datum::Wgs84 => "wgs84",
            Datum::Indian1975 => "indian1975",
        }
    }

    /// Its name as the app shows it.
    pub fn name(self) -> &'static str {
        match self {
            Datum::Wgs84 => "WGS 84",
            Datum::Indian1975 => "Indian 1975",
        }
    }

    /// The 1:50,000 series numbered on it.
    pub fn series(self) -> &'static str {
        match self {
            Datum::Wgs84 => "L7018",
            Datum::Indian1975 => "L7017",
        }
    }

    fn geographic(self) -> String {
        match self {
            Datum::Wgs84 => "+proj=longlat +datum=WGS84 +no_defs".into(),
            Datum::Indian1975 => format!("+proj=longlat {INDIAN_1975} +no_defs"),
        }
    }

    fn utm(self, zone: u8) -> String {
        match self {
            Datum::Wgs84 => format!("+proj=utm +zone={zone} +datum=WGS84 +units=m +no_defs"),
            Datum::Indian1975 => format!("+proj=utm +zone={zone} {INDIAN_1975} +units=m +no_defs"),
        }
    }
}

/// Converts between two proj4 systems; geographic ones in degrees.
struct Convert {
    /// The two systems; the definitions are this module's own, so they always parse (a failure
    /// would convert nothing).
    systems: Option<(proj4rs::Proj, proj4rs::Proj)>,
}

impl Convert {
    fn new(from: &str, to: &str) -> Convert {
        let p = |d: &str| proj4rs::Proj::from_proj_string(d).ok();
        Convert { systems: p(from).zip(p(to)) }
    }

    fn run(&self, c: Coord) -> Option<Coord> {
        let (from, to) = self.systems.as_ref()?;
        if !c.is_finite() {
            return None;
        }
        let mut p = if from.is_latlong() { (c.x.to_radians(), c.y.to_radians(), 0.0) } else { (c.x, c.y, 0.0) };
        proj4rs::transform::transform(from, to, &mut p).ok()?;
        let out = if to.is_latlong() { Coord::new(p.0.to_degrees(), p.1.to_degrees()) } else { Coord::new(p.0, p.1) };
        out.is_finite().then_some(out)
    }
}

const WGS84: &str = "+proj=longlat +datum=WGS84 +no_defs";

/// A WGS 84 longitude and latitude on `datum`'s graticule.
pub fn to_datum(c: Coord, datum: Datum) -> Option<Coord> {
    match datum {
        Datum::Wgs84 => Some(c),
        d => Convert::new(WGS84, &d.geographic()).run(c),
    }
}

/// A longitude and latitude on `datum`'s graticule in WGS 84.
pub fn from_datum(c: Coord, datum: Datum) -> Option<Coord> {
    match datum {
        Datum::Wgs84 => Some(c),
        d => Convert::new(&d.geographic(), WGS84).run(c),
    }
}

/// The UTM zone Thai maps put a WGS 84 longitude in: 47 west of 102°E, 48 east of it.
pub fn zone_of(lon: f64) -> u8 {
    if lon < 102.0 { 47 } else { 48 }
}

/// WGS 84 to UTM `zone` on `datum`, in metres.
pub fn to_utm(c: Coord, datum: Datum, zone: u8) -> Option<Coord> {
    Convert::new(WGS84, &datum.utm(zone)).run(c)
}

/// UTM `zone` on `datum` to WGS 84.
pub fn from_utm(m: Coord, datum: Datum, zone: u8) -> Option<Coord> {
    Convert::new(&datum.utm(zone), WGS84).run(m)
}

// ---------------------------------------------------------------------------------------------
// 1:50,000

/// The 30′ blocks of the series, row by row from the north: each holds up to four sheets.
const BLOCKS: &[u16] = &[
    4849, 4949, 5049, 5149, //
    4548, 4648, 4748, 4848, 4948, 5048, 5148, 5248, //
    4547, 4647, 4747, 4847, 4947, 5047, 5147, 5247, //
    4446, 4546, 4646, 4746, 4846, 4946, 5046, 5146, 5246, //
    4445, 4545, 4645, 4745, 4845, 4945, 5045, 5145, 5245, 5345, 5445, 5545, 5645, 5745, 5845, //
    4544, 4644, 4744, 4844, 4944, 5044, 5144, 5244, 5344, 5444, 5544, 5644, 5744, 5844, 5944, //
    4643, 4743, 4843, 4943, 5043, 5143, 5243, 5343, 5443, 5543, 5643, 5743, 5843, 5943, //
    4642, 4742, 4842, 4942, 5042, 5142, 5242, 5342, 5442, 5542, 5642, 5742, 5842, 5942, //
    4741, 4841, 4941, 5041, 5141, 5241, 5341, 5441, 5541, 5641, 5741, 5841, 5941, 6041, //
    4740, 4840, 4940, 5040, 5140, 5240, 5340, 5440, 5540, 5640, 5740, 5840, 5940, 6040, 6140, //
    4639, 4739, 4839, 4939, 5039, 5139, 5239, 5339, 5439, 5539, 5639, 5739, 5839, 5939, 6039, 6139, //
    4638, 4738, 4838, 4938, 5038, 5138, 5238, 5338, 5438, 5538, 5638, 5738, 5838, 5938, 6038, 6138, //
    4737, 4837, 4937, 5037, 5137, 5237, 5337, 5437, 5537, 5637, 5737, 5837, 5937, 6037, //
    4836, 4936, 5036, 5136, 5236, 5336, 5436, 5536, //
    4835, 4935, 5035, 5135, 5235, 5335, 5435, 5535, //
    4834, 4934, 5034, 5134, 5234, 5334, 5434, 5534, //
    4833, 4933, 5433, 5533, //
    4832, 4932, 5432, 5532, //
    4831, 4931, //
    4730, 4830, //
    4729, 4829, //
    4628, 4728, 4828, 4928, //
    4627, 4727, 4827, 4927, //
    4626, 4726, 4826, 4926, 5026, //
    4625, 4725, 4825, 4925, 5025, //
    4624, 4724, 4824, 4924, 5024, //
    4823, 4923, 5023, 5123, //
    4822, 4922, 5022, 5122, 5222, 5322, //
    5021, 5121, 5221, 5321, 5421, //
    5220, 5320,
];

/// The quarters of those blocks the series has no sheet for (sea, or another country), as
/// `CCRR * 10 + quarter`.
const MISSING: &[u32] = &[
    48494, 51491, 51492, 51494, 47484, 46481, 45481, 45483, 45484, 51481, 52481, 52484, 45473, 45474, 44461, 44463, 44464, 44452, 44453,
    44454, 52451, 52452, 53451, 53453, 53454, 54451, 54454, 55451, 55453, 55454, 58451, 58452, 45443, 59441, 59442, 59444, 46433, 46422,
    46423, 46424, 60411, 61401, 61402, 46394, 61391, 61392, 46383, 61381, 61382, 47373, 56372, 57372, 57373, 58372, 58373, 59372, 59373,
    60372, 55362, 50352, 51353, 51354, 55351, 55352, 55354, 48343, 50341, 50342, 50343, 51343, 55341, 55342, 48333, 48334, 54333, 48321,
    48323, 48324, 49322, 54322, 54323, 54324, 48314, 49311, 49312, 49313, 47303, 47304, 48291, 48292, 46281, 46283, 46284, 48281, 48282,
    49281, 49284, 46273, 46274, 48271, 46264, 50261, 50262, 50264, 46253, 46254, 50251, 46242, 46243, 46244, 47243, 48233, 51231, 51232,
    51234, 48221, 48223, 48224, 53221, 53222, 50211, 50212, 50213, 51212, 51213, 54211, 54212, 52202, 53202, 53203,
];

/// Sheets off the regular grid, to hold the land along the coast of the south and the Myanmar
/// border: `(CCRR * 10 + quarter, east, north, width, height)`, the shift of the north-west corner
/// and the size in minutes.
const SHIFTED: &[(u32, f64, f64, f64, f64)] = &[
    (46271, 3.0, 0.0, 15.0, 15.0),
    (47274, 3.0, 0.0, 15.0, 15.0),
    (47271, 3.0, 0.0, 15.0, 15.0),
    (48274, 3.0, 0.0, 18.0, 15.0),
    (49271, 3.0, 0.0, 17.0, 15.0),
    (49274, 0.0, 0.0, 18.0, 15.0),
    (48284, -5.0, 0.0, 20.0, 15.0),
    (47284, -5.0, 0.0, 15.0, 15.0),
    (47281, -5.0, 0.0, 15.0, 15.0),
    (49282, 6.0, 3.0 + 16.0 / 60.0, 15.0, 18.0 + 16.0 / 60.0),
    (49283, 0.0, 3.0 + 16.0 / 60.0, 15.0, 18.0 + 16.0 / 60.0),
    (48331, 5.0, 0.0, 15.0, 15.0),
    (48332, 5.0, 0.0, 15.0, 15.0),
    (49331, 5.0, 0.0, 15.0, 15.0),
    (49332, 5.0, 0.0, 15.0, 15.0),
    (49333, 5.0, 0.0, 15.0, 15.0),
    (49334, 5.0, 0.0, 15.0, 15.0),
    (48322, 5.0, 0.0, 15.0, 15.0),
    (49323, 5.0, 0.0, 15.0, 15.0),
    (48313, 5.0, 0.0, 15.0, 15.0),
    (48312, 5.0, 0.0, 15.0, 15.0),
    (47301, 5.0, 0.0, 15.0, 15.0),
    (48304, 5.0, 0.0, 15.0, 15.0),
    (48301, 5.0, 0.0, 15.0, 15.0),
    (47294, 7.5, 0.0, 15.0, 15.0),
    (47291, 7.5, 0.0, 15.0, 15.0),
    (48294, 7.5, 0.0, 15.0, 15.0),
    (48222, -7.0, -5.0, 15.0, 15.0),
];

/// A 1:50,000 sheet: its frame on its datum's graticule, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sheet50k {
    /// The 30′ block, `CCRR`.
    pub block: u16,
    /// The quarter, 1 (I) to 4 (IV).
    pub quarter: u8,
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
    /// Whether the series has the sheet (not one of the regular grid's over the sea or abroad).
    pub in_series: bool,
}

const ROMAN: [&str; 4] = ["I", "II", "III", "IV"];

impl Sheet50k {
    /// `5541 I`.
    pub fn label(&self) -> String {
        format!("{:04} {}", self.block, ROMAN[(self.quarter.clamp(1, 4) - 1) as usize])
    }

    /// The regular grid's sheet `block`, `quarter`.
    fn regular(block: u16, quarter: u8) -> Sheet50k {
        let (col, row) = ((block / 100) as f64, (block % 100) as f64);
        let (mut west, mut north) = (75.0 + col / 2.0, (row - 8.0) / 2.0);
        if matches!(quarter, 1 | 2) {
            west += 0.25;
        }
        if matches!(quarter, 2 | 3) {
            north -= 0.25;
        }
        Sheet50k { block, quarter, west, south: north - 0.25, east: west + 0.25, north, in_series: false }
    }

    fn contains(&self, c: Coord) -> bool {
        c.x >= self.west && c.x < self.east && c.y >= self.south && c.y < self.north
    }

    /// Its frame in WGS 84, the edges followed in steps of a minute.
    pub fn ring_wgs84(&self, datum: Datum) -> Vec<Coord> {
        let (w, s, e, n) = (self.west, self.south, self.east, self.north);
        let corners = [Coord::new(w, s), Coord::new(e, s), Coord::new(e, n), Coord::new(w, n)];
        let geo = Convert::new(&datum.geographic(), WGS84);
        let steps = 15;
        let mut ring = Vec::with_capacity(4 * steps + 1);
        for i in 0..4 {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            for k in 0..steps {
                let t = k as f64 / steps as f64;
                let p = Coord::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                ring.push(if datum == Datum::Wgs84 { p } else { geo.run(p).unwrap_or(p) });
            }
        }
        ring.push(ring[0]);
        ring
    }
}

/// The series' 830 sheets.
pub fn sheets_50k() -> &'static [Sheet50k] {
    static ALL: std::sync::OnceLock<Vec<Sheet50k>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let mut v = Vec::with_capacity(830);
        for &block in BLOCKS {
            for quarter in 1..=4u8 {
                let key = block as u32 * 10 + quarter as u32;
                if MISSING.contains(&key) {
                    continue;
                }
                let mut s = Sheet50k { in_series: true, ..Sheet50k::regular(block, quarter) };
                if let Some(&(_, east, north, width, height)) = SHIFTED.iter().find(|t| t.0 == key) {
                    let (w, n) = (s.west + east / 60.0, s.north + north / 60.0);
                    s = Sheet50k { west: w, north: n, east: w + width / 60.0, south: n - height / 60.0, ..s };
                }
                v.push(s);
            }
        }
        v
    })
}

/// The 1:50,000 sheet a longitude and latitude on the sheets' datum is on: the series' own, or
/// the regular grid's (`in_series` false) where the series has none.
pub fn sheet_50k(c: Coord) -> Sheet50k {
    let col = ((c.x - 75.0) * 2.0).floor();
    let row = ((c.y + 4.5) * 2.0).floor();
    let quarter = match (c.x - (75.0 + col / 2.0) >= 0.25, c.y - (-4.5 + row / 2.0) >= 0.25) {
        (true, true) => 1,
        (true, false) => 2,
        (false, false) => 3,
        (false, true) => 4,
    };
    let block = (col.clamp(0.0, 99.0) * 100.0 + row.clamp(0.0, 99.0)) as u16;
    let all = sheets_50k();
    // The regular sheet when the series has it where it is; else a shifted or widened one over
    // the point.
    all.iter()
        .find(|s| s.block == block && s.quarter == quarter && s.contains(c))
        .or_else(|| all.iter().find(|s| s.contains(c)))
        .copied()
        .unwrap_or_else(|| Sheet50k::regular(block, quarter))
}

/// The series' sheets that meet an extent on their datum's graticule (west, south, east, north).
pub fn sheets_50k_in(area: [f64; 4]) -> Vec<Sheet50k> {
    let [w, s, e, n] = area;
    sheets_50k().iter().filter(|t| t.west < e && t.east > w && t.south < n && t.north > s).copied().collect()
}

// ---------------------------------------------------------------------------------------------
// Cadastral UTM sheets

/// The scale of a cadastral sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    S4000,
    S2000,
    S1000,
    S500,
}

impl Scale {
    pub const ALL: [Scale; 4] = [Scale::S4000, Scale::S2000, Scale::S1000, Scale::S500];

    /// Reads 4000, 2000, 1000 or 500 (or `1:4000`, `1:4,000`).
    pub fn parse(s: &str) -> Result<Scale, String> {
        let d: String = s.rsplit(':').next().unwrap_or("").chars().filter(char::is_ascii_digit).collect();
        match d.as_str() {
            "4000" => Ok(Scale::S4000),
            "2000" => Ok(Scale::S2000),
            "1000" => Ok(Scale::S1000),
            "500" => Ok(Scale::S500),
            _ => Err(format!("'{s}': a cadastral sheet's scale is 4000, 2000, 1000 or 500")),
        }
    }

    pub fn denominator(self) -> u32 {
        match self {
            Scale::S4000 => 4000,
            Scale::S2000 => 2000,
            Scale::S1000 => 1000,
            Scale::S500 => 500,
        }
    }

    /// `1:4,000`.
    pub fn label(self) -> &'static str {
        match self {
            Scale::S4000 => "1:4,000",
            Scale::S2000 => "1:2,000",
            Scale::S1000 => "1:1,000",
            Scale::S500 => "1:500",
        }
    }

    /// The side of a sheet on the ground, in metres: 50 cm on paper.
    pub fn side(self) -> f64 {
        self.denominator() as f64 / 2.0
    }

    /// Sheets along a side of the 1:4,000 sheet.
    fn per_side(self) -> u32 {
        4000 / self.denominator()
    }
}

/// The side of a 1:4,000 sheet, in metres.
const SIDE_4000: f64 = 2000.0;

/// A cadastral sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UtmSheet {
    pub datum: Datum,
    pub zone: u8,
    pub scale: Scale,
    /// The 1:4,000 sheet's lower-left corner, in metres.
    pub e4000: f64,
    pub n4000: f64,
    /// This sheet's lower-left corner, in metres.
    pub x: f64,
    pub y: f64,
    /// Its number within the 1:4,000 sheet, from 1 at the top left (none at 1:4,000).
    pub part: Option<u32>,
    /// The 1:50,000 sheet the 1:4,000 sheet's lower-left corner is on.
    pub sheet50k: Sheet50k,
}

impl UtmSheet {
    /// `5638 II 2606` at 1:4,000, `5638 II 2606-5` below it.
    pub fn label(&self) -> String {
        let km = |m: f64| ((m / 1000.0).round() as i64).rem_euclid(100);
        let base = format!("{} {:02}{:02}", self.sheet50k.label(), km(self.e4000), km(self.n4000));
        match self.part {
            Some(k) => format!("{base}-{k}"),
            None => base,
        }
    }

    /// Its frame in WGS 84, the edges followed in steps of an eighth.
    pub fn ring_wgs84(&self) -> Vec<Coord> {
        let side = self.scale.side();
        let (x0, y0) = (self.x, self.y);
        let corners = [Coord::new(x0, y0), Coord::new(x0 + side, y0), Coord::new(x0 + side, y0 + side), Coord::new(x0, y0 + side)];
        let back = Convert::new(&self.datum.utm(self.zone), WGS84);
        let steps = 8;
        let mut ring = Vec::with_capacity(4 * steps + 1);
        for i in 0..4 {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            for k in 0..steps {
                let t = k as f64 / steps as f64;
                if let Some(p) = back.run(Coord::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)) {
                    ring.push(p);
                }
            }
        }
        if let Some(&first) = ring.first() {
            ring.push(first);
        }
        ring
    }
}

/// The cadastral sheet at a UTM point on `datum`, `zone`.
fn utm_sheet_at_metres(m: Coord, datum: Datum, zone: u8, scale: Scale) -> Option<UtmSheet> {
    let e4000 = (m.x / SIDE_4000).floor() * SIDE_4000;
    let n4000 = (m.y / SIDE_4000).floor() * SIDE_4000;
    let side = scale.side();
    let (x, y) = ((m.x / side).floor() * side, (m.y / side).floor() * side);
    let part = (scale != Scale::S4000).then(|| {
        let k = scale.per_side();
        let col = (((x - e4000) / side).round() as u32).min(k - 1);
        let from_top = k - 1 - (((y - n4000) / side).round() as u32).min(k - 1);
        from_top * k + col + 1
    });
    // The corner on the datum's graticule, for the 1:50,000 sheet it is on.
    let corner = Convert::new(&datum.utm(zone), &datum.geographic()).run(Coord::new(e4000, n4000))?;
    Some(UtmSheet { datum, zone, scale, e4000, n4000, x, y, part, sheet50k: sheet_50k(corner) })
}

/// The cadastral sheet at a WGS 84 point, numbered on `datum` in `zone` (by default the one its
/// longitude is in).
pub fn utm_sheet(c: Coord, datum: Datum, scale: Scale, zone: Option<u8>) -> Option<UtmSheet> {
    let zone = zone.unwrap_or_else(|| zone_of(c.x));
    utm_sheet_at_metres(to_utm(c, datum, zone)?, datum, zone, scale)
}

/// The cadastral sheets that meet a WGS 84 extent (west, south, east, north), at most `max` (an
/// error past it).
pub fn utm_sheets_in(area: [f64; 4], datum: Datum, scale: Scale, zone: Option<u8>, max: usize) -> Result<Vec<UtmSheet>, String> {
    let [w, s, e, n] = area;
    let zone = zone.unwrap_or_else(|| zone_of((w + e) / 2.0));
    // The extent's outline in UTM, which a square of lon/lat bends a little.
    let mut lo = Coord::new(f64::INFINITY, f64::INFINITY);
    let mut hi = Coord::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for i in 0..=8 {
        let t = i as f64 / 8.0;
        for p in
            [Coord::new(w + (e - w) * t, s), Coord::new(w + (e - w) * t, n), Coord::new(w, s + (n - s) * t), Coord::new(e, s + (n - s) * t)]
        {
            let m = to_utm(p, datum, zone).ok_or("the area is outside the UTM zone")?;
            lo = Coord::new(lo.x.min(m.x), lo.y.min(m.y));
            hi = Coord::new(hi.x.max(m.x), hi.y.max(m.y));
        }
    }
    let side = scale.side();
    let (x0, y0) = ((lo.x / side).floor() * side, (lo.y / side).floor() * side);
    let cols = ((hi.x - x0) / side).ceil().max(1.0);
    let rows = ((hi.y - y0) / side).ceil().max(1.0);
    if cols * rows > max as f64 {
        return Err(format!(
            "the area holds about {} sheets at {}, more than {max}: zoom in, or pick a smaller scale",
            (cols * rows) as u64,
            scale.label()
        ));
    }
    let mut out = Vec::with_capacity((cols * rows) as usize);
    for r in 0..rows as u32 {
        for c in 0..cols as u32 {
            let centre = Coord::new(x0 + (c as f64 + 0.5) * side, y0 + (r as f64 + 0.5) * side);
            if let Some(sheet) = utm_sheet_at_metres(centre, datum, zone, scale) {
                out.push(sheet);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_series_has_830_sheets_with_khon_kaen_on_5541_i() {
        assert_eq!(sheets_50k().len(), 830);
        // Andreas Hoerstemeier's check: Khon Kaen.
        let s = sheet_50k(Coord::new(102.8360, 16.4424));
        assert_eq!(s.label(), "5541 I");
        assert!(s.in_series);
        assert_eq!((s.west, s.south, s.east, s.north), (102.75, 16.25, 103.0, 16.5));
        // The quarters go clockwise from the north-east.
        assert_eq!(sheet_50k(Coord::new(102.9, 16.1)).label(), "5541 II");
        assert_eq!(sheet_50k(Coord::new(102.6, 16.1)).label(), "5541 III");
        assert_eq!(sheet_50k(Coord::new(102.6, 16.4)).label(), "5541 IV");
        // Ayutthaya town, on 5137 IV.
        assert_eq!(sheet_50k(Coord::new(100.57, 14.35)).label(), "5137 IV");
        // Chiang Mai town.
        assert_eq!(sheet_50k(Coord::new(98.985, 18.79)).label(), "4746 I");
        // The sea west of Phuket: the regular grid's number, not one of the series.
        let sea = sheet_50k(Coord::new(97.3, 7.9));
        assert!(!sea.in_series);
    }

    #[test]
    fn shifted_sheets_hold_their_coast() {
        // 4833 I is 5′ east of the regular grid, over Prachuap Khiri Khan's coast.
        let s = sheets_50k().iter().find(|s| s.block == 4833 && s.quarter == 1).unwrap();
        assert!((s.west - (99.25 + 5.0 / 60.0)).abs() < 1e-9);
        // A point on its east strip, past the regular sheet, is still on it.
        assert_eq!(sheet_50k(Coord::new(99.25 + 15.0 / 60.0 + 2.0 / 60.0, 12.4)).label(), "4833 I");
        // 4928 II is taller: 18′16″.
        let t = sheets_50k().iter().find(|s| s.block == 4928 && s.quarter == 2).unwrap();
        assert!(((t.north - t.south) * 60.0 - (18.0 + 16.0 / 60.0)).abs() < 1e-9);
        // Every sheet meets the regular grid's sheet of its number.
        for s in sheets_50k() {
            let r = Sheet50k::regular(s.block, s.quarter);
            assert!(s.west < r.east && s.east > r.west && s.south < r.north && s.north > r.south, "{}", s.label());
        }
    }

    #[test]
    fn cadastral_sheets_follow_the_department_of_lands() {
        // A Department of Lands record in Ayutthaya: 5137 IV 6888, the 2 km square whose
        // lower-left corner is at 668 km E, 1588 km N (UTM zone 47).
        let centre = from_utm(Coord::new(669_000.0, 1_589_000.0), Datum::Wgs84, 47).unwrap();
        let s = utm_sheet(centre, Datum::Wgs84, Scale::S4000, None).unwrap();
        assert_eq!(s.zone, 47);
        assert_eq!((s.e4000, s.n4000), (668_000.0, 1_588_000.0));
        assert_eq!(s.label(), "5137 IV 6888");
        // The regulation's example: 326–328 km E, 1606–1608 km N on a sheet of 5638 II.
        let m = Coord::new(326_100.0, 1_607_900.0);
        let c = from_utm(m, Datum::Indian1975, 48).unwrap();
        let one = utm_sheet(c, Datum::Indian1975, Scale::S1000, Some(48)).unwrap();
        assert_eq!(one.label(), "5638 II 2606-1");
        assert_eq!(utm_sheet(c, Datum::Indian1975, Scale::S4000, Some(48)).unwrap().label(), "5638 II 2606");
        // From the top left, row by row.
        let at = |dx: f64, dy: f64, scale| {
            let c = from_utm(Coord::new(326_000.0 + dx, 1_606_000.0 + dy), Datum::Indian1975, 48).unwrap();
            utm_sheet(c, Datum::Indian1975, scale, Some(48)).unwrap().part
        };
        assert_eq!(at(100.0, 1900.0, Scale::S2000), Some(1));
        assert_eq!(at(1900.0, 1900.0, Scale::S2000), Some(2));
        assert_eq!(at(100.0, 100.0, Scale::S2000), Some(3));
        assert_eq!(at(1900.0, 100.0, Scale::S2000), Some(4));
        assert_eq!(at(100.0, 1400.0, Scale::S1000), Some(5));
        assert_eq!(at(1900.0, 100.0, Scale::S1000), Some(16));
        assert_eq!(at(1900.0, 100.0, Scale::S500), Some(64));
        assert_eq!(at(300.0, 1900.0, Scale::S500), Some(2));
        assert_eq!(at(100.0, 100.0, Scale::S4000), None);
    }

    #[test]
    fn indian_1975_sheets_sit_a_few_hundred_metres_off_wgs_84() {
        let c = Coord::new(100.5, 13.75);
        let w = to_utm(c, Datum::Wgs84, 47).unwrap();
        let i = to_utm(c, Datum::Indian1975, 47).unwrap();
        let d = ((w.x - i.x).powi(2) + (w.y - i.y).powi(2)).sqrt();
        assert!((200.0..600.0).contains(&d), "{d}");
        let back = from_datum(to_datum(c, Datum::Indian1975).unwrap(), Datum::Indian1975).unwrap();
        assert!((back.x - c.x).abs() < 1e-7 && (back.y - c.y).abs() < 1e-7);
    }

    #[test]
    fn grids_count_their_sheets_and_refuse_too_many() {
        // A 1:4,000 sheet's worth of ground at 1:1,000: about 16 sheets, 25 with the edges.
        let c = from_utm(Coord::new(669_000.0, 1_589_000.0), Datum::Wgs84, 47).unwrap();
        let d = 0.008;
        let v = utm_sheets_in([c.x - d, c.y - d, c.x + d, c.y + d], Datum::Wgs84, Scale::S1000, None, 100).unwrap();
        assert!((9..=25).contains(&v.len()), "{}", v.len());
        assert!(v.iter().all(|s| s.ring_wgs84().len() == 33));
        assert!(utm_sheets_in([99.0, 13.0, 101.0, 15.0], Datum::Wgs84, Scale::S500, None, 5000).is_err());
        let fifty = sheets_50k_in([100.0, 13.5, 101.0, 14.5]);
        assert_eq!(fifty.len(), 16);
        assert_eq!(fifty[0].ring_wgs84(Datum::Indian1975).len(), 61);
    }

    #[test]
    fn names_parse() {
        assert_eq!(Datum::parse("Indian 1975"), Ok(Datum::Indian1975));
        assert_eq!(Datum::parse("WGS84"), Ok(Datum::Wgs84));
        assert!(Datum::parse("everest").is_err());
        assert_eq!(Scale::parse("1:4,000"), Ok(Scale::S4000));
        assert_eq!(Scale::parse("500"), Ok(Scale::S500));
        assert!(Scale::parse("1:5000").is_err());
    }
}
