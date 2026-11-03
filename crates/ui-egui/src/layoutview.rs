//! The layout view: the page with its elements, shown instead of the map when a layout is open.
//! Click an element to select it (Shift- or Ctrl-click adds or takes one away, Ctrl-drag selects
//! those inside a box; a group is picked whole), drag it to move, drag its handles to resize, its
//! round knob to turn it, Shift-drag a map frame to pan its map; the wheel zooms the page. The properties panel edits the selection.
//! Rulers run along the top and left: drag out of one to add a guide, drag a guide back onto one
//! to remove it. Dragged elements snap to guides, the page and each other (Alt places freely).

use egui::{Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::map::layout::{
    AreaPatch, Arrange, ArrowStyle, CoordFormat, ElementKind, Grid, GridKind, GridStyle, LabelText, Layout, LegendLook, LineHead,
    LinePatch, MAX_BAR_HEIGHT, MAX_COLORBAR_TICKS, MAX_FRAME_SCALE, MAX_GRID_WIDTH, MAX_LEGEND_SPACING, MAX_PATCH, MAX_SCALE_DIVISIONS,
    MAX_TICK_MM, MIN_FRAME_SCALE, NumberPlace, Orientation, PT_PER_MM, ScaleBarStyle, ScaleUnits, TickSide, UnitLabel, UnitPlace,
    frame_camera, line_angle, line_length,
};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, regular, semibold};
use crate::widgets::Dragged;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    /// Moving an element: its id and the pointer's page position (mm) at the start.
    Move(u32, (f64, f64)),
    /// Resizing from a handle (0..8 clockwise from top-left).
    Resize(u32, usize, (f64, f64)),
    /// Panning a map frame's map.
    PanFrame(u32, (f64, f64)),
    /// Panning the page view.
    Pan,
    /// Drawing a lasso frame.
    Lasso,
    /// Dragging a new guide out of a ruler: vertical (from the left ruler) or horizontal.
    NewGuide(bool),
    /// Moving a guide: vertical or horizontal, and its index.
    Guide(bool, usize),
    /// Turning an element by its rotation handle: its id, its angle at the start (a line's
    /// direction, another element's rotation), and the pointer's angle about its centre then
    /// (degrees).
    Rotate(u32, f64, f64),
    /// Selecting the elements inside a box (Ctrl-drag): where it started (mm), and whether they
    /// are added to the selection (Shift as well).
    Marquee((f64, f64), bool),
}

/// Width of the rulers along the view's top and left edges (points).
const RULER: f32 = 18.0;
/// How near (points) a dragged edge snaps to a line.
const SNAP_PT: f32 = 6.0;
/// How near (points) the pointer picks up a guide.
const GUIDE_PICK: f32 = 4.0;
const GUIDE_COLOR: Color32 = Color32::from_rgb(0x00, 0x9E, 0xD8);
const SNAP_COLOR: Color32 = Color32::from_rgb(0xE0, 0x3E, 0x8C);

/// An element's box: x, y, width, height (mm).
type Box4 = (f64, f64, f64, f64);

#[derive(Default)]
pub struct LayoutView {
    texture: Option<egui::TextureHandle>,
    key: Option<(u64, usize, [usize; 2], i64, i64, i64)>,
    /// The tile cache's generation the page was drawn at while a legend picture it shows was
    /// still on its way (in the browser, which doesn't wait for them): drawn again once a fetch
    /// lands.
    legends_wait: Option<u64>,
    /// Page zoom (1 = fit) and pan, in screen points.
    zoom: f32,
    pan: egui::Vec2,
    pub selected: Option<u32>,
    /// The others selected with `selected` (Shift- or Ctrl-click, a box, a group): the element
    /// they were picked with, and theirs. They count only while it is still `selected`, so
    /// anything that selects another element on its own drops them.
    also: (Option<u32>, Vec<u32>),
    /// The selection the Format pane last opened for, by layout and element (ids are per layout):
    /// a new one opens it again, so closing the pane keeps the selection without the pane coming
    /// back by itself.
    formatted: Option<(usize, u32)>,
    /// The element whose fields the Format pane drew this frame, by layout and element: text
    /// typed into any other is saved at the end of the frame (`save_drafts`).
    drawn: Option<(usize, u32)>,
    /// The project (`Session::opened`) the selection and the text being typed are for.
    project: Option<u64>,
    drag: Option<Drag>,
    /// The page rectangle and points per millimetre the drag last saw.
    drag_page: Option<(Rect, f32)>,
    /// A frame being drawn: "polygon" (clicked) or "lasso" (dragged), and its points (mm).
    pub tool: Option<&'static str>,
    sketch: Vec<(f64, f64)>,
    /// Text being typed into the element properties, by (layout, element, field): kept here so
    /// keystrokes survive the frame, and saved when the field loses focus or stops being shown.
    typing: std::collections::HashMap<(usize, u32, &'static str), String>,
    /// The layout shown, shared between frames until the project changes (layouts can hold
    /// megabytes of pictures, too much to copy every frame), keyed by layout and revision.
    shown: Option<(usize, u64, std::rc::Rc<Layout>)>,
    /// The map series page count and the current page's name, by the same key.
    series: Option<(usize, u64, usize, String)>,
    /// Dragged elements don't snap (they do by default; Alt places freely for one drag).
    pub no_snap: bool,
    /// The rulers are hidden.
    pub no_rulers: bool,
    /// Map frames are navigated rather than moved: dragging one pans its map, the wheel zooms it.
    pub navigate: bool,
    /// A wheel zoom of a map frame shown but not yet applied (one undo step per gesture, not per
    /// wheel step): its layout's name (unique, unlike an index, which shifts when layouts are
    /// removed), the frame, the factor on its scale, the page point (mm) kept under the pointer,
    /// and when the wheel last turned (seconds).
    frame_zoom: Option<(String, u32, f64, (f64, f64), f64)>,
    /// Where the primary button last went down: egui forgets it on release, and a click is
    /// judged by where it started.
    pressed: Option<Pos2>,
    /// The map of a frame being panned, with [`PAN_OVERSCAN`] of map past each side: drawn once
    /// as the pan starts and slid under the pointer, so the map comes into view as it moves; None
    /// when it couldn't be drawn (it isn't tried again for that pan).
    pan_preview: Option<(u32, Option<egui::TextureHandle>)>,
    /// The arrow-key nudges so far: how many runs of them there have been, and the last one's
    /// layout, element and time (seconds), for [`LayoutView::nudge_key`].
    nudges: u64,
    nudged: Option<(usize, u32, f64)>,
}

impl LayoutView {
    /// Every element of `layout` selected: the one picked first, then the others.
    pub fn selection(&self, layout: &Layout) -> Vec<u32> {
        let Some(first) = self.selected.filter(|id| layout.element(*id).is_some()) else { return Vec::new() };
        let mut out = vec![first];
        if self.also.0 == Some(first) {
            for id in &self.also.1 {
                if layout.element(*id).is_some() && !out.contains(id) {
                    out.push(*id);
                }
            }
        }
        out
    }

    /// Selects these elements, the first as the one picked first, with the groups they are in;
    /// none, when the list is empty.
    pub fn select_all(&mut self, layout: &Layout, ids: &[u32]) {
        let mut all = layout.with_groups(ids);
        self.selected = ids.iter().copied().find(|id| layout.element(*id).is_some()).or(all.first().copied());
        all.retain(|id| Some(*id) != self.selected);
        self.also = (self.selected, all);
    }

    /// Selects `id` with its group, or nothing.
    pub fn select(&mut self, layout: &Layout, id: Option<u32>) {
        match id {
            Some(id) => self.select_all(layout, &[id]),
            None => {
                self.selected = None;
                self.also = (None, Vec::new());
            }
        }
    }

    /// Adds `id` and its group to the selection, or takes them out when it is in it.
    pub fn toggle(&mut self, layout: &Layout, id: u32) {
        let mut sel = self.selection(layout);
        let members = layout.members(id);
        if sel.contains(&id) {
            sel.retain(|x| !members.contains(x));
        } else {
            sel.extend(members);
        }
        self.select_all(layout, &sel);
    }
}

/// The one element selected, when only one is.
fn single<'a>(app: &RustGisApp, layout: &'a Layout) -> Option<&'a rustgis_engine::map::layout::Element> {
    match app.layout.selection(layout).as_slice() {
        [id] => layout.element(*id),
        _ => None,
    }
}

/// An element's corners on screen, turned as it is, or as it would be at `rotation`.
fn screen_corners(page: Rect, s: f32, e: &rustgis_engine::map::layout::Element, rotation: Option<f64>) -> [Pos2; 4] {
    let mut e2 = e.clone();
    if let Some(r) = rotation {
        e2.rotation = r;
    }
    e2.corners().map(|p| to_screen(page, s, p))
}

/// How far (mm) an arrow key nudges the selected layout element, and with Shift.
pub const NUDGE_MM: f64 = 1.0;
pub const NUDGE_FAST_MM: f64 = 10.0;

impl LayoutView {
    /// The undo key for an arrow key nudging element `id` of layout `li` at `now` (seconds): the
    /// last nudge's while the same element keeps being nudged, at most a second apart, so a held
    /// key (or quick taps) is one step to undo.
    pub fn nudge_key(&mut self, li: usize, id: u32, now: f64) -> String {
        let same = self.nudged.is_some_and(|(l, i, t)| l == li && i == id && (0.0..=1.0).contains(&(now - t)));
        if !same {
            self.nudges = self.nudges.wrapping_add(1);
        }
        self.nudged = Some((li, id, now));
        format!("layout nudge {}", self.nudges)
    }
}

/// Forgets the selection and the text typed into its fields once another project has replaced
/// the one they were for: its elements may have the same ids. Run after every command, after
/// what agents and scripts sent, and before drafts are saved.
pub(crate) fn follow_project(app: &mut RustGisApp) {
    let opened = app.session.opened;
    if app.layout.project.is_some_and(|p| p != opened) {
        app.layout.selected = None;
        app.layout.also = (None, Vec::new());
        app.layout.formatted = None;
        app.layout.drawn = None;
        app.layout.typing.clear();
    }
    app.layout.project = Some(opened);
}

/// How much map a pan's preview draws past each side of the frame, in frame widths and heights.
const PAN_OVERSCAN: f64 = 1.0;
/// Most pixels a pan's preview is drawn with: it's drawn on the spot as the pan starts.
const PAN_PREVIEW_PIXELS: f64 = 6_000_000.0;
/// Longest side, in pixels, a pan's preview is drawn with, within what the GPU takes.
const PAN_PREVIEW_SIDE: f64 = 8192.0;

/// A map frame's map with [`PAN_OVERSCAN`] past each side, at the screen's resolution (or less,
/// within [`PAN_PREVIEW_PIXELS`] and [`PAN_PREVIEW_SIDE`]), for a pan's preview. It doesn't wait
/// for tiles still on their way, as it's drawn while the pointer moves.
fn pan_preview(app: &mut RustGisApp, ctx: &egui::Context, layout: &Layout, id: u32, s: f32) -> Option<egui::TextureHandle> {
    let e = layout.element(id)?;
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &e.kind else { return None };
    // A tilted view's perspective follows the drawing's height, so a taller drawing wouldn't match
    // the frame: a pitched frame slides the page as before.
    if *pitch != 0.0 {
        return None;
    }
    let m = app.session.project.maps.iter_mut().find(|m| m.name.eq_ignore_ascii_case(map))?;
    let cam = frame_camera(m, *center, *scale, *bearing, *pitch);
    let k = 1.0 + 2.0 * PAN_OVERSCAN;
    let (w, h) = (e.w * PT_PER_MM * k, e.h * PT_PER_MM * k);
    let screen = f64::from(s) / PT_PER_MM * f64::from(ctx.pixels_per_point());
    // Within the pixel budget, and within the longest side the GPU takes for a texture (a long,
    // thin frame could pass the first and not the second).
    let side = (ctx.input(|i| i.max_texture_side) as f64).min(PAN_PREVIEW_SIDE);
    let ppp = screen.min((PAN_PREVIEW_PIXELS / (w * h).max(1.0)).sqrt()).min(side / w.max(h).max(1.0)).max(0.01);
    let pm = rustgis_engine::render::layout::frame_map_preview(m, cam, w, h, ppp)?;
    let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
    Some(ctx.load_texture("layout_pan_preview", img, egui::TextureOptions::LINEAR))
}

/// Seconds after the wheel stops before a map frame's zoom is applied.
const FRAME_ZOOM_SETTLE: f64 = 0.35;

impl LayoutView {
    /// Draws the page again on the next frame, though the project hasn't changed (the installed
    /// fonts a text names have just been indexed).
    pub fn redraw(&mut self) {
        self.key = None;
    }

    /// Stops drawing a polygon or lasso frame, or navigating map frames (Esc).
    pub fn cancel_tool(&mut self) {
        self.navigate = false;
        self.tool = None;
        self.sketch.clear();
        if self.drag == Some(Drag::Lasso) {
            self.drag = None;
        }
    }
}

/// Page geometry on screen: the page rectangle and points per millimetre.
fn page_rect(lv: &LayoutView, area: Rect, l: &Layout) -> (Rect, f32) {
    let fit = ((area.width() - 48.0) / l.width as f32).min((area.height() - 48.0) / l.height as f32).max(0.05);
    let s = fit * if lv.zoom > 0.0 { lv.zoom } else { 1.0 };
    let size = vec2(l.width as f32 * s, l.height as f32 * s);
    (Rect::from_center_size(area.center() + lv.pan, size), s)
}

fn to_mm(page: Rect, s: f32, p: Pos2) -> (f64, f64) {
    (((p.x - page.min.x) / s) as f64, ((p.y - page.min.y) / s) as f64)
}

fn to_screen(page: Rect, s: f32, (x, y): (f64, f64)) -> Pos2 {
    pos2(page.min.x + x as f32 * s, page.min.y + y as f32 * s)
}

/// A resize handle's drawn size, and how near (points) the pointer picks one up.
const HANDLE: f32 = 10.0;
const HANDLE_PICK: f32 = 11.0;

/// The selected element's resize handle at a screen point: the element and the handle (0..8
/// clockwise from top-left), the nearest within reach. On a small element the reach shrinks, so
/// its inside still moves it; outside, it keeps a floor, so a thin element's handles can still be
/// picked up. Navigating, a map frame under the pointer pans instead: no handle. A locked
/// element has none.
fn handle_at(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(u32, usize)> {
    if nav_frame(app, layout, page, s, pos).is_some() {
        return None;
    }
    // One element, upright: a turned one is sized in the Format pane.
    let e = single(app, layout).filter(|e| !e.locked && (e.rotation == 0.0 || !e.kind.turns()))?;
    let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
    let third = r.width().min(r.height()) / 3.0;
    let reach = HANDLE_PICK.min(if r.contains(pos) { third } else { third.max(5.0) });
    handles(r)
        .iter()
        .enumerate()
        .map(|(i, h)| (i, (h.x - pos.x).abs().max((h.y - pos.y).abs())))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| (e.id, i))
}

/// How far (points) above a selected element its rotation handle sits.
const ROTATE_GAP: f32 = 22.0;

/// The selected element's rotation handle, unless it's locked or more are selected: the element,
/// where the handle is and the element's centre, on screen. It sits above the middle of the
/// element's top edge, turned with it.
fn rotate_handle(app: &RustGisApp, layout: &Layout, page: Rect, s: f32) -> Option<(u32, Pos2, Pos2)> {
    let e = single(app, layout).filter(|e| !e.locked)?;
    if !e.kind.turns() {
        let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
        return Some((e.id, pos2(r.center().x, r.top() - ROTATE_GAP), r.center()));
    }
    let k = screen_corners(page, s, e, None);
    let top = k[0] + (k[1] - k[0]) / 2.0;
    let c = to_screen(page, s, e.center());
    let up = (top - c).normalized();
    let up = if up.is_finite() && up.length() > 0.5 { up } else { vec2(0.0, -1.0) };
    Some((e.id, top + up * ROTATE_GAP, c))
}

/// The rotation handle at a screen point: the line, and its centre on screen.
fn rotate_at(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(u32, Pos2)> {
    let (id, h, c) = rotate_handle(app, layout, page, s)?;
    ((h - pos).length() <= HANDLE_PICK).then_some((id, c))
}

/// An element's angle (degrees, anticlockwise): a line's from pointing right, another's rotation.
fn element_angle(layout: &Layout, id: u32) -> Option<f64> {
    let e = layout.element(id)?;
    match e.kind {
        ElementKind::Line { dir, head, .. } => Some(line_angle((e.x, e.y, e.w, e.h), dir, head)),
        _ => Some(e.rotation),
    }
}

/// The angle (degrees, anticlockwise from pointing right) of `p` seen from `c`, on screen.
fn screen_angle(c: Pos2, p: Pos2) -> f64 {
    f64::from(c.y - p.y).atan2(f64::from(p.x - c.x)).to_degrees()
}

/// A line's angle after the pointer went round its centre from `from` to `to` degrees: in 15°
/// steps with `steps` (Shift), else onto a multiple of 45° within 3° of one, so level, upright
/// and diagonal lines are easy to get. Within -180..180.
fn turned(start: f64, from: f64, to: f64, steps: bool) -> f64 {
    let a = start + (to - from);
    let a = if steps {
        (a / 15.0).round() * 15.0
    } else {
        let m = (a / 45.0).round() * 45.0;
        if (a - m).abs() <= 3.0 { m } else { a }
    };
    (a + 180.0).rem_euclid(360.0) - 180.0
}

/// The cursor for dragging handle `h`.
fn handle_cursor(h: usize) -> CursorIcon {
    match h {
        0 | 4 => CursorIcon::ResizeNwSe,
        2 | 6 => CursorIcon::ResizeNeSw,
        1 | 5 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeHorizontal,
    }
}

fn handles(r: Rect) -> [Pos2; 8] {
    let (c, m) = (r.center(), r);
    [
        m.left_top(),
        pos2(c.x, m.top()),
        m.right_top(),
        pos2(m.right(), c.y),
        m.right_bottom(),
        pos2(c.x, m.bottom()),
        m.left_bottom(),
        pos2(m.left(), c.y),
    ]
}

/// A rectangle after dragging handle `h` by (dx, dy) mm. A side the handle doesn't move keeps its
/// size, so widening a hairline rule leaves it a hairline.
fn resized((x, y, w, h): (f64, f64, f64, f64), handle: usize, dx: f64, dy: f64) -> (f64, f64, f64, f64) {
    let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
    match handle {
        0 => (x0, y0) = (x0 + dx, y0 + dy),
        1 => y0 += dy,
        2 => (x1, y0) = (x1 + dx, y0 + dy),
        3 => x1 += dx,
        4 => (x1, y1) = (x1 + dx, y1 + dy),
        5 => y1 += dy,
        6 => (x0, y1) = (x0 + dx, y1 + dy),
        _ => x0 += dx,
    }
    let (sizes_x, sizes_y) = (!matches!(handle, 1 | 5), !matches!(handle, 3 | 7));
    let w = if sizes_x { (x1 - x0).abs().max(2.0) } else { w };
    let h = if sizes_y { (y1 - y0).abs().max(2.0) } else { h };
    (x0.min(x1), y0.min(y1), w, h)
}

/// The lines a dragged element snaps to (mm): the page's edges and centre, the guides, and the
/// other visible elements' edges and centres; vertical lines (x), then horizontal (y).
fn snap_lines(l: &Layout, moving: &[u32]) -> (Vec<f64>, Vec<f64>) {
    let mut xs = vec![0.0, l.width / 2.0, l.width];
    let mut ys = vec![0.0, l.height / 2.0, l.height];
    xs.extend(&l.guides.x);
    ys.extend(&l.guides.y);
    for e in l.elements.iter().filter(|e| !moving.contains(&e.id) && e.visible) {
        // A turned element by the box it covers.
        let (x, y, w, h) = e.bounds();
        xs.extend([x, x + w / 2.0, x + w]);
        ys.extend([y, y + h / 2.0, y + h]);
    }
    (xs, ys)
}

/// The smallest shift that puts one of `edges` on one of `lines`, if one is within `tol`, and
/// the line.
fn snap_to(edges: &[f64], lines: &[f64], tol: f64) -> Option<(f64, f64)> {
    edges
        .iter()
        .flat_map(|e| lines.iter().map(move |l| (l - e, *l)))
        .filter(|(d, _)| d.abs() <= tol)
        .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()))
}

/// Where an element lands when moved (no handle) or resized from a handle by the pointer's
/// offset (mm): snapped to `lines` within `tol` mm when given, with the lines it snapped to.
fn landing(
    r: Box4,
    handle: Option<usize>,
    (mut dx, mut dy): (f64, f64),
    snap: Option<(&(Vec<f64>, Vec<f64>), f64)>,
) -> (Box4, Option<f64>, Option<f64>) {
    let (x, y, w, h) = r;
    let (mut sx, mut sy) = (None, None);
    if let Some(((xs, ys), tol)) = snap {
        // The edges that move: every edge and the centre for a move, the handle's for a resize.
        let (ex, ey) = match handle {
            None => (vec![x, x + w / 2.0, x + w], vec![y, y + h / 2.0, y + h]),
            Some(hd) => (
                match hd {
                    0 | 6 | 7 => vec![x],
                    2..=4 => vec![x + w],
                    _ => vec![],
                },
                match hd {
                    0..=2 => vec![y],
                    4..=6 => vec![y + h],
                    _ => vec![],
                },
            ),
        };
        let ex: Vec<f64> = ex.iter().map(|e| e + dx).collect();
        let ey: Vec<f64> = ey.iter().map(|e| e + dy).collect();
        if let Some((d, line)) = snap_to(&ex, xs, tol) {
            dx += d;
            sx = Some(line);
        }
        if let Some((d, line)) = snap_to(&ey, ys, tol) {
            dy += d;
            sy = Some(line);
        }
    }
    let out = match handle {
        Some(hd) => resized(r, hd, dx, dy),
        None => (x + dx, y + dy, w, h),
    };
    (out, sx, sy)
}

/// The element being moved or resized, where it lands with the pointer at `at` (mm), and the
/// lines it snapped to.
fn drag_landing(lv: &LayoutView, ui: &Ui, layout: &Layout, s: f32, at: (f64, f64)) -> Option<(u32, Box4, Option<f64>, Option<f64>)> {
    let (id, handle, start) = match lv.drag? {
        Drag::Move(id, start) => (id, None, start),
        Drag::Resize(id, h, start) => (id, Some(h), start),
        _ => return None,
    };
    let e = layout.element(id)?;
    let free = lv.no_snap || ui.input(|i| i.modifiers.alt);
    // Moved with others, it snaps to none of them.
    let sel = lv.selection(layout);
    let moving = if sel.contains(&id) { sel } else { vec![id] };
    let lines = (!free).then(|| snap_lines(layout, &moving));
    let tol = f64::from(SNAP_PT / s.max(1e-3));
    let d = (at.0 - start.0, at.1 - start.1);
    if handle.is_none() {
        // Moved, it snaps by the box it covers, turned or not.
        let b = e.bounds();
        let (r, sx, sy) = landing(b, None, d, lines.as_ref().map(|l| (l, tol)));
        return Some((id, (e.x + r.0 - b.0, e.y + r.1 - b.1, e.w, e.h), sx, sy));
    }
    let (r, sx, sy) = landing((e.x, e.y, e.w, e.h), handle, d, lines.as_ref().map(|l| (l, tol)));
    Some((id, r, sx, sy))
}

