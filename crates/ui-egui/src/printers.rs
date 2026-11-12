//! The computer's printers: which there are, and sending pages to one.
//!
//! macOS and Linux print through CUPS (`lpstat`, `lp`), which takes a PDF as it is. Windows has no
//! program that prints a PDF, so there the pages go as pictures to Windows PowerShell, which
//! prints them with .NET (`System.Drawing.Printing`). Both are programs the system ships: nothing
//! here calls a system library, which would need `unsafe`.
//!
//! Everything runs off the frame (a listing can wait on the network, a job on the spooler) and
//! says what went wrong as a [`Problem`], which the frame puts into words in its language.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;

/// Whether [`send`] takes the pages as PNG pictures (`page-0001.png`, …) rather than one PDF.
pub const PICTURES: bool = cfg!(windows);

/// The longest a listing may take: CUPS can wait on printers it finds over the network.
const LIST_LIMIT: Duration = Duration::from_secs(30);
/// The longest handing a job to the spooler may take (a large page's picture is slow to draw).
const SEND_LIMIT: Duration = Duration::from_secs(600);

/// The printers this computer has, and which one it prints to unless told otherwise.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Printers {
    pub names: Vec<String>,
    pub default: Option<String>,
}

/// A job for a printer.
#[derive(Clone, Debug)]
pub struct Job {
    pub printer: String,
    /// The job's name in the printer's queue.
    pub title: String,
    pub copies: u32,
    /// Sized to fit the paper; otherwise at the page's own size.
    pub fit: bool,
    /// The page's width and height in millimetres.
    pub page_mm: (f64, f64),
    /// Windows: the file the printer writes instead of printing (Microsoft Print to PDF), which
    /// is how the tests print without a printer.
    pub to_file: Option<PathBuf>,
}

/// What became of a job that was sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sent {
    /// The paper the pages went on, when the printer has none of the page's size (Windows, where
    /// the printer's papers are known).
    pub other_paper: Option<String>,
}

/// Why a listing or a job failed.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// The computer lacks the programs printing goes through.
    Missing,
    /// It has no printer of the job's name.
    NoPrinter,
    /// The step ran past its time and was stopped.
    TimedOut,
    /// What the system said of it.
    Said(String),
}

/// The computer's printers.
pub fn list() -> Result<Printers, Problem> {
    if cfg!(windows) { windows_list(powershell()) } else { Cups::default().list() }
}

/// Sends the pages in `dir` to the job's printer: `pages` pictures where [`PICTURES`], else
/// `document.pdf`.
pub fn send(job: &Job, dir: &Path, pages: usize) -> Result<Sent, Problem> {
    if cfg!(windows) { windows_send(powershell(), job, dir, pages) } else { Cups::default().send(job, &dir.join(PDF_NAME)) }
}

/// The PDF [`send`] prints, in the job's folder.
pub const PDF_NAME: &str = "document.pdf";

/// The name of page `n` (from 1) of the pictures [`send`] prints, in the job's folder.
pub fn page_name(n: usize) -> String {
    format!("page-{n:04}.png")
}

/// Runs `c` to its end, or stops it after `limit`. No console window on Windows.
fn run(mut c: Command, limit: Duration) -> Result<Output, Problem> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Problem::Missing),
        Err(e) => return Err(Problem::Said(e.to_string())),
    };
    // Both pipes are read while it runs: a full pipe would hold it until the time limit.
    fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    }
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Problem::Said("the program's output could not be read".into()));
    };
    let (out, err) = (drain(out), drain(err));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Problem::TimedOut);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Problem::Said(e.to_string()));
            }
        }
    };
    Ok(Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
}

/// The first line a program wrote that says something, for an error: its standard error's, else
/// its output's, else how it ended.
fn said(out: &Output) -> Problem {
    let line = |bytes: &[u8]| lines(bytes).into_iter().next();
    Problem::Said(line(&out.stderr).or_else(|| line(&out.stdout)).unwrap_or_else(|| out.status.to_string()))
}

// --- CUPS (macOS, Linux) -----------------------------------------------------------------------

/// CUPS's programs. The tests put scripts in their place.
struct Cups {
    lp: PathBuf,
    lpstat: PathBuf,
}

impl Default for Cups {
    fn default() -> Self {
        Self { lp: "lp".into(), lpstat: "lpstat".into() }
    }
}

