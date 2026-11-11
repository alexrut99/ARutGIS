//! The document's fonts: for each face text is set in, Type 3 fonts whose glyphs are the face's
//! outlines, in font units, 256 glyphs to a font (a Type 3 font's codes are single bytes), each
//! with the map from its codes back to the characters they stand for.

use std::collections::HashMap;
use std::fmt::Write as _;

use ab_glyph::{Font, FontRef, GlyphId, OutlineCurve};

use super::Doc;
use super::content::num;

#[derive(Default)]
pub(super) struct Fonts {
    faces: Vec<Face>,
    /// Faces by the address of their `FontRef` (fonts are parsed once and kept).
    by_font: HashMap<usize, usize>,
}

struct Face {
    font: &'static FontRef<'static>,
    upm: f32,
    chunks: Vec<Chunk>,
    /// The chunk and code of each thing drawn: a glyph that stands for two characters (the AA of
    /// a SARA AM, and an AA written) or is drawn at two heights has a code for each.
    codes: HashMap<Shape, (usize, u8)>,
}

/// What a code draws and stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Shape {
    id: u16,
    /// The character copying its text gives ([`crate::text::NO_CHAR`]: none).
    ch: char,
    /// The character after it that `with` stands for ([`crate::text::NO_CHAR`]: none; `with` is
    /// then part of `ch`).
    also: char,
    /// How far above the baseline it is drawn, in font units: a mark a shaper raises.
    rise: i16,
    /// Another glyph drawn with it, its origin this far (right, up; font units) from this one's:
    /// the ring of a SARA AM with its AA, the two standing for the one character; or a tone mark
    /// with the vowel it is over, the two standing for their two characters.
    with: Option<(u16, i16, i16)>,
}

struct Chunk {
    /// The font's object number.
    id: usize,
    glyphs: Vec<Shape>,
}

/// Where a glyph is: its font's object number, its code there and its advance in ems.
pub(super) struct Placed {
    pub font: usize,
    pub code: u8,
    pub advance: f32,
}

impl Fonts {
    /// `id` of `font` (standing for `ch`, raised `rise` ems above the baseline: a mark a shaper
    /// moves; drawn with the glyph `with`, its origin so many ems right and up of this one's),
    /// given a code in one of its Type 3 fonts the first time; `None` only past what a font can
    /// hold. The rise is in the glyph's procedure, so the glyph is set on the baseline as the
    /// rest of its line is and the text reads in order.
    pub fn place(
        &mut self,
        doc: &mut Doc,
        font: &'static FontRef<'static>,
        id: GlyphId,
        (ch, also): (char, char),
        rise: f32,
        with: Option<(GlyphId, f32, f32)>,
    ) -> Option<Placed> {
        let key = std::ptr::from_ref(font) as usize;
        let fi = match self.by_font.get(&key) {
            Some(&i) => i,
            None => {
                // A face with no usable units per em is read as the usual 1000, as the pixels read
                // it at one unit a pixel's em: its text is kept, not left out.
                let upm = font.units_per_em().filter(|u| u.is_finite() && *u > 0.0).unwrap_or(1000.0);
                self.faces.push(Face { font, upm, chunks: Vec::new(), codes: HashMap::new() });
                self.by_font.insert(key, self.faces.len() - 1);
                self.faces.len() - 1
            }
        };
        let face = self.faces.get_mut(fi)?;
        let advance = font.h_advance_unscaled(id) / face.upm;
        let advance = if advance.is_finite() { advance } else { 0.0 };
        let upm = face.upm;
        let units = |ems: f32| if ems.is_finite() { (ems * upm).round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16 } else { 0 };
        let shape = Shape { id: id.0, ch, also, rise: units(rise), with: with.map(|(g, x, y)| (g.0, units(x), units(y))) };
        let (ci, code) = match face.codes.get(&shape) {
            Some(&c) => c,
            None => {
                if face.chunks.last().is_none_or(|c| c.glyphs.len() >= 256) {
                    face.chunks.push(Chunk { id: doc.reserve(), glyphs: Vec::new() });
                }
                let ci = face.chunks.len() - 1;
                let chunk = face.chunks.get_mut(ci)?;
                let code = u8::try_from(chunk.glyphs.len()).ok()?;
                chunk.glyphs.push(shape);
                face.codes.insert(shape, (ci, code));
                (ci, code)
            }
        };
        Some(Placed { font: face.chunks.get(ci)?.id, code, advance })
    }

