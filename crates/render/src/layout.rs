//! Draws a page layout: the page, its map frames (each a map rendered at the frame's scale and
//! clipped to its shape, with grids and extent indicators), and the surrounds.

use crate::Canvas;
use rustgis_data::GeometryType;
use rustgis_geom::{Coord, Extent};
use rustgis_map::layout::GridEdge as Edge;
use rustgis_map::layout::{
    AreaPatch, ArrowStyle, ColorScale, ElementKind, FontFamily, FrameShape, Grid, GridKind, LabelText, Layout, LegendLook, LineHead,
    LinePatch, MAX_LEGEND_COLUMNS, NumberPlace, Orientation, PT_PER_MM, ScaleBarStyle, ScaleUnits, TextAlign, TickSide, UnitLabel,
    UnitPlace, frame_camera,
};
use rustgis_map::symbology::LegendEntry;
use rustgis_map::{Map, Rgba};
use tiny_skia::{FillRule, LineCap, Mask, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

use crate::{View, paint, point_marks, text};

/// Page values for dynamic text.
#[derive(Clone, Debug, Default)]
pub struct PageInfo {
    /// 1-based page number and count (a map series has many).
    pub page: usize,
    pub pages: usize,
    /// The map series page's name.
    pub page_name: String,
}

/// Largest page drawn, in pixels per side and in all (less in the browser, where memory is
/// tighter): a larger page is drawn at a lower resolution.
pub const MAX_SIDE: f64 = 12_000.0;
pub(crate) const MAX_PIXELS: f64 = if cfg!(target_arch = "wasm32") { 25_000_000.0 } else { 60_000_000.0 };

/// Whether skipping a failed frame would omit required complete-source content.
pub(crate) fn frame_requires_complete(map: &Map, view: &View) -> bool {
    let mut scales = vec![map.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M];
    if view.camera.pitch != 0.0 && !map.projection.is_globe() {
        scales.extend(view.camera.tilt_bands(view.width, view.height).iter().map(|b| map.ground_res_at(&b.camera) / rustgis_geom::PIXEL_M));
    }
    let complete = |source: &rustgis_map::source::SourceLayer| {
        source.chunk_source().is_some() || (!source.is_open() && crate::is_local_shapefile(source))
    };
    map.layers.iter().filter(|l| scales.iter().any(|scale| map.draws_at(l, *scale))).any(|layer| {
        layer.source.as_ref().is_some_and(complete)
            || layer
                .mask
                .as_deref()
                .and_then(|name| {
                    map.layers.iter().find(|m| m.name.eq_ignore_ascii_case(name) && m.geometry_type() == GeometryType::Polygon)
                })
                .and_then(|mask| mask.source.as_ref())
                .is_some_and(complete)
    })
}

/// Renders a layout at `dpi` (dots per inch). `maps` are the project's maps, found by name.
pub fn render(layout: &Layout, maps: &mut [Map], dpi: f64, info: &PageInfo) -> Option<Pixmap> {
    render_pending(layout, maps, dpi, info).map(|(pm, _)| pm)
}

/// [`render`], and whether a map frame on the page drew while some of its tiles, or a read of its
/// streamed layers or rasters, were still on their way: in the browser, which doesn't wait for
/// them, the page is drawn again once they land.
pub fn render_pending(layout: &Layout, maps: &mut [Map], dpi: f64, info: &PageInfo) -> Option<(Pixmap, bool)> {
    let (w, h, k) = page_geometry(layout, dpi);
    let bounds = tiny_skia::IntRect::from_xywh(0, 0, w, h)?;
    let mut pending = false;
    let pm = draw_page(layout, maps, info, k, bounds, None, None, &mut pending)?.into_pixmap();
    Some((pm, pending))
}

/// [`render`] kept as vectors (for a vector PDF): text, shapes, surrounds and flat, north-up map
/// frames as paths and glyphs, what is pixels by nature (rasters, tiles, pictures, a tilted or
/// globe frame) as pictures drawn at `dpi`, in the drawing's pixels at that resolution. With it,
/// the frames (id and name) drawn as pictures because their vectors passed
/// [`crate::vector::MAX_COST`]. A drawing that overflowed all the same (its surrounds) is
/// incomplete: [`crate::vector::Drawing::overflowed`] says so.
pub fn render_drawing(
    layout: &Layout,
    maps: &mut [Map],
    dpi: f64,
    info: &PageInfo,
) -> Option<(crate::vector::Drawing, Vec<(u32, String)>)> {
    let (w, h, k) = page_geometry(layout, dpi);
    let bounds = tiny_skia::IntRect::from_xywh(0, 0, w, h)?;
    let mut pictured = Vec::new();
    let drawing = draw_page(layout, maps, info, k, bounds, None, Some(&mut pictured), &mut false)?.into_drawing()?;
    Some((drawing, pictured))
}

/// Page pixel dimensions and the shared millimetre scale, capped to the renderer's budget.
pub(crate) fn page_geometry(layout: &Layout, dpi: f64) -> (u32, u32, f64) {
    let ppp = (if dpi.is_finite() { dpi } else { 96.0 } / 96.0).clamp(0.25, 8.0);
    let k = PT_PER_MM * ppp;
    let k = k.min((MAX_PIXELS / (layout.width * layout.height).max(1.0)).sqrt());
    let (w, h) = ((layout.width * k).clamp(1.0, MAX_SIDE), (layout.height * k).clamp(1.0, MAX_SIDE));
    (w as u32, h as u32, k.min(w / layout.width).min(h / layout.height))
}

/// One element on transparency, allocating only its on-page pixel bounds.
/// Outer `None` means rendering failed; inner `None` means the element is off-page.
pub(crate) fn render_element(layout: &Layout, id: u32, maps: &mut [Map], dpi: f64, info: &PageInfo) -> Option<Option<(Pixmap, [f64; 4])>> {
    let Some((bounds, k, mm)) = element_bounds(layout, id, dpi)? else { return Some(None) };
    let pm = draw_page(layout, maps, info, k, bounds, Some(id), None, &mut false)?.into_pixmap();
    Some(Some((pm, mm)))
}

/// [`render_element`] kept as vectors (see [`render_drawing`]): the element alone, recorded in
/// the pixels of its on-page bounds at `dpi`, those bounds in millimetres, and the pixels a
/// millimetre. The frames drawn as pictures (past the budget) go to `pictured`.
pub(crate) fn record_element(
    layout: &Layout,
    id: u32,
    maps: &mut [Map],
    dpi: f64,
    info: &PageInfo,
    pictured: &mut Vec<(u32, String)>,
) -> Option<Option<(crate::vector::Drawing, [f64; 4], f64)>> {
    let Some((bounds, k, mm)) = element_bounds(layout, id, dpi)? else { return Some(None) };
    let drawing = draw_page(layout, maps, info, k, bounds, Some(id), Some(pictured), &mut false)?.into_drawing()?;
    Some(Some((drawing, mm, k)))
}

/// Element `id`'s on-page bounds in whole pixels at `dpi`, the pixels a millimetre, and the bounds
/// in millimetres; inner `None` for an element off the page.
fn element_bounds(layout: &Layout, id: u32, dpi: f64) -> Option<Option<(tiny_skia::IntRect, f64, [f64; 4])>> {
    let e = layout.element(id)?;
    let (w, h, k) = page_geometry(layout, dpi);
    // A frame's grid labelled outside it reaches past its edges.
    let m = match &e.kind {
        ElementKind::MapFrame { grid: Some(g), .. } => g.outside_reach(),
        _ => 0.0,
    };
    // A turned element covers its turned box (its labels outside it turn too, further out).
    let (bx, by, bw, bh) = e.bounds();
    let m = if e.rotation != 0.0 { m * std::f64::consts::SQRT_2 } else { m };
    let x0 = ((bx - m) * k).floor().clamp(0.0, f64::from(w));
    let y0 = ((by - m) * k).floor().clamp(0.0, f64::from(h));
    let x1 = ((bx + bw + m) * k).ceil().clamp(0.0, f64::from(w));
    let y1 = ((by + bh + m) * k).ceil().clamp(0.0, f64::from(h));
    let bounds = tiny_skia::IntRect::from_xywh(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32);
    Some(bounds.map(|b| (b, k, [x0 / k, y0 / k, (x1 - x0) / k, (y1 - y0) / k])))
}

/// Draws the selected region with the full layout's frame and dynamic-text context, recorded
/// when `record` holds a list (see [`render_drawing`]), which gets the names of the frames
/// drawn as pictures. `pending` is set when a frame drew while some of what it shows was still on
/// its way (see [`render_pending`]).
fn draw_page(
    layout: &Layout,
    maps: &mut [Map],
    info: &PageInfo,
    k: f64,
    bounds: tiny_skia::IntRect,
    element: Option<u32>,
    mut record: Option<&mut Vec<(u32, String)>>,
    pending: &mut bool,
) -> Option<Canvas> {
    let ppp = k / PT_PER_MM;
    let mut pm =
        if record.is_some() { Canvas::recording(bounds.width(), bounds.height())? } else { Canvas::new(bounds.width(), bounds.height())? };
    if element.is_none() {
        pm.fill(tiny_skia::Color::WHITE);
    }
    let visible = |e: &&rustgis_map::layout::Element| e.visible && element.is_none_or(|id| e.id == id);
    // Frame cameras first: surrounds and extent indicators read them.
    let frames: Vec<(u32, usize, rustgis_map::Camera, f64, f64)> = layout
        .elements
        .iter()
        .filter_map(|e| match &e.kind {
            ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } => {
                let mi = map_index(maps, map)?;
                let m = maps.get(mi)?;
                Some((e.id, mi, frame_camera(m, *center, *scale, *bearing, *pitch), e.w * PT_PER_MM, e.h * PT_PER_MM))
            }
            _ => None,
        })
        .collect();
    let frame = |id: u32| frames.iter().find(|f| f.0 == id);
    // What each legend's frame draws, worked out before any element is drawn: its map is synced
    // to that frame's view first, so a streamed layer holds that frame's features whichever order
    // the frames and legends come in, and whichever other frame showed the map last.
    let mut shown: Vec<(u32, Vec<(u32, Vec<LegendEntry>)>)> = Vec::new();
    for e in layout.elements.iter().filter(visible) {
        if let ElementKind::Legend { frame: f, all_classes: false, .. } = &e.kind
            && !shown.iter().any(|(id, _)| id == f)
            && let Some(&(_, mi, cam, fw, fh)) = frame(*f)
            && let Some(map) = maps.get_mut(mi)
        {
            let view = View { width: fw, height: fh, pixels_per_point: 1.0, camera: cam, sky: false };
            let lonlat = crate::view_lonlat(map, &view);
            let scale = map.ground_res_at(&cam) / rustgis_geom::PIXEL_M;
            map.sync_sources(&lonlat, scale, true);
            shown.push((*f, complete_shown_legends(map, scale)?));
        }
    }
    for e in layout.elements.iter().filter(visible) {
        let (px, py, ew, eh) =
            ((e.x * k - f64::from(bounds.x())) as f32, (e.y * k - f64::from(bounds.y())) as f32, (e.w * k) as f32, (e.h * k) as f32);
        // A turned element is drawn upright on a sheet of its own, with room around it for
        // outlines and labels outside it, and laid on the page turned about its centre.
        let turned = e.rotation != 0.0 && e.kind.turns();
        let pad = if turned {
            match &e.kind {
                ElementKind::MapFrame { grid, .. } => {
                    (grid.as_ref().map_or(0.0, Grid::outside_reach).max(OUTSIDE_LABEL_MM) * k).ceil() as f32 + 2.0
                }
                _ => (30.0 * ppp).ceil() as f32 + 2.0,
            }
        } else {
            0.0
        };
        let mut sheet = if turned {
            let (sw, sh) = ((ew.max(1.0) + 2.0 * pad).ceil(), (eh.max(1.0) + 2.0 * pad).ceil());
            // Too large to draw on a sheet of its own: left out rather than run out of memory.
            if f64::from(sw) * f64::from(sh) > 4.0 * MAX_PIXELS {
                continue;
            }
            // One that records when the page does, so the element stays vectors turned.
            match pm.sub(sw as u32, sh as u32) {
                Some(p) => Some(p),
                None => continue,
            }
        } else {
            None
        };
        let (x, y) = if turned { (pad, pad) } else { (px, py) };
        // What the page has kept so far, for a frame's vectors to fit in with (a sheet keeps none).
        let page_cost = pm.recorded_cost();
        let target: &mut Canvas = match sheet.as_mut() {
            Some(p) => p,
            None => &mut pm,
        };
        let Some(r) = Rect::from_xywh(x, y, ew.max(1.0), eh.max(1.0)) else { continue };
        match &e.kind {
            ElementKind::MapFrame { shape, grid, extent_of, extent_color, extent_width, .. } => {
                let Some(&(_, mi, cam, fw, fh)) = frame(e.id) else {
                    placeholder(target, r, "Missing map", ppp);
                    continue;
                };
                let Some(map) = maps.get_mut(mi) else { continue };
                // A frame larger than the page budget is drawn at a lower resolution and scaled up.
                let fppp = ppp.min((MAX_PIXELS / (fw * fh).max(1.0)).sqrt()).max(0.01);
                let view = View { width: fw, height: fh, pixels_per_point: fppp, camera: cam, sky: false };
                let Some(mut frame_pm) = frame_canvas(map, cam, fw, fh, fppp, target.is_recording(), pending) else {
                    if frame_requires_complete(map, &view) {
                        return None;
                    }
                    // Preserve upstream's best-effort page export for ordinary maps.
                    continue;
                };
                // A frame whose vectors grew past the budget, or would take the page past it, is
                // drawn as a picture instead.
                let too_large = frame_pm.overflowed() || page_cost.saturating_add(frame_pm.recorded_cost()) > crate::vector::max_cost();
                if frame_pm.is_recording()
                    && too_large
                    && let Some(pixels) = frame_canvas(map, cam, fw, fh, fppp, false, pending)
                {
                    frame_pm = pixels;
                    if let Some(list) = record.as_deref_mut() {
                        list.push((e.id, e.name.clone()));
                    }
                }
                let outside = match grid {
                    Some(g) => draw_grid(&mut frame_pm, map, &view, g, layout.decimal_comma),
                    None => Vec::new(),
                };
                if let Some(other) = extent_of.and_then(frame)
                    && let Some(om) = maps.get(other.1)
                {
                    let ring = frame_outline_lonlat(om, &other.2, other.3, other.4);
                    if let Some(map) = maps.get(mi) {
                        outline_lonlat(&mut frame_pm, map, &view, &ring, *extent_color, *extent_width as f32);
                    }
                }
                // Whole pixels, so the clipped image and its outline line up.
                let (x0, y0) = (x.round(), y.round());
                let r = Rect::from_xywh(x0, y0, ew.max(1.0), eh.max(1.0)).unwrap_or(r);
                let path = shape_path(shape, r);
                let up = (ppp / fppp) as f32;
                // Shaped frames are clipped by a mask of the frame's own size.
                if !matches!(shape, FrameShape::Rectangle)
                    && !frame_pm.is_recording()
                    && let Some(p) =
                        path.as_ref().and_then(|p| p.clone().transform(Transform::from_translate(-x0, -y0).post_scale(1.0 / up, 1.0 / up)))
                    && let Some(mut m) = Mask::new(frame_pm.width(), frame_pm.height())
                {
                    m.fill_path(&p, FillRule::Winding, true, Transform::identity());
                    frame_pm.pixels().apply_mask(&m);
                }
                let place = Transform::from_row(up, 0.0, 0.0, up, x0, y0);
                if frame_pm.is_recording() {
                    // Kept as vectors, cut to the frame (its paths reach past it).
                    let clip =
                        path.clone().map(|p| crate::vector::Clip { path: p, rule: FillRule::Winding, transform: Transform::identity() });
                    target.place(frame_pm, place, 1.0, rustgis_map::effects::Blend::Normal, clip);
                } else {
                    target.draw_pixmap(
                        0,
                        0,
                        frame_pm.pixmap().as_ref(),
                        &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() },
                        place,
                        None,
                    );
                }
                if let Some(p) = &path {
                    target.stroke_path(
                        p,
                        &paint(Rgba::rgb(0x30, 0x30, 0x30), 1.0),
                        &Stroke { width: 0.8 * ppp as f32, ..Stroke::default() },
                        Transform::identity(),
                        None,
                    );
                }
                if !outside.is_empty() {
                    outside_labels(target, outside, (x0, y0, ew.max(1.0), eh.max(1.0)), up, ppp as f32);
                }
            }
            ElementKind::NorthArrow { frame: f, style, fill, outline, label, text } => {
                // Turned with its frame's map.
                let bearing = frame(*f).map_or(0.0, |fr| fr.2.bearing);
                north_arrow(target, r, &Arrow { style: *style, fill: *fill, outline: *outline, label, text }, ppp, bearing)
            }
            ElementKind::ScaleBar { frame: f, .. } => {
                if let Some(&(_, mi, cam, _, _)) = frame(*f)
                    && let Some(map) = maps.get(mi)
                    && let Some(look) = ScaleLook::of(&e.kind)
                {
                    scale_bar(target, r, map.ground_res_at(&cam), &look, ppp, layout.decimal_comma);
                }
            }
            ElementKind::Legend { frame: f, title, all_classes, hide_other, exclude, look, columns } => {
                if let Some(&(_, mi, cam, ..)) = frame(*f)
                    && let Some(map) = maps.get(mi)
                {
                    let drawn = (!*all_classes).then(|| shown.iter().find(|(id, _)| id == f).map(|(_, v)| v.as_slice()).unwrap_or(&[]));
                    let scale = map.ground_res_at(&cam) / rustgis_geom::PIXEL_M;
                    wait_for_tile_legends(map, scale, exclude);
                    let opts = LegendOptions { hide_other: *hide_other, exclude, scale, look };
                    legend(target, r, map, (title, *columns), drawn, opts, ppp);
                }
            }
            ElementKind::Colorbar { frame: f, layer, title, orientation, ticks, tick_side, reverse } => {
                let scale = frame(*f)
                    .and_then(|fr| maps.get(fr.1))
                    .and_then(|m| rustgis_map::layout::colorbar_layer(m, layer.as_deref()))
                    .and_then(|l| l.raster.as_ref())
                    .and_then(rustgis_map::layout::color_scale);
                match scale {
                    Some(sc) => colorbar(
                        target,
                        r,
                        &sc,
                        &Bar {
                            title,
                            orientation: *orientation,
                            ticks: *ticks,
                            side: *tick_side,
                            reverse: *reverse,
                            comma: layout.decimal_comma,
                        },
                        ppp,
                    ),
                    None => placeholder(target, r, "Colorbar", ppp),
                }
            }
            ElementKind::Text { text: t, size, color, bold, italic, align, letter_spacing, font, font_name } => {
                let first = layout.first_frame().and_then(frame);
                let s = expand(t, layout, info, first.and_then(|f| maps.get(f.1).map(|m| (m, f.2))));
                // An installed family by name (#762), else its generic one. The app doesn't wait
                // for the font index while it is being built; a script or the command line builds
                // it now (`families::find_ready`).
                let family = font_name.as_deref().and_then(crate::families::find_ready);
                let face = text::Face { bold: *bold, italic: *italic, serif: *font == rustgis_map::layout::FontFamily::Serif, family };
                let style = TextStyle { face, align: *align, spacing: *letter_spacing as f32 };
                text_block(target, r, &s, *size as f32, *color, style, ppp);
            }
            ElementKind::Picture { image } => {
                if let Some(img) = crate::picture::decode(image) {
                    let (iw, ih) = (img.width() as f32, img.height() as f32);
                    let s = (ew / iw).min(eh / ih);
                    let (dw, dh) = (iw * s, ih * s);
                    let t = Transform::from_row(s, 0.0, 0.0, s, x + (ew - dw) / 2.0, y + (eh - dh) / 2.0);
                    target.draw_pixmap(
                        0,
                        0,
                        img.as_ref(),
                        &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() },
                        t,
                        None,
                    );
                } else {
                    placeholder(target, r, "Picture", ppp);
                }
            }
            ElementKind::Table { frame: f, layer, rows } => {
                let ds = frame(*f).and_then(|fr| maps.get(fr.1)).and_then(|m| m.layer_by_name(layer)).map(|l| &l.dataset);
                match ds {
                    Some(ds) => table(target, r, ds, *rows, ppp),
                    None => placeholder(target, r, "Table", ppp),
                }
            }
            ElementKind::Chart { frame: f, layer, field, label } => {
                let ds = frame(*f).and_then(|fr| maps.get(fr.1)).and_then(|m| m.layer_by_name(layer)).map(|l| &l.dataset);
                match ds {
                    Some(ds) => chart(target, r, ds, field, label.as_deref(), ppp),
                    None => placeholder(target, r, "Chart", ppp),
                }
            }
            ElementKind::Swatch { geometry, symbol, line_patch, area_patch } => {
                lone_swatch(target, r, *geometry, symbol, (*line_patch, *area_patch), ppp as f32);
            }
            ElementKind::Rectangle { fill, outline, width, radius } => {
                // Square corners filled as before, so an existing layout draws to the same pixels.
                let rounded = rounded_rect(r, (*radius * PT_PER_MM * ppp) as f32);
                match &rounded {
                    Some(p) => target.fill_path(p, &paint(*fill, 1.0), FillRule::Winding, Transform::identity(), None),
                    None => target.fill_rect(r, &paint(*fill, 1.0), Transform::identity(), None),
                }
                let p = rounded.unwrap_or_else(|| PathBuilder::from_rect(r));
                if *width > 0.0 {
                    target.stroke_path(
                        &p,
                        &paint(*outline, 1.0),
                        &Stroke { width: (*width as f32) * ppp as f32, ..Stroke::default() },
                        Transform::identity(),
                        None,
                    );
                }
            }
            ElementKind::Ellipse { fill, outline, width } => {
                if let Some(p) = PathBuilder::from_oval(r) {
                    target.fill_path(&p, &paint(*fill, 1.0), FillRule::Winding, Transform::identity(), None);
                    if *width > 0.0 {
                        let stroke = Stroke { width: (*width as f32) * ppp as f32, ..Stroke::default() };
                        target.stroke_path(&p, &paint(*outline, 1.0), &stroke, Transform::identity(), None);
                    }
                }
            }
            ElementKind::Line { color, width, dir, head } => {
                let [(x0, y0), (x1, y1)] = dir.ends((r.x().into(), r.y().into(), r.width().into(), r.height().into()));
                let (a, b) = ((x0 as f32, y0 as f32), (x1 as f32, y1 as f32));
                let stroke_w = ((*width as f32) * ppp as f32).max(0.5);
                let (at_start, at_end) = (matches!(head, LineHead::Start | LineHead::Both), matches!(head, LineHead::End | LineHead::Both));
                let len = (b.0 - a.0).hypot(b.1 - a.1);
                // A head grows with the line's width, but never past 40% of the line.
                let heads = u8::from(at_start) + u8::from(at_end);
                let head_len = if heads == 0 || len <= 0.0 {
                    0.0
                } else {
                    ((6.0 + 2.5 * *width as f32) * ppp as f32).min(len * 0.4 / f32::from(heads))
                };
                let (ux, uy) = if len > 0.0 { ((b.0 - a.0) / len, (b.1 - a.1) / len) } else { (1.0, 0.0) };
                // The shaft stops inside each head, so its square end doesn't poke through the tip.
                let inset = head_len * 0.8;
                let (s0, s1) = (
                    if at_start { (a.0 + ux * inset, a.1 + uy * inset) } else { a },
                    if at_end { (b.0 - ux * inset, b.1 - uy * inset) } else { b },
                );
                let mut pb = PathBuilder::new();
                pb.move_to(s0.0, s0.1);
                pb.line_to(s1.0, s1.1);
                if let Some(p) = pb.finish() {
                    let stroke = Stroke { width: stroke_w, ..Stroke::default() };
                    target.stroke_path(&p, &paint(*color, 1.0), &stroke, Transform::identity(), None);
                }
                // Each head: a filled triangle, its tip on the line's end.
                let half = (head_len * 0.45).max(stroke_w);
                for (tip, (dx, dy)) in [(at_start, a, (-ux, -uy)), (at_end, b, (ux, uy))].into_iter().filter(|h| h.0).map(|h| (h.1, h.2)) {
                    let base = (tip.0 - dx * head_len, tip.1 - dy * head_len);
                    let mut pb = PathBuilder::new();
                    pb.move_to(tip.0, tip.1);
                    pb.line_to(base.0 - dy * half, base.1 + dx * half);
                    pb.line_to(base.0 + dy * half, base.1 - dx * half);
                    pb.close();
                    if let Some(p) = pb.finish() {
                        target.fill_path(&p, &paint(*color, 1.0), FillRule::Winding, Transform::identity(), None);
                    }
                }
            }
        }
        if let Some(sheet) = sheet {
            // Anticlockwise on the page, whose y runs down: a negative angle to tiny-skia.
            let (cx, cy) = (px + ew / 2.0, py + eh / 2.0);
            let place = Transform::from_translate(px - pad, py - pad).post_rotate_at(-(e.rotation as f32), cx, cy);
            // Its pixels drawn smoothly, or its recording kept turned.
            pm.place(sheet, place, 1.0, rustgis_map::effects::Blend::Normal, None);
        }
    }
    Some(pm)
}

/// A map frame's map, `w` × `h` points at `ppp` pixels a point, labels and attribution included:
/// what a frame shows before its grid, indicator and outline. It waits for the map's tiles, as a
/// page needs them all.
pub fn frame_map(map: &mut Map, camera: rustgis_map::Camera, w: f64, h: f64, ppp: f64) -> Option<Pixmap> {
    frame_canvas(map, camera, w, h, ppp, false, &mut false).map(Canvas::into_pixmap)
}

/// [`frame_map`] on a canvas: one that records (see [`crate::canvas`]) when `record` and the
/// frame can be kept as vectors (flat, north up, without view effects), else its pixels.
/// `pending` is set when some of its tiles, or a read of its streamed layers or rasters, were
/// still on their way (in the browser, which doesn't wait for them).
fn frame_canvas(map: &mut Map, camera: rustgis_map::Camera, w: f64, h: f64, ppp: f64, record: bool, pending: &mut bool) -> Option<Canvas> {
    let view = View { width: w, height: h, pixels_per_point: ppp, camera, sky: false };
    let img = if record { crate::render_complete_recorded(map, &view) } else { crate::render_complete(map, &view) }?;
    *pending |= img.tiles_pending || img.data_pending;
    Some(finish_frame(img, ppp))
}

/// [`frame_map`] without waiting for tiles still on their way, for a preview drawn while the
/// interface waits: a pan's preview, whose size is more than the frame's for the overscan slid
/// under the pointer.
pub fn frame_map_preview(map: &mut Map, camera: rustgis_map::Camera, w: f64, h: f64, ppp: f64) -> Option<Pixmap> {
    let view = View { width: w, height: h, pixels_per_point: ppp, camera, sky: false };
    crate::render(map, &view).map(|img| finish_frame(img, ppp).into_pixmap())
}

/// A rendered frame map with its labels and attribution drawn on it: on its recording when it
/// was recorded, else on its pixels.
fn finish_frame(img: crate::Rendered, ppp: f64) -> Canvas {
    let mut pm = img.recorded.unwrap_or_else(|| Canvas::from_pixmap(img.pixmap));
    for l in img.unplaced.iter().chain(&img.labels) {
        text::draw(&mut pm, l, ppp as f32);
    }
    crate::draw_attribution(&mut pm, &img.attribution, ppp as f32);
    pm
}

/// Expands layout text tokens: the page ones here, the map ones from the first frame.
pub fn expand(t: &str, layout: &Layout, info: &PageInfo, frame: Option<(&Map, rustgis_map::Camera)>) -> String {
    // The frame's scale, worked out once for both its forms below.
    let scale = frame.map(|(m, cam)| m.ground_res_at(&cam) / rustgis_map::layout::PAPER_M_PER_PT);
    // A layout with a decimal comma prints its scale with points between thousands: put in first,
    // so a page or layout name holding "{scale}" isn't read as the token.
    let t = match scale {
        Some(scale) if layout.decimal_comma => t.replace("{scale}", &decimal(rustgis_map::graphics::scale_ratio(scale), true)),
        _ => t.to_string(),
    };
    let t = t
        .replace("{page}", &info.page.max(1).to_string())
        .replace("{pages}", &info.pages.max(1).to_string())
        .replace("{pageName}", &info.page_name)
        .replace("{layout}", &layout.name);
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let v = match frame {
        Some((m, cam)) => rustgis_map::graphics::TokenValues {
            map: m.name.clone(),
            scale: scale.unwrap_or(0.0),
            crs: m.projection.label(),
            layers: m.layers.iter().filter(|l| !l.graphics).count(),
            center: m.to_lonlat(cam.center).unwrap_or_default(),
            now,
        },
        None => rustgis_map::graphics::TokenValues {
            map: String::new(),
            scale: 0.0,
            crs: String::new(),
            layers: 0,
            center: Coord::default(),
            now,
        },
    };
    rustgis_map::graphics::expand(&t, &v)
}

/// A rectangle with its corners rounded by `rad` pixels (at most half its shorter side), each a
/// quarter circle drawn as a cubic; `None` for square corners, drawn as the plain rectangle.
pub(crate) fn rounded_rect(r: tiny_skia::Rect, rad: f32) -> Option<tiny_skia::Path> {
    let rad = rad.min(r.width() / 2.0).min(r.height() / 2.0);
    if rad.is_nan() || rad <= 0.0 {
        return None;
    }
    // The control points' distance that makes a cubic a quarter circle.
    let c = rad * 0.552_284_8;
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(rt - rad, t);
    pb.cubic_to(rt - rad + c, t, rt, t + rad - c, rt, t + rad);
    pb.line_to(rt, b - rad);
    pb.cubic_to(rt, b - rad + c, rt - rad + c, b, rt - rad, b);
    pb.line_to(l + rad, b);
    pb.cubic_to(l + rad - c, b, l, b - rad + c, l, b - rad);
    pb.line_to(l, t + rad);
    pb.cubic_to(l, t + rad - c, l + rad - c, t, l + rad, t);
    pb.close();
    pb.finish()
}

/// A number as printed with a decimal comma when `comma` (`0.05` to `0,05`, `6,300,000` to
/// `6.300.000`): its points and commas swapped.
fn decimal(s: String, comma: bool) -> String {
    if !comma {
        return s;
    }
    s.chars()
        .map(|c| match c {
            '.' => ',',
            ',' => '.',
            c => c,
        })
        .collect()
}

fn shape_path(shape: &FrameShape, r: Rect) -> Option<tiny_skia::Path> {
    match shape {
        FrameShape::Rectangle => Some(PathBuilder::from_rect(r)),
        FrameShape::Circle => {
            let d = r.width().min(r.height());
            PathBuilder::from_oval(Rect::from_xywh(r.x() + (r.width() - d) / 2.0, r.y() + (r.height() - d) / 2.0, d, d)?)
        }
        FrameShape::Ellipse => PathBuilder::from_oval(r),
        FrameShape::Polygon(_) => {
            let mut pb = PathBuilder::new();
            for (i, (u, v)) in shape.outline().into_iter().enumerate() {
                let (px, py) = (r.x() + u as f32 * r.width(), r.y() + v as f32 * r.height());
                if i == 0 { pb.move_to(px, py) } else { pb.line_to(px, py) }
            }
            pb.close();
            pb.finish()
        }
    }
}

/// The lon/lat outline of what a frame shows.
fn frame_outline_lonlat(map: &Map, cam: &rustgis_map::Camera, w: f64, h: f64) -> Vec<Coord> {
    let mut ring = Vec::new();
    let n = 16;
    let edge = |t: f64, side: usize| match side {
        0 => (t * w, 0.0),
        1 => (w, t * h),
        2 => ((1.0 - t) * w, h),
        _ => (0.0, (1.0 - t) * h),
    };
    for side in 0..4 {
        for i in 0..n {
            let (sx, sy) = edge(i as f64 / n as f64, side);
            if let Some(c) = map.to_lonlat(cam.to_map(sx, sy, w, h)) {
                ring.push(c);
            }
        }
    }
    if let Some(f) = ring.first().copied() {
        ring.push(f);
    }
    ring
}

/// Strokes the ring `ring` (longitude, latitude) on a frame's map `width` points wide: another
/// frame's extent, as an extent indicator shows it.
fn outline_lonlat(pm: &mut Canvas, map: &Map, view: &View, ring: &[Coord], color: Rgba, width: f32) {
    let ppp = view.pixels_per_point as f32;
    let mut pb = PathBuilder::new();
    let mut first = true;
    for c in ring {
        let Some(m) = map.to_map(*c) else { continue };
        // A tilted frame's outline breaks where it passes behind the eye.
        let Some((x, y)) = view.camera.try_to_screen(m, view.width, view.height) else {
            first = true;
            continue;
        };
        let (x, y) = (x as f32 * ppp, y as f32 * ppp);
        if first {
            pb.move_to(x, y);
            first = false;
        } else {
            pb.line_to(x, y);
        }
    }
    if let Some(p) = pb.finish() {
        pm.stroke_path(&p, &paint(color, 1.0), &Stroke { width: width * ppp, ..Stroke::default() }, Transform::identity(), None);
    }
}

/// A graticule label as [`Grid::label`] writes it, with plain `'` and `"` for minutes and seconds
/// when the label font has no prime marks.
fn grid_label(g: &Grid, v: f64, lon: bool, step: f64) -> String {
    let s = g.label(v, lon, step);
    if text::covers(&s, false) { s } else { s.replace('′', "'").replace('″', "\"") }
}

/// One of a grid's lines: its points in longitude/latitude, its label and its label written
/// whole (as the grid writes them, before a decimal comma), whether it runs north–south
/// (labelled along the top and bottom first), whether it is labelled (every
/// [`label_every`](Grid::label_every)th), and whether it is a minor line, drawn as a short
/// tick at the edges only.
#[derive(Clone, Debug)]
struct GridLine {
    pts: Vec<Coord>,
    label: String,
    full: String,
    lon: bool,
    labelled: bool,
    minor: bool,
}

/// The most minor lines a grid draws each way.
const MAX_MINOR_LINES: usize = 2000;

/// Whether the line `v` (on lines `step` apart) is one of every `every` labelled.
fn labelled(v: f64, step: f64, every: u32) -> bool {
    let k = (v / step).round();
    every <= 1 || !k.is_finite() || (k as i64).rem_euclid(i64::from(every)) == 0
}

/// The values of the minor lines between `first` and `last` (lines `step` apart, each step split
/// into `parts`): none when `parts` is under 2.
fn minor_values(first: f64, last: f64, step: f64, parts: u32) -> Vec<f64> {
    let mut out = Vec::new();
    if parts < 2 || step.is_nan() || step <= 0.0 {
        return out;
    }
    let sub = step / f64::from(parts);
    let mut v = first - step;
    while v <= last + step * 1e-9 && out.len() < MAX_MINOR_LINES {
        for j in 1..parts {
            out.push(v + sub * f64::from(j));
        }
        v += step;
    }
    out
}

/// A graticule's lines over `ext` (longitude/latitude), each with its label.
fn graticule_lines(g: &Grid, ext: Extent, lines: &mut Vec<GridLine>) {
    if ext.is_empty() {
        return;
    }
    let span = ext.width().max(ext.height());
    let step = if g.step > 0.0 {
        g.step.max(0.001)
    } else {
        *[0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 45.0].iter().find(|s| span / **s <= 8.0).unwrap_or(&60.0)
    };
    // Lines run a little past the frame so they cross its edges rather than end on them.
    let pad = span * 0.02;
    let (lat0, lat1) = ((ext.min_y - pad).max(-89.9), (ext.max_y + pad).min(89.9));
    let (lon0, lon1) = (ext.min_x - pad, ext.max_x + pad);
    let meridian = |lon: f64| (0..=48).map(|i| Coord::new(lon, lat0 + (lat1 - lat0) * f64::from(i) / 48.0)).collect();
    let parallel = |lat: f64| (0..=48).map(|i| Coord::new(lon0 + (lon1 - lon0) * f64::from(i) / 48.0, lat)).collect();
    let mut n = 0;
    let first_lon = (ext.min_x / step).floor() * step;
    let mut lon = first_lon;
    while lon <= ext.max_x + step * 1e-9 && n < 200 {
        let label = grid_label(g, lon, true, step);
        let labelled = labelled(lon, step, g.label_every);
        lines.push(GridLine { pts: meridian(lon), full: label.clone(), label, lon: true, labelled, minor: false });
        lon += step;
        n += 1;
    }
    let last_lon = lon - step;
    let first_lat = (ext.min_y / step).floor() * step;
    let mut lat = first_lat;
    while lat <= ext.max_y + step * 1e-9 && n < 400 {
        if lat.abs() <= 90.0 {
            let label = grid_label(g, lat, false, step);
            let labelled = labelled(lat, step, g.label_every);
            lines.push(GridLine { pts: parallel(lat), full: label.clone(), label, lon: false, labelled, minor: false });
        }
        lat += step;
        n += 1;
    }
    let last_lat = lat - step;
    let minor = |pts: Vec<Coord>, lon: bool| GridLine { pts, label: String::new(), full: String::new(), lon, labelled: false, minor: true };
    for v in minor_values(first_lon, last_lon, step, g.minor_ticks) {
        lines.push(minor(meridian(v), true));
    }
    for v in minor_values(first_lat, last_lat, step, g.minor_ticks).into_iter().filter(|v| v.abs() <= 90.0) {
        lines.push(minor(parallel(v), false));
    }
}