/// The guide under a screen point: vertical or not, and its index.
fn guide_at(l: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(bool, usize)> {
    let near = |d: f32| d.abs() < GUIDE_PICK;
    l.guides
        .x
        .iter()
        .position(|x| near(to_screen(page, s, (*x, 0.0)).x - pos.x))
        .map(|i| (true, i))
        .or_else(|| l.guides.y.iter().position(|y| near(to_screen(page, s, (0.0, *y)).y - pos.y)).map(|i| (false, i)))
}

/// Which ruler a screen point is on: Some(true) the left one (vertical guides), Some(false) the top.
fn ruler_at(lv: &LayoutView, area: Rect, p: Pos2) -> Option<bool> {
    if lv.no_rulers || !area.contains(p) {
        return None;
    }
    let (left, top) = (p.x < area.left() + RULER, p.y < area.top() + RULER);
    match (left, top) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// Puts down a guide dragged out of a ruler or moved: dropped on a ruler or off the view, it's
/// removed.
#[allow(clippy::too_many_arguments)]
fn drop_guide(
    app: &mut RustGisApp,
    layout: &Layout,
    li: usize,
    page: Rect,
    s: f32,
    area: Rect,
    vertical: bool,
    moving: Option<usize>,
    at: Option<Pos2>,
) {
    let mut list = if vertical { layout.guides.x.clone() } else { layout.guides.y.clone() };
    if let Some(i) = moving.filter(|i| *i < list.len()) {
        list.remove(i);
    }
    if let Some(p) = at.filter(|p| area.contains(*p) && (app.layout.no_rulers || (p.x >= area.left() + RULER && p.y >= area.top() + RULER)))
    {
        let (mx, my) = to_mm(page, s, p);
        // To a tenth of a millimetre.
        list.push(((if vertical { mx } else { my }) * 10.0).round() / 10.0);
    }
    let key = if vertical { "x" } else { "y" };
    let _ = app.run("layout.guides", json!({"layout": li, key: list}));
}

/// Rulers along the view's top and left edges, in millimetres from the page's top-left corner,
/// with the guides and the pointer marked.
fn rulers(painter: &egui::Painter, area: Rect, page: Rect, s: f32, l: &Layout, pointer: Option<Pos2>, t: &Tokens) {
    const STEPS: [f64; 10] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];
    let sf = f64::from(s);
    let minor = STEPS.iter().copied().find(|st| st * sf >= 5.0).unwrap_or(1000.0);
    let major = STEPS.iter().copied().find(|st| st * sf >= 45.0 && (st / minor).fract() == 0.0).unwrap_or(1000.0);
    let top = Rect::from_min_max(area.min, pos2(area.max.x, area.min.y + RULER));
    let left = Rect::from_min_max(area.min, pos2(area.min.x + RULER, area.max.y));
    painter.rect_filled(top, 0.0, t.ribbon);
    painter.rect_filled(left, 0.0, t.ribbon);
    // The page's span, lighter.
    let span_x = Rect::from_min_max(pos2(page.min.x.max(left.max.x), top.min.y), pos2(page.max.x.min(top.max.x), top.max.y));
    let span_y = Rect::from_min_max(pos2(left.min.x, page.min.y.max(top.max.y)), pos2(left.max.x, page.max.y.min(left.max.y)));
    for r in [span_x, span_y] {
        if r.is_positive() {
            painter.rect_filled(r, 0.0, t.input);
        }
    }
    let tick = Stroke::new(1.0, t.text_dim);
    let is_major = |v: f64| ((v / major) - (v / major).round()).abs() < 1e-6;
    // Along the top, then down the side; at most a few thousand ticks either way.
    let (x0, y0) = to_mm(page, s, pos2(left.max.x, top.max.y));
    let (x1, y1) = to_mm(page, s, area.max);
    for k in ((x0 / minor).ceil() as i64..=(x1 / minor).floor() as i64).take(4000) {
        let v = k as f64 * minor;
        let x = to_screen(page, s, (v, 0.0)).x;
        let len = if is_major(v) { RULER * 0.6 } else { RULER * 0.25 };
        painter.line_segment([pos2(x, top.max.y - len), pos2(x, top.max.y)], tick);
        if is_major(v) {
            painter.text(pos2(x + 2.0, top.min.y + 1.0), Align2::LEFT_TOP, format!("{v:.0}"), regular(9.0), t.text_dim);
        }
    }
    for k in ((y0 / minor).ceil() as i64..=(y1 / minor).floor() as i64).take(4000) {
        let v = k as f64 * minor;
        let y = to_screen(page, s, (0.0, v)).y;
        let len = if is_major(v) { RULER * 0.6 } else { RULER * 0.25 };
        painter.line_segment([pos2(left.max.x - len, y), pos2(left.max.x, y)], tick);
        if is_major(v) {
            painter.text(pos2(left.min.x + 1.0, y + 1.0), Align2::LEFT_TOP, format!("{v:.0}"), regular(9.0), t.text_dim);
        }
    }
    // The guides, as small triangles pointing into the page.
    for x in &l.guides.x {
        let x = to_screen(page, s, (*x, 0.0)).x;
        if x > left.max.x {
            let pts = vec![pos2(x - 4.0, top.max.y - 6.0), pos2(x + 4.0, top.max.y - 6.0), pos2(x, top.max.y)];
            painter.add(egui::Shape::convex_polygon(pts, GUIDE_COLOR, Stroke::NONE));
        }
    }
    for y in &l.guides.y {
        let y = to_screen(page, s, (0.0, *y)).y;
        if y > top.max.y {
            let pts = vec![pos2(left.max.x - 6.0, y - 4.0), pos2(left.max.x - 6.0, y + 4.0), pos2(left.max.x, y)];
            painter.add(egui::Shape::convex_polygon(pts, GUIDE_COLOR, Stroke::NONE));
        }
    }
    // Where the pointer is.
    if let Some(p) = pointer.filter(|p| area.contains(*p)) {
        let mark = Stroke::new(1.0, t.accent);
        if p.x > left.max.x {
            painter.line_segment([pos2(p.x, top.min.y), pos2(p.x, top.max.y)], mark);
        }
        if p.y > top.max.y {
            painter.line_segment([pos2(left.min.x, p.y), pos2(left.max.x, p.y)], mark);
        }
    }
    painter.rect_filled(Rect::from_min_size(area.min, vec2(RULER, RULER)), 0.0, t.ribbon);
    painter.line_segment([pos2(left.max.x, top.max.y), pos2(top.max.x, top.max.y)], Stroke::new(1.0, t.border));
    painter.line_segment([pos2(left.max.x, top.max.y), pos2(left.max.x, left.max.y)], Stroke::new(1.0, t.border));
}

/// The layout to show, copied from the project only when it has changed since the last copy.
fn shown_layout(app: &mut RustGisApp, li: usize) -> Option<std::rc::Rc<Layout>> {
    let rev = app.session.revision;
    match &app.layout.shown {
        Some((i, r, l)) if *i == li && *r == rev => Some(l.clone()),
        _ => {
            let l = std::rc::Rc::new(app.session.project.layouts.get(li).cloned()?);
            app.layout.shown = Some((li, rev, l.clone()));
            Some(l)
        }
    }
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let Some(li) = app.session.active_layout else { return };
    let Some(layout) = shown_layout(app, li) else { return };
    let t = Tokens::get(ui.ctx());
    if app.layout.selected.is_some_and(|id| layout.element(id).is_none()) {
        app.layout.selected = None;
    }
    // Selecting an element shows its properties in the Format pane (#516). The pane is drawn
    // before the page, so it shows them from the next frame.
    let selected = app.layout.selected.map(|id| (li, id));
    if selected != app.layout.formatted {
        app.layout.formatted = selected;
        if selected.is_some() {
            app.ui.open_right(crate::RightPane::Format);
            ui.ctx().request_repaint();
        }
    }
    let area = ui.available_rect_before_wrap();
    // A stable id: dragging an element selects it and opens the Format pane beside the page,
    // and an automatic id would change with it, losing the drag before it is dropped (#325).
    ui.advance_cursor_after_rect(area);
    let resp = ui.interact(area, ui.scope_id().with("layout_page"), Sense::click_and_drag());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, if t.dark { Color32::from_gray(38) } else { Color32::from_rgb(0xD9, 0xDD, 0xE3) });
    let (page, s) = page_rect(&app.layout, area, &layout);
    interact(app, ui, &resp, &layout, li, page, s);
    // Right-click: what the element under the pointer, and the others selected with it, can do.
    // An element not selected is selected first.
    if resp.secondary_clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let (x, y) = to_mm(page, s, pos);
        match layout.hit(x, y) {
            Some(id) if !app.layout.selection(&layout).contains(&id) => app.layout.select(&layout, Some(id)),
            None => app.layout.select(&layout, None),
            _ => {}
        }
    }
    resp.context_menu(|ui| element_menu(app, ui, &layout, li));
    // The properties or a drag may have just changed the layout: draw the page as it is now,
    // or the texture would be keyed to the new revision but show the old layout (#325).
    let Some(layout) = shown_layout(app, li) else { return };
    let (page, s) = page_rect(&app.layout, area, &layout);

    // Render the page at screen resolution when it changes.
    let ppp = ui.ctx().pixels_per_point();
    let px_per_mm = s * ppp;
    // The texture can't be larger than the GPU allows: zoomed far in, the page draws coarser.
    let max_side = ui.ctx().input(|i| i.max_texture_side) as f64;
    let dpi = (px_per_mm as f64 * 25.4).min(max_side / layout.width.max(layout.height) * 25.4).clamp(20.0, 300.0);
    let size = [(page.width() * ppp) as usize, (page.height() * ppp) as usize];
    let key = (app.session.revision, li, size, (dpi * 10.0) as i64, layout.width as i64, layout.height as i64);
    // Only once the pictures have: any other fetch landing (the map's tiles) bumps the generation.
    let landed = app.layout.legends_wait.is_some_and(|g| g != rustgis_engine::render::rustgis_tiles::cache::generation())
        && !rustgis_engine::render::layout::legends_pending(&layout, &app.session.project.maps);
    if (app.layout.key != Some(key) || landed) && size[0] > 2 && size[1] > 2 {
        let info = page_info(app, li);
        // Before drawing: a fetch landing while it draws is a reason to draw again.
        let generation = rustgis_engine::render::rustgis_tiles::cache::generation();
        if let Some(pm) = rustgis_engine::render::layout::render(&layout, &mut app.session.project.maps, dpi, &info) {
            let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
            match &mut app.layout.texture {
                Some(tex) => tex.set(img, egui::TextureOptions::LINEAR),
                None => app.layout.texture = Some(ui.ctx().load_texture("layout", img, egui::TextureOptions::LINEAR)),
            }
            app.layout.key = Some(key);
            // Natively the page waited for them already: drawn again, it would wait again.
            app.layout.legends_wait = (cfg!(target_arch = "wasm32")
                && rustgis_engine::render::layout::legends_pending(&layout, &app.session.project.maps))
            .then_some(generation);
        }
    }
    if app.layout.legends_wait.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    // A pan's preview is drawn as the pan starts, and let go when it ends.
    match app.layout.drag {
        Some(Drag::PanFrame(id, _)) if app.layout.pan_preview.as_ref().is_none_or(|(p, _)| *p != id) => {
            let ctx = ui.ctx().clone();
            let map = pan_preview(app, &ctx, &layout, id, s);
            app.layout.pan_preview = Some((id, map));
        }
        Some(Drag::PanFrame(..)) => {}
        _ => app.layout.pan_preview = None,
    }
    // Shadow, page, elements.
    painter.rect_filled(page.translate(vec2(3.0, 4.0)), 2.0, Color32::from_black_alpha(50));
    painter.rect_filled(page, 0.0, Color32::WHITE);
    let pointer = ui.input(|i| i.pointer.latest_pos());
    if let Some(tex) = &app.layout.texture {
        painter.image(tex.id(), page, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        // A map frame being panned or zoomed: its part of the page drawn shifted or scaled at
        // once, until the change is applied and the page redrawn.
        let preview = match (app.layout.drag, pointer) {
            (Some(Drag::PanFrame(id, start)), Some(p)) => {
                let (mx, my) = to_mm(page, s, p);
                Some((id, 1.0, (mx - start.0, my - start.1), (0.0, 0.0)))
            }
            _ => app.layout.frame_zoom.as_ref().filter(|z| z.0 == layout.name).map(|z| (z.1, z.2, (0.0, 0.0), z.3)),
        };
        if let Some((id, k, (dx, dy), (ax, ay))) = preview
            && let Some(e) = layout.element(id)
        {
            let frame = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
            // The page region the frame now shows: moved back by the pan, scaled about the anchor.
            let src = |(x, y): (f64, f64)| {
                let (u, v) = (ax + (x - ax) * k - dx, ay + (y - ay) * k - dy);
                pos2((u / layout.width) as f32, (v / layout.height) as f32)
            };
            let uv = Rect::from_min_max(src((e.x, e.y)), src((e.x + e.w, e.y + e.h)));
            let clip = painter.with_clip_rect(frame);
            clip.rect_filled(frame, 0.0, Color32::WHITE);
            match &app.layout.pan_preview {
                // A pan: the frame's map with the overscan around it, moved with the pointer, so
                // what comes into view is map, not the page beside the frame.
                Some((pid, Some(map))) if *pid == id && k == 1.0 => {
                    let o = PAN_OVERSCAN as f32;
                    let around = Rect::from_min_max(frame.min - frame.size() * o, frame.max + frame.size() * o);
                    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    clip.image(map.id(), around.translate(vec2(dx as f32, dy as f32) * s), full, Color32::WHITE);
                }
                _ => {
                    clip.image(tex.id(), frame, uv, Color32::WHITE);
                }
            }
        }
    }
    let accent = t.accent;
    // Guides across the view; one being moved is drawn at the pointer instead.
    let moving = match app.layout.drag {
        Some(Drag::Guide(v, i)) => Some((v, i)),
        _ => None,
    };
    let vline = |x: f32, c: Color32| painter.line_segment([pos2(x, area.top()), pos2(x, area.bottom())], Stroke::new(1.0, c));
    let hline = |y: f32, c: Color32| painter.line_segment([pos2(area.left(), y), pos2(area.right(), y)], Stroke::new(1.0, c));
    for (_, x) in layout.guides.x.iter().enumerate().filter(|(i, _)| moving != Some((true, *i))) {
        vline(to_screen(page, s, (*x, 0.0)).x, GUIDE_COLOR);
    }
    for (_, y) in layout.guides.y.iter().enumerate().filter(|(i, _)| moving != Some((false, *i))) {
        hline(to_screen(page, s, (0.0, *y)).y, GUIDE_COLOR);
    }
    if let (Some(Drag::NewGuide(v) | Drag::Guide(v, _)), Some(p)) = (app.layout.drag, pointer) {
        let (mx, my) = to_mm(page, s, p);
        let label = if v {
            vline(p.x, GUIDE_COLOR);
            format!("{mx:.1} mm")
        } else {
            hline(p.y, GUIDE_COLOR);
            format!("{my:.1} mm")
        };
        painter.text(p + vec2(8.0, 8.0), Align2::LEFT_TOP, label, regular(11.0), GUIDE_COLOR);
    }
    // The dragged element's outline where it will land, and the lines it snapped to.
    if let Some(p) = pointer
        && let Some((id, r, sx, sy)) = drag_landing(&app.layout, ui, &layout, s, to_mm(page, s, p))
    {
        if let Some(x) = sx {
            vline(to_screen(page, s, (x, 0.0)).x, SNAP_COLOR);
        }
        if let Some(y) = sy {
            hline(to_screen(page, s, (0.0, y)).y, SNAP_COLOR);
        }
        // Where it lands, and the others moved with it, turned as they are.
        let moved = match app.layout.drag {
            Some(Drag::Move(..)) => {
                let sel = app.layout.selection(&layout);
                let others = if sel.contains(&id) { sel } else { vec![id] };
                let (dx, dy) = layout.element(id).map_or((0.0, 0.0), |e| (r.0 - e.x, r.1 - e.y));
                others.iter().filter_map(|o| layout.element(*o)).map(|e| (e.clone(), dx, dy)).collect()
            }
            _ => Vec::new(),
        };
        if moved.is_empty() {
            let rr = Rect::from_min_max(to_screen(page, s, (r.0, r.1)), to_screen(page, s, (r.0 + r.2, r.1 + r.3)));
            painter.rect_stroke(rr, 0.0, Stroke::new(1.5, accent), egui::StrokeKind::Middle);
        }
        for (mut e, dx, dy) in moved {
            e.x += dx;
            e.y += dy;
            let k = screen_corners(page, s, &e, None);
            painter.add(egui::Shape::closed_line(k.to_vec(), Stroke::new(1.5, accent)));
        }
    }
    let sel = app.layout.selection(&layout);
    let one = sel.len() == 1;
    for (i, e) in sel.iter().filter_map(|id| layout.element(*id)).enumerate() {
        let k = screen_corners(page, s, e, None);
        let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
        // The one picked first a little bolder: sizing alike goes by it.
        painter.extend(egui::Shape::dashed_line(
            &[k[0], k[1], k[2], k[3], k[0]],
            Stroke::new(if i == 0 { 1.6 } else { 1.0 }, accent),
            5.0,
            3.0,
        ));
        if e.locked {
            // No handles: a lock at the top-right corner says why.
            crate::icons::paint(&painter, Rect::from_center_size(k[1], vec2(16.0, 16.0)), "lock", accent, accent);
        } else if one {
            // The handle under the pointer, or being dragged, is filled. A turned element has
            // none: it is sized in the Format pane.
            let active = match app.layout.drag {
                Some(Drag::Resize(id, h, _)) if id == e.id => Some(h),
                None => pointer.and_then(|p| handle_at(app, &layout, page, s, p)).map(|(_, h)| h),
                _ => None,
            };
            if e.rotation == 0.0 || !e.kind.turns() {
                for (i, h) in handles(r).into_iter().enumerate() {
                    let hr = Rect::from_center_size(h, vec2(HANDLE, HANDLE));
                    painter.rect_filled(hr, 2.0, if active == Some(i) { accent } else { Color32::WHITE });
                    painter.rect_stroke(hr, 2.0, Stroke::new(1.5, accent), egui::StrokeKind::Middle);
                }
            }
            // The rotation handle: a round knob on a stem above it, filled while it's under the
            // pointer or turning.
            if let Some((_, knob, c)) = rotate_handle(app, &layout, page, s) {
                let on = matches!(app.layout.drag, Some(Drag::Rotate(..)))
                    || (app.layout.drag.is_none() && pointer.is_some_and(|p| rotate_at(app, &layout, page, s, p).is_some()));
                let up = (knob - c).normalized();
                painter.line_segment([knob - up * ROTATE_GAP, knob], Stroke::new(1.2, accent));
                painter.circle(knob, HANDLE * 0.6, if on { accent } else { Color32::WHITE }, Stroke::new(1.5, accent));
            }
        }
        if i == 0 {
            let label = if one {
                e.name.clone()
            } else {
                crate::i18n::fmt("{name} and {n} more", &[("name", &e.name), ("n", &(sel.len() - 1).to_string())])
            };
            let top = k.iter().fold(k[0], |a, p| if p.y < a.y { *p } else { a });
            painter.text(
                pos2(k.iter().map(|p| p.x).fold(f32::MAX, f32::min), top.y - 4.0),
                Align2::LEFT_BOTTOM,
                label,
                regular(11.0),
                accent,
            );
        }
    }
    // An element being turned: where it will point, and its angle.
    if let (Some(Drag::Rotate(id, a0, from)), Some(p)) = (app.layout.drag, pointer)
        && let Some(e) = layout.element(id)
    {
        let c = to_screen(page, s, e.center());
        let a = turned(a0, from, screen_angle(c, p), ui.input(|i| i.modifiers.shift));
        if let ElementKind::Line { dir, .. } = e.kind {
            let half = (line_length((e.x, e.y, e.w, e.h), dir) * f64::from(s) / 2.0) as f32;
            let u = vec2(a.to_radians().cos() as f32, -(a.to_radians().sin() as f32));
            painter.line_segment([c - u * half, c + u * half], Stroke::new(2.0, accent));
            painter.circle_filled(c + u * half, 3.5, accent);
        } else {
            let k = screen_corners(page, s, e, Some(a));
            painter.add(egui::Shape::closed_line(k.to_vec(), Stroke::new(2.0, accent)));
        }
        painter.text(p + vec2(12.0, 12.0), Align2::LEFT_TOP, format!("{a:.0}°"), semibold(12.0), accent);
    }
    // A box being dragged out to select what's inside it.
    if let (Some(Drag::Marquee(start, _)), Some(p)) = (app.layout.drag, pointer) {
        let r = Rect::from_two_pos(to_screen(page, s, start), p);
        painter.rect_filled(r, 0.0, accent.gamma_multiply(0.08));
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Middle);
    }
    // A frame being drawn.
    if app.layout.tool.is_some() && !app.layout.sketch.is_empty() {
        let mut pts: Vec<Pos2> = app.layout.sketch.iter().map(|p| to_screen(page, s, *p)).collect();
        if let (Some("polygon"), Some(p)) = (app.layout.tool, pointer) {
            pts.push(p);
        }
        painter.add(egui::Shape::closed_line(pts, Stroke::new(2.0, Color32::from_rgb(0x16, 0x9A, 0x5A))));
    }
    // The rulers, and room for them beside the notes drawn over the view.
    let inset = if app.layout.no_rulers {
        0.0
    } else {
        rulers(&painter, area, page, s, &layout, pointer, &t);
        RULER
    };
    // Status: page size, zoom and hint.
    let hint = match app.layout.tool {
        Some("polygon") => tl!("Click the frame's corners · double-click to finish · Esc to stop").to_string(),
        Some(_) => tl!("Drag the frame's outline · Esc to stop").to_string(),
        None if app.layout.navigate => tl!("Drag a map frame to pan its map, wheel to zoom it · Esc to stop").to_string(),
        None if app.layout.selected.is_some_and(|id| locked(&layout, id)) => {
            tl!("Locked: unlock it in the Format pane to move or change it").to_string()
        }
        None => crate::i18n::fmt(
            "{w} × {h} mm · {zoom}% · drag to move (Alt: no snapping), Shift-drag a map frame to pan it, wheel to zoom",
            &[
                ("w", &format!("{:.0}", layout.width)),
                ("h", &format!("{:.0}", layout.height)),
                ("zoom", &format!("{:.0}", (if app.layout.zoom > 0.0 { app.layout.zoom } else { 1.0 }) * 100.0)),
            ],
        ),
    };
    let g = painter.layout_no_wrap(hint, regular(11.5), t.text_dim);
    let bg = Rect::from_min_size(pos2(area.min.x + inset + 10.0, area.max.y - g.size().y - 14.0), g.size() + vec2(14.0, 8.0));
    painter.rect_filled(bg, CornerRadius::same(10), t.ribbon.gamma_multiply(0.9));
    painter.galley(bg.min + vec2(7.0, 4.0), g, t.text_dim);
    if layout.series.is_some()
        && let Some((total, name)) = series_label(app, li)
    {
        let page_no = layout.series.as_ref().map_or(0, |s| s.page);
        let label = crate::i18n::fmt(
            "Page {n} of {total}: {name}",
            &[("n", &(page_no + 1).to_string()), ("total", &total.to_string()), ("name", &name)],
        );
        let g = painter.layout_no_wrap(label, semibold(12.0), t.text);
        let bg = Rect::from_min_size(pos2(area.min.x + inset + 10.0, area.min.y + inset + 10.0), g.size() + vec2(16.0, 10.0));
        painter.rect_filled(bg, CornerRadius::same(10), t.ribbon);
        painter.galley(bg.min + vec2(8.0, 5.0), g, t.text);
    }
}