impl Cups {
    /// One of its programs, answering in English whatever the system's language: the answers are
    /// read.
    fn program(path: &Path) -> Command {
        let mut c = Command::new(path);
        c.env("LC_ALL", "C");
        c
    }

    fn list(&self) -> Result<Printers, Problem> {
        // Every destination, those found over the network too; an older CUPS has only `-a`.
        let mut every = Self::program(&self.lpstat);
        every.arg("-e");
        let out = run(every, LIST_LIMIT)?;
        let names = if out.status.success() {
            lines(&out.stdout)
        } else {
            let mut accepting = Self::program(&self.lpstat);
            accepting.arg("-a");
            let older = run(accepting, LIST_LIMIT)?;
            if older.status.success() {
                lines(&older.stdout).iter().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect()
            } else if String::from_utf8_lossy(&older.stderr).contains("No destinations") {
                Vec::new()
            } else {
                return Err(said(&older));
            }
        };
        let mut default = Self::program(&self.lpstat);
        default.arg("-d");
        let default = run(default, LIST_LIMIT).ok().filter(|o| o.status.success()).and_then(|o| default_destination(&o.stdout));
        Ok(printers(names, default))
    }

    fn send(&self, job: &Job, pdf: &Path) -> Result<Sent, Problem> {
        // A name that reads as an option is no printer's.
        if job.printer.is_empty() || job.printer.starts_with('-') {
            return Err(Problem::NoPrinter);
        }
        let mut c = Self::program(&self.lp);
        c.args(lp_args(job)).arg("--").arg(pdf);
        let out = run(c, SEND_LIMIT)?;
        if out.status.success() {
            return Ok(Sent::default());
        }
        let why = String::from_utf8_lossy(&out.stderr);
        if why.contains("does not exist") || why.contains("Unknown destination") { Err(Problem::NoPrinter) } else { Err(said(&out)) }
    }
}

/// The lines of a program's output that say something.
fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// The names in order, each once, with the default kept only when it is one of them.
fn printers(mut names: Vec<String>, default: Option<String>) -> Printers {
    let mut seen = std::collections::HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    let default = default.filter(|d| names.contains(d));
    Printers { names, default }
}

/// The printer `lpstat -d` names: `system default destination: NAME`.
fn default_destination(out: &[u8]) -> Option<String> {
    let line = lines(out).into_iter().find(|l| l.starts_with("system default destination"))?;
    let name = line.split_once(':')?.1.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// `lp`'s options for a job, before the file.
fn lp_args(job: &Job) -> Vec<String> {
    let mut args = vec![
        "-d".to_string(),
        job.printer.clone(),
        "-n".into(),
        job.copies.clamp(1, MAX_COPIES).to_string(),
        "-t".into(),
        if job.title.trim().is_empty() { "ARutGIS".into() } else { job.title.clone() },
        "-o".into(),
        format!("print-scaling={}", if job.fit { "fit" } else { "none" }),
    ];
    // A standard page asks for its paper; any other prints on the printer's own.
    if let Some(paper) = paper_name(job.page_mm) {
        args.push("-o".into());
        args.push(format!("media={paper}"));
    }
    args
}

/// The most copies a job asks for.
pub const MAX_COPIES: u32 = 99;

/// The standard paper a page of this size is, either way up, as CUPS names it.
pub fn paper_name((w, h): (f64, f64)) -> Option<&'static str> {
    const PAPERS: [(&str, f64, f64); 9] = [
        ("A5", 148.0, 210.0),
        ("A4", 210.0, 297.0),
        ("A3", 297.0, 420.0),
        ("A2", 420.0, 594.0),
        ("A1", 594.0, 841.0),
        ("A0", 841.0, 1189.0),
        ("Letter", 215.9, 279.4),
        ("Legal", 215.9, 355.6),
        ("Tabloid", 279.4, 431.8),
    ];
    let (short, long) = (w.min(h), w.max(h));
    PAPERS.iter().find(|(_, pw, ph)| (short - pw).abs() <= 1.0 && (long - ph).abs() <= 1.0).map(|p| p.0)
}

// --- Windows PowerShell and .NET ---------------------------------------------------------------

/// Windows PowerShell, which every Windows has, by its own path rather than the search path's.
fn powershell() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe"))
        .filter(|exe| exe.is_file())
        .unwrap_or_else(|| "powershell.exe".into())
}