/// The most lines a measured grid draws each way: past them its step is widened to draw at most
/// [`WIDENED_LINES`].
const MAX_MEASURED_LINES: f64 = 100.0;
/// About how many lines a step too fine for the frame is widened to.
const WIDENED_LINES: f64 = 50.0;

/// A measured grid's lines over the frame whose edges are `lonlats`, in the coordinate system of
/// `tr`: eastings (north–south) and northings, each back in longitude/latitude, with its label.
fn measured_lines(g: &Grid, tr: &rustgis_data::raster::Transformer, lonlats: &[Coord], lines: &mut Vec<GridLine>) {
    let mut ext = Extent::EMPTY;
    for c in lonlats.iter().filter_map(|c| tr.from_lonlat(*c)) {
        ext.expand(c);
    }
    if ext.is_empty() || !(ext.width().is_finite() && ext.height().is_finite()) {
        return;
    }
    let span = ext.width().max(ext.height());
    let given = g.step.clamp(0.0, rustgis_map::layout::MAX_MEASURED_STEP);
    let step = if given > 0.0 {
        given.max(rustgis_map::layout::MIN_MEASURED_STEP)
    } else {
        nice_step(span, rustgis_map::layout::MIN_MEASURED_STEP)
    };
    // A step too fine for the frame (a 1 m grid over a country) is widened, not drawn as a blur.
    let step = if span / step > MAX_MEASURED_LINES { nice_step(span * 8.0 / WIDENED_LINES, step) } else { step };
    let pad = span * 0.02;
    let (x0, x1, y0, y1) = (ext.min_x - pad, ext.max_x + pad, ext.min_y - pad, ext.max_y + pad);
    let back = |x: f64, y: f64| tr.to_lonlat(Coord::new(x, y));
    // (A point that doesn't convert is left out.)
    let easting = |x: f64| -> Vec<Coord> { (0..=48).filter_map(|i| back(x, y0 + (y1 - y0) * f64::from(i) / 48.0)).collect() };
    let northing = |y: f64| -> Vec<Coord> { (0..=48).filter_map(|i| back(x0 + (x1 - x0) * f64::from(i) / 48.0, y)).collect() };
    // Counted too: at coordinates far larger than the step, adding it may not move a line on.
    let most = 2 * MAX_MEASURED_LINES as usize;
    let first = |min: f64| (min / step).floor() * step;
    let values = |min: f64, max: f64| -> Vec<f64> {
        std::iter::successors(Some(first(min)), |v| Some(v + step)).take_while(|v| *v <= max + step * 1e-9).take(most).collect()
    };
    let (xs, ys) = (values(ext.min_x, ext.max_x), values(ext.min_y, ext.max_y));
    let all: Vec<(f64, bool)> = xs.iter().map(|x| (*x, true)).chain(ys.iter().map(|y| (*y, false))).collect();
    for (v, east) in all {
        lines.push(GridLine {
            pts: if east { easting(v) } else { northing(v) },
            label: g.measured_text(v, step, east, false),
            full: g.measured_text(v, step, east, true),
            lon: east,
            labelled: labelled(v, step, g.label_every),
            minor: false,
        });
    }
    let minor = |pts: Vec<Coord>, lon: bool| GridLine { pts, label: String::new(), full: String::new(), lon, labelled: false, minor: true };
    if let (Some(a), Some(z)) = (xs.first(), xs.last()) {
        for v in minor_values(*a, *z, step, g.minor_ticks) {
            lines.push(minor(easting(v), true));
        }
    }
    if let (Some(a), Some(z)) = (ys.first(), ys.last()) {
        for v in minor_values(*a, *z, step, g.minor_ticks) {
            lines.push(minor(northing(v), false));
        }
    }
}

/// Where a polyline (in pixels) crosses the edges of a `w` × `h` frame, as (edge, position along
/// it): the top and bottom edges give x, the left and right ones y.
fn edge_crossings(pts: &[(f32, f32)], w: f32, h: f32) -> Vec<(Edge, f32)> {
    let mut out = Vec::new();
    for pair in pts.windows(2) {
        let &[(x0, y0), (x1, y1)] = pair else { continue };
        for (edge, a, b, at) in [(Edge::Top, y0, y1, 0.0), (Edge::Bottom, y0, y1, h), (Edge::Left, x0, x1, 0.0), (Edge::Right, x0, x1, w)] {
            let (da, db) = (a - at, b - at);
            // Counted once where a vertex sits on the edge, and never along it.
            if !((da <= 0.0 && db > 0.0) || (da > 0.0 && db <= 0.0)) {
                continue;
            }
            let t = da / (da - db);
            let (along, limit) = match edge {
                Edge::Top | Edge::Bottom => (x0 + (x1 - x0) * t, w),
                Edge::Left | Edge::Right => (y0 + (y1 - y0) * t, h),
            };
            if along.is_finite() && (0.0..=limit).contains(&along) {
                out.push((edge, along));
            }
        }
    }
    out
}

/// The coordinate system of a measured grid on `map`: the grid's own, else the map's when it is
/// projected, else the WGS 84 UTM zone at `center` (longitude/latitude). `None` for one that
/// can't be read or isn't projected.
pub fn measured_crs(g: &Grid, map: &Map, center: Coord) -> Option<rustgis_data::raster::Crs> {
    use rustgis_data::raster::Crs;
    let named = g.crs.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let crs = match named {
        // (One longer than a command keeps, from a crafted project, isn't read each frame.)
        Some(c) if c.chars().count() > rustgis_map::layout::MAX_GRID_CRS => return None,
        Some(c) => Crs::parse(c).ok()?,
        // The map's own: its code (a preset, EPSG:, ESRI: or local:) as its PROJ definition.
        None => match &map.projection {
            rustgis_map::Projection::Custom(code) => rustgis_map::projection::lookup(code)
                .ok()
                .and_then(|(def, _)| Crs::parse(&def).ok())
                .or_else(|| Crs::parse(code).ok())
                .filter(|c| !c.is_geographic()),
            _ => None,
        }
        .map_or_else(|| Crs::from_epsg(rustgis_map::layout::utm_epsg(center.x, center.y)), Some)?,
    };
    (!crs.is_geographic()).then_some(crs)
}

/// A step that draws about eight lines across `span`: 1, 2 or 5 times a power of ten.
fn nice_step(span: f64, smallest: f64) -> f64 {
    let rough = (span / 8.0).max(smallest);
    let p = 10f64.powf(rough.log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|k| k * p).find(|s| *s >= rough).unwrap_or(10.0 * p)
}

/// The dark grey grid labels are in until a colour is set.
const GRID_LABEL: Rgba = Rgba::rgb(0x20, 0x20, 0x20);
/// How large an emphasised label's small figures are, of its large ones.
const SMALL_FIGURES: f32 = 0.7;

/// How a grid's labels are drawn: their type, size (pixels), colour and halo.
#[derive(Clone, Debug, PartialEq)]
struct GridInk {
    t: LabelText,
    px: f32,
    color: Rgba,
    halo: Option<Rgba>,
}

impl GridInk {
    /// `g`'s labels at `ppp` pixels a point, inside the frame (`inside`) or past it, upright or
    /// turned (`turned`).
    fn of(g: &Grid, ppp: f32, inside: bool, turned: bool) -> GridInk {
        GridInk {
            t: g.text.clone(),
            px: g.label_size() as f32 * 1.33 * ppp,
            color: g.text.color.unwrap_or(GRID_LABEL),
            halo: g.halo_color(inside, turned),
        }
    }

    /// Drawn as grid labels always were: the sans, one piece, a white halo or none.
    fn plain(&self, runs: &[(String, f32)]) -> bool {
        plain_type(&self.t) && runs.len() == 1 && self.halo.is_none_or(|h| h == rustgis_map::layout::DEFAULT_GRID_HALO)
    }

    /// How wide `s` is at `k` times the size.
    fn width(&self, s: &str, k: f32) -> f32 {
        let px = self.px * k;
        if plain_type(&self.t) { text::measure(s, px / 1.33, self.t.bold) } else { label_width(s, px, label_face(&self.t)) }
    }

    /// How wide the pieces `runs` are, side by side.
    fn runs_width(&self, runs: &[(String, f32)]) -> f32 {
        runs.iter().map(|(s, k)| self.width(s, *k)).sum()
    }

    /// `s` at `k` times the size in `color`, starting at `left`, its middle at `cy`.
    fn ink(&self, pm: &mut Canvas, s: &str, k: f32, left: f32, cy: f32, color: Rgba) {
        let px = self.px * k;
        if plain_type(&self.t) {
            text::text_at(pm, s, left, cy, px, color, self.t.bold, text::Align::Left, false);
        } else {
            label_at(pm, s, left, cy, px, color, label_face(&self.t), false);
        }
    }

    /// The pieces `runs` side by side, centred on (`cx`, `cy`): the small ones raised to the
    /// large ones' tops, every halo under every piece.
    fn draw(&self, pm: &mut Canvas, runs: &[(String, f32)], cx: f32, cy: f32) {
        if self.plain(runs) {
            if let Some((s, _)) = runs.first() {
                text::text_at(pm, s, cx, cy, self.px, self.color, self.t.bold, text::Align::Center, self.halo.is_some());
            }
            return;
        }
        let raised = |k: f32| cy - (self.px - self.px * k) * 0.45;
        let left = cx - self.runs_width(runs) / 2.0;
        if let Some(h) = self.halo {
            let r = (self.px * 0.09).max(1.0);
            let mut x = left;
            for (s, k) in runs {
                for (dx, dy) in [(-r, 0.0), (r, 0.0), (0.0, -r), (0.0, r)] {
                    self.ink(pm, s, *k, x + dx, raised(*k) + dy, h);
                }
                x += self.width(s, *k);
            }
        }
        let mut x = left;
        for (s, k) in runs {
            self.ink(pm, s, *k, x, raised(*k), self.color);
            x += self.width(s, *k);
        }
    }

    /// The pieces `runs` centred on (`cx`, `cy`), turned to read from bottom to top.
    /// Kept as glyphs turned when `pm` records, else drawn upright and turned as pixels.
    fn draw_turned(&self, pm: &mut Canvas, runs: &[(String, f32)], cx: f32, cy: f32) {
        // Whole pixels throughout, so the quarter turn moves pixels without resampling them.
        let tw = (self.runs_width(runs) + self.px * 0.4).ceil().max(1.0);
        let th = (self.px * 1.6).ceil().max(1.0);
        let (x, y) = ((cx - th / 2.0).round(), (cy + tw / 2.0).round());
        let t = Transform::from_row(0.0, -1.0, 1.0, 0.0, x, y);
        if pm.is_recording() {
            pm.with_transform(t, |c| self.draw(c, runs, tw / 2.0, th / 2.0));
            return;
        }
        let Some(mut tmp) = Canvas::new(tw as u32, th as u32) else { return };
        self.draw(&mut tmp, runs, tw / 2.0, th / 2.0);
        pm.draw_pixmap(
            0,
            0,
            tmp.pixmap().as_ref(),
            &PixmapPaint { quality: tiny_skia::FilterQuality::Nearest, ..PixmapPaint::default() },
            t,
            None,
        );
    }

    /// The pieces `runs` across an `edge`'s line (turned when `turned`): how far they reach
    /// from it, and how long they are along it.
    fn extent(&self, edge: Edge, runs: &[(String, f32)], turned: bool) -> (f32, f32) {
        let w = self.runs_width(runs);
        let side = matches!(edge, Edge::Left | Edge::Right);
        // Along a side upright, or along the top or bottom turned: a line high along the edge.
        if side != turned { (w, self.px) } else { (self.px, w) }
    }

    /// The pieces `runs` beside `edge`, `gap` pixels from its line at `line` (inside the frame
    /// when `inward`), centred on `at` along it, turned when `turned`.
    fn draw_at(&self, pm: &mut Canvas, runs: &[(String, f32)], edge: Edge, (line, at): (f32, f32), gap: f32, inward: bool, turned: bool) {
        let (depth, _) = self.extent(edge, runs, turned);
        // A line high out from the edge: its middle 0.4 of a line in (0.45 outside), as the
        // labels always sat; a label's length out: its middle half of it in.
        let side = matches!(edge, Edge::Left | Edge::Right);
        let half = if side != turned {
            depth / 2.0
        } else if inward {
            self.px * 0.4
        } else {
            self.px * 0.45
        };
        let out = gap + half;
        // Away from the frame's middle: outward from the top and left edges is up and left.
        let sign = match (edge, inward) {
            (Edge::Top | Edge::Left, true) | (Edge::Bottom | Edge::Right, false) => 1.0,
            _ => -1.0,
        };
        let (cx, cy) = if side { (line + sign * out, at) } else { (at, line + sign * out) };
        if turned { self.draw_turned(pm, runs, cx, cy) } else { self.draw(pm, runs, cx, cy) }
    }
}

/// A label's pieces as drawn, each with its size (of the label's): the decimal comma applied, and
/// an emphasised measured value split into its small leading figures, its large kilometres and
/// its small rest.
fn label_runs(g: &Grid, s: &str, comma: bool) -> Vec<(String, f32)> {
    let pieces = if g.kind == GridKind::Measured && g.emphasis { emphasis(s) } else { vec![(s.to_string(), 1.0)] };
    pieces.into_iter().filter(|(t, _)| !t.is_empty()).map(|(t, k)| (decimal(t, comma), k)).collect()
}

/// `s` (a measured value: a sign, figures perhaps with thousands apart, decimals and a unit) as
/// topographic maps print it: the two figures before the last three of its whole part large,
/// the rest small. A value of fewer than four whole figures is large throughout.
fn emphasis(s: &str) -> Vec<(String, f32)> {
    let chars: Vec<char> = s.chars().collect();
    let start = usize::from(chars.first() == Some(&'-'));
    let end = chars.iter().skip(start).position(|c| !(c.is_ascii_digit() || *c == ',')).map_or(chars.len(), |i| i + start);
    let figures: Vec<usize> = (start..end).filter(|&i| chars.get(i).is_some_and(char::is_ascii_digit)).collect();
    let n = figures.len();
    if n < 4 {
        return vec![(s.to_string(), 1.0)];
    }
    // The two figures before the last three (the one, of four).
    let (Some(&from), Some(&to)) = (figures.get(n.saturating_sub(5)), figures.get(n - 4)) else {
        return vec![(s.to_string(), 1.0)];
    };
    let part = |a: usize, z: usize| chars.get(a..z).map(|c| c.iter().collect::<String>()).unwrap_or_default();
    vec![(part(0, from), SMALL_FIGURES), (part(from, to + 1), 1.0), (part(to + 1, chars.len()), SMALL_FIGURES)]
}

/// A label kept, to draw past the frame's edges: its edge, place along it (pixels of the frame's
/// picture), pieces, look, whether it is turned, and how far it is moved off the edge (mm).
#[derive(Clone, Debug)]
struct OutsideLabel {
    edge: Edge,
    at: f32,
    runs: Vec<(String, f32)>,
    /// Its look at one pixel a point.
    ink: GridInk,
    turned: bool,
    offset: f64,
}

/// A grid over a frame: lines across it, ticks along its edges or both, labelled at the edges the
/// grid names. A graticule's lines are latitudes and longitudes; a measured grid's are eastings and
/// northings in its coordinate system, bent through it onto the map.
/// Draws `g` (and its second) over a frame's picture `pm`; their labels too, unless they go
/// outside the frame (`Grid::outside`): those are returned for the page to draw past the
/// frame's edges ([`outside_labels`]).
fn draw_grid(pm: &mut Canvas, map: &Map, view: &View, g: &Grid, comma: bool) -> Vec<OutsideLabel> {
    let mut out = draw_one_grid(pm, map, view, g, comma);
    if let Some(second) = &g.second {
        out.extend(draw_one_grid(pm, map, view, second, comma));
    }
    out
}

fn draw_one_grid(pm: &mut Canvas, map: &Map, view: &View, g: &Grid, comma: bool) -> Vec<OutsideLabel> {
    let ppp = view.pixels_per_point as f32;
    let (w, h) = (view.width, view.height);
    // The frame's edges in longitude/latitude: its corners and points along each side.
    let mut edge_points = Vec::new();
    for i in 0..=8 {
        let t = f64::from(i) / 8.0;
        edge_points.extend([(w * t, 0.0), (w * t, h), (0.0, h * t), (w, h * t)]);
    }
    let lonlats: Vec<Coord> = edge_points.iter().filter_map(|&(sx, sy)| map.to_lonlat(view.camera.to_map(sx, sy, w, h))).collect();
    let mut ext = Extent::EMPTY;
    for c in &lonlats {
        ext.expand(*c);
    }
    if ext.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<GridLine> = Vec::new();
    match g.kind {
        GridKind::Graticule => {
            // Over the extent of the frame's corners and midpoints, as it always was (its step and
            // lines unchanged for the layouts made before measured grids).
            let mut corners = Extent::EMPTY;
            for (sx, sy) in [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h), (w / 2.0, 0.0), (w / 2.0, h), (0.0, h / 2.0), (w, h / 2.0)] {
                if let Some(c) = map.to_lonlat(view.camera.to_map(sx, sy, w, h)) {
                    corners.expand(c);
                }
            }
            graticule_lines(g, corners, &mut lines);
        }
        GridKind::Measured => {
            let center = map.to_lonlat(view.camera.to_map(w / 2.0, h / 2.0, w, h)).unwrap_or_else(|| ext.center());
            let Some(tr) = measured_crs(g, map, center).and_then(|c| c.transformer().ok()) else { return Vec::new() };
            measured_lines(g, &tr, &lonlats, &mut lines);
        }
    }
    // A tilted frame's lines break where they pass behind the eye.
    let to_px = |c: Coord| {
        map.to_map(c)
            .and_then(|m| view.camera.try_to_screen(m, w, h))
            .map(|(x, y)| (x as f32 * ppp, y as f32 * ppp))
            .filter(|(x, y)| x.is_finite() && y.is_finite())
    };
    let (pw, ph) = (pm.width() as f32, pm.height() as f32);
    let width = g.width.max(0.0) as f32 * ppp;
    let tick = if g.style.ticks() { (g.tick_length * PT_PER_MM) as f32 * ppp } else { 0.0 };
    // Minor ticks half a tick long (half the length set when the grid draws lines only).
    let minor_tick = (g.tick_length * PT_PER_MM) as f32 * ppp * 0.5;
    let mut lines_pb = PathBuilder::new();
    let mut ticks: Vec<((f32, f32), (f32, f32))> = Vec::new();
    // Each label's edge, place along it, text (and whole), and whether it is a meridian's.
    let mut labels: Vec<(Edge, f32, String, bool)> = Vec::new();
    let mut whole: Vec<(Edge, f32, String)> = Vec::new();
    for line in lines {
        let px: Vec<Option<(f32, f32)>> = line.pts.into_iter().map(to_px).collect();
        // Runs of points that projected, so a line breaks where its projection does.
        for run in px.split(|p| p.is_none()) {
            let run: Vec<(f32, f32)> = run.iter().flatten().copied().collect();
            if g.style.lines() && !line.minor {
                for (i, (x, y)) in run.iter().enumerate() {
                    if i == 0 { lines_pb.move_to(*x, *y) } else { lines_pb.line_to(*x, *y) }
                }
            }
            for (edge, at) in edge_crossings(&run, pw, ph) {
                let len = if line.minor { minor_tick } else { tick };
                if len > 0.0 {
                    let (a, b) = match edge {
                        Edge::Top => ((at, 0.0), (at, len)),
                        Edge::Bottom => ((at, ph), (at, ph - len)),
                        Edge::Left => ((0.0, at), (len, at)),
                        Edge::Right => ((pw, at), (pw - len, at)),
                    };
                    ticks.push((a, b));
                }
                let shown = match edge {
                    Edge::Top => g.edges.top,
                    Edge::Bottom => g.edges.bottom,
                    Edge::Left => g.edges.left,
                    Edge::Right => g.edges.right,
                };
                if g.labels && shown && line.labelled && !line.minor {
                    labels.push((edge, at, line.label.clone(), line.lon));
                    whole.push((edge, at, line.full.clone()));
                }
            }
        }
    }
    let paint_line = paint(g.color, 1.0);
    // Solid lines and their ticks are one path, so a see-through colour isn't darker where they
    // meet; dashed or dotted lines leave the ticks solid.
    let solid = g.dash == rustgis_map::layout::GridDash::Solid;
    let mut ticks_pb = PathBuilder::new();
    for (a, b) in ticks {
        let pb = if solid { &mut lines_pb } else { &mut ticks_pb };
        pb.move_to(a.0, a.1);
        pb.line_to(b.0, b.1);
    }
    if let Some(p) = lines_pb.finish() {
        let dash = match g.dash {
            rustgis_map::layout::GridDash::Solid => None,
            rustgis_map::layout::GridDash::Dashed => tiny_skia::StrokeDash::new(vec![width.max(ppp) * 6.0, width.max(ppp) * 4.0], 0.0),
            rustgis_map::layout::GridDash::Dotted => tiny_skia::StrokeDash::new(vec![width.max(ppp) * 0.01, width.max(ppp) * 2.5], 0.0),
        };
        let cap = if g.dash == rustgis_map::layout::GridDash::Dotted { LineCap::Round } else { LineCap::Butt };
        pm.stroke_path(&p, &paint_line, &Stroke { width, dash, line_cap: cap, ..Stroke::default() }, Transform::identity(), None);
    }
    if let Some(p) = ticks_pb.finish() {
        pm.stroke_path(&p, &paint_line, &Stroke { width, ..Stroke::default() }, Transform::identity(), None);
    }
    let inside = !g.outside;
    let runs = |s: &str| label_runs(g, s, comma);
    let ink = |edge: Edge| GridInk::of(g, ppp, inside, g.vertical(edge));
    let along = |edge: Edge, s: &str| ink(edge).extent(edge, &runs(s), g.vertical(edge)).1;
    let label_px = g.label_size() as f32 * 1.33 * ppp;
    let mut kept = kept_labels(labels.clone(), (pw, ph), along, label_px, ppp);
    // The labels nearest each corner written whole, where they still fit.
    if g.kind == GridKind::Measured && g.full_corners && !kept.is_empty() {
        let mut ends: Vec<(Edge, f32)> = Vec::new();
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            let on: Vec<f32> = kept.iter().filter(|(e, ..)| *e == edge).map(|(_, at, _)| *at).collect();
            if let (Some(a), Some(z)) = (on.iter().copied().reduce(f32::min), on.iter().copied().reduce(f32::max)) {
                ends.push((edge, a));
                ends.push((edge, z));
            }
        }
        let is_end = |edge: Edge, at: f32| ends.iter().any(|(e, a)| *e == edge && *a == at);
        let relabelled: Vec<(Edge, f32, String, bool)> = labels
            .iter()
            .zip(&whole)
            .filter(|((e, at, ..), _)| kept.iter().any(|(ke, ka, _)| ke == e && ka == at))
            .map(|((e, at, s, lon), (_, _, full))| (*e, *at, if is_end(*e, *at) { full.clone() } else { s.clone() }, *lon))
            .collect();
        kept = kept_labels(relabelled, (pw, ph), along, label_px, ppp);
    }
    if g.outside {
        // Their look at one pixel a point: the page draws them at its own.
        return kept
            .into_iter()
            .map(|(edge, at, s)| OutsideLabel {
                edge,
                at,
                runs: runs(&s),
                ink: GridInk::of(g, 1.0, false, g.vertical(edge)),
                turned: g.vertical(edge),
                offset: g.label_offset,
            })
            .collect();
    }
    // Labels sit just inside the frame, past the ticks.
    let inset = tick + 3.0 * ppp + (g.label_offset * PT_PER_MM) as f32 * ppp;
    for (edge, at, s) in kept {
        let line = match edge {
            Edge::Top | Edge::Left => 0.0,
            Edge::Bottom => ph,
            Edge::Right => pw,
        };
        ink(edge).draw_at(pm, &runs(&s), edge, (line, at), inset, true, g.vertical(edge));
    }
    Vec::new()
}

/// How far (mm) a grid's outside labels reach past its frame's edges, at least (see
/// [`Grid::outside_reach`]).
pub const OUTSIDE_LABEL_MM: f64 = rustgis_map::layout::MIN_OUTSIDE_REACH;

/// A grid's labels past its frame's edges (`Grid::outside`), on the page `pm`: `labels` from
/// [`draw_grid`] in pixels of the frame's picture, which is `up` page pixels each and placed at
/// (`x0`, `y0`), `w` × `h` page pixels.
fn outside_labels(pm: &mut Canvas, labels: Vec<OutsideLabel>, (x0, y0, w, h): (f32, f32, f32, f32), up: f32, ppp: f32) {
    for l in labels {
        let ink = GridInk { px: l.ink.px * ppp, ..l.ink };
        let gap = (1.5 + l.offset) as f32 * PT_PER_MM as f32 * ppp;
        let (line, at) = match l.edge {
            Edge::Top => (y0, x0 + l.at * up),
            Edge::Bottom => (y0 + h, x0 + l.at * up),
            Edge::Left => (x0, y0 + l.at * up),
            Edge::Right => (x0 + w, y0 + l.at * up),
        };
        ink.draw_at(pm, &l.runs, l.edge, (line, at), gap, false, l.turned);
    }
}

/// The graticule labels drawn, of those at each edge crossing (`labels`: edge, place along it,
/// text, a meridian's or not) on a frame `size` pixels, `label_h` tall: away from the corners (all
/// of a label, `along` its edge, fits, and none at a side's ends by a top or bottom label), and
/// none over another. Meridians are labelled along the top and
/// bottom and parallels along the sides first; a line that leaves the frame through the other
/// edges (a slanting meridian on a projected map) is labelled there only well clear of the labels
/// already set on that edge.
fn kept_labels(
    mut labels: Vec<(Edge, f32, String, bool)>,
    (pw, ph): (f32, f32),
    along: impl Fn(Edge, &str) -> f32,
    label_h: f32,
    ppp: f32,
) -> Vec<(Edge, f32, String)> {
    let own_edge = |edge: Edge, lon: bool| matches!(edge, Edge::Top | Edge::Bottom) == lon;
    labels.sort_by_key(|(edge, _, _, lon)| !own_edge(*edge, *lon));
    let mut taken: Vec<(Edge, f32, f32)> = Vec::new();
    let mut kept = Vec::new();
    for (edge, at, s, lon) in labels {
        let length = along(edge, &s);
        let side = matches!(edge, Edge::Left | Edge::Right);
        let limit = if side { ph } else { pw };
        // A side's labels keep two label heights from the corners, below the top's labels and
        // above the bottom's, so a corner holds no two.
        let margin = (length / 2.0 + 3.0 * ppp).max(12.0 * ppp).max(if side { 2.0 * label_h } else { 0.0 });
        if at < margin || at > limit - margin {
            continue;
        }
        let (a, z) = (at - length / 2.0 - 2.0 * ppp, at + length / 2.0 + 2.0 * ppp);
        // Off its own edge, a label keeps a further length of its own clear of the others, so
        // it doesn't read as one of theirs.
        let clear = if own_edge(edge, lon) { 0.0 } else { length };
        if taken.iter().any(|(e, ta, tz)| *e == edge && a - clear < *tz && *ta < z + clear) {
            continue;
        }
        taken.push((edge, a, z));
        kept.push((edge, at, s));
    }
    kept
}

/// A grey box with `label` in it, where a frame's map can't be drawn.
fn placeholder(pm: &mut Canvas, r: Rect, label: &str, ppp: f64) {
    pm.fill_rect(r, &paint(Rgba::rgb(0xF0, 0xF1, 0xF3), 1.0), Transform::identity(), None);
    let p = PathBuilder::from_rect(r);
    pm.stroke_path(
        &p,
        &paint(Rgba::rgb(0xA0, 0xA4, 0xAA), 1.0),
        &Stroke { width: ppp as f32, ..Stroke::default() },
        Transform::identity(),
        None,
    );
    text::text_at(
        pm,
        label,
        r.x() + r.width() / 2.0,
        r.y() + r.height() / 2.0,
        10.0 * 1.33 * ppp as f32,
        Rgba::rgb(0x70, 0x74, 0x7A),
        false,
        text::Align::Center,
        false,
    );
}

/// Fills `pts` with `fill` and outlines it with `outline`.
fn shape(pm: &mut Canvas, pts: &[(f32, f32)], fill: Rgba, outline: Rgba, width: f32) {
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        if i == 0 { pb.move_to(*x, *y) } else { pb.line_to(*x, *y) }
    }
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(fill, 1.0), FillRule::Winding, Transform::identity(), None);
        pm.stroke_path(&p, &paint(outline, 1.0), &Stroke { width, ..Stroke::default() }, Transform::identity(), None);
    }
}

/// A north arrow's look, as its element has it.
pub struct Arrow<'a> {
    pub style: ArrowStyle,
    pub fill: Rgba,
    pub outline: Rgba,
    /// The letter over it (`N`); empty for none.
    pub label: &'a str,
    pub text: &'a LabelText,
}

/// The face a north arrow's letter or a scale bar's numbers are drawn in.
fn label_face(t: &LabelText) -> text::Face {
    let family = t.font_name.as_deref().and_then(crate::families::find_ready);
    text::Face { bold: t.bold, italic: t.italic, serif: t.font == FontFamily::Serif, family }
}

/// One line of `s` in `face` at `px`, its middle at `cy`: centred on `x`, or starting at it
/// (`centred` false). Returns how wide its ink is.
#[allow(clippy::too_many_arguments)]
fn label_at(pm: &mut Canvas, s: &str, x: f32, cy: f32, px: f32, color: Rgba, face: text::Face, centred: bool) -> f32 {
    let (lo, hi) = text::styled_extent(s, px, face, 0.0);
    let left = if centred { x - (lo + hi) / 2.0 } else { x - lo };
    text::styled_at(pm, s, left, cy, px, color, face, 0.0);
    hi - lo
}

/// How wide `s`'s ink is in `face` at `px`.
fn label_width(s: &str, px: f32, face: text::Face) -> f32 {
    let (lo, hi) = text::styled_extent(s, px, face, 0.0);
    hi - lo
}

/// Strokes the polyline `pts` (closed when `close`) in `color`.
fn polyline(pm: &mut Canvas, pts: &[(f32, f32)], color: Rgba, width: f32, close: bool) {
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        if i == 0 { pb.move_to(*x, *y) } else { pb.line_to(*x, *y) }
    }
    if close {
        pb.close();
    }
    if let Some(p) = pb.finish() {
        let stroke = Stroke { width, line_cap: tiny_skia::LineCap::Round, line_join: tiny_skia::LineJoin::Round, ..Stroke::default() };
        pm.stroke_path(&p, &paint(color, 1.0), &stroke, Transform::identity(), None);
    }
}

/// A ring of radius `rad` about (`x`, `y`).
fn ring(pm: &mut Canvas, (x, y): (f32, f32), rad: f32, color: Rgba, width: f32) {
    if let Some(p) = PathBuilder::from_circle(x, y, rad.max(0.5)) {
        pm.stroke_path(&p, &paint(color, 1.0), &Stroke { width, ..Stroke::default() }, Transform::identity(), None);
    }
}

/// A north arrow in its style, with its letter (`N`) above it, pointing north on a map turned to
/// `bearing` (degrees clockwise from north): turned about the middle of `r`, the letter kept
/// upright.
pub fn north_arrow(pm: &mut Canvas, r: Rect, a: &Arrow, ppp: f64, bearing: f64) {
    let (style, fill, outline) = (a.style, a.fill, a.outline);
    let white = Rgba::rgb(0xFF, 0xFF, 0xFF);
    let sw = 0.8 * ppp as f32;
    let (cx, top) = (r.x() + r.width() / 2.0, r.y());
    let label = a.label.trim();
    // The letter's height: its size when set, else a quarter of the element (14 points at most).
    let px = if a.text.size > 0.0 { (a.text.size * 1.33 * ppp) as f32 } else { (r.height() * 0.25).min(14.0 * ppp as f32) * 1.1 };
    let label_h = if label.is_empty() { 0.0 } else { (px / 1.1).min(r.height() * 0.6) };
    let (tip, base) = (top + label_h + if label.is_empty() { 1.0 } else { 2.0 }, r.y() + r.height() - 1.0);
    // North lies `bearing` anticlockwise of the page's up.
    let (sin, cos) = if bearing.is_finite() { (-bearing.to_radians()).sin_cos() } else { (0.0, 1.0) };
    let (sin, cos) = (sin as f32, cos as f32);
    let (mx, my) = (cx, r.y() + r.height() / 2.0);
    let turn = |(x, y): (f32, f32)| {
        let (dx, dy) = (x - mx, y - my);
        (mx + dx * cos - dy * sin, my + dx * sin + dy * cos)
    };
    let shape = |pm: &mut Canvas, pts: &[(f32, f32)], fill: Rgba, outline: Rgba, sw: f32| {
        let pts: Vec<(f32, f32)> = pts.iter().map(|p| turn(*p)).collect();
        shape(pm, &pts, fill, outline, sw)
    };
    let lines = |pm: &mut Canvas, pts: &[(f32, f32)], w: f32| {
        let pts: Vec<(f32, f32)> = pts.iter().map(|p| turn(*p)).collect();
        polyline(pm, &pts, fill, w, false)
    };
    let (h, cy) = (base - tip, (tip + base) / 2.0);
    let rad = (r.width() / 2.0 - sw).min(h / 2.0).max(1.0);
    // A split arrowhead from `tip` to `base`, `s` either side.
    let split = |pm: &mut Canvas, tip: f32, base: f32, s: f32| {
        let mid = tip + (base - tip) * 0.68;
        shape(pm, &[(cx, tip), (cx - s, base), (cx, mid)], fill, fill, sw);
        shape(pm, &[(cx, tip), (cx + s, base), (cx, mid)], white, outline, sw);
    };
    // An arm from the centre, split down its length: one half filled, one light.
    let arm = |pm: &mut Canvas, angle: f32, len: f32, half: f32| {
        let (dx, dy) = (angle.sin(), -angle.cos());
        let end = (cx + dx * len, cy + dy * len);
        let (lx, ly) = (cx + dy * half, cy - dx * half);
        let (rx, ry) = (cx - dy * half, cy + dx * half);
        shape(pm, &[(cx, cy), end, (lx, ly)], fill, outline, sw);
        shape(pm, &[(cx, cy), end, (rx, ry)], white, outline, sw);
    };
    let quarter = std::f32::consts::FRAC_PI_2;
    match style {
        ArrowStyle::Classic => split(pm, tip, base, (r.width() * 0.5).min(r.height() * 0.32)),
        ArrowStyle::Triangle => {
            let s = (r.width() * 0.4).min(r.height() * 0.25);
            shape(pm, &[(cx, tip), (cx + s, base), (cx - s, base)], fill, outline, sw);
        }
        ArrowStyle::Compass | ArrowStyle::Star | ArrowStyle::Rose => {
            if style == ArrowStyle::Rose {
                ring(pm, turn((cx, cy)), rad * 0.72, outline, sw * 1.4);
            }
            if style != ArrowStyle::Compass {
                for k in 0..4 {
                    arm(pm, std::f32::consts::FRAC_PI_4 + k as f32 * quarter, rad * 0.6, rad * 0.14);
                }
            }
            for k in 0..4 {
                arm(pm, k as f32 * quarter, rad, rad * 0.2);
            }
        }
        ArrowStyle::Needle => {
            let s = (r.width() * 0.22).min(h * 0.14);
            shape(pm, &[(cx, tip), (cx + s, cy), (cx - s, cy)], fill, outline, sw);
            shape(pm, &[(cx, base), (cx - s, cy), (cx + s, cy)], white, outline, sw);
        }
        ArrowStyle::Circle => {
            ring(pm, turn((cx, cy)), rad, outline, sw * 1.5);
            split(pm, cy - rad * 0.82, cy + rad * 0.62, rad * 0.45);
        }
        ArrowStyle::Simple => {
            let s = (r.width() * 0.35).min(h * 0.22);
            let w = (r.width() * 0.06).max(1.5 * ppp as f32);
            lines(pm, &[(cx, base), (cx, tip)], w);
            lines(pm, &[(cx - s, tip + s * 1.2), (cx, tip), (cx + s, tip + s * 1.2)], w);
        }
        ArrowStyle::Chevron => {
            let s = (r.width() * 0.45).min(h * 0.4);
            shape(pm, &[(cx, tip), (cx + s, base), (cx, base - h * 0.3), (cx - s, base)], fill, outline, sw);
        }
        ArrowStyle::Block => {
            let (s, hh) = ((r.width() * 0.45).min(h * 0.35), h * 0.45);
            let sh = s * 0.42;
            let pts = [
                (cx, tip),
                (cx + s, tip + hh),
                (cx + sh, tip + hh),
                (cx + sh, base),
                (cx - sh, base),
                (cx - sh, tip + hh),
                (cx - s, tip + hh),
            ];
            shape(pm, &pts, fill, outline, sw);
        }
    }
    if !label.is_empty() {
        let (nx, ny) = turn((cx, top + label_h / 2.0));
        label_at(pm, label, nx, ny, px, a.text.color.unwrap_or(outline), label_face(a.text), true);
    }
}

