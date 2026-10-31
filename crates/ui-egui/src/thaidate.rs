//! Dates as a Thai reader reads them, while the interface is in Thai: the day, the Thai month and
//! the Buddhist Era year (31 ต.ค. 2569). Only what is shown changes: data, edits, searches,
//! files and the time slider's own format are left as they are.

use rustgis_engine::data::time;
use rustgis_engine::data::time::Unit;
use rustgis_engine::map::time::TimeSlider;

/// Whether the interface is in Thai.
fn thai() -> bool {
    crate::i18n::current().code() == "th"
}

/// The Thai format that shows a time slider's steps apart (as [`Unit::format`] does in English).
fn unit_format(unit: Unit) -> &'static str {
    match unit {
        Unit::Hour => "D ดดด BBBB HH:mm",
        Unit::Day => "D ดดด BBBB",
        Unit::Month => "ดดดด BBBB",
        Unit::Year => "BBBB",
    }
}

/// A time slider's date as shown: in Thai while the interface is, unless the slider has a format
/// of its own.
pub(crate) fn slider_label(t: &TimeSlider, ms: f64) -> String {
    if thai() && t.format.trim().is_empty() { time::format(ms, unit_format(t.unit)) } else { t.label(ms) }
}

/// A table cell's text shown in Thai while the interface is, when it is an ISO date
/// (`2026-10-31`, or with a time: `2026-10-31T14:05:00Z`); `None` for anything else.
pub(crate) fn cell(text: &str) -> Option<String> {
    thai().then(|| thai_date(text)).flatten()
}

/// An ISO date or date and time in Thai: `31 ต.ค. 2569`, `31 ต.ค. 2569 14:05`.
fn thai_date(text: &str) -> Option<String> {
    let s = text.trim();
    let b = s.as_bytes();
    // `YYYY-MM-DD` exactly at the start: a number or a free-text field is left alone.
    let iso = b.len() >= 10
        && b.get(..4).is_some_and(|y| y.iter().all(u8::is_ascii_digit))
        && b.get(4) == Some(&b'-')
        && b.get(5..7).is_some_and(|m| m.iter().all(u8::is_ascii_digit))
        && b.get(7) == Some(&b'-')
        && b.get(8..10).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    if !iso || !(b.len() == 10 || matches!(b.get(10), Some(b'T' | b't' | b' '))) {
        return None;
    }
    let t = time::parse_text(s)?;
    // A time of day is shown only when the value has one other than midnight.
    let has_time = b.len() > 10 && time::format(t, "HH:mm") != "00:00";
    Some(time::format(t, if has_time { "D ดดด BBBB HH:mm" } else { "D ดดด BBBB" }))
}

#[cfg(test)]
mod tests {
    use super::thai_date;

    #[test]
    fn iso_dates_read_in_thai() {
        assert_eq!(thai_date("2026-10-31").as_deref(), Some("31 ต.ค. 2569"));
        assert_eq!(thai_date("2026-10-31T14:05:00Z").as_deref(), Some("31 ต.ค. 2569 14:05"));
        assert_eq!(thai_date("2026-10-31 00:00:00").as_deref(), Some("31 ต.ค. 2569"));
        // Not dates, or not ISO ones: left alone.
        for s in ["2026", "20261031", "31/10/2026", "2026-13-01", "2026-10-31abc", "Bangkok", ""] {
            assert_eq!(thai_date(s), None, "{s}");
        }
    }
}