/// A script as PowerShell's `-EncodedCommand` takes it: its UTF-16 text in Base64. Nothing of
/// the user's is in a script: what a job names goes in the environment.
fn encoded(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

/// Runs `script` and gives its answer's lines. The scripts answer in ASCII, names and messages
/// in Base64 of their UTF-8, so the console's code page (Thai Windows: 874) changes nothing;
/// `OK` ends an answer, `ERR` and a message is a failure.
fn ask(exe: PathBuf, script: &str, env: &[(&str, String)], limit: Duration) -> Result<Vec<String>, Problem> {
    let mut c = Command::new(exe);
    c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded(script)]);
    for (name, value) in env {
        c.env(name, value);
    }
    let out = run(c, limit)?;
    answer(&out)
}

/// A script's answer (see [`ask`]).
fn answer(out: &Output) -> Result<Vec<String>, Problem> {
    let mut answered = Vec::new();
    for line in lines(&out.stdout) {
        if line == "OK" {
            return Ok(answered);
        }
        if let Some(message) = line.strip_prefix("ERR ") {
            let message = unbase64(message);
            return Err(if message == "printer-not-found" { Problem::NoPrinter } else { Problem::Said(message) });
        }
        answered.push(line);
    }
    // It never answered: PowerShell itself failed (a policy that locks it down, say).
    Err(said(out))
}