/// The map series page count and the current page's name, worked out once per project change.
fn series_label(app: &mut RustGisApp, li: usize) -> Option<(usize, String)> {
    let rev = app.session.revision;
    if let Some((i, r, n, name)) = &app.layout.series
        && *i == li
        && *r == rev
    {
        return Some((*n, name.clone()));
    }
    let page = app.session.project.layouts.get(li).and_then(|l| l.series.as_ref()).map_or(0, |s| s.page);
    let (n, name) = rustgis_engine::cmd::layout::series_info(&app.session, li, page)?;
    app.layout.series = Some((li, rev, n, name.clone()));
    Some((n, name))
}

/// Page values for dynamic text in the view.
pub fn page_info(app: &mut RustGisApp, li: usize) -> rustgis_engine::render::layout::PageInfo {
    let page = app.session.project.layouts.get(li).and_then(|l| l.series.as_ref()).map_or(0, |s| s.page);
    let (pages, page_name) = series_label(app, li).unwrap_or((1, String::new()));
    rustgis_engine::render::layout::PageInfo { page: page + 1, pages, page_name }
}

/// The map frame under a screen point, when map frames are being navigated.
fn nav_frame(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, p: Pos2) -> Option<u32> {
    let (x, y) = to_mm(page, s, p);
    layout.hit(x, y).filter(|id| {
        app.layout.navigate && layout.element(*id).is_some_and(|e| !e.locked && matches!(e.kind, ElementKind::MapFrame { .. }))
    })
}

/// Whether the element is locked (#536): selected, but neither moved nor resized.
fn locked(layout: &Layout, id: u32) -> bool {
    layout.element(id).is_some_and(|e| e.locked)
}

fn interact(app: &mut RustGisApp, ui: &mut Ui, resp: &egui::Response, layout: &Layout, li: usize, page: Rect, s: f32) {
    // A map frame's wheel zoom is applied once the wheel has stopped.
    let now = ui.input(|i| i.time);
    // A pan ending this frame is worked out from `layout` as it was: applying the zoom first
    // would have the pan overwrite its centre. It waits a frame, for the layout after the pan.
    let pan_ends = resp.drag_stopped() && matches!(app.layout.drag, Some(Drag::PanFrame(..)));
    if let Some((name, id, k, anchor, last)) = app.layout.frame_zoom.clone() {
        if name != layout.name {
            // Another layout is open now: the zoom was for a frame of that one.
            app.layout.frame_zoom = None;
        } else if now - last >= FRAME_ZOOM_SETTLE && !pan_ends {
            app.layout.frame_zoom = None;
            zoom_frame(app, layout, li, id, k, anchor);
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(FRAME_ZOOM_SETTLE));
        }
    }
    // Wheel zooms the page about the pointer, or a map frame's map when navigating.
    if let Some(p) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.zoom_delta().ln() * 300.0);
        if scroll.abs() > 0.1
            && let Some(id) = nav_frame(app, layout, page, s, p)
        {
            // Wheel up zooms in: a smaller scale denominator.
            let step = f64::from(-scroll * 0.0028).exp();
            let same = app.layout.frame_zoom.as_ref().filter(|z| z.1 == id && z.0 == layout.name).map(|z| z.2);
            let k = match same {
                Some(k) => k * step,
                None => {
                    // Another frame's zoom still waiting is applied, not lost, before this one's.
                    if let Some((name, zid, zk, anchor, _)) = app.layout.frame_zoom.take()
                        && name == layout.name
                    {
                        zoom_frame(app, layout, li, zid, zk, anchor);
                    }
                    step
                }
            };
            app.layout.frame_zoom = Some((layout.name.clone(), id, k.clamp(1e-3, 1e3), to_mm(page, s, p), now));
        } else if scroll.abs() > 0.1 {
            let old = if app.layout.zoom > 0.0 { app.layout.zoom } else { 1.0 };
            let new = (old * (scroll * 0.0028).exp()).clamp(0.2, 12.0);
            // Keep the page point under the pointer fixed.
            let centre = resp.rect.center() + app.layout.pan;
            app.layout.pan += (p - centre) * (1.0 - new / old);
            app.layout.zoom = new;
        }
    }
    // The page moves under a drag when the properties open beside it (the drag selects the
    // element): keep the drag's start under the same screen point, so the element follows the
    // pointer rather than the page.
    if let Some((old, old_s)) = app.layout.drag_page
        && (old, old_s) != (page, s)
    {
        let rebase = |p| to_mm(page, s, to_screen(old, old_s, p));
        app.layout.drag = match app.layout.drag {
            Some(Drag::Move(id, p)) => Some(Drag::Move(id, rebase(p))),
            Some(Drag::Resize(id, h, p)) => Some(Drag::Resize(id, h, rebase(p))),
            Some(Drag::PanFrame(id, p)) => Some(Drag::PanFrame(id, rebase(p))),
            d => d,
        };
    }
    app.layout.drag_page = Some((page, s));
    let pointer_mm = resp.interact_pointer_pos().map(|p| to_mm(page, s, p));
    // Drawing a polygon or lasso frame.
    if let Some(tool) = app.layout.tool {
        if tool == "polygon" {
            if resp.double_clicked() {
                finish_frame(app, li, "polygon");
            } else if resp.clicked()
                && let Some(p) = pointer_mm
            {
                app.layout.sketch.push(p);
            }
        } else {
            if resp.drag_started() {
                app.layout.sketch.clear();
                app.layout.drag = Some(Drag::Lasso);
            }
            if app.layout.drag == Some(Drag::Lasso)
                && let Some(p) = pointer_mm
                && app.layout.sketch.last().is_none_or(|l| ((l.0 - p.0).powi(2) + (l.1 - p.1).powi(2)).sqrt() > 1.0)
            {
                app.layout.sketch.push(p);
            }
            if resp.drag_stopped() {
                app.layout.drag = None;
                finish_frame(app, li, "lasso");
            }
        }
        return;
    }
    if resp.is_pointer_button_down_on()
        && let Some(o) = ui.input(|i| i.pointer.press_origin())
    {
        app.layout.pressed = Some(o);
    }
    // Rulers and guides show they can be dragged.
    if let Some(p) = resp.hover_pos() {
        let vertical = match app.layout.drag {
            Some(Drag::NewGuide(v) | Drag::Guide(v, _)) => Some(v),
            Some(_) => None,
            None => ruler_at(&app.layout, resp.rect, p).or_else(|| guide_at(layout, page, s, p).map(|g| g.0)),
        };
        let navigating = app.layout.drag.is_none() && nav_frame(app, layout, page, s, p).is_some();
        let handle = match app.layout.drag {
            Some(Drag::Resize(_, h, _)) => Some(h),
            None if !navigating => handle_at(app, layout, page, s, p).map(|(_, h)| h),
            _ => None,
        };
        let rotating = matches!(app.layout.drag, Some(Drag::Rotate(..)));
        let knob = app.layout.drag.is_none() && rotate_at(app, layout, page, s, p).is_some();
        match (vertical, handle) {
            _ if rotating => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
            _ if knob => ui.ctx().set_cursor_icon(CursorIcon::Grab),
            (_, Some(h)) => ui.ctx().set_cursor_icon(handle_cursor(h)),
            (Some(true), _) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
            (Some(false), _) => ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical),
            _ if matches!(app.layout.drag, Some(Drag::PanFrame(..))) => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
            _ if navigating => ui.ctx().set_cursor_icon(CursorIcon::Grab),
            _ if matches!(app.layout.drag, Some(Drag::Move(..))) => ui.ctx().set_cursor_icon(CursorIcon::Move),
            _ if app.layout.drag.is_none() && app.layout.tool.is_none() && {
                let (x, y) = to_mm(page, s, p);
                layout.hit(x, y).is_some_and(|id| !locked(layout, id))
            } =>
            {
                ui.ctx().set_cursor_icon(CursorIcon::Move)
            }
            _ => {}
        }
    }
    // A click on a ruler, or on the selected element's handle (which can stick out over another
    // element), keeps the selection: where the click started, as the pointer may have moved a
    // little by the release. A press serves one click or drag, never a later one.
    let pressed = if resp.clicked() || resp.drag_started() { app.layout.pressed.take() } else { None };
    if resp.clicked()
        && let Some(p) = pointer_mm
        && pressed.or(resp.interact_pointer_pos()).is_none_or(|pos| {
            ruler_at(&app.layout, resp.rect, pos).is_none()
                && handle_at(app, layout, page, s, pos).is_none()
                && rotate_at(app, layout, page, s, pos).is_none()
        })
    {
        // Shift or Ctrl (⌘) adds the element clicked to the selection, or takes it out; a plain
        // click selects it alone (its group with it), or nothing.
        let adding = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        match layout.hit(p.0, p.1) {
            Some(id) if adding => app.layout.toggle(layout, id),
            None if adding => {}
            hit => app.layout.select(layout, hit),
        }
        // A click on the element already selected brings back the pane closed over it.
        if app.layout.selected.is_some_and(|id| app.layout.formatted == Some((li, id))) {
            app.ui.open_right(crate::RightPane::Format);
        }
    }
    if resp.double_clicked()
        && let Some(id) = app.layout.selected
        && matches!(layout.element(id).map(|e| &e.kind), Some(ElementKind::MapFrame { .. }))
    {
        // Double-clicking a map frame zooms it to the map view.
        let _ = app.run("layout.frameZoom", json!({"layout": li, "element": id, "to": "view"}));
    }
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        // What was pressed decides the drag, and the drag runs from where it was pressed: egui
        // starts one only once the pointer has moved, by then often off a small handle (which
        // then moved the element instead of resizing it).
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
        let p = to_mm(page, s, origin);
        // Navigating, a map frame pans as Shift-dragging one does; its handles stay put.
        let shift = ui.input(|i| i.modifiers.shift) || app.layout.navigate;
        let handle = handle_at(app, layout, page, s, origin);
        let ruler = ruler_at(&app.layout, resp.rect, origin);
        let guide = guide_at(layout, page, s, origin);
        // The rotation knob, outside the element's box, comes before anything under it.
        let rotate = ruler.is_none().then(|| rotate_at(app, layout, page, s, origin)).flatten();
        // Ctrl (⌘) drags out a box selecting what's inside it, Shift as well adding to the
        // selection.
        let (command, adding) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        app.layout.drag = if let Some((id, c)) = rotate {
            element_angle(layout, id).map(|a| Drag::Rotate(id, a, screen_angle(c, origin)))
        } else if command && ruler.is_none() && handle.is_none() && app.layout.tool.is_none() {
            Some(Drag::Marquee(p, adding))
        } else {
            match (ruler, handle, guide, layout.hit(p.0, p.1)) {
                (Some(vertical), ..) => Some(Drag::NewGuide(vertical)),
                (None, Some((id, h)), ..) => Some(Drag::Resize(id, h, p)),
                (None, None, Some((vertical, i)), _) => Some(Drag::Guide(vertical, i)),
                // A locked element is selected and stays put, its map too.
                (None, None, None, Some(id)) if locked(layout, id) => {
                    app.layout.select(layout, Some(id));
                    None
                }
                (None, None, None, Some(id))
                    if shift && matches!(layout.element(id).map(|e| &e.kind), Some(ElementKind::MapFrame { .. })) =>
                {
                    app.layout.select(layout, Some(id));
                    Some(Drag::PanFrame(id, p))
                }
                (None, None, None, Some(id)) => {
                    // Dragging one of the selection moves it all; another is selected first.
                    if !app.layout.selection(layout).contains(&id) {
                        app.layout.select(layout, Some(id));
                    }
                    Some(Drag::Move(id, p))
                }
                (None, None, None, None) => Some(Drag::Pan),
            }
        };
    }
    if app.layout.drag == Some(Drag::Pan) {
        app.layout.pan += resp.drag_delta();
    }
    if resp.drag_stopped() {
        let end = pointer_mm.or_else(|| ui.input(|i| i.pointer.latest_pos()).map(|p| to_mm(page, s, p)));
        match (app.layout.drag, end) {
            (Some(Drag::Move(_, start)), Some(end)) => {
                // A press that hardly moved is a click, not a move.
                if ((end.0 - start.0).abs() > 0.2 || (end.1 - start.1).abs() > 0.2)
                    && let Some((id, (x, y, _, _), _, _)) = drag_landing(&app.layout, ui, layout, s, end)
                {
                    // With others selected (or in its group), they all move, in one step.
                    let sel = app.layout.selection(layout);
                    if sel.len() > 1
                        && sel.contains(&id)
                        && let Some(e) = layout.element(id)
                    {
                        let _ = app.run("layout.moveElements", json!({"layout": li, "elements": sel, "dx": x - e.x, "dy": y - e.y}));
                    } else {
                        let _ = app.run("layout.update", json!({"layout": li, "element": id, "x": x, "y": y}));
                    }
                }
            }
            (Some(Drag::Marquee(start, adding)), Some(end)) => {
                // What lies wholly inside the box.
                let (x0, x1) = (start.0.min(end.0), start.0.max(end.0));
                let (y0, y1) = (start.1.min(end.1), start.1.max(end.1));
                let inside: Vec<u32> = layout
                    .elements
                    .iter()
                    .filter(|e| e.visible)
                    .filter(|e| {
                        let (x, y, w, h) = e.bounds();
                        x >= x0 && y >= y0 && x + w <= x1 && y + h <= y1
                    })
                    .map(|e| e.id)
                    .collect();
                let mut ids = if adding { app.layout.selection(layout) } else { Vec::new() };
                ids.extend(inside.iter().filter(|id| !ids.contains(id)).copied().collect::<Vec<_>>());
                app.layout.select_all(layout, &ids);
            }
            (Some(Drag::Resize(..)), Some(end)) => {
                if let Some((id, (x, y, w, hh), _, _)) = drag_landing(&app.layout, ui, layout, s, end) {
                    let _ = app.run("layout.update", json!({"layout": li, "element": id, "x": x, "y": y, "w": w, "h": hh}));
                }
            }
            (Some(Drag::NewGuide(v)), _) => {
                let at = ui.input(|i| i.pointer.latest_pos());
                drop_guide(app, layout, li, page, s, resp.rect, v, None, at);
            }
            (Some(Drag::Guide(v, i)), _) => {
                let at = ui.input(|i| i.pointer.latest_pos());
                drop_guide(app, layout, li, page, s, resp.rect, v, Some(i), at);
            }
            (Some(Drag::PanFrame(id, start)), Some(end)) => pan_frame(app, layout, li, id, end.0 - start.0, end.1 - start.1),
            (Some(Drag::Rotate(id, a0, from)), _) => {
                if let (Some(at), Some(e)) = (ui.input(|i| i.pointer.latest_pos()), layout.element(id)) {
                    let c = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h))).center();
                    let a = turned(a0, from, screen_angle(c, at), ui.input(|i| i.modifiers.shift));
                    if (a - a0).abs() > 0.05 {
                        // A line turns by its direction, any other element by its rotation.
                        let key = if e.kind.turns() { "rotation" } else { "angle" };
                        let _ = app.run("layout.update", json!({"layout": li, "element": id, key: a}));
                    }
                }
            }
            _ => {}
        }
        app.layout.drag = None;
    }
}

/// Moves a map frame's map by a page offset (mm).
fn pan_frame(app: &mut RustGisApp, layout: &Layout, li: usize, id: u32, dx: f64, dy: f64) {
    let Some(e) = layout.element(id) else { return };
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &e.kind else { return };
    let Some(m) = app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)) else { return };
    let mut cam = frame_camera(m, *center, *scale, *bearing, *pitch);
    // Dragged at the frame's centre, so a turned or tilted frame's map follows the pointer.
    let (w, h) = (e.w * PT_PER_MM, e.h * PT_PER_MM);
    cam.drag_px((w / 2.0, h / 2.0), (w / 2.0 + dx * PT_PER_MM, h / 2.0 + dy * PT_PER_MM), w, h);
    if let Some(ll) = m.to_lonlat(cam.center) {
        let _ = app.run("layout.update", json!({"layout": li, "element": id, "center": [ll.x, ll.y]}));
    }
}

/// Zooms a map frame's map by `k` on its scale (below 1 zooms in), keeping the map under the
/// page point `anchor` (mm) where it is.
fn zoom_frame(app: &mut RustGisApp, layout: &Layout, li: usize, id: u32, k: f64, (ax, ay): (f64, f64)) {
    let Some(e) = layout.element(id) else { return };
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &e.kind else { return };
    let Some(m) = app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)) else { return };
    let cam = frame_camera(m, *center, *scale, *bearing, *pitch);
    let new_scale = (scale * k).clamp(MIN_FRAME_SCALE, MAX_FRAME_SCALE);
    // A turned or tilted frame zooms as the map view does, about the anchor.
    if cam.bearing != 0.0 || cam.pitch != 0.0 {
        let (w, h) = (e.w * PT_PER_MM, e.h * PT_PER_MM);
        let mut c = cam;
        c.zoom_at(*scale / new_scale, (ax - e.x) * PT_PER_MM, (ay - e.y) * PT_PER_MM, w, h);
        if let Some(ll) = m.to_lonlat(c.center) {
            let _ = app.run("layout.update", json!({"layout": li, "element": id, "scale": new_scale, "center": [ll.x, ll.y]}));
        }
        return;
    }
    // The anchor's offset from the frame's centre, in points; times the resolution, map units.
    let (px, py) = ((ax - (e.x + e.w / 2.0)) * PT_PER_MM, -(ay - (e.y + e.h / 2.0)) * PT_PER_MM);
    let target = rustgis_engine::geom::Coord::new(cam.center.x + px * cam.res, cam.center.y + py * cam.res);
    // Centre the frame so the anchor's map point stays put. The resolution for a scale depends on
    // the centre's latitude (Web Mercator stretches away from the equator), so solve for the
    // centre's y by Newton's method, then place x with the resolution there.
    let res_at =
        |x: f64, y: f64| m.to_lonlat(rustgis_engine::geom::Coord::new(x, y)).map(|ll| frame_camera(m, ll, new_scale, 0.0, 0.0).res);
    let mut cx = cam.center.x;
    let mut cy = target.y - py * cam.res * new_scale / scale;
    for _ in 0..3 {
        for _ in 0..20 {
            let h = cy.abs().max(1.0) * 1e-7;
            let (Some(r0), Some(r1)) = (res_at(cx, cy), res_at(cx, cy + h)) else { return };
            let (f0, f1) = (cy + py * r0 - target.y, cy + h + py * r1 - target.y);
            let slope = (f1 - f0) / h;
            if !slope.is_finite() || slope.abs() < 1e-12 {
                break;
            }
            cy -= f0 / slope;
            if f0.abs() < r0 * 1e-3 {
                break;
            }
        }
        let Some(r) = res_at(cx, cy) else { return };
        cx = target.x - px * r;
    }
    let Some(ll) = m.to_lonlat(rustgis_engine::geom::Coord::new(cx, cy)) else { return };
    let _ = app.run("layout.update", json!({"layout": li, "element": id, "scale": new_scale, "center": [ll.x, ll.y]}));
}

/// Adds a polygon or lasso frame from the sketch.
fn finish_frame(app: &mut RustGisApp, li: usize, shape: &str) {
    let pts = std::mem::take(&mut app.layout.sketch);
    app.layout.tool = None;
    if pts.len() < 3 {
        return;
    }
    let (x0, y0) = pts.iter().fold((f64::MAX, f64::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
    let (x1, y1) = pts.iter().fold((f64::MIN, f64::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let unit: Vec<[f64; 2]> = pts.iter().map(|p| [(p.0 - x0) / w, (p.1 - y0) / h]).collect();
    if let Ok(v) = app.run("layout.addFrame", json!({"layout": li, "shape": shape, "points": unit, "x": x0, "y": y0, "w": w, "h": h})) {
        app.layout.selected = v["element"].as_u64().map(|v| v as u32);
    }
}

/// Saves the text typed into fields the Format pane didn't draw this frame: the selection or the
/// layout changed, or the pane was closed or hidden, before the field could notice it lost focus.
/// Runs once a frame, after the panes and the view.
pub(crate) fn save_drafts(app: &mut RustGisApp) {
    follow_project(app);
    let shown = app.layout.drawn.take();
    let stale: Vec<_> = app.layout.typing.keys().filter(|(l, id, _)| Some((*l, *id)) != shown).copied().collect();
    for key in stale {
        let Some(text) = app.layout.typing.remove(&key) else { continue };
        let (l, id, field) = key;
        let current = app.session.project.layouts.get(l).and_then(|x| x.element(id)).and_then(|e| match (field, &e.kind) {
            ("name", _) => Some(e.name.clone()),
            ("text", ElementKind::Text { text, .. }) => Some(text.clone()),
            ("title", ElementKind::Legend { title, .. } | ElementKind::Colorbar { title, .. }) => Some(title.clone()),
            _ => None,
        });
        if current.is_some_and(|c| c != text) {
            let _ = app.run("layout.update", json!({"layout": l, "element": id, field: text}));
        }
    }
}

/// A text field whose typing is kept in the view's state while it has focus. Returns the new text
/// when the field loses focus with a change.
/// A gallery of a north arrow's or scale bar's styles drawn as they print (`arrow`: north
/// arrows), the one in use marked; the style clicked.
fn style_gallery(ui: &mut Ui, items: &[(&'static str, &'static str)], now: &str, arrow: bool) -> Option<&'static str> {
    let (w, h, per_row) = if arrow { (40.0, 56.0, 5) } else { (104.0, 26.0, 2) };
    let mut picked = None;
    egui::Grid::new(("style_gallery", arrow)).spacing(vec2(4.0, 4.0)).show(ui, |ui| {
        for (i, (code, label)) in items.iter().enumerate() {
            let tex = swatch(ui.ctx(), code, arrow, (w, h));
            let on = *code == now;
            let mut button = egui::Button::image(egui::Image::new(&tex).fit_to_exact_size(vec2(w, h))).fill(Color32::WHITE).selected(on);
            if on {
                button = button.stroke(Stroke::new(2.0, ui.visuals().selection.stroke.color));
            }
            let r = ui.add(button).on_hover_text(tl!(label));
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, tl!(label)));
            if r.clicked() && !on {
                picked = Some(*code);
            }
            if (i + 1) % per_row == 0 {
                ui.end_row();
            }
        }
    });
    if let Some((_, label)) = items.iter().find(|(c, _)| *c == now) {
        ui.label(egui::RichText::new(tl!(label)).weak().small());
    }
    picked
}

/// A style's picture, drawn once at twice its size and kept.
fn swatch(ctx: &egui::Context, code: &str, arrow: bool, (w, h): (f32, f32)) -> egui::TextureHandle {
    let key = egui::Id::unique(("layout_style_swatch", arrow, code.to_string()));
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return t;
    }
    let (pw, ph) = ((w * 2.0) as u32, (h * 2.0) as u32);
    let pm = if arrow {
        ArrowStyle::from_name(code).and_then(|s| rustgis_engine::render::layout::north_arrow_swatch(s, pw, ph))
    } else {
        ScaleBarStyle::from_name(code).and_then(|s| rustgis_engine::render::layout::scale_bar_swatch(s, pw, ph))
    };
    let img = match pm {
        Some(pm) => egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()),
        None => egui::ColorImage::new([1, 1], vec![Color32::TRANSPARENT]),
    };
    let t = ctx.load_texture(format!("layout-style-{code}"), img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(key, t.clone()));
    t
}

