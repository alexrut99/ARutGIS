//! Where a line may break in text written without spaces between its words: Thai, Lao, Khmer and
//! Burmese. A line of it can't be broken at spaces as other text is, nor between any two letters
//! (a vowel or tone mark would leave its consonant, and words would be cut in two): the places
//! are between words, which take a dictionary to find.
//!
//! The words are found by ICU4X's line segmenter (Unicode's UAX #14 with its dictionaries and
//! its LSTM models for these scripts). The dictionary knows compounds and place names the model
//! cuts ("เชียงใหม่", "กรุงเทพมหานคร") and the model keeps names the dictionary doesn't know whole
//! ("อินทนนท์", which the dictionary cuts at every syllable), so a line breaks only where both
//! find a break.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use icu_segmenter::options::LineBreakOptions;
use icu_segmenter::{LineSegmenter, LineSegmenterBorrowed};

/// The most of a text looked at for breaks, in bytes: past it there are none.
const MAX_BYTES: usize = 8192;
/// The texts whose breaks are kept, labels being wrapped again at every draw.
const KEPT: usize = 2048;

/// Whether `c` is a letter or mark of a script written without spaces between words.
fn unspaced_char(c: char) -> bool {
    matches!(c, '\u{0E01}'..='\u{0E3A}' | '\u{0E40}'..='\u{0E4E}' | '\u{0E81}'..='\u{0ECE}' | '\u{0EDC}'..='\u{0EDF}' | '\u{1000}'..='\u{103F}' | '\u{1050}'..='\u{109F}' | '\u{1780}'..='\u{17D3}')
}

/// Whether `text` has Thai, Lao, Khmer or Burmese in it.
pub fn unspaced(text: &str) -> bool {
    text.chars().any(unspaced_char)
}

/// The places in `text` (byte offsets, ascending, each on a character boundary and inside it)
/// where a line may break between two words of Thai, Lao, Khmer or Burmese. Breaks at spaces,
/// dashes and the like are the caller's, as in any text; text without those scripts has none.
pub fn breaks(text: &str) -> Arc<[usize]> {
    static NONE: OnceLock<Arc<[usize]>> = OnceLock::new();
    let none = || NONE.get_or_init(|| Arc::from(Vec::new())).clone();
    if text.len() > MAX_BYTES || !unspaced(text) {
        return none();
    }
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<[usize]>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(found) = cache.lock().ok().and_then(|c| c.get(text).cloned()) {
        return found;
    }
    let found: Arc<[usize]> = Arc::from(find(text));
    if let Ok(mut c) = cache.lock() {
        if c.len() >= KEPT {
            c.clear();
        }
        c.insert(text.to_string(), found.clone());
    }
    found
}

/// [`breaks`], found afresh.
fn find(text: &str) -> Vec<usize> {
    static SEGMENTERS: OnceLock<(LineSegmenterBorrowed<'static>, LineSegmenterBorrowed<'static>)> = OnceLock::new();
    let (dictionary, model) = SEGMENTERS
        .get_or_init(|| (LineSegmenter::new_dictionary(LineBreakOptions::default()), LineSegmenter::new_lstm(LineBreakOptions::default())));
    let by_model: Vec<usize> = model.segment_str(text).collect();
    dictionary
        .segment_str(text)
        .filter(|at| by_model.binary_search(at).is_ok())
        // Between two letters of these scripts only: the rest are breaks any text has.
        .filter(|&at| {
            let before = text.get(..at).and_then(|t| t.chars().next_back());
            let after = text.get(at..).and_then(|t| t.chars().next());
            before.is_some_and(unspaced_char) && after.is_some_and(unspaced_char)
        })
        .collect()
}

/// `text` in the pieces a line may break between ([`breaks`]): the whole of it when there are none.
pub fn pieces(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    for &at in breaks(text).iter() {
        if let Some(piece) = text.get(from..at).filter(|p| !p.is_empty()) {
            out.push(piece);
            from = at;
        }
    }
    out.push(text.get(from..).unwrap_or(text));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thai_breaks_between_words_and_keeps_names_whole() {
        let join = |t: &str| pieces(t).join("|");
        assert_eq!(join("แผนที่แสดงพื้นที่เสี่ยงภัยน้ำท่วมในเขตอำเภอเมืองเชียงใหม่"), "แผนที่|แสดง|พื้นที่|เสี่ยง|ภัย|น้ำ|ท่วม|ใน|เขต|อำเภอ|เมือง|เชียงใหม่");
        // A compound the model cuts and a name the dictionary cuts are both whole.
        assert_eq!(join("กรุงเทพมหานคร"), "กรุงเทพมหานคร");
        assert_eq!(join("อุทยานแห่งชาติดอยอินทนนท์"), "อุทยาน|แห่ง|ชาติดอย|อินทนนท์");
        // No piece starts with a vowel or tone mark written over or under a letter.
        for piece in pieces("น้ำท่วมที่ดินผู้ใหญ่บ้านกิ่งอำเภอ") {
            assert!(!piece.starts_with(|c| matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}')), "{piece}");
        }
    }

    #[test]
    fn other_text_has_no_breaks_of_these() {
        assert!(breaks("Times Square–42nd Street").is_empty());
        assert!(breaks("").is_empty());
        // Spaces and brackets between Thai words are the caller's breaks.
        assert_eq!(pieces("บ้าน (หนอง)"), ["บ้าน (หนอง)"]);
        // Lao too; text too long to look at has none, and the same text is found the same again.
        assert!(!breaks("ຫຼວງພະບາງເມືອງມໍລະດົກໂລກ").is_empty());
        assert!(breaks(&"แผนที่".repeat(2000)).is_empty());
        assert_eq!(breaks("แผนที่แสดง"), breaks("แผนที่แสดง"));
        for junk in ["\u{0E48}\u{0E48}\u{0E48}", "เ", "ก\u{0}ข", "แ แ แ"] {
            let _ = pieces(junk);
        }
    }
}