/// A picture of a north arrow style for a gallery: `w` × `h` pixels, dark on clear.
pub fn north_arrow_swatch(style: ArrowStyle, w: u32, h: u32) -> Option<Pixmap> {
    let mut pm = Canvas::new(w, h)?;
    let r = Rect::from_xywh(2.0, 2.0, w as f32 - 4.0, h as f32 - 4.0)?;
    let ink = Rgba::rgb(0x22, 0x22, 0x22);
    let text = LabelText::arrow();
    north_arrow(&mut pm, r, &Arrow { style, fill: ink, outline: ink, label: "N", text: &text }, (h as f64 / 40.0).max(0.5), 0.0);
    Some(pm.into_pixmap())
}

/// A scale bar label for `v`, a multiple of `step`: with the decimals the step needs (up to 6), so
/// 0.0125 km stays 0.0125 rather than rounding to 0.01. A step with no exact decimal (a third)
/// keeps two significant digits of it.
fn scale_number(v: f64, step: f64) -> String {
    let places = (0..6usize)
        .find(|&n| {
            let k = step * 10f64.powi(n as i32);
            (k - k.round()).abs() < 1e-9 * k.abs().max(1.0)
        })
        .unwrap_or_else(|| {
            let mag = if step.is_finite() && step > 0.0 { step.log10().floor() } else { 0.0 };
            (1.0 - mag).clamp(0.0, 6.0) as usize
        });
    let s = format!("{v:.prec$}", prec = places);
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// A scale bar's look, as its element has it.
pub struct ScaleLook<'a> {
    pub units: ScaleUnits,
    pub divisions: usize,
    pub style: ScaleBarStyle,
    pub subdivisions: usize,
    pub division: f64,
    pub unit_label: UnitLabel,
    pub unit_text: &'a str,
    pub unit_place: UnitPlace,
    pub numbers: NumberPlace,
    pub color: Rgba,
    /// The light parts.
    pub fill: Rgba,
    pub bar_height: f64,
    pub text: &'a LabelText,
}

impl<'a> ScaleLook<'a> {
    /// A scale bar element's look; `None` for other elements.
    pub fn of(kind: &'a ElementKind) -> Option<ScaleLook<'a>> {
        match kind {
            ElementKind::ScaleBar {
                units,
                divisions,
                style,
                subdivisions,
                division,
                unit_label,
                unit_text,
                unit_place,
                numbers,
                color,
                fill,
                bar_height,
                text,
                ..
            } => Some(ScaleLook {
                units: *units,
                divisions: *divisions,
                style: *style,
                subdivisions: *subdivisions,
                division: *division,
                unit_label: *unit_label,
                unit_text,
                unit_place: *unit_place,
                numbers: *numbers,
                color: *color,
                fill: *fill,
                bar_height: *bar_height,
                text,
            }),
            _ => None,
        }
    }
}

/// The length a scale bar counts to, in its unit, with the unit's metres and abbreviation:
/// `division` × `divisions` when the division is set (in the units, or in metres when they are
/// automatic), else a round length that fits `room_m` ground metres.
pub fn scale_length(units: ScaleUnits, divisions: usize, division: f64, room_m: f64) -> (f64, f64, &'static str) {
    if division.is_finite() && division > 0.0 {
        let fixed = units.resolve(1.0).0;
        let total_m = division * divisions.max(1) as f64 * if units == ScaleUnits::Auto { 1.0 } else { fixed };
        let (unit_m, abbr) = units.resolve(total_m);
        return (total_m / unit_m, unit_m, abbr);
    }
    let (unit_m, abbr) = units.resolve(rustgis_geom::nice_distance(room_m));
    (rustgis_geom::nice_distance(room_m / unit_m), unit_m, abbr)
}

/// A scale bar across most of the element's width, in its units, with its divisions labelled.
/// `ground_per_pt` is ground metres per logical point of paper.
pub fn scale_bar(pm: &mut Canvas, r: Rect, ground_per_pt: f64, look: &ScaleLook, ppp: f64, comma: bool) {
    if !(ground_per_pt.is_finite() && ground_per_pt > 0.0) {
        return;
    }
    let divisions = look.divisions.clamp(1, rustgis_map::layout::MAX_SCALE_DIVISIONS);
    let px = if look.text.size > 0.0 { look.text.size * 1.33 * ppp } else { 8.0 * 1.33 * ppp } as f32;
    let face = label_face(look.text);
    let (dark, light) = (look.color, look.fill);
    let ink = look.text.color.unwrap_or(dark);
    let gap = 3.0 * ppp as f32;
    let name_of = |abbr: &str| look.unit_label.name(abbr, look.unit_text.trim());
    // Room along the element; a unit after the bar takes its share.
    let mut room = r.width() - 4.0;
    let (mut total, mut unit_m, mut abbr) = scale_length(look.units, divisions, look.division, ground_per_pt * room as f64 / ppp * 0.85);
    if look.unit_place == UnitPlace::AfterBar && look.division <= 0.0 {
        let w = label_width(&name_of(abbr), px, face);
        if w > 0.0 {
            room = (room - w - gap).max(room * 0.3);
            (total, unit_m, abbr) = scale_length(look.units, divisions, 0.0, ground_per_pt * room as f64 / ppp * 0.95);
        }
    }
    if !(total.is_finite() && total > 0.0) {
        return;
    }
    let name = name_of(abbr);
    let len = (total * unit_m / ground_per_pt * ppp) as f32;
    if !(len.is_finite() && len > 0.5) || len > 1.0e6 {
        return;
    }
    let bar_h = if look.bar_height > 0.0 { (look.bar_height * ppp) as f32 } else { (r.height() * 0.28).min(6.0 * ppp as f32) };
    // Rows from the top: the unit above, the numbers above, the bar, the numbers below, the unit
    // below; centred down the element.
    let row = px * 1.3;
    let unit_row = |p: UnitPlace| look.unit_place == p && !name.is_empty();
    let above = matches!(look.numbers, NumberPlace::Above | NumberPlace::Ends);
    let below = look.numbers == NumberPlace::Below;
    let rows_h = [unit_row(UnitPlace::Above), above, below, unit_row(UnitPlace::Below)].iter().filter(|v| **v).count() as f32 * row;
    let mut y = r.y() + ((r.height() - rows_h - bar_h) / 2.0).max(0.0);
    let x0 = r.x() + 2.0;
    let mut unit_above_y = None;
    if unit_row(UnitPlace::Above) {
        unit_above_y = Some(y + row / 2.0);
        y += row;
    }
    let numbers_above_y = above.then_some(y + row * 0.45);
    if above {
        y += row;
    }
    let y0 = y;
    let numbers_below_y = below.then_some(y0 + bar_h + row * 0.6);
    let unit_below_y = unit_row(UnitPlace::Below).then(|| y0 + bar_h + if below { row } else { 0.0 } + row / 2.0);

    // The parts the bar is drawn in: the first division split into its subdivisions.
    let seg = len / divisions as f32;
    let sub = if look.subdivisions >= 2 { look.subdivisions } else { 1 };
    let mut cuts: Vec<(f32, u8)> = vec![(x0, 2)];
    for k in 1..sub {
        cuts.push((x0 + seg * k as f32 / sub as f32, 0));
    }
    for i in 1..=divisions {
        cuts.push((x0 + seg * i as f32, if i == divisions { 2 } else { 1 }));
    }
    let parts: Vec<(f32, f32)> = cuts.windows(2).map(|w| (w[0].0, w[1].0)).collect();
    let thin = Stroke { width: 0.6 * ppp as f32, ..Stroke::default() };
    let line_w = 1.0 * ppp as f32;
    let fill = |pm: &mut Canvas, x: f32, y: f32, w: f32, h: f32, c: Rgba| {
        if let Some(b) = Rect::from_xywh(x, y, w, h) {
            pm.fill_rect(b, &paint(c, 1.0), Transform::identity(), None);
        }
    };
    let frame = |pm: &mut Canvas, x: f32, y: f32, w: f32, h: f32| {
        if let Some(b) = Rect::from_xywh(x, y, w, h) {
            pm.stroke_path(&PathBuilder::from_rect(b), &paint(dark, 1.0), &thin, Transform::identity(), None);
        }
    };
    // A tick's length for a cut: the ends, the divisions, the subdivisions.
    let tick = |rank: u8| match rank {
        2 => bar_h,
        1 => bar_h * 0.6,
        _ => bar_h * 0.35,
    };
    match look.style {
        ScaleBarStyle::Alternating => {
            for (i, (a, b)) in parts.iter().enumerate() {
                fill(pm, *a, y0, b - a, bar_h, if i % 2 == 0 { dark } else { light });
            }
            frame(pm, x0, y0, len, bar_h);
        }
        ScaleBarStyle::DoubleAlternating => {
            let half = bar_h / 2.0;
            for (i, (a, b)) in parts.iter().enumerate() {
                fill(pm, *a, y0, b - a, half, if i % 2 == 0 { dark } else { light });
                fill(pm, *a, y0 + half, b - a, half, if i % 2 == 0 { light } else { dark });
            }
            frame(pm, x0, y0, len, bar_h);
            polyline(pm, &[(x0, y0 + half), (x0 + len, y0 + half)], dark, thin.width, false);
        }
        ScaleBarStyle::Hollow => {
            fill(pm, x0, y0, len, bar_h, light);
            for (a, b) in &parts {
                frame(pm, *a, y0, b - a, bar_h);
            }
        }
        ScaleBarStyle::Stepped => {
            let mut pts = vec![(x0, y0 + bar_h)];
            for (i, (a, b)) in parts.iter().enumerate() {
                let y = if i % 2 == 0 { y0 } else { y0 + bar_h };
                pts.extend([(*a, y), (*b, y)]);
            }
            pts.push((x0 + len, y0 + bar_h));
            polyline(pm, &pts, dark, line_w, false);
        }
        ScaleBarStyle::Line | ScaleBarStyle::LineDown | ScaleBarStyle::LineCross => {
            let base = match look.style {
                ScaleBarStyle::Line => y0 + bar_h,
                ScaleBarStyle::LineDown => y0,
                _ => y0 + bar_h / 2.0,
            };
            polyline(pm, &[(x0, base), (x0 + len, base)], dark, line_w, false);
            for (x, rank) in &cuts {
                let t = tick(*rank);
                let (a, b) = match look.style {
                    ScaleBarStyle::Line => (base, base - t),
                    ScaleBarStyle::LineDown => (base, base + t),
                    _ => (base - t / 2.0, base + t / 2.0),
                };
                polyline(pm, &[(*x, a), (*x, b)], dark, thin.width, false);
            }
        }
        ScaleBarStyle::Ruler => {
            let strip = (bar_h * 0.3).max(1.0);
            fill(pm, x0, y0 + bar_h - strip, len, strip, dark);
            for (x, rank) in &cuts {
                polyline(pm, &[(*x, y0 + bar_h), (*x, y0 + bar_h - tick(*rank))], dark, thin.width, false);
            }
        }
    }
    // The numbers at the divisions, the last with the unit when it goes there; one that would touch
    // its neighbour is left out (never the ends).
    if let Some(ty) = numbers_above_y.or(numbers_below_y) {
        let ends = look.numbers == NumberPlace::Ends;
        let with_unit = look.unit_place == UnitPlace::AfterLabel && !name.is_empty();
        let labels: Vec<(f32, String)> = (0..=divisions)
            .filter(|i| !ends || *i == 0 || *i == divisions)
            .map(|i| {
                let v = decimal(scale_number(total * i as f64 / divisions as f64, total / divisions as f64), comma);
                (x0 + seg * i as f32, if i == divisions && with_unit { format!("{v} {name}") } else { v })
            })
            .collect();
        let half = |s: &str| label_width(s, px, face) / 2.0;
        let n = labels.len();
        let last = labels.last().map(|(x, s)| x - half(s)).unwrap_or(f32::MAX);
        let mut right = f32::MIN;
        for (i, (x, s)) in labels.iter().enumerate() {
            let end = i + 1 == n;
            if !end && i != 0 && (x - half(s) < right + gap || x + half(s) + gap > last) {
                continue;
            }
            // The last number with its unit is centred on its number, as a plain one is.
            let shift = if end && with_unit { half(s) - half(s.rsplit_once(' ').map_or(s.as_str(), |(v, _)| v)) } else { 0.0 };
            label_at(pm, s, x + shift, ty, px, ink, face, true);
            right = x + shift + half(s);
        }
    }
    if look.unit_place == UnitPlace::AfterBar && !name.is_empty() {
        label_at(pm, &name, x0 + len + gap, y0 + bar_h / 2.0, px, ink, face, false);
    }
    for uy in [unit_above_y, unit_below_y].into_iter().flatten() {
        label_at(pm, &name, x0 + len / 2.0, uy, px, ink, face, true);
    }
}

/// A picture of a scale bar style for a gallery: `w` × `h` pixels, four divisions, no numbers.
pub fn scale_bar_swatch(style: ScaleBarStyle, w: u32, h: u32) -> Option<Pixmap> {
    let mut pm = Canvas::new(w, h)?;
    let r = Rect::from_xywh(2.0, 0.0, w as f32 - 4.0, h as f32)?;
    let text = LabelText::scale_bar();
    let look = ScaleLook {
        units: ScaleUnits::Meters,
        divisions: 4,
        style,
        subdivisions: 0,
        division: (w as f64 - 10.0) / 4.0,
        unit_label: UnitLabel::None,
        unit_text: "",
        unit_place: UnitPlace::AfterLabel,
        numbers: NumberPlace::None,
        color: Rgba::rgb(0x22, 0x22, 0x22),
        fill: Rgba::rgb(0xFF, 0xFF, 0xFF),
        bar_height: h as f64 * 0.45,
        text: &text,
    };
    scale_bar(&mut pm, r, 1.0, &look, 1.0, false);
    Some(pm.into_pixmap())
}

/// Each visible layer's legend for what it draws at a scale denominator (#207), by layer id; a
/// layer that draws nothing there isn't listed. The scale is the one the renderer draws the frame
/// at (the one its layers' scale ranges and display filters see), not the paper scale `{scale}`
/// prints, so the legend follows what the frame actually shows.
fn shown_legends(map: &Map, scale: f64) -> Vec<(u32, Vec<LegendEntry>)> {
    map.layers
        .iter()
        .filter(|l| map.draws_at(l, scale) && !l.graphics && !l.is_raster())
        .filter_map(|l| l.shown_legend(scale).map(|entries| (l.id, entries)))
        .collect()
}

/// Complete-source legends retain one minimal value per used class, never its geometry.
fn complete_shown_legends(map: &mut Map, scale: f64) -> Option<Vec<(u32, Vec<LegendEntry>)>> {
    if !map.layers.iter().any(|l| l.source.as_ref().and_then(|s| s.chunk_source()).is_some()) {
        return Some(shown_legends(map, scale));
    }
    let wanted: Vec<usize> =
        map.layers.iter().enumerate().filter(|(_, l)| map.draws_at(l, scale) && !l.graphics && !l.is_raster()).map(|(i, _)| i).collect();
    let mut out = Vec::new();
    for index in wanted {
        let layer = map.layers.get_mut(index)?;
        let Some(source) = layer.source.as_ref().and_then(|s| s.chunk_source()) else {
            if let Some(entries) = layer.shown_legend(scale) {
                out.push((layer.id, entries));
            }
            continue;
        };
        let unique = match &layer.renderer {
            rustgis_map::Renderer::Unique { field, classes, .. } => Some((field, classes)),
            _ => None,
        };
        // By expression: a feature of each class seen is kept whole, for the legend to class again.
        let by_expression = layer.renderer.expression().is_some();
        let fi = unique.filter(|_| !by_expression).and_then(|(field, _)| layer.dataset.field_index(field));
        let fields = if by_expression {
            layer.dataset.fields.clone()
        } else {
            unique
                .filter(|_| fi.is_some())
                .map_or_else(Vec::new, |(field, _)| vec![rustgis_data::Field::new(field, rustgis_data::FieldType::Text)])
        };
        let mut sample = rustgis_data::Dataset::new("legend", layer.geometry_type(), fields);
        let mut used = std::collections::HashSet::new();
        let mut any = false;
        let mut chunk = layer.clone();
        chunk.source = None;
        chunk.clear_cache();
        let result = crate::visit_complete(source.as_ref(), &mut |features| {
            chunk.dataset.features = features.into();
            let rows = chunk.drawn_rows(scale);
            let classer = by_expression.then(|| chunk.renderer.classer(&chunk.dataset, Some(scale)));
            for row in rows.iter().enumerate().filter_map(|(row, keep)| keep.then_some(row)) {
                any = true;
                if let Some(c) = &classer {
                    let key = chunk.renderer.unique_class(&chunk.dataset, row, c).unwrap_or(usize::MAX);
                    // Kept with its id ($id), without its geometry.
                    if used.insert(key)
                        && let Some(f) = chunk.dataset.features.get(row)
                    {
                        let mut f = f.clone();
                        f.geometry = None;
                        sample.features.push(f);
                    }
                } else if let Some((_, classes)) = unique {
                    let value = fi.map(|fi| chunk.dataset.value(row, fi).to_string());
                    let key = value.as_ref().and_then(|v| classes.iter().position(|(k, _)| k == v)).unwrap_or(classes.len());
                    if used.insert(key) {
                        sample.push(None, value.map_or_else(Vec::new, |v| vec![rustgis_data::Value::Text(v)]));
                    }
                }
            }
            chunk.dataset.features = Vec::new().into();
            Ok(())
        });
        if let Err(e) = result {
            if let Some(source) = &mut layer.source {
                source.error = Some(e.to_string());
            }
            return None;
        }
        if any {
            // (A sample of the classes seen: classed again at this scale, by expression.)
            out.push((layer.id, layer.renderer.legend_at(&sample, &[], Some(scale))));
        }
    }
    Some(out)
}

/// What a legend leaves out: `<all other values>`, and layers by name; the scale denominator
/// whose legend pictures a WMS or WMTS layer shows; and how it looks.
#[derive(Clone, Copy)]
struct LegendOptions<'a> {
    hide_other: bool,
    exclude: &'a [String],
    scale: f64,
    look: &'a LegendLook,
}

/// A row of a legend: a layer drawn in one symbol (its swatch and name), a layer's name over its
/// classes, or one of them, or a WMS or WMTS layer's legend picture (under its name).
enum LegendRow<'a> {
    Plain(&'a rustgis_map::Layer),
    Head(&'a str),
    Class(&'a rustgis_map::Layer, LegendEntry),
    Picture(std::sync::Arc<rustgis_tiles::cache::Payload>),
}

impl LegendRow<'_> {
    /// Its height at 1:`scale`, in points, the room under it left out: a picture's a pixel a
    /// point, as the Layers pane shows it; a text row's as tall as its type and patch need, and no
    /// less than [`LEGEND_ROW`]; a row grows to hold its point symbol.
    fn height(&self, look: &LegendLook, scale: f64) -> f32 {
        let patch = patch_height(look) + 6.0;
        let (_, pt) = self.typ(look);
        match self {
            LegendRow::Picture(p) => picture_size(p).1,
            LegendRow::Head(_) => text_row(pt),
            LegendRow::Plain(_) | LegendRow::Class(..) => text_row(pt).max(patch).max(self.symbol_side(scale)),
        }
    }

    /// Its height and the room under it, in points ([`LegendLook::spacing`]).
    fn pitch(&self, opts: &LegendOptions) -> f32 {
        self.height(opts.look, opts.scale) + opts.look.spacing as f32
    }

    /// The type its text is drawn in, and its size in points: the one set for this layer or class,
    /// else the legend's for its kind of row.
    fn typ<'b>(&'b self, look: &'b LegendLook) -> (&'b LabelText, f32) {
        let (t, default) = match self {
            LegendRow::Plain(l) => (look.name_type(&l.name).unwrap_or(&look.label), LAYER_PT),
            LegendRow::Head(name) => (look.name_type(name).unwrap_or(&look.layer), LAYER_PT),
            LegendRow::Class(l, en) => (look.label_type(&l.name, &en.label).unwrap_or(&look.label), LABEL_PT),
            LegendRow::Picture(_) => (&look.label, LABEL_PT),
        };
        (t, type_size(t, default))
    }

    /// The text it shows: a layer's name or a class's label, or the one the legend sets for it.
    fn text<'b>(&'b self, look: &'b LegendLook) -> &'b str {
        match self {
            LegendRow::Plain(l) => look.name_of(&l.name).unwrap_or(&l.name),
            LegendRow::Head(name) => look.name_of(name).unwrap_or(name),
            LegendRow::Class(l, en) => look.label_of(&l.name, &en.label).unwrap_or(&en.label),
            LegendRow::Picture(_) => "",
        }
    }

    /// The side of the square its point symbol takes at 1:`scale`, in points, as far as its marks
    /// reach ([`point_marks::reach`]); 0 for any other row. No larger than a legend picture.
    fn symbol_side(&self, scale: f64) -> f32 {
        let (LegendRow::Plain(l) | LegendRow::Class(l, _)) = self else { return 0.0 };
        if l.geometry_type() != GeometryType::Point {
            return 0.0;
        }
        let (s, strokes) = match self {
            LegendRow::Class(_, en) => (en.symbol, row_strokes(l, en)),
            _ => (l.renderer.symbol_for(&l.dataset, 0), l.line_strokes.as_slice()),
        };
        (2.0 * point_marks::reach(&s, strokes) * l.size_factor(scale)).min(MAX_LEGEND_SIDE as f32)
    }

    /// Its width beside a column of patches `column` points wide ([`swatch_column`]), in points,
    /// in the type `look` sets.
    fn width(&self, column: f32, look: &LegendLook) -> f32 {
        let (t, pt) = self.typ(look);
        match self {
            LegendRow::Plain(_) => column + 6.0 + legend_text_width(self.text(look), pt, t),
            LegendRow::Head(_) => legend_text_width(self.text(look), pt, t),
            LegendRow::Class(..) => column + 12.0 + legend_text_width(self.text(look), pt, t),
            LegendRow::Picture(p) => 6.0 + picture_size(p).0,
        }
    }
}

/// A legend's text sizes until set (points): the title's, a layer's name's and a class's label's.
const TITLE_PT: f32 = 12.0;
const LAYER_PT: f32 = 9.0;
const LABEL_PT: f32 = 8.5;

/// `t`'s size in points, `default` when it is sized automatically.
fn type_size(t: &LabelText, default: f32) -> f32 {
    if t.size > 0.0 { t.size as f32 } else { default }
}

/// The height of a row of text `pt` points high: [`LEGEND_ROW`] at most sizes.
fn text_row(pt: f32) -> f32 {
    (pt * 1.66).max(LEGEND_ROW)
}

/// The title's row.
fn title_row(look: &LegendLook) -> f32 {
    (type_size(&look.title, TITLE_PT) * 1.625).max(LEGEND_ROW * 1.3)
}

/// A patch's width and height (points): 16 by 9 until set.
fn patch_width(look: &LegendLook) -> f32 {
    if look.patch_width > 0.0 { look.patch_width as f32 } else { LEGEND_SWATCH }
}

fn patch_height(look: &LegendLook) -> f32 {
    if look.patch_height > 0.0 { look.patch_height as f32 } else { LEGEND_ROW * 0.6 }
}

/// Whether `t` is the legend's own sans, drawn as it always was.
fn plain_type(t: &LabelText) -> bool {
    !t.italic && t.font == FontFamily::Sans && t.font_name.is_none()
}

/// A legend's text `s` in type `t` at `pt` points, starting at `x`, its middle at `cy`.
#[allow(clippy::too_many_arguments)]
fn legend_text(pm: &mut Canvas, s: &str, x: f32, cy: f32, pt: f32, t: &LabelText, ppp: f32) {
    let color = t.color.unwrap_or(LEGEND_TEXT);
    if plain_type(t) {
        text::text_at(pm, s, x, cy, pt * 1.33 * ppp, color, t.bold, text::Align::Left, false);
    } else {
        label_at(pm, s, x, cy, pt * 1.33 * ppp, color, label_face(t), false);
    }
}

/// How wide [`legend_text`] draws `s`, in points.
fn legend_text_width(s: &str, pt: f32, t: &LabelText) -> f32 {
    if plain_type(t) { text::measure(s, pt, t.bold) } else { label_width(s, pt * 1.33, label_face(t)) }
}

/// Rows laid into at most `columns` columns `limit` points high: the row each column starts at,
/// and whether they all fit. A layer's name stays with the row under it; a row taller than
/// `limit` has a column to itself, and the last column takes what is left. The room under a
/// column's last row ([`LegendRow::pitch`]) isn't counted.
fn flow(rows: &[LegendRow], columns: usize, limit: f32, opts: &LegendOptions) -> (Vec<usize>, bool) {
    // A thousandth of a row's slack, as the rows have when drawn.
    let limit = limit + LEGEND_ROW * 1e-3 + opts.look.spacing as f32;
    let (mut starts, mut used, mut fits, mut i) = (vec![0], 0.0f32, true, 0);
    while i < rows.len() {
        let end = if matches!(rows[i], LegendRow::Head(_)) { (i + 2).min(rows.len()) } else { i + 1 };
        let part: f32 = rows[i..end].iter().map(|row| row.pitch(opts)).sum();
        if used > 0.0 && used + part > limit && starts.len() < columns {
            starts.push(i);
            used = 0.0;
        }
        used += part;
        fits &= used <= limit;
        i = end;
    }
    (starts, fits)
}

/// The height of a column of `rows`, in points: their pitches, the room under the last left out.
fn column_height(rows: &[LegendRow], opts: &LegendOptions) -> f32 {
    if rows.is_empty() {
        return 0.0;
    }
    (rows.iter().map(|row| row.pitch(opts)).sum::<f32>() - opts.look.spacing as f32).max(0.0)
}

/// The rows laid into at most `columns` columns as evenly as they go: where each starts
/// ([`flow`]), and the height of the tallest, in points.
fn balanced(rows: &[LegendRow], columns: usize, opts: &LegendOptions) -> (Vec<usize>, f32) {
    let total = column_height(rows, opts);
    let (mut low, mut high) = (total / columns as f32, total);
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        if flow(rows, columns, middle, opts).1 {
            high = middle;
        } else {
            low = middle;
        }
    }
    let starts = flow(rows, columns, high, opts).0;
    let tallest = column_parts(rows, &starts).map(|part| column_height(part, opts)).fold(0.0f32, f32::max);
    (starts, tallest)
}

/// The rows of each column, from where they start.
fn column_parts<'r, 'a>(rows: &'r [LegendRow<'a>], starts: &'r [usize]) -> impl Iterator<Item = &'r [LegendRow<'a>]> {
    starts.iter().enumerate().map(|(k, &start)| &rows[start..starts.get(k + 1).copied().unwrap_or(rows.len())])
}

/// The widest of a column's `rows`, in points.
fn widest(rows: &[LegendRow], opts: &LegendOptions) -> f32 {
    let column = swatch_column(rows, opts);
    rows.iter().map(|row| row.width(column, opts.look)).fold(0.0, f32::max)
}

/// The stacked strokes a legend's class row draws: its class's own, else its layer's, as
/// [`rustgis_map::Layer::strokes`] finds them for a feature.
fn row_strokes<'a>(l: &'a rustgis_map::Layer, en: &'a LegendEntry) -> &'a [rustgis_map::LineStroke] {
    if en.strokes.is_empty() { &l.line_strokes } else { &en.strokes }
}

/// The width of a legend's column of patches, in points: the patch's width ([`patch_width`]),
/// grown to hold the widest point symbol, so the labels line up.
fn swatch_column(rows: &[LegendRow], opts: &LegendOptions) -> f32 {
    rows.iter().map(|row| row.symbol_side(opts.scale)).fold(patch_width(opts.look), f32::max)
}

/// A legend picture's size in points (a pixel a point).
fn picture_size(p: &rustgis_tiles::cache::Payload) -> (f32, f32) {
    match p {
        rustgis_tiles::cache::Payload::Image(pm) => (pm.width() as f32, pm.height() as f32),
        _ => (0.0, 0.0),
    }
}

/// Whether a legend lists a WMS or WMTS layer: drawn, and with a legend.
fn tile_listed(t: &rustgis_map::TileLayer) -> bool {
    t.draws() && t.opacity > 0.0 && !t.legends.is_empty()
}

/// A WMS or WMTS layer's legend picture for the map at 1:`scale`, as the Layers pane shows it:
/// the first of its legends there that is a picture. `None` while the one to show is still on
/// its way (`pending` then set), or when none can be shown. The addresses come from a service or
/// a project, so they are checked as the layer's own are.
fn tile_legend(t: &rustgis_map::TileLayer, scale: f64, pending: &mut bool) -> Option<std::sync::Arc<rustgis_tiles::cache::Payload>> {
    use rustgis_tiles::cache::{self, Payload, State, Want};
    for l in rustgis_data::ogc::legends_at(&t.legends, scale) {
        if rustgis_tiles::address::check(&l.url, Some(&t.url)).is_err() {
            continue;
        }
        match cache::get(&l.url, Want::Image) {
            // No larger than the Layers pane shows one.
            State::Ready(p) if matches!(&*p, Payload::Image(pm) if pm.width() <= MAX_LEGEND_SIDE && pm.height() <= MAX_LEGEND_SIDE) => {
                return Some(p);
            }
            State::Pending => {
                *pending = true;
                return None;
            }
            // The next, if there is one.
            _ => {}
        }
    }
    None
}

/// Whether a legend picture `map`'s WMS and WMTS layers show at 1:`scale` is still on its way
/// (asking for those not yet asked for); those `exclude` names aren't listed, so aren't waited for.
pub fn tile_legends_pending(map: &Map, scale: f64, exclude: &[String]) -> bool {
    let mut pending = false;
    for t in map.tiles.iter().filter(|t| tile_listed(t) && !exclude.iter().any(|n| n.eq_ignore_ascii_case(&t.name))) {
        let _ = tile_legend(t, scale, &mut pending);
    }
    pending
}

/// Whether a legend on the page `layout` lists a WMS or WMTS layer whose legend picture is still
/// on its way, at its frame's scale: in the browser, which doesn't wait for them, the page is
/// drawn again once it lands.
pub fn legends_pending(layout: &Layout, maps: &[Map]) -> bool {
    layout.elements.iter().filter(|e| e.visible).any(|e| {
        let ElementKind::Legend { frame, exclude, .. } = &e.kind else { return false };
        let Some(ElementKind::MapFrame { map, center, scale, bearing, pitch, .. }) = layout.element(*frame).map(|f| &f.kind) else {
            return false;
        };
        let Some(m) = maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)) else { return false };
        let cam = frame_camera(m, *center, *scale, *bearing, *pitch);
        tile_legends_pending(m, m.ground_res_at(&cam) / rustgis_geom::PIXEL_M, exclude)
    })
}

/// Waits (natively, up to half a minute) for the legend pictures a legend of `map` at 1:`scale`
/// shows, as a frame waits for its tiles: a page needs them all. Only for those: not for the
/// cache's other fetches, nor for a layer the legend leaves out (`exclude`).
fn wait_for_tile_legends(map: &Map, scale: f64, exclude: &[String]) {
    let start = web_time::Instant::now();
    let budget = web_time::Duration::from_secs(30);
    // In the browser, which can't wait, the page draws what has arrived.
    while cfg!(not(target_arch = "wasm32")) && tile_legends_pending(map, scale, exclude) && start.elapsed() < budget {
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
}

/// The widest and tallest legend picture shown, in pixels, as in the Layers pane, and the largest
/// side of a legend's point symbol, in points.
const MAX_LEGEND_SIDE: u32 = 2048;

/// A legend's text colour.
const LEGEND_TEXT: Rgba = Rgba::rgb(0x22, 0x22, 0x22);
/// A legend row's height, in points.
const LEGEND_ROW: f32 = 15.0;
/// A legend swatch's width, in points.
const LEGEND_SWATCH: f32 = 16.0;
/// The room between a legend's columns, in points.
const LEGEND_COLUMN_GAP: f32 = 10.0;

/// The rows of the legend of a frame's map. With `drawn` (from [`shown_legends`]) it shows what the
/// frame draws: only the layers listed there, with their classes. Without, every class of every
/// visible layer. `opts` leaves out `<all other values>` and the layers it names, either way.
fn legend_rows<'a>(map: &'a Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>, opts: &LegendOptions) -> Vec<LegendRow<'a>> {
    let excluded = |name: &str| opts.exclude.iter().any(|n| n.eq_ignore_ascii_case(name));
    let mut rows = Vec::new();
    // In the order the map draws them and the Layers pane lists them, top first.
    for item in map.stack() {
        let l = match item {
            rustgis_map::Drawn::Layer(i) => match map.layers.get(i) {
                Some(l) => l,
                None => continue,
            },
            // A WMS or WMTS layer's legend is its service's picture, whatever the frame draws of it.
            rustgis_map::Drawn::Tile(i) => {
                if let Some(t) = map.tiles.get(i).filter(|t| tile_listed(t) && !excluded(&t.name))
                    && let Some(p) = tile_legend(t, opts.scale, &mut false)
                {
                    rows.push(LegendRow::Head(&t.name));
                    rows.push(LegendRow::Picture(p));
                }
                continue;
            }
        };
        if !(map.shows(l) && !l.graphics && !l.is_raster() && !excluded(&l.name)) {
            continue;
        }
        let mut entries = match drawn {
            Some(d) => match d.iter().find(|(id, _)| *id == l.id) {
                Some((_, entries)) => entries.clone(),
                None => continue,
            },
            None => l.renderer.legend(),
        };
        // The fallback row only (a class that happens to bear its name stays).
        if opts.hide_other && entries.iter().any(|e| e.fallback) {
            entries.retain(|e| !e.fallback);
            // Nothing left to show for this layer: leave it out rather than draw its plain swatch
            // (unless a diagram lists its categories below).
            if entries.is_empty() && l.diagram.is_none() {
                continue;
            }
        }
        // A layer with a diagram lists its categories under its classes.
        if let Some(d) = &l.diagram {
            entries.extend(d.legend(l.geometry_type() == GeometryType::Polyline));
        }
        if entries.is_empty() {
            // A Unique Values layer that lists nothing draws nothing visible (#207).
            if !matches!(l.renderer, rustgis_map::Renderer::Unique { .. }) {
                rows.push(LegendRow::Plain(l));
            }
            continue;
        }
        rows.push(LegendRow::Head(&l.name));
        rows.extend(entries.into_iter().map(|en| LegendRow::Class(l, en)));
    }
    rows
}