    /// Writes every font given a glyph.
    pub fn write(self, doc: &mut Doc) {
        for face in &self.faces {
            for chunk in &face.chunks {
                write_chunk(doc, face, chunk);
            }
        }
    }
}

/// One Type 3 font: its glyph procedures, widths, encoding and map back to Unicode.
fn write_chunk(doc: &mut Doc, face: &Face, chunk: &Chunk) {
    let mut procs = String::new();
    let mut widths = String::new();
    let mut names = String::new();
    let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (code, shape) in chunk.glyphs.iter().enumerate() {
        let advance = face.font.h_advance_unscaled(GlyphId(shape.id));
        let advance = if advance.is_finite() { advance } else { 0.0 };
        let mut parts = vec![(GlyphId(shape.id), 0.0, f32::from(shape.rise))];
        if let Some((g, x, y)) = shape.with {
            parts.push((GlyphId(g), f32::from(x), f32::from(shape.rise) + f32::from(y)));
        }
        let (ops, b) = glyph_procedure(face.font, &parts, advance);
        if let Some(b) = b {
            bbox = [bbox[0].min(b[0]), bbox[1].min(b[1]), bbox[2].max(b[2]), bbox[3].max(b[3])];
        }
        let proc_id = doc.reserve();
        doc.stream(proc_id, "", &ops, false);
        let _ = write!(procs, "/g{code} {proc_id} 0 R ");
        let _ = write!(names, "/g{code}");
        let mut w = Vec::new();
        num(&mut w, advance, 2);
        widths.push_str(&String::from_utf8_lossy(&w));
        widths.push(' ');
    }
    if bbox[0] > bbox[2] {
        bbox = [0.0; 4];
    }
    let to_unicode = doc.reserve();
    doc.stream(to_unicode, "", to_unicode_cmap(&chunk.glyphs.iter().map(|g| (g.ch, g.also)).collect::<Vec<_>>()).as_bytes(), false);
    let mut matrix = Vec::new();
    for v in [1.0 / face.upm, 0.0, 0.0, 1.0 / face.upm, 0.0, 0.0] {
        num(&mut matrix, v, 9);
        matrix.push(b' ');
    }
    let mut b = Vec::new();
    for v in bbox {
        num(&mut b, v, 0);
        b.push(b' ');
    }
    doc.put(
        chunk.id,
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [{}] /FontMatrix [{}] /CharProcs << {procs}>> /Encoding << /Type /Encoding /Differences [0 {names}] >> /FirstChar 0 /LastChar {} /Widths [{widths}] /Resources << >> /ToUnicode {to_unicode} 0 R >>",
            String::from_utf8_lossy(&b).trim_end(),
            String::from_utf8_lossy(&matrix).trim_end(),
            chunk.glyphs.len().saturating_sub(1),
        )
        .as_bytes(),
    );
}

