//! Thai Map Sheets (Home › Navigate › Locate): the sheet numbers at the map's centre, going to a
//! sheet by its number, and a grid of sheets where the map is.

use egui::vec2;
use rustgis_engine::geom::Coord;
use rustgis_engine::map::thai_sheets::{self as ts, Datum, Scale};
use serde_json::json;

use crate::RustGisApp;

/// The datum picked, kept in the dialog's mode: `wgs84` or `indian1975`.
fn datum(app: &mut RustGisApp) -> Datum {
    if !matches!(app.dialogs.mode, "wgs84" | "indian1975") {
        app.dialogs.mode = "wgs84";
    }
    Datum::parse(app.dialogs.mode).unwrap_or(Datum::Wgs84)
}

/// A scale code kept in a dialog field (`c`: a part's scale; `d`: the grid's).
fn pick(value: &mut String, default: &str) {
    if !matches!(value.as_str(), "50000" | "4000" | "2000" | "1000" | "500") {
        *value = default.to_string();
    }
}

fn scale_name(code: &str) -> &'static str {
    match code {
        "50000" => "1:50,000 (RTSD topographic)",
        "4000" => "1:4,000 (Department of Lands)",
        "2000" => "1:2,000 (Department of Lands)",
        "500" => "1:500 (Department of Lands)",
        _ => "1:1,000 (Department of Lands)",
    }
}

/// The dialog. Returns whether it should close.
pub fn thai_sheets(app: &mut RustGisApp, ui: &mut egui::Ui) -> bool {
    let d = datum(app);
    pick(&mut app.dialogs.c, "1000");
    pick(&mut app.dialogs.d, "4000");
    ui.label(
        egui::RichText::new(tl!(
            "Sheet numbers of the Royal Thai Survey Department's 1:50,000 maps and the Department of Lands' UTM cadastral maps, worked out from where a point is."
        ))
        .weak(),
    );
    egui::Grid::new("thai_sheet_datum").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Datum"));
        let text = |d: Datum| match d {
            Datum::Wgs84 => tl!("WGS 84 (series L7018, today's cadastral sheets)"),
            Datum::Indian1975 => tl!("Indian 1975 (series L7017, older cadastral sheets)"),
        };
        egui::ComboBox::from_id_salt("thai_sheet_datum_pick")
            .selected_text(text(d))
            .width(330.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.dialogs.mode, "wgs84", text(Datum::Wgs84));
                ui.selectable_value(&mut app.dialogs.mode, "indian1975", text(Datum::Indian1975));
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
    });
    let d = datum(app);

    ui.add_space(6.0);
    ui.strong(tl!("At the map's centre"));
    let e = rustgis_engine::cmd::workspace::view_extent(&app.session);
    let centre = (!e.is_empty()).then(|| Coord::new((e.min_x + e.max_x) / 2.0, (e.min_y + e.max_y) / 2.0));
    match centre.and_then(|c| Some((c, ts::to_datum(c, d)?))) {
        Some((c, on)) => {
            let fifty = ts::sheet_50k(on);
            let zone = ts::zone_of(c.x);
            egui::Grid::new("thai_sheet_here").num_columns(2).spacing(vec2(14.0, 4.0)).striped(true).show(ui, |ui| {
                ui.label(format!("1:50,000 ({})", d.series()));
                let mut text = fifty.label();
                if !fifty.in_series {
                    text = format!("{text} · {}", tl!("no printed sheet here"));
                }
                ui.add(egui::Label::new(egui::RichText::new(text).strong()).selectable(true));
                ui.end_row();
                for scale in Scale::ALL {
                    ui.label(scale.label());
                    let label = ts::utm_sheet(c, d, scale, Some(zone)).map(|u| u.label()).unwrap_or_default();
                    ui.add(egui::Label::new(egui::RichText::new(label).strong()).selectable(true));
                    ui.end_row();
                }
                if let Some(m) = ts::to_utm(c, d, zone) {
                    ui.label("UTM");
                    let fmt = |v: f64| {
                        let s = format!("{:.0}", v);
                        let mut out = String::new();
                        for (i, ch) in s.chars().enumerate() {
                            if i > 0 && (s.len() - i) % 3 == 0 {
                                out.push(',');
                            }
                            out.push(ch);
                        }
                        out
                    };
                    ui.add(egui::Label::new(format!("{} {zone}N · E {} · N {}", tl!("Zone"), fmt(m.x), fmt(m.y))).selectable(true));
                    ui.end_row();
                }
            });
        }
        None => {
            ui.label(egui::RichText::new(tl!("The map's extent isn't known yet.")).weak());
        }
    }

    ui.add_space(8.0);
    ui.strong(tl!("Go to a sheet"));
    let mut go = false;
    ui.horizontal(|ui| {
        let r =
            ui.add(egui::TextEdit::singleline(&mut app.dialogs.a).hint_text("5541 I · 5137 IV 6888 · 5137 IV 6888-5").desired_width(250.0));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            go = true;
        }
        egui::ComboBox::from_id_salt("thai_sheet_part_scale")
            .selected_text(tl!(match app.dialogs.c.as_str() {
                "2000" => "Parts at 1:2,000",
                "500" => "Parts at 1:500",
                _ => "Parts at 1:1,000",
            }))
            .width(130.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.dialogs.c, "2000".to_string(), tl!("Parts at 1:2,000"));
                ui.selectable_value(&mut app.dialogs.c, "1000".to_string(), tl!("Parts at 1:1,000"));
                ui.selectable_value(&mut app.dialogs.c, "500".to_string(), tl!("Parts at 1:500"));
            });
        if ui.button(tl!("Go")).clicked() {
            go = true;
        }
    });
    if go && !app.dialogs.a.trim().is_empty() {
        let p = json!({"sheet": app.dialogs.a.trim(), "scale": app.dialogs.c, "datum": d.code()});
        match app.run("map.goToThaiSheet", p) {
            Ok(_) => {
                app.dialogs.error = None;
                app.map.invalidate();
            }
            Err(e) => app.dialogs.error = Some(e),
        }
    }

    ui.add_space(8.0);
    ui.strong(tl!("Sheet grid"));
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("thai_sheet_grid_scale").selected_text(tl!(scale_name(&app.dialogs.d))).width(250.0).show_ui(
            ui,
            |ui| {
                for code in ["50000", "4000", "2000", "1000", "500"] {
                    ui.selectable_value(&mut app.dialogs.d, code.to_string(), tl!(scale_name(code)));
                }
            },
        );
        if ui.button(tl!("Add Where the Map Is")).clicked() {
            match app.run("layer.thaiSheetGrid", json!({"series": app.dialogs.d, "datum": d.code()})) {
                Ok(v) => {
                    app.dialogs.error = None;
                    app.dialogs.info = Some(crate::i18n::fmt("Added {n} sheets", &[("n", &v["sheets"].to_string())]));
                    app.map.invalidate();
                }
                Err(e) => {
                    app.dialogs.info = None;
                    app.dialogs.error = Some(e);
                }
            }
        }
    });
    ui.label(
        egui::RichText::new(tl!(
            "The grid is a layer of outlines labelled with their numbers: style it, or add it to a layout like any layer."
        ))
        .weak(),
    );
    ui.add_space(4.0);
    ui.button(tl!("Close")).clicked()
}
