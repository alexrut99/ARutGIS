//! Thailand's administrative boundaries (provinces, districts, subdistricts) with Thai names, from
//! two sources the user picks between:
//!
//! - [`Source::Dopa`]: OpenGISData-Thailand, from the Department of Provincial Administration
//!   (กรมการปกครอง, DOPA), CC BY 4.0. Read from its GitHub repository at a fixed commit.
//! - [`Source::Rtsd`]: the Royal Thai Survey Department's (กรมแผนที่ทหาร, RTSD) common operational
//!   dataset on OCHA's Humanitarian Data Exchange, CC BY-IGO. Converted to GeoJSON and simplified by
//!   `.github/workflows/thai-boundaries-data.yml`, kept on ARutGIS's `boundaries-data` branch.
//!
//! Both are for mapping and analysis, not legal boundaries. Requests are built here; the fetching
//! is the caller's.

/// Where a source's files are read from.
const DOPA_BASE: &str = "https://raw.githubusercontent.com/chingchai/OpenGISData-Thailand/fafcea179cda88b689f5697c3866a449e79cd298";
const RTSD_BASE: &str = "https://raw.githubusercontent.com/alexrut99/ARutGIS/boundaries-data/rtsd";

/// Who made a set of boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// กรมการปกครอง, through OpenGISData-Thailand.
    Dopa,
    /// กรมแผนที่ทหาร, through OCHA HDX.
    Rtsd,
}

/// A level of Thailand's administration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// จังหวัด
    Province,
    /// อำเภอ / เขต
    District,
    /// ตำบล / แขวง
    Subdistrict,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Dopa, Source::Rtsd];

    /// Reads `dopa` or `rtsd`.
    pub fn parse(s: &str) -> Result<Source, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dopa" => Ok(Source::Dopa),
            "rtsd" => Ok(Source::Rtsd),
            other => Err(format!("'{other}': a source is dopa (กรมการปกครอง) or rtsd (กรมแผนที่ทหาร)")),
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Source::Dopa => "dopa",
            Source::Rtsd => "rtsd",
        }
    }

    /// The agency, in Thai, for a layer's name.
    pub fn agency(self) -> &'static str {
        match self {
            Source::Dopa => "กรมการปกครอง",
            Source::Rtsd => "กรมแผนที่ทหาร",
        }
    }

    /// Who made it, through whom, and on what terms: shown beside the choice and kept with the
    /// layer.
    pub fn attribution(self) -> &'static str {
        match self {
            Source::Dopa => {
                "Department of Provincial Administration (DOPA), via OpenGISData-Thailand (github.com/chingchai/OpenGISData-Thailand), CC BY 4.0"
            }
            Source::Rtsd => {
                "Royal Thai Survey Department (RTSD), via OCHA ROAP on the Humanitarian Data Exchange (data.humdata.org/dataset/cod-ab-tha), CC BY-IGO"
            }
        }
    }
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Province, Level::District, Level::Subdistrict];

    /// Reads `province`, `district` or `subdistrict` (or 1, 2, 3).
    pub fn parse(s: &str) -> Result<Level, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "province" | "1" | "adm1" => Ok(Level::Province),
            "district" | "2" | "adm2" => Ok(Level::District),
            "subdistrict" | "3" | "adm3" => Ok(Level::Subdistrict),
            other => Err(format!("'{other}': a level is province, district or subdistrict")),
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Level::Province => "province",
            Level::District => "district",
            Level::Subdistrict => "subdistrict",
        }
    }

    /// The level's name in Thai, for a layer's name.
    pub fn thai(self) -> &'static str {
        match self {
            Level::Province => "จังหวัด",
            Level::District => "อำเภอ",
            Level::Subdistrict => "ตำบล",
        }
    }

    fn adm(self) -> u8 {
        match self {
            Level::Province => 1,
            Level::District => 2,
            Level::Subdistrict => 3,
        }
    }
}

/// The GeoJSON file of `source`'s `level`.
pub fn url(source: Source, level: Level) -> String {
    match (source, level) {
        (Source::Dopa, Level::Province) => format!("{DOPA_BASE}/provinces.geojson"),
        (Source::Dopa, Level::District) => format!("{DOPA_BASE}/districts.geojson"),
        (Source::Dopa, Level::Subdistrict) => format!("{DOPA_BASE}/subdistricts.geojson"),
        (Source::Rtsd, l) => format!("{RTSD_BASE}/adm{}.geojson", l.adm()),
    }
}

/// The field holding the Thai name at `level`, which the layer is labelled by.
pub fn name_field(source: Source, level: Level) -> String {
    match (source, level) {
        (Source::Dopa, Level::Province) => "pro_th".into(),
        (Source::Dopa, Level::District) => "amp_th".into(),
        (Source::Dopa, Level::Subdistrict) => "tam_th".into(),
        // HDX's current schema: adm1_name (English), adm1_name1 (Thai), adm1_pcode.
        (Source::Rtsd, l) => format!("adm{}_name1", l.adm()),
    }
}

/// The layer's name: the level and the agency, in Thai (`ตำบล (กรมการปกครอง)`).
pub fn layer_name(source: Source, level: Level) -> String {
    format!("{} ({})", level.thai(), source.agency())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_and_levels_name_their_files_and_fields() {
        assert_eq!(Source::parse("DOPA"), Ok(Source::Dopa));
        assert!(Source::parse("osm").is_err());
        assert_eq!(Level::parse("3"), Ok(Level::Subdistrict));
        assert!(Level::parse("village").is_err());
        assert!(url(Source::Dopa, Level::Subdistrict).ends_with("/subdistricts.geojson"));
        assert!(url(Source::Rtsd, Level::Province).ends_with("/rtsd/adm1.geojson"));
        assert_eq!(name_field(Source::Rtsd, Level::District), "adm2_name1");
        assert_eq!(name_field(Source::Dopa, Level::Province), "pro_th");
        assert_eq!(layer_name(Source::Dopa, Level::Subdistrict), "ตำบล (กรมการปกครอง)");
        for s in Source::ALL {
            assert!(url(s, Level::Province).starts_with("https://raw.githubusercontent.com/"));
            assert!(s.attribution().contains("CC BY"));
        }
    }
}