/// A glyph's procedure: its advance and box (`d1`, so it takes the text's colour) and its
/// outline, filled: that of each of `parts`, a glyph with its origin so far right and up, in font
/// units (one glyph at the origin, but for a mark a shaper raises and a SARA AM's two glyphs);
/// and its box in font units, `None` for a glyph with no outline (a space).
fn glyph_procedure(font: &FontRef<'static>, parts: &[(GlyphId, f32, f32)], advance: f32) -> (Vec<u8>, Option<[f32; 4]>) {
    let mut path = Vec::new();
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    let mut grow = |p: ab_glyph::Point| {
        if p.x.is_finite() && p.y.is_finite() {
            b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
        }
    };
    let pt = |out: &mut Vec<u8>, p: ab_glyph::Point| {
        num(out, p.x, 1);
        out.push(b' ');
        num(out, p.y, 1);
        out.push(b' ');
    };
    for &(id, dx, dy) in parts {
        let Some(o) = font.outline(id) else { continue };
        let mv = |p: ab_glyph::Point| ab_glyph::point(p.x + dx, p.y + dy);
        {
            let mut last: Option<ab_glyph::Point> = None;
            // Every curve: the outline is already read, and a glyph cut short would draw wrong.
            for c in &o.curves {
                let (start, end) = match c {
                    OutlineCurve::Line(a, b) | OutlineCurve::Quad(a, _, b) | OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
                };
                if last != Some(start) {
                    if last.is_some() {
                        path.extend_from_slice(b"h\n");
                    }
                    grow(mv(start));
                    pt(&mut path, mv(start));
                    path.extend_from_slice(b"m\n");
                }
                match c {
                    OutlineCurve::Line(_, e) => {
                        grow(mv(*e));
                        pt(&mut path, mv(*e));
                        path.extend_from_slice(b"l\n");
                    }
                    OutlineCurve::Quad(s, k, e) => {
                        // A quadratic as the cubic with the same curve.
                        let lerp = |a: ab_glyph::Point, t: f32| ab_glyph::point(a.x + (k.x - a.x) * t, a.y + (k.y - a.y) * t);
                        for p in [lerp(*s, 2.0 / 3.0), lerp(*e, 2.0 / 3.0), *e] {
                            grow(mv(p));
                            pt(&mut path, mv(p));
                        }
                        path.extend_from_slice(b"c\n");
                    }
                    OutlineCurve::Cubic(_, k1, k2, e) => {
                        for p in [*k1, *k2, *e] {
                            grow(mv(p));
                            pt(&mut path, mv(p));
                        }
                        path.extend_from_slice(b"c\n");
                    }
                }
                last = Some(end);
            }
            if last.is_some() {
                path.extend_from_slice(b"h\nf\n");
            }
        }
    }
    // The box holds the glyph's origin too: a mark drawn wholly left of it (a Thai vowel or tone
    // mark, which takes no advance) is otherwise refused by some readers as a bad box, and left
    // out.
    let bbox = (b[0] <= b[2]).then_some([b[0].min(0.0), b[1], b[2].max(0.0), b[3]]);
    let mut ops = Vec::with_capacity(path.len() + 48);
    num(&mut ops, advance, 2);
    ops.extend_from_slice(b" 0 ");
    let [x0, y0, x1, y1] = bbox.unwrap_or([0.0; 4]);
    for v in [x0.floor(), y0.floor(), x1.ceil(), y1.ceil()] {
        num(&mut ops, v, 0);
        ops.push(b' ');
    }
    ops.extend_from_slice(b"d1\n");
    ops.extend_from_slice(&path);
    (ops, bbox)
}