/// The type of a north arrow's letter or a scale bar's numbers and unit: size (automatic at 0),
/// colour (the element's `own` unless set), weight, slant and family.
/// The type of a north arrow's letter or a scale bar's numbers (its options `textSize`,
/// `textColor`, `bold`…), or, with `wrap`, of a legend's title, layer names or labels (an object
/// of `size`, `color`, `bold`… under that option).
#[allow(clippy::too_many_arguments)]
fn label_type(
    app: &mut RustGisApp,
    ui: &mut Ui,
    (li, id): (usize, u32),
    text: &LabelText,
    own: rustgis_engine::map::Rgba,
    title: &str,
    wrap: Option<&'static str>,
    update: &dyn Fn(&mut RustGisApp, Value),
) {
    // A north arrow's or scale bar's one type open; a legend's three folded until opened.
    ui.add_space(4.0);
    let Some(changed) = egui::CollapsingHeader::new(tl!(title))
        .id_salt(("format_section", title))
        .default_open(wrap.is_none())
        .show(ui, |ui| type_form(app, ui, ("layout_label_type", li, id, wrap), text, own, "Use the element's color"))
        .body_returned
        .flatten()
    else {
        return;
    };
    match wrap {
        // The legend's: an object under its option.
        Some(key) => update(app, json!({key: changed})),
        // A north arrow's or scale bar's: options of the element's own.
        None => {
            let options: serde_json::Map<String, Value> = changed
                .into_iter()
                .map(|(k, v)| {
                    let k = match k.as_str() {
                        "size" => "textSize".to_string(),
                        "color" => "textColor".to_string(),
                        _ => k,
                    };
                    (k, v)
                })
                .collect();
            update(app, Value::Object(options));
        }
    }
}

/// The form for a type `text` (size, colour, weight, slant and family) and a button that sets its
/// colour back to `own` (labelled `reset`): what was changed and let go, as an object of `size`,
/// `color` (`""` for `own`), `bold`, `italic`, `font` and `fallback`; `None` while nothing was.
fn type_form(
    app: &mut RustGisApp,
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    text: &LabelText,
    own: rustgis_engine::map::Rgba,
    reset: &'static str,
) -> Option<serde_json::Map<String, Value>> {
    let mut none = String::new();
    let (mut sz, mut c, mut bb, mut it) = (text.size, crate::theme::c32(text.color.unwrap_or(own)), text.bold, text.italic);
    let (mut fam, mut named) = (text.font, text.font_name.clone());
    let fields = crate::textform::Fields {
        text: &mut none,
        size: &mut sz,
        color: Some(&mut c),
        bold: Some(&mut bb),
        italic: Some(&mut it),
        align: None,
        letter_spacing: None,
        font: Some(&mut fam),
        font_name: Some(&mut named),
        named_fonts: true,
        select: false,
    };
    let settled = crate::textform::show_type(ui, salt, fields, true);
    let reset = text.color.is_some() && ui.small_button(tl!(reset)).clicked();
    let mut out = serde_json::Map::new();
    if settled.size {
        out.insert("size".into(), json!(sz));
    }
    if let Some(resp) = &settled.color {
        crate::widgets::one_step_per_press(app, resp);
        out.insert("color".into(), json!(crate::theme::rgba(c).hex()));
    }
    if reset {
        out.insert("color".into(), json!(""));
    }
    if settled.bold {
        out.insert("bold".into(), json!(bb));
    }
    if settled.italic {
        out.insert("italic".into(), json!(it));
    }
    if settled.font {
        out.insert("font".into(), json!(named.as_deref().unwrap_or(fam.code())));
        if named.is_some() {
            out.insert("fallback".into(), json!(fam.code()));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A legend's items: for each layer it can list, its name and its classes' labels as the legend
/// shows them, each typed over (empty shows the layer's own again), a small picture of each
/// class's symbol, and a button that opens the layer's symbology to change the symbols
/// themselves (the map and every legend show those).
fn legend_items(
    app: &mut RustGisApp,
    ui: &mut Ui,
    (li, id): (usize, u32),
    mi: usize,
    look: &LegendLook,
    exclude: &[String],
    update: &dyn Fn(&mut RustGisApp, Value),
) {
    use rustgis_engine::data::GeometryType;
    // The map's layers the legend can list, top first, as it lists them.
    let layers: Vec<(u32, String, GeometryType, Vec<(String, rustgis_engine::map::Symbol)>)> = {
        let m = &app.session.project.maps[mi];
        m.layers
            .iter()
            .rev()
            .filter(|l| !l.graphics && !l.is_raster() && !exclude.iter().any(|x| x.eq_ignore_ascii_case(&l.name)))
            .map(|l| {
                // None for a layer in one symbol: its name only.
                let entries = l.renderer.legend().into_iter().map(|e| (e.label, e.symbol)).collect();
                (l.id, l.name.clone(), l.geometry_type(), entries)
            })
            .collect()
    };
    if layers.is_empty() {
        return;
    }
    crate::textform::section(ui, "Legend items", |ui| {
        ui.label(
            egui::RichText::new(tl!("Type the text the legend shows; empty shows the layer's own. Symbology changes the symbols."))
                .small()
                .weak(),
        );
        for (lid, name, gt, entries) in &layers {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(name.as_str()).strong());
                if ui.small_button(tl!("Symbology…")).on_hover_text(tl!("Change this layer's symbols and labels")).clicked() {
                    let map_name = app.session.project.maps[mi].name.clone();
                    if app.session.project.active_map != mi {
                        let _ = app.run("map.activate", json!({"name": map_name}));
                    }
                    app.session.active_layer = Some(*lid);
                    app.ui.open_right(crate::RightPane::Symbology);
                }
            });
            egui::Grid::new(("legend_items", li, id, *lid)).num_columns(3).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
                let input_label = ui.label(tl!("Shown as"));
                if let Some(t) = legend_text_edit(ui, ("legend_name", li, id, *lid), look.name_of(name).unwrap_or(""), name, input_label.id)
                {
                    update(app, json!({"layerNames": {name.as_str(): t}}));
                }
                // A layer drawn in one symbol is named in the labels' type, the others in the
                // layers' names'.
                let base = if entries.is_empty() { &look.label } else { &look.layer };
                let own = look.name_type(name);
                if let Some(v) = item_type(app, ui, ("legend_name_type", li, id, *lid), name, own, base) {
                    update(app, json!({"layerTypes": {name.as_str(): v}}));
                }
                ui.end_row();
                for (label, symbol) in entries {
                    // The class's symbol and own label, then the label the legend shows.
                    let own = ui
                        .horizontal(|ui| {
                            symbol_swatch(ui, *gt, symbol);
                            ui.add(egui::Label::new(label.as_str()).truncate())
                        })
                        .inner;
                    let shown = look.label_of(name, label).unwrap_or("");
                    if let Some(t) = legend_text_edit(ui, ("legend_label", li, id, *lid, label.as_str()), shown, label, own.id) {
                        update(app, json!({"classLabels": {name.as_str(): {label.as_str(): t}}}));
                    }
                    let set = look.label_type(name, label);
                    if let Some(v) = item_type(app, ui, ("legend_label_type", li, id, *lid, label.as_str()), label, set, &look.label) {
                        update(app, json!({"classTypes": {name.as_str(): {label.as_str(): v}}}));
                    }
                    ui.end_row();
                }
            });
        }
        ui.add_space(8.0);
        let convert = ui.button(tl!("Convert to Graphics")).on_hover_text(tl!(
            "Replace the legend with a text for each name and label and a patch for each symbol, grouped: Ungroup to move and restyle them one by one. They no longer follow the layers."
        ));
        if convert.clicked() {
            match app.run("layout.convertLegend", json!({"layout": li, "element": id})) {
                Ok(r) => {
                    // The graphics selected in the legend's place.
                    let first = r["elements"].as_array().and_then(|a| a.first()).and_then(Value::as_u64).map(|v| v as u32);
                    app.layout.selected = first;
                }
                Err(e) => {
                    let text = crate::i18n::fmt("Couldn't convert the legend: {error}", &[("error", tl_raw!(e.as_str()))]);
                    app.session.notify(text, true);
                }
            }
        }
    });
}

/// A small button that gives a layer's name or a class's label a type of its own, and the type
/// form under it while open: `set` its own type (`None` while it has none), `base` the legend's
/// it is shown in otherwise. The change to send (an object of `size`, `color`, … or `null` for
/// the legend's type again), once one is made.
fn item_type(
    app: &mut RustGisApp,
    ui: &mut Ui,
    key: impl std::hash::Hash + std::fmt::Debug + Clone,
    item: &str,
    set: Option<&LabelText>,
    base: &LabelText,
) -> Option<Value> {
    let open_id = egui::Id::unique(("legend_item_type", key.clone()));
    let mut open = ui.data_mut(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    let tip = if set.is_some() { tl!("Its own type: change it, or use the legend's again") } else { tl!("Give it a type of its own") };
    let r = ui.selectable_label(open || set.is_some(), "Aa").on_hover_text(tip);
    // Named for screen readers by the item it styles.
    let name = crate::i18n::fmt("Type of {item}", &[("item", item)]);
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, open || set.is_some(), &name));
    if r.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(open_id, open));
    }
    if !open {
        return None;
    }
    ui.end_row();
    ui.label("");
    let dark = rustgis_engine::map::Rgba::rgb(0x22, 0x22, 0x22);
    let mut out = None;
    ui.vertical(|ui| {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            if let Some(changed) = type_form(app, ui, ("legend_item_form", key), set.unwrap_or(base), dark, "Use the legend's color") {
                out = Some(Value::Object(changed));
            }
            if set.is_some() && ui.small_button(tl!("Use the legend's type")).clicked() {
                out = Some(Value::Null);
            }
        });
    });
    out
}

/// A small picture of a symbol, as the legend draws it.
fn symbol_swatch(ui: &mut Ui, gt: rustgis_engine::data::GeometryType, s: &rustgis_engine::map::Symbol) -> egui::Response {
    use rustgis_engine::data::GeometryType;
    let (r, painter) = ui.allocate_painter(vec2(28.0, 16.0), Sense::hover());
    let rect = r.rect;
    let fill = crate::theme::c32(s.fill);
    let stroke = Stroke::new(s.stroke_width.max(0.5), crate::theme::c32(s.stroke));
    match gt {
        GeometryType::Point => {
            painter.circle(rect.center(), 5.0, fill, stroke);
        }
        GeometryType::Polyline => {
            let line = Stroke::new(s.stroke_width.clamp(1.0, 4.0), crate::theme::c32(s.stroke));
            let (l, c, rt) = (rect.left() + 2.0, rect.center().y, rect.right() - 2.0);
            painter.line_segment([pos2(l, c + 3.0), pos2(l + 9.0, c - 3.0)], line);
            painter.line_segment([pos2(l + 9.0, c - 3.0), pos2(rt, c + 1.5)], line);
        }
        _ => {
            let b = Rect::from_center_size(rect.center(), vec2(22.0, 11.0));
            painter.rect(b, CornerRadius::ZERO, fill, stroke, egui::StrokeKind::Inside);
        }
    }
    r
}

/// A one-line field for a legend text: `shown` the one set (empty when none), `own` the layer's
/// or class's own, shown greyed when none is set. The text once it is left changed, `""` to show
/// the own one again.
fn legend_text_edit(
    ui: &mut Ui,
    key: impl std::hash::Hash + std::fmt::Debug,
    shown: &str,
    own: &str,
    labelled_by: egui::Id,
) -> Option<String> {
    let id = egui::Id::unique(("legend_text", key));
    let typing = id.with("typing");
    let mut buf = if ui.memory(|m| m.has_focus(id)) {
        ui.data_mut(|d| d.get_temp::<String>(typing)).unwrap_or_else(|| shown.to_string())
    } else {
        ui.data_mut(|d| d.remove::<String>(typing));
        shown.to_string()
    };
    let r = ui
        .add(egui::TextEdit::singleline(&mut buf).id(id).hint_text(own).desired_width(ui.available_width().max(120.0)))
        .labelled_by(labelled_by);
    if r.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(typing));
        if buf.trim() != shown.trim() && !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            return Some(buf.trim().to_string());
        }
    } else if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(typing, buf));
    }
    None
}