/// Text a script sent as Base64 of its UTF-8.
fn unbase64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.decode(text.trim()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

/// Lists the printers: `D` and a name for the default, `P` for the others.
const WINDOWS_LIST: &str = r#"
$ErrorActionPreference = 'Stop'
function B64($s) { [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([string]$s)) }
try {
  Add-Type -AssemblyName System.Drawing
  $default = (New-Object System.Drawing.Printing.PrinterSettings).PrinterName
  foreach ($n in [System.Drawing.Printing.PrinterSettings]::InstalledPrinters) {
    $mark = 'P'
    if ($n -eq $default) { $mark = 'D' }
    [Console]::Out.WriteLine($mark + ' ' + (B64 $n))
  }
  [Console]::Out.WriteLine('OK')
} catch {
  [Console]::Out.WriteLine('ERR ' + (B64 $_.Exception.Message))
  exit 1
}
"#;

/// Prints the pictures `page-0001.png`… of a folder, each on a page. Sizes are in hundredths of
/// an inch, .NET's printing unit. The paper is the printer's of the page's size, either way up,
/// when it has one (`PAPER 1`), else the one it is set to (`PAPER 0`). A picture is drawn from
/// the paper's corner at the page's size, or centred and sized to what the printer can reach.
const WINDOWS_PRINT: &str = r#"
$ErrorActionPreference = 'Stop'
function B64($s) { [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([string]$s)) }
try {
  Add-Type -AssemblyName System.Drawing
  $dir = $env:RUSTGIS_PRINT_DIR
  $count = [int]$env:RUSTGIS_PRINT_PAGES
  $w = [double]$env:RUSTGIS_PRINT_WIDTH
  $h = [double]$env:RUSTGIS_PRINT_HEIGHT
  $fit = $env:RUSTGIS_PRINT_FIT -eq '1'
  $doc = New-Object System.Drawing.Printing.PrintDocument
  $doc.PrinterSettings.PrinterName = $env:RUSTGIS_PRINT_PRINTER
  if (-not $doc.PrinterSettings.IsValid) { throw 'printer-not-found' }
  $doc.DocumentName = $env:RUSTGIS_PRINT_TITLE
  $doc.PrintController = New-Object System.Drawing.Printing.StandardPrintController
  $doc.PrinterSettings.Copies = [int16]$env:RUSTGIS_PRINT_COPIES
  if ($env:RUSTGIS_PRINT_FILE) {
    $doc.PrinterSettings.PrintToFile = $true
    $doc.PrinterSettings.PrintFileName = $env:RUSTGIS_PRINT_FILE
  }
  $short = [Math]::Min($w, $h)
  $long = [Math]::Max($w, $h)
  $paper = $null
  try {
    foreach ($p in $doc.PrinterSettings.PaperSizes) {
      if ([Math]::Abs($p.Width - $short) -le 6 -and [Math]::Abs($p.Height - $long) -le 6) { $paper = $p; break }
    }
  } catch { $paper = $null }
  if ($null -ne $paper) { $doc.DefaultPageSettings.PaperSize = $paper }
  $doc.DefaultPageSettings.Landscape = ($w -gt $h)
  $state = @{ page = 0 }
  $doc.add_PrintPage({
    param($sender, $e)
    $state.page = $state.page + 1
    $img = [System.Drawing.Image]::FromFile((Join-Path $dir ('page-{0:d4}.png' -f $state.page)))
    try {
      $g = $e.Graphics
      $g.PageUnit = [System.Drawing.GraphicsUnit]::Display
      $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
      $hx = $e.PageSettings.HardMarginX
      $hy = $e.PageSettings.HardMarginY
      $pw = $e.PageBounds.Width
      $ph = $e.PageBounds.Height
      if ($fit) {
        $s = [Math]::Min(($pw - 2 * $hx) / $w, ($ph - 2 * $hy) / $h)
        $dw = $w * $s
        $dh = $h * $s
        $x = ($pw - $dw) / 2 - $hx
        $y = ($ph - $dh) / 2 - $hy
      } else {
        $dw = $w
        $dh = $h
        $x = -$hx
        $y = -$hy
      }
      $g.DrawImage($img, [single]$x, [single]$y, [single]$dw, [single]$dh)
    } finally {
      $img.Dispose()
    }
    $e.HasMorePages = ($state.page -lt $count)
  })
  $doc.Print()
  $found = '0'
  if ($null -ne $paper) { $found = '1' }
  [Console]::Out.WriteLine('PAPER ' + $found + ' ' + (B64 $doc.DefaultPageSettings.PaperSize.PaperName))
  [Console]::Out.WriteLine('OK')
} catch {
  [Console]::Out.WriteLine('ERR ' + (B64 $_.Exception.Message))
  exit 1
}
"#;

fn windows_list(exe: PathBuf) -> Result<Printers, Problem> {
    let answered = ask(exe, WINDOWS_LIST, &[], LIST_LIMIT)?;
    Ok(listed(&answered))
}

/// The printers in a listing's answer.
fn listed(answered: &[String]) -> Printers {
    let (mut names, mut default) = (Vec::new(), None);
    for line in answered {
        let Some((mark, name)) = line.split_once(' ') else { continue };
        let name = unbase64(name);
        if name.is_empty() || !matches!(mark, "D" | "P") {
            continue;
        }
        if mark == "D" {
            default = Some(name.clone());
        }
        names.push(name);
    }
    printers(names, default)
}

/// A length in millimetres in hundredths of an inch, whole.
fn hundredths(mm: f64) -> String {
    format!("{:.0}", (mm / 25.4 * 100.0).clamp(1.0, 1_000_000.0))
}

/// What a job's script reads from the environment.
fn job_env(job: &Job, dir: &Path, pages: usize) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("RUSTGIS_PRINT_DIR", dir.display().to_string()),
        ("RUSTGIS_PRINT_PAGES", pages.to_string()),
        ("RUSTGIS_PRINT_WIDTH", hundredths(job.page_mm.0)),
        ("RUSTGIS_PRINT_HEIGHT", hundredths(job.page_mm.1)),
        ("RUSTGIS_PRINT_FIT", if job.fit { "1" } else { "0" }.to_string()),
        ("RUSTGIS_PRINT_PRINTER", job.printer.clone()),
        ("RUSTGIS_PRINT_TITLE", if job.title.trim().is_empty() { "ARutGIS".into() } else { job.title.clone() }),
        ("RUSTGIS_PRINT_COPIES", job.copies.clamp(1, MAX_COPIES).to_string()),
        // Set even when empty, so one in the app's own environment is never read.
        ("RUSTGIS_PRINT_FILE", String::new()),
    ];
    if let Some(file) = &job.to_file
        && let Some(last) = env.last_mut()
    {
        last.1 = file.display().to_string();
    }
    env
}

fn windows_send(exe: PathBuf, job: &Job, dir: &Path, pages: usize) -> Result<Sent, Problem> {
    if job.printer.is_empty() {
        return Err(Problem::NoPrinter);
    }
    if pages == 0 {
        return Err(Problem::Said("there is no page to print".into()));
    }
    let answered = ask(exe, WINDOWS_PRINT, &job_env(job, dir, pages), SEND_LIMIT)?;
    Ok(sent(&answered))
}