/// The CMap taking each code of a font to the character its glyph stands for, or the two (the
/// second [`crate::text::NO_CHAR`] when there is one).
fn to_unicode_cmap(glyphs: &[(char, char)]) -> String {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    // A glyph that stands for no character is marked on the page as having no text; for a reader
    // that doesn't take the mark, it stands for a space of no width.
    let none = crate::text::NO_CHAR;
    let entries: Vec<(usize, String)> = glyphs
        .iter()
        .enumerate()
        .map(|(code, (ch, also))| {
            let text = [if *ch == none { '\u{200B}' } else { *ch }, *also];
            let hex = text
                .iter()
                .filter(|c| **c != none)
                .flat_map(|c| c.encode_utf16(&mut [0; 2]).iter().map(|u| format!("{u:04X}")).collect::<Vec<_>>());
            (code, hex.collect())
        })
        .collect();
    // At most 100 entries to a block.
    for block in entries.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", block.len());
        for (code, hex) in block {
            let _ = writeln!(s, "<{code:02X}> <{hex}>");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_share_a_font_until_it_holds_256() {
        let font = crate::text::label_font().unwrap();
        let mut doc = Doc::new();
        let mut fonts = Fonts::default();
        let a = fonts.place(&mut doc, font, font.glyph_id('A'), ('A', crate::text::NO_CHAR), 0.0, None).unwrap();
        let again = fonts.place(&mut doc, font, font.glyph_id('A'), ('A', crate::text::NO_CHAR), 0.0, None).unwrap();
        assert_eq!((a.font, a.code), (again.font, again.code));
        assert!(a.advance > 0.3 && a.advance < 1.0, "an em fraction: {}", a.advance);
        // Glyph ids past what one font holds go to a second.
        let mut last = a;
        for g in 1..300u16 {
            last = fonts.place(&mut doc, font, GlyphId(g), ('x', crate::text::NO_CHAR), 0.0, None).unwrap();
        }
        assert_ne!(last.font, again.font);
        // 'A' and the 299 standing for 'x' (one of them A's glyph, which has a code for each
        // character it stands for).
        assert_eq!(fonts.faces[0].chunks.iter().map(|c| c.glyphs.len()).collect::<Vec<_>>(), vec![256, 300 - 256]);
        // A glyph raised, or drawn with another, is a code of its own; the same again is not.
        let raised = fonts.place(&mut doc, font, font.glyph_id('A'), ('A', crate::text::NO_CHAR), 0.25, None).unwrap();
        let pair = (font.glyph_id('V'), 0.1, 0.2);
        assert_ne!((raised.font, raised.code), (again.font, again.code));
        let with = fonts.place(&mut doc, font, font.glyph_id('A'), ('A', 'V'), 0.0, Some(pair)).unwrap();
        let again = fonts.place(&mut doc, font, font.glyph_id('A'), ('A', 'V'), 0.0, Some(pair)).unwrap();
        assert_ne!((with.font, with.code), (raised.font, raised.code));
        assert_eq!((with.font, with.code), (again.font, again.code));
        assert_eq!(fonts.faces[0].chunks.iter().map(|c| c.glyphs.len()).sum::<usize>(), 302);
    }

    #[test]
    fn a_glyph_procedure_is_its_outline_in_font_units() {
        let font = crate::text::label_font().unwrap();
        let id = font.glyph_id('H');
        let (ops, bbox) = glyph_procedure(font, &[(id, 0.0, 0.0)], font.h_advance_unscaled(id));
        let ops = String::from_utf8(ops).unwrap();
        assert!(ops.contains(" d1\n") && ops.contains(" m\n") && ops.contains(" l\n") && ops.trim_end().ends_with("h\nf"), "{ops}");
        let [x0, y0, x1, y1] = bbox.unwrap();
        // An H stands on the baseline, a cap height tall.
        let upm = font.units_per_em().unwrap();
        assert!(y0.abs() < upm * 0.02 && (y1 - y0) > upm * 0.5 && x1 > x0, "{bbox:?}");
        let (space, none) = glyph_procedure(font, &[(font.glyph_id(' '), 0.0, 0.0)], 200.0);
        assert!(none.is_none());
        assert_eq!(String::from_utf8(space).unwrap(), "200 0 0 0 0 0 d1\n");
    }

    #[test]
    fn the_unicode_map_spans_blocks_and_astral_characters() {
        let glyphs: Vec<(char, char)> = (0..150).map(|i| (if i == 120 { '🙂' } else { 'é' }, crate::text::NO_CHAR)).collect();
        let cmap = to_unicode_cmap(&glyphs);
        assert_eq!(cmap.matches("beginbfchar").count(), 2);
        assert!(cmap.contains("100 beginbfchar") && cmap.contains("50 beginbfchar"));
        assert!(cmap.contains("<00> <00E9>") && cmap.contains("<78> <D83DDE42>"));
    }
}
