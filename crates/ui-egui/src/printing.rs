//! Print: a layout or the map view sent to one of the computer's printers.
//!
//! The Print dialog lists the printers ([`crate::printers`]), takes the copies and the size, and
//! sends the pages. The job goes on a thread of its own; [`land`] says what became of it. Open as
//! PDF stays for a computer whose printers can't be reached this way: it opens the page in the
//! system's PDF viewer, which prints it.

use std::sync::PoisonError;

use egui::vec2;
use rustgis_engine::Session;
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::printers::{self, Job, Printers, Problem, Sent};
use crate::theme::Tokens;

/// What is printed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum What {
    /// The layout at this index.
    Layout(usize),
    /// The map view.
    Map,
}

/// The Print dialog.
pub struct Form {
    what: What,
    /// The project it was opened in, and the layout's or map's name: another project, or another
    /// layout at that index, closes it.
    opened: u64,
    name: String,
    /// The printers, once listed; `listing` while they are looked for.
    printers: Option<Printers>,
    listing: Option<crate::Slot<Result<Printers, Problem>>>,
    printer: String,
    copies: u32,
    fit: bool,
    all_pages: bool,
    error: Option<String>,
}

/// A job on its way to the printer.
pub struct Sending {
    slot: crate::Slot<Result<Sent, Problem>>,
    name: String,
    printer: String,
    fit: bool,
    page_mm: (f64, f64),
    /// The resolution the pages' pictures were drawn at, when less than [`DPI`].
    lowered: Option<f64>,
}

/// What Print keeps between dialogs: the job under way and the printer last printed to.
#[derive(Default)]
pub struct State {
    pub sending: Option<Sending>,
    last_printer: Option<String>,
}

/// The resolution pages are drawn at.
const DPI: f64 = 300.0;
/// The most pixels a page's picture has: a larger page is drawn at a lower resolution.
const MAX_PIXELS: f64 = 64_000_000.0;

/// The resolution a page's picture is drawn at: [`DPI`], or what keeps it within [`MAX_PIXELS`].
fn picture_dpi((w, h): (f64, f64)) -> f64 {
    let square_inches = (w / 25.4) * (h / 25.4);
    if !square_inches.is_finite() || square_inches <= 0.0 {
        return DPI;
    }
    (MAX_PIXELS / square_inches).sqrt().clamp(36.0, DPI)
}

/// The name, page size in millimetres and page count (a map series') of what is printed, while
/// it is still there.
fn subject(s: &Session, what: What) -> Option<(String, (f64, f64), usize)> {
    match what {
        What::Layout(li) => {
            let l = s.project.layouts.get(li)?;
            let pages = rustgis_engine::cmd::layout::series_info(s, li, 0).map_or(1, |i| i.0).max(1);
            Some((l.name.clone(), (l.width, l.height), pages))
        }
        What::Map => Some((s.map().name.clone(), rustgis_engine::cmd::share::map_page_mm(s), 1)),
    }
}

/// A page's size in words: `297 × 210 mm (A4)`.
fn size_text((w, h): (f64, f64)) -> String {
    let mm = format!("{w:.0} × {h:.0} mm");
    match printers::paper_name((w, h)) {
        Some(paper) => format!("{mm} ({paper})"),
        None => mm,
    }
}

/// Opens the Print dialog on `what`.
pub fn open(app: &mut RustGisApp, what: What) -> Result<Value, String> {
    let (name, _, _) = subject(&app.session, what).ok_or("open a layout first")?;
    let form = Form {
        what,
        opened: app.session.opened,
        name,
        printers: None,
        listing: None,
        printer: app.print.last_printer.clone().unwrap_or_default(),
        copies: 1,
        // A layout's page is a paper size, so it prints at its own size; the map view's page is
        // the window's shape, which no paper has.
        fit: what == What::Map,
        all_pages: true,
        error: None,
    };
    app.dialogs = crate::dialogs::Dialogs { open: Some("print"), print: Some(form), ..Default::default() };
    Ok(Value::Null)
}