/// What a job's answer says of its paper.
fn sent(answered: &[String]) -> Sent {
    let other_paper = answered.iter().find_map(|line| {
        let mut parts = line.strip_prefix("PAPER ")?.splitn(2, ' ');
        let found = parts.next()?;
        let name = unbase64(parts.next().unwrap_or_default());
        (found == "0").then_some(name)
    });
    Sent { other_paper }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn job(printer: &str) -> Job {
        Job { printer: printer.into(), title: "Flood map".into(), copies: 2, fit: false, page_mm: (297.0, 210.0), to_file: None }
    }

    #[test]
    fn a_standard_page_is_its_paper_either_way_up() {
        assert_eq!(paper_name((210.0, 297.0)), Some("A4"));
        assert_eq!(paper_name((297.0, 210.0)), Some("A4"));
        assert_eq!(paper_name((420.0, 297.0)), Some("A3"));
        assert_eq!(paper_name((279.4, 215.9)), Some("Letter"));
        assert_eq!(paper_name((1189.0, 841.0)), Some("A0"));
        // The map view's page is no paper's.
        assert_eq!(paper_name((344.0, 185.0)), None);
        assert_eq!(paper_name((f64::NAN, 297.0)), None);
    }

    #[test]
    fn lp_is_told_the_printer_the_copies_the_size_and_the_paper() {
        assert_eq!(lp_args(&job("Office")), ["-d", "Office", "-n", "2", "-t", "Flood map", "-o", "print-scaling=none", "-o", "media=A4"]);
        let fitted = Job { fit: true, copies: 0, page_mm: (344.0, 185.0), title: "  ".into(), ..job("Office") };
        assert_eq!(lp_args(&fitted), ["-d", "Office", "-n", "1", "-t", "ARutGIS", "-o", "print-scaling=fit"]);
        assert_eq!(lp_args(&Job { copies: 5000, ..job("Office") })[3], "99");
    }

    #[test]
    fn the_default_printer_is_read_from_lpstat() {
        assert_eq!(default_destination(b"system default destination: Office_Laser\n"), Some("Office_Laser".into()));
        assert_eq!(default_destination(b"no system default destination\n"), None);
        assert_eq!(default_destination(b""), None);
        // A default that isn't among the printers is none; a name listed twice is one printer.
        let p = printers(vec!["A".into(), "B".into(), "A".into()], Some("C".into()));
        assert_eq!(p, Printers { names: vec!["A".into(), "B".into()], default: None });
    }

    #[test]
    fn a_script_is_encoded_as_powershell_reads_it() {
        // "hi" in UTF-16LE: 68 00 69 00.
        assert_eq!(encoded("hi"), "aABpAA==");
        // The scripts are plain ASCII, so every reading of them agrees.
        assert!(WINDOWS_LIST.is_ascii() && WINDOWS_PRINT.is_ascii());
        // Windows takes at most 32,767 characters on a command line.
        assert!(encoded(WINDOWS_PRINT).len() < 20_000, "{}", encoded(WINDOWS_PRINT).len());
    }

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        // An exit status can only come from a process: `sh` ends with the code asked for.
        #[cfg(unix)]
        let status = Command::new("sh").arg("-c").arg(format!("exit {code}")).status().unwrap();
        #[cfg(windows)]
        let status = Command::new("cmd").args(["/C", &format!("exit {code}")]).status().unwrap();
        Output { status, stdout: stdout.into(), stderr: stderr.into() }
    }

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    #[test]
    fn a_listing_names_the_printers_and_the_default() {
        // A Thai printer name comes through whatever the console's code page.
        let thai = "เครื่องพิมพ์ ชั้น 2";
        let out = output(0, &format!("P {}\r\nD {}\r\nP {}\r\nOK\r\n", b64("Microsoft Print to PDF"), b64(thai), b64("HP LaserJet")), "");
        let p = listed(&answer(&out).unwrap());
        assert_eq!(p.names, ["Microsoft Print to PDF", thai, "HP LaserJet"]);
        assert_eq!(p.default.as_deref(), Some(thai));
        // No printers at all is an answer, not a failure.
        assert_eq!(listed(&answer(&output(0, "OK\n", "")).unwrap()), Printers::default());
    }

    #[test]
    fn a_script_s_failure_is_said_in_its_words() {
        let out = output(1, &format!("ERR {}\n", b64("The RPC server is unavailable")), "");
        assert_eq!(answer(&out), Err(Problem::Said("The RPC server is unavailable".into())));
        assert_eq!(answer(&output(1, &format!("ERR {}\n", b64("printer-not-found")), "")), Err(Problem::NoPrinter));
        // PowerShell that never ran the script says so itself.
        let locked = output(1, "", "\nCannot create type. Only core types are supported in this language mode.\n");
        assert_eq!(answer(&locked), Err(Problem::Said("Cannot create type. Only core types are supported in this language mode.".into())));
        // An answer cut short (no OK) is a failure too.
        assert!(matches!(answer(&output(0, &format!("P {}\n", b64("X")), "")), Err(Problem::Said(_))));
    }

    #[test]
    fn a_job_says_when_the_printer_lacks_the_page_s_paper() {
        assert_eq!(sent(&[format!("PAPER 1 {}", b64("A4"))]), Sent { other_paper: None });
        assert_eq!(sent(&[format!("PAPER 0 {}", b64("Letter"))]), Sent { other_paper: Some("Letter".into()) });
        assert_eq!(sent(&[]), Sent::default());
    }

    #[test]
    fn a_job_s_script_reads_everything_from_the_environment() {
        let dir = Path::new("/tmp/job");
        let env = job_env(&Job { fit: true, to_file: Some("/tmp/out.pdf".into()), ..job("HP \"Laser\"; rm -rf") }, dir, 3);
        let get = |name: &str| env.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str()).unwrap();
        // A printer's name is passed as it is, never put into the script's text.
        assert_eq!(get("RUSTGIS_PRINT_PRINTER"), "HP \"Laser\"; rm -rf");
        assert!(!WINDOWS_PRINT.contains("Laser"));
        // A4 landscape: 297 mm is 11.69 in, 210 mm is 8.27 in.
        assert_eq!((get("RUSTGIS_PRINT_WIDTH"), get("RUSTGIS_PRINT_HEIGHT")), ("1169", "827"));
        assert_eq!((get("RUSTGIS_PRINT_PAGES"), get("RUSTGIS_PRINT_COPIES"), get("RUSTGIS_PRINT_FIT")), ("3", "2", "1"));
        assert_eq!(get("RUSTGIS_PRINT_FILE"), "/tmp/out.pdf");
        assert_eq!(job_env(&job("X"), dir, 1).iter().find(|(n, _)| *n == "RUSTGIS_PRINT_FILE").unwrap().1, "");
        assert_eq!(page_name(1), "page-0001.png");
        assert_eq!(page_name(12), "page-0012.png");
    }

    /// Scripts standing for CUPS's programs, in a folder of their own.
    #[cfg(unix)]
    fn cups_stand_in(dir: &Path, lpstat: &str, lp: &str) -> Cups {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        for (name, body) in [("lpstat", lpstat), ("lp", lp)] {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Cups { lp: dir.join("lp"), lpstat: dir.join("lpstat") }
    }

    #[cfg(unix)]
    #[test]
    fn cups_lists_its_printers_and_takes_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("lp.log");
        let cups = cups_stand_in(
            dir.path(),
            r#"case "$1" in
  -e) printf 'Office_Laser\nPlotter_A0\n' ;;
  -d) echo "system default destination: Plotter_A0" ;;