/// A piece of a legend as [`legend`] lays it out, in pixels: a text (its left and its middle),
/// a symbol's patch (in its box at `x`, `y`: a column of patches `w` wide, the row `h` high, for
/// the layer's symbol and the strokes stacked on it), or a WMS or WMTS layer's legend picture (at
/// `y` in its column `r`, cut at the legend's bottom).
enum Piece<'a> {
    Text {
        s: String,
        x: f32,
        cy: f32,
        pt: f32,
        t: LabelText,
    },
    Swatch {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        layer: &'a rustgis_map::Layer,
        symbol: rustgis_map::Symbol,
        strokes: Vec<rustgis_map::LineStroke>,
    },
    Picture {
        r: Rect,
        y: f32,
        pic: std::sync::Arc<rustgis_tiles::cache::Payload>,
    },
}

/// The pieces of the legend of a frame's map in `r`: its title, and its rows ([`legend_rows`])
/// under it in `columns` columns, each as wide as its widest row, in the type, patches and spacing
/// its look sets. As even as they go in the height the box leaves; in less, each is filled in turn
/// and the last rows are left out. In a box too narrow they close up, so that none is left out for
/// it: long labels run into the next, and the last column's past the box, as a long label does in
/// one column.
fn legend_pieces<'a>(
    r: Rect,
    map: &'a Map,
    (title, columns): (&str, usize),
    drawn: Option<&[(u32, Vec<LegendEntry>)]>,
    opts: LegendOptions,
    ppp: f32,
) -> Vec<Piece<'a>> {
    let look = opts.look;
    let mut out = Vec::new();
    let mut y = r.y();
    if !title.is_empty() {
        // Its middle where it always was: half a row down, the rest of its row under it.
        let line = title_row(look) * ppp;
        let cy = y + (line - LEGEND_ROW * 0.3 * ppp) / 2.0;
        out.push(Piece::Text { s: title.to_string(), x: r.x(), cy, pt: type_size(&look.title, TITLE_PT), t: look.title.clone() });
        y += line;
    }
    let rows = legend_rows(map, drawn, &opts);
    let columns = columns.clamp(1, MAX_LEGEND_COLUMNS);
    let starts = flow(&rows, columns, balanced(&rows, columns, &opts).1.min((r.bottom() - y) / ppp), &opts).0;
    let across =
        (column_parts(&rows, &starts).map(|part| widest(part, &opts)).sum::<f32>() + LEGEND_COLUMN_GAP * (starts.len() - 1) as f32) * ppp;
    let close_up = if across > r.width() { r.width() / across } else { 1.0 };
    let mut x = r.x();
    for (k, part) in column_parts(&rows, &starts).enumerate() {
        // The last has the rest of the box's width.
        let right = if k + 1 < starts.len() { x + widest(part, &opts) * ppp * close_up } else { r.right() };
        let Some(column) = Rect::from_ltrb(x, y, right, r.bottom()) else { break };
        column_pieces(&mut out, column, part, columns > 1, &opts, ppp);
        x = right + LEGEND_COLUMN_GAP * ppp * close_up;
    }
    out
}

/// The pieces of a column of a legend: its rows from the top of `r`, as many as fit. With
/// `name_with_class`, a layer's name only with room for the class under it, as [`flow`] keeps them
/// together.
fn column_pieces<'a>(out: &mut Vec<Piece<'a>>, r: Rect, rows: &[LegendRow<'a>], name_with_class: bool, opts: &LegendOptions, ppp: f32) {
    let look = opts.look;
    let mut y = r.y();
    let line = LEGEND_ROW * ppp;
    let gap = look.spacing as f32 * ppp;
    let sw = swatch_column(rows, opts) * ppp;
    let mut rows = rows.iter().peekable();
    while let Some(row) = rows.next() {
        if let LegendRow::Picture(p) = row {
            match &**p {
                rustgis_tiles::cache::Payload::Image(pic) => match picture_fit(r, y, pic, ppp) {
                    Some((h, whole)) => {
                        out.push(Piece::Picture { r, y, pic: p.clone() });
                        if !whole {
                            // Cut at the bottom.
                            break;
                        }
                        y += h + gap;
                    }
                    // No room for it.
                    None => break,
                },
                _ => continue,
            }
            continue;
        }
        let height = row.height(look, opts.scale) * ppp;
        // The rectangle's own bottom, which a sum of its top and height can miss by a rounding,
        // and a thousandth of a row's slack: a legend sized to its rows keeps the last one.
        if y + height > r.bottom() + line * 1e-3 {
            break;
        }
        // A WMS or WMTS layer's name only with room for some of its picture under it.
        if let (LegendRow::Head(_), Some(LegendRow::Picture(p))) = (row, rows.peek())
            && let rustgis_tiles::cache::Payload::Image(pic) = &**p
            && y + height + gap + picture_scale(r, pic, ppp) > r.bottom() + line * 1e-3
        {
            break;
        }
        if let (true, LegendRow::Head(_), Some(next)) = (name_with_class, row, rows.peek())
            && matches!(next, LegendRow::Class(..))
            && y + height + gap + next.height(look, opts.scale) * ppp > r.bottom() + line * 1e-3
        {
            break;
        }
        let s = row.text(look).to_string();
        let (t, pt) = row.typ(look);
        let t = t.clone();
        let cy = y + height / 2.0;
        match row {
            LegendRow::Plain(l) => {
                let symbol = l.renderer.symbol_for(&l.dataset, 0);
                out.push(Piece::Swatch { x: r.x(), y, w: sw, h: height, layer: l, symbol, strokes: l.line_strokes.clone() });
                out.push(Piece::Text { s, x: r.x() + sw + 6.0 * ppp, cy, pt, t });
            }
            LegendRow::Head(_) => out.push(Piece::Text { s, x: r.x(), cy, pt, t }),
            LegendRow::Class(l, en) => {
                let strokes = row_strokes(l, en).to_vec();
                out.push(Piece::Swatch { x: r.x() + 6.0 * ppp, y, w: sw, h: height, layer: l, symbol: en.symbol, strokes });
                out.push(Piece::Text { s, x: r.x() + sw + 12.0 * ppp, cy, pt, t });
            }
            LegendRow::Picture(_) => {}
        }
        y += height + gap;
    }
}

/// The legend of a frame's map: its pieces ([`legend_pieces`]), with the patches its look sets,
/// a point layer's symbol drawn as the map draws its points.
fn legend(
    pm: &mut Canvas,
    r: Rect,
    map: &Map,
    title_columns: (&str, usize),
    drawn: Option<&[(u32, Vec<LegendEntry>)]>,
    opts: LegendOptions,
    ppp: f64,
) {
    let ppp = ppp as f32;
    let look = opts.look;
    let patch = Patch { w: patch_width(look) * ppp, h: patch_height(look) * ppp, set: look.patch_height > 0.0, look };
    for piece in legend_pieces(r, map, title_columns, drawn, opts, ppp) {
        match piece {
            Piece::Text { s, x, cy, pt, t } => legend_text(pm, &s, x, cy, pt, &t, ppp),
            Piece::Swatch { x, y, w, h, layer, symbol, strokes } => {
                let opacity = map.layer_opacity(layer);
                swatch(pm, (x, y, w, h), layer, (&symbol, &strokes), (opacity, opts.scale), &patch, ppp);
            }
            Piece::Picture { r, y, pic } => {
                if let rustgis_tiles::cache::Payload::Image(pic) = &*pic {
                    legend_picture(pm, r, y, pic, ppp);
                }
            }
        }
    }
}

/// A piece of a legend turned into graphics, in millimetres from the legend's top left.
#[derive(Clone, Debug, PartialEq)]
pub enum LegendPiece {
    /// A text: its left, its middle, how wide it draws and its size in points, in the type it is
    /// drawn in.
    Text { text: String, x: f64, y: f64, w: f64, size: f64, t: LabelText },
    /// A symbol's patch filling the box `x`, `y`, `w`, `h`.
    Patch { x: f64, y: f64, w: f64, h: f64, geometry: GeometryType, symbol: rustgis_map::Symbol },
    /// A WMS or WMTS layer's legend picture (PNG) filling its box.
    Picture { x: f64, y: f64, w: f64, h: f64, png: Vec<u8> },
}

/// The pieces of a legend `w_mm` by `h_mm` (as [`legend`] draws it in `columns` columns, the rows
/// that fit), for turning it into graphics: the classes a frame shows at the renderer's scale
/// denominator `scale` (as [`legend_sizes_mm`] reads them), or every class with `all_classes`.
/// `None` when a complete source can't be read.
#[allow(clippy::too_many_arguments)]
pub fn legend_pieces_mm(
    map: &mut Map,
    scale: f64,
    (title, columns): (&str, usize),
    all_classes: bool,
    hide_other: bool,
    exclude: &[String],
    look: &LegendLook,
    (w_mm, h_mm): (f64, f64),
) -> Option<Vec<LegendPiece>> {
    let drawn = if all_classes { None } else { Some(complete_shown_legends(map, scale)?) };
    wait_for_tile_legends(map, scale, exclude);
    let r = Rect::from_xywh(0.0, 0.0, (w_mm * PT_PER_MM) as f32, (h_mm * PT_PER_MM) as f32)?;
    let opts = LegendOptions { hide_other, exclude, scale, look };
    let mm = |v: f32| f64::from(v) / PT_PER_MM;
    let (pw, ph, set) = (patch_width(look), patch_height(look), look.patch_height > 0.0);
    let mut out = Vec::new();
    for piece in legend_pieces(r, map, (title, columns), drawn.as_deref(), opts, 1.0) {
        out.push(match piece {
            Piece::Text { s, x, cy, pt, t } => {
                let w = mm(legend_text_width(&s, pt, &t));
                LegendPiece::Text { text: s, x: mm(x), y: mm(cy), w, size: f64::from(pt), t }
            }
            // A point's symbol in the whole of its box, as the legend draws it.
            Piece::Swatch { x, y, w, h, layer, symbol, .. } if layer.geometry_type() == GeometryType::Point => {
                LegendPiece::Patch { x: mm(x), y: mm(y), w: mm(w), h: mm(h), geometry: GeometryType::Point, symbol }
            }
            Piece::Swatch { x, y, w, h, layer, symbol, .. } => {
                // The box `swatch` fills in the row, in the middle of the column: the patch's height
                // set by hand, else the share of the row each kind of patch takes.
                let gt = layer.geometry_type();
                let bh = match gt {
                    _ if set => ph,
                    GeometryType::Polyline => h * 0.4,
                    _ => h * 0.6,
                };
                let x = x + (w - pw) / 2.0;
                LegendPiece::Patch { x: mm(x), y: mm(y + (h - bh) / 2.0), w: mm(pw), h: mm(bh), geometry: gt, symbol }
            }
            Piece::Picture { r, y, pic } => {
                let rustgis_tiles::cache::Payload::Image(pic) = &*pic else { continue };
                let Some((h, _)) = picture_fit(r, y, pic, 1.0) else { continue };
                let k = picture_scale(r, pic, 1.0);
                let rows = (h / k.max(f32::MIN_POSITIVE)).round().clamp(1.0, pic.height() as f32) as u32;
                let shown = tiny_skia::IntRect::from_xywh(0, 0, pic.width(), rows).and_then(|c| pic.clone_rect(c));
                let Some(png) = shown.and_then(|p| p.encode_png().ok()) else { continue };
                LegendPiece::Picture { x: mm(r.x() + 6.0), y: mm(y), w: mm(pic.width() as f32 * k), h: mm(h), png }
            }
        });
    }
    Some(out)
}

/// Draws a legend's patch on its own (a [`ElementKind::Swatch`]), filling `r`: a point's symbol as
/// the map draws it, made smaller to fit when it is larger than `r`.
fn lone_swatch(pm: &mut Canvas, r: Rect, gt: GeometryType, symbol: &rustgis_map::Symbol, shapes: (LinePatch, AreaPatch), ppp: f32) {
    if gt == GeometryType::Point {
        let side = 2.0 * point_marks::reach(symbol, &[]) * ppp;
        let k = if side > r.width().min(r.height()) && side > 0.0 { r.width().min(r.height()) / side } else { 1.0 };
        point_swatch(pm, (r.x(), r.y(), r.width(), r.height()), (symbol, &[]), 1.0, ppp * k);
        return;
    }
    let look = LegendLook { line_patch: shapes.0, area_patch: shapes.1, ..LegendLook::default() };
    let patch = Patch { w: r.width(), h: r.height(), set: true, look: &look };
    patch_swatch(pm, (r.x(), r.y(), r.height()), gt, symbol, &patch, ppp);
}

/// The pixels a legend picture's pixel takes in the legend `r`: a pixel a point, narrowed to fit
/// across after its indent.
fn picture_scale(r: Rect, pic: &Pixmap, ppp: f32) -> f32 {
    ppp.min((r.right() - r.x() - 6.0 * ppp).max(0.0) / pic.width().max(1) as f32)
}

/// The source rows of a legend picture that fit from `y` in the legend `r`, a thousandth of a
/// row's slack, as the rows have; `None` when not one does.
fn picture_rows(r: Rect, y: f32, pic: &Pixmap, ppp: f32) -> Option<f32> {
    let k = picture_scale(r, pic, ppp);
    let room = r.bottom() - y;
    let rows = if k > 0.0 { ((room + LEGEND_ROW * ppp * 1e-3) / k).floor().min(pic.height() as f32) } else { 0.0 };
    (rows >= 1.0).then_some(rows)
}

/// The height a legend picture takes from `y` in the legend `r` and whether it all fits, as
/// [`legend_picture`] draws it; `None` when none of it does.
fn picture_fit(r: Rect, y: f32, pic: &Pixmap, ppp: f32) -> Option<(f32, bool)> {
    let rows = picture_rows(r, y, pic, ppp)?;
    Some((rows * picture_scale(r, pic, ppp), (rows as u32) == pic.height()))
}

/// Draws a WMS or WMTS layer's legend picture at `y` in the legend `r`, indented as a layer's
/// classes, a pixel a point, narrowed to fit across ([`picture_scale`]). One taller than the room
/// left is cut at the bottom. The height drawn and whether it all fitted; `None` when none of it
/// did.
fn legend_picture(pm: &mut Canvas, r: Rect, y: f32, pic: &Pixmap, ppp: f32) -> Option<(f32, bool)> {
    let x = r.x() + 6.0 * ppp;
    let k = picture_scale(r, pic, ppp);
    let rows = picture_rows(r, y, pic, ppp)?;
    let shown;
    let src = if (rows as u32) < pic.height() {
        shown = tiny_skia::IntRect::from_xywh(0, 0, pic.width(), rows as u32).and_then(|c| pic.clone_rect(c))?;
        &shown
    } else {
        pic
    };
    pm.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() },
        Transform::from_row(k, 0.0, 0.0, k, x, y),
        None,
    );
    Some((rows * k, (rows as u32) == pic.height()))
}

/// The size a legend with no title takes in one column, in two and so on, in mm (width, height):
/// its rows as [`legend`] draws them in the type, patches and spacing `look` sets, the classes a
/// frame shows at the renderer's scale denominator `scale` (as the layout finds it for a frame, a
/// complete source read through as [`render`] reads it), or every class with `all_classes`. For
/// sizing a legend to its content. `None` when a complete source can't be read, as [`render`] then
/// draws no page. A WMS or WMTS layer's legend picture still on its way isn't waited for, nor
/// counted ([`tile_legends_pending`] says whether one is).
pub fn legend_sizes_mm(
    map: &mut Map,
    scale: f64,
    all_classes: bool,
    hide_other: bool,
    exclude: &[String],
    look: &LegendLook,
) -> Option<[(f64, f64); MAX_LEGEND_COLUMNS]> {
    let drawn = if all_classes { None } else { Some(complete_shown_legends(map, scale)?) };
    let opts = LegendOptions { hide_other, exclude, scale, look };
    let rows = legend_rows(map, drawn.as_deref(), &opts);
    let mm = 25.4 / 96.0;
    Some(std::array::from_fn(|i| {
        let (starts, height) = balanced(&rows, i + 1, &opts);
        let width: f32 =
            column_parts(&rows, &starts).map(|part| widest(part, &opts)).sum::<f32>() + LEGEND_COLUMN_GAP * (starts.len() - 1) as f32;
        (f64::from(width) * mm, f64::from(height) * mm)
    }))
}

/// The height in mm [`legend`] gives a title in `look`'s type above its rows, which
/// [`legend_sizes_mm`] leaves out.
pub fn legend_title_mm(look: &LegendLook) -> f64 {
    f64::from(title_row(look)) * 25.4 / 96.0
}

/// The renderer's scale denominator for a frame showing `map` at paper scale 1:`scale` (the one
/// [`legend_sizes_mm`] takes).
pub fn frame_render_scale(map: &Map, center: Coord, scale: f64) -> f64 {
    let cam = frame_camera(map, center, scale, 0.0, 0.0);
    map.ground_res_at(&cam) / rustgis_geom::PIXEL_M
}

/// Whether a layer only outlines its polygons, with no fill and no labels: context around a map's
/// data, such as the neighbouring states' borders, which a surround can sit on. No Fill counts,
/// whatever the renderer; a transparent symbol under stacked fills such as a hatch (the layer's or a
/// class's), a fill by expression, diagrams or extrusion are more than an outline.
fn only_outlines(l: &rustgis_map::Layer) -> bool {
    use rustgis_map::NoFill;
    let hollow = l.no_fill == Some(NoFill::Override)
        || (!l.has_fills()
            && (l.no_fill == Some(NoFill::Symbols) || matches!(&l.renderer, rustgis_map::Renderer::Single(s) if s.fill.0[3] == 0)));
    l.geometry_type() == GeometryType::Polygon
        && !l.labels.enabled
        && hollow
        && l.symbol_by.fill.is_none()
        && l.diagram.is_none()
        && l.extrusion.is_none()
}

/// Where a frame showing `map` at 1:`scale` draws its layers: a grid of about `cells` cells across
/// the frame's longer side, row by row from the top, each `true` where a layer (or its labels)
/// drew. The basemap is left out, so the frame's background isn't counted, and so are layers that
/// only outline their polygons ([`only_outlines`]) unless nothing else draws at that scale. For
/// keeping a layout's surrounds off what its map shows. `None` when the frame can't be drawn.
///
/// The map itself is drawn, not a copy of it (every layer's features): its basemap, web tiles
/// (a background, like the basemap) and graticule are set aside for the drawing and put back,
/// however it ends, as is a label lock waiting to be filled, which the coarse drawing would
/// otherwise fill.
pub fn frame_coverage(map: &mut Map, center: Coord, scale: f64, w_mm: f64, h_mm: f64, cells: u32) -> Option<(u32, u32, Vec<bool>)> {
    if !(w_mm > 0.0 && h_mm > 0.0) {
        return None;
    }
    let (w, h) = (w_mm * PT_PER_MM, h_mm * PT_PER_MM);
    let ppp = f64::from(cells.clamp(8, 512)) / w.max(h);
    let view = View { width: w, height: h, pixels_per_point: ppp, camera: frame_camera(map, center, scale, 0.0, 0.0), sky: false };
    // Put back when this ends, a panic in the drawing included.
    struct Aside<'a> {
        basemap: rustgis_map::Basemap,
        tiles: Vec<rustgis_map::TileLayer>,
        graticule: bool,
        lock: Option<Vec<(rustgis_map::LabelKey, rustgis_map::LockedPlace)>>,
        anchors: std::collections::HashMap<rustgis_map::LabelKey, Coord>,
        outlines: Vec<u32>,
        map: &'a mut Map,
    }
    impl Drop for Aside<'_> {
        fn drop(&mut self) {
            for l in self.map.layers.iter_mut().filter(|l| self.outlines.contains(&l.id)) {
                l.visible = true;
            }
            self.map.basemap = self.basemap;
            self.map.tiles = std::mem::take(&mut self.tiles);
            self.map.graticule = self.graticule;
            self.map.label_lock = self.lock.take();
            self.map.label_lock_line_anchors = std::mem::take(&mut self.anchors);
        }
    }
    let at = frame_render_scale(map, center, scale);
    let outlines: Vec<u32> = if map.layers.iter().filter(|l| map.draws_at(l, at)).all(only_outlines) {
        Vec::new()
    } else {
        map.layers.iter().filter(|l| l.visible && only_outlines(l)).map(|l| l.id).collect()
    };
    for l in map.layers.iter_mut().filter(|l| outlines.contains(&l.id)) {
        l.visible = false;
    }
    let aside = Aside {
        outlines,
        basemap: std::mem::replace(&mut map.basemap, rustgis_map::Basemap::Blank),
        tiles: std::mem::take(&mut map.tiles),
        graticule: std::mem::replace(&mut map.graticule, false),
        lock: map.label_lock.take(),
        anchors: std::mem::take(&mut map.label_lock_line_anchors),
        map,
    };
    let pm = crate::render(&mut *aside.map, &view)?.pixmap;
    drop(aside);
    let (gw, gh) = (pm.width(), pm.height());
    // Anything not the blank basemap's paper, pure opaque white (the graticule set aside too): a
    // pale or translucent fill counts (only a fill of that very white, invisible on it, doesn't).
    let drawn = pm.pixels().iter().map(|p| p.alpha() > 0 && (p.red(), p.green(), p.blue(), p.alpha()) != (255, 255, 255, 255)).collect();
    Some((gw, gh, drawn))
}

/// A legend's patch: its width and height in pixels (the height set by hand, or the row's own
/// proportion), and the shapes it draws lines and areas in.
struct Patch<'a> {
    w: f32,
    h: f32,
    set: bool,
    look: &'a LegendLook,
}

/// A legend entry's symbol `s` of the layer `l` in the box (`x`, `y`, `w`, `h`): a point as
/// [`point_swatch`] draws it with `strokes` (at the layer's `opacity`, with its reference scale at
/// 1:`scale`), or a line or an area in its `patch` ([`patch_swatch`]), in the middle of the box.
fn swatch(
    pm: &mut Canvas,
    (x, y, w, h): (f32, f32, f32, f32),
    l: &rustgis_map::Layer,
    (s, strokes): (&rustgis_map::Symbol, &[rustgis_map::LineStroke]),
    (opacity, scale): (f32, f64),
    patch: &Patch,
    ppp: f32,
) {
    let gt = l.geometry_type();
    if gt == GeometryType::Point {
        point_swatch(pm, (x, y, w, h), (s, strokes), opacity, ppp * l.size_factor(scale));
        return;
    }
    patch_swatch(pm, (x + (w - patch.w) / 2.0, y, h), gt, s, patch, ppp);
}

/// The symbol `s` of a line or polygon layer (geometry `gt`) in its patch, at `x` in the row
/// `y`..`y + h`.
fn patch_swatch(pm: &mut Canvas, (x, y, h): (f32, f32, f32), gt: GeometryType, s: &rustgis_map::Symbol, patch: &Patch, ppp: f32) {
    let cy = y + h / 2.0;
    let w = patch.w;
    let outline = |pm: &mut Canvas, p: &tiny_skia::Path, min: f32| {
        pm.stroke_path(
            p,
            &paint(s.stroke, 1.0),
            &Stroke { width: s.stroke_width.max(min) * ppp, ..Stroke::default() },
            Transform::identity(),
            None,
        );
    };
    match gt {
        GeometryType::Polyline => {
            // Up and down by a third of the patch's height, as the row's 0.2 always was.
            let half = if patch.set { patch.h / 2.0 } else { h * 0.2 };
            let mut pb = PathBuilder::new();
            match patch.look.line_patch {
                LinePatch::Zigzag => {
                    pb.move_to(x, cy + half);
                    pb.line_to(x + w * 0.4, cy - half);
                    pb.line_to(x + w, cy + half * 0.5);
                }
                LinePatch::Straight => {
                    pb.move_to(x, cy);
                    pb.line_to(x + w, cy);
                }
                LinePatch::Curve => {
                    pb.move_to(x, cy + half * 0.6);
                    pb.cubic_to(x + w * 0.35, cy - half * 1.6, x + w * 0.65, cy + half * 1.6, x + w, cy - half * 0.6);
                }
            }
            if let Some(p) = pb.finish() {
                let stroke = Stroke { width: s.stroke_width.max(1.0) * ppp, line_cap: tiny_skia::LineCap::Round, ..Stroke::default() };
                let stroke = if patch.look.line_patch == LinePatch::Zigzag {
                    Stroke { line_cap: tiny_skia::LineCap::Butt, ..stroke }
                } else {
                    stroke
                };
                pm.stroke_path(&p, &paint(s.stroke, 1.0), &stroke, Transform::identity(), None);
            }
        }
        _ => {
            let bh = if patch.set { patch.h } else { h * 0.6 };
            let Some(b) = Rect::from_xywh(x, cy - bh / 2.0, w, bh) else { return };
            let path = match patch.look.area_patch {
                AreaPatch::Rectangle => {
                    pm.fill_rect(b, &paint(s.fill, 1.0), Transform::identity(), None);
                    outline(pm, &PathBuilder::from_rect(b), 0.5);
                    return;
                }
                AreaPatch::Rounded => rounded_rect(b, w.min(bh) * 0.3).or_else(|| Some(PathBuilder::from_rect(b))),
                AreaPatch::Circle => {
                    let d = w.min(bh);
                    Rect::from_xywh(x + (w - d) / 2.0, cy - d / 2.0, d, d).and_then(PathBuilder::from_oval)
                }
                AreaPatch::Natural => natural_area(b),
            };
            if let Some(p) = path {
                pm.fill_path(&p, &paint(s.fill, 1.0), FillRule::Winding, Transform::identity(), None);
                outline(pm, &p, 0.5);
            }
        }
    }
}

/// A rounded, irregular shape filling `b`, as a lake: a smooth curve through points about an
/// ellipse, nearer and farther from its centre in turn.
fn natural_area(b: Rect) -> Option<tiny_skia::Path> {
    let (cx, cy) = (b.left() + b.width() / 2.0, b.top() + b.height() / 2.0);
    let reach = [1.0, 0.78, 0.95, 0.7, 1.0, 0.85, 0.92, 0.74];
    let pts: Vec<(f32, f32)> = reach
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let a = i as f32 / reach.len() as f32 * std::f32::consts::TAU;
            (cx + a.cos() * b.width() / 2.0 * k, cy + a.sin() * b.height() / 2.0 * k)
        })
        .collect();
    let mid = |a: (f32, f32), c: (f32, f32)| ((a.0 + c.0) / 2.0, (a.1 + c.1) / 2.0);
    let n = pts.len();
    let mut pb = PathBuilder::new();
    let start = mid(pts[n - 1], pts[0]);
    pb.move_to(start.0, start.1);
    for i in 0..n {
        let m = mid(pts[i], pts[(i + 1) % n]);
        pb.quad_to(pts[i].0, pts[i].1, m.0, m.1);
    }
    pb.close();
    pb.finish()
}

/// A point's symbol at (`x`, `y`), unturned, by the path the map takes for it: its marker, or
/// `strokes`' stacked marks.
fn draw_point(pm: &mut Canvas, (x, y): (f32, f32), s: &rustgis_map::Symbol, strokes: &[rustgis_map::LineStroke], ppp: f32) {
    if point_marks::plain(strokes, 0.0) {
        crate::draw_marker(pm, x, y, s, 1.0, None, ppp);
    } else {
        point_marks::draw(pm, x, y, s, strokes, 0.0, None, ppp);
    }
}

/// A point layer's symbol in the middle of the box, as the map draws its points: `strokes`' stacked
/// marks or its marker, at `ppp` (with the layer's reference scale), unturned. See-through, the
/// marks are composited as one, as the map composites the layer, in a picture no larger than the
/// page. So are those too large for the box (past [`LegendRow::symbol_side`]'s limit), which the
/// picture cuts to it.
fn point_swatch(
    pm: &mut Canvas,
    (x, y, w, h): (f32, f32, f32, f32),
    (s, strokes): (&rustgis_map::Symbol, &[rustgis_map::LineStroke]),
    opacity: f32,
    ppp: f32,
) {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    // A thousandth of slack: the box's size went through a rounding of its own.
    let fits = 2.0 * point_marks::reach(s, strokes) * ppp <= w.min(h) * 1.001;
    if opacity >= 0.999 && fits {
        draw_point(pm, (cx, cy), s, strokes, ppp);
        return;
    }
    // Kept on the canvas: a picture as large as the canvas then covers all of it that shows.
    let (left, top) = (x.floor().max(0.0), y.floor().max(0.0));
    let (cw, ch) = (((x + w).ceil() - left) as u32, ((y + h).ceil() - top) as u32);
    let (cw, ch) = (cw.min(pm.width()), ch.min(pm.height()));
    if let Some(mut picture) = pm.sub(cw, ch) {
        draw_point(&mut picture, (cx - left, cy - top), s, strokes, ppp);
        // A recording keeps what lies past its picture's edge, so it is cut here, at the box.
        let clip = Rect::from_xywh(x, y, w, h).map(|r| crate::vector::Clip {
            path: PathBuilder::from_rect(r),
            rule: FillRule::Winding,
            transform: Transform::identity(),
        });
        pm.place(picture, Transform::from_translate(left, top), opacity, rustgis_map::effects::Blend::Normal, clip);
    }
}

/// How a colorbar is laid out.
struct Bar<'a> {
    title: &'a str,
    orientation: Orientation,
    ticks: usize,
    side: TickSide,
    reverse: bool,
    /// Labels with a decimal comma ([`Layout::decimal_comma`]).
    comma: bool,
}

/// `n` values from `lo` to `hi`, the ends included, with the fewest decimals that keep each within
/// a twentieth of the step of its value.
fn tick_labels(lo: f64, hi: f64, n: usize) -> Vec<(f64, String)> {
    if !(lo.is_finite() && hi.is_finite()) {
        return Vec::new();
    }
    let n = if hi > lo { n.clamp(2, rustgis_map::layout::MAX_COLORBAR_TICKS) } else { 1 };
    let step = if n > 1 { (hi - lo) / (n - 1) as f64 } else { 0.0 };
    let values: Vec<f64> = (0..n).map(|i| if i + 1 == n { hi } else { lo + step * i as f64 }).collect();
    if values.iter().any(|v| v.abs() >= 1e6) {
        return values.into_iter().map(|v| (v, rustgis_map::symbology::fmt_num(v))).collect();
    }
    let tol = if step > 0.0 { step * 0.05 } else { (lo.abs() * 1e-3).max(1e-9) };
    let decimals = (0..=6)
        .find(|d| {
            let k = 10f64.powi(*d);
            values.iter().all(|v| ((v * k).round() / k - v).abs() <= tol)
        })
        .unwrap_or(6) as usize;
    // Rounded first, then + 0.0, so a value rounded to zero isn't printed "-0".
    let k = 10f64.powi(decimals as i32);
    values.into_iter().map(|v| (v, format!("{:.*}", decimals, (v * k).round() / k + 0.0))).collect()
}

/// A colorbar: the bar, ticks and labels along it, and its title above.
fn colorbar(pm: &mut Canvas, r: Rect, scale: &ColorScale, bar: &Bar, ppp: f64) {
    let ppp = ppp as f32;
    let dark = Rgba::rgb(0x22, 0x22, 0x22);
    let px = 8.0 * 1.33 * ppp;
    let width = |s: &str| text::measure(s, px / 1.33, false);
    let (mut top, bottom) = (r.y(), r.y() + r.height());
    if !bar.title.is_empty() {
        let tpx = 9.0 * 1.33 * ppp;
        text::text_at(pm, bar.title, r.x(), top + tpx * 0.6, tpx, dark, true, text::Align::Left, false);
        top += tpx * 1.4;
    }
    // Where each block or sample sits along the bar (0 low to 1 high), and the labels with theirs.
    let (blocks, labels): (Vec<Rgba>, Vec<(f32, String)>) = match scale {
        ColorScale::Ramp { lo, hi, colors } => {
            let span = hi - lo;
            let labels = tick_labels(*lo, *hi, bar.ticks)
                .into_iter()
                .map(|(v, s)| ((if span > 0.0 { (v - lo) / span } else { 0.5 }) as f32, s))
                .collect();
            (colors.clone(), labels)
        }
        ColorScale::Steps { edges, colors } => {
            let n = colors.len().max(1) as f32;
            let labels = edges.iter().enumerate().map(|(i, v)| (i as f32 / n, rustgis_map::symbology::fmt_num(*v))).collect();
            (colors.clone(), labels)
        }
        ColorScale::Values(v) => {
            let n = v.len().max(1) as f32;
            let labels = v.iter().enumerate().map(|(i, (val, _))| ((i as f32 + 0.5) / n, rustgis_map::symbology::fmt_num(*val))).collect();
            (v.iter().map(|(_, c)| *c).collect(), labels)
        }
    };
    if blocks.is_empty() || r.width() < 2.0 || bottom - top < 2.0 {
        return;
    }
    let labels: Vec<(f32, String)> = labels.into_iter().map(|(at, s)| (at, decimal(s, bar.comma))).collect();
    let tick = 2.5 * ppp;
    let gap = 1.5 * ppp;
    let vertical = bar.orientation == Orientation::Vertical;
    // The bar's rectangle, leaving room for the labels on their side and for the end labels to
    // stand past the bar's ends.
    let (bx, by, bw, bh) = if vertical {
        let label_w = labels.iter().map(|(_, s)| width(s)).fold(0.0f32, f32::max);
        let bw = (r.width() - label_w - tick - gap).clamp(2.0 * ppp, (r.width() * 0.45).max(2.0 * ppp)).min(r.width());
        let inset = (px / 2.0).min((bottom - top) * 0.25);
        let bx = if bar.side == TickSide::Start { r.x() + r.width() - bw } else { r.x() };
        (bx, top + inset, bw, (bottom - top - 2.0 * inset).max(1.0))
    } else {
        let ends = labels.first().map(|(_, s)| width(s)).unwrap_or(0.0).max(labels.last().map(|(_, s)| width(s)).unwrap_or(0.0));
        let inset = (ends / 2.0).min(r.width() * 0.25);
        // Too short for its labels (left out below), the bar fills the element.
        let bh = match bottom - top - px * 1.2 - tick - gap {
            h if h >= 3.0 * ppp => h,
            _ => bottom - top,
        };
        let by = if bar.side == TickSide::Start { bottom - bh } else { top };
        (r.x() + inset, by, (r.width() - 2.0 * inset).max(1.0), bh)
    };
    let len = if vertical { bh } else { bw };
    // Distance along the bar from its start (left or top) for a position from low to high.
    let flip = if vertical { !bar.reverse } else { bar.reverse };
    let along = |t: f32| if flip { (1.0 - t) * len } else { t * len };
    let n = blocks.len();
    for (i, c) in blocks.iter().enumerate() {
        let (a, b) = (along(i as f32 / n as f32), along((i + 1) as f32 / n as f32));
        let (s0, s1) = (a.min(b), a.max(b));
        // A touch longer than its share, so neighbours leave no seam.
        let rect = if vertical { Rect::from_xywh(bx, by + s0, bw, s1 - s0 + 0.6) } else { Rect::from_xywh(bx + s0, by, s1 - s0 + 0.6, bh) };
        if let Some(rect) = rect.and_then(|q| q.intersect(&Rect::from_xywh(bx, by, bw, bh)?)) {
            pm.fill_rect(rect, &paint(*c, 1.0), Transform::identity(), None);
        }
    }
    let line = Stroke { width: 0.6 * ppp, ..Stroke::default() };
    if let Some(b) = Rect::from_xywh(bx, by, bw, bh) {
        pm.stroke_path(&PathBuilder::from_rect(b), &paint(dark, 1.0), &line, Transform::identity(), None);
    }
    // A label (and its tick) is left out where it would spill out of a short or narrow element.
    let inside =
        |x: f32, y: f32, w: f32, h: f32| x >= r.x() - 1.0 && y >= r.y() - 1.0 && x + w <= r.right() + 1.0 && y + h <= r.bottom() + 1.0;
    // Labels in order along the bar, each drawn only if it clears the one before.
    let mut last_end = f32::NEG_INFINITY;
    let mut ordered: Vec<(f32, &String)> = labels.iter().map(|(t, s)| (along(t.clamp(0.0, 1.0)), s)).collect();
    ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (d, label) in ordered {
        let extent = if vertical { px } else { width(label) };
        if d - extent / 2.0 < last_end + gap {
            continue;
        }
        last_end = d + extent / 2.0;
        let mut pb = PathBuilder::new();
        if vertical {
            let (y, x0) = (by + d, if bar.side == TickSide::Start { bx } else { bx + bw });
            let x1 = if bar.side == TickSide::Start { x0 - tick } else { x0 + tick };
            pb.move_to(x0, y);
            pb.line_to(x1, y);
            let lx = if bar.side == TickSide::Start { x1 - gap - width(label) } else { x1 + gap };
            if !inside(lx, y - px / 2.0, width(label), px) {
                continue;
            }
            text::text_at(pm, label, lx, y, px, dark, false, text::Align::Left, false);
        } else {
            let (x, y0) = (bx + d, if bar.side == TickSide::Start { by } else { by + bh });
            let y1 = if bar.side == TickSide::Start { y0 - tick } else { y0 + tick };
            pb.move_to(x, y0);
            pb.line_to(x, y1);
            let ly = if bar.side == TickSide::Start { y1 - gap - px * 0.5 } else { y1 + gap + px * 0.5 };
            if !inside(x - width(label) / 2.0, ly - px / 2.0, width(label), px) {
                continue;
            }
            text::text_at(pm, label, x, ly, px, dark, false, text::Align::Center, false);
        }
        if let Some(p) = pb.finish() {
            pm.stroke_path(&p, &paint(dark, 1.0), &line, Transform::identity(), None);
        }
    }
}