/// What went wrong, in the interface's language. On the frame: the language is the frame's.
fn words(problem: &Problem, printer: &str) -> String {
    match problem {
        Problem::Missing if cfg!(windows) => {
            tl!("Printing from here needs Windows PowerShell, which this computer doesn't have. Open as PDF prints from the PDF viewer.")
                .to_string()
        }
        Problem::Missing => {
            tl!("Printing from here needs CUPS (its lp program), which this computer doesn't have. Open as PDF prints from the PDF viewer.")
                .to_string()
        }
        Problem::NoPrinter => crate::i18n::fmt("This computer has no printer named {printer}", &[("printer", printer)]),
        Problem::TimedOut => tl!("The printer didn't answer in time").to_string(),
        Problem::Said(why) => crate::i18n::fmt("Printing failed: {error}", &[("error", why)]),
    }
}

/// Looks for the printers on a thread of its own; the frame is drawn again when they are found.
fn look(ctx: &egui::Context) -> crate::Slot<Result<Printers, Problem>> {
    let slot = crate::Slot::default();
    let (out, ctx) = (slot.clone(), ctx.clone());
    let spawned = std::thread::Builder::new().name("printers".into()).spawn(move || {
        let found = printers::list();
        *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
        ctx.request_repaint();
    });
    if let Err(e) = spawned {
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Err(Problem::Said(e.to_string())));
    }
    slot
}

impl Form {
    /// Takes the printers once they are found: the one last printed to stays chosen while the
    /// computer still has it, else its default, else the first.
    fn take_listing(&mut self) {
        let Some(slot) = &self.listing else { return };
        let Some(found) = slot.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
        self.listing = None;
        match found {
            Ok(printers) => {
                if !printers.names.contains(&self.printer) {
                    self.printer = printers.default.clone().or_else(|| printers.names.first().cloned()).unwrap_or_default();
                }
                self.printers = Some(printers);
            }
            Err(problem) => {
                self.printers = Some(Printers::default());
                self.error = Some(words(&problem, ""));
            }
        }
    }
}

/// Draws the pages into a new folder of their own, as [`printers::send`] takes them, and gives
/// the folder and how many pages it holds.
fn draw(app: &mut RustGisApp, what: What, all: bool, dpi: f64) -> Result<(std::path::PathBuf, usize), String> {
    let session = &mut app.session;
    let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<Vec<u8>>, String> {
        use rustgis_engine::cmd::{layout, share};
        match (what, printers::PICTURES) {
            (What::Layout(li), true) => layout::layout_pngs(session, li, dpi, all),
            (What::Layout(li), false) => layout::layout_bytes(session, li, true, dpi, all).map(|pdf| vec![pdf]),
            (What::Map, true) => share::map_page_png(session, dpi).map(|png| vec![png]),
            (What::Map, false) => share::map_pdf(session, dpi).map(|pdf| vec![pdf]),
        }
    }))
    .unwrap_or_else(|_| Err("printing failed unexpectedly".into()))?;
    if drawn.is_empty() {
        return Err("there is no page to print".into());
    }
    let dir = crate::print_job_dir()?;
    for (i, bytes) in drawn.iter().enumerate() {
        let name = if printers::PICTURES { printers::page_name(i + 1) } else { printers::PDF_NAME.to_string() };
        rustgis_engine::data::io::write_atomic(&dir.join(name), bytes).map_err(|e| e.to_string())?;
    }
    Ok((dir, drawn.len()))
}