fn text_field(
    app: &mut RustGisApp,
    ui: &mut Ui,
    key: (usize, u32, &'static str),
    current: &str,
    multiline: bool,
    label_id: egui::Id,
) -> Option<String> {
    let mut buf = app.layout.typing.remove(&key).unwrap_or_else(|| current.to_string());
    let edit = if multiline {
        egui::TextEdit::multiline(&mut buf).desired_rows(3).desired_width(240.0)
    } else {
        egui::TextEdit::singleline(&mut buf)
    };
    let r = ui.add(edit).labelled_by(label_id);
    if r.lost_focus() {
        return (buf != current).then_some(buf);
    }
    if r.has_focus() {
        app.layout.typing.insert(key, buf);
    }
    None
}

/// A map frame's grid options; `set` sends the changed ones (`false` removes the grid).
fn grid_properties(app: &mut RustGisApp, ui: &mut Ui, grid: Option<&Grid>, id: u32, set: &dyn Fn(&mut RustGisApp, Value)) {
    let mut on = grid.is_some();
    if ui.checkbox(&mut on, tl!("Show grid")).changed() {
        set(app, json!(on));
    }
    let Some(g) = grid else { return };
    // Each number kept across a drag until it is let go (#607).
    let key = |field: &str| egui::Id::unique(("grid_drag", id, field));
    let measured = g.kind == GridKind::Measured;
    egui::Grid::new(("grid_props", id)).num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
        // Latitudes and longitudes, or eastings and northings (ArcGIS Pro's measured grid).
        let input_label = ui.label(tl!("Type"));
        egui::ComboBox::from_id_salt(("grid_kind", id))
            .selected_text(tl!(g.kind.label()))
            .show_ui(ui, |ui| {
                for v in GridKind::ALL {
                    if ui.selectable_label(g.kind == v, tl!(v.label())).clicked() && g.kind != v {
                        set(app, json!({"type": v.code()}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        if measured {
            crs_row(app, ui, g, id, set);
        }
        let input_label = ui.label(tl!("Interval"));
        let mut step = g.step;
        let drag = Dragged::lend(ui, key("step"), &mut step);
        let (most, speed) = if measured {
            (rustgis_engine::map::layout::MAX_MEASURED_STEP, 10.0)
        } else {
            (rustgis_engine::map::layout::MAX_GRATICULE_STEP, 0.05)
        };
        let r = ui
            .add(egui::DragValue::new(&mut step).range(0.0..=most).speed(speed).max_decimals(4).custom_formatter(|v, _| {
                if v <= 0.0 {
                    tl!("Auto").to_string()
                } else if measured {
                    // (The system's units: metres for UTM.)
                    format!("{v}")
                } else {
                    format!("{v}°")
                }
            }))
            .on_hover_text(if measured { tl!("In the coordinate system's units: metres for UTM.") } else { tl!("In degrees.") })
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut step) && (step - g.step).abs() > 1e-9 {
            set(app, json!({"step": step}));
        }
        ui.end_row();
        let input_label = ui.label(tl!("Style"));
        egui::ComboBox::from_id_salt(("grid_style", id))
            .selected_text(tl!(g.style.label()))
            .show_ui(ui, |ui| {
                for v in GridStyle::ALL {
                    if ui.selectable_label(g.style == v, tl!(v.label())).clicked() && g.style != v {
                        set(app, json!({"style": v.code()}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        ui.label(tl!("Color"));
        let mut c = crate::theme::c32(g.color);
        let resp = crate::widgets::color_button(ui, &mut c);
        if resp.changed() {
            crate::widgets::one_step_per_press(app, &resp);
            set(app, json!({"color": crate::theme::rgba(c).hex()}));
        }
        ui.end_row();
        let input_label = ui.label(tl!("Line width"));
        let mut w = g.width;
        let drag = Dragged::lend(ui, key("width"), &mut w);
        let r = ui
            .add(egui::DragValue::new(&mut w).range(0.0..=MAX_GRID_WIDTH).speed(0.05).max_decimals(2).suffix(" pt"))
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut w) && (w - g.width).abs() > 1e-9 {
            set(app, json!({"width": w}));
        }
        ui.end_row();
        if g.style.ticks() {
            let input_label = ui.label(tl!("Tick length"));
            let mut t = g.tick_length;
            let drag = Dragged::lend(ui, key("tickLength"), &mut t);
            let r = ui
                .add(egui::DragValue::new(&mut t).range(0.0..=MAX_TICK_MM).speed(0.1).max_decimals(1).suffix(" mm"))
                .labelled_by(input_label.id);
            if drag.settled(ui, &r, &mut t) && (t - g.tick_length).abs() > 1e-9 {
                set(app, json!({"tickLength": t}));
            }
            ui.end_row();
        }
        let input_label = ui.label(tl!("Labels"));
        let mut labels = g.labels;
        if ui.checkbox(&mut labels, "").labelled_by(input_label.id).changed() {
            set(app, json!({"labels": labels}));
        }
        ui.end_row();
        if !g.labels {
            return;
        }
        let input_label = ui.label(tl!("Labels outside"));
        let mut outside = g.outside;
        if ui
            .checkbox(&mut outside, "")
            .labelled_by(input_label.id)
            .on_hover_text(tl!("Past the frame's edges, as ArcGIS and topographic maps print them; off, just inside the frame."))
            .changed()
        {
            set(app, json!({"outside": outside}));
        }
        ui.end_row();
        if measured {
            let input_label = ui.label(tl!("Short labels"));
            let mut short = g.short;
            if ui
                .checkbox(&mut short, "")
                .labelled_by(input_label.id)
                .on_hover_text(tl!("The kilometres' last two digits, as topographic maps print them: 43 for 543000."))
                .changed()
            {
                set(app, json!({"short": short}));
            }
            ui.end_row();
        } else {
            // Degrees: decimal or degrees, minutes and seconds, with hemisphere letters or signs.
            ui.label(tl!("Format"));
            ui.horizontal(|ui| {
                for (label, v, code) in [("Decimal", CoordFormat::Decimal, "decimal"), ("DMS", CoordFormat::Dms, "dms")] {
                    if ui.selectable_label(g.format == v, tl!(label)).clicked() && g.format != v {
                        set(app, json!({"format": code}));
                    }
                }
            });
            ui.end_row();
            let input_label = ui.label(tl!("N/S/E/W"));
            let mut hemi = g.hemisphere;
            if ui
                .checkbox(&mut hemi, "")
                .labelled_by(input_label.id)
                .on_hover_text(tl!("Off, west and south are negative: -112.5°."))
                .changed()
            {
                set(app, json!({"hemisphere": hemi}));
            }
            ui.end_row();
        }
        // Two rows, so the four edges fit the panel's width.
        let e = g.edges;
        let all = [("Top", "top", e.top), ("Bottom", "bottom", e.bottom), ("Left", "left", e.left), ("Right", "right", e.right)];
        for (row, label) in [(0, tl!("Edges")), (2, "")] {
            ui.label(label);
            ui.horizontal(|ui| {
                for (i, (label, _, shown)) in all.iter().enumerate().skip(row).take(2) {
                    if ui.selectable_label(*shown, tl!(label)).clicked() {
                        let names: Vec<&str> = all
                            .iter()
                            .enumerate()
                            .filter(|(j, (_, _, s))| if *j == i { !*s } else { *s })
                            .map(|(_, (_, n, _))| *n)
                            .collect();
                        set(app, json!({"edges": names}));
                    }
                }
            });
            ui.end_row();
        }
        ui.label(tl!("Side labels"));
        ui.horizontal(|ui| {
            let vertical = g.label_orientation == Orientation::Vertical;
            for (label, v) in [("Horizontal", false), ("Vertical", true)] {
                if ui.selectable_label(vertical == v, tl!(label)).clicked() && vertical != v {
                    set(app, json!({"labelOrientation": if v { "vertical" } else { "horizontal" }}));
                }
            }
        });
        ui.end_row();
    });
}

/// A measured grid's coordinate system, typed (EPSG:25832, WKT or a PROJ string) and set when the
/// field is left; empty is the map's when projected, else the UTM zone at the frame's centre.
fn crs_row(app: &mut RustGisApp, ui: &mut Ui, g: &Grid, id: u32, set: &dyn Fn(&mut RustGisApp, Value)) {
    let input_label = ui.label(tl!("Coordinate system"));
    let key = egui::Id::unique(("grid_crs", id));
    let current = g.crs.clone().unwrap_or_default();
    let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| current.clone());
    let r = ui
        .add(egui::TextEdit::singleline(&mut buf).hint_text(tl!("The map's, else its UTM zone")).desired_width(150.0))
        .labelled_by(input_label.id)
        .on_hover_text(tl!("EPSG:25832, WKT or a PROJ string; empty: the map's when projected, else the UTM zone at the frame's centre."));
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(key, buf));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
        if r.lost_focus() && buf.trim() != current {
            set(app, json!({"crs": buf.trim()}));
        }
    }
    ui.end_row();
}

/// The Format pane (#516): the selected layout element's properties, while its layout is on
/// screen. On the map (no layout open) or under the Model Builder, it waits for a selection.
pub fn format(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let on_screen = app.session.active_layout.filter(|_| !app.mb.shown());
    let layout = on_screen.and_then(|li| Some((li, shown_layout(app, li)?)));
    let selected = layout.as_ref().and_then(|(li, l)| Some((*li, app.layout.selected.filter(|id| l.element(*id).is_some())?)));
    // More than one selected: what they can do together.
    if let Some((li, l)) = layout.as_ref()
        && app.layout.selection(l).len() > 1
    {
        let (li, l) = (*li, l.clone());
        app.layout.drawn = selected;
        many(app, ui, &l, li);
        return;
    }
    match (layout, selected) {
        (Some((li, layout)), Some(drawn)) => {
            app.layout.drawn = Some(drawn);
            let Some(e) = layout.element(drawn.1) else { return };
            // Where it is ("Layout: Text"), as the pane also formats a text on the map ("Map: Text").
            let heading = crate::i18n::fmt("Layout: {kind}", &[("kind", tl_raw!(e.kind.label()))]);
            ui.label(egui::RichText::new(heading).font(semibold(14.0)));
            ui.separator();
            // Locked (#536), its properties show but take no change until it is unlocked.
            let mut lock = e.locked;
            if ui
                .checkbox(&mut lock, tl!("Locked"))
                .on_hover_text(tl!("Keep it from being moved, resized, deleted or edited by mistake"))
                .changed()
            {
                let _ = app.run("layout.update", json!({"layout": li, "element": e.id, "locked": lock}));
            }
            ui.add_enabled_ui(!e.locked, |ui| properties(app, ui, &layout, li));
            element_actions(app, ui, &layout, li, e.id);
        }
        _ => {
            ui.label(egui::RichText::new(tl!("Select an element to format it.")).color(t.text_dim));
        }
    }
}

/// The selected element's properties.
fn properties(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize) {
    let Some(e) = app.layout.selected.and_then(|id| layout.element(id)).cloned() else { return };
    let id = e.id;
    // Each number kept across a drag until it is let go (#607): one key per layout, element and field.
    let drag_key = |field: &str| egui::Id::unique(("layout_drag", li, id, field));
    let update = |app: &mut RustGisApp, p: Value| {
        let mut p = p;
        p["layout"] = json!(li);
        p["element"] = json!(id);
        let _ = app.run("layout.update", p);
    };
    crate::textform::section(ui, "Element", |ui| {
        egui::Grid::new(("layout_props", id)).num_columns(2).min_col_width(crate::textform::LABEL_W).spacing(vec2(8.0, 6.0)).show(
            ui,
            |ui| {
                let input_label = ui.label(tl!("Name"));
                if let Some(name) = text_field(app, ui, (li, id, "name"), &e.name, false, input_label.id) {
                    update(app, json!({"name": name}));
                }
                ui.end_row();
                for (label, key, v) in [("X (mm)", "x", e.x), ("Y (mm)", "y", e.y), ("Width (mm)", "w", e.w), ("Height (mm)", "h", e.h)] {
                    let input_label = ui.label(tl!(label));
                    let mut val = v;
                    let drag = Dragged::lend(ui, drag_key(key), &mut val);
                    let r = ui.add(egui::DragValue::new(&mut val).speed(0.5).max_decimals(1)).labelled_by(input_label.id);
                    if drag.settled(ui, &r, &mut val) && (val - v).abs() > 1e-6 {
                        update(app, json!({key: val}));
                    }
                    ui.end_row();
                }
                // Turned about its centre; a line turns by its angle, below.
                if e.kind.turns() {
                    let input_label = ui.label(tl!("Rotation (°)"));
                    let v = e.rotation;
                    let mut val = v;
                    let drag = Dragged::lend(ui, drag_key("rotation"), &mut val);
                    let r = ui
                        .add(egui::DragValue::new(&mut val).speed(1.0).range(-180.0..=180.0).max_decimals(1))
                        .labelled_by(input_label.id)
                        .on_hover_text(tl!("Degrees anticlockwise about its centre; or drag its round handle"));
                    if drag.settled(ui, &r, &mut val) && (val - v).abs() > 1e-6 {
                        update(app, json!({"rotation": val}));
                    }
                    ui.end_row();
                }
                let input_label = ui.label(tl!("Visible"));
                let mut vis = e.visible;
                if ui.checkbox(&mut vis, "").labelled_by(input_label.id).changed() {
                    update(app, json!({"visible": vis}));
                }
                ui.end_row();
                match &e.kind {
                    // Its text, size, colour and boldness: the shared form, below the grid.
                    ElementKind::Text { .. } => {}
                    ElementKind::Legend { title, all_classes, hide_other, look, .. } => {
                        let input_label = ui.label(tl!("Title"));
                        if let Some(tt) = text_field(app, ui, (li, id, "title"), title, false, input_label.id) {
                            update(app, json!({"title": tt}));
                        }
                        ui.end_row();
                        // Off, the legend lists only what the frame draws (#207).
                        let input_label = ui.label(tl!("All classes"));
                        let mut all = *all_classes;
                        if ui.checkbox(&mut all, "").labelled_by(input_label.id).changed() {
                            update(app, json!({"allClasses": all}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Hide other values"));
                        let mut hide = *hide_other;
                        if ui.checkbox(&mut hide, "").labelled_by(input_label.id).changed() {
                            update(app, json!({"hideOther": hide}));
                        }
                        ui.end_row();
                        // The patches the symbols are drawn in, and the room between the rows.
                        for (label, key, current, most) in [
                            ("Patch width", "patchWidth", look.patch_width, MAX_PATCH),
                            ("Patch height", "patchHeight", look.patch_height, MAX_PATCH),
                            ("Row spacing", "rowSpacing", look.spacing, MAX_LEGEND_SPACING),
                        ] {
                            let input_label = ui.label(tl!(label));
                            let mut v = current;
                            let drag = Dragged::lend(ui, drag_key(key), &mut v);
                            let zero = if key == "rowSpacing" { "0 pt".to_string() } else { tl!("Auto").to_string() };
                            let r = ui
                                .add(
                                    egui::DragValue::new(&mut v)
                                        .range(0.0..=most)
                                        .speed(0.2)
                                        .custom_formatter(
                                            move |v, _| if v <= 0.0 { zero.clone() } else { format!("{} pt", (v * 10.0).round() / 10.0) },
                                        )
                                        .custom_parser(|t| {
                                            let t: String = t.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
                                            Some(t.parse::<f64>().unwrap_or(0.0))
                                        }),
                                )
                                .labelled_by(input_label.id);
                            if drag.settled(ui, &r, &mut v) && (v - current).abs() > 1e-9 {
                                update(app, json!({key: v}));
                            }
                            ui.end_row();
                        }
                        let shapes = [
                            (
                                "Line patch",
                                "linePatch",
                                (look.line_patch.code(), look.line_patch.label()),
                                LinePatch::ALL.iter().map(|v| (v.code(), v.label())).collect::<Vec<_>>(),
                            ),
                            (
                                "Area patch",
                                "areaPatch",
                                (look.area_patch.code(), look.area_patch.label()),
                                AreaPatch::ALL.iter().map(|v| (v.code(), v.label())).collect(),
                            ),
                        ];
                        for (label, key, now, all) in shapes {
                            let input_label = ui.label(tl!(label));
                            egui::ComboBox::from_id_salt((key, id))
                                .selected_text(tl!(now.1))
                                .show_ui(ui, |ui| {
                                    for (code, shown) in all {
                                        if ui.selectable_label(now.0 == code, tl!(shown)).clicked() && now.0 != code {
                                            update(app, json!({key: code}));
                                        }
                                    }
                                })
                                .response
                                .labelled_by(input_label.id);
                            ui.end_row();
                        }
                    }
                    ElementKind::Colorbar { frame, layer, title, orientation, ticks, tick_side, reverse } => {
                        let input_label = ui.label(tl!("Title"));
                        if let Some(tt) = text_field(app, ui, (li, id, "title"), title, false, input_label.id) {
                            update(app, json!({"title": tt}));
                        }
                        ui.end_row();
                        // The rasters of the frame's map; none picked follows the top visible one.
                        let input_label = ui.label(tl!("Layer"));
                        let rasters: Vec<String> = match layout.element(*frame).map(|f| &f.kind) {
                            Some(ElementKind::MapFrame { map, .. }) => app
                                .session
                                .project
                                .maps
                                .iter()
                                .find(|m| m.name.eq_ignore_ascii_case(map))
                                .map(|m| m.layers.iter().filter(|l| l.is_raster()).map(|l| l.name.clone()).collect())
                                .unwrap_or_default(),
                            _ => Vec::new(),
                        };
                        let top = tl!("Top visible raster");
                        egui::ComboBox::from_id_salt(("colorbar_layer", id))
                            .selected_text(layer.as_deref().unwrap_or(top))
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(layer.is_none(), top).clicked() {
                                    update(app, json!({"layer": ""}));
                                }
                                for n in rasters {
                                    if ui.selectable_label(layer.as_deref() == Some(n.as_str()), n.as_str()).clicked() {
                                        update(app, json!({"layer": n}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        let vertical = *orientation == Orientation::Vertical;
                        ui.label(tl!("Orientation"));
                        ui.horizontal(|ui| {
                            for (label, v) in [("Horizontal", false), ("Vertical", true)] {
                                if ui.selectable_label(vertical == v, tl!(label)).clicked() && vertical != v {
                                    update(app, json!({"orientation": if v { "vertical" } else { "horizontal" }}));
                                }
                            }
                        });
                        ui.end_row();
                        ui.label(tl!("Labels"));
                        ui.horizontal(|ui| {
                            let sides =
                                if vertical { [("Right", "right"), ("Left", "left")] } else { [("Below", "below"), ("Above", "above")] };
                            for ((label, code), side) in sides.into_iter().zip([TickSide::End, TickSide::Start]) {
                                if ui.selectable_label(*tick_side == side, tl!(label)).clicked() && *tick_side != side {
                                    update(app, json!({"tickSide": code}));
                                }
                            }
                        });
                        ui.end_row();
                        let input_label = ui.label(tl!("Ticks"));
                        let mut n = *ticks as u64;
                        let drag = Dragged::lend(ui, drag_key("ticks"), &mut n);
                        let r = ui.add(egui::DragValue::new(&mut n).range(2..=MAX_COLORBAR_TICKS as u64)).labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut n) && n as usize != *ticks {
                            update(app, json!({"ticks": n}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Reverse"));
                        let mut rev = *reverse;
                        if ui.checkbox(&mut rev, "").labelled_by(input_label.id).changed() {
                            update(app, json!({"reverse": rev}));
                        }
                        ui.end_row();
                    }
                    ElementKind::MapFrame { map, scale, bearing, pitch, .. } => {
                        let input_label = ui.label(tl!("Map"));
                        let maps: Vec<String> = app.session.project.maps.iter().map(|m| m.name.clone()).collect();
                        egui::ComboBox::from_id_salt(("frame_map", id))
                            .selected_text(map.as_str())
                            .show_ui(ui, |ui| {
                                for m in maps {
                                    if ui.selectable_label(m == *map, m.as_str()).clicked() {
                                        update(app, json!({"map": m}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        let input_label = ui.label(tl!("Scale 1:"));
                        let mut sc = *scale;
                        let speed = sc * 0.01;
                        // Below 1:10, tenths: 1:0.5 is a frame at 2:1 (#902).
                        let decimals = if sc < 10.0 { 1 } else { 0 };
                        let drag = Dragged::lend(ui, drag_key("scale"), &mut sc);
                        let r = ui
                            .add(egui::DragValue::new(&mut sc).range(MIN_FRAME_SCALE..=MAX_FRAME_SCALE).speed(speed).max_decimals(decimals))
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut sc) && (sc / scale - 1.0).abs() > 1e-3 {
                            update(app, json!({"scale": sc}));
                        }
                        ui.end_row();
                        // Turned and tilted as the map view is (Ctrl+drag there; Shift+arrows).
                        for (label, key, current, range) in
                            [("Bearing", "bearing", *bearing, -180.0..=180.0), ("Pitch", "pitch", *pitch, 0.0..=85.0)]
                        {
                            let input_label = ui.label(tl!(label));
                            let mut v = current;
                            let drag = Dragged::lend(ui, drag_key(key), &mut v);
                            let r = ui
                                .add(egui::DragValue::new(&mut v).range(range).speed(0.5).max_decimals(1).suffix("°"))
                                .labelled_by(input_label.id);
                            if drag.settled(ui, &r, &mut v) && (v - current).abs() > 1e-6 {
                                update(app, json!({key: v}));
                            }
                            ui.end_row();
                        }
                    }
                    ElementKind::NorthArrow { fill, outline, label, .. } => {
                        for (label, key, current) in [("Fill", "fill", *fill), ("Outline", "outline", *outline)] {
                            ui.label(tl!(label));
                            let mut c = crate::theme::c32(current);
                            let resp = crate::widgets::color_button(ui, &mut c);
                            if resp.changed() {
                                crate::widgets::one_step_per_press(app, &resp);
                                update(app, json!({key: crate::theme::rgba(c).hex()}));
                            }
                            ui.end_row();
                        }
                        // The letter over the arrow, typed as wanted: N, น, North, ทิศเหนือ; none.
                        let input_label = ui.label(tl!("Letter"));
                        if let Some(t) = text_field(app, ui, (li, id, "label"), label, false, input_label.id) {
                            update(app, json!({"label": t}));
                        }
                        ui.end_row();
                    }
                    ElementKind::ScaleBar {
                        units,
                        divisions,
                        subdivisions,
                        division,
                        unit_label,
                        unit_text,
                        unit_place,
                        numbers,
                        color,
                        alt_color,
                        bar_height,
                        ..
                    } => {
                        let combo =
                            |app: &mut RustGisApp, ui: &mut Ui, label: &str, key: &str, now: (&str, &str), all: Vec<(&str, &str)>| {
                                let input_label = ui.label(tl!(label));
                                egui::ComboBox::from_id_salt((key, id))
                                    .selected_text(tl!(now.1))
                                    .show_ui(ui, |ui| {
                                        for (code, shown) in all {
                                            if ui.selectable_label(now.0 == code, tl!(shown)).clicked() && now.0 != code {
                                                update(app, json!({key: code}));
                                            }
                                        }
                                    })
                                    .response
                                    .labelled_by(input_label.id);
                                ui.end_row();
                            };
                        let pairs = |v: Vec<(&'static str, &'static str)>| v;
                        combo(
                            app,
                            ui,
                            "Units",
                            "units",
                            (units.code(), units.label()),
                            pairs(ScaleUnits::ALL.iter().map(|v| (v.code(), v.label())).collect()),
                        );
                        let input_label = ui.label(tl!("Divisions"));
                        let mut n = *divisions as u64;
                        let drag = Dragged::lend(ui, drag_key("divisions"), &mut n);
                        let r = ui.add(egui::DragValue::new(&mut n).range(1..=MAX_SCALE_DIVISIONS as u64)).labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut n) && n as usize != *divisions {
                            update(app, json!({"divisions": n}));
                        }
                        ui.end_row();
                        // The first division split again, as ArcGIS's subdivisions.
                        let input_label = ui.label(tl!("Subdivisions"));
                        let mut n = *subdivisions as u64;
                        let drag = Dragged::lend(ui, drag_key("subdivisions"), &mut n);
                        let none = tl!("None");
                        let r = ui
                            .add(
                                egui::DragValue::new(&mut n)
                                    .range(0..=MAX_SCALE_DIVISIONS as u64)
                                    .custom_formatter(move |v, _| if v < 2.0 { none.to_string() } else { format!("{v}") }),
                            )
                            .labelled_by(input_label.id)
                            .on_hover_text(tl!("Split the first division into this many parts"));
                        if drag.settled(ui, &r, &mut n) && n as usize != *subdivisions {
                            update(app, json!({"subdivisions": n}));
                        }
                        ui.end_row();
                        // Each division's length; 0 fits a round one to the element.
                        let input_label = ui.label(tl!("Division length"));
                        let unit = if *units == ScaleUnits::Auto { "m" } else { units.resolve(1.0).1 };
                        let mut v = *division;
                        let speed = (v * 0.01).max(0.01);
                        let drag = Dragged::lend(ui, drag_key("division"), &mut v);
                        let auto = tl!("Auto");
                        let r = ui
                            .add(
                                egui::DragValue::new(&mut v)
                                    .range(0.0..=1.0e7)
                                    .speed(speed)
                                    .custom_formatter(move |v, _| if v <= 0.0 { auto.to_string() } else { format!("{v} {unit}") })
                                    .custom_parser(|t| {
                                        let t: String = t.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
                                        Some(t.parse::<f64>().unwrap_or(0.0))
                                    }),
                            )
                            .labelled_by(input_label.id)
                            .on_hover_text(tl!("Each division's length, e.g. 1 km; Auto picks a round one that fits"));
                        if drag.settled(ui, &r, &mut v) && (v - division).abs() > 1e-9 {
                            update(app, json!({"division": v}));
                        }
                        ui.end_row();
                        combo(
                            app,
                            ui,
                            "Unit name",
                            "unitLabel",
                            (unit_label.code(), unit_label.label()),
                            pairs(UnitLabel::ALL.iter().map(|v| (v.code(), v.label())).collect()),
                        );
                        // The unit as typed: anything typed here names it (custom).
                        let input_label = ui.label(tl!("Unit text"));
                        let key = (li, id, "unitText");
                        let shown = if *unit_label == UnitLabel::Custom { unit_text.clone() } else { String::new() };
                        let mut buf = app.layout.typing.remove(&key).unwrap_or_else(|| shown.clone());
                        let abbr = if *units == ScaleUnits::Auto { "km" } else { units.resolve(1.0).1 };
                        let hint = unit_label.name(abbr, "");
                        let r = ui
                            .add(egui::TextEdit::singleline(&mut buf).hint_text(hint).desired_width(140.0))
                            .labelled_by(input_label.id)
                            .on_hover_text(tl!("Type the unit as it should read, e.g. กิโลเมตร; empty goes back to the unit name"));
                        if r.lost_focus() {
                            if buf.trim() != shown.trim() {
                                if buf.trim().is_empty() {
                                    update(app, json!({"unitText": "", "unitLabel": "abbreviation"}));
                                } else {
                                    update(app, json!({"unitText": buf}));
                                }
                            }
                        } else if r.has_focus() {
                            app.layout.typing.insert(key, buf);
                        }
                        ui.end_row();
                        combo(
                            app,
                            ui,
                            "Unit place",
                            "unitPlace",
                            (unit_place.code(), unit_place.label()),
                            pairs(UnitPlace::ALL.iter().map(|v| (v.code(), v.label())).collect()),
                        );
                        combo(
                            app,
                            ui,
                            "Numbers",
                            "numbers",
                            (numbers.code(), numbers.label()),
                            pairs(NumberPlace::ALL.iter().map(|v| (v.code(), v.label())).collect()),
                        );
                        let input_label = ui.label(tl!("Bar height"));
                        let mut v = *bar_height;
                        let drag = Dragged::lend(ui, drag_key("barHeight"), &mut v);
                        let auto = tl!("Auto");
                        let r = ui
                            .add(
                                egui::DragValue::new(&mut v)
                                    .range(0.0..=MAX_BAR_HEIGHT)
                                    .speed(0.1)
                                    .custom_formatter(
                                        move |v, _| if v <= 0.0 { auto.to_string() } else { format!("{} pt", (v * 10.0).round() / 10.0) },
                                    )
                                    .custom_parser(|t| {
                                        let t: String = t.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
                                        Some(t.parse::<f64>().unwrap_or(0.0))
                                    }),
                            )
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut v) && (v - bar_height).abs() > 1e-9 {
                            update(app, json!({"barHeight": v}));
                        }
                        ui.end_row();
                        for (label, key, current) in [("Bar color", "color", *color), ("Light color", "altColor", *alt_color)] {
                            ui.label(tl!(label));
                            let mut c = crate::theme::c32(current);
                            let resp = crate::widgets::color_button(ui, &mut c);
                            if resp.changed() {
                                crate::widgets::one_step_per_press(app, &resp);
                                update(app, json!({key: crate::theme::rgba(c).hex()}));
                            }
                            ui.end_row();
                        }
                    }
                    ElementKind::Table { rows, .. } => {
                        let input_label = ui.label(tl!("Rows"));
                        let mut n = *rows as u64;
                        let drag = Dragged::lend(ui, drag_key("rows"), &mut n);
                        let r = ui.add(egui::DragValue::new(&mut n).range(1..=500)).labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut n) && n as usize != *rows {
                            update(app, json!({"rows": n}));
                        }
                        ui.end_row();
                    }
                    ElementKind::Rectangle { fill, outline, width, .. } | ElementKind::Ellipse { fill, outline, width } => {
                        // A fill with no opacity leaves only the outline; an outline 0 wide, only the fill.
                        for (label, key, current) in [("Fill", "fill", *fill), ("Outline", "outline", *outline)] {
                            ui.label(tl!(label));
                            let mut c = crate::theme::c32(current);
                            let resp = crate::widgets::color_button(ui, &mut c);
                            if resp.changed() {
                                crate::widgets::one_step_per_press(app, &resp);
                                update(app, json!({key: crate::theme::rgba(c).hex()}));
                            }
                            ui.end_row();
                        }
                        let input_label = ui.label(tl!("Outline width"));
                        let mut wd = *width;
                        let drag = Dragged::lend(ui, drag_key("width"), &mut wd);
                        let r = ui
                            .add(egui::DragValue::new(&mut wd).range(0.0..=50.0).speed(0.1).max_decimals(1).suffix(" pt"))
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut wd) && (wd - width).abs() > 1e-6 {
                            update(app, json!({"width": wd}));
                        }
                        ui.end_row();
                    }
                    ElementKind::Line { color, width, dir, head } => {
                        ui.label(tl!("Color"));
                        let mut c = crate::theme::c32(*color);
                        let resp = crate::widgets::color_button(ui, &mut c);
                        if resp.changed() {
                            crate::widgets::one_step_per_press(app, &resp);
                            update(app, json!({"color": crate::theme::rgba(c).hex()}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Width"));
                        let mut wd = *width;
                        let drag = Dragged::lend(ui, drag_key("width"), &mut wd);
                        let r = ui
                            .add(egui::DragValue::new(&mut wd).range(0.1..=50.0).speed(0.1).max_decimals(1).suffix(" pt"))
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut wd) && (wd - width).abs() > 1e-6 {
                            update(app, json!({"width": wd}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Arrowheads"));
                        egui::ComboBox::from_id_salt(("line_head", id))
                            .selected_text(tl!(head.label()))
                            .show_ui(ui, |ui| {
                                for h in LineHead::ALL {
                                    if ui.selectable_label(h == *head, tl!(h.label())).clicked() {
                                        update(app, json!({"head": h.code()}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        // Anticlockwise from pointing right; the rotation handle above it turns it too.
                        let input_label = ui.label(tl!("Angle"));
                        let current = line_angle((e.x, e.y, e.w, e.h), *dir, *head);
                        let mut a = current;
                        let drag = Dragged::lend(ui, drag_key("angle"), &mut a);
                        let r = ui
                            .add(egui::DragValue::new(&mut a).range(-180.0..=180.0).speed(1.0).max_decimals(1).suffix("°"))
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut a) && (a - current).abs() > 1e-6 {
                            update(app, json!({"angle": a}));
                        }
                        ui.end_row();
                    }
                    _ => {}
                }
            },
        );
    });
    // A north arrow's or scale bar's style, picked from pictures of them as ArcGIS shows them, and
    // the type of its letter or numbers.
    let surround = match &e.kind {
        ElementKind::NorthArrow { style, outline, text, .. } => {
            let items: Vec<(&'static str, &'static str)> = ArrowStyle::ALL.iter().map(|v| (v.code(), v.label())).collect();
            Some((style.code(), items, true, text.clone(), *outline))
        }
        ElementKind::ScaleBar { style, color, text, .. } => {
            let items: Vec<(&'static str, &'static str)> = ScaleBarStyle::ALL.iter().map(|v| (v.code(), v.label())).collect();
            Some((style.code(), items, false, text.clone(), *color))
        }
        _ => None,
    };
    if let Some((now, items, arrow, text, own)) = surround {
        let picked = crate::textform::section(ui, "Style", |ui| style_gallery(ui, &items, now, arrow)).flatten();
        if let Some(code) = picked {
            update(app, json!({"style": code}));
        }
        label_type(app, ui, (li, id), &text, own, if arrow { "Letter type" } else { "Number type" }, None, &update);
    }
    // A layout's text: the form a text graphic has too (#516, #535), with the page's dynamic text.
    if let ElementKind::Text { text, size, color, bold, italic, align, letter_spacing, font, font_name } = &e.kind {
        let key = (li, id, "text");
        let mut buf = app.layout.typing.remove(&key).unwrap_or_else(|| text.clone());
        let (mut sz, mut c, mut bb) = (*size, crate::theme::c32(*color), *bold);
        let (mut it, mut al, mut ls, mut fam, mut named) = (*italic, *align, *letter_spacing, *font, font_name.clone());
        let tokens = [rustgis_engine::map::graphics::TOKENS, rustgis_engine::map::layout::PAGE_TOKENS];
        let fields = crate::textform::Fields {
            text: &mut buf,
            size: &mut sz,
            color: Some(&mut c),
            bold: Some(&mut bb),
            italic: Some(&mut it),
            align: Some(&mut al),
            letter_spacing: Some(&mut ls),
            font: Some(&mut fam),
            font_name: Some(&mut named),
            named_fonts: true,
            select: false,
        };
        let settled = crate::textform::section(ui, "Text", |ui| crate::textform::show(ui, ("layout_text", li, id), &tokens, fields))
            .unwrap_or_default();
        if settled.typing {
            app.layout.typing.insert(key, buf);
        } else if settled.text && buf != *text {
            update(app, json!({"text": buf}));
        }
        if settled.size {
            update(app, json!({"size": sz}));
        }
        if let Some(resp) = &settled.color {
            crate::widgets::one_step_per_press(app, resp);
            update(app, json!({"color": crate::theme::rgba(c).hex()}));
        }
        if settled.bold {
            update(app, json!({"bold": bb}));
        }
        if settled.italic {
            update(app, json!({"italic": it}));
        }
        if settled.align {
            update(app, json!({"align": al.code()}));
        }
        if settled.letter_spacing {
            update(app, json!({"letterSpacing": ls}));
        }
        if settled.font {
            // A named family with its fallback (#762), else the generic one.
            let font = match &named {
                Some(n) => json!({"font": n, "fallback": fam.code()}),
                None => json!({"font": fam.code()}),
            };
            update(app, font);
        }
    }
    // The layers a legend lists: a checkbox for each vector layer of its frame's map and each
    // WMS or WMTS layer with a legend, ticked when listed. Names gone from the map drop out of
    // the list as it's written back.
    if let ElementKind::Legend { frame, exclude, .. } = &e.kind {
        let map = layout.element(*frame).and_then(|f| match &f.kind {
            ElementKind::MapFrame { map, .. } => app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)),
            _ => None,
        });
        let names: Vec<String> = map
            .map(|m| {
                // Once each: layers may share a name, and the legend leaves names out.
                let mut names: Vec<String> = Vec::new();
                let listed = m.layers.iter().filter(|l| !l.graphics && !l.is_raster()).map(|l| &l.name);
                for name in listed.chain(m.tiles.iter().filter(|t| !t.legends.is_empty()).map(|t| &t.name)) {
                    if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                        names.push(name.clone());
                    }
                }
                names
            })
            .unwrap_or_default();
        // The names left out that the map still has, those not listed here (a raster's) too.
        let kept: Vec<String> = exclude
            .iter()
            .filter(|x| {
                map.is_some_and(|m| {
                    m.layers.iter().map(|l| &l.name).chain(m.tiles.iter().map(|t| &t.name)).any(|n| n.eq_ignore_ascii_case(x))
                })
            })
            .cloned()
            .collect();
        if !names.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new(tl!("Layers")).id_salt(("legend_layers", id)).default_open(!exclude.is_empty()).show(ui, |ui| {
                let left_out = |n: &str| exclude.iter().any(|x| x.eq_ignore_ascii_case(n));
                for n in &names {
                    let mut listed = !left_out(n);
                    if ui.checkbox(&mut listed, n.as_str()).on_hover_text(tl!("List this layer in the legend")).changed() {
                        let mut next: Vec<&String> = kept.iter().filter(|x| !x.eq_ignore_ascii_case(n)).collect();
                        if !listed {
                            next.push(n);
                        }
                        update(app, json!({"exclude": next}));
                    }
                }
            });
        }
    }
    // A legend's texts for its layers and classes, and its type (folded until opened).
    if let ElementKind::Legend { frame, look, exclude, .. } = &e.kind {
        let map = layout.element(*frame).and_then(|f| match &f.kind {
            ElementKind::MapFrame { map, .. } => app.session.project.maps.iter().position(|m| m.name.eq_ignore_ascii_case(map)),
            _ => None,
        });
        if let Some(mi) = map {
            legend_items(app, ui, (li, id), mi, look, exclude, &update);
        }
        let dark = rustgis_engine::map::Rgba::rgb(0x22, 0x22, 0x22);
        for (title, key, t) in [
            ("Title type", "titleType", &look.title),
            ("Layer name type", "layerType", &look.layer),
            ("Label type", "labelType", &look.label),
        ] {
            label_type(app, ui, (li, id), t, dark, title, Some(key), &update);
        }
    }
    if let ElementKind::MapFrame { grid, .. } = &e.kind {
        ui.add_space(4.0);
        egui::CollapsingHeader::new(tl!("Grid")).id_salt(("frame_grid", id)).default_open(grid.is_some()).show(ui, |ui| {
            grid_properties(app, ui, grid.as_ref(), id, &|app, g| update(app, json!({"grid": g})));
        });
    }
    // The extent indicator this frame draws (another frame's extent): its colour and width.
    if let ElementKind::MapFrame { extent_of: Some(of), extent_color, extent_width, .. } = &e.kind {
        let other = layout.element(*of).map_or_else(|| of.to_string(), |o| o.name.clone());
        ui.add_space(4.0);
        egui::CollapsingHeader::new(tl!("Extent Indicator")).id_salt(("frame_indicator", id)).default_open(true).show(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::fmt("Outlines {frame}", &[("frame", &other)])).size(11.5));
            egui::Grid::new(("indicator_props", id)).num_columns(2).show(ui, |ui| {
                ui.label(tl!("Color"));
                let mut c = crate::theme::c32(*extent_color);
                let resp = crate::widgets::color_button(ui, &mut c);
                if resp.changed() {
                    crate::widgets::one_step_per_press(app, &resp);
                    update(app, json!({"extentIndicator": {"color": crate::theme::rgba(c).hex()}}));
                }
                ui.end_row();
                let input_label = ui.label(tl!("Line width"));
                let mut w = *extent_width;
                let drag = Dragged::lend(ui, drag_key("extentWidth"), &mut w);
                let r = ui
                    .add(
                        egui::DragValue::new(&mut w)
                            .range(rustgis_engine::map::layout::MIN_INDICATOR_WIDTH..=rustgis_engine::map::layout::MAX_INDICATOR_WIDTH)
                            .speed(0.05)
                            .max_decimals(2)
                            .suffix(" pt"),
                    )
                    .labelled_by(input_label.id);
                if drag.settled(ui, &r, &mut w) && (w - extent_width).abs() > 1e-6 {
                    update(app, json!({"extentIndicator": {"width": w}}));
                }
                ui.end_row();
            });
            if ui.button(tl!("Remove Extent Indicator")).clicked() {
                update(app, json!({"extentIndicator": false}));
            }
        });
    }
    ui.add_space(8.0);
    if let ElementKind::MapFrame { .. } = e.kind {
        ui.toggle_value(&mut app.layout.navigate, tl!("Pan and Zoom Map"))
            .on_hover_text(tl!("Drag inside map frames to pan their maps and turn the wheel to zoom them, instead of moving them."));
        ui.horizontal_wrapped(|ui| {
            for (label, to) in [("Zoom To Map View", "view"), ("Full Extent", "full"), ("Selection", "selection")] {
                if ui.button(tl!(label)).clicked() {
                    let _ = app.run("layout.frameZoom", json!({"layout": li, "element": id, "to": to}));
                }
            }
        });
    }
}

/// Duplicate and Delete, under the properties: a locked element can be copied (the copy isn't
/// locked) but not deleted, nor a frame with a locked surround. Then the drawing order: Bring to
/// Front, Bring Forward, Send Backward, Send to Back.
fn element_actions(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize, id: u32) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(tl!("Duplicate")).on_hover_text(tl!("Copy this element (Ctrl+D, ⌘D on a Mac)")).clicked()
            && let Ok(v) = app.run("layout.duplicateElement", json!({"layout": li, "element": id}))
        {
            app.layout.selected = v["element"].as_u64().and_then(|v| u32::try_from(v).ok());
        }
        if ui
            .add_enabled(layout.locked_in_removal(id).is_none(), egui::Button::new(tl!("Delete Element")))
            .on_hover_text(tl!("Delete (or Backspace)"))
            .on_disabled_hover_text(locked_delete())
            .clicked()
        {
            let _ = app.run("layout.removeElement", json!({"layout": li, "element": id}));
            app.layout.selected = None;
        }
    });
    // Lined up with the page.
    let locked_now = layout.element(id).is_some_and(|e| e.locked);
    ui.add_enabled_ui(!locked_now, |ui| align_buttons(app, ui, li, &[id], tl!("Align to the page")));
    if layout.element(id).is_some_and(|e| e.group.is_some()) {
        // A grouped element is picked with its group; this takes it out.
        if ui.button(tl!("Ungroup")).on_hover_text(tl!("Take the group apart (Ctrl+Shift+G)")).clicked() {
            let _ = app.run("layout.ungroup", json!({"layout": li, "elements": [id]}));
        }
    }
    // The drawing order: a legend brought to the front is drawn over the map frame under it.
    // A step it can't take (already on top, or at the bottom) is greyed, as is a locked element.
    let Some((at, n)) = layout.draw_order(id) else { return };
    let locked = layout.element(id).is_some_and(|e| e.locked);
    ui.horizontal_wrapped(|ui| {
        for to in Arrange::ALL {
            let can = match to {
                Arrange::Front | Arrange::Forward => at + 1 < n,
                Arrange::Backward | Arrange::Back => at > 0,
            };
            let tip = match to {
                Arrange::Front => "Draw it over every other element",
                Arrange::Forward => "Draw it over the element above it",
                Arrange::Backward => "Draw it under the element below it",
                Arrange::Back => "Draw it under every other element",
            };
            if ui.add_enabled(can && !locked, egui::Button::new(tl!(to.label()))).on_hover_text(tl!(tip)).clicked() {
                let _ = app.run("layout.arrange", json!({"layout": li, "element": id, "to": to.code()}));
            }
        }
    });
}

/// The align buttons for `ids`: lined up by the box around them, or one by the page; spaced and
/// sized alike when there are enough of them.
fn align_buttons(app: &mut RustGisApp, ui: &mut Ui, li: usize, ids: &[u32], heading: &str) {
    use rustgis_engine::map::layout::Align;
    ui.add_space(6.0);
    ui.label(egui::RichText::new(heading).strong());
    let rows: [&[(Align, &str)]; 3] = [
        &[
            (Align::Left, "Line up the left edges"),
            (Align::Center, "Line up the centres, across"),
            (Align::Right, "Line up the right edges"),
            (Align::Top, "Line up the top edges"),
            (Align::Middle, "Line up the middles, down"),
            (Align::Bottom, "Line up the bottom edges"),
        ],
        &[
            (Align::DistributeH, "The same space between each, left to right (three or more)"),
            (Align::DistributeV, "The same space between each, top to bottom (three or more)"),
        ],
        &[
            (Align::SameWidth, "As wide as the one picked first"),
            (Align::SameHeight, "As high as the one picked first"),
            (Align::SameSize, "As large as the one picked first"),
        ],
    ];
    for (i, row) in rows.iter().enumerate() {
        if i > 0 && ids.len() < 2 {
            break;
        }
        ui.horizontal_wrapped(|ui| {
            for (how, tip) in row.iter() {
                let text = tl!(how.label());
                let enough = ids.len() >= how.least();
                if ui.add_enabled(enough, egui::Button::new(text)).on_hover_text(tl!(tip)).clicked() {
                    let _ = app.run("layout.align", json!({"layout": li, "elements": ids, "to": how.code()}));
                }
            }
        });
    }
}

/// The Format pane for several elements: lining them up, spacing and sizing them alike,
/// grouping, copying and deleting them together.
fn many(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize) {
    let ids = app.layout.selection(layout);
    let heading = crate::i18n::fmt("Layout: {n} elements", &[("n", &ids.len().to_string())]);
    ui.label(egui::RichText::new(heading).font(semibold(14.0)));
    ui.label(
        egui::RichText::new(tl!("Shift- or Ctrl-click adds or takes one away; Ctrl-drag selects those inside a box."))
            .small()
            .color(ui.visuals().weak_text_color()),
    );
    ui.separator();
    let locked = ids.iter().filter_map(|id| layout.element(*id)).any(|e| e.locked);
    if locked {
        ui.label(egui::RichText::new(tl!("One of them is locked: unlock it to move them together.")).color(ui.visuals().warn_fg_color));
    }
    ui.add_enabled_ui(!locked, |ui| {
        align_buttons(app, ui, li, &ids, tl!("Align"));
        if ui.button(tl!("Align to the page")).on_hover_text(tl!("Centre them all on the page, across and down")).clicked() {
            let _ = app.run("layout.align", json!({"layout": li, "elements": ids, "to": "center", "page": true}));
            let _ = app.run("layout.align", json!({"layout": li, "elements": ids, "to": "middle", "page": true}));
        }
    });
    ui.add_space(8.0);
    let grouped = ids.iter().filter_map(|id| layout.element(*id)?.group).collect::<std::collections::HashSet<_>>();
    let all_one = grouped.len() == 1 && ids.iter().all(|id| layout.element(*id).is_some_and(|e| e.group.is_some()));
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!all_one, egui::Button::new(tl!("Group"))).on_hover_text(tl!("Pick and move them as one (Ctrl+G)")).clicked() {
            let _ = app.run("layout.group", json!({"layout": li, "elements": ids}));
        }
        if ui
            .add_enabled(!grouped.is_empty(), egui::Button::new(tl!("Ungroup")))
            .on_hover_text(tl!("Take the group apart (Ctrl+Shift+G)"))
            .clicked()
        {
            let _ = app.run("layout.ungroup", json!({"layout": li, "elements": ids}));
            // Still selected, each now on its own.
            app.layout.select_all(layout, &ids);
        }
        if ui.button(tl!("Copy")).on_hover_text(tl!("Copy them, to paste here or in another layout (Ctrl+C, Ctrl+V)")).clicked() {
            copy_selection(app, li, &ids);
        }
        if ui.button(tl!("Duplicate")).on_hover_text(tl!("Copy them (Ctrl+D, ⌘D on a Mac)")).clicked() {
            duplicate_selection(app, li, &ids);
        }
        let can_delete = ids.iter().all(|id| layout.locked_in_removal(*id).is_none());
        if ui
            .add_enabled(can_delete, egui::Button::new(tl!("Delete Elements")))
            .on_hover_text(tl!("Delete (or Backspace)"))
            .on_disabled_hover_text(locked_delete())
            .clicked()
            && app.run("layout.removeElements", json!({"layout": li, "elements": ids})).is_ok()
        {
            app.layout.select(layout, None);
        }
    });
}

/// The right-click menu on the page: the selected elements' copy, paste, group, order and
/// delete.
fn element_menu(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize) {
    let ids = app.layout.selection(layout);
    if ids.is_empty() {
        ui.label(egui::RichText::new(tl!("Right-click an element for what it can do; paste with Ctrl+V.")).small());
        return;
    }
    let locked = ids.iter().any(|id| layout.locked_in_removal(*id).is_some());
    if ui.button(tl!("Copy")).on_hover_text("Ctrl+C").clicked() {
        copy_selection(app, li, &ids);
        ui.close();
    }
    if ui.add_enabled(!locked, egui::Button::new(tl!("Cut"))).on_hover_text("Ctrl+X").clicked() {
        if copy_selection(app, li, &ids) && app.run("layout.removeElements", json!({"layout": li, "elements": ids})).is_ok() {
            app.layout.select(layout, None);
        }
        ui.close();
    }
    if ui.button(tl!("Duplicate")).on_hover_text("Ctrl+D").clicked() {
        duplicate_selection(app, li, &ids);
        ui.close();
    }
    ui.separator();
    let grouped = ids.iter().any(|id| layout.element(*id).is_some_and(|e| e.group.is_some()));
    if ids.len() > 1 && ui.button(tl!("Group")).on_hover_text("Ctrl+G").clicked() {
        let _ = app.run("layout.group", json!({"layout": li, "elements": ids}));
        ui.close();
    }
    if grouped && ui.button(tl!("Ungroup")).on_hover_text("Ctrl+Shift+G").clicked() {
        let _ = app.run("layout.ungroup", json!({"layout": li, "elements": ids}));
        app.layout.select_all(layout, &ids);
        ui.close();
    }
    if let [id] = ids.as_slice() {
        ui.add_enabled_ui(!locked, |ui| {
            for to in [Arrange::Front, Arrange::Back] {
                if ui.button(tl!(to.label())).clicked() {
                    let _ = app.run("layout.arrange", json!({"layout": li, "element": id, "to": to.code()}));
                    ui.close();
                }
            }
        });
    }
    ui.separator();
    let delete = if ids.len() > 1 { "Delete Elements" } else { "Delete Element" };
    if ui.add_enabled(!locked, egui::Button::new(tl!(delete))).on_hover_text("Delete").on_disabled_hover_text(locked_delete()).clicked() {
        if app.run("layout.removeElements", json!({"layout": li, "elements": ids})).is_ok() {
            app.layout.select(layout, None);
        }
        ui.close();
    }
}

/// Puts the elements on the clipboard, as text `layout.paste` takes, here or in another layout.
pub(crate) fn copy_selection(app: &mut RustGisApp, li: usize, ids: &[u32]) -> bool {
    match app.run("layout.copyElements", json!({"layout": li, "elements": ids})) {
        Ok(v) => {
            app.ui.copy_out = v["text"].as_str().map(str::to_string);
            true
        }
        Err(_) => false,
    }
}

/// Copies of the elements 5 mm off, selected.
pub(crate) fn duplicate_selection(app: &mut RustGisApp, li: usize, ids: &[u32]) {
    let Ok(v) = app.run("layout.copyElements", json!({"layout": li, "elements": ids})) else { return };
    let Some(text) = v["text"].as_str().map(str::to_string) else { return };
    if let Ok(r) = app.run("layout.paste", json!({"layout": li, "text": text, "dx": 5, "dy": 5})) {
        select_pasted(app, li, &r);
    }
}

/// Selects what a paste added.
pub(crate) fn select_pasted(app: &mut RustGisApp, li: usize, r: &Value) {
    let ids: Vec<u32> = r["elements"].as_array().into_iter().flatten().filter_map(|v| u32::try_from(v.as_u64()?).ok()).collect();
    if let Some(l) = app.session.project.layouts.get(li).cloned() {
        app.layout.select_all(&l, &ids);
    }
}

/// Why a locked element, or a frame with a locked surround, isn't deleted.
pub(crate) fn locked_delete() -> &'static str {
    tl!("It is locked, or one of its surrounds is: unlock it first")
}

#[cfg(test)]
mod tests {
    use super::{RULER, landing, page_rect, resized, rotate_handle, snap_to, to_screen, turned};
    use crate::RustGisApp;
    use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};
    use serde_json::json;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn turns_snap_to_level_upright_and_diagonal() {
        assert_eq!(turned(0.0, 90.0, 0.0, false), -90.0, "a quarter turn clockwise");
        assert_eq!(turned(10.0, 0.0, 33.0, false), 45.0, "43° snaps to 45°");
        assert!((turned(10.0, 0.0, 20.0, false) - 30.0).abs() < 1e-9, "30° is left alone");
        assert_eq!(turned(0.0, 0.0, 20.0, true), 15.0, "Shift: 15° steps");
        assert!((turned(170.0, 0.0, 30.0, false) + 160.0).abs() < 1e-9, "200° is -160°");
    }

    /// Dragging an arrow's round handle from above its centre round to its right turns it a
    /// quarter turn clockwise, about its centre, keeping its length.
    #[test]
    fn an_arrow_turns_by_its_rotation_handle() {
        use rustgis_engine::map::layout::{ElementKind, line_angle, line_length};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Turn", "template": "blank"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let id = app.run("layout.addElement", json!({"layout": li, "kind": "arrow", "angle": 0, "length": 60})).unwrap()["element"]
            .as_u64()
            .unwrap() as u32;
        app.layout.selected = Some(id);
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let (_, knob, c) = rotate_handle(h.state(), &layout, page, s).unwrap();
        let to = c + vec2((knob - c).length(), 0.0);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(knob));
        h.step();
        h.event(button(knob, true));
        h.step();
        for i in 1..=6 {
            h.event(Event::PointerMoved(knob + (to - knob) * i as f32 / 6.0));
            h.step();
        }
        h.event(button(to, false));
        h.step();
        let app = h.state();
        let e = app.session.project.layouts[li].element(id).unwrap();
        let ElementKind::Line { dir, head, .. } = e.kind else { panic!("not a line") };
        let r = (e.x, e.y, e.w, e.h);
        assert!((line_angle(r, dir, head) + 90.0).abs() < 1e-9, "points down: {}°", line_angle(r, dir, head));
        assert!((line_length(r, dir) - 60.0).abs() < 1e-6);
        let old = layout.element(id).unwrap();
        assert!(((e.x + e.w / 2.0) - (old.x + old.w / 2.0)).abs() < 1e-9 && ((e.y + e.h / 2.0) - (old.y + old.h / 2.0)).abs() < 1e-9);
        assert_eq!(app.layout.selected, Some(id), "still selected");
    }

    /// While a map frame is panned, its map is drawn past the frame's sides and slid with the
    /// pointer, so what comes into view is map rather than the page beside the frame; the drawing
    /// is let go when the pan ends, which moves the frame's map as before.
    #[test]
    fn a_panned_frame_shows_map_beyond_its_edges() {
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Pan"})).unwrap();
        app.layout.navigate = true;
        let li = app.session.active_layout.unwrap();
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let id = layout.first_frame().unwrap();
        let center = |l: &rustgis_engine::map::layout::Layout| match &l.element(id).unwrap().kind {
            ElementKind::MapFrame { center, .. } => *center,
            k => panic!("{k:?}"),
        };
        let before = center(&layout);
        let e = layout.element(id).unwrap();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let from = to_screen(page, s, (e.x + e.w / 2.0, e.y + e.h / 2.0));
        let to = from + vec2(120.0, 0.0);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        for i in 1..=4 {
            h.event(Event::PointerMoved(from + (to - from) * i as f32 / 4.0));
            h.step();
        }
        // Mid-pan: the frame's map with its overscan is drawn, three frames wide and high.
        let (pid, map) = h.state().layout.pan_preview.clone().expect("a preview while panning");
        assert_eq!(pid, id);
        let size = map.expect("the frame's map was drawn").size();
        let frame_px = (e.w as f32 * s * h.ctx.pixels_per_point(), e.h as f32 * s * h.ctx.pixels_per_point());
        assert!(
            (size[0] as f32 - 3.0 * frame_px.0).abs() < 3.0 && (size[1] as f32 - 3.0 * frame_px.1).abs() < 3.0,
            "{size:?} for a frame of {frame_px:?} px"
        );
        h.event(button(to, false));
        h.step();
        assert!(h.state().layout.pan_preview.is_none(), "let go after the pan");
        assert_ne!(center(&h.state().session.project.layouts[li]), before, "the pan moved the frame's map");
    }

    /// Pressing a corner handle and dragging away fast resizes the element, though by the time
    /// egui starts the drag the pointer is already off the handle; it used to move the element.
    #[test]
    fn a_fast_drag_from_a_corner_handle_resizes() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Resize"})).unwrap();
        app.layout.no_snap = true;
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        // A little inside the bottom-right handle, not on its centre: no bull's-eye needed.
        let corner = to_screen(page, s, (title.x + title.w, title.y + title.h)) + vec2(-4.0, -3.0);
        let to = corner + vec2(60.0, 30.0);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(corner));
        h.step();
        h.event(button(corner, true));
        h.step();
        // Three big steps: the first already leaves the handle behind.
        for i in 1..=3 {
            h.event(Event::PointerMoved(corner + (to - corner) * i as f32 / 3.0));
            h.step();
        }
        h.event(button(to, false));
        h.step();
        let app = h.state();
        let (_, s) = app.layout.drag_page.unwrap();
        let e = app.session.project.layouts[li].element(title.id).unwrap();
        assert!(
            (e.x - title.x).abs() < 0.01 && (e.y - title.y).abs() < 0.01,
            "the top-left stays: {:?} → {:?}",
            (title.x, title.y),
            (e.x, e.y)
        );
        let (dw, dh) = ((e.w - title.w) * s as f64, (e.h - title.h) * s as f64);
        assert!((59.0..=61.0).contains(&dw) && (29.0..=31.0).contains(&dh), "grew by {dw} × {dh} points, not 60 × 30");
    }

    /// A click that starts on the selected element's handle keeps the selection, though the
    /// pointer moved off the handle (and off the element) before the release.
    #[test]
    fn a_click_started_on_a_handle_keeps_the_selection() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Click"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        // Pressed at the edge of the bottom-right handle's reach, released a few points further
        // out: still a click, but on nothing.
        let corner = to_screen(page, s, (title.x + title.w, title.y + title.h));
        let (from, to) = (corner + vec2(8.0, 8.0), corner + vec2(12.0, 12.0));
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        h.event(Event::PointerMoved(to));
        h.step();
        h.event(button(to, false));
        h.run_steps(2);
        let (x, y) = super::to_mm(page, s, to);
        assert!(layout.hit(x, y).is_none(), "the release is off every element");
        assert_eq!(h.state().layout.selected, Some(title.id));
    }

    /// Dragging an element redraws the page with it where it was dropped (#325).
    #[test]
    fn dropped_element_is_drawn_where_it_lands() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Drag"})).unwrap();
        // The drop itself, not snapping (tested below).
        app.layout.no_snap = true;
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        // Fonts take effect on the frame after they are installed.
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let li = h.state().session.active_layout.unwrap();
        let layout = h.state().session.project.layouts[li].clone();
        let title = layout.elements.iter().find(|e| e.name == "Title").unwrap().clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let from = to_screen(page, s, (title.x + title.w / 2.0, title.y + title.h / 2.0));
        let to = from + vec2(0.0, 120.0);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        for i in 1..=6 {
            h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
            h.step();
        }
        let before = h.state().session.revision;
        h.event(button(to, false));
        h.step();
        let app = h.state();
        assert!(app.session.revision > before, "the drop should update the layout");
        let moved = app.session.project.layouts[li].element(title.id).unwrap();
        // A vertical drag moves it straight down by the pointer's motion, although the
        // properties opened beside the page and re-fitted it mid-drag.
        let (_, s) = app.layout.drag_page.unwrap();
        assert!((moved.x - title.x).abs() < 0.01, "x moved from {} to {}", title.x, moved.x);
        // The drag runs from where the button went down, so the element moved with the pointer.
        let dy = (moved.y - title.y) * s as f64;
        assert!((119.0..=120.5).contains(&dy), "y moved {dy} points");
        // The page was drawn, and keyed, from the layout as it is after the drop.
        let (i, rev, drawn) = app.layout.shown.as_ref().unwrap();
        assert_eq!((*i, *rev), (li, app.session.revision));
        assert_eq!(drawn.element(title.id).map(|e| e.y), Some(moved.y));
        assert_eq!(app.layout.key.map(|k| k.0), Some(app.session.revision));
    }

    /// Dragging the map in a turned frame moves it the way the pointer goes, as in the map view.
    #[test]
    fn panning_a_turned_frame_follows_the_pointer() {
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Turned"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let id = app.session.project.layouts[li].first_frame().unwrap();
        // East at the top: the page's right is south, its left north.
        app.run("layout.update", json!({"layout": li, "element": id, "bearing": 90, "center": [10.0, 20.0]})).unwrap();
        let layout = app.session.project.layouts[li].clone();
        super::pan_frame(&mut app, &layout, li, id, 10.0, 0.0);
        let Some(ElementKind::MapFrame { center, .. }) = app.session.project.layouts[li].element(id).map(|e| e.kind.clone()) else {
            panic!("not a map frame")
        };
        // Dragged right, the map brings in what lay to the left: the centre moves north.
        assert!(center.y > 20.5 && (center.x - 10.0).abs() < 0.01, "{center:?}");
    }

    /// Zooming a map frame about a point keeps the map under that point where it was.
    #[test]
    fn zooming_a_frame_keeps_the_map_under_the_pointer() {
        use rustgis_engine::map::layout::{ElementKind, PT_PER_MM, frame_camera};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Zoom"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let layout = app.session.project.layouts[li].clone();
        let frame = layout.elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).unwrap().clone();
        // A point a quarter of the way into the frame.
        let anchor = (frame.x + frame.w / 4.0, frame.y + frame.h / 4.0);
        // The map point (map units) at the anchor, the frame's scale, and map units per point.
        let map_at = |app: &RustGisApp| {
            let e = app.session.project.layouts[li].element(frame.id).unwrap().clone();
            let ElementKind::MapFrame { map, center, scale, .. } = &e.kind else { panic!("not a map frame") };
            let m = app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).unwrap();
            let cam = frame_camera(m, *center, *scale, 0.0, 0.0);
            let x = cam.center.x + (anchor.0 - (e.x + e.w / 2.0)) * PT_PER_MM * cam.res;
            let y = cam.center.y - (anchor.1 - (e.y + e.h / 2.0)) * PT_PER_MM * cam.res;
            (x, y, *scale, cam.res)
        };
        let before = map_at(&app);
        let undo = app.session.undo_depth();
        super::zoom_frame(&mut app, &layout, li, frame.id, 0.5, anchor);
        let after = map_at(&app);
        assert!((after.2 - before.2 * 0.5).abs() < 1e-6 * before.2, "the scale halves: {} → {}", before.2, after.2);
        assert!((after.0 - before.0).hypot(after.1 - before.1) < after.3, "the anchor's map point moved: {before:?} → {after:?}");
        assert_eq!(app.session.undo_depth(), undo + 1, "one undo step");
    }

    /// A wheel zoom still waiting is applied, not lost, when another frame is zoomed; one waiting
    /// for a frame of another layout is dropped, never applied to the open layout.
    #[test]
    fn waiting_frame_zooms_are_applied_or_dropped_never_misapplied() {
        use rustgis_engine::map::layout::ElementKind;
        let scale_of = |app: &RustGisApp, li: usize, id: u32| match app.session.project.layouts[li].element(id).map(|e| &e.kind) {
            Some(ElementKind::MapFrame { scale, .. }) => *scale,
            k => panic!("not a map frame: {k:?}"),
        };
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Two"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let name = app.session.project.layouts[li].name.clone();
        let a = app.session.project.layouts[li].first_frame().unwrap();
        let b = app.run("layout.addFrame", json!({"layout": li, "x": 20, "y": 20, "w": 60, "h": 40})).unwrap()["element"].as_u64().unwrap()
            as u32;
        app.layout.navigate = true;
        let (sa, sb) = (scale_of(&app, li, a), scale_of(&app, li, b));
        // Frame A's zoom waits: its wheel turned "later than ever", so it can't settle by itself.
        app.layout.frame_zoom = Some((name.clone(), a, 0.5, (100.0, 100.0), f64::MAX));
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(2);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        // Zoom in over frame B.
        h.event(Event::PointerMoved(to_screen(page, s, (50.0, 40.0))));
        h.step();
        h.event(Event::Zoom(1.25));
        h.step();
        assert!((scale_of(h.state(), li, a) - sa * 0.5).abs() < 1e-6 * sa, "A's waiting zoom was applied");
        assert_eq!(h.state().layout.frame_zoom.as_ref().map(|z| z.1), Some(b), "B's zoom waits for the wheel to stop");
        h.run_steps(3);
        assert!(h.state().layout.frame_zoom.is_none());
        assert!(scale_of(h.state(), li, b) < sb, "B zoomed in once the wheel stopped");

        // A zoom left waiting by another layout is dropped without touching this one.
        h.state_mut().layout.frame_zoom = Some(("Another layout".into(), a, 0.5, (100.0, 100.0), 0.0));
        let rev = h.state().session.revision;
        h.run_steps(2);
        assert!(h.state().layout.frame_zoom.is_none());
        assert_eq!(h.state().session.revision, rev, "nothing was applied");
    }

    /// Arrow keys nudge the selected element 1 mm, 10 mm with Shift; a run of them, held or
    /// tapped, is one step to undo; a locked element stays put.
    #[test]
    fn arrow_keys_nudge_the_selected_element_and_undo_as_one() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Nudge"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                crate::chrome::shortcuts(app, &ctx);
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let key = |key, modifiers, repeat| Event::Key { key, physical_key: None, pressed: true, repeat, modifiers };
        let at = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            let e = h.state().session.project.layouts[li].element(title.id).unwrap();
            (e.x - title.x, e.y - title.y)
        };
        let depth = h.state().session.undo_depth();
        h.event(key(Key::ArrowRight, Modifiers::NONE, false));
        h.step();
        assert_eq!(at(&h), (1.0, 0.0), "→ nudges 1 mm");
        h.event(key(Key::ArrowDown, Modifiers::SHIFT, false));
        h.step();
        assert_eq!(at(&h), (1.0, 10.0), "Shift-↓ nudges 10 mm");
        // A held key repeats, several times in a frame.
        for _ in 0..3 {
            h.event(key(Key::ArrowLeft, Modifiers::NONE, true));
        }
        h.step();
        assert_eq!(at(&h), (-2.0, 10.0));
        assert_eq!(h.state().session.undo_depth(), depth + 1, "one step to undo, not five");
        h.state_mut().run("edit.undo", serde_json::Value::Null).unwrap();
        assert_eq!(at(&h), (0.0, 0.0), "one undo puts it back");

        // A locked element stays put.
        h.state_mut().run("layout.update", json!({"layout": li, "element": title.id, "locked": true})).unwrap();
        h.event(key(Key::ArrowUp, Modifiers::NONE, false));
        h.step();
        assert_eq!(at(&h), (0.0, 0.0));
    }

    /// A harness drawing the layout view with the app's shortcuts, and the view's area.
    fn view_harness(app: RustGisApp) -> (egui_kittest::Harness<'static, RustGisApp>, Rc<Cell<Rect>>) {
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                crate::chrome::shortcuts(app, &ctx);
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        (h, area)
    }

    /// A blank layout with three boxes, side by side: the app, the layout and their ids.
    fn three_boxes() -> (RustGisApp, usize, [u32; 3]) {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Boxes", "template": "blank"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let mut ids = [0; 3];
        for (i, x) in [20.0, 80.0, 140.0].into_iter().enumerate() {
            let r = app.run("layout.addElement", json!({"layout": li, "kind": "rectangle", "x": x, "y": 40, "w": 30, "h": 20})).unwrap();
            ids[i] = r["element"].as_u64().unwrap() as u32;
        }
        (app, li, ids)
    }

    fn drag(h: &mut egui_kittest::Harness<'_, RustGisApp>, from: Pos2, to: Pos2, modifiers: Modifiers) {
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers };
        h.event(Event::ModifiersChanged(modifiers));
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        for i in 1..=6 {
            h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
            h.step();
        }
        h.event(button(to, false));
        h.step();
        h.event(Event::ModifiersChanged(Modifiers::NONE));
        h.step();
    }

    fn click(h: &mut egui_kittest::Harness<'_, RustGisApp>, at: Pos2, modifiers: Modifiers) {
        h.event(Event::ModifiersChanged(modifiers));
        h.event(Event::PointerMoved(at));
        h.step();
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers });
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers });
        h.step();
        h.event(Event::ModifiersChanged(Modifiers::NONE));
        h.step();
    }

    /// Shift-clicking adds an element to the selection; dragging one of them moves both, in one
    /// step to undo; a plain click selects one again.
    #[test]
    fn shift_click_selects_several_and_a_drag_moves_them_all() {
        let (app, li, [a, b, c]) = three_boxes();
        let (mut h, area) = view_harness(app);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let centre = |id: u32| to_screen(page, s, layout.element(id).unwrap().center());
        click(&mut h, centre(a), Modifiers::NONE);
        click(&mut h, centre(b), Modifiers::SHIFT);
        assert_eq!(h.state().layout.selection(&layout), vec![a, b]);
        let depth = h.state().session.undo_depth();
        drag(&mut h, centre(b), centre(b) + vec2(0.0, 10.0 * s), Modifiers::NONE);
        let l = h.state().session.project.layouts[li].clone();
        let dy = |id: u32| l.element(id).unwrap().y - layout.element(id).unwrap().y;
        assert!(dy(a) > 5.0 && (dy(a) - dy(b)).abs() < 1e-9, "both moved alike: {} {}", dy(a), dy(b));
        assert_eq!(dy(c), 0.0, "the third stays");
        assert_eq!(h.state().session.undo_depth(), depth + 1, "one step to undo");
        // Shift-clicking a selected one takes it out; a plain click selects one alone.
        let moved_a = centre(a) + vec2(0.0, dy(a) as f32 * s);
        click(&mut h, moved_a, Modifiers::SHIFT);
        let l = h.state().session.project.layouts[li].clone();
        assert_eq!(h.state().layout.selection(&l), vec![b]);
        click(&mut h, centre(c), Modifiers::NONE);
        assert_eq!(h.state().layout.selection(&l), vec![c]);
    }

    /// Ctrl-dragging a box selects what lies wholly inside it; Ctrl+G groups them, and clicking
    /// one then picks the group.
    #[test]
    fn a_box_selects_what_is_inside_and_ctrl_g_groups_it() {
        let (app, li, [a, b, c]) = three_boxes();
        let (mut h, area) = view_harness(app);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        // From above-left of the first to below-right of the second, short of the third.
        drag(&mut h, to_screen(page, s, (10.0, 30.0)), to_screen(page, s, (120.0, 70.0)), Modifiers::COMMAND);
        assert_eq!(h.state().layout.selection(&layout), vec![a, b]);
        h.event(Event::Key { key: Key::G, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND });
        h.step();
        let l = h.state().session.project.layouts[li].clone();
        assert!(l.element(a).unwrap().group.is_some() && l.element(a).unwrap().group == l.element(b).unwrap().group);
        click(&mut h, to_screen(page, s, l.element(c).unwrap().center()), Modifiers::NONE);
        click(&mut h, to_screen(page, s, l.element(b).unwrap().center()), Modifiers::NONE);
        assert_eq!(h.state().layout.selection(&l), vec![b, a], "the group comes with it");
    }

    /// Copying selected elements and pasting them adds copies 5 mm off, selected; in another
    /// layout they land where they were.
    #[test]
    fn copied_elements_paste_here_and_in_another_layout() {
        let (mut app, li, [a, b, _]) = three_boxes();
        let layout = app.session.project.layouts[li].clone();
        app.layout.select_all(&layout, &[a, b]);
        let (mut h, _) = view_harness(app);
        h.event(Event::Copy);
        h.step();
        let text = h.state_mut().ui.copy_out.take().expect("copied to the clipboard");
        let n = h.state().session.project.layouts[li].elements.len();
        h.event(Event::Paste(text.clone()));
        h.step();
        let l = h.state().session.project.layouts[li].clone();
        assert_eq!(l.elements.len(), n + 2);
        let sel = h.state().layout.selection(&l);
        assert_eq!(sel.len(), 2, "the copies are selected");
        let copy = l.element(sel[0]).unwrap();
        assert_eq!((copy.x, copy.y), (layout.element(a).unwrap().x + 5.0, layout.element(a).unwrap().y + 5.0));
        // Another layout: where they were.
        h.state_mut().run("layout.new", json!({"name": "Other", "template": "blank"})).unwrap();
        let other = h.state().session.active_layout.unwrap();
        h.step();
        h.event(Event::Paste(text));
        h.step();
        let l2 = &h.state().session.project.layouts[other];
        assert_eq!(l2.elements.len(), 2);
        assert_eq!((l2.elements[0].x, l2.elements[0].y), (layout.element(a).unwrap().x, layout.element(a).unwrap().y));
    }

    /// A rectangle turns by its round handle like a line: dragged from above its centre round to
    /// its right, a quarter turn clockwise; its corners' handles go, as it's sized in the pane.
    #[test]
    fn a_rectangle_turns_by_its_rotation_handle() {
        let (mut app, li, [a, _, _]) = three_boxes();
        let layout = app.session.project.layouts[li].clone();
        app.layout.select(&layout, Some(a));
        let (mut h, area) = view_harness(app);
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let (_, knob, c) = rotate_handle(h.state(), &layout, page, s).unwrap();
        assert!(knob.y < c.y && (knob.x - c.x).abs() < 0.5, "above its centre");
        drag(&mut h, knob, c + vec2((knob - c).length(), 0.0), Modifiers::NONE);
        let l = h.state().session.project.layouts[li].clone();
        let e = l.element(a).unwrap();
        assert_eq!(e.rotation, -90.0);
        assert_eq!(e.center(), layout.element(a).unwrap().center(), "about its centre");
        // Turned, it has no resize handles, and the knob turns with it, to its right.
        let corner = to_screen(page, s, (e.x, e.y));
        assert!(super::handle_at(h.state(), &l, page, s, corner).is_none());
        let (_, knob, c) = rotate_handle(h.state(), &l, page, s).unwrap();
        assert!(knob.x > c.x && (knob.y - c.y).abs() < 0.5);
    }

    /// The selected element's Bring to Front and Send to Back buttons move it in the drawing
    /// order; a step it can't take is greyed.
    #[test]
    fn the_arrange_buttons_move_an_element_up_and_down() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Arrange"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let frame = app.session.project.layouts[li].first_frame().unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(600.0, 200.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                let layout = app.session.project.layouts[li].clone();
                super::element_actions(app, ui, &layout, li, frame);
            },
            app,
        );
        h.run_steps(2);
        let at = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.project.layouts[li].draw_order(frame).unwrap();
        let n = at(&h).1;
        h.get_by_label("Bring to Front").click();
        h.run_steps(2);
        assert_eq!(at(&h), (n - 1, n));
        // On top, it can't go further up.
        assert!(h.get_by_label("Bring Forward").accesskit_node().is_disabled());
        h.get_by_label("Send Backward").click();
        h.run_steps(2);
        assert_eq!(at(&h).0, n - 2);
        h.get_by_label("Send to Back").click();
        h.run_steps(2);
        assert_eq!(at(&h).0, 0);
        assert!(h.get_by_label("Send to Back").accesskit_node().is_disabled());
    }

    /// Ctrl+D (Cmd+D) duplicates the selected element and selects the copy; Backspace, the Mac's
    /// delete key, deletes it.
    #[test]
    fn keys_duplicate_and_delete_the_selected_element() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Keys"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                crate::chrome::shortcuts(app, &ctx);
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let n = h.state().session.project.layouts[li].elements.len();

        h.event(key(Key::D, Modifiers::COMMAND));
        h.step();
        let app = h.state();
        let layout = &app.session.project.layouts[li];
        assert_eq!(layout.elements.len(), n + 1, "Ctrl+D added a copy");
        let copy = layout.element(app.layout.selected.unwrap()).unwrap();
        assert_ne!(copy.id, title.id, "the copy is selected");
        assert_eq!((copy.name.as_str(), copy.x, copy.y, copy.w), ("Title 2", title.x + 5.0, title.y + 5.0, title.w));
        let copy = copy.id;

        h.event(key(Key::Backspace, Modifiers::NONE));
        h.step();
        let app = h.state();
        let layout = &app.session.project.layouts[li];
        assert_eq!(layout.elements.len(), n, "Backspace deleted the copy");
        assert!(layout.element(copy).is_none() && layout.element(title.id).is_some());
        assert_eq!(app.layout.selected, None);
    }

    /// A locked element is selected, but a drag from inside it or from where its corner handle
    /// would be doesn't move or resize it, and Delete keeps it and says why (#536).
    #[test]
    fn a_locked_element_stays_put() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Locked"})).unwrap();
        app.layout.no_snap = true;
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.run("layout.update", json!({"layout": li, "element": title.id, "locked": true})).unwrap();
        let title = app.session.project.layouts[li].element(title.id).unwrap().clone();
        app.layout.selected = Some(title.id);
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                let ctx = ui.ctx().clone();
                crate::chrome::shortcuts(app, &ctx);
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let middle = to_screen(page, s, (title.x + title.w / 2.0, title.y + title.h / 2.0));
        let corner = to_screen(page, s, (title.x + title.w, title.y + title.h)) + vec2(-4.0, -3.0);
        for from in [middle, corner] {
            h.event(Event::PointerMoved(from));
            h.step();
            h.event(button(from, true));
            h.step();
            for i in 1..=3 {
                h.event(Event::PointerMoved(from + vec2(20.0, 15.0) * i as f32));
                h.step();
            }
            h.event(button(from + vec2(60.0, 45.0), false));
            h.step();
            let app = h.state();
            assert_eq!(app.session.project.layouts[li].element(title.id), Some(&title), "dragged from {from:?}");
            assert_eq!(app.layout.selected, Some(title.id), "it stays selected");
        }
        let key = Event::Key { key: Key::Backspace, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        h.event(key);
        h.step();
        let app = h.state();
        assert!(app.session.project.layouts[li].element(title.id).is_some(), "Backspace kept it");
        assert_eq!(app.layout.selected, Some(title.id));
        assert!(app.session.notices.iter().any(|n| n.text.contains(super::locked_delete())), "it says why");
    }

    /// A text element's properties set italic, alignment and letter spacing (#213, point c).
    #[test]
    fn a_text_element_takes_italic_alignment_and_spacing() {
        use egui_kittest::kittest::{NodeT, Queryable};
        use rustgis_engine::map::layout::{ElementKind, TextAlign};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Type"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        use egui::accesskit::{Action, ActionData, ActionRequest, Role};
        assert!(h.query_by_label("Alignment").is_some());
        // Ticking Italic, and setting the spacing as assistive technology does, store them.
        h.get_by_role_and_label(Role::CheckBox, "Italic").click();
        h.run_steps(2);
        let (node, tree) = h.get_by_role_and_label(Role::SpinButton, "Letter spacing").accesskit_node().locate();
        h.event(egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target_tree: tree,
            target_node: node,
            data: Some(ActionData::NumericValue(3.5)),
        }));
        h.run_steps(3);
        let kind = &h.state().session.project.layouts[li].element(title).unwrap().kind;
        assert!(
            matches!(kind, ElementKind::Text { italic: true, align: TextAlign::Left, letter_spacing, .. } if *letter_spacing == 3.5),
            "{kind:?}"
        );
        // The family (#517): Serif picked from the Font list is stored.
        h.get_by_role_and_label(Role::ComboBox, "Font").click();
        h.run_steps(2);
        h.get_by_label("Serif").click();
        h.run_steps(3);
        let kind = &h.state().session.project.layouts[li].element(title).unwrap().kind;
        assert!(matches!(kind, ElementKind::Text { font: rustgis_engine::map::layout::FontFamily::Serif, .. }), "{kind:?}");
        // Said where no serif is installed, and only there.
        let said = h.query_by_label_contains("No serif font is installed").is_some();
        assert_eq!(said, !rustgis_engine::render::fonts::serif_available());
    }

    /// A number dragged in the Format pane and let go is applied (#607): X moves the element by
    /// the drag, though the pane lends the saved X each frame (it used to end where it started).
    #[test]
    fn dragging_x_in_the_format_pane_moves_the_element() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Drag"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        // X is the pane's first number, the topmost: Name, X, Y, Width, Height.
        let at = h.query_all_by_role(Role::SpinButton).map(|n| n.rect()).min_by(|a, b| a.min.y.total_cmp(&b.min.y)).unwrap().center();
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let undo = h.state().session.undo_depth();
        h.event(Event::PointerMoved(at));
        h.step();
        h.event(button(at, true));
        h.step();
        // Dragged right over several frames (0.5 mm a point), then let go without moving.
        for i in 1..=4 {
            h.event(Event::PointerMoved(at + vec2(10.0 * i as f32, 0.0)));
            h.step();
        }
        h.event(button(at + vec2(40.0, 0.0), false));
        h.run_steps(3);
        let e = h.state().session.project.layouts[li].element(title.id).unwrap().clone();
        assert!(e.x - title.x > 10.0, "moved from {} to {}", title.x, e.x);
        assert_eq!(e.y, title.y, "only X changed");
        // Applied once, on release: one undo step, not one a frame of the drag.
        assert_eq!(h.state().session.undo_depth(), undo + 1, "one undo step");
        assert_eq!(h.state().session.undo_label(), Some("Element Properties"));
    }

    #[test]
    fn dragging_the_angle_in_the_format_pane_turns_the_arrow() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::{ElementKind, line_angle};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Turn", "template": "blank"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let id =
            app.run("layout.addElement", json!({"kind": "arrow", "angle": 0, "length": 60})).unwrap()["element"].as_u64().unwrap() as u32;
        app.layout.selected = Some(id);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        // Angle is the line's last number, the bottommost.
        let at = h.query_all_by_role(Role::SpinButton).map(|n| n.rect()).max_by(|a, b| a.min.y.total_cmp(&b.min.y)).unwrap().center();
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let undo = h.state().session.undo_depth();
        h.event(Event::PointerMoved(at));
        h.step();
        h.event(button(at, true));
        h.step();
        // Dragged right over several frames, then let go without moving: the turn is kept.
        for i in 1..=4 {
            h.event(Event::PointerMoved(at + vec2(10.0 * i as f32, 0.0)));
            h.step();
        }
        h.event(button(at + vec2(40.0, 0.0), false));
        h.run_steps(3);
        let e = h.state().session.project.layouts[li].element(id).unwrap().clone();
        let ElementKind::Line { dir, head, .. } = e.kind else { panic!("{:?}", e.kind) };
        let a = line_angle((e.x, e.y, e.w, e.h), dir, head);
        assert!(a > 10.0, "turned to {a}°");
        assert_eq!(h.state().session.undo_depth(), undo + 1, "one undo step");
    }

    /// A drag in the picker of a text's colour, in the shared text form, is one undo step (#1194).
    #[test]
    fn a_colour_drag_in_the_format_pane_is_one_undo_step() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Colour"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let color = |app: &RustGisApp| match &app.session.project.layouts[li].element(title).unwrap().kind {
            ElementKind::Text { color, .. } => *color,
            kind => panic!("{kind:?}"),
        };
        let before = color(h.state());
        let undo = h.state().session.undo_depth();
        let pane_numbers = h.query_all_by_role(Role::SpinButton).count();
        h.get_by_role(Role::ColorWell).click();
        h.run();
        // The picker's first number (red) follows the pane's own.
        let at = h.query_all_by_role(Role::SpinButton).nth(pane_numbers).unwrap().rect().center();
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        for dx in [16.0, 32.0, 48.0] {
            h.hover_at(at + vec2(dx, 0.0));
            h.step();
            assert_ne!(color(h.state()), before, "the page follows the picker");
            assert_eq!(h.state().session.undo_depth(), undo + 1, "one step however many frames");
        }
        h.drop_at(at + vec2(48.0, 0.0));
        h.run();
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(color(h.state()), before);
    }

    /// A layout's text has the shared text form (#516): the page's dynamic text as well as the
    /// map's, each added to the text and saved; the size in the range text graphics have.
    #[test]
    fn a_layout_text_offers_the_page_s_dynamic_text() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Tokens"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let text = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(title).unwrap().kind {
            ElementKind::Text { text, .. } => text.clone(),
            _ => panic!("a text"),
        };
        let before = text(&h);
        for item in ["Page number  {page}", "Map scale  {scale}"] {
            h.get_by_label("Insert Dynamic Text").click();
            h.run_steps(2);
            h.get_by_label(item).click();
            h.run_steps(2);
        }
        assert_eq!(text(&h), format!("{before}{{page}}{{scale}}"));
        // Larger than text graphics drew before, as a layout's text could already be.
        h.state_mut().run("layout.update", json!({"layout": li, "element": title, "size": 999})).unwrap();
        let size = match &h.state().session.project.layouts[li].element(title).unwrap().kind {
            ElementKind::Text { size, .. } => *size,
            _ => panic!("a text"),
        };
        assert_eq!(size, *rustgis_engine::map::graphics::FONT_SIZE.end());
    }

    /// An overview frame's properties show its extent indicator's colour and width (#213), and
    /// its Remove button takes the indicator away.
    #[test]
    fn an_overview_frame_shows_its_extent_indicator() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Overview"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let main = app.session.project.layouts[li].elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).unwrap().id;
        let inset = app.run("layout.addFrame", json!({"x": 200, "y": 150, "w": 60, "h": 40})).unwrap()["element"].as_u64().unwrap() as u32;
        app.run("layout.addElement", json!({"kind": "extentIndicator", "frame": inset, "of": main, "width": 1.5})).unwrap();
        app.layout.selected = Some(inset);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        assert!(h.query_by_label("Extent Indicator").is_some(), "the section shows for an overview frame");
        h.get_by_label("Remove Extent Indicator").click();
        h.run_steps(2);
        let kind = &h.state().session.project.layouts[li].element(inset).unwrap().kind;
        assert!(matches!(kind, ElementKind::MapFrame { extent_of: None, .. }), "removed");
        h.run_steps(2);
        assert!(h.query_by_label("Remove Extent Indicator").is_none(), "and its section with it");
    }

    /// A legend's properties list the layers of its frame's map; unticking one leaves it out of
    /// the legend.
    #[test]
    fn a_legend_leaves_out_the_layers_unticked() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Legend"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let legend = app.session.project.layouts[li].elements.iter().find(|e| matches!(e.kind, ElementKind::Legend { .. })).unwrap().id;
        // A layer the section doesn't list (graphics here), left out through the command, stays
        // left out whatever is ticked.
        app.session.map_mut().layers.iter_mut().find(|l| l.name == "Cities").unwrap().graphics = true;
        app.run("layout.update", json!({"element": legend, "exclude": ["Cities"]})).unwrap();
        app.layout.selected = Some(legend);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        assert!(h.query_by_role_and_label(Role::CheckBox, "Cities").is_none());
        h.get_by_role_and_label(Role::CheckBox, "Rivers").click();
        h.run_steps(2);
        let left_out =
            |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(legend).unwrap().kind {
                ElementKind::Legend { exclude, .. } => exclude.clone(),
                _ => panic!("a legend"),
            };
        assert_eq!(left_out(&h), ["Cities", "Rivers"]);
        // Ticked again, it's listed again.
        h.get_by_role_and_label(Role::CheckBox, "Rivers").click();
        h.run_steps(2);
        assert_eq!(left_out(&h), ["Cities"]);
    }

    /// The layout view with the Format pane beside it, as the app draws them: after following
    /// what agents ran (`RustGisApp::logic`), and saving drafts at the end.
    fn with_format(app: &mut RustGisApp, ui: &mut egui::Ui) {
        super::follow_project(app);
        let t = crate::theme::Tokens::get(ui.ctx());
        if app.ui.right == Some(crate::RightPane::Format) {
            egui::Panel::right("format_pane").exact_size(320.0).show(ui, |ui| super::format(app, ui, &t));
        }
        super::show(app, ui);
        super::save_drafts(app);
    }

    /// A frame's grid in the Format pane: a graticule's degree rows, or a measured grid's
    /// coordinate system (typed, set once left) and short labels.
    #[test]
    fn a_measured_grid_is_set_in_the_format_pane() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Hike"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let frame = app.session.project.layouts[li].elements.iter().find(|e| e.kind.code() == "mapFrame").unwrap().id;
        app.run("layout.addElement", json!({"kind": "grid"})).unwrap();
        app.run("ui.pane.format", json!({})).unwrap();
        app.layout.selected = Some(frame);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 1400.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let grid = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(frame).unwrap().kind {
            ElementKind::MapFrame { grid, .. } => grid.clone().unwrap(),
            _ => panic!("not a frame"),
        };
        assert!(h.query_by_label("N/S/E/W").is_some() && h.query_by_label("Coordinate system").is_none(), "a graticule's rows");
        h.state_mut().run("layout.update", json!({"element": frame, "grid": {"type": "measured"}})).unwrap();
        h.run_steps(3);
        assert!(h.query_by_label("N/S/E/W").is_none(), "no degrees for a measured grid");
        assert!(h.query_by_label("Short labels").is_some());
        // A system typed is set once the field is left; until then, the grid's stays.
        h.get_by_role_and_label(Role::TextInput, "Coordinate system").focus();
        h.run_steps(2);
        h.get_by_role_and_label(Role::TextInput, "Coordinate system").type_text("EPSG:2056");
        h.run_steps(2);
        assert_eq!(grid(&h).crs, None, "not while typing");
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(grid(&h).crs.as_deref(), Some("EPSG:2056"));
        h.get_by_label("Short labels").click();
        h.run_steps(2);
        assert!(grid(&h).short);
    }

    /// A scale bar's and a north arrow's style is picked from pictures in the Format pane, the
    /// unit typed in Thai, the arrow's letter typed. (No Thai face is installed here: the app's
    /// script fonts aren't loaded in tests, so Thai draws as boxes.)
    #[test]
    fn surrounds_are_styled_from_pictures_in_the_format_pane() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::{ArrowStyle, ElementKind, ScaleBarStyle, UnitLabel};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Thai"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let find =
            |app: &RustGisApp, code: &str| app.session.project.layouts[li].elements.iter().find(|e| e.kind.code() == code).unwrap().id;
        let (bar, arrow) = (find(&app, "scaleBar"), find(&app, "northArrow"));
        app.run("ui.pane.format", json!({})).unwrap();
        app.layout.selected = Some(bar);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(vec2(1200.0, 1600.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let kind =
            |h: &egui_kittest::Harness<'_, RustGisApp>, id: u32| h.state().session.project.layouts[li].element(id).unwrap().kind.clone();
        h.get_by_role_and_label(Role::Button, "Ruler").click();
        h.run_steps(2);
        assert!(matches!(kind(&h, bar), ElementKind::ScaleBar { style: ScaleBarStyle::Ruler, .. }));
        h.get_by_role_and_label(Role::TextInput, "Unit text").focus();
        h.run_steps(2);
        h.get_by_role_and_label(Role::TextInput, "Unit text").type_text("กิโลเมตร");
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert!(
            matches!(kind(&h, bar), ElementKind::ScaleBar { unit_label: UnitLabel::Custom, ref unit_text, .. } if unit_text == "กิโลเมตร"),
            "{:?}",
            kind(&h, bar)
        );
        h.state_mut().layout.selected = Some(arrow);
        h.run_steps(3);
        h.get_by_role_and_label(Role::Button, "Star in a ring").click();
        h.run_steps(2);
        assert!(matches!(kind(&h, arrow), ElementKind::NorthArrow { style: ArrowStyle::Rose, .. }));
        h.get_by_role_and_label(Role::TextInput, "Letter").focus();
        h.run_steps(2);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.get_by_role_and_label(Role::TextInput, "Letter").type_text("น");
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert!(matches!(kind(&h, arrow), ElementKind::NorthArrow { ref label, .. } if label == "น"), "{:?}", kind(&h, arrow));
    }

    /// A legend's type, patches and texts are set in the Format pane: a layer's name and a
    /// class's label typed over, in Thai, the area patch picked, and its labels' size.
    #[test]
    fn a_legends_items_and_type_are_set_in_the_format_pane() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::{AreaPatch, ElementKind};
        let mut app = RustGisApp::with_sample();
        app.run("layer.symbology", json!({"layer": "Countries", "type": "unique", "field": "continent"})).unwrap();
        app.run("layout.new", json!({"name": "Legend look"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let legend = app.session.project.layouts[li].elements.iter().find(|e| e.kind.code() == "legend").unwrap().id;
        app.run("ui.pane.format", json!({})).unwrap();
        app.layout.selected = Some(legend);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(vec2(1200.0, 4000.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| with_format(app, ui));
            },
            app,
        );
        h.run_steps(3);
        let look = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(legend).unwrap().kind {
            ElementKind::Legend { look, .. } => (**look).clone(),
            k => panic!("{k:?}"),
        };
        // The top layer's name, typed over.
        let field = h.get_all_by_role_and_label(Role::TextInput, "Shown as").next().unwrap();
        field.focus();
        h.run_steps(2);
        h.get_all_by_role_and_label(Role::TextInput, "Shown as").next().unwrap().type_text("ชั้นบน");
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        let top = h.state().session.project.maps[0].layers.last().unwrap().name.clone();
        assert_eq!(look(&h).name_of(&top), Some("ชั้นบน"), "{:?}", look(&h));
        // A class's label, typed over.
        h.get_by_role_and_label(Role::TextInput, "Asia").focus();
        h.run_steps(2);
        h.get_by_role_and_label(Role::TextInput, "Asia").type_text("เอเชีย");
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(look(&h).label_of("Countries", "Asia"), Some("เอเชีย"), "{:?}", look(&h));
        // The area patch, from its list.
        h.get_by_role_and_label(Role::ComboBox, "Area patch").click();
        h.run_steps(2);
        h.get_by_label("Natural area").click();
        h.run_steps(3);
        assert_eq!(look(&h).area_patch, AreaPatch::Natural);
        // The symbology button opens the layer's Symbology pane.
        h.get_all_by_role_and_label(Role::Button, "Symbology…").next().unwrap().click();
        h.run_steps(3);
        assert_eq!(h.state().session.active_layer, h.state().session.project.maps[0].layers.last().map(|l| l.id));
        assert!(h.state().ui.right_open.contains(&crate::RightPane::Symbology));
    }

    /// A class's label takes a type of its own in Legend items (its Aa button, then the type
    /// form), and the legend converts to graphics from the pane: grouped texts and patches,
    /// selected where it was.
    #[test]
    fn a_legends_items_take_their_own_type_and_it_converts_to_graphics() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layer.symbology", json!({"layer": "Countries", "type": "unique", "field": "continent"})).unwrap();
        app.run("layout.new", json!({"name": "Legend items"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let legend = app.session.project.layouts[li].elements.iter().find(|e| e.kind.code() == "legend").unwrap().id;
        app.run("ui.pane.format", json!({})).unwrap();
        app.layout.selected = Some(legend);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(vec2(1200.0, 6000.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| with_format(app, ui));
            },
            app,
        );
        h.run_steps(3);
        let look = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(legend).unwrap().kind {
            ElementKind::Legend { look, .. } => (**look).clone(),
            k => panic!("{k:?}"),
        };
        // Asia's Aa opens its type form; Bold there gives it a bold type of its own.
        assert!(h.query_by_role_and_label(Role::CheckBox, "Bold").is_none(), "folded until asked for");
        h.get_by_role_and_label(Role::Button, "Type of Asia").click();
        h.run_steps(3);
        h.get_by_role_and_label(Role::CheckBox, "Bold").click();
        h.run_steps(3);
        let t = look(&h).label_type("Countries", "Asia").cloned().expect("Asia has its own type");
        assert!(t.bold && !look(&h).label.bold, "{t:?}");
        assert!(look(&h).label_type("Countries", "Europe").is_none(), "only Asia");
        // Back to the legend's type.
        h.get_by_role_and_label(Role::Button, "Use the legend's type").click();
        h.run_steps(3);
        assert!(look(&h).label_type("Countries", "Asia").is_none());
        // Converted: the legend is replaced by grouped graphics, the first selected.
        h.get_by_role_and_label(Role::Button, "Convert to Graphics").click();
        h.run_steps(3);
        let l = &h.state().session.project.layouts[li];
        assert!(l.element(legend).is_none(), "the legend is gone");
        let sel = h.state().layout.selected.and_then(|id| l.element(id)).expect("a graphic selected");
        assert!(sel.group.is_some() && matches!(sel.kind, ElementKind::Text { .. }), "{sel:?}");
        assert!(l.elements.iter().any(|e| matches!(&e.kind, ElementKind::Text { text, .. } if text == "Asia")));
        assert!(l.elements.iter().any(|e| matches!(e.kind, ElementKind::Swatch { .. })));
    }

    /// Selecting an element opens the Format pane, a tab like the others (#516). Closing it keeps
    /// the selection, and selecting another element brings it back.
    #[test]
    fn a_selected_element_is_formatted_in_its_pane() {
        use crate::RightPane::{Bookmarks, Format};
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Format"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let ids: Vec<u32> = app.session.project.layouts[li].elements.iter().map(|e| e.id).collect();
        app.run("ui.pane.bookmarks", json!({})).unwrap();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(2);
        assert_eq!(h.state().ui.right, Some(Bookmarks), "nothing selected, nothing opened");
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        assert!(h.query_by_label("Select an element to format it.").is_some(), "opened by hand: nothing to format");
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.state_mut().layout.selected = ids.first().copied();
        h.run_steps(2);
        assert_eq!(
            (h.state().ui.right, h.state().ui.right_open.clone()),
            (Some(Format), vec![Bookmarks, Format]),
            "a tab beside the others"
        );
        assert!(h.query_all_by_label("Name").next().is_some(), "the element's properties");
        // Closed: the selection stays, and the pane doesn't come back by itself.
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        assert_eq!((h.state().ui.right, h.state().layout.selected), (Some(Bookmarks), ids.first().copied()));
        // Another element selected: it is back.
        h.state_mut().layout.selected = ids.get(1).copied();
        h.run_steps(2);
        assert_eq!(h.state().ui.right, Some(Format));
        // A name being typed when the pane closes is kept, the element still selected.
        use egui::accesskit::Role;
        let name = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.project.layouts[li].element(ids[1]).unwrap().name.clone();
        let before = name(&h);
        h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some(before.as_str())).unwrap().focus();
        h.run_steps(2);
        h.query_all_by_role(Role::TextInput).find(|n| n.is_focused()).unwrap().type_text("Draft");
        h.run_steps(2);
        assert_eq!(name(&h), before, "still being typed");
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        assert!(name(&h).contains("Draft"), "saved when the pane closed: {}", name(&h));
        assert_eq!(h.state().layout.selected, ids.get(1).copied());
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        // The layout out of sight, under the Model Builder or for the map: nothing to edit.
        let hint = |h: &egui_kittest::Harness<'_, RustGisApp>| h.query_by_label("Select an element to format it.").is_some();
        (h.state_mut().mb.open, h.state_mut().mb.active) = (true, true);
        h.run_steps(2);
        assert!(hint(&h), "under the Model Builder");
        (h.state_mut().mb.open, h.state_mut().mb.active) = (false, false);
        h.run_steps(2);
        assert!(!hint(&h));
        h.state_mut().run("layout.close", json!({})).unwrap();
        h.run_steps(2);
        assert!(hint(&h), "on the map");
        assert_eq!(h.state().ui.right, Some(Format), "the pane stays, as other panes do");
        // Closed, then the element of the same id selected in another layout: that is another
        // element, so the pane opens for it.
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.state_mut().run("layout.new", json!({"name": "Other"})).unwrap();
        let other = h.state().session.active_layout.unwrap();
        assert!(h.state().session.project.layouts[other].element(ids[1]).is_some(), "the id exists there too");
        h.state_mut().layout.selected = ids.get(1).copied();
        h.run_steps(2);
        assert_eq!(h.state().ui.right, Some(Format));
    }

    /// Text being typed when another project opens is for the old one: it never reaches the
    /// element of the same id in the new one, whether the app opened it or an agent did through
    /// the session itself (the MCP relay, the file bridge).
    #[test]
    fn a_draft_is_dropped_with_its_project() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        for by_agent in [false, true] {
            let mut app = RustGisApp::with_sample();
            app.run("layout.new", json!({"name": "Drafts"})).unwrap();
            let li = app.session.active_layout.unwrap();
            let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
            let text = serde_json::to_string(&app.session.project).unwrap();
            app.layout.selected = Some(title);
            let mut fonts = false;
            let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
                move |ui, app: &mut RustGisApp| {
                    if !fonts {
                        crate::theme::install_fonts(ui.ctx(), &[], None);
                        fonts = true;
                        return;
                    }
                    with_format(app, ui);
                },
                app,
            );
            h.run_steps(3);
            h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("Title")).unwrap().focus();
            h.run_steps(2);
            h.query_all_by_role(Role::TextInput).find(|n| n.is_focused()).unwrap().type_text("Draft");
            h.run_steps(2);
            assert!(!h.state().layout.typing.is_empty(), "being typed");
            let open = json!({"text": text});
            if by_agent {
                // As the relay runs an agent's command: on the session, past `RustGisApp::run`;
                // the copy's layout opened in the same go.
                h.state_mut().session.run("project.openText", &open).unwrap();
                h.state_mut().session.run("layout.activate", &json!({"layout": li})).unwrap();
            } else {
                h.state_mut().run("project.openText", open).unwrap();
            }
            h.run_steps(2);
            let app = h.state();
            assert_eq!(app.session.project.layouts[li].element(title).map(|e| e.name.as_str()), Some("Title"), "agent: {by_agent}");
            assert!(app.layout.typing.is_empty() && app.layout.selected.is_none(), "agent: {by_agent}");
        }
    }

    /// Element ids are per layout: opening another layout ends the selection, so Delete or
    /// Ctrl+D never reaches the element of the same id there.
    #[test]
    fn opening_another_layout_clears_the_selection() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "First"})).unwrap();
        app.run("layout.new", json!({"name": "Second"})).unwrap();
        app.run("layout.activate", json!({"name": "First"})).unwrap();
        let first = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[first].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        app.run("layout.removeElement", json!({"element": 999_999})).ok();
        assert_eq!(app.layout.selected, Some(title), "a command in the same layout keeps it");
        app.run("layout.activate", json!({"name": "Second"})).unwrap();
        assert_ne!(app.session.active_layout, Some(first));
        assert!(app.session.project.layouts[app.session.active_layout.unwrap()].element(title).is_some(), "the id exists there too");
        assert_eq!(app.layout.selected, None);
    }

    #[test]
    fn dragged_elements_snap_to_the_nearest_line() {
        let lines = (vec![0.0, 100.0, 150.0], vec![0.0, 50.0]);
        assert_eq!(snap_to(&[98.5, 120.0], &lines.0, 2.0), Some((1.5, 100.0)));
        assert_eq!(snap_to(&[90.0], &lines.0, 2.0), None);
        let r = (10.0, 10.0, 40.0, 20.0);
        // Moved: the right edge (50 + 49) is nearest a line, so the box lands with it on 100.
        let (moved, sx, sy) = landing(r, None, (49.0, 0.5), Some((&lines, 2.0)));
        assert_eq!((moved, sx, sy), ((60.0, 10.5, 40.0, 20.0), Some(100.0), None));
        // Without snapping it lands where the pointer put it.
        assert_eq!(landing(r, None, (49.0, 0.5), None).0, (59.0, 10.5, 40.0, 20.0));
        // Resized from the bottom-right handle: only those edges snap (148.5 → 150, 49 → 50);
        // the top-left stays.
        let (sized, sx, sy) = landing(r, Some(4), (98.5, 19.0), Some((&lines, 2.0)));
        assert_eq!((sized, sx, sy), ((10.0, 10.0, 140.0, 40.0), Some(150.0), Some(50.0)));
    }

    /// Dragging out of the top ruler adds a horizontal guide; dragging it back onto the ruler
    /// removes it.
    #[test]
    fn guides_come_out_of_the_rulers_and_go_back() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Guides", "template": "blank"})).unwrap();
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let li = h.state().session.active_layout.unwrap();
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let drag = |h: &mut egui_kittest::Harness<'_, RustGisApp>, from: Pos2, to: Pos2| {
            let button =
                |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            h.event(Event::PointerMoved(from));
            h.step();
            h.event(button(from, true));
            h.step();
            for i in 1..=6 {
                h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
                h.step();
            }
            h.event(button(to, false));
            h.step();
        };
        let ruler = Pos2::new(page.center().x, area.get().top() + RULER / 2.0);
        let at = to_screen(page, s, (0.0, 40.0));
        drag(&mut h, ruler, Pos2::new(ruler.x, at.y));
        let y = h.state().session.project.layouts[li].guides.y.clone();
        assert_eq!(y.len(), 1, "a guide was added: {y:?}");
        assert!((y[0] - 40.0).abs() < 0.5, "at about 40 mm: {y:?}");
        drag(&mut h, Pos2::new(page.center().x, to_screen(page, s, (0.0, y[0])).y), ruler);
        assert!(h.state().session.project.layouts[li].guides.y.is_empty(), "dropped on the ruler, it's gone");
    }

    #[test]
    fn widening_a_hairline_keeps_it_thin() {
        let rule = (10.0, 30.0, 100.0, 0.15);
        assert_eq!(resized(rule, 3, 20.0, 5.0), (10.0, 30.0, 120.0, 0.15));
        assert_eq!(resized(rule, 7, -10.0, 0.0), (0.0, 30.0, 110.0, 0.15));
        // Corner and top/bottom handles size the height, at least 2 mm.
        assert_eq!(resized(rule, 4, 0.0, 0.5), (10.0, 30.0, 100.0, 2.0));
        assert_eq!(resized(rule, 5, 0.0, 0.5), (10.0, 30.0, 100.0, 2.0));
    }
}