/// How a text element's lines are drawn, beyond their size and colour.
#[derive(Clone, Copy)]
struct TextStyle {
    /// Bold, italic and the family.
    face: text::Face,
    align: TextAlign,
    /// Letter spacing in points.
    spacing: f32,
}

/// The distance between two lines of text at `size` points, in mm, as [`text_block`] sets them.
pub fn line_pitch_mm(size: f64) -> f64 {
    size.clamp(2.0, 400.0) * 1.33 * 1.25 * 25.4 / 96.0
}

/// The face a text element is drawn in, for [`wrap_text`]: as the element's own fields.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextFace<'a> {
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    /// An installed family by name, drawn in the sans or serif where it isn't installed.
    pub font_name: Option<&'a str>,
    /// In points.
    pub letter_spacing: f64,
}

/// `s` with each line wider than a text element `width_mm` wide broken between its words, as
/// the element draws it at `size` points in `face`: at spaces and dashes, and between the words
/// of Thai and the other scripts written without spaces ([`text::wrap_by`]), into lines of about
/// the same length. Lines the text has stay lines, a line that fits is as it was, and a word
/// wider than the element keeps a line of its own. Dynamic text is measured as it is written,
/// not as it is filled in.
pub fn wrap_text(s: &str, width_mm: f64, size: f64, face: TextFace<'_>) -> String {
    let family = face.font_name.and_then(crate::families::find_ready);
    let drawn = text::Face { bold: face.bold, italic: face.italic, serif: face.serif, family };
    let px = size.clamp(2.0, 400.0) as f32 * 1.33;
    let width = (width_mm * rustgis_map::layout::PT_PER_MM) as f32;
    wrapped_lines(s, width, px, drawn, face.letter_spacing as f32 * 1.33).join("\n")
}

/// [`wrap_text`]'s lines: `width`, the size `px` and `spacing` in the same units. Every line and
/// every word of the text is in them, however long it is: the text is kept as it is broken, so
/// the most lines and characters a page draws don't bound it.
fn wrapped_lines(s: &str, width: f32, px: f32, face: text::Face, spacing: f32) -> Vec<String> {
    let lines = s.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line));
    if !(width.is_finite() && width > 0.0) {
        return lines.map(str::to_string).collect();
    }
    let ink = |t: &str| {
        let (left, right) = text::styled_extent(t, px, face, spacing);
        (right - left).max(0.0)
    };
    lines.flat_map(|line| text::wrap_whole_by(line, width, ink)).collect()
}

/// Text in an element: lines split at `\n` from the top, each placed by the style's alignment. A
/// centred or right-aligned line wider than the element runs out past its left edge, as a
/// left-aligned one runs past its right.
fn text_block(pm: &mut Canvas, r: Rect, s: &str, size: f32, color: Rgba, style: TextStyle, ppp: f64) {
    let px = size.clamp(2.0, 400.0) * 1.33 * ppp as f32;
    // Letter spacing is in points, as the size is.
    let spacing = style.spacing * 1.33 * ppp as f32;
    let mut y = r.y() + px * 0.6;
    for line in s.lines().take(200) {
        if y > r.y() + r.height() + px {
            break;
        }
        // Centred and right-aligned lines are placed by the ink they draw (italic overhang and a
        // negative spacing included), which an advance doesn't measure; left-aligned ones start
        // at the edge as plain text always has.
        let left = match style.align {
            TextAlign::Left => r.x(),
            aligned => {
                let (ink_left, ink_right) = text::styled_extent(line, px, style.face, spacing);
                let free = r.width() - (ink_right - ink_left);
                r.x() + if aligned == TextAlign::Center { free / 2.0 } else { free } - ink_left
            }
        };
        text::styled_at(pm, line, left, y, px, color, style.face, spacing);
        y += px * 1.25;
    }
}

/// The first `rows` rows of a table, with as many columns as fit.
fn table(pm: &mut Canvas, r: Rect, ds: &rustgis_data::Dataset, rows: usize, ppp: f64) {
    let ppp = ppp as f32;
    let px = 8.0 * 1.33 * ppp;
    let row_h = px * 1.6;
    let col_w = (90.0 * ppp).min(r.width());
    let cols = ((r.width() / col_w).floor() as usize).clamp(1, ds.fields.len().max(1));
    let n = rows.min(ds.features.len()).min(((r.height() / row_h) as usize).saturating_sub(1));
    let dark = Rgba::rgb(0x22, 0x22, 0x22);
    if let Some(bg) = Rect::from_xywh(r.x(), r.y(), col_w * cols as f32, row_h * (n + 1) as f32) {
        pm.fill_rect(bg, &paint(Rgba::rgb(0xFF, 0xFF, 0xFF), 1.0), Transform::identity(), None);
    }
    if let Some(hdr) = Rect::from_xywh(r.x(), r.y(), col_w * cols as f32, row_h) {
        pm.fill_rect(hdr, &paint(Rgba::rgb(0xE4, 0xE8, 0xEE), 1.0), Transform::identity(), None);
    }
    let fit = |s: &str| -> String {
        let mut t: String = s.chars().take(40).collect();
        while !t.is_empty() && text::measure(&t, 8.0, false) * ppp > col_w - 6.0 * ppp {
            t.pop();
        }
        t
    };
    for (c, f) in ds.fields.iter().take(cols).enumerate() {
        text::text_at(
            pm,
            &fit(f.display()),
            r.x() + c as f32 * col_w + 3.0 * ppp,
            r.y() + row_h / 2.0,
            px,
            dark,
            true,
            text::Align::Left,
            false,
        );
    }
    for row in 0..n {
        let y = r.y() + row_h * (row + 1) as f32;
        if row % 2 == 1
            && let Some(b) = Rect::from_xywh(r.x(), y, col_w * cols as f32, row_h)
        {
            pm.fill_rect(b, &paint(Rgba::rgb(0xF5, 0xF6, 0xF8), 1.0), Transform::identity(), None);
        }
        for c in 0..cols {
            text::text_at(
                pm,
                &fit(&ds.value(row, c).to_string()),
                r.x() + c as f32 * col_w + 3.0 * ppp,
                y + row_h / 2.0,
                px,
                dark,
                false,
                text::Align::Left,
                false,
            );
        }
    }
}

/// A bar chart of a field's values, largest first (up to 20 bars).
fn chart(pm: &mut Canvas, r: Rect, ds: &rustgis_data::Dataset, field: &str, label: Option<&str>, ppp: f64) {
    let ppp = ppp as f32;
    let Some(fi) = ds.field_index(field) else {
        placeholder(pm, r, "Chart", ppp as f64);
        return;
    };
    let li = label.and_then(|l| ds.field_index(l));
    let mut vals: Vec<(f64, String)> = (0..ds.features.len())
        .filter_map(|row| {
            ds.value(row, fi).as_f64().filter(|v| v.is_finite()).map(|v| (v, li.map(|i| ds.value(row, i).to_string()).unwrap_or_default()))
        })
        .collect();
    vals.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    vals.truncate(20);
    let max = vals.iter().map(|v| v.0).fold(0.0_f64, f64::max);
    if vals.is_empty() || max <= 0.0 {
        placeholder(pm, r, "Chart", ppp as f64);
        return;
    }
    let dark = Rgba::rgb(0x22, 0x22, 0x22);
    let px = 7.5 * 1.33 * ppp;
    let title_h = px * 1.6;
    text::text_at(pm, field, r.x(), r.y() + title_h / 2.0, px * 1.15, dark, true, text::Align::Left, false);
    let label_w = if li.is_some() { r.width() * 0.32 } else { 0.0 };
    let bar_area = r.width() - label_w - 4.0;
    let bh = ((r.height() - title_h) / vals.len() as f32).min(18.0 * ppp);
    for (i, (v, name)) in vals.iter().enumerate() {
        let y = r.y() + title_h + bh * i as f32;
        let w = (bar_area * (*v / max) as f32).max(1.0);
        if let Some(b) = Rect::from_xywh(r.x() + label_w, y + bh * 0.15, w, bh * 0.7) {
            pm.fill_rect(b, &paint(Rgba::rgb(0x3B, 0x82, 0xC4), 1.0), Transform::identity(), None);
        }
        if li.is_some() {
            let mut t: String = name.chars().take(30).collect();
            while !t.is_empty() && text::measure(&t, 7.5, false) * ppp > label_w - 4.0 * ppp {
                t.pop();
            }
            text::text_at(pm, &t, r.x(), y + bh / 2.0, px.min(bh * 0.9), dark, false, text::Align::Left, false);
        }
    }
}