/// Draws the pages and starts the job. One at a time.
fn print(app: &mut RustGisApp, f: &Form, ctx: &egui::Context) -> Result<(), String> {
    if app.print.sending.is_some() {
        return Err(tl!("A page is still on its way to the printer: wait for it.").to_string());
    }
    let (name, page_mm, _) = subject(&app.session, f.what).ok_or_else(|| tl!("The layout is no longer there").to_string())?;
    if f.printer.is_empty() {
        return Err(tl!("Choose a printer").to_string());
    }
    let dpi = if printers::PICTURES { picture_dpi(page_mm) } else { DPI };
    let (dir, pages) = draw(app, f.what, f.all_pages, dpi)?;
    let job = Job {
        printer: f.printer.clone(),
        title: name.clone(),
        copies: f.copies.clamp(1, printers::MAX_COPIES),
        fit: f.fit,
        page_mm,
        to_file: None,
    };
    let slot = crate::Slot::default();
    let (out, ctx) = (slot.clone(), ctx.clone());
    let sending = Sending {
        slot,
        name,
        printer: job.printer.clone(),
        fit: job.fit,
        page_mm,
        lowered: (printers::PICTURES && dpi < DPI - 0.5).then_some(dpi),
    };
    std::thread::Builder::new()
        .name("print".into())
        .spawn(move || {
            let sent = printers::send(&job, &dir, pages);
            // The spooler has its copy; the folder goes whether or not it took the job.
            let _ = std::fs::remove_dir_all(&dir);
            *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(sent);
            ctx.request_repaint();
        })
        .map_err(|e| e.to_string())?;
    app.print.last_printer = Some(sending.printer.clone());
    app.print.sending = Some(sending);
    Ok(())
}

/// Says what became of the job under way once the printer's queue has it (or why it hasn't):
/// called every frame. `true` while one is still on its way.
pub fn land(app: &mut RustGisApp) -> bool {
    let Some(sending) = &app.print.sending else { return false };
    let Some(done) = sending.slot.lock().unwrap_or_else(PoisonError::into_inner).take() else { return true };
    let Some(sending) = app.print.sending.take() else { return false };
    let showing = app.dialogs.open == Some("print");
    match done {
        Ok(sent) => {
            app.session
                .notify(crate::i18n::fmt("Sent {name} to {printer}", &[("name", &sending.name), ("printer", &sending.printer)]), false);
            // At its own size on another paper, part of the page can be off the paper.
            if let Some(paper) = sent.other_paper.filter(|_| !sending.fit) {
                app.session.notify(
                    crate::i18n::fmt(
                        "{printer} has no paper of the page's size ({size}): the page was printed at its own size on {paper}, so part of it may be cut off. Fit to paper prints the whole page.",
                        &[("printer", &sending.printer), ("size", &size_text(sending.page_mm)), ("paper", &paper)],
                    ),
                    false,
                );
            }
            if let Some(dpi) = sending.lowered {
                app.session.notify(
                    crate::i18n::fmt(
                        "The page is large ({size}), so it was printed at {dpi} dpi rather than 300",
                        &[("size", &size_text(sending.page_mm)), ("dpi", &format!("{dpi:.0}"))],
                    ),
                    false,
                );
            }
            if showing {
                app.dialogs.open = None;
            }
        }
        Err(problem) => {
            let text = words(&problem, &sending.printer);
            app.session.notify(text.clone(), true);
            // The dialog stays, with the reason and Open as PDF.
            if showing && let Some(f) = &mut app.dialogs.print {
                f.error = Some(text);
            }
        }
    }
    false
}

/// The job under way in the status bar.
pub fn status(app: &RustGisApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.print.sending.is_none() {
        return;
    }
    ui.spinner();
    ui.label(egui::RichText::new(tl!("Sending to the printer…")).size(11.5).color(t.text_dim));
}

