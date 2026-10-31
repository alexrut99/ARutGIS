//! The world's elevation for a terrain any map can turn on with one click: Mapzen's Terrain Tiles
//! (Terrarium PNGs, about 30 m over Thailand from SRTM), kept by AWS's Registry of Open Data
//! (registry.opendata.aws/terrain-tiles). Their sources ask for the credit in [`ATTRIBUTION`] on
//! the map, and in full ([`ATTRIBUTION_FULL`]) where the sources are listed.

/// The tiles, read as `map.terrain`'s `tiles` with the Terrarium encoding.
pub const WORLD_TERRAIN_URL: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png";

/// The short credit, drawn on the map while its terrain comes from [`WORLD_TERRAIN_URL`].
pub const ATTRIBUTION: &str = "Terrain Tiles: Mapzen, AWS Open Data; SRTM, GMTED2010 (USGS), ETOPO1 (NOAA) and others";

/// The credit the tiles' sources require (github.com/tilezen/joerd/blob/master/docs/attribution.md).
pub const ATTRIBUTION_FULL: &str = "Mapzen. \
ArcticDEM terrain data DEM(s) were created from DigitalGlobe, Inc., imagery and funded under National Science Foundation awards 1043681, 1559691, and 1542736; \
Australia terrain data © Commonwealth of Australia (Geoscience Australia) 2017; \
Austria terrain data © offene Daten Österreichs – Digitales Geländemodell (DGM) Österreich; \
Canada terrain data contains information licensed under the Open Government Licence – Canada; \
Europe terrain data produced using Copernicus data and information funded by the European Union - EU-DEM layers; \
Global ETOPO1 terrain data U.S. National Oceanic and Atmospheric Administration; \
Mexico terrain data source: INEGI, Continental relief, 2016; \
New Zealand terrain data Copyright 2011 Crown copyright (c) Land Information New Zealand and the New Zealand Government (All rights reserved); \
Norway terrain data © Kartverket; \
United Kingdom terrain data © Environment Agency copyright and/or database right 2015. All rights reserved; \
United States 3DEP (formerly NED) and global GMTED2010 and SRTM terrain data courtesy of the U.S. Geological Survey.";

/// The credit owed for a terrain read from `url`, if it is the world's.
pub fn attribution(url: &str) -> Option<&'static str> {
    (url == WORLD_TERRAIN_URL).then_some(ATTRIBUTION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_world_terrain_is_an_xyz_url_with_its_credit() {
        for k in ["{z}", "{x}", "{y}"] {
            assert!(WORLD_TERRAIN_URL.contains(k));
        }
        assert!(WORLD_TERRAIN_URL.starts_with("https://"));
        assert_eq!(attribution(WORLD_TERRAIN_URL), Some(ATTRIBUTION));
        assert_eq!(attribution("https://e.com/{z}/{x}/{y}.png"), None);
        assert!(ATTRIBUTION_FULL.contains("SRTM") && ATTRIBUTION.contains("SRTM"));
    }
}