/// The first case-insensitive frame-map name match; a missing reference renders no map.
pub fn map_index(maps: &[rustgis_map::Map], name: &str) -> Option<usize> {
    maps.iter().position(|map| map.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A legend's look as it always was.
    fn plain() -> &'static LegendLook {
        static PLAIN: std::sync::OnceLock<LegendLook> = std::sync::OnceLock::new();
        PLAIN.get_or_init(LegendLook::default)
    }

    /// A page with a frame of the built-in world and a title.
    fn vector_page() -> (Layout, Vec<Map>) {
        let mut layout = Layout::new("Vectors", 120.0, 90.0);
        let kind = serde_json::from_value(serde_json::json!({"MapFrame": {
            "map": "Map", "center": {"x": 0, "y": 0}, "scale": 150000000
        }}))
        .unwrap();
        layout.add("World", (10.0, 20.0, 100.0, 60.0), kind);
        let title = ElementKind::text("Shelby County", 18.0, Rgba::rgb(20, 20, 20), true);
        layout.add("Title", (10.0, 4.0, 100.0, 12.0), title);
        (layout, vec![Map::new("Map")])
    }

    #[test]
    fn a_recorded_page_keeps_its_text_and_frame_as_vectors() {
        use crate::vector::Item;
        let (layout, mut maps) = vector_page();
        let (drawing, pictured) = render_drawing(&layout, &mut maps, 150.0, &PageInfo::default()).unwrap();
        assert!(pictured.is_empty() && !drawing.overflowed());
        let (_, _, items) = drawing.finish();
        let text: String = items
            .iter()
            .filter_map(|i| if let Item::Glyphs(r) = i { Some(r) } else { None })
            .flat_map(|r| r.glyphs.iter().map(|g| g.ch))
            .collect();
        assert_eq!(text, "Shelby County", "the title, its space kept for copying");
        // The frame: a group cut to its box, its land as paths, not a picture of it.
        let frame = items.iter().find_map(|i| if let Item::Group(g) = i { Some(g) } else { None }).expect("the frame's group");
        assert!(frame.clip.is_some());
        assert!(frame.items.iter().filter(|i| matches!(i, Item::Fill { .. })).count() > 50);
        assert!(!frame.items.iter().any(|i| matches!(i, Item::Image(_))));
    }

    /// Wrapping a text element's text breaks the lines wider than the element between words
    /// (Thai ones too, in a face that draws them), keeping the lines the text has.
    #[test]
    fn wrapping_text_breaks_its_long_lines_between_words() {
        let face = text::Face { family: crate::families::find("Sarabun"), ..text::Face::default() };
        let title = "แผนที่แสดงพื้นที่เสี่ยงภัยน้ำท่วมในเขตอำเภอเมืองเชียงใหม่";
        let (px, width) = (16.0, 200.0);
        let ink = |t: &str| {
            let (l, r) = text::styled_extent(t, px, face, 0.0);
            r - l
        };
        assert!(ink(title) > width, "the title is wider than the element: {}", ink(title));
        let lines = wrapped_lines(&format!("{title}\nShort"), width, px, face, 0.0);
        assert!(lines.len() > 2 && lines.last().is_some_and(|l| l == "Short"), "{lines:?}");
        assert!(lines.iter().all(|l| ink(l) <= width), "{lines:?}");
        assert_eq!(lines[..lines.len() - 1].concat(), title, "nothing is added or lost where Thai breaks");
        let english = "The quick brown fox jumps over the lazy dog";
        let lines = wrapped_lines(english, 120.0, px, face, 0.0);
        assert!(lines.len() > 1 && lines.join(" ") == english, "{lines:?}");
        // A width that is none wraps nothing; the text is as the element 50 mm wide draws it.
        assert_eq!(wrapped_lines("a b", f32::NAN, px, face, 0.0), ["a b"]);
        let sarabun = TextFace { font_name: Some("Sarabun"), ..TextFace::default() };
        let wrapped = wrap_text(&format!("{title}\nShort"), 50.0, 12.0, sarabun);
        assert!(
            wrapped.lines().count() > 2 && wrapped.ends_with("\nShort") && wrapped.replace('\n', "") == format!("{title}Short"),
            "{wrapped}"
        );
        assert_eq!(wrap_text("Short", 50.0, 12.0, sarabun), "Short");
        // Wrapped again, text that fits is as it was.
        assert_eq!(wrap_text(&wrapped, 50.0, 12.0, sarabun), wrapped);
        // The text is kept as it is broken, so all of it is: a line longer than a label's 2000
        // characters, more lines than a page draws, and the empty lines it ends with.
        let long = format!("{}END", "word ".repeat(600));
        let lines = wrapped_lines(&long, 120.0, px, face, 0.0);
        assert!(lines.len() > 200 && lines.join(" ") == long, "{} lines", lines.len());
        let many = "a\n".repeat(300);
        assert_eq!(wrap_text(&many, 50.0, 12.0, sarabun), many);
        assert_eq!(wrap_text("a b\r\nc\n\n", 50.0, 12.0, sarabun), "a b\nc\n\n");
    }

    #[test]
    fn a_recorded_page_draws_what_the_picture_of_it_shows() {
        let (layout, mut maps) = vector_page();
        let info = PageInfo::default();
        let png = render(&layout, &mut maps, 96.0, &info).unwrap();
        let (drawing, _) = render_drawing(&layout, &mut maps, 96.0, &info).unwrap();
        let (_, _, items) = drawing.finish();
        let mut replayed = Pixmap::new(png.width(), png.height()).unwrap();
        crate::vector::replay(&items, &mut replayed, Transform::identity());
        // The same paths through the same rasteriser: only glyph edges (coverage against filled
        // outlines) may differ.
        let differ = png.pixels().iter().zip(replayed.pixels()).filter(|(a, b)| {
            let (a, b) = (a.demultiply(), b.demultiply());
            a.red().abs_diff(b.red()).max(a.green().abs_diff(b.green())).max(a.blue().abs_diff(b.blue())) > 48
        });
        let share = differ.count() as f64 / png.pixels().len() as f64;
        assert!(share < 0.005, "{:.3}% of pixels differ", share * 100.0);
    }

    #[test]
    fn a_huge_page_records_within_its_bounds() {
        let mut layout = Layout::new("Huge", 5000.0, 5000.0);
        layout.add("Title", (10.0, 10.0, 4000.0, 400.0), ElementKind::text("Shelby County", 300.0, Rgba::rgb(0, 0, 0), true));
        let (drawing, _) = render_drawing(&layout, &mut [], 600.0, &PageInfo::default()).unwrap();
        // Held to the page budget's size, as the picture of it is.
        assert!(drawing.width as f64 <= MAX_SIDE && drawing.height as f64 <= MAX_SIDE);
        assert!(!drawing.overflowed());
    }

    #[test]
    fn a_turned_frame_stays_vectors() {
        use crate::vector::Item;
        let (mut layout, mut maps) = vector_page();
        if let Some(ElementKind::MapFrame { bearing, .. }) = layout.elements.first_mut().map(|e| &mut e.kind) {
            *bearing = 30.0;
        }
        let (drawing, _) = render_drawing(&layout, &mut maps, 96.0, &PageInfo::default()).unwrap();
        let (_, _, items) = drawing.finish();
        let frame = items.iter().find_map(|i| if let Item::Group(g) = i { Some(g) } else { None }).expect("the frame's group");
        // The turned view holds the north-up frame, turned.
        let Some(Item::Group(north)) = frame.items.first() else { panic!("the north-up frame inside") };
        assert!(north.transform.ky.abs() > 0.3, "turned: {:?}", north.transform);
        assert!(north.items.iter().filter(|i| matches!(i, Item::Fill { .. })).count() > 50);
    }

    #[test]
    fn a_frame_too_large_for_vectors_is_drawn_as_a_picture_and_named() {
        use crate::vector::{Item, TEST_MAX_COST};
        let (layout, mut maps) = vector_page();
        TEST_MAX_COST.with(|l| l.set(Some(20_000)));
        let result = render_drawing(&layout, &mut maps, 72.0, &PageInfo::default());
        TEST_MAX_COST.with(|l| l.set(None));
        let (drawing, pictured) = result.unwrap();
        assert_eq!(pictured.iter().map(|p| p.1.as_str()).collect::<Vec<_>>(), ["World"]);
        let (_, _, items) = drawing.finish();
        assert!(items.iter().any(|i| matches!(i, Item::Image(_))), "the frame as a picture");
    }

    /// Every token the Format pane offers a layout's text is one the page expands (#516).
    #[test]
    fn the_tokens_offered_are_the_ones_expanded() {
        let layout = Layout::new("Atlas", 100.0, 100.0);
        let info = PageInfo { page: 2, pages: 5, page_name: "North".into() };
        let tokens = rustgis_map::layout::PAGE_TOKENS.iter().chain(rustgis_map::graphics::TOKENS);
        for (token, _) in tokens {
            assert!(!expand(token, &layout, &info, None).contains('{'), "{token}");
        }
        assert_eq!(expand("{page} of {pages}, {pageName} · {layout}", &layout, &info, None), "2 of 5, North · Atlas");
    }

    /// A WMS or WMTS layer's legend is its service's picture under its name, as the Layers pane
    /// shows it, narrowed to fit across and sized for; left out by name, hidden, or with none at
    /// the frame's scale, it isn't listed.
    #[test]
    fn a_web_layers_legend_is_its_services_picture() {
        crate::tiles::tests::serve();
        let mut map = Map::new("m");
        let mut t = rustgis_map::TileLayer::new("Walls", rustgis_map::TileKind::Raster, "https://tiles.test/red/{z}/{x}/{y}.png");
        t.legends = vec![rustgis_data::ogc::Legend { url: "https://tiles.test/red/legend.png".into(), ..Default::default() }];
        map.tiles.push(t);
        // The red drawn (the picture is red; the name is dark) and how far right, in pixels.
        let red = |map: &Map, width: f32, exclude: &[String], scale: f64| {
            let mut pm = Pixmap::new(300, 400).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let opts = LegendOptions { hide_other: false, exclude, scale, look: plain() };
            crate::canvas::with_canvas(&mut pm, |c| {
                legend(c, Rect::from_xywh(0.0, 0.0, width, 400.0).unwrap(), map, ("", 1), None, opts, 1.0)
            });
            let hits: Vec<u32> = (0..pm.height())
                .flat_map(|y| (0..pm.width()).map(move |x| (x, y)))
                .filter(|&(x, y)| pm.pixel(x, y).is_some_and(|p| p.red() > 200 && p.green() < 60))
                .map(|(x, _)| x)
                .collect();
            (hits.len(), hits.iter().max().copied().unwrap_or(0))
        };
        wait_for_tile_legends(&map, 1e6, &[]);
        let (ink, right) = red(&map, 300.0, &[], 1e6);
        assert!(ink > 200 * 200, "the picture is drawn: {ink} px");
        assert!(right < 6 + 256, "a pixel a point: {right}");
        // Its name's row and the picture, 256 points down and 6 + 256 across.
        let (w, h) = legend_sizes_mm(&mut map, 1e6, false, false, &[], plain()).unwrap()[0];
        let mm = 25.4 / 96.0;
        assert!((w - 262.0 * mm).abs() < 1e-6 && (h - (f64::from(LEGEND_ROW) + 256.0) * mm).abs() < 1e-6, "{w} × {h} mm");
        let (_, right) = red(&map, 100.0, &[], 1e6);
        assert!(right < 100, "narrowed to fit: {right}");
        assert_eq!(red(&map, 300.0, &["WALLS".into()], 1e6).0, 0, "left out by name");
        map.tiles[0].legends[0].max_scale = Some(1e5);
        assert_eq!(red(&map, 300.0, &[], 1e6).0, 0, "none at this scale");
        map.tiles[0].legends[0].max_scale = None;
        // Narrowed, a picture takes only the height it's drawn at: a second layer's fits under it
        // (a 70-point legend: 64 points across, so 64 down), and the drawing order is the map's.
        let mut second = map.tiles[0].clone();
        second.name = "Floors".into();
        map.tiles.push(second);
        let mut pm = Pixmap::new(70, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() };
        crate::canvas::with_canvas(&mut pm, |c| legend(c, Rect::from_xywh(0.0, 0.0, 70.0, 200.0).unwrap(), &map, ("", 1), None, opts, 1.0));
        let red_rows =
            (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| pm.pixel(x, y).is_some_and(|p| p.red() > 200 && p.green() < 60))).count();
        assert!(red_rows >= 2 * 63, "both pictures, {red_rows} rows");
        let names: Vec<&str> = legend_rows(&map, None, &opts)
            .iter()
            .filter_map(|r| match r {
                LegendRow::Head(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["Walls", "Floors"]);
        // Among the map's layers where the map draws them: a web layer placed over a feature
        // layer is listed just above it, under the feature layers above.
        let lower = map.add_dataset(rustgis_data::Dataset::new("Lower", GeometryType::Point, vec![]));
        map.add_dataset(rustgis_data::Dataset::new("Upper", GeometryType::Point, vec![]));
        map.tiles[1].over = Some(lower);
        let listed: Vec<&str> = legend_rows(&map, None, &opts)
            .iter()
            .filter_map(|r| match r {
                LegendRow::Head(n) => Some(*n),
                LegendRow::Plain(l) => Some(l.name.as_str()),
                _ => None,
            })
            .collect();
        let feature_order: Vec<&str> = map.layers.iter().map(|l| l.name.as_str()).collect();
        let expected: Vec<&str> =
            feature_order.iter().flat_map(|n| if *n == "Lower" { vec!["Floors", "Lower"] } else { vec![*n] }).chain(["Walls"]).collect();
        assert_eq!(listed, expected, "layers top first: {feature_order:?}");
        // A heading with no room for any of its picture under it isn't drawn: a legend one row
        // tall stays blank (the map's layers set aside, whose rows would fit).
        map.layers.clear();
        let mut pm = Pixmap::new(300, 15).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(0.0, 0.0, 300.0, LEGEND_ROW).unwrap(), &map, ("", 1), None, opts, 1.0)
        });
        assert!(pm.pixels().iter().all(|p| p.red() > 200 && p.green() > 200), "no heading alone");
        map.tiles.truncate(1);
        map.tiles[0].visible = false;
        assert_eq!(red(&map, 300.0, &[], 1e6).0, 0, "hidden");
    }

    /// A map of a point layer classed a, b and c, a line layer and a polygon layer, the last
    /// two in one symbol each.
    fn legend_map() -> Map {
        use rustgis_data::{Dataset, Field, FieldType};
        let mut map = Map::new("m");
        let pts = map.add_dataset(Dataset::new("Towns", GeometryType::Point, vec![Field::new("g", FieldType::Text)]));
        map.add_dataset(Dataset::new("Roads", GeometryType::Polyline, vec![]));
        map.add_dataset(Dataset::new("Lakes", GeometryType::Polygon, vec![]));
        let s = |c| rustgis_map::Symbol::default_for(GeometryType::Point, c);
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
            let classes = ["a", "b", "c"].iter().zip(0u32..).map(|(k, i)| (k.to_string(), s(i))).collect();
            l.renderer = rustgis_map::Renderer::Unique {
                field: "g".into(),
                classes,
                other: s(3),
                ramp: rustgis_map::Ramp::Ember,
                class_labels: Default::default(),
                other_label: None,
                class_strokes: Default::default(),
                other_strokes: Vec::new(),
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: None,
            };
        }
        map
    }

    /// Draws the legend of `map` with every class in `look`, titled.
    fn legend_with(map: &Map, look: &LegendLook) -> Pixmap {
        let mut pm = Pixmap::new(400, 400).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { hide_other: true, exclude: &[], scale: 1e6, look };
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(0.0, 0.0, 400.0, 400.0).unwrap(), map, ("Legend", 1), None, opts, 1.0)
        });
        pm
    }

    /// How far right and down a legend draws, in pixels.
    fn ink_reach(pm: &Pixmap) -> (u32, u32) {
        let inked = |x, y| pm.pixel(x, y).is_some_and(|p| p.red() < 200 || p.green() < 200 || p.blue() < 200);
        let right = (0..pm.width()).filter(|&x| (0..pm.height()).any(|y| inked(x, y))).max().unwrap_or(0);
        let down = (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| inked(x, y))).max().unwrap_or(0);
        (right, down)
    }

    /// A legend's look sets its type, its patches, the room between its rows and the texts it
    /// shows; the default draws it as it always was.
    #[test]
    fn a_legends_look_sets_its_type_patches_spacing_and_texts() {
        let mut map = legend_map();
        let size = |map: &mut Map, look: &LegendLook| legend_sizes_mm(map, 1e6, true, true, &[], look).unwrap()[0];
        let plain_pm = legend_with(&map, plain());
        let (w0, h0) = size(&mut map, plain());
        // Towns and its three classes, Roads, Lakes: six rows of the height they always had.
        let mm = 25.4 / 96.0;
        assert!((h0 - 6.0 * f64::from(LEGEND_ROW) * mm).abs() < 1e-6, "{h0} mm");
        // Larger type: taller and wider, and drawn so.
        let mut big = LegendLook::default();
        big.label.size = 16.0;
        big.layer.size = 14.0;
        let (w, h) = size(&mut map, &big);
        assert!(w > w0 && h > h0 + 5.0, "{w0} × {h0} to {w} × {h}");
        assert!(ink_reach(&legend_with(&map, &big)).1 > ink_reach(&plain_pm).1 + 20);
        // Wider patches push the text right by as much; room between the rows adds to the height.
        // (Within a ten-thousandth of a mm: the widths are summed in f32.)
        let wide = LegendLook { patch_width: 40.0, spacing: 4.0, ..LegendLook::default() };
        let (w, h) = size(&mut map, &wide);
        assert!((w - w0 - 24.0 * mm).abs() < 1e-4, "{w0} to {w}");
        assert!((h - h0 - 5.0 * 4.0 * mm).abs() < 1e-4, "five gaps: {h0} to {h}");
        // A taller patch makes its rows taller.
        let tall = LegendLook { patch_height: 20.0, ..LegendLook::default() };
        assert!(size(&mut map, &tall).1 > h0 + 10.0 * 4.0 * mm);
        // Texts set by hand: a layer's name and a class's label, any case of the layer's name.
        let mut named = LegendLook::default();
        named.set_name("roads", "Roads and highways of the whole region");
        named.set_label("TOWNS", "b", "A rather long label for class b");
        let (w, _) = size(&mut map, &named);
        assert!(w > w0 + 20.0, "{w0} to {w}");
        assert!(ink_reach(&legend_with(&map, &named)).0 > ink_reach(&plain_pm).0 + 50);
        // The title's colour and the labels' slant and family.
        let mut red = LegendLook::default();
        red.title.color = Some(Rgba::rgb(0xE0, 0x10, 0x10));
        let pm = legend_with(&map, &red);
        assert!(pm.pixels().iter().any(|p| p.red() > 200 && p.green() < 60), "a red title");
        let mut serif = LegendLook::default();
        serif.label.font = FontFamily::Serif;
        serif.label.italic = true;
        assert_ne!(legend_with(&map, &serif).data(), plain_pm.data());
        // Each line and area shape draws its own patch.
        let mut seen: Vec<Vec<u8>> = vec![plain_pm.data().to_vec()];
        for line_patch in [LinePatch::Straight, LinePatch::Curve] {
            seen.push(legend_with(&map, &LegendLook { line_patch, ..LegendLook::default() }).data().to_vec());
        }
        for area_patch in [AreaPatch::Rounded, AreaPatch::Circle, AreaPatch::Natural] {
            seen.push(legend_with(&map, &LegendLook { area_patch, ..LegendLook::default() }).data().to_vec());
        }
        for (i, a) in seen.iter().enumerate() {
            assert!(seen[i + 1..].iter().all(|b| a != b), "patch {i} looks like another");
        }
    }

    /// A layer's name or a class's label set in a type of its own: that row only takes it, the
    /// others keep the legend's.
    #[test]
    fn each_layer_and_class_takes_a_type_of_its_own() {
        let mut map = legend_map();
        let size = |map: &mut Map, look: &LegendLook| legend_sizes_mm(map, 1e6, true, true, &[], look).unwrap()[0];
        let (w0, h0) = size(&mut map, plain());
        let mm = 25.4 / 96.0;
        // Towns' name at 20 points: its row alone grows, to a 20-point row.
        let mut big = LegendLook::default();
        big.set_name_type("towns", Some(LabelText { size: 20.0, ..LabelText::arrow() }));
        let (w, h) = size(&mut map, &big);
        assert!((h - h0 - f64::from(text_row(20.0) - LEGEND_ROW) * mm).abs() < 1e-4, "{h0} to {h}");
        assert!(w > w0, "the larger name is wider: {w0} to {w}");
        // Class b in red: red ink, and no size change.
        let mut red = LegendLook::default();
        red.set_label_type("TOWNS", "b", Some(LabelText { color: Some(Rgba::rgb(0xE0, 0x10, 0x10)), ..LabelText::scale_bar() }));
        assert_eq!(size(&mut map, &red), (w0, h0));
        let pm = legend_with(&map, &red);
        let reds: Vec<u32> = (0..pm.height())
            .filter(|&y| (0..pm.width()).any(|x| pm.pixel(x, y).is_some_and(|p| p.red() > 200 && p.green() < 60 && p.blue() < 60)))
            .collect();
        // Only on class b's row: the title's, then Lakes, Roads, Towns and a (the map's order, top
        // first; each 15 points).
        let row_b = (title_row(plain()) + 4.0 * LEGEND_ROW) as u32;
        assert!(!reds.is_empty() && reds.iter().all(|y| (row_b..row_b + LEGEND_ROW as u32).contains(y)), "{reds:?}");
        // Cleared, the legend's own type again.
        red.set_label_type("towns", "b", None);
        big.set_name_type("Towns", None);
        assert!(red.is_default() && big.is_default());
    }

    /// A legend turned into pieces lists what it draws, where it draws it: the title, each name and
    /// label in its type, and each patch in the box its row draws it in.
    #[test]
    fn a_legend_comes_apart_into_what_it_draws() {
        let mut map = legend_map();
        let mut look = LegendLook::default();
        look.set_label_type("Towns", "a", Some(LabelText { size: 11.0, bold: true, ..LabelText::scale_bar() }));
        let (w, h) = legend_sizes_mm(&mut map, 1e6, true, true, &[], &look).unwrap()[0];
        let title_mm = f64::from(title_row(&look)) * 25.4 / 96.0;
        let pieces = legend_pieces_mm(&mut map, 1e6, ("Legend", 1), true, true, &[], &look, (w + 5.0, h + title_mm + 1.0)).unwrap();
        let texts: Vec<(&str, f64)> = pieces
            .iter()
            .filter_map(|p| match p {
                LegendPiece::Text { text, size, .. } => Some((text.as_str(), *size)),
                _ => None,
            })
            .collect();
        assert_eq!(texts, [("Legend", 12.0), ("Lakes", 9.0), ("Roads", 9.0), ("Towns", 9.0), ("a", 11.0), ("b", 8.5), ("c", 8.5)]);
        let patches: Vec<GeometryType> = pieces
            .iter()
            .filter_map(|p| match p {
                LegendPiece::Patch { geometry, .. } => Some(*geometry),
                _ => None,
            })
            .collect();
        assert_eq!(patches, [GeometryType::Polygon, GeometryType::Polyline, GeometryType::Point, GeometryType::Point, GeometryType::Point]);
        // Top to bottom, inside the legend; a class's label right of its patch.
        let mut last = 0.0;
        for p in &pieces {
            let (x, y) = match p {
                LegendPiece::Text { x, y, .. } => (*x, *y),
                LegendPiece::Patch { x, y, w: pw, h: ph, .. } => {
                    assert!(*pw > 0.0 && *ph > 0.0 && x + pw <= w + 5.0, "{p:?}");
                    (*x, y + ph / 2.0)
                }
                LegendPiece::Picture { .. } => panic!("no pictures here: {p:?}"),
            };
            assert!(x >= 0.0 && y >= last - 1e-4, "{p:?} after {last}");
            last = y;
        }
        // Too short for all its rows: only those that fit.
        let short = legend_pieces_mm(&mut map, 1e6, ("Legend", 1), true, true, &[], &look, (w + 5.0, title_mm + 5.0)).unwrap();
        assert!(short.len() < pieces.len() && !short.is_empty());
    }

    /// A legend's patch on its own fills its box with the symbol, in the shape it was given.
    #[test]
    fn a_lone_patch_draws_its_symbol_in_its_box() {
        let mut l = Layout::new("Layout", 100.0, 50.0);
        let red = Rgba::rgb(255, 0, 0);
        let symbol = rustgis_map::Symbol { fill: red, stroke: red, stroke_width: 1.0, size: 7.0, marker: rustgis_map::Marker::Circle };
        let area = |area_patch| ElementKind::Swatch { geometry: GeometryType::Polygon, symbol, line_patch: LinePatch::Zigzag, area_patch };
        l.add("Patch", (10.0, 10.0, 30.0, 20.0), area(AreaPatch::Rectangle));
        l.add("Round", (60.0, 10.0, 30.0, 20.0), area(AreaPatch::Circle));
        let pm = render(&l, &mut [], 254.0, &PageInfo::default()).unwrap();
        let is_red = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|p| p.red() > 200 && p.green() < 60);
        // Ten pixels a millimetre: the rectangle fills its box to the corner; the circle doesn't.
        assert!(is_red(250, 200) && is_red(105, 105), "rectangle");
        assert!(is_red(750, 200) && !is_red(605, 105), "circle");
        assert!(!is_red(500, 200), "nothing between");
        // A file's sizes are kept in range.
        let mut bad = l.clone();
        if let ElementKind::Swatch { symbol, .. } = &mut bad.elements[0].kind {
            symbol.size = f32::NAN;
            symbol.stroke_width = 1e9;
        }
        bad.clamp();
        let ElementKind::Swatch { symbol, .. } = &bad.elements[0].kind else { panic!("{:?}", bad.elements[0].kind) };
        assert_eq!((symbol.size, symbol.stroke_width), (0.0, rustgis_map::symbol_by::MAX_WIDTH));
    }

    /// A layer that only outlines its polygons isn't counted as drawn, unless nothing else is shown,
    /// and it is shown again after; one with labels is more than an outline.
    #[test]
    fn frame_coverage_leaves_out_layers_that_only_outline() {
        use rustgis_data::{Dataset, Geometry};
        let count = |map: &mut Map| {
            let (.., cells) = frame_coverage(map, Coord::new(0.0, 0.0), 1_000_000.0, 100.0, 100.0, 64).unwrap();
            cells.iter().filter(|c| **c).count()
        };
        let point = || {
            let mut d = Dataset::new("far", GeometryType::Point, Vec::new());
            d.push(Some(Geometry::Point(Coord::new(-0.3, -0.3))), Vec::new());
            d
        };
        let mut map = Map::new("m");
        let mut square = Dataset::new("border", GeometryType::Polygon, Vec::new());
        square.push(
            Some(Geometry::Polygon(vec![vec![
                [(-0.2, -0.2), (0.2, -0.2), (0.2, 0.2), (-0.2, 0.2), (-0.2, -0.2)].map(|(x, y)| Coord::new(x, y)).to_vec(),
            ]])),
            Vec::new(),
        );
        let border = map.add_dataset(square);
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == border) {
            let outline = rustgis_map::Symbol::default_for(GeometryType::Polygon, 0);
            l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol { fill: Rgba::new(0, 0, 0, 0), ..outline });
        }
        assert!(count(&mut map) > 0, "alone, it is what the map shows");
        map.add_dataset(point());
        let beside = count(&mut map);
        assert!(map.layer(border).is_some_and(|l| l.visible), "shown again");
        let mut alone = Map::new("m");
        alone.add_dataset(point());
        assert_eq!(beside, count(&mut alone), "only the point counts");
        let l = map.layers.iter_mut().find(|l| l.id == border).unwrap();
        assert!(only_outlines(l));
        l.labels.enabled = true;
        assert!(!only_outlines(l), "labels make it more than an outline");
        l.labels.enabled = false;
        l.fill_layers = vec![rustgis_map::fills::FillLayer::default()];
        assert!(!only_outlines(l), "a stacked fill draws inside it");
        l.no_fill = Some(rustgis_map::NoFill::Symbols);
        assert!(!only_outlines(l), "No Fill's symbols leave a stacked fill drawn");
        l.no_fill = Some(rustgis_map::NoFill::Override);
        assert!(only_outlines(l), "No Fill's override is the fill that hides the rest");
        l.no_fill = None;
        l.fill_layers.clear();
        l.symbol_by.fill = Some("'#ff0000'".into());
        assert!(!only_outlines(l), "a fill by expression draws inside it");
        l.symbol_by.fill = None;
        l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol::default_for(GeometryType::Polygon, 0));
        assert!(!only_outlines(l), "a filled symbol");
        l.no_fill = Some(rustgis_map::NoFill::Symbols);
        assert!(only_outlines(l), "No Fill counts as no fill");
        let symbol = rustgis_map::Symbol::default_for(GeometryType::Polygon, 0);
        l.renderer = rustgis_map::Renderer::Unique {
            field: "g".into(),
            classes: vec![("a".into(), symbol)],
            other: symbol,
            ramp: rustgis_map::Ramp::Ember,
            class_labels: Default::default(),
            other_label: None,
            class_strokes: Default::default(),
            other_strokes: Vec::new(),
            class_fills: [("a".to_string(), vec![rustgis_map::fills::FillLayer::default()])].into(),
            other_fills: Vec::new(),
            expression: None,
        };
        assert!(!only_outlines(l), "No Fill's symbols leave a class's stacked fill drawn");
        l.no_fill = Some(rustgis_map::NoFill::Override);
        assert!(only_outlines(l), "the override hides that too");
        l.no_fill = None;
        l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol {
            fill: Rgba::new(0, 0, 0, 0),
            ..rustgis_map::Symbol::default_for(GeometryType::Polygon, 0)
        });

        // A layer out of scale at the frame's scale draws nothing there: the outline is all it shows.
        let drawn_alone = {
            let far = map.layers.iter().find(|l| l.id != border).map(|l| l.id).unwrap();
            map.layers.iter_mut().find(|l| l.id == far).unwrap().min_scale = Some(1.0);
            let shown = count(&mut map);
            map.layers.iter_mut().find(|l| l.id == far).unwrap().min_scale = None;
            shown
        };
        assert!(drawn_alone > beside, "{drawn_alone} of {beside}");
    }

    /// A legend's rows go into columns: as even as they go, a layer's name with the row under it,
    /// and in a box too short each filled in turn; one column draws as it always has.
    #[test]
    fn a_legends_rows_go_into_columns() {
        use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
        let mut map = Map::new("m");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("c0".into())]);
        let pts = map.add_dataset(d);
        let s = |c| rustgis_map::Symbol::default_for(GeometryType::Point, c);
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
            // The layer's name, 30 classes and the other values: 32 rows.
            let classes = (0u32..30).map(|i| (format!("c{i}"), s(i))).collect();
            l.renderer = rustgis_map::Renderer::Unique {
                field: "g".into(),
                classes,
                other: s(31),
                ramp: rustgis_map::Ramp::Ember,
                class_labels: Default::default(),
                other_label: None,
                class_strokes: Default::default(),
                other_strokes: Vec::new(),
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: None,
            };
        }
        let opts = || LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() };
        let rows = legend_rows(&map, None, &opts());
        assert_eq!(rows.len(), 32);
        assert_eq!(balanced(&rows, 1, &opts()), (vec![0], 32.0 * LEGEND_ROW));
        assert_eq!(balanced(&rows, 2, &opts()), (vec![0, 16], 16.0 * LEGEND_ROW));
        assert_eq!(balanced(&rows, 3, &opts()).1, 11.0 * LEGEND_ROW);
        assert_eq!(balanced(&[], 3, &opts()), (vec![0], 0.0));
        // Too short for them all: each filled in turn, and the last holds what fits of the rest.
        let (starts, fits) = flow(&rows, 3, 5.0 * LEGEND_ROW, &opts());
        assert_eq!((starts, fits), (vec![0, 5, 10], false));
        // A layer's name isn't left at the foot of a column; the last, with none under it, can be.
        let heads = [LegendRow::Head("a"), LegendRow::Head("b"), LegendRow::Head("c"), LegendRow::Head("d"), LegendRow::Head("e")];
        assert_eq!(flow(&heads, 3, 3.0 * LEGEND_ROW, &opts()), (vec![0, 2], true));

        let sizes = legend_sizes_mm(&mut map, 1e6, true, false, &[], plain()).unwrap();
        let row = f64::from(LEGEND_ROW) * 25.4 / 96.0;
        for (size, rows) in sizes.iter().zip([32.0, 16.0, 11.0]) {
            assert!((size.1 - rows * row).abs() < 1e-6, "{size:?} for {rows} rows");
        }
        assert!(sizes[0].0 < sizes[1].0 && sizes[1].0 < sizes[2].0, "{sizes:?}");

        // How far right and down the legend draws in a box, and how many pixels.
        let ink = |columns: usize, w: f32, h: f32| {
            let mut pm = Pixmap::new(500, 500).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            crate::canvas::with_canvas(&mut pm, |c| {
                legend(c, Rect::from_xywh(0.0, 0.0, w, h).unwrap(), &map, ("", columns), None, opts(), 1.0);
            });
            let dark = |x, y| pm.pixel(x, y).is_some_and(|p| p.red() < 200 || p.green() < 200 || p.blue() < 200);
            let right = (0..500).filter(|&x| (0..500).any(|y| dark(x, y))).max().unwrap_or(0);
            let down = (0..500).filter(|&y| (0..500).any(|x| dark(x, y))).max().unwrap_or(0);
            (right, down)
        };
        let mm_pt = 96.0 / 25.4;
        let (right1, down1) = ink(1, 500.0, 500.0);
        let (right2, down2) = ink(2, 500.0, 500.0);
        let (right3, down3) = ink(3, 500.0, 500.0);
        assert!(f64::from(down1) > 31.0 * sizes[0].1 / 32.0 * mm_pt, "{down1}");
        assert!(f64::from(down2) <= sizes[1].1 * mm_pt && down2 > down3, "{down2} then {down3}");
        assert!(right1 < right2 && right2 < right3, "{right1}, {right2}, {right3}");
        assert!(f64::from(right3) <= sizes[2].0 * mm_pt + 1.0, "{right3} across {:?}", sizes[2]);
        // A value out of range, from a file edited by hand, draws what the nearest would.
        assert_eq!(ink(0, 500.0, 500.0), (right1, down1));
        assert_eq!(ink(9, 500.0, 500.0), (right3, down3));
        // A box too narrow for the columns closes them up: the last still starts inside it, past
        // where the first two end in a box of their own.
        let narrow = f64::from(right3) as f32 * 0.8;
        let (right_narrow, down_narrow) = ink(3, narrow, 500.0);
        assert!(f64::from(right_narrow) < f64::from(right3) && right_narrow > right1, "{right_narrow} of {right3}");
        // Moved sideways, the letters' lowest edge can land a pixel higher; a lost row would be 15.
        assert!(down_narrow.abs_diff(down3) < LEGEND_ROW as u32 / 2, "no row left out: {down_narrow} of {down3}");
    }

    /// The widest class label of `rows`, in points.
    fn widest_label(rows: &[LegendRow]) -> f32 {
        rows.iter().map(|row| row.width(0.0, plain()) - 12.0).fold(0.0, f32::max)
    }

    /// A point layer in Unique Values, for a legend's rows.
    fn classed_point_map(classes: Vec<(String, rustgis_map::Symbol)>, small: rustgis_map::Symbol) -> Map {
        point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Unique {
                field: "g".into(),
                classes,
                other: small,
                ramp: rustgis_map::Ramp::Ember,
                class_labels: Default::default(),
                other_label: None,
                class_strokes: Default::default(),
                other_strokes: Vec::new(),
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: None,
            };
        })
    }

    /// A box one row tall: in one column a layer's name is drawn alone, as it always was; in more,
    /// it waits for room for the class under it, and the first column stays blank.
    #[test]
    fn a_layers_name_stays_with_its_class_only_in_columns() {
        let small = rustgis_map::Symbol::default_for(GeometryType::Point, 0);
        let classes = (0u32..4).map(|i| (format!("c{i}"), small)).collect();
        let map = classed_point_map(classes, small);
        let opts = || LegendOptions { hide_other: true, exclude: &[], scale: 1e6, look: plain() };
        let rows = legend_rows(&map, None, &opts());
        let (starts, _) = flow(&rows, 2, LEGEND_ROW, &opts());
        assert_eq!(starts, vec![0, 2]);
        let first = widest(column_parts(&rows, &starts).next().unwrap(), &opts());
        let drawn_in_first = |columns: usize| {
            let mut pm = Pixmap::new(300, 15).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            crate::canvas::with_canvas(&mut pm, |c| {
                legend(c, Rect::from_xywh(0.0, 0.0, 300.0, LEGEND_ROW).unwrap(), &map, ("", columns), None, opts(), 1.0);
            });
            (0..first as u32).any(|x| (0..15).any(|y| pm.pixel(x, y).is_some_and(|p| p.red() < 200)))
        };
        assert!(drawn_in_first(1), "one column draws the name alone");
        assert!(!drawn_in_first(2), "two columns keep it with its class");
    }

    /// A large point symbol makes its row taller and widens its column's symbols only: the columns
    /// balance what the rows take, and the other column stays as narrow as its labels let it.
    #[test]
    fn a_legends_columns_hold_large_point_symbols() {
        let small = rustgis_map::Symbol { size: 4.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        let classes =
            (0u32..10).map(|i| (format!("c{i}"), rustgis_map::Symbol { size: if i < 2 { 40.0 } else { 4.0 }, ..small })).collect();
        let map = classed_point_map(classes, small);
        let opts = LegendOptions { hide_other: true, exclude: &[], scale: 1e6, look: plain() };
        let rows = legend_rows(&map, None, &opts);
        let tall = rows[1].height(plain(), 1e6);
        assert!(tall > 2.0 * LEGEND_ROW, "{tall}");
        // The name and the two large symbols, then the eight small ones: counted row by row, these
        // would be split at the middle.
        let (starts, height) = balanced(&rows, 2, &opts);
        assert_eq!(starts, vec![0, 3], "{height}");
        assert_eq!(height, 8.0 * LEGEND_ROW);
        let parts: Vec<_> = column_parts(&rows, &starts).collect();
        assert!(widest(parts[1], &opts) < widest(parts[0], &opts) - 20.0, "{} and {}", widest(parts[0], &opts), widest(parts[1], &opts));
        // In one column, the small rows' labels line up with the large symbols' as before.
        assert_eq!(widest(&rows, &opts), widest(parts[0], &opts).max(swatch_column(&rows, &opts) + 12.0 + widest_label(parts[1])));
    }

    #[test]
    fn legends_leave_out_what_the_frame_does_not_draw() {
        use rustgis_data::{Dataset, Field, FieldType, Geometry, Value};
        // A layer with one feature, of class "a", classed a, b and c; and an empty layer.
        let mut map = Map::new("m");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![Field::new("g", FieldType::Text)]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("a".into())]);
        let pts = map.add_dataset(d);
        map.add_dataset(Dataset::new("empty", GeometryType::Point, vec![]));
        let s = |c| rustgis_map::Symbol::default_for(GeometryType::Point, c);
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
            let classes = ["a", "b", "c"].iter().zip(0u32..).map(|(k, i)| (k.to_string(), s(i))).collect();
            l.renderer = rustgis_map::Renderer::Unique {
                field: "g".into(),
                classes,
                other: s(3),
                ramp: rustgis_map::Ramp::Ember,
                class_labels: Default::default(),
                other_label: None,
                class_strokes: Default::default(),
                other_strokes: Vec::new(),
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: None,
            };
        }
        // How far down the legend draws, in pixels.
        let depth_without = |map: &Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>, hide_other: bool, exclude: &[String]| {
            let mut pm = Pixmap::new(300, 400).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let opts = LegendOptions { hide_other, exclude, scale: 1e6, look: plain() };
            crate::canvas::with_canvas(&mut pm, |c| {
                legend(c, Rect::from_xywh(0.0, 0.0, 300.0, 400.0).unwrap(), map, ("", 1), drawn, opts, 1.0)
            });
            (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| pm.pixel(x, y).is_some_and(|p| p.red() < 200))).max().unwrap_or(0)
        };
        let depth = |map: &Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>, hide_other: bool| depth_without(map, drawn, hide_other, &[]);
        let drawn = shown_legends(&map, 1e6);
        assert_eq!(drawn.len(), 1, "the empty layer draws nothing: {drawn:?}");
        let (shown, all) = (depth(&map, Some(&drawn), false), depth(&map, None, false));
        // The drawn legend is two rows (the layer's heading and "a"); every class makes it six
        // (b, c, <all other values> and the empty layer's row too). Compared with each other, so
        // the font and antialiasing don't matter.
        assert!(shown > 0, "the drawn class is listed");
        assert!(all >= 2 * shown, "{shown} px of {all}");
        // The size a legend takes holds what it draws, to within a row: two rows drawn, six with
        // every class.
        let row = f64::from(LEGEND_ROW) * 25.4 / 96.0;
        // How far right the legend draws, in pixels (points at this scale), as `depth` goes down.
        let reach = |map: &Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>| {
            let mut pm = Pixmap::new(300, 400).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            crate::canvas::with_canvas(&mut pm, |c| {
                legend(
                    c,
                    Rect::from_xywh(0.0, 0.0, 300.0, 400.0).unwrap(),
                    map,
                    ("", 1),
                    drawn,
                    LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() },
                    1.0,
                )
            });
            (0..pm.width()).filter(|&x| (0..pm.height()).any(|y| pm.pixel(x, y).is_some_and(|p| p.red() < 200))).max().unwrap_or(0)
        };
        for (all_classes, ink, right) in [(false, shown, reach(&map, Some(&drawn))), (true, all, reach(&map, None))] {
            let (w, h) = legend_sizes_mm(&mut map, 1e6, all_classes, false, &[], plain()).unwrap()[0];
            let (ink, right) = (f64::from(ink) * 25.4 / 96.0, f64::from(right) * 25.4 / 96.0);
            assert!(w > 0.0 && ink <= h && h - ink < row, "{ink} mm drawn in {h} mm");
            // The widths in the same units as the drawing: the ink ends within the width measured,
            // short of it by no more than a glyph's side bearing.
            assert!(right <= w + 0.3 && right > w - 1.5, "{right} mm drawn across {w} mm");
        }
        assert!((legend_sizes_mm(&mut map, 1e6, false, false, &[], plain()).unwrap()[0].1 - 2.0 * row).abs() < 1e-9);
        // A layer the legend leaves out (by name, any case) isn't listed, drawn or not.
        let name = map.layer(pts).unwrap().name.to_uppercase();
        assert_eq!(depth_without(&map, Some(&drawn), false, std::slice::from_ref(&name)), 0, "its only drawn layer left out");
        let rest = depth_without(&map, None, false, &[name]);
        assert!(rest > 0 && rest < all, "the other layer is still listed: {rest} px of {all}");
        // Hiding <all other values> takes one row off the full legend, and nothing off the drawn
        // one, which has none.
        assert!(depth(&map, None, true) < all);
        assert_eq!(depth(&map, Some(&drawn), true), shown);
        // A layer whose drawn features all fall in <all other values>, hidden, is left out rather
        // than drawn as a plain swatch.
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts)
            && let rustgis_map::Renderer::Unique { classes, .. } = &mut l.renderer
        {
            classes.retain(|(k, _)| k != "a");
        }
        let drawn = shown_legends(&map, 1e6);
        assert!(depth(&map, Some(&drawn), false) > 0, "<all other values> alone is listed");
        assert_eq!(depth(&map, Some(&drawn), true), 0, "and hidden, the layer is left out");
        // So is one whose drawn features all fall in a fully transparent <all other values>,
        // rather than drawn as a plain swatch of its first feature.
        let set_other_alpha = |map: &mut Map, a: u8| {
            if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts)
                && let rustgis_map::Renderer::Unique { other, .. } = &mut l.renderer
            {
                other.fill = other.fill.with_alpha(a);
                other.stroke = other.stroke.with_alpha(a);
            }
        };
        set_other_alpha(&mut map, 0);
        let drawn = shown_legends(&map, 1e6);
        assert_eq!(depth(&map, Some(&drawn), false), 0, "the invisible fallback stands for nothing");
        set_other_alpha(&mut map, 255);
        // A class that bears the fallback's name is a class: hiding the fallback keeps it.
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts)
            && let rustgis_map::Renderer::Unique { classes, .. } = &mut l.renderer
        {
            classes.push(("a".into(), s(0)));
            classes.iter_mut().for_each(|(k, _)| {
                if k == "a" {
                    *k = rustgis_map::symbology::OTHER_VALUES.into();
                }
            });
        }
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
            l.dataset.features.iter_mut().for_each(|f| f.attrs = vec![Value::Text(rustgis_map::symbology::OTHER_VALUES.into())]);
        }
        let drawn = shown_legends(&map, 1e6);
        assert!(depth(&map, Some(&drawn), true) > 0, "the class named like the fallback is still listed");
        // A Graduated layer with no classes still shows its plain swatch, as before #207.
        if let Some(l) = map.layers.iter_mut().find(|l| l.id == pts) {
            l.renderer = rustgis_map::Renderer::Graduated {
                field: "g".into(),
                breaks: Vec::new(),
                symbols: Vec::new(),
                ramp: rustgis_map::Ramp::Ember,
                method: rustgis_map::ClassMethod::NaturalBreaks,
                min: 0.0,
                classes: 0,
                class_labels: Default::default(),
                class_strokes: Default::default(),
                class_fills: Default::default(),
            };
        }
        map.layers.retain(|l| l.id == pts);
        assert!(depth(&map, None, false) > 0, "the empty Graduated layer is listed");
    }

    /// A layout with one map frame on `map` and a colorbar of its raster at 10–90 × 10–30 mm
    /// (a vertical one at 10–30 × 10–90 mm), drawn at 96 dpi.
    fn colorbar_page(map: &Map, orientation: Orientation, reverse: bool, title: &str) -> Pixmap {
        let mut l = Layout::new("Layout", 100.0, 100.0);
        let frame = l.add(
            "Map",
            (95.0, 95.0, 4.0, 4.0),
            ElementKind::MapFrame {
                map: map.name.clone(),
                center: Coord::new(0.0, 0.0),
                scale: 1e8,
                shape: FrameShape::Rectangle,
                grid: None,
                extent_of: None,
                extent_color: rustgis_map::layout::INDICATOR_COLOR,
                extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        let rect = if orientation == Orientation::Vertical { (10.0, 10.0, 20.0, 80.0) } else { (10.0, 10.0, 80.0, 20.0) };
        let kind =
            ElementKind::Colorbar { frame, layer: None, title: title.into(), orientation, ticks: 5, tick_side: TickSide::End, reverse };
        l.add("Colorbar", rect, kind);
        render(&l, &mut [map.clone()], 96.0, &PageInfo::default()).unwrap()
    }

    /// A map with one 100 × 1 raster valued 0–99 by column, styled by `renderer`.
    fn ramp_map(renderer: rustgis_map::raster::RasterRenderer, stretch: rustgis_map::raster::Stretch) -> Map {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.01, pixel_height: -0.01 };
        let data: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let r = Raster::new("dem", 100, 1, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let mut rl = rustgis_map::raster::RasterLayer::from_path(r, "dem.tif");
        rl.style.renderer = renderer;
        rl.style.stretch = stretch;
        let mut map = Map::new("m");
        map.add_raster("dem", rl);
        map
    }

    /// A page with a main frame and an inset (5–55 mm) that outlines it in `color` at `width`.
    fn indicator_page(color: Rgba, width: f64) -> Pixmap {
        let map = Map::new("m");
        let frame = |center, scale, extent_of| ElementKind::MapFrame {
            map: "m".into(),
            center,
            scale,
            shape: FrameShape::Rectangle,
            grid: None,
            extent_of,
            extent_color: color,
            extent_width: width,
            bearing: 0.0,
            pitch: 0.0,
        };
        let mut l = Layout::new("Layout", 100.0, 100.0);
        let main = l.add("Main", (60.0, 60.0, 30.0, 30.0), frame(Coord::new(20.0, 10.0), 5e7, None));
        l.add("Inset", (5.0, 5.0, 50.0, 50.0), frame(Coord::new(0.0, 0.0), 2e8, Some(main)));
        render(&l, &mut [map], 96.0, &PageInfo::default()).unwrap()
    }

    /// Pixels inside the inset (5–55 mm) close to `c`.
    fn inset_pixels(pm: &Pixmap, c: Rgba) -> usize {
        let k = PT_PER_MM;
        let near = |p: (u8, u8, u8)| p.0.abs_diff(c.r()) < 40 && p.1.abs_diff(c.g()) < 40 && p.2.abs_diff(c.b()) < 40;
        ((5.0 * k) as u32..(55.0 * k) as u32)
            .flat_map(|y| ((5.0 * k) as u32..(55.0 * k) as u32).map(move |x| (x, y)))
            .filter(|&(x, y)| near(rgb(pm, x, y)))
            .count()
    }

    /// One line of black text drawn in a white 400 × 60 px box with `style`.
    fn styled_line(style: TextStyle) -> Pixmap {
        let mut pm = Pixmap::new(400, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let r = Rect::from_xywh(0.0, 0.0, 400.0, 60.0).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| text_block(c, r, "Oceano Atlântico", 20.0, Rgba::rgb(0, 0, 0), style, 1.0));
        pm
    }

    /// The leftmost and rightmost inked columns, and the inked column nearest the left at the
    /// top and at the bottom of the letters.
    fn text_ink(pm: &Pixmap) -> (u32, u32, u32, u32) {
        let dark = |x: u32, y: u32| rgb(pm, x, y).0 < 128;
        let rows: Vec<u32> = (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| dark(x, y))).collect();
        let (top, bottom) = (rows[0], rows[rows.len() - 1]);
        let cols: Vec<u32> = (0..pm.width()).filter(|&x| (0..pm.height()).any(|y| dark(x, y))).collect();
        let first = |y: u32| (0..pm.width()).find(|&x| dark(x, y)).unwrap_or(0);
        (cols[0], cols[cols.len() - 1], first(top + 1), first(bottom - 1))
    }

    /// The height of the inked rows.
    fn text_height(pm: &Pixmap) -> u32 {
        let rows: Vec<u32> = (0..pm.height()).filter(|&y| (0..pm.width()).any(|x| rgb(pm, x, y).0 < 128)).collect();
        rows[rows.len() - 1] - rows[0]
    }

    /// Text elements take italic, alignment and letter spacing (#213, point c); plain, they draw
    /// as before.
    #[test]
    fn text_takes_italic_alignment_and_letter_spacing() {
        let plain = TextStyle { face: text::Face::default(), align: TextAlign::Left, spacing: 0.0 };
        let base = styled_line(plain);
        let (l, r, ..) = text_ink(&base);
        assert!(l < 4, "left-aligned starts at the left: {l}");
        // Plain is what the old upright, left-aligned drawing gave.
        let mut old = Pixmap::new(400, 60).unwrap();
        old.fill(tiny_skia::Color::WHITE);
        let px = 20.0 * 1.33;
        crate::canvas::with_canvas(&mut old, |c| {
            text::text_at(c, "Oceano Atlântico", 0.0, px * 0.6, px, Rgba::rgb(0, 0, 0), false, text::Align::Left, false)
        });
        assert_eq!(text_ink(&old).0.abs_diff(l), 0);
        assert_eq!(text_ink(&old).1.abs_diff(r), 0);
        // Right and centre: the same width, moved.
        let (rl, rr, ..) = text_ink(&styled_line(TextStyle { align: TextAlign::Right, ..plain }));
        assert!((397..=399).contains(&rr) && (rr - rl).abs_diff(r - l) <= 1, "right-aligned ends at the right: {rl}..{rr}");
        let (cl, cr, ..) = text_ink(&styled_line(TextStyle { align: TextAlign::Center, ..plain }));
        assert!(cl.abs_diff(400 - cr) <= 2, "centred: {cl}..{cr}");
        // Spacing: 15 gaps of 3 pt widen the line by about 15 × 3 × 1.33 px.
        let (sl, sr, ..) = text_ink(&styled_line(TextStyle { spacing: 3.0, ..plain }));
        let grown = (sr - sl) as f32 - (r - l) as f32;
        assert!((grown - 15.0 * 3.0 * 1.33).abs() < 3.0, "{grown}");
        // Italic: a straight stem (an `l`) leans right, its top about tan 12° of its height
        // right of its foot; upright, it stands straight.
        let stem = |italic: bool| {
            let mut pm = Pixmap::new(120, 120).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let r = Rect::from_xywh(40.0, 0.0, 80.0, 120.0).unwrap();
            crate::canvas::with_canvas(&mut pm, |c| {
                text_block(c, r, "l", 60.0, Rgba::rgb(0, 0, 0), TextStyle { face: text::Face { italic, ..plain.face }, ..plain }, 1.0)
            });
            let (.., top, bottom) = text_ink(&pm);
            (top as i32 - bottom as i32, text_height(&pm))
        };
        let ((upright, _), (lean, height)) = (stem(false), stem(true));
        assert!(upright.abs() <= 1, "upright stem: {upright}");
        assert!((lean as f32 - 0.2126 * height as f32).abs() <= 3.0, "lean {lean} over {height} px");
        // Right-aligned, slanted or tightened past the letters' width, the ink stays in the box.
        for style in [
            TextStyle { align: TextAlign::Right, face: text::Face { italic: true, ..plain.face }, ..plain },
            TextStyle { align: TextAlign::Right, spacing: -5.0, ..plain },
        ] {
            let mut pm = Pixmap::new(400, 60).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let r = Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap();
            crate::canvas::with_canvas(&mut pm, |c| text_block(c, r, "Oceano Atlântico", 20.0, Rgba::rgb(0, 0, 0), style, 1.0));
            let (l, r, ..) = text_ink(&pm);
            assert!((296..=300).contains(&r) && l < r, "inked {l}..{r} in a 300 px box");
        }
        // 2 pt letters at -5 pt run backwards: still drawn, inside the box.
        let mut pm = Pixmap::new(400, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let r = Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| {
            text_block(c, r, "MMMM", 2.0, Rgba::rgb(0, 0, 0), TextStyle { align: TextAlign::Right, spacing: -5.0, ..plain }, 4.0)
        });
        let (l, r, ..) = text_ink(&pm);
        assert!(r <= 300 && l < r, "inked {l}..{r}");
    }

    /// The extent indicator is drawn in its own colour and width (#213): red and 2 pt until set.
    #[test]
    fn extent_indicators_take_their_colour_and_width() {
        use rustgis_map::layout::{INDICATOR_COLOR, INDICATOR_WIDTH};
        let blue = Rgba::rgb(0x1F, 0x4E, 0x9A);
        let default = indicator_page(INDICATOR_COLOR, INDICATOR_WIDTH);
        assert!(inset_pixels(&default, INDICATOR_COLOR) > 0, "red by default");
        let thin = indicator_page(blue, 1.0);
        let thick = indicator_page(blue, 6.0);
        assert_eq!(inset_pixels(&thin, INDICATOR_COLOR), 0, "no red once restyled");
        let (t, w) = (inset_pixels(&thin, blue), inset_pixels(&thick, blue));
        assert!(t > 0 && w > 3 * t, "a wider outline covers more: {t} at 1 pt, {w} at 6 pt");
    }

    fn rgb(pm: &Pixmap, x: u32, y: u32) -> (u8, u8, u8) {
        pm.pixel(x, y).map(|p| (p.red(), p.green(), p.blue())).unwrap_or((255, 255, 255))
    }

    #[test]
    fn colorbars_draw_the_raster_colour_scale() {
        use rustgis_map::raster::{RasterRenderer, Stretch};
        let ramp = rustgis_map::Ramp::Thermal;
        let map = ramp_map(RasterRenderer::Stretch { band: 0, ramp: ramp.clone(), invert: false }, Stretch::MinMax);
        let k = PT_PER_MM;
        // A row through the bar, near its top edge (the labels are below it).
        let y = (12.0 * k) as u32;
        let pm = colorbar_page(&map, Orientation::Horizontal, false, "");
        let (left, right) = (rgb(&pm, (16.0 * k) as u32, y), rgb(&pm, (84.0 * k) as u32, y));
        let close = |a: (u8, u8, u8), b: Rgba| a.0.abs_diff(b.r()) < 40 && a.1.abs_diff(b.g()) < 40 && a.2.abs_diff(b.b()) < 40;
        assert!(close(left, ramp.at(0.0)), "the low end is the ramp's start: {left:?} vs {:?}", ramp.at(0.0));
        assert!(close(right, ramp.at(1.0)), "the high end is the ramp's end: {right:?} vs {:?}", ramp.at(1.0));
        // Labelled below the bar: dark text under it, between the bar and the element's bottom.
        let inked = |pm: &Pixmap, y0: f64, y1: f64| {
            ((y0 * k) as u32..(y1 * k) as u32).any(|y| (0..pm.width()).any(|x| rgb(pm, x, y).0 < 80 && rgb(pm, x, y).2 < 80))
        };
        assert!(inked(&pm, 26.0, 30.0), "tick labels below the bar");
        // Reversed, the ends swap.
        let rev = colorbar_page(&map, Orientation::Horizontal, true, "");
        assert!(close(rgb(&rev, (16.0 * k) as u32, y), ramp.at(1.0)));
        assert!(close(rgb(&rev, (84.0 * k) as u32, y), ramp.at(0.0)));
        // Vertical, low values sit at the bottom.
        let v = colorbar_page(&map, Orientation::Vertical, false, "");
        let x = (11.0 * k) as u32;
        assert!(close(rgb(&v, x, (85.0 * k) as u32), ramp.at(0.0)), "{:?}", rgb(&v, x, (85.0 * k) as u32));
        assert!(close(rgb(&v, x, (15.0 * k) as u32), ramp.at(1.0)), "{:?}", rgb(&v, x, (15.0 * k) as u32));
        // A title pushes the bar down under it.
        let t = colorbar_page(&map, Orientation::Horizontal, false, "Elevation (m)");
        assert!(inked(&t, 10.0, 13.0), "the title is drawn at the top");

        // Classes draw as steps, one colour each.
        let breaks = vec![24.0, 49.0, 74.0, 99.0];
        let classify =
            RasterRenderer::Classify { band: 0, method: rustgis_map::ClassMethod::EqualInterval, ramp: ramp.clone(), breaks, classes: 4 };
        let pm = colorbar_page(&ramp_map(classify, Stretch::MinMax), Orientation::Horizontal, false, "");
        let mut colours: Vec<(u8, u8, u8)> = ((16.0 * k) as u32..(84.0 * k) as u32).map(|x| rgb(&pm, x, y)).collect();
        colours.dedup();
        assert!((4..=7).contains(&colours.len()), "four steps (and their seams): {colours:?}");

        // In an element too short for its labels, they are left out rather than drawn under it.
        let mut l = Layout::new("Layout", 100.0, 100.0);
        let frame = l.add(
            "Map",
            (95.0, 95.0, 4.0, 4.0),
            ElementKind::MapFrame {
                map: "m".into(),
                center: Coord::new(0.0, 0.0),
                scale: 1e8,
                shape: FrameShape::Rectangle,
                grid: None,
                extent_of: None,
                extent_color: rustgis_map::layout::INDICATOR_COLOR,
                extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        let kind = ElementKind::Colorbar {
            frame,
            layer: None,
            title: String::new(),
            orientation: Orientation::Horizontal,
            ticks: 5,
            tick_side: TickSide::End,
            reverse: false,
        };
        l.add("Colorbar", (10.0, 10.0, 80.0, 3.0), kind);
        let short = render(&l, &mut [map.clone()], 96.0, &PageInfo::default()).unwrap();
        assert!(!inked(&short, 13.5, 20.0), "nothing spills below the element");
        assert!(close(rgb(&short, (16.0 * k) as u32, (11.0 * k) as u32), ramp.at(0.0)), "the bar is still drawn");

        // RGB has no colour scale: a placeholder, not a bar.
        let pm = colorbar_page(
            &ramp_map(RasterRenderer::Rgb { red: 0, green: 0, blue: 0 }, Stretch::MinMax),
            Orientation::Horizontal,
            false,
            "",
        );
        assert_eq!(rgb(&pm, (16.0 * k) as u32, y), (0xF0, 0xF1, 0xF3));
    }

    #[test]
    fn colorbar_tick_labels_are_even_and_tidy() {
        let labels = |lo, hi, n| tick_labels(lo, hi, n).into_iter().map(|(_, s)| s).collect::<Vec<_>>();
        assert_eq!(labels(0.0, 100.0, 5), ["0", "25", "50", "75", "100"]);
        assert_eq!(labels(0.0, 1.0, 5), ["0.00", "0.25", "0.50", "0.75", "1.00"]);
        assert_eq!(labels(0.0, 1.0, 3), ["0.0", "0.5", "1.0"]);
        assert_eq!(labels(-1.0, 1.0, 2), ["-1", "1"]);
        // Odd bounds keep only the decimals the step needs.
        assert_eq!(labels(12.345, 87.9, 2), ["12", "88"]);
        assert_eq!(labels(-0.004, 1.0, 2), ["0", "1"], "no -0");
        // One value: one label. Out of range counts are clamped.
        assert_eq!(labels(5.0, 5.0, 5), ["5"]);
        assert_eq!(tick_labels(0.0, 1.0, 0).len(), 2);
        assert_eq!(tick_labels(0.0, 1.0, 1000).len(), rustgis_map::layout::MAX_COLORBAR_TICKS);
        assert!(tick_labels(f64::NAN, 1.0, 5).is_empty());
        assert_eq!(labels(0.0, 4e6, 2), ["0", "4.0M"]);
    }

    #[test]
    fn hairline_rectangles_draw_thin() {
        // A filled 0.15 mm rule at 300 dpi is under two pixels tall, not a 1 mm bar (#211).
        let mut l = Layout::new("Layout", 50.0, 20.0);
        l.add(
            "Rule",
            (5.0, 10.0, 40.0, 0.15),
            ElementKind::Rectangle { fill: Rgba::rgb(0, 0, 0), outline: Rgba::rgb(0, 0, 0), width: 0.0, radius: 0.0 },
        );
        let pm = render(&l, &mut [], 300.0, &PageInfo::default()).unwrap();
        let x = pm.width() / 2;
        let inked = (0..pm.height()).filter(|&y| pm.pixel(x, y).is_some_and(|p| p.red() < 250)).count();
        assert!((1..=3).contains(&inked), "{inked} rows inked");
    }

    #[test]
    fn arrowheads_are_drawn_at_the_ends_they_point_to() {
        use rustgis_map::layout::LineDir;
        // 100 × 20 mm at 254 dpi (ten pixels a millimetre); a level arrow from 10 to 90 mm.
        let mut l = Layout::new("Layout", 100.0, 20.0);
        let blue = Rgba::rgb(0, 0, 255);
        l.add(
            "Arrow",
            (10.0, 7.0, 80.0, 6.0),
            ElementKind::Line { color: blue, width: 1.0, dir: LineDir::Horizontal, head: LineHead::End },
        );
        let pm = render(&l, &mut [], 254.0, &PageInfo::default()).unwrap();
        let inked = |x: u32, y: u32| pm.pixel(x, y).is_some_and(|p| p.red() < 128 && p.blue() > 128);
        // At 254 dpi (2.65 pixels a point) the shaft is about 2.6 px wide and the head about 22 px
        // long and 10 px either side of the line: 5 px off the line, 15 px back from the tip, is
        // inside the head, but clear of the shaft at the tail.
        assert!(inked(500, 100), "the shaft");
        assert!(inked(885, 105) && inked(885, 95), "the head, either side of the line");
        assert!(!inked(115, 105) && !inked(115, 95), "no head at the tail");
        assert!(!inked(905, 100), "nothing past the tip");
    }

    #[test]
    fn ellipses_and_lines_draw_inside_their_boxes() {
        use rustgis_map::layout::LineDir;
        // 100 × 50 mm at 254 dpi: ten pixels a millimetre.
        let mut l = Layout::new("Layout", 100.0, 50.0);
        let red = Rgba::rgb(255, 0, 0);
        l.add("Circle", (10.0, 10.0, 30.0, 30.0), ElementKind::Ellipse { fill: red, outline: red, width: 0.0 });
        l.add(
            "Line",
            (50.0, 10.0, 40.0, 30.0),
            ElementKind::Line { color: Rgba::rgb(0, 0, 255), width: 2.0, dir: LineDir::Rising, head: LineHead::None },
        );
        let pm = render(&l, &mut [], 254.0, &PageInfo::default()).unwrap();
        let is = |x: u32, y: u32, want: (u8, u8, u8)| pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == want);
        let (white, red, blue) = ((255, 255, 255), (255, 0, 0), (0, 0, 255));
        // The circle's middle is filled; its box's corner, outside the circle, isn't.
        assert!(is(250, 250, red), "circle centre");
        assert!(is(110, 110, white), "outside the circle, in its box's corner");
        // The rising line runs from the box's bottom-left to its top-right, through the middle.
        assert!(is(700, 250, blue), "the line's middle");
        assert!(is(550, 150, white) && is(850, 350, white), "the other diagonal stays blank");
    }

    /// A 100 × 100 mm page with one map frame filling 10–90 mm, showing the world at 1:200M with
    /// `grid` over it, drawn at 96 dpi.
    fn grid_page(grid: rustgis_map::layout::Grid) -> Pixmap {
        let mut maps = vec![Map::new("m")];
        let mut l = Layout::new("Layout", 100.0, 100.0);
        l.add(
            "Map",
            (10.0, 10.0, 80.0, 80.0),
            ElementKind::MapFrame {
                map: "m".into(),
                center: Coord::new(0.0, 0.0),
                scale: 2e8,
                shape: FrameShape::Rectangle,
                grid: Some(grid),
                extent_of: None,
                extent_color: rustgis_map::layout::INDICATOR_COLOR,
                extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap()
    }

    /// Pixels in the box (page millimetres) that `hit` picks.
    fn count(pm: &Pixmap, (x0, y0, x1, y1): (f64, f64, f64, f64), hit: impl Fn(tiny_skia::PremultipliedColorU8) -> bool) -> usize {
        let px = |mm: f64| (mm * PT_PER_MM) as u32;
        (px(y0)..px(y1)).flat_map(|y| (px(x0)..px(x1)).map(move |x| (x, y))).filter(|&(x, y)| pm.pixel(x, y).is_some_and(&hit)).count()
    }

    fn red(p: tiny_skia::PremultipliedColorU8) -> bool {
        p.red() > 180 && p.green() < 90 && p.blue() < 90
    }

    fn ink(p: tiny_skia::PremultipliedColorU8) -> bool {
        p.red() < 90 && p.green() < 90 && p.blue() < 90
    }

    /// A page whose 80 mm frame shows Winterthur at 1:25,000 (2 km across) on a map in `projection`.
    fn winterthur_page(grid: rustgis_map::layout::Grid, projection: rustgis_map::Projection) -> Pixmap {
        let mut m = Map::new("m");
        m.projection = projection;
        let mut maps = vec![m];
        let mut l = Layout::new("Layout", 100.0, 100.0);
        l.add(
            "Map",
            (10.0, 10.0, 80.0, 80.0),
            ElementKind::MapFrame {
                map: "m".into(),
                center: Coord::new(8.73, 47.5),
                scale: 25_000.0,
                shape: FrameShape::Rectangle,
                grid: Some(grid),
                extent_of: None,
                extent_color: rustgis_map::layout::INDICATOR_COLOR,
                extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap()
    }

    /// Where red runs cross a row of the page (page millimetres): the middle of each run.
    fn red_runs(pm: &Pixmap, y_mm: f64) -> Vec<f64> {
        let y = (y_mm * PT_PER_MM) as u32;
        let mut out = Vec::new();
        let mut start = None;
        for x in 0..pm.width() {
            match (pm.pixel(x, y).is_some_and(red), start) {
                (true, None) => start = Some(x),
                (false, Some(s)) => {
                    out.push(f64::from(s + x - 1) / 2.0 / PT_PER_MM);
                    start = None;
                }
                _ => {}
            }
        }
        out
    }

    /// A measured grid: eastings and northings its step apart in its coordinate system (1 km is
    /// 40 mm at 1:25,000), bent onto the map's projection, straight on a map in that system; the
    /// map's own system, else the frame's UTM zone, when it names none; nothing for a system it
    /// can't draw.
    #[test]
    fn measured_grids_draw_eastings_and_northings() {
        use rustgis_map::layout::{Grid, GridKind};
        let utm = Grid {
            kind: GridKind::Measured,
            crs: Some("EPSG:25832".into()),
            step: 1000.0,
            color: Rgba::rgb(0xFF, 0, 0),
            labels: false,
            width: 1.5,
            ..Grid::default()
        };
        let gaps = |xs: &[f64]| xs.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>();
        // On Web Mercator: 1 km is 40 mm on paper, give or take Mercator's scale at 47.5°N.
        let pm = winterthur_page(utm.clone(), rustgis_map::Projection::WebMercator);
        let xs = red_runs(&pm, 50.0);
        assert!(xs.len() >= 2, "{xs:?}");
        assert!(gaps(&xs).iter().all(|g| (g - 40.0).abs() < 1.5), "{xs:?}");
        // On a map in EPSG:25832 itself: exactly 40 mm, and straight (the same at the top and bottom).
        let own = rustgis_map::Projection::Custom("EPSG:25832".into());
        let pm = winterthur_page(utm.clone(), own.clone());
        let (top, bottom) = (red_runs(&pm, 20.0), red_runs(&pm, 80.0));
        assert!(gaps(&top).iter().all(|g| (g - 40.0).abs() < 0.3), "{top:?}");
        assert_eq!(top.len(), bottom.len());
        assert!(top.iter().zip(&bottom).all(|(a, b)| (a - b).abs() < 0.3), "{top:?} {bottom:?}");
        // Naming none: the map's own system draws the same grid; on Web Mercator, UTM 32N's
        // (EPSG:32632, the same eastings as 25832 to a metre) does.
        let unnamed = Grid { crs: None, ..utm.clone() };
        assert!(red_runs(&winterthur_page(unnamed.clone(), own), 20.0).iter().zip(&top).all(|(a, b)| (a - b).abs() < 0.3));
        let mercator = red_runs(&winterthur_page(utm.clone(), rustgis_map::Projection::WebMercator), 20.0);
        let zone = red_runs(&winterthur_page(unnamed, rustgis_map::Projection::WebMercator), 20.0);
        assert_eq!(mercator.len(), zone.len());
        assert!(mercator.iter().zip(&zone).all(|(a, b)| (a - b).abs() < 0.3), "{mercator:?} {zone:?}");
        // A map in an ESRI system (or a preset): its own, by its PROJ definition, straight on it.
        let esri = rustgis_map::Projection::Custom("ESRI:102031".into());
        let pm = winterthur_page(Grid { crs: None, ..utm.clone() }, esri);
        let (top, bottom) = (red_runs(&pm, 20.0), red_runs(&pm, 80.0));
        assert!(top.len() >= 2 && top.len() == bottom.len(), "{top:?} {bottom:?}");
        assert!(top.iter().zip(&bottom).all(|(a, b)| (a - b).abs() < 0.3), "the map's own system: straight {top:?} {bottom:?}");
        // A system it can't draw (geographic, unknown): nothing.
        for crs in ["EPSG:4326", "EPSG:1", "not a system"] {
            let pm = winterthur_page(Grid { crs: Some(crs.into()), ..utm.clone() }, rustgis_map::Projection::WebMercator);
            assert_eq!(count(&pm, (10.0, 10.0, 90.0, 90.0), red), 0, "{crs}");
        }
        // A step too fine for the frame is widened rather than drawn as a blur.
        let fine = winterthur_page(Grid { step: 1.0, ..utm.clone() }, rustgis_map::Projection::WebMercator);
        // (About 2 km across: 50 m apart, some 40 lines across the frame.)
        let across = red_runs(&fine, 50.0).len();
        assert!((20..=55).contains(&across), "{across} lines across: not a red blur");
        // Its labels: eastings and northings, whole or short, a step apart.
        let tr = rustgis_data::raster::Crs::parse("EPSG:25832").unwrap().transformer().unwrap();
        let frame: Vec<Coord> =
            [(8.71, 47.49), (8.75, 47.49), (8.75, 47.51), (8.71, 47.51)].iter().map(|&(x, y)| Coord::new(x, y)).collect();
        let mut lines = Vec::new();
        measured_lines(&utm, &tr, &frame, &mut lines);
        let eastings: Vec<&str> = lines.iter().filter(|l| l.lon).map(|l| l.label.as_str()).collect();
        assert!(eastings.contains(&"479000") && eastings.contains(&"480000"), "{eastings:?}");
        assert!(lines.iter().filter(|l| !l.lon).any(|l| l.label == "5261000"), "{lines:?}");
        let mut short = Vec::new();
        measured_lines(&Grid { short: true, ..utm.clone() }, &tr, &frame, &mut short);
        assert!(short.iter().any(|l| l.lon && l.label == "79") && short.iter().any(|l| !l.lon && l.label == "61"));
        // Written whole too, for the corners.
        assert!(short.iter().any(|l| l.lon && l.label == "79" && l.full == "479000"));
        // A frame of no extent draws none.
        let mut none = Vec::new();
        measured_lines(&utm, &tr, &[], &mut none);
        assert!(none.is_empty());
        assert_eq!((nice_step(2000.0, 1.0), nice_step(7000.0, 1.0), nice_step(1.0, 1.0)), (500.0, 1000.0, 1.0));
    }

    #[test]
    fn grids_draw_lines_ticks_or_both() {
        use rustgis_map::layout::{Grid, GridStyle};
        let g = |style| Grid {
            step: 30.0,
            color: Rgba::rgb(0xFF, 0, 0),
            labels: false,
            style,
            tick_length: 4.0,
            width: 1.0,
            ..Grid::default()
        };
        let inner = (25.0, 25.0, 75.0, 75.0);
        let rim = (10.0, 10.0, 90.0, 13.0);
        let lines = grid_page(g(GridStyle::Lines));
        let ticks = grid_page(g(GridStyle::Ticks));
        let both = grid_page(g(GridStyle::LinesAndTicks));
        // Ticks only: nothing across the middle of the frame, but marks along its edge.
        assert!(count(&lines, inner, red) > 100, "lines cross the frame");
        assert_eq!(count(&ticks, inner, red), 0, "ticks stay at the edges");
        assert!(count(&ticks, rim, red) > 10, "ticks along the top edge");
        // Lines and ticks: the lines, with the ticks' extra width along the edge.
        assert!(count(&both, inner, red) > 100);
        assert!(count(&both, rim, red) > count(&lines, rim, red), "ticks thicken the lines at the edge");
        // The line width is the grid's.
        let thick = grid_page(Grid { width: 3.0, ..g(GridStyle::Lines) });
        assert!(count(&thick, inner, red) > 2 * count(&lines, inner, red));
    }

    #[test]
    fn grid_labels_go_on_the_edges_picked() {
        use rustgis_map::layout::{Grid, GridEdges};
        let g = |edges, label_orientation| Grid { step: 30.0, color: Rgba::new(0, 0, 0, 0), edges, label_orientation, ..Grid::default() };
        let (top, bottom) = ((20.0, 10.0, 80.0, 16.0), (20.0, 84.0, 80.0, 90.0));
        let (left, right) = ((10.0, 20.0, 18.0, 80.0), (82.0, 20.0, 90.0, 80.0));
        let default = grid_page(g(GridEdges::default(), Orientation::Horizontal));
        assert!(count(&default, bottom, ink) > 0 && count(&default, left, ink) > 0, "bottom and left by default");
        assert_eq!(count(&default, top, ink) + count(&default, right, ink), 0);
        let flipped = grid_page(g(GridEdges { top: true, bottom: false, left: false, right: true }, Orientation::Horizontal));
        assert!(count(&flipped, top, ink) > 0 && count(&flipped, right, ink) > 0, "top and right when picked");
        assert_eq!(count(&flipped, bottom, ink) + count(&flipped, left, ink), 0);
        // Vertical side labels run along the edge: narrower across it than horizontal ones.
        // Columns with any text in them, inside the frame's left edge; antialiased text counts
        // too, as a thin system font (Windows) may leave no pixel of a stem fully dark.
        let across = |pm: &Pixmap| {
            let px = |mm: f64| (mm * PT_PER_MM) as u32;
            let shaded = |p: tiny_skia::PremultipliedColorU8| p.red() < 200 && p.green() < 200 && p.blue() < 200;
            (px(11.0)..px(30.0)).filter(|&x| (px(20.0)..px(80.0)).any(|y| pm.pixel(x, y).is_some_and(shaded))).count()
        };
        let horizontal = grid_page(g(GridEdges { top: false, bottom: false, left: true, right: false }, Orientation::Horizontal));
        let vertical = grid_page(g(GridEdges { top: false, bottom: false, left: true, right: false }, Orientation::Vertical));
        assert!(count(&vertical, left, ink) > 0);
        assert!(across(&vertical) < across(&horizontal), "{} columns, against {}", across(&vertical), across(&horizontal));
    }

    /// A grid's labels take a type, halo, offset and direction of their own, and a measured
    /// grid's whole values a unit, emphasis and corners written whole (#grid-labels).
    #[test]
    fn grid_labels_take_their_own_type_halo_offset_and_direction() {
        use rustgis_map::layout::{EdgeOrientations, Grid, GridEdges};
        let g = |f: &dyn Fn(&mut Grid)| {
            let mut g = Grid {
                step: 30.0,
                color: Rgba::new(0, 0, 0, 0),
                edges: GridEdges { top: false, bottom: true, left: false, right: false },
                ..Grid::default()
            };
            f(&mut g);
            g
        };
        let bottom = (12.0, 76.0, 88.0, 90.0);
        let reds = |pm: &Pixmap| count(pm, bottom, red);
        let plain = grid_page(g(&|_| {}));
        // In red, and larger: more red.
        let small = grid_page(g(&|g| g.text.color = Some(Rgba::rgb(0xFF, 0, 0))));
        let large = grid_page(g(&|g| (g.text.color, g.text.size) = (Some(Rgba::rgb(0xFF, 0, 0)), 14.0)));
        assert_eq!(reds(&plain), 0, "dark grey until a colour is set");
        assert!(reds(&small) > 0 && reds(&large) > reds(&small) * 2, "{} {}", reds(&small), reds(&large));
        // A blue halo, and none.
        let blue = |p: tiny_skia::PremultipliedColorU8| p.blue() > 180 && p.red() < 90 && p.green() < 90;
        let haloed = grid_page(g(&|g| g.halo = Some(Rgba::rgb(0, 0, 0xFF))));
        assert!(count(&haloed, bottom, blue) > 20, "a blue halo");
        assert_eq!(count(&plain, bottom, blue), 0);
        let bare = grid_page(g(&|g| g.halo = Some(Rgba::new(0, 0, 0, 0))));
        assert_ne!(bare.data(), plain.data(), "no white halo");
        // Moved off the edge: the labels sit higher in the frame.
        let lowest = |pm: &Pixmap| {
            let px = |mm: f64| (mm * PT_PER_MM) as u32;
            (px(70.0)..px(90.0)).filter(|&y| (px(12.0)..px(88.0)).any(|x| pm.pixel(x, y).is_some_and(ink))).max().unwrap_or(0)
        };
        let moved = grid_page(g(&|g| g.label_offset = 5.0));
        assert!(lowest(&moved) + 10 < lowest(&plain), "{} {}", lowest(&moved), lowest(&plain));
        // The bottom's labels turned: taller than they are wide.
        let rows = |pm: &Pixmap| {
            let px = |mm: f64| (mm * PT_PER_MM) as u32;
            (px(60.0)..px(90.0)).filter(|&y| (px(12.0)..px(88.0)).any(|x| pm.pixel(x, y).is_some_and(ink))).count()
        };
        let turned =
            grid_page(g(&|g| g.orientations = Some(EdgeOrientations { bottom: Orientation::Vertical, ..EdgeOrientations::default() })));
        assert!(rows(&turned) * 2 > rows(&plain) * 3, "{} rows against {}", rows(&turned), rows(&plain));
    }

    /// An emphasised value: the two figures before the last three large, the rest small; a short
    /// one large throughout. The labels' pieces take the decimal comma.
    #[test]
    fn emphasis_splits_a_value_as_topographic_maps_print_it() {
        let parts = |s: &str| emphasis(s).into_iter().map(|(t, k)| (t, k < 1.0)).collect::<Vec<_>>();
        let small = |t: &str| (t.to_string(), true);
        let large = |t: &str| (t.to_string(), false);
        assert_eq!(parts("543000"), [small("5"), large("43"), small("000")]);
        assert_eq!(parts("1520000mN"), [small("15"), large("20"), small("000mN")]);
        assert_eq!(parts("543,000"), [small("5"), large("43"), small(",000")]);
        assert_eq!(parts("4000"), [small(""), large("4"), small("000")]);
        assert_eq!(parts("-12500.5"), [small("-"), large("12"), small("500.5")]);
        assert_eq!(parts("43"), [large("43")]);
        let g = rustgis_map::layout::Grid { kind: GridKind::Measured, emphasis: true, ..rustgis_map::layout::Grid::default() };
        let runs = label_runs(&g, "543,000.5", true);
        assert_eq!(runs.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(), ["5", "43", ".000,5"], "empty pieces left out");
        // A graticule's labels aren't split.
        let deg = rustgis_map::layout::Grid { emphasis: true, ..rustgis_map::layout::Grid::default() };
        assert_eq!(label_runs(&deg, "12000", false).len(), 1);
    }

    /// Minor lines (ticks only) split each step; every so many lines labelled; a measured
    /// grid's corners whole, its dashed lines drawn in part, and a second grid drawn too.
    #[test]
    fn grids_take_minor_ticks_label_every_dashes_and_a_second() {
        use rustgis_map::layout::{Grid, GridDash, GridStyle};
        let tr = rustgis_data::raster::Crs::parse("EPSG:25832").unwrap().transformer().unwrap();
        let frame: Vec<Coord> =
            [(8.71, 47.49), (8.75, 47.49), (8.75, 47.51), (8.71, 47.51)].iter().map(|&(x, y)| Coord::new(x, y)).collect();
        let utm = Grid { kind: GridKind::Measured, crs: Some("EPSG:25832".into()), step: 1000.0, ..Grid::default() };
        let mut lines = Vec::new();
        measured_lines(&Grid { minor_ticks: 4, label_every: 2, ..utm.clone() }, &tr, &frame, &mut lines);
        let major = lines.iter().filter(|l| !l.minor).count();
        let minor = lines.iter().filter(|l| l.minor).count();
        assert!(minor >= major * 3, "{minor} minor lines for {major}");
        assert!(lines.iter().filter(|l| l.minor).all(|l| !l.labelled && l.label.is_empty()));
        for l in lines.iter().filter(|l| !l.minor) {
            let v: f64 = l.full.parse().unwrap();
            assert_eq!(l.labelled, ((v / 1000.0).round() as i64).rem_euclid(2) == 0, "{}", l.full);
        }
        // Whole with their unit, the others short.
        let mut topo = Vec::new();
        let g = Grid { short: true, suffix: rustgis_map::layout::GridSuffix::Metres, ..utm.clone() };
        measured_lines(&g, &tr, &frame, &mut topo);
        assert!(topo.iter().any(|l| l.lon && l.label == "79" && l.full == "479000mE"));
        assert!(topo.iter().any(|l| !l.lon && l.full == "5261000mN"));
        // A dashed or dotted line draws less of the frame than a solid one; ticks stay solid.
        let lined = |dash| {
            winterthur_page(
                Grid { color: Rgba::rgb(0xFF, 0, 0), labels: false, style: GridStyle::Lines, dash, width: 1.0, ..utm.clone() },
                rustgis_map::Projection::WebMercator,
            )
        };
        let inner = (20.0, 20.0, 80.0, 80.0);
        let (solid, dashed, dotted) = (
            count(&lined(GridDash::Solid), inner, red),
            count(&lined(GridDash::Dashed), inner, red),
            count(&lined(GridDash::Dotted), inner, red),
        );
        assert!(dashed > 0 && dashed < solid * 9 / 10, "{dashed} of {solid}");
        assert!(dotted > 0 && dotted < dashed, "{dotted} of {dashed}");
        // A second grid in blue over the first.
        let blue = |p: tiny_skia::PremultipliedColorU8| p.blue() > 180 && p.red() < 90 && p.green() < 90;
        let second =
            Grid { color: Rgba::rgb(0, 0, 0xFF), labels: false, style: GridStyle::Lines, step: 0.01, width: 1.0, ..Grid::default() };
        let both = winterthur_page(
            Grid { second: Some(Box::new(second)), ..Grid { color: Rgba::rgb(0xFF, 0, 0), labels: false, width: 1.0, ..utm.clone() } },
            rustgis_map::Projection::WebMercator,
        );
        assert!(count(&both, inner, blue) > 100 && count(&both, inner, red) > 100, "both grids drawn");
    }

    #[test]
    fn north_arrow_styles_differ_and_take_their_colours() {
        let draw = |style, fill| {
            let mut pm = Pixmap::new(80, 120).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let text = LabelText::arrow();
            let a = Arrow { style, fill, outline: Rgba::rgb(0x22, 0x22, 0x22), label: "N", text: &text };
            crate::canvas::with_canvas(&mut pm, |c| north_arrow(c, Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap(), &a, 1.0, 0.0));
            pm
        };
        let red_fill = Rgba::rgb(0xFF, 0, 0);
        let all: Vec<Pixmap> = ArrowStyle::ALL.iter().map(|&s| draw(s, red_fill)).collect();
        for (i, a) in all.iter().enumerate() {
            let filled = (0..120).flat_map(|y| (0..80).map(move |x| (x, y))).filter(|&(x, y)| a.pixel(x, y).is_some_and(red)).count();
            assert!(filled > 50, "{:?} uses the fill colour", ArrowStyle::ALL[i]);
            for b in &all[i + 1..] {
                assert_ne!(a.data(), b.data(), "each style draws differently");
            }
        }
    }

    #[test]
    fn a_north_arrow_turns_with_its_frame() {
        // The mean position of the red arrowhead (its N and outline are dark), as an offset from
        // the middle of an 80 × 120 arrow.
        let lean = |bearing| {
            let mut pm = Pixmap::new(80, 120).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let r = Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap();
            let text = LabelText::arrow();
            let a = Arrow {
                style: ArrowStyle::Triangle,
                fill: Rgba::rgb(0xFF, 0, 0),
                outline: Rgba::rgb(0x22, 0x22, 0x22),
                label: "N",
                text: &text,
            };
            crate::canvas::with_canvas(&mut pm, |c| north_arrow(c, r, &a, 1.0, bearing));
            let pts: Vec<(f64, f64)> = (0..120)
                .flat_map(|y| (0..80).map(move |x| (x, y)))
                .filter(|&(x, y)| pm.pixel(x, y).is_some_and(red))
                .map(|(x, y)| (f64::from(x), f64::from(y)))
                .collect();
            let n = pts.len().max(1) as f64;
            (pts.iter().map(|p| p.0).sum::<f64>() / n - 40.0, pts.iter().map(|p| p.1).sum::<f64>() / n - 60.0)
        };
        // North up: the arrowhead's bulk (its base) lies below the middle, its tip above.
        let (x, y) = lean(0.0);
        assert!(x.abs() < 1.0 && y > 3.0, "{x}, {y}");
        // East up (the map turned 90°): north points left, so the base lies right of the middle.
        let (x, y) = lean(90.0);
        assert!(x > 3.0 && y.abs() < 1.0, "{x}, {y}");
        let (x, _) = lean(-90.0);
        assert!(x < -3.0, "{x}");
    }

    /// A scale bar's element, changed by `f`.
    fn bar_kind(f: impl FnOnce(&mut ElementKind)) -> ElementKind {
        let mut k = ElementKind::scale_bar(1);
        f(&mut k);
        k
    }

    /// A 300 × 60 scale bar at 100 m of ground per point.
    fn bar_page(kind: &ElementKind) -> Pixmap {
        let mut pm = Pixmap::new(300, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let look = ScaleLook::of(kind).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap(), 100.0, &look, 1.0, false));
        pm
    }

    #[test]
    fn scale_bar_styles_units_and_labels_differ() {
        let plain = bar_page(&ElementKind::scale_bar(1));
        let mut seen = vec![plain.data().to_vec()];
        let mut differs = |what: &str, f: &dyn Fn(&mut ElementKind)| {
            let pm = bar_page(&bar_kind(f));
            assert!(pm.pixels().iter().any(|p| p.red() < 0x80), "{what} draws");
            assert!(!seen.iter().any(|d| d == pm.data()), "{what} draws differently");
            seen.push(pm.data().to_vec());
        };
        for style in ScaleBarStyle::ALL.into_iter().skip(1) {
            differs(style.code(), &move |k| {
                if let ElementKind::ScaleBar { style: s, .. } = k {
                    *s = style;
                }
            });
        }
        for label in [UnitLabel::Name, UnitLabel::None] {
            differs(label.code(), &move |k| {
                if let ElementKind::ScaleBar { unit_label, .. } = k {
                    *unit_label = label;
                }
            });
        }
        for place in [UnitPlace::AfterBar, UnitPlace::Above, UnitPlace::Below] {
            differs(place.code(), &move |k| {
                if let ElementKind::ScaleBar { unit_place, .. } = k {
                    *unit_place = place;
                }
            });
        }
        for numbers in [NumberPlace::Below, NumberPlace::Ends, NumberPlace::None] {
            differs(numbers.code(), &move |k| {
                if let ElementKind::ScaleBar { numbers: n, .. } = k {
                    *n = numbers;
                }
            });
        }
        differs("subdivisions", &|k| {
            if let ElementKind::ScaleBar { subdivisions, .. } = k {
                *subdivisions = 4;
            }
        });
        differs("a set division", &|k| {
            if let ElementKind::ScaleBar { division, .. } = k {
                *division = 1000.0;
            }
        });
        differs("a larger type", &|k| {
            if let ElementKind::ScaleBar { text, .. } = k {
                text.size = 12.0;
            }
        });
        differs("colours", &|k| {
            if let ElementKind::ScaleBar { color, .. } = k {
                *color = Rgba::rgb(0x00, 0x40, 0x90);
            }
        });
    }

    /// Every style on one sheet, written to `RUSTGIS_SURROUNDS_SHEET` when it is set (to look at).
    #[test]
    fn surrounds_sheet() {
        let Some(path) = std::env::var_os("RUSTGIS_SURROUNDS_SHEET") else { return };
        let mut pm = Canvas::new(1300, 900).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let ink = Rgba::rgb(0x22, 0x22, 0x22);
        for (i, style) in ArrowStyle::ALL.into_iter().enumerate() {
            let t = LabelText::arrow();
            let r = Rect::from_xywh(20.0 + i as f32 * 125.0, 10.0, 70.0, 110.0).unwrap();
            north_arrow(&mut pm, r, &Arrow { style, fill: ink, outline: ink, label: if i == 9 { "น" } else { "N" }, text: &t }, 1.5, 0.0);
        }
        let looks: Vec<ElementKind> = ScaleBarStyle::ALL
            .into_iter()
            .enumerate()
            .map(|(i, style)| {
                bar_kind(|k| {
                    if let ElementKind::ScaleBar { style: s, unit_label, unit_place, numbers, subdivisions, .. } = k {
                        *s = style;
                        *unit_label = [UnitLabel::Thai, UnitLabel::ThaiAbbreviation, UnitLabel::Name, UnitLabel::Abbreviation][i % 4];
                        *unit_place = UnitPlace::ALL[i % 4];
                        *numbers = [NumberPlace::Above, NumberPlace::Below, NumberPlace::Ends, NumberPlace::Above][i % 4];
                        *subdivisions = if i == 0 { 4 } else { 0 };
                    }
                })
            })
            .collect();
        for (i, k) in looks.iter().enumerate() {
            let (col, row) = (i % 2, i / 2);
            let r = Rect::from_xywh(20.0 + col as f32 * 640.0, 150.0 + row as f32 * 180.0, 600.0, 150.0).unwrap();
            scale_bar(&mut pm, r, 50.0, &ScaleLook::of(k).unwrap(), 1.5, false);
        }
        pm.pixmap().save_png(&path).unwrap();
        // Legends: as they were, then in other type, patches and texts.
        let map = legend_map();
        let mut sheet = Canvas::new(1300, 420).unwrap();
        sheet.fill(tiny_skia::Color::WHITE);
        let mut looks = vec![LegendLook::default()];
        let mut thai =
            LegendLook { line_patch: LinePatch::Curve, area_patch: AreaPatch::Natural, patch_width: 28.0, ..LegendLook::default() };
        thai.set_name("Towns", "ชุมชน");
        thai.set_label("Towns", "a", "เมืองหลัก");
        thai.set_name("Lakes", "แหล่งน้ำ");
        thai.title.color = Some(Rgba::rgb(0x1F, 0x4E, 0x8C));
        thai.title.size = 16.0;
        looks.push(thai);
        let mut serif =
            LegendLook { area_patch: AreaPatch::Rounded, line_patch: LinePatch::Straight, spacing: 4.0, ..LegendLook::default() };
        serif.label.font = FontFamily::Serif;
        serif.label.italic = true;
        serif.layer.font = FontFamily::Serif;
        looks.push(serif);
        for (i, look) in looks.iter().enumerate() {
            let opts = LegendOptions { hide_other: true, exclude: &[], scale: 1e6, look };
            let r = Rect::from_xywh(20.0 + i as f32 * 430.0, 20.0, 400.0, 390.0).unwrap();
            legend(&mut sheet, r, &map, ("Legend คำอธิบาย", 1), None, opts, 2.0);
        }
        let mut legends = std::path::PathBuf::from(path);
        legends.set_extension("legends.png");
        sheet.pixmap().save_png(legends).unwrap();
    }

    #[test]
    fn scale_units_are_named_in_english_or_thai() {
        assert_eq!(UnitLabel::Abbreviation.name("km", ""), "km");
        assert_eq!(UnitLabel::Name.name("m", ""), "Meters");
        assert_eq!(UnitLabel::ThaiAbbreviation.name("km", ""), "กม.");
        assert_eq!(UnitLabel::Thai.name("km", ""), "กิโลเมตร");
        assert_eq!(UnitLabel::Thai.name("NM", ""), "ไมล์ทะเล");
        assert_eq!(UnitLabel::Custom.name("km", "ก.ม."), "ก.ม.");
        assert_eq!(UnitLabel::None.name("km", ""), "");
        // A set division counts in the units, or in metres when they are automatic.
        assert_eq!(scale_length(ScaleUnits::Kilometers, 4, 2.5, 1.0), (10.0, 1000.0, "km"));
        assert_eq!(scale_length(ScaleUnits::Auto, 5, 500.0, 1.0), (2.5, 1000.0, "km"));
        assert_eq!(scale_length(ScaleUnits::Auto, 4, 50.0, 1.0), (200.0, 1.0, "m"));
    }

    #[test]
    fn north_arrows_take_a_typed_letter_or_none() {
        let draw = |label: &str, text: &LabelText| {
            let mut pm = Pixmap::new(80, 120).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let ink = Rgba::rgb(0x22, 0x22, 0x22);
            let a = Arrow { style: ArrowStyle::Triangle, fill: ink, outline: ink, label, text };
            crate::canvas::with_canvas(&mut pm, |c| north_arrow(c, Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap(), &a, 1.0, 0.0));
            pm
        };
        let t = LabelText::arrow();
        let (n, none, word) = (draw("N", &t), draw("", &t), draw("North", &t));
        assert_ne!(n.data(), none.data());
        assert_ne!(n.data(), word.data());
        let red = LabelText { color: Some(Rgba::rgb(0xFF, 0, 0)), size: 20.0, ..LabelText::arrow() };
        let pm = draw("N", &red);
        assert!(pm.pixels().iter().any(|p| p.red() > 0xE0 && p.green() < 0x30), "the letter takes its colour");
        for style in ArrowStyle::ALL {
            let sw = north_arrow_swatch(style, 40, 56).unwrap();
            assert!(sw.pixels().iter().any(|p| p.alpha() > 0), "{style:?} has a swatch");
        }
        for style in ScaleBarStyle::ALL {
            let sw = scale_bar_swatch(style, 80, 20).unwrap();
            assert!(sw.pixels().iter().any(|p| p.alpha() > 0), "{style:?} has a swatch");
        }
    }

    #[test]
    fn scale_bars_count_in_their_units_and_divisions() {
        let draw_in = |(units, divisions, style): (ScaleUnits, usize, ScaleBarStyle), (color, fill): (Rgba, Rgba)| {
            let mut pm = Pixmap::new(300, 60).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            // 100 m of ground per point: about 25 km across the bar.
            let kind = bar_kind(|k| {
                if let ElementKind::ScaleBar { units: u, divisions: d, style: s, color: c, fill: f, .. } = k {
                    (*u, *d, *s, *c, *f) = (units, divisions, style, color, fill);
                }
            });
            let look = ScaleLook::of(&kind).unwrap();
            let r = Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap();
            crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 100.0, &look, 1.0, false));
            pm
        };
        let draw = |bar| draw_in(bar, (Rgba::rgb(0x22, 0x22, 0x22), Rgba::rgb(0xFF, 0xFF, 0xFF)));
        // Dark runs along the bar's middle row: one per dark division of an alternating bar.
        let runs = |pm: &Pixmap| {
            let y = (60.0 * 0.62) as u32;
            let dark: Vec<bool> = (0..pm.width()).map(|x| pm.pixel(x, y).is_some_and(ink)).collect();
            dark.windows(2).filter(|w| !w[0] && w[1]).count() + usize::from(dark.first() == Some(&true))
        };
        let four = draw((ScaleUnits::Auto, 4, ScaleBarStyle::Alternating));
        let five = draw((ScaleUnits::Auto, 5, ScaleBarStyle::Alternating));
        assert_eq!(runs(&four), 2, "two dark divisions");
        assert_eq!(runs(&five), 3, "three dark divisions, the last at the end");
        // Miles and kilometres give bars of different lengths and labels.
        assert_ne!(draw((ScaleUnits::Miles, 4, ScaleBarStyle::Alternating)).data(), four.data());
        // A line or a stepped line fills no division.
        for style in [ScaleBarStyle::Line, ScaleBarStyle::Stepped] {
            let pm = draw((ScaleUnits::Auto, 4, style));
            let dark = (0..300).filter(|&x| pm.pixel(x, 37).is_some_and(ink)).count();
            assert!(dark < 20, "{style:?}: {dark} dark pixels across the bar");
            assert_ne!(pm.data(), four.data());
        }
    }

    #[test]
    fn scale_bars_draw_in_their_colours() {
        let mut pm = Pixmap::new(300, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        type Px = tiny_skia::PremultipliedColorU8;
        // A red colour and a blue fill.
        let kind = bar_kind(|k| {
            if let ElementKind::ScaleBar { color, fill, .. } = k {
                (*color, *fill) = (Rgba::rgb(0xD0, 0x10, 0x10), Rgba::rgb(0x10, 0x10, 0xD0));
            }
        });
        let look = ScaleLook::of(&kind).unwrap();
        let r = Rect::from_xywh(0.0, 0.0, 300.0, 60.0).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| scale_bar(c, r, 100.0, &look, 1.0, false));
        let count = |hit: fn(Px) -> bool, y: u32| (0..300).filter(|&x| pm.pixel(x, y).is_some_and(hit)).count();
        let y = (60.0 * 0.62) as u32;
        // Along the bar: the dark divisions in the colour, the light ones in the fill, nothing in
        // the old ink.
        assert!(count(red, y) > 40, "red divisions");
        assert!(count(|p| p.blue() > 180 && p.red() < 90 && p.green() < 90, y) > 40, "blue divisions");
        assert_eq!(count(ink, y), 0, "no default ink left");
        // The labels above the bar are in the colour too.
        let labels = (0..y.saturating_sub(8)).map(|row| count(|p| p.red() > 120 && p.green() < 90 && p.blue() < 90, row)).sum::<usize>();
        assert!(labels > 0, "red labels");
        assert_eq!((0..y.saturating_sub(8)).map(|row| count(ink, row)).sum::<usize>(), 0, "no ink labels");
    }

    /// A page draws a scale bar in its own colour and fill: the element's, through `draw_page`.
    #[test]
    fn a_page_draws_its_scale_bar_in_its_colours() {
        let mut maps = vec![Map::new("m")];
        let mut l = Layout::new("Layout", 100.0, 60.0);
        let frame = l.add(
            "Map",
            (5.0, 35.0, 90.0, 20.0),
            ElementKind::MapFrame {
                map: "m".into(),
                center: Coord::new(0.0, 0.0),
                scale: 10_000_000.0,
                shape: FrameShape::Rectangle,
                grid: None,
                extent_of: None,
                extent_color: rustgis_map::layout::INDICATOR_COLOR,
                extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
        let (crimson, navy) = (Rgba::rgb(0xD0, 0x10, 0x10), Rgba::rgb(0x10, 0x10, 0xD0));
        let bar = ElementKind::ScaleBar {
            frame,
            units: ScaleUnits::Auto,
            divisions: 4,
            style: ScaleBarStyle::Alternating,
            subdivisions: 0,
            division: 0.0,
            unit_label: UnitLabel::default(),
            unit_text: String::new(),
            unit_place: UnitPlace::default(),
            numbers: NumberPlace::default(),
            color: crimson,
            fill: navy,
            bar_height: 0.0,
            text: LabelText::scale_bar(),
        };
        l.add("Scale Bar", (5.0, 5.0, 90.0, 20.0), bar);
        let pm = render(&l, &mut maps, 96.0, &PageInfo::default()).unwrap();
        // Above the frame (30 mm of the page's 60): the bar's divisions in its colour and fill, and
        // nothing in the default ink.
        let above = pm.height() / 2;
        let count = |hit: fn(tiny_skia::PremultipliedColorU8) -> bool| {
            (0..above).flat_map(|y| (0..pm.width()).map(move |x| (x, y))).filter(|&(x, y)| pm.pixel(x, y).is_some_and(hit)).count()
        };
        assert!(count(red) > 100, "red divisions and labels");
        assert!(count(|p| p.blue() > 180 && p.red() < 90 && p.green() < 90) > 100, "blue divisions");
        assert_eq!(count(ink), 0, "no default ink");
    }

    #[test]
    fn scale_numbers_are_short() {
        assert_eq!(scale_number(5.0, 1.25), "5");
        assert_eq!(scale_number(2.5, 1.25), "2.5");
        assert_eq!(scale_number(1.25, 1.25), "1.25");
        assert_eq!(scale_number(0.0, 1.25), "0");
        assert_eq!(scale_number(100.0, 50.0), "100");
        // Small totals keep their precision: 0.05 km in four divisions.
        assert_eq!(scale_number(0.0125, 0.0125), "0.0125");
        assert_eq!(scale_number(0.0375, 0.0125), "0.0375");
        assert_eq!(scale_number(0.125, 0.125), "0.125");
        // A step that never comes out even keeps two significant digits of it.
        assert_eq!(scale_number(1.0 / 3.0, 1.0 / 3.0), "0.33");
        assert_eq!(scale_number(2.0 / 3.0, 1.0 / 3.0), "0.67");
        assert_eq!(scale_number(1.0, 1.0 / 3.0), "1");
        assert_eq!(scale_number(20.0 / 3.0, 10.0 / 3.0), "6.7");
        assert_eq!(scale_number(200.0 / 3.0, 100.0 / 3.0), "67");
        assert_eq!(scale_number(0.01 / 3.0, 0.01 / 3.0), "0.0033");
    }

    /// Graticule labels: meridians along the top and bottom and parallels along the sides come
    /// first; a slanting meridian leaving through a side is labelled there only clear of the
    /// parallels' labels, and none lands by a corner.
    #[test]
    fn graticule_labels_keep_clear_of_each_other() {
        let along = |_: Edge, s: &str| s.len() as f32 * 5.0;
        let label = |edge, at: f32, s: &str, lon| (edge, at, s.to_string(), lon);
        let labels = vec![
            // A meridian leaving through the right edge just by a parallel's label (put first, so
            // only the order fixes it), and another well clear of any.
            label(Edge::Right, 205.0, "36°W", true),
            label(Edge::Right, 200.0, "6°S", false),
            label(Edge::Right, 400.0, "34°W", true),
            label(Edge::Top, 150.0, "42°W", true),
            // A parallel curving out through the top near a meridian's label: clear of it, but
            // too close to read as its own.
            label(Edge::Top, 185.0, "2°S", false),
            // By a corner: left out, and a side's label within two label heights of the top.
            label(Edge::Top, 4.0, "44°W", true),
            label(Edge::Right, 15.0, "2°S", false),
        ];
        let kept: Vec<String> = kept_labels(labels, (600.0, 500.0), along, 10.0, 1.0).into_iter().map(|(.., s)| s).collect();
        assert_eq!(kept, ["6°S", "42°W", "34°W"]);
    }

    /// A layout with a decimal comma swaps the marks of the numbers it prints, `{scale}` included,
    /// and leaves other text alone.
    #[test]
    fn a_decimal_comma_swaps_the_marks() {
        assert_eq!(decimal("0.05".into(), true), "0,05");
        assert_eq!(decimal("6,300,000".into(), true), "6.300.000");
        assert_eq!(decimal("0.05".into(), false), "0.05");
        let map = Map::new("m");
        let cam = frame_camera(&map, Coord::new(0.0, 0.0), 6_300_000.0, 0.0, 0.0);
        let info = PageInfo { page: 1, pages: 1, page_name: String::new() };
        let mut layout = Layout::new("L", 100.0, 100.0);
        let point = expand("Scale {scale}, page {page}.", &layout, &info, Some((&map, cam)));
        layout.decimal_comma = true;
        let comma = expand("Scale {scale}, page {page}.", &layout, &info, Some((&map, cam)));
        // "Scale 1:6,300,000, page 1." and "Scale 1:6.300.000, page 1.": the scale's marks swap, the
        // text's comma stays.
        let scale = point.strip_prefix("Scale ").and_then(|t| t.strip_suffix(", page 1.")).unwrap_or_else(|| panic!("{point}"));
        assert!(scale.starts_with("1:") && scale.contains(','), "{point}");
        assert_eq!(comma, format!("Scale {}, page 1.", scale.replace(',', ".")));
    }

    /// A turned element is drawn turned about its centre, on the page and on its own.
    #[test]
    fn turned_elements_are_drawn_turned() {
        let info = PageInfo { page: 1, pages: 1, page_name: String::new() };
        let mut layout = Layout::new("T", 100.0, 100.0);
        let black = Rgba::rgb(0, 0, 0);
        let id =
            layout.add("Bar", (20.0, 45.0, 60.0, 10.0), ElementKind::Rectangle { fill: black, outline: black, width: 0.0, radius: 0.0 });
        let dark = |pm: &Pixmap, x: f64, y: f64| {
            let k = f64::from(pm.width()) / 100.0;
            pm.pixel((x * k) as u32, (y * k) as u32).is_some_and(|p| p.red() < 64)
        };
        let pm = render(&layout, &mut [], 96.0, &info).unwrap();
        assert!(dark(&pm, 25.0, 50.0) && !dark(&pm, 50.0, 25.0), "level");
        layout.element_mut(id).unwrap().rotation = 90.0;
        let pm = render(&layout, &mut [], 96.0, &info).unwrap();
        assert!(!dark(&pm, 25.0, 50.0) && dark(&pm, 50.0, 25.0) && dark(&pm, 50.0, 75.0), "upright");
        // At 45° the bar runs from bottom-left to top-right.
        layout.element_mut(id).unwrap().rotation = 45.0;
        let pm = render(&layout, &mut [], 96.0, &info).unwrap();
        assert!(dark(&pm, 35.0, 65.0) && dark(&pm, 65.0, 35.0) && !dark(&pm, 35.0, 35.0));
        // On its own: the picture covers its turned box.
        let (_, [x, y, w, h]) = render_element(&layout, id, &mut [], 96.0, &info).unwrap().unwrap();
        let (bx, by, bw, bh) = layout.element(id).unwrap().bounds();
        assert!(x <= bx + 0.5 && y <= by + 0.5 && x + w >= bx + bw - 0.5 && y + h >= by + bh - 0.5, "{:?}", (x, y, w, h));
    }

    /// A turned element recorded for a vector PDF or SVG stays vectors: its text as glyphs and its
    /// shapes as paths, in a group turned about its centre, not a picture of it.
    #[test]
    fn a_turned_element_stays_vectors_when_recorded() {
        use crate::vector::Item;
        let mut layout = Layout::new("T", 100.0, 100.0);
        let black = Rgba::rgb(0, 0, 0);
        let bar =
            layout.add("Bar", (20.0, 45.0, 60.0, 10.0), ElementKind::Rectangle { fill: black, outline: black, width: 0.0, radius: 0.0 });
        let title = layout.add("Title", (20.0, 10.0, 60.0, 12.0), ElementKind::text("แผนที่ Map", 14.0, black, false));
        for id in [bar, title] {
            layout.element_mut(id).unwrap().rotation = 30.0;
        }
        let (drawing, _) = render_drawing(&layout, &mut [], 96.0, &PageInfo::default()).unwrap();
        let (_, _, items) = drawing.finish();
        let groups: Vec<&crate::vector::Group> = items.iter().filter_map(|i| if let Item::Group(g) = i { Some(g) } else { None }).collect();
        assert_eq!(groups.len(), 2, "a group for each turned element, of {} items", items.len());
        for g in &groups {
            assert!(g.transform.kx.abs() > 0.3 && g.transform.ky.abs() > 0.3, "turned: {:?}", g.transform);
            assert!(!g.items.iter().any(|i| matches!(i, Item::Image(_))), "no picture");
        }
        assert!(groups.iter().any(|g| g.items.iter().any(|i| matches!(i, Item::Fill { .. }))), "the bar as a path");
        assert!(groups.iter().any(|g| g.items.iter().any(|i| matches!(i, Item::Glyphs(_)))), "the title as glyphs");
    }

    /// A legend's columns take its look: the room between its rows, its patches' width and its
    /// type count in each column's size, and a look's legend in columns draws as sized.
    #[test]
    fn a_legends_columns_take_its_look() {
        let mut map = legend_map();
        let mm = 25.4 / 96.0;
        let plain_sizes = legend_sizes_mm(&mut map, 1e6, true, true, &[], plain()).unwrap();
        let look = LegendLook { spacing: 4.0, patch_width: 40.0, ..LegendLook::default() };
        let sizes = legend_sizes_mm(&mut map, 1e6, true, true, &[], &look).unwrap();
        // Six rows: five gaps in one column; in two, three rows each (a name kept with its class).
        assert!((sizes[0].1 - (6.0 * f64::from(LEGEND_ROW) + 5.0 * 4.0) * mm).abs() < 1e-4, "{sizes:?}");
        assert!(sizes[1].1 < sizes[0].1 && sizes[1].0 > sizes[0].0, "{sizes:?}");
        for (k, (s, p)) in sizes.iter().zip(&plain_sizes).enumerate() {
            assert!(s.0 > p.0 + 20.0 * mm, "column count {}: wider patches widen it: {s:?} of {p:?}", k + 1);
        }
        // Drawn in two columns, in the box it was sized to: every row drawn, none past the box.
        let pieces = legend_pieces_mm(&mut map, 1e6, ("", 2), true, true, &[], &look, (sizes[1].0 + 0.5, sizes[1].1 + 0.5)).unwrap();
        let labels = pieces.iter().filter(|p| matches!(p, LegendPiece::Text { .. })).count();
        assert_eq!(labels, 6, "{pieces:?}");
        let xs: Vec<f64> = pieces.iter().filter_map(|p| if let LegendPiece::Patch { x, .. } = p { Some(*x) } else { None }).collect();
        assert!(xs.iter().any(|x| *x > sizes[0].0 / 2.0), "a second column: {xs:?}");
    }

    /// A map of one point layer, one point, set by `edit`.
    fn point_legend_map(edit: impl FnOnce(&mut rustgis_map::Layer)) -> Map {
        use rustgis_data::{Dataset, Geometry};
        let mut map = Map::new("m");
        let mut d = Dataset::new("pts", GeometryType::Point, vec![]);
        d.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        map.add_dataset(d);
        if let Some(l) = map.layers.last_mut() {
            edit(l);
        }
        map
    }

    /// The legend of `map` at 1:1e6, every class, on white.
    fn point_legend(map: &Map) -> Pixmap {
        let mut pm = Pixmap::new(200, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() };
        crate::canvas::with_canvas(&mut pm, |c| legend(c, Rect::from_xywh(0.0, 0.0, 200.0, 200.0).unwrap(), map, ("", 1), None, opts, 1.0));
        pm
    }

    /// The legend of `map` at 1:1e6 recorded for a vector export, on a page of `side` pixels, drawn
    /// back on white.
    fn recorded_legend(map: &Map, side: u32) -> Pixmap {
        let mut c = Canvas::recording(side, side).unwrap();
        let opts = LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() };
        legend(&mut c, Rect::from_xywh(0.0, 0.0, side as f32, side as f32).unwrap(), map, ("", 1), None, opts, 1.0);
        let (_, _, items) = c.into_drawing().unwrap().finish();
        let mut pm = Pixmap::new(side, side).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::vector::replay(&items, &mut pm, Transform::identity());
        pm
    }

    /// What the map's code draws for a point at (`x`, `y`) on white.
    fn drawn_point(s: &rustgis_map::Symbol, strokes: &[rustgis_map::LineStroke], (x, y): (f32, f32), ppp: f32) -> Pixmap {
        let mut pm = Pixmap::new(200, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::canvas::with_canvas(&mut pm, |c| {
            if strokes.is_empty() {
                crate::draw_marker(c, x, y, s, 1.0, None, ppp);
            } else {
                point_marks::draw(c, x, y, s, strokes, 0.0, None, ppp);
            }
        });
        pm
    }

    /// What the map's code draws for a point at (`x`, `y`), composited as one at `opacity`, on
    /// white.
    fn drawn_see_through(
        s: &rustgis_map::Symbol,
        strokes: &[rustgis_map::LineStroke],
        (x, y): (f32, f32),
        ppp: f32,
        opacity: f32,
    ) -> Pixmap {
        let mut own = Pixmap::new(200, 200).unwrap();
        crate::canvas::with_canvas(&mut own, |c| draw_point(c, (x, y), s, strokes, ppp));
        let mut pm = Pixmap::new(200, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        pm.draw_pixmap(0, 0, own.as_ref(), &PixmapPaint { opacity, ..PixmapPaint::default() }, Transform::identity(), None);
        pm
    }

    /// Whether two pictures differ by no more than a rounding over `w` × `h` from the top left
    /// corner.
    fn near_corner(a: &Pixmap, b: &Pixmap, w: u32, h: u32) -> bool {
        (0..h).all(|y| {
            (0..w).all(|x| match (a.pixel(x, y), b.pixel(x, y)) {
                (Some(p), Some(q)) => p.red().abs_diff(q.red()).max(p.green().abs_diff(q.green())).max(p.blue().abs_diff(q.blue())) <= 2,
                _ => false,
            })
        })
    }

    /// Whether two pictures are the same over `w` × `h` from the top left corner.
    fn same_corner(a: &Pixmap, b: &Pixmap, w: u32, h: u32) -> bool {
        (0..h).all(|y| (0..w).all(|x| a.pixel(x, y) == b.pixel(x, y)))
    }

    /// A stacked stroke: a square of `size` points in `color`, with no outline.
    fn square_mark(color: Rgba, size: f32) -> rustgis_map::LineStroke {
        let markers = rustgis_map::LineMarkers {
            mark: rustgis_map::LineMark::Marker(rustgis_map::Marker::Square),
            size,
            ..rustgis_map::LineMarkers::default()
        };
        rustgis_map::LineStroke { color: Some(color), width: Some(0.0), markers: Some(markers), ..rustgis_map::LineStroke::default() }
    }

    /// Every marker, tiny and ordinary, with an outline and without, at the pixels the map's code
    /// gives it.
    #[test]
    fn a_legend_draws_a_point_as_the_map_does() {
        let red = Rgba::rgb(255, 0, 0);
        let base =
            rustgis_map::Symbol { fill: red, stroke: Rgba::rgb(0, 0, 255), ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        // Every shape, tiny and ordinary, with no outline and with one.
        for marker in rustgis_map::Marker::ALL {
            for (size, stroke_width) in [(2.0, 0.0), (10.0, 0.0), (10.0, 1.0)] {
                let s = rustgis_map::Symbol { marker, size, stroke_width, ..base };
                let map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s));
                // The first row, 15 points tall, its swatch 16 wide.
                let want = drawn_point(&s, &[], (8.0, 7.5), 1.0);
                assert!(same_corner(&point_legend(&map), &want, 16, 15), "{marker:?} of {size} pt, outline {stroke_width} pt");
            }
        }
        // A class's symbol too, indented under its layer's name.
        let s = rustgis_map::Symbol { marker: rustgis_map::Marker::Triangle, size: 10.0, stroke_width: 0.0, ..base };
        let map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Unique {
                field: "g".into(),
                classes: vec![("a".to_string(), s)],
                other: s,
                ramp: rustgis_map::Ramp::Ember,
                class_labels: Default::default(),
                other_label: None,
                class_strokes: Default::default(),
                other_strokes: Vec::new(),
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: None,
            };
        });
        let got = point_legend(&map);
        let want = drawn_point(&s, &[], (6.0 + 8.0, 15.0 + 7.5), 1.0);
        assert!((15..30).all(|y| (6..22).all(|x| got.pixel(x, y) == want.pixel(x, y))), "the class's triangle");
    }

    /// An outline under half a pixel is as thin in a legend as on the map.
    #[test]
    fn a_legend_draws_a_hairline_outline_as_the_map_does() {
        // Thinner than half a pixel: the map's marker strokes it at its width, with no minimum.
        let s = rustgis_map::Symbol {
            size: 10.0,
            stroke: Rgba::rgb(0, 0, 255),
            stroke_width: 0.1,
            ..rustgis_map::Symbol::default_for(GeometryType::Point, 0)
        };
        let map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s));
        assert!(same_corner(&point_legend(&map), &drawn_point(&s, &[], (8.0, 7.5), 1.0), 16, 15));
    }

    /// A layer's stacked marks replace its marker, in a row that holds them.
    #[test]
    fn a_legend_draws_a_points_stacked_marks() {
        let strokes = vec![square_mark(Rgba::rgb(255, 0, 0), 12.0), square_mark(Rgba::rgb(0, 0, 0), 4.0)];
        let s = rustgis_map::Symbol::default_for(GeometryType::Point, 0);
        let map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Single(s);
            l.line_strokes = strokes.clone();
        });
        // 12 pt squares reach 7.2 pt: the row is 15 points tall still.
        assert!(same_corner(&point_legend(&map), &drawn_point(&s, &strokes, (8.0, 7.5), 1.0), 16, 15));
    }

    /// Stacked marks at the opacity of a layer and its groups, on pixels and in a recording.
    #[test]
    fn a_see_through_point_is_composited_as_one_with_its_groups() {
        let s = rustgis_map::Symbol { size: 10.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        let strokes = vec![square_mark(Rgba::rgb(255, 0, 0), 12.0), square_mark(Rgba::rgb(0, 0, 0), 12.0)];
        let mut map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Single(s);
            l.line_strokes = strokes.clone();
            l.transparency = 50.0;
        });
        let middle = |map: &Map| point_legend(map).pixel(8, 7).map(|p| (p.red(), p.green(), p.blue())).unwrap();
        // Black at half over white: grey, with none of the red under it showing.
        let (r, g, b) = middle(&map);
        assert!((120..=135).contains(&r) && r == g && g == b, "{r} {g} {b}");
        // Its group's transparency applies too: half of half.
        let id = map.layers[0].id;
        let mut group = rustgis_map::LayerGroup::new("g");
        group.transparency = 50.0;
        group.children.push(id);
        map.add_group(group);
        let (r, g, b) = middle(&map);
        assert!((185..=197).contains(&r) && r == g && g == b, "{r} {g} {b}");
        // Recorded for a vector export, it is half of half too.
        let p = recorded_legend(&map, 200).pixel(8, 7).unwrap();
        let (r, g, b) = (p.red(), p.green(), p.blue());
        assert!((185..=197).contains(&r) && r == g && g == b, "recorded: {r} {g} {b}");
    }

    /// A square turned by its own angle reaches past 0.6 of its size: its row holds its corners,
    /// which the picture of a see-through symbol would cut.
    #[test]
    fn a_see_through_mark_with_its_own_angle_keeps_its_corners() {
        let s = rustgis_map::Symbol { size: 10.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        let mut mark = square_mark(Rgba::rgb(0, 0, 255), 40.0);
        if let Some(m) = &mut mark.markers {
            m.angle = 45.0;
        }
        let strokes = vec![mark];
        let map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Single(s);
            l.line_strokes = strokes.clone();
            l.transparency = 50.0;
        });
        let side = 2.0 * point_marks::reach(&s, &strokes);
        assert!(side > 50.0, "{side}");
        let drawn = drawn_see_through(&s, &strokes, (side / 2.0, side / 2.0), 1.0, 0.5);
        assert!(near_corner(&point_legend(&map), &drawn, side.ceil() as u32, side.ceil() as u32));
    }

    /// A plain point at half its opacity, its outline over its fill: composited as one, so the fill
    /// doesn't show through the outline.
    #[test]
    fn a_see_through_plain_point_is_composited_as_the_map_composites_a_layer() {
        let s = rustgis_map::Symbol {
            fill: Rgba::rgb(255, 0, 0),
            stroke: Rgba::rgb(0, 0, 255),
            stroke_width: 1.0,
            size: 8.0,
            marker: rustgis_map::Marker::Square,
        };
        let map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Single(s);
            l.transparency = 50.0;
        });
        assert!(near_corner(&point_legend(&map), &drawn_see_through(&s, &[], (8.0, 7.5), 1.0, 0.5), 16, 15));
    }

    /// A large symbol takes a taller row and a wider column in a legend's size; an ordinary one
    /// doesn't.
    #[test]
    fn a_legend_grows_to_hold_a_large_point() {
        let mm = 25.4 / 96.0;
        let s = |size| rustgis_map::Symbol { size, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        let label = f64::from(6.0 + text::measure("pts", 9.0, false));
        // An ordinary symbol keeps the row and the swatch's column.
        let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s(10.0)));
        let [(w, h), ..] = legend_sizes_mm(&mut map, 1e6, true, false, &[], plain()).unwrap();
        assert!((h - f64::from(LEGEND_ROW) * mm).abs() < 1e-6, "{h} mm");
        // Its width within a rounding: it is summed in f32, past a name as wide as its font makes it.
        assert!((w - (f64::from(LEGEND_SWATCH) + label) * mm).abs() < 1e-4, "{w} mm");
        // One of 40 pt reaches 24 pt from its point: 48 pt each way.
        let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s(40.0)));
        let [(w, h), ..] = legend_sizes_mm(&mut map, 1e6, true, false, &[], plain()).unwrap();
        assert!((h - 48.0 * mm).abs() < 1e-6, "{h} mm");
        assert!((w - (48.0 + label) * mm).abs() < 1e-4, "{w} mm");
        assert!(same_corner(&point_legend(&map), &drawn_point(&s(40.0), &[], (24.0, 24.0), 1.0), 48, 48));
        // A frame too short for the row draws no part of it.
        let mut pm = Pixmap::new(200, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let blank = pm.clone();
        let opts = LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() };
        crate::canvas::with_canvas(&mut pm, |c| legend(c, Rect::from_xywh(0.0, 0.0, 200.0, 47.0).unwrap(), &map, ("", 1), None, opts, 1.0));
        assert!(pm.data() == blank.data());
        // A size no page could hold stays a size a legend can take.
        let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s(f32::MAX)));
        let [(w, h), ..] = legend_sizes_mm(&mut map, 1e6, true, false, &[], plain()).unwrap();
        assert!(w.is_finite() && h.is_finite(), "{w} × {h} mm");
    }

    /// A layer's reference scale sizes its symbol at the frame's scale.
    #[test]
    fn a_legend_scales_a_point_with_its_reference_scale() {
        let s = rustgis_map::Symbol { size: 10.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        // Twice as large at 1:1e6: 10 pt reaches 12 pt from its point, in a row of 24.
        let map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Single(s);
            l.reference_scale = Some(2e6);
        });
        assert!(same_corner(&point_legend(&map), &drawn_point(&s, &[], (12.0, 12.0), 2.0), 24, 24));
    }

    /// A diagram's categories on a point layer with stacked marks: each row shows its own colour,
    /// not the layer's marks.
    #[test]
    fn a_diagrams_rows_keep_their_own_mark_on_a_layer_with_stacked_marks() {
        let (red, green) = (Rgba::rgb(255, 0, 0), Rgba::rgb(0, 255, 0));
        let category = |field: &str, colour| rustgis_map::DiagramCategory { field: field.into(), colour };
        let map = point_legend_map(|l| {
            l.line_strokes = vec![square_mark(Rgba::rgb(0, 0, 0), 12.0)];
            l.diagram = Some(rustgis_map::DiagramSettings {
                kind: rustgis_map::DiagramKind::Pie,
                categories: vec![category("a", red), category("b", green)],
                size: rustgis_map::DiagramSize::Fixed(12.0),
                start_angle: 0.0,
                background: None,
                stroke: rustgis_map::DiagramSettings::default_stroke(),
                stroke_width: 0.75,
                labels: rustgis_map::DiagramLabel::None,
            });
        });
        let pm = point_legend(&map);
        let at = |x, y| pm.pixel(x, y).map(|p| (p.red(), p.green(), p.blue()));
        // The layer's name, then a row each, the swatch 6 points in.
        assert_eq!(at(14, 22), Some((255, 0, 0)));
        assert_eq!(at(14, 37), Some((0, 255, 0)));
    }

    /// A box off the pixel grid: a recording cuts its symbol at the box, not at the whole pixels
    /// round it, so the pixel its edge crosses is half covered.
    #[test]
    fn a_recorded_point_is_cut_at_its_box() {
        let s = rustgis_map::Symbol {
            fill: Rgba::rgb(0, 0, 255),
            size: 3000.0,
            stroke_width: 0.0,
            marker: rustgis_map::Marker::Square,
            ..rustgis_map::Symbol::default_for(GeometryType::Point, 0)
        };
        let map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(s));
        let mut c = Canvas::recording(2100, 2100).unwrap();
        let opts = LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() };
        legend(&mut c, Rect::from_xywh(0.5, 0.5, 2099.0, 2099.0).unwrap(), &map, ("", 1), None, opts, 1.0);
        let (_, _, items) = c.into_drawing().unwrap().finish();
        let mut pm = Pixmap::new(2100, 2100).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        crate::vector::replay(&items, &mut pm, Transform::identity());
        // The box runs from 0.5 to 2048.5.
        let red = |x, y| pm.pixel(x, y).map_or(0, |p| p.red());
        assert_eq!(red(10, 10), 0);
        assert!((60..200).contains(&red(2048, 10)), "{}", red(2048, 10));
        assert!((60..200).contains(&red(10, 2048)), "{}", red(10, 2048));
    }

    /// An upright square's and triangle's corners lie further than 0.6 of their size from their
    /// centre, but along each axis, which is what the box holds, they stay inside it.
    #[test]
    fn an_upright_mark_stays_in_its_row() {
        let s = rustgis_map::Symbol { size: 10.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        for marker in [rustgis_map::Marker::Square, rustgis_map::Marker::Triangle] {
            let mut mark = square_mark(Rgba::rgb(0, 0, 255), 40.0);
            if let Some(m) = &mut mark.markers {
                m.mark = rustgis_map::LineMark::Marker(marker);
            }
            let map = point_legend_map(|l| {
                l.renderer = rustgis_map::Renderer::Single(s);
                l.line_strokes = vec![mark];
            });
            let pm = point_legend(&map);
            let white = |x, y| pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == (255, 255, 255));
            // A box of 48 points: its edges are clear, and the mark is in it.
            assert!((0..48).all(|i| white(i, 0) && white(i, 47) && white(0, i) && white(47, i)), "{marker:?}");
            assert!(!white(24, 24), "{marker:?}");
        }
    }

    /// A class's row draws the marks the map gives its features, else its layer's.
    #[test]
    fn a_legend_draws_a_class_with_its_own_marks() {
        let s = rustgis_map::Symbol::default_for(GeometryType::Point, 0);
        let (own, others, layers) = (
            vec![square_mark(Rgba::rgb(255, 0, 0), 12.0)],
            vec![square_mark(Rgba::rgb(0, 255, 0), 12.0)],
            vec![square_mark(Rgba::rgb(0, 0, 255), 12.0)],
        );
        let map = point_legend_map(|l| {
            l.line_strokes = layers.clone();
            l.renderer = rustgis_map::Renderer::Unique {
                field: "g".into(),
                classes: vec![("a".to_string(), s), ("b".to_string(), s)],
                other: s,
                ramp: rustgis_map::Ramp::Ember,
                class_labels: Default::default(),
                other_label: None,
                class_strokes: [("a".to_string(), own.clone())].into_iter().collect(),
                other_strokes: others.clone(),
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: None,
            };
        });
        let got = point_legend(&map);
        // Under the layer's name, each row 15 points tall, its swatch 6 in: class a has its own,
        // class b takes the layer's, and the fallback has its own.
        for (row, strokes) in [(1u32, &own), (2, &layers), (3, &others)] {
            let top = row * 15;
            let want = drawn_point(&s, strokes, (14.0, top as f32 + 7.5), 1.0);
            assert!((top..top + 15).all(|y| (6..22).all(|x| got.pixel(x, y) == want.pixel(x, y))), "row {row}");
        }
    }

    /// A large point widens the column: a line or a box stays a swatch wide, and the names line up.
    #[test]
    fn a_wide_column_keeps_lines_and_boxes_a_swatch_wide_in_its_middle() {
        use rustgis_data::Dataset;
        let blue = Rgba::rgb(0, 0, 255);
        let point =
            rustgis_map::Symbol { fill: blue, size: 40.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        for gt in [GeometryType::Polyline, GeometryType::Polygon] {
            // A point of 40 pt makes the column 48 wide: a swatch of 16 in its middle runs from 16 to 32.
            let mut map = point_legend_map(|l| l.renderer = rustgis_map::Renderer::Single(point));
            let id = map.add_dataset(Dataset::new("line", gt, vec![]));
            let red = Rgba::rgb(255, 0, 0);
            if let Some(l) = map.layer_mut(id) {
                l.renderer = rustgis_map::Renderer::Single(rustgis_map::Symbol {
                    fill: red,
                    stroke: red,
                    stroke_width: 2.0,
                    ..rustgis_map::Symbol::default_for(gt, 0)
                });
            }
            let pm = point_legend(&map);
            let columns = |ink: &dyn Fn(u8, u8, u8) -> bool| -> Vec<u32> {
                (0..pm.width())
                    .filter(|&x| {
                        (0..pm.height()).any(|y| pm.pixel(x, y).is_some_and(|p| p.alpha() > 0 && ink(p.red(), p.green(), p.blue())))
                    })
                    .collect()
            };
            let swatch = columns(&|r, g, b| r > 200 && g < 60 && b < 60);
            let (left, right) = (swatch.first().copied().unwrap_or(0), swatch.last().copied().unwrap_or(0));
            assert!((14..=17).contains(&left) && (31..=34).contains(&right), "{gt:?}: {left} to {right}");
            // Both names start past the column, where the blue point has no part in it.
            let names = columns(&|r, g, b| r < 100 && g < 100 && b < 100);
            assert!(names.first().is_some_and(|x| (53..60).contains(x)), "{gt:?}: {names:?}");
        }
    }

    /// A symbol past the limit fills its box and stays in it, on pixels and in a recording.
    #[test]
    fn an_oversized_point_stays_in_its_row() {
        let blue = Rgba::rgb(0, 0, 255);
        let s = rustgis_map::Symbol {
            fill: blue,
            size: 3000.0,
            stroke_width: 0.0,
            marker: rustgis_map::Marker::Square,
            ..rustgis_map::Symbol::default_for(GeometryType::Point, 0)
        };
        // Opaque and see-through: a row and a column of 2048 pt, which the square fills.
        for transparency in [0.0, 50.0] {
            let map = point_legend_map(|l| {
                l.renderer = rustgis_map::Renderer::Single(s);
                l.transparency = transparency;
            });
            let mut pm = Pixmap::new(2100, 2100).unwrap();
            pm.fill(tiny_skia::Color::WHITE);
            let opts = LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() };
            crate::canvas::with_canvas(&mut pm, |c| {
                legend(c, Rect::from_xywh(0.0, 0.0, 2100.0, 2100.0).unwrap(), &map, ("", 1), None, opts, 1.0)
            });
            // Drawn, and recorded for a vector export, where only its clip cuts it.
            for pm in [pm, recorded_legend(&map, 2100)] {
                let white = |x, y| pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == (255, 255, 255));
                assert!(!white(10, 10) && !white(2040, 2040), "transparency {transparency}: it fills the box");
                assert!(white(10, 2060) && white(2060, 10), "transparency {transparency}: it stays in the box");
            }
        }
    }

    /// A legend starting off the canvas: a symbol larger than the canvas still covers all of it.
    #[test]
    fn a_see_through_point_larger_than_the_canvas_covers_it_from_off_its_edge() {
        let s = rustgis_map::Symbol {
            fill: Rgba::rgb(0, 0, 255),
            size: 3000.0,
            stroke_width: 0.0,
            marker: rustgis_map::Marker::Square,
            ..rustgis_map::Symbol::default_for(GeometryType::Point, 0)
        };
        let map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Single(s);
            l.transparency = 50.0;
        });
        let mut pm = Pixmap::new(300, 300).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let opts = LegendOptions { hide_other: false, exclude: &[], scale: 1e6, look: plain() };
        crate::canvas::with_canvas(&mut pm, |c| {
            legend(c, Rect::from_xywh(-50.0, -50.0, 2100.0, 2100.0).unwrap(), &map, ("", 1), None, opts, 1.0)
        });
        // Its box runs from -50 to 1998: every corner of the canvas is in it.
        for (x, y) in [(5, 5), (295, 5), (5, 295), (295, 295)] {
            let white = pm.pixel(x, y).is_some_and(|p| (p.red(), p.green(), p.blue()) == (255, 255, 255));
            assert!(!white, "({x}, {y})");
        }
    }

    /// A class's large symbol counts in a legend's size as a plain row's does.
    #[test]
    fn a_class_row_takes_the_wide_column_in_a_legends_size() {
        let mm = 25.4 / 96.0;
        let s = rustgis_map::Symbol { size: 40.0, stroke_width: 0.0, ..rustgis_map::Symbol::default_for(GeometryType::Point, 0) };
        let mut map = point_legend_map(|l| {
            l.renderer = rustgis_map::Renderer::Unique {
                field: "g".into(),
                classes: vec![("a".to_string(), s)],
                other: s,
                ramp: rustgis_map::Ramp::Ember,
                class_labels: Default::default(),
                other_label: None,
                class_strokes: Default::default(),
                other_strokes: Vec::new(),
                class_fills: Default::default(),
                other_fills: Vec::new(),
                expression: None,
            };
        });
        // The layer's name over the class: a row of 15 and one of 48, 6 + 48 + 6 before its label.
        let [(w, h), ..] = legend_sizes_mm(&mut map, 1e6, true, true, &[], plain()).unwrap();
        assert!((h - f64::from(LEGEND_ROW + 48.0) * mm).abs() < 1e-6, "{h} mm");
        assert!((w - f64::from(48.0 + 12.0 + text::measure("a", 8.5, false)) * mm).abs() < 1e-4, "{w} mm");
    }
}
