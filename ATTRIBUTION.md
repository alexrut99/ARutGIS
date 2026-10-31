# Attribution

Every asset file in this repository, with its source and licence. The licences of the Rust
crates RustGIS is built from, with their copyright notices, are in
[THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt) (the Python package's in
[apps/rustgis-python/THIRD-PARTY-LICENSES.txt](apps/rustgis-python/THIRD-PARTY-LICENSES.txt)),
which every package ships next to LICENSE. They are generated from `Cargo.lock` by
`cargo xtask notices` (it needs `cargo install --locked --features cli cargo-about@0.9.2`);
`deny.toml` lists the licences a dependency may have.

| File | Author / source | Licence |
|---|---|---|
| Noto Sans Thai (`apps/rustgis-web/fonts/NotoSansThai-Regular.ttf`, `NotoSansThai-Bold.ttf`), served with the web app for Thai text | Google ([notofonts/thai](https://github.com/notofonts/thai)), version 2.000 | OFL-1.1 (`apps/rustgis-web/fonts/OFL.txt`) |
| Sarabun (`crates/render/fonts/Sarabun-Regular.ttf`, `Sarabun-Bold.ttf`), built into the app for Thai map layouts | Cadson Demak, via [Google Fonts](https://github.com/google/fonts/tree/main/ofl/sarabun) | OFL-1.1 (`crates/render/fonts/OFL.txt`) |
| Web app icons (`apps/rustgis-web/icons/`) | The ARutGIS logo, drawn by `rustgis-cli logo` | Apache-2.0 |
| ARutGIS logo (`docs/assets/arutgis-logo.svg`, and drawn in code in `crates/render/src/logo.rs`) | Made for ARutGIS | Apache-2.0 |
| Sample data (`countries`, `countries_chn`, `cities`, `rivers`, `lakes` GeoJSON), embedded at build time from [rustgis-assets](https://github.com/opengeos/rustgis-assets/tree/main/data) | Natural Earth 1:110m, attributes trimmed; `countries_chn` is Natural Earth's China point of view (1:10m, simplified) | Public domain |
| QGIS screenshot (`qgis-world-atlas.webp` in [rustgis-assets](https://github.com/opengeos/rustgis-assets/tree/main/images)), on the [RustGIS and QGIS](docs/comparison/qgis.md) page | Taken by the RustGIS project: [QGIS](https://qgis.org) 4.2 showing the Natural Earth sample data above | QGIS is GPL-2.0-or-later; the data is public domain |
| QGIS test corpus (`crates/map/testdata/qgis/generated/`: styles, projects and their sample data) | Dominik Strebel ([@pick2510](https://github.com/pick2510)) for the RustGIS project: written by [QGIS](https://qgis.org) 4.2.3 through its API, from `generate.py` there; the data is made up | Apache-2.0 |
| Analysis reference corpus (`crates/analysis/tests/reference/`: layers and expected results) | Written for the RustGIS project from `generate.py` there: the layers are made up; the expected results were computed by [GEOS](https://libgeos.org) 3.13.1 through [Shapely](https://shapely.readthedocs.io) 2.1.2, and distances by GeographicLib in [PROJ](https://proj.org) 9.5.1 through [pyproj](https://pyproj4.github.io/pyproj/) 3.7.2, run only to write them (GEOS is LGPL-2.1, Shapely BSD-3-Clause, PROJ and pyproj MIT; none is included in or linked into RustGIS) | Apache-2.0 |

Raster coordinate systems are looked up by EPSG code in the PROJ definitions of the
[crs-definitions](https://crates.io/crates/crs-definitions) crate (CC0-1.0), compiled into the
app. The codes it lacks (EPSG 9020–20003, `crates/data/src/raster/epsg_gap.rs`) are from the
[EPSG Geodetic Parameter Dataset](https://epsg.org) v12.029 (© IOGP, under its
[terms of use](https://epsg.org/terms-of-use.html)), written as PROJ definitions by PROJ 9.8.
The Equi7 grids' codes (EPSG 27701–27707, in the same file) are from the dataset's v13.106,
written as PROJ definitions of its Azimuthal Equidistant method.
The axis order of those systems (`crates/data/src/raster/epsg_axes.rs`: which are written north
or latitude first) is from the EPSG Geodetic Parameter Dataset v11.022 (© IOGP, under its terms
of use), read from the PROJ 9.5.1 database through pyproj 3.7.2.
The map's ESRI codes (`crates/data/src/raster/esri.rs`) are Esri's projected coordinate systems
from Esri's [projection-engine-db-doc](https://github.com/Esri/projection-engine-db-doc)
(Apache-2.0), as imported into the [PROJ](https://proj.org) 9.7.1 database (MIT) and written as
PROJ definitions by PROJ 9.7.1.
The WKT2 test corpus (`crates/data/src/raster/wkt2_corpus.tsv`) is 60 of those EPSG and Esri
coordinate systems written as WKT2 by PROJ 9.7.1, through pyproj 3.7.2.
The ESRI datum test corpus (`crates/data/src/raster/esri_datum_corpus.tsv`) contains 13 EPSG
coordinate systems written as ESRI WKT1 through pyproj 3.7.2; reference coordinates use PROJ
with the bundled `crs-definitions` Helmert definitions. The OSGB36, Amersfoort and GGRS87 WKT2
reference coordinates use those same transformations.

The GeoLibre tool suite ([geolibre-rust](https://github.com/opengeos/geolibre-rust), MIT, built
on [whitebox-wasm](https://github.com/opengeos/whitebox-wasm), MIT OR Apache-2.0) is not part of
RustGIS: the desktop app and command line run its separately downloaded `geolibre` runner, and
the web app loads its WebAssembly runner (the `geolibre-wasm` npm package, from jsDelivr, which
carries its own licence and third-party notices) when asked. Its tool names, descriptions and
categories come from the runner's manifests.

The scientific colour ramps (`crates/map/src/color/scientific.rs`) are sampled from published
colour maps: Viridis, Plasma, Inferno and Magma by Stéfan van der Walt and Nathaniel Smith (CC0),
and Cividis by Jamie R. Nuñez, Christopher R. Anderton and Ryan S. Renslow, as
[matplotlib](https://matplotlib.org) ships them (matplotlib licence); Turbo by Anton Mikhailov,
Copyright 2019 Google LLC (Apache-2.0); Batlow, Vik, Roma and Oleron from Fabio Crameri's
[Scientific colour maps](https://www.fabiocrameri.ch/colourmaps/), Copyright (c) 2020 Fabio
Crameri (MIT, through [cmcrameri](https://github.com/callumrollo/cmcrameri)); and Balance,
Haline, Deep and Topo from [cmocean](https://matplotlib.org/cmocean/), Copyright (c) 2015
Kristen M. Thyng (MIT). The Matplotlib group's colour maps (`crates/map/src/color/matplotlib.rs`)
are sampled from [rio-tiler](https://github.com/cogeotiff/rio-tiler)'s tables, Copyright (c) 2021
cogeotiff (BSD-3-Clause): matplotlib's own maps (matplotlib licence; its
ColorBrewer maps, such as Blues and RdYlBu, by Cynthia Brewer, Apache-2.0), cmocean's (MIT, as
above) and rio-tiler's cfastie, rplumbo and schwarzwald. The MIT licence's terms: permission is granted, free of charge, to use,
copy, modify, merge, publish, distribute, sublicense and sell copies, provided the copyright
notice and permission notice are included; the software is provided "as is", without warranty
of any kind.

Icons are original vector drawings in code (`crates/ui-egui/src/icons.rs`). Fonts are not
bundled: the interface uses an installed system sans-serif when one is found, otherwise the
Ubuntu Light face that ships inside `epaint_default_fonts` (Ubuntu Font Licence 1.0).

## Data ARutGIS reads from the web

Not in the repository or the packages: fetched when asked for, with the credit shown in the app.

| Data | Author / source | Licence |
|---|---|---|
| Thai administrative boundaries, กรมการปกครอง (`crates/tiles/src/thai_boundaries.rs`) | Department of Provincial Administration (DOPA), via [OpenGISData-Thailand](https://github.com/chingchai/OpenGISData-Thailand) | CC BY 4.0 |
| Thai administrative boundaries, กรมแผนที่ทหาร (the `boundaries-data` branch, from `.github/workflows/thai-boundaries-data.yml`) | Royal Thai Survey Department (RTSD), via OCHA ROAP on the [Humanitarian Data Exchange](https://data.humdata.org/dataset/cod-ab-tha) | CC BY-IGO |
| World elevation for 3D terrain (`crates/tiles/src/elevation.rs`) | [Terrain Tiles](https://registry.opendata.aws/terrain-tiles) by Mapzen on AWS Open Data; over Thailand from SRTM. The credit its sources require ([attribution](https://github.com/tilezen/joerd/blob/master/docs/attribution.md)): Mapzen. ArcticDEM terrain data DEM(s) were created from DigitalGlobe, Inc., imagery and funded under National Science Foundation awards 1043681, 1559691, and 1542736; Australia terrain data © Commonwealth of Australia (Geoscience Australia) 2017; Austria terrain data © offene Daten Österreichs – Digitales Geländemodell (DGM) Österreich; Canada terrain data contains information licensed under the Open Government Licence – Canada; Europe terrain data produced using Copernicus data and information funded by the European Union - EU-DEM layers; Global ETOPO1 terrain data U.S. National Oceanic and Atmospheric Administration; Mexico terrain data source: INEGI, Continental relief, 2016; New Zealand terrain data Copyright 2011 Crown copyright (c) Land Information New Zealand and the New Zealand Government (All rights reserved); Norway terrain data © Kartverket; United Kingdom terrain data © Environment Agency copyright and/or database right 2015. All rights reserved; United States 3DEP (formerly NED) and global GMTED2010 and SRTM terrain data courtesy of the U.S. Geological Survey. | As each source's terms |