esac"#,
            &format!(r#"printf '%s\n' "$@" > '{}'; echo "request id is Plotter_A0-7 (1 file(s))""#, log.display()),
        );
        let p = cups.list().unwrap();
        assert_eq!(p, Printers { names: vec!["Office_Laser".into(), "Plotter_A0".into()], default: Some("Plotter_A0".into()) });
        let pdf = dir.path().join(PDF_NAME);
        std::fs::write(&pdf, b"%PDF-1.7").unwrap();
        assert_eq!(cups.send(&job("Plotter_A0"), &pdf), Ok(Sent::default()));
        let args: Vec<String> = std::fs::read_to_string(&log).unwrap().lines().map(str::to_string).collect();
        let mut want = lp_args(&job("Plotter_A0"));
        want.extend(["--".to_string(), pdf.display().to_string()]);
        assert_eq!(args, want);
        // A name that would be read as an option is refused before lp sees it.
        assert_eq!(cups.send(&job("-oraw"), &pdf), Err(Problem::NoPrinter));
    }

    #[cfg(unix)]
    #[test]
    fn cups_says_what_went_wrong() {
        let dir = tempfile::tempdir().unwrap();
        // An older CUPS has no `-e`; none added is an empty list; a printer gone is said so.
        let older = cups_stand_in(
            &dir.path().join("older"),
            r#"case "$1" in
  -e) echo "lpstat: Unknown option" >&2; exit 1 ;;
  -a) echo "Old_Printer accepting requests since Mon" ;;
  -d) echo "no system default destination" ;;