/// The Print dialog.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut f) = app.dialogs.print.take() else {
        app.dialogs.open = None;
        return;
    };
    // Another project, or another layout or map where this one was, closes it.
    let Some((name, page_mm, pages)) = subject(&app.session, f.what).filter(|(name, ..)| app.session.opened == f.opened && *name == f.name)
    else {
        app.dialogs.open = None;
        return;
    };
    if f.printers.is_none() && f.listing.is_none() {
        f.listing = Some(look(ctx));
    }
    f.take_listing();
    let sending = app.print.sending.is_some();
    let t = Tokens::get(ctx);
    let (mut open, mut close) = (true, false);
    let (mut print_now, mut as_pdf) = (false, false);
    let title = if f.what == What::Map { tl!("Print Map") } else { tl!("Print") };
    let mut refresh = false;
    egui::Window::new(title)
        .id(egui::Id::unique("print"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center() + vec2(0.0, -40.0))
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.label(egui::RichText::new(&name).strong());
            egui::Grid::new("print_options").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                let printer_label = ui.label(tl!("Printer"));
                ui.horizontal(|ui| match &f.printers {
                    None => {
                        ui.spinner();
                        ui.label(tl!("Looking for printers…"));
                    }
                    Some(found) => {
                        if found.names.is_empty() {
                            ui.label(egui::RichText::new(tl!("No printer found")).color(t.text_dim));
                        } else {
                            egui::ComboBox::from_id_salt("print_printer")
                                .selected_text(f.printer.as_str())
                                .width(260.0)
                                .show_ui(ui, |ui| {
                                    for name in &found.names {
                                        ui.selectable_value(&mut f.printer, name.clone(), name);
                                    }
                                })
                                .response
                                .labelled_by(printer_label.id);
                        }
                        refresh = ui.button(tl!("Refresh")).on_hover_text(tl!("Look for the computer's printers again.")).clicked();
                    }
                });
                ui.end_row();
                let copies_label = ui.label(tl!("Copies"));
                ui.add(egui::DragValue::new(&mut f.copies).range(1..=printers::MAX_COPIES).speed(0.1)).labelled_by(copies_label.id);
                ui.end_row();
                ui.label(tl!("Size"));
                ui.vertical(|ui| {
                    ui.radio_value(&mut f.fit, false, tl!("Actual size")).on_hover_text(tl!(
                        "At the page's own size, so the map keeps its scale. What falls in the printer's margins isn't printed."
                    ));
                    ui.radio_value(&mut f.fit, true, tl!("Fit to paper"))
                        .on_hover_text(tl!("The whole page, made smaller or larger to fit the paper: the map's scale changes with it."));
                });
                ui.end_row();
                if pages > 1 {
                    ui.label(tl!("Pages"));
                    ui.vertical(|ui| {
                        ui.radio_value(&mut f.all_pages, true, crate::i18n::fmt("All {n} pages", &[("n", &pages.to_string())]));
                        ui.radio_value(&mut f.all_pages, false, tl!("This page"));
                    });
                    ui.end_row();
                }
                ui.label(tl!("Page"));
                ui.label(size_text(page_mm));
                ui.end_row();
            });
            let note = if f.fit {
                tl!("The page is sized to fit the printer's paper, so the map's scale changes.")
            } else if printers::paper_name(page_mm).is_some() {
                tl!("Put paper of the page's size in the printer: the page is printed at its own size, and the map keeps its scale.")
            } else {
                tl!("The page isn't a standard paper size. At its own size, what doesn't fit the printer's paper is cut off.")
            };
            ui.label(egui::RichText::new(note).small().color(t.text_dim));
            if let Some(error) = &f.error {
                ui.add(egui::Label::new(egui::RichText::new(error).color(t.red)).wrap().selectable(true));
            }
            if sending {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Sending to the printer…"));
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button(tl!("Open as PDF"))
                    .on_hover_text(tl!("Open the page in the system's PDF viewer and print from there, as before."))
                    .clicked()
                {
                    as_pdf = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    let ready = !sending && f.printers.as_ref().is_some_and(|p| p.names.contains(&f.printer));
                    print_now = ui
                        .add_enabled_ui(ready, |ui| {
                            crate::widgets::dialog_button(ui, tl!("Print"), Some(t.accent), vec2(84.0, 28.0)).clicked()
                        })
                        .inner;
                    close = crate::widgets::dialog_button(ui, tl!("Cancel"), None, vec2(84.0, 28.0)).clicked();
                });
            });
        });
    if refresh {
        f.printers = None;
        f.error = None;
    }
    if print_now {
        f.error = print(app, &f, ctx).err();
        if let Some(error) = &f.error {
            app.session.notify(error.clone(), true);
        }
    }
    if as_pdf {
        let command = if f.what == What::Map { "ui.printMap" } else { "ui.printLayout" };
        // The layout the dialog is on, whichever is open now, and the pages chosen in it.
        let params = match f.what {
            What::Layout(li) => json!({"viewer": true, "layout": li, "all": f.all_pages}),
            What::Map => json!({"viewer": true}),
        };
        match app.run(command, params) {
            Ok(_) => close = true,
            Err(e) => f.error = Some(e),
        }
    }
    if !open || close {
        app.dialogs.open = None;
    } else if app.dialogs.open == Some("print") {
        app.dialogs.print = Some(f);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn layout_app() -> RustGisApp {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"page": "a4", "template": "map", "name": "Flood map"})).unwrap();
        app
    }

    fn sending(app: &mut RustGisApp, done: Result<Sent, Problem>, fit: bool, lowered: Option<f64>) {
        let slot = crate::Slot::default();
        *slot.lock().unwrap() = Some(done);
        app.print.sending =
            Some(Sending { slot, name: "Flood map".into(), printer: "Office".into(), fit, page_mm: (297.0, 210.0), lowered });
    }

    fn said(app: &RustGisApp) -> Vec<(String, bool)> {
        app.session.notices.iter().map(|n| (n.text.clone(), n.error)).collect()
    }

    #[test]
    fn print_opens_the_dialog_on_the_layout_or_the_map() {
        let mut app = layout_app();
        app.run("ui.printLayout", Value::Null).unwrap();
        assert_eq!(app.dialogs.open, Some("print"));
        let f = app.dialogs.print.as_ref().unwrap();
        // A layout prints at its own size, the map view fitted to the paper.
        assert_eq!((f.what, f.name.as_str(), f.fit, f.copies), (What::Layout(0), "Flood map", false, 1));
        assert_eq!(subject(&app.session, f.what).unwrap().1, (297.0, 210.0));
        app.run("ui.printMap", Value::Null).unwrap();
        let f = app.dialogs.print.as_ref().unwrap();
        assert_eq!((f.what, f.fit), (What::Map, true));
        // The printer last printed to is the one the next dialog opens on.
        app.print.last_printer = Some("Plotter".into());
        app.run("ui.printMap", Value::Null).unwrap();
        assert_eq!(app.dialogs.print.as_ref().unwrap().printer, "Plotter");
        // No layout open: nothing to print.
        let mut app = RustGisApp::with_sample();
        assert!(app.run("ui.printLayout", Value::Null).is_err());
        assert_eq!(app.dialogs.open, None);
    }

    #[test]
    fn the_printers_found_choose_the_one_to_print_to() {
        let mut app = layout_app();
        open(&mut app, What::Layout(0)).unwrap();
        let mut f = app.dialogs.print.take().unwrap();
        let found = |names: &[&str], default: Option<&str>| {
            let slot = crate::Slot::default();
            *slot.lock().unwrap() =
                Some(Ok(Printers { names: names.iter().map(|n| n.to_string()).collect(), default: default.map(str::to_string) }));
            slot
        };
        // The default, when nothing was chosen before.
        f.listing = Some(found(&["Office", "Plotter"], Some("Plotter")));
        f.take_listing();
        assert_eq!((f.printer.as_str(), f.listing.is_none()), ("Plotter", true));
        // The one chosen stays while the computer has it; gone, the first takes its place.
        f.printer = "Office".into();
        f.listing = Some(found(&["Office", "Plotter"], Some("Plotter")));
        f.take_listing();
        assert_eq!(f.printer, "Office");
        f.listing = Some(found(&["Plotter", "Other"], None));
        f.take_listing();
        assert_eq!(f.printer, "Plotter");
        // A computer that can't list them says why, and has none to choose.
        let slot = crate::Slot::default();
        *slot.lock().unwrap() = Some(Err(Problem::Missing));
        f.listing = Some(slot);
        f.take_listing();
        assert!(f.printers.as_ref().unwrap().names.is_empty());
        assert!(f.error.as_ref().unwrap().contains("Open as PDF"), "{:?}", f.error);
    }

    #[test]
    fn a_job_sent_says_so_and_closes_the_dialog() {
        let mut app = layout_app();
        open(&mut app, What::Layout(0)).unwrap();
        sending(&mut app, Ok(Sent::default()), false, None);
        assert!(!land(&mut app));
        assert_eq!(said(&app).last().unwrap(), &("Sent Flood map to Office".to_string(), false));
        assert_eq!(app.dialogs.open, None);
        assert!(app.print.sending.is_none());
        // Nothing under way: nothing said.
        let before = said(&app).len();
        assert!(!land(&mut app));
        assert_eq!(said(&app).len(), before);
    }

    #[test]
    fn a_job_still_on_its_way_is_waited_for() {
        let mut app = layout_app();
        app.print.sending = Some(Sending {
            slot: crate::Slot::default(),
            name: "Flood map".into(),
            printer: "Office".into(),
            fit: false,
            page_mm: (297.0, 210.0),
            lowered: None,
        });
        let before = said(&app).len();
        assert!(land(&mut app));
        assert!(app.print.sending.is_some());
        assert_eq!(said(&app).len(), before);
        // A second job waits for the first.
        open(&mut app, What::Layout(0)).unwrap();
        let mut f = app.dialogs.print.take().unwrap();
        f.printer = "Office".into();
        let e = print(&mut app, &f, &egui::Context::default()).unwrap_err();
        assert!(e.contains("still on its way"), "{e}");
    }

    #[test]
    fn a_page_on_another_paper_or_at_a_lower_resolution_is_said() {
        let mut app = layout_app();
        // At its own size on a paper of another size: part may be cut off.
        sending(&mut app, Ok(Sent { other_paper: Some("Letter".into()) }), false, None);
        land(&mut app);
        let notices = said(&app);
        assert!(
            notices.iter().any(|(text, error)| !error
                && text.contains("Office has no paper of the page's size (297 × 210 mm (A4))")
                && text.contains("on Letter")),
            "{notices:?}"
        );
        // Fitted to the paper, nothing is cut off, so nothing more is said.
        let mut app = layout_app();
        sending(&mut app, Ok(Sent { other_paper: Some("Letter".into()) }), true, None);
        land(&mut app);
        assert!(!said(&app).iter().any(|(text, _)| text.contains("no paper")), "{:?}", said(&app));
        // A page too large for 300 dpi says the resolution it went at.
        let mut app = layout_app();
        sending(&mut app, Ok(Sent::default()), false, Some(196.4));
        land(&mut app);
        assert!(said(&app).iter().any(|(text, _)| text.contains("printed at 196 dpi")), "{:?}", said(&app));
    }

    #[test]
    fn a_job_that_failed_says_why_and_keeps_the_dialog() {
        for (problem, want) in [
            (Problem::NoPrinter, "This computer has no printer named Office"),
            (Problem::TimedOut, "The printer didn't answer in time"),
            (Problem::Said("The RPC server is unavailable".into()), "Printing failed: The RPC server is unavailable"),
            (Problem::Missing, "Open as PDF prints from the PDF viewer"),
        ] {
            let mut app = layout_app();
            open(&mut app, What::Layout(0)).unwrap();
            sending(&mut app, Err(problem), false, None);
            assert!(!land(&mut app));
            let (text, error) = said(&app).last().cloned().unwrap();
            assert!(error && text.contains(want), "{text}");
            assert_eq!(app.dialogs.open, Some("print"));
            assert_eq!(app.dialogs.print.as_ref().unwrap().error.as_deref(), Some(text.as_str()));
        }
    }

    #[test]
    fn the_pages_are_drawn_as_the_printer_takes_them() {
        let mut app = layout_app();
        let (dir, pages) = draw(&mut app, What::Layout(0), true, 72.0).unwrap();
        assert_eq!(pages, 1);
        let (name, magic): (String, &[u8]) =
            if printers::PICTURES { (printers::page_name(1), b"\x89PNG") } else { (printers::PDF_NAME.into(), b"%PDF") };
        assert!(std::fs::read(dir.join(&name)).unwrap().starts_with(magic), "{name}");
        let (dir, pages) = draw(&mut app, What::Map, true, 72.0).unwrap();
        assert_eq!(pages, 1);
        assert!(std::fs::read(dir.join(&name)).unwrap().starts_with(magic), "{name}");
        let _ = std::fs::remove_dir_all(dir);
        // A layout that is gone is an error, not a crash.
        assert!(draw(&mut app, What::Layout(7), true, 72.0).is_err());
    }

    #[test]
    fn a_page_opened_in_the_viewer_leaves_a_job_s_folder() {
        // Two kinds of folder, as the viewer's pages and a printer's jobs are: a new one of a
        // kind removes the earlier ones of that kind and no other.
        let job = crate::private_dir("rustgis-testjob").unwrap();
        std::fs::write(job.join(printers::page_name(1)), b"page").unwrap();
        let first = crate::private_dir("rustgis-test").unwrap();
        let second = crate::private_dir("rustgis-test").unwrap();
        assert!(!first.exists() && second.is_dir());
        assert!(job.join(printers::page_name(1)).is_file(), "the job's page went with the viewer's folder");
        let _ = std::fs::remove_dir_all(&job);
        let _ = std::fs::remove_dir_all(&second);
        // The viewer's own folder is of another kind than a job's.
        let viewer = crate::print_dir().unwrap();
        let name = |dir: &std::path::Path| dir.file_name().unwrap().to_string_lossy().into_owned();
        let pid = std::process::id();
        assert!(name(&viewer).starts_with(&format!("rustgis-print-{pid}-")), "{}", name(&viewer));
        assert!(!format!("rustgis-printjob-{pid}-1").starts_with(&format!("rustgis-print-{pid}-")));
        let _ = std::fs::remove_dir_all(&viewer);
    }

    #[test]
    fn the_viewer_s_pdf_has_the_pages_chosen() {
        use std::sync::{Arc, Mutex};
        // The web app's download stands in for the viewer: the same pages go to both.
        let got: Arc<Mutex<Vec<u8>>> = Arc::default();
        let to = got.clone();
        let mut app = layout_app().with_services(crate::Services {
            download: Some(Box::new(move |_, bytes| {
                *to.lock().unwrap() = bytes;
                Ok(())
            })),
            ..Default::default()
        });
        let pages = app.run("layout.series", json!({"layer": "Lakes", "nameField": "name"})).unwrap()["pages"].as_u64().unwrap();
        assert!(pages > 1);
        let count = |app: &mut RustGisApp, params: Value| {
            app.run("ui.printLayout", params).unwrap();
            let pdf = String::from_utf8_lossy(&got.lock().unwrap()).into_owned();
            (1..=pages).find(|n| pdf.contains(&format!("/Count {n} "))).unwrap()
        };
        assert_eq!(count(&mut app, json!({"viewer": true, "layout": 0, "all": false})), 1);
        assert_eq!(count(&mut app, json!({"viewer": true, "layout": 0, "all": true})), pages);
        // Print without the dialog's choice is every page, as it always was.
        assert_eq!(count(&mut app, json!({"viewer": true})), pages);
    }

    #[test]
    fn a_large_page_s_picture_is_drawn_at_a_lower_resolution() {
        assert_eq!(picture_dpi((297.0, 210.0)), 300.0);
        assert_eq!(picture_dpi((420.0, 297.0)), 300.0);
        // A0: 46.8 × 33.1 in. At 300 dpi it would be 139 million pixels.
        let a0 = picture_dpi((1189.0, 841.0));
        assert!((190.0..210.0).contains(&a0), "{a0}");
        assert!((1189.0 / 25.4 * a0) * (841.0 / 25.4 * a0) <= MAX_PIXELS * 1.001);
        assert_eq!(picture_dpi((f64::NAN, 100.0)), 300.0);
        assert_eq!(picture_dpi((0.0, 100.0)), 300.0);
        assert_eq!(size_text((297.0, 210.0)), "297 × 210 mm (A4)");
        assert_eq!(size_text((344.2, 185.4)), "344 × 185 mm");
    }

    #[test]
    fn the_dialog_closes_when_its_layout_is_gone() {
        let mut app = layout_app();
        open(&mut app, What::Layout(0)).unwrap();
        // Another project in its place.
        app.run("project.new", Value::Null).unwrap();
        assert!(subject(&app.session, What::Layout(0)).is_none());
        let mut out = egui::Context::default().run_ui(Default::default(), |ui| show(&mut app, ui.ctx()));
        out.textures_delta.clear();
        assert_eq!(app.dialogs.open, None);
    }
}