esac"#,
            r#"echo "lp: The printer or class does not exist." >&2; exit 1"#,
        );
        assert_eq!(older.list().unwrap(), Printers { names: vec!["Old_Printer".into()], default: None });
        assert_eq!(older.send(&job("Gone"), Path::new("x.pdf")), Err(Problem::NoPrinter));
        let none = cups_stand_in(&dir.path().join("none"), r#"echo "lpstat: No destinations added." >&2; exit 1"#, "exit 1");
        assert_eq!(none.list().unwrap(), Printers::default());
        let down = cups_stand_in(
            &dir.path().join("down"),
            r#"echo "lpstat: Bad file descriptor" >&2; exit 1"#,
            r#"echo "lp: Error - scheduler not responding." >&2; exit 1"#,
        );
        assert_eq!(down.list(), Err(Problem::Said("lpstat: Bad file descriptor".into())));
        assert_eq!(down.send(&job("Office"), Path::new("x.pdf")), Err(Problem::Said("lp: Error - scheduler not responding.".into())));
        // A computer without CUPS says that, not "No such file or directory".
        let missing = Cups { lp: dir.path().join("no-lp"), lpstat: dir.path().join("no-lpstat") };
        assert_eq!(missing.list(), Err(Problem::Missing));
        assert_eq!(missing.send(&job("Office"), Path::new("x.pdf")), Err(Problem::Missing));
    }

    #[cfg(unix)]
    #[test]
    fn a_program_that_hangs_is_stopped() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 30"]);
        let start = Instant::now();
        assert_eq!(run(c, Duration::from_millis(200)).err(), Some(Problem::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    /// The real thing on Windows: PowerShell lists the printers, and a page printed to Microsoft
    /// Print to PDF (which writes a file instead of asking where) comes out as a PDF.
    #[cfg(windows)]
    #[test]
    fn windows_lists_its_printers_and_prints_a_page() {
        let printers = list().expect("Windows PowerShell lists the printers");
        eprintln!("printers: {printers:?}");
        let dir = tempfile::tempdir().unwrap();
        let mut page = image::RgbaImage::from_pixel(600, 400, image::Rgba([255, 255, 255, 255]));
        for x in 100..500 {
            for y in 100..300 {
                page.put_pixel(x, y, image::Rgba([200, 30, 30, 255]));
            }
        }
        page.save(dir.path().join(page_name(1))).unwrap();
        page.save(dir.path().join(page_name(2))).unwrap();
        // A printer this computer doesn't have is said to be missing.
        let gone = Job { printer: "No such printer 5e1f".into(), ..job("") };
        assert_eq!(send(&gone, dir.path(), 2), Err(Problem::NoPrinter));
        let Some(pdf_printer) = printers.names.iter().find(|n| n.as_str() == "Microsoft Print to PDF") else {
            eprintln!("no Microsoft Print to PDF here: the print itself is not tested");
            return;
        };
        for (fit, name) in [(false, "actual.pdf"), (true, "fit.pdf")] {
            let out = dir.path().join(name);
            let j = Job { printer: pdf_printer.clone(), fit, copies: 1, to_file: Some(out.clone()), ..job("") };
            let sent = send(&j, dir.path(), 2).expect("the pages print");
            eprintln!("{name}: {sent:?}, {} bytes", std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0));
            let bytes = std::fs::read(&out).expect("the printer wrote its file");
            assert!(bytes.starts_with(b"%PDF"), "{name} is a PDF");
            // Two pages, each with its picture.
            assert!(bytes.len() > 2000, "{name}: {} bytes", bytes.len());
        }
    }
}
