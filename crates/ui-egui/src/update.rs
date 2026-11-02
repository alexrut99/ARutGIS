//! Help › Check for Updates: asks GitHub for the latest release and compares it with this build.
//! Nothing is sent until the user asks. When a newer release has an installer for the way this
//! copy was installed (setup.exe, .msi, .dmg, AppImage, .deb or .rpm), Update Now downloads it,
//! checks it against the release's SHA256SUMS and starts it as the app closes.

use egui::Align2;
use serde_json::Value;

use crate::{RustGisApp, Slot};

/// The latest release, as JSON naming its tag and page.
const LATEST: &str = "https://api.github.com/repos/alexrut99/ARutGIS/releases/latest";
/// Every release, for when the check can't reach GitHub.
const RELEASES: &str = "https://github.com/alexrut99/ARutGIS/releases";
/// Where this project's release files are: an installer from anywhere else isn't run.
const DOWNLOADS: &str = "https://github.com/alexrut99/ARutGIS/releases/download/";
/// The release JSON is a few kilobytes; anything far larger isn't it.
const MAX_BYTES: usize = 1024 * 1024;
/// The largest installer downloaded (they are 30–65 MB).
const MAX_INSTALLER: u64 = 512 * 1024 * 1024;
/// The release's checksums, one line per file.
const SUMS: &str = "SHA256SUMS";

/// The Check for Updates window.
#[derive(Default)]
pub struct UpdateState {
    pub open: bool,
    /// Start a check on the next frame (commands have no egui context to repaint with).
    start: bool,
    pending: Option<Slot<Result<Vec<u8>, String>>>,
    result: Option<Result<Release, String>>,
    /// How this copy was installed, once looked at (`Some(None)`: a way Update Now can't
    /// replace, such as the portable .zip or the Microsoft Store).
    install: Option<Option<Install>>,
    get: Get,
    /// What runs as the app closes: the downloaded installer.
    launch: Option<Launch>,
}

impl UpdateState {
    /// Opens the window and checks again.
    pub fn check(&mut self) {
        self.open = true;
        self.start = true;
        self.result = None;
        if matches!(self.get, Get::Failed(_)) {
            self.get = Get::Idle;
        }
    }
}

/// Update Now's download.
#[derive(Default)]
enum Get {
    #[default]
    Idle,
    Downloading {
        progress: std::sync::Arc<rustgis_engine::plugins::registry::Progress>,
        done: Slot<Result<std::path::PathBuf, String>>,
    },
    Failed(String),
}

/// What starts the update once the app has closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub install: Install,
    pub path: std::path::PathBuf,
}

/// A way this copy may have been installed, each with its own release file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// Windows, per user: `…-windows-<arch>-setup.exe`.
    Setup,
    /// Windows, for all users: `…-windows-<arch>.msi`.
    Msi,
    /// macOS: `…-macos-<arch>.dmg`.
    Dmg,
    /// Linux: `…-linux-<arch>.AppImage`, replaced where it is.
    AppImage,
    /// Debian and Ubuntu: `rustgis_<version>_<amd64|arm64>.deb`.
    Deb,
    /// Fedora and openSUSE: `rustgis-<version>-1.<arch>.rpm`.
    Rpm,
}

impl Install {
    /// The release file for `version` on processor `arch` (`x86_64` or `aarch64`).
    pub fn file(self, version: &str, arch: &str) -> String {
        let debian = if arch == "aarch64" { "arm64" } else { "amd64" };
        match self {
            Install::Setup => format!("ARutGIS-{version}-windows-{arch}-setup.exe"),
            Install::Msi => format!("ARutGIS-{version}-windows-{arch}.msi"),
            Install::Dmg => format!("ARutGIS-{version}-macos-{arch}.dmg"),
            Install::AppImage => format!("ARutGIS-{version}-linux-{arch}.AppImage"),
            Install::Deb => format!("rustgis_{version}_{debian}.deb"),
            Install::Rpm => format!("rustgis-{version}-1.{arch}.rpm"),
        }
    }
}

/// A file published with a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A published release: its version (the tag without its `v`), its page and its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub assets: Vec<Asset>,
}

impl Release {
    /// The file Update Now installs for `install` on `arch`, with the checksums to check it
    /// against: both must be this project's release files.
    pub fn installer(&self, install: Install, arch: &str) -> Option<(&Asset, &Asset)> {
        let name = install.file(&self.version, arch);
        let find = |n: &str| self.assets.iter().find(|a| a.name == n);
        Some((find(&name)?, find(SUMS)?))
    }
}

/// Reads GitHub's latest-release JSON.
pub fn parse_release(body: &[u8]) -> Result<Release, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "GitHub's reply isn't a release".to_string())?;
    let tag =
        v.get("tag_name").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).ok_or("GitHub's reply names no release")?;
    // A tag that isn't a version can't be compared: saying "up to date" would be a guess.
    if version(tag).is_none() {
        return Err("GitHub's latest release has no version number".into());
    }
    let url = v.get("html_url").and_then(Value::as_str).filter(|u| u.starts_with("https://")).unwrap_or(RELEASES);
    // Only files served from this project's releases, with plain names (no folders).
    let assets = v
        .get("assets")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .take(500)
                .filter_map(|a| {
                    let name = a.get("name").and_then(Value::as_str)?;
                    let url = a.get("browser_download_url").and_then(Value::as_str)?;
                    let plain = !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..";
                    (plain && url.starts_with(DOWNLOADS)).then(|| Asset {
                        name: name.to_string(),
                        url: url.to_string(),
                        size: a.get("size").and_then(Value::as_u64).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { version: tag.trim_start_matches(['v', 'V']).to_string(), url: url.to_string(), assets })
}

/// The SHA-256 that a `SHA256SUMS` file (`<hex>  <name>` lines) gives `name`.
pub fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.trim().split_once(char::is_whitespace)?;
        let file = file.trim().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_ascii_lowercase())
    })
}

/// A pre-release identifier: numbers sort numerically and before words (`beta.2` < `beta.10`,
/// `1` < `alpha`).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Num(u64),
    Word(String),
}
/// A version's dotted numbers (one to three; missing ones are 0) and its pre-release
/// identifiers, without build metadata; `None` when it isn't a version.
fn version(v: &str) -> Option<([u64; 3], Vec<Ident>)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let v = v.split('+').next().unwrap_or(v);
    let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p)));
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut n = [0u64; 3];
    for (slot, part) in n.iter_mut().zip(&parts) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    let pre = match pre {
        None => Vec::new(),
        Some("") => return None,
        Some(p) => p.split('.').map(|i| i.parse().map_or_else(|_| Ident::Word(i.to_string()), Ident::Num)).collect(),
    };
    Some((n, pre))
}

/// Whether version `latest` is newer than `current`, in semantic-version order: the numbers
/// first, then a pre-release (`0.2.0-beta.1`) before its release, and pre-releases by their
/// identifiers. False when either isn't a version.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (Some((a, a_pre)), Some((b, b_pre))) = (version(latest), version(current)) else { return false };
    if a != b {
        return a > b;
    }
    match (a_pre.is_empty(), b_pre.is_empty()) {
        (true, true) | (false, true) => false,
        (true, false) => true,
        (false, false) => a_pre > b_pre,
    }
}

/// How this copy was installed, from where it runs: `None` for a copy Update Now can't replace
/// (the portable .zip, the Microsoft Store's, one built from source).
#[cfg(not(target_arch = "wasm32"))]
fn this_install() -> Option<Install> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    if cfg!(windows) {
        // NSIS's per-user install leaves its uninstaller beside the app.
        if dir.join("uninstall.exe").is_file() {
            return Some(Install::Setup);
        }
        // The .msi installs into Program Files\ARutGIS (the Store's copy is under WindowsApps).
        let program_files = ["ProgramW6432", "ProgramFiles"].iter().filter_map(std::env::var_os).map(std::path::PathBuf::from);
        let msi = program_files.into_iter().any(|p| dir == p.join("ARutGIS"));
        return msi.then_some(Install::Msi);
    }
    if cfg!(target_os = "macos") {
        let s = exe.to_string_lossy();
        return s.contains(".app/Contents/MacOS/").then_some(Install::Dmg);
    }
    // Linux: an AppImage says where its file is; the packages install /usr/bin/rustgis.
    if std::env::var_os("APPIMAGE").is_some_and(|p| std::path::Path::new(&p).is_file()) {
        return Some(Install::AppImage);
    }
    if exe != std::path::Path::new("/usr/bin/rustgis") {
        return None;
    }
    if std::path::Path::new("/var/lib/dpkg/info/rustgis.list").is_file() {
        return Some(Install::Deb);
    }
    ["/etc/fedora-release", "/etc/redhat-release", "/etc/SUSE-brand"]
        .iter()
        .any(|p| std::path::Path::new(p).is_file())
        .then_some(Install::Rpm)
}

#[cfg(target_arch = "wasm32")]
fn this_install() -> Option<Install> {
    None
}

/// Downloads the installer and its checksums off the frame, checks the one against the other
/// and gives where the checked installer is (an AppImage: already in its place).
#[cfg(not(target_arch = "wasm32"))]
fn start_download(install: Install, file: &Asset, sums: &Asset, ctx: &egui::Context) -> Get {
    use rustgis_engine::plugins::registry;
    let progress = std::sync::Arc::new(registry::Progress::default());
    let done: Slot<Result<std::path::PathBuf, String>> = Slot::default();
    let (file, sums, ctx) = (file.clone(), sums.clone(), ctx.clone());
    let (p, out) = (progress.clone(), done.clone());
    std::thread::spawn(move || {
        let r = (|| -> Result<std::path::PathBuf, String> {
            let text = registry::fetch(&sums.url, MAX_BYTES)?;
            let want = sum_for(&String::from_utf8_lossy(&text), &file.name)
                .ok_or_else(|| crate::i18n::fmt("The release's checksums don't list {file}.", &[("file", &file.name)]))?;
            // An AppImage is downloaded beside itself, so it can replace itself in one rename.
            let appimage = std::env::var_os("APPIMAGE").map(std::path::PathBuf::from);
            let dest = match (install, &appimage) {
                (Install::AppImage, Some(a)) => {
                    let mut d = a.clone().into_os_string();
                    d.push(".update");
                    std::path::PathBuf::from(d)
                }
                _ => {
                    let dir = std::env::temp_dir().join("ARutGIS-update");
                    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    dir.join(&file.name)
                }
            };
            let got = registry::download_file(&file.url, &dest, MAX_INSTALLER, &p)?;
            if got != want {
                let _ = std::fs::remove_file(&dest);
                return Err(crate::i18n::fmt("{file} doesn't match the release's checksum: it wasn't installed.", &[("file", &file.name)]));
            }
            if let (Install::AppImage, Some(a)) = (install, appimage) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
                        .map_err(|e| format!("{}: {e}", dest.display()))?;
                }
                std::fs::rename(&dest, &a).map_err(|e| {
                    let _ = std::fs::remove_file(&dest);
                    format!("{}: {e}", a.display())
                })?;
                return Ok(a);
            }
            Ok(dest)
        })();
        if let Ok(mut s) = out.lock() {
            *s = Some(r);
        }
        ctx.request_repaint();
    });
    Get::Downloading { progress, done }
}

#[cfg(target_arch = "wasm32")]
fn start_download(_: Install, _: &Asset, _: &Asset, _: &egui::Context) -> Get {
    Get::Failed("the web app updates itself".into())
}

/// Starts the downloaded update: called as the app closes, so the installer can replace it.
/// Each installer is run as the user would open it, and asks before it changes anything.
pub fn launch(app: &mut RustGisApp) {
    let Some(l) = app.update.launch.take() else { return };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::process::Command;
        let p = &l.path;
        let started = match l.install {
            Install::Setup | Install::AppImage => Command::new(p).spawn(),
            Install::Msi => Command::new("msiexec").arg("/i").arg(p).spawn(),
            Install::Dmg => Command::new("open").arg(p).spawn(),
            Install::Deb | Install::Rpm => Command::new("xdg-open").arg(p).spawn(),
        };
        if let Err(e) = started {
            log::warn!("starting the update {}: {e}", p.display());
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = l;
}

/// What the user is told the update does once it's downloaded (translated where shown).
fn ready_text(install: Install) -> &'static str {
    match install {
        Install::Setup | Install::Msi => "The update is ready. ARutGIS closes, then its installer opens: follow it to finish.",
        Install::Dmg => {
            "The update is ready. ARutGIS closes and the disk image opens: drag ARutGIS to Applications, replacing the old one."
        }
        Install::AppImage => "The update is installed. ARutGIS restarts as the new version when it closes.",
        Install::Deb | Install::Rpm => "The update is ready. ARutGIS closes and the package opens in your software installer.",
    }
}

/// Draws the window, starting and collecting the check.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    if !app.update.open {
        return;
    }
    // A check asked for while one is running waits for that one rather than sending another.
    if std::mem::take(&mut app.update.start) && app.update.pending.is_none() && !cfg!(target_arch = "wasm32") {
        app.update.pending = Some(crate::pluginsui::fetch(app, LATEST, MAX_BYTES, Some(ctx.clone())));
    }
    if let Some(r) = app.update.pending.as_ref().and_then(crate::pluginsui::take) {
        app.update.pending = None;
        app.update.result = Some(r.and_then(|body| parse_release(&body)));
    }
    let install = *app.update.install.get_or_insert_with(this_install);
    // The download finished: on success, the app closes to install it (the Save Changes
    // question first, when the project has some).
    let finished = match &app.update.get {
        Get::Downloading { done, .. } => done.lock().ok().and_then(|mut s| s.take()),
        _ => None,
    };
    match (finished, install) {
        (Some(Ok(path)), Some(install)) => {
            app.update.get = Get::Idle;
            app.update.launch = Some(Launch { install, path });
            app.quit_requested = true;
        }
        (Some(Err(e)), _) => app.update.get = Get::Failed(e),
        _ => {}
    }
    let current = env!("CARGO_PKG_VERSION");
    let arch = std::env::consts::ARCH;
    let mut open = true;
    let mut again = false;
    let mut visit: Option<String> = None;
    let mut update_now: Option<(Install, Asset, Asset)> = None;
    let mut stop = false;
    let mut install_now = false;
    let mut not_now = false;
    egui::Window::new(tl!("Check for Updates"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.set_width(400.0);
            if cfg!(target_arch = "wasm32") {
                ui.label(tl!("The web app always runs the latest release: reload the page to get it."));
                return;
            }
            // Downloaded and waiting for the app to close (its Save Changes question was cancelled).
            if let Some(l) = &app.update.launch {
                ui.label(egui::RichText::new(tl!(ready_text(l.install))).strong());
                ui.label(tl!("It starts when ARutGIS closes."));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(tl!("Close and Install")).clicked() {
                        install_now = true;
                    }
                    if ui.button(tl!("Not Now")).on_hover_text(tl!("Keep this version: the update isn't started")).clicked() {
                        not_now = true;
                    }
                });
                return;
            }
            match &app.update.result {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tl!("Checking for updates…"));
                    });
                }
                Some(Ok(r)) if is_newer(&r.version, current) => {
                    ui.label(egui::RichText::new(crate::i18n::fmt("RustGIS {latest} is available.", &[("latest", &r.version)])).strong());
                    ui.label(crate::i18n::fmt("You have version {version}.", &[("version", current)]));
                    ui.add_space(6.0);
                    let installer = install.and_then(|i| r.installer(i, arch).map(|(f, s)| (i, f, s)));
                    match &app.update.get {
                        Get::Downloading { progress, .. } => {
                            use std::sync::atomic::Ordering;
                            let got = progress.got.load(Ordering::Relaxed);
                            let total = progress.total.load(Ordering::Relaxed);
                            let mb = |b: u64| format!("{:.1}", b as f64 / (1024.0 * 1024.0));
                            let text = if total > 0 {
                                crate::i18n::fmt("Downloading {got} of {total} MB…", &[("got", &mb(got)), ("total", &mb(total))])
                            } else {
                                crate::i18n::fmt("Downloading {got} MB…", &[("got", &mb(got))])
                            };
                            let fraction = if total > 0 { (got as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
                            ui.add(egui::ProgressBar::new(fraction).text(text).animate(total == 0));
                            ui.add_space(4.0);
                            if ui.button(tl!("Stop")).clicked() {
                                stop = true;
                            }
                            ctx.request_repaint_after(std::time::Duration::from_millis(200));
                        }
                        get => {
                            if let Get::Failed(e) = get {
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    crate::i18n::fmt("Couldn't download the update: {error}", &[("error", tl_raw!(e.as_str()))]),
                                );
                                ui.add_space(4.0);
                            }
                            if installer.is_none() {
                                ui.label(tl!("This copy can't update itself (a portable or store copy): download the new version from its release page."));
                                ui.add_space(4.0);
                            }
                            ui.horizontal(|ui| {
                                if let Some((i, f, s)) = &installer {
                                    let size = format!("{:.0}", f.size as f64 / (1024.0 * 1024.0));
                                    let tip = crate::i18n::fmt("Downloads {file} ({size} MB), checks it and installs it", &[("file", &f.name), ("size", &size)]);
                                    if ui.button(egui::RichText::new(tl!("Update Now")).strong()).on_hover_text(tip).clicked() {
                                        update_now = Some((*i, (*f).clone(), (*s).clone()));
                                    }
                                }
                                if ui.button(tl!("Open the Release Page")).clicked() {
                                    visit = Some(r.url.clone());
                                }
                                if ui.button(tl!("Check Again")).clicked() {
                                    again = true;
                                }
                            });
                        }
                    }
                }
                Some(Ok(_)) => {
                    ui.label(crate::i18n::fmt("RustGIS {version} is the latest version.", &[("version", current)]));
                    ui.add_space(6.0);
                    if ui.button(tl!("Check Again")).clicked() {
                        again = true;
                    }
                }
                Some(Err(e)) => {
                    ui.label(crate::i18n::fmt("Couldn't check for updates: {error}", &[("error", tl_raw!(e.as_str()))]));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(tl!("Open the Release Page")).clicked() {
                            visit = Some(RELEASES.to_string());
                        }
                        if ui.button(tl!("Check Again")).clicked() {
                            again = true;
                        }
                    });
                }
            }
        });
    if let Some((i, f, s)) = update_now {
        app.update.get = start_download(i, &f, &s, ctx);
    }
    if stop {
        if let Get::Downloading { progress, .. } = &app.update.get {
            progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        app.update.get = Get::Idle;
    }
    if install_now {
        app.quit_requested = true;
    }
    if not_now {
        app.update.launch = None;
    }
    if again {
        app.update.check();
    }
    if visit.is_some() {
        app.ui.open_url = visit;
    }
    if !open {
        app.update.open = false;
        app.update.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_with_pre_releases_first() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.2.0-beta.2", "0.2.0-beta.1"));
        assert!(is_newer("0.2.0-beta.10", "0.2.0-beta.2"));
        assert!(is_newer("0.2.0-rc.1", "0.2.0-beta.3"));
        assert!(is_newer("0.2.0-beta", "0.2.0-1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0-beta.1"));
        assert!(!is_newer("0.1.0+build.5", "0.1.0"));
        assert!(!is_newer("junk", "0.1.0"));
        assert!(!is_newer("release-2.0.0", "0.1.0"));
        assert!(!is_newer("1.2.3.4", "0.1.0"));
        assert!(!is_newer("", ""));
    }

    #[test]
    fn release_json_is_read_and_checked() {
        let r = parse_release(br#"{"tag_name":"v0.2.0","html_url":"https://github.com/opengeos/RustGIS/releases/tag/v0.2.0"}"#).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(r.url.ends_with("/v0.2.0"));
        // A page that isn't HTTPS falls back to the releases list.
        assert_eq!(parse_release(br#"{"tag_name":"0.2.0","html_url":"javascript:alert(1)"}"#).unwrap().url, RELEASES);
        assert!(parse_release(b"<html>rate limited</html>").is_err());
        assert!(parse_release(br#"{"message":"Not Found"}"#).is_err());
        assert!(parse_release(br#"{"tag_name":"  "}"#).is_err());
        // A tag that isn't a version is an error, not "up to date".
        assert!(parse_release(br#"{"tag_name":"release-2.0.0"}"#).is_err());
        assert_eq!(parse_release(br#"{"tag_name":"v1.0.0-rc.1"}"#).unwrap().version, "1.0.0-rc.1");
    }

    /// v0.5.1's release, as GitHub lists it (the file names `package.sh` gives them).
    fn published() -> Release {
        let names = [
            "ARutGIS-0.5.1-linux-aarch64.AppImage",
            "ARutGIS-0.5.1-linux-x86_64.AppImage",
            "ARutGIS-0.5.1-linux-x86_64.AppImage.zsync",
            "ARutGIS-0.5.1-macos-aarch64.dmg",
            "ARutGIS-0.5.1-macos-x86_64.dmg",
            "ARutGIS-0.5.1-windows-aarch64-setup.exe",
            "ARutGIS-0.5.1-windows-x86_64-portable.zip",
            "ARutGIS-0.5.1-windows-x86_64-setup.exe",
            "ARutGIS-0.5.1-windows-x86_64.msi",
            "rustgis-0.5.1-1.aarch64.rpm",
            "rustgis_0.5.1_amd64.deb",
            "rustgis_0.5.1_arm64.deb",
            "SHA256SUMS",
        ];
        let assets: Vec<Value> = names
            .iter()
            .map(|n| serde_json::json!({"name": n, "size": 1000, "browser_download_url": format!("{DOWNLOADS}v0.5.1/{n}")}))
            .collect();
        let body = serde_json::json!({"tag_name": "v0.5.1", "html_url": format!("{RELEASES}/tag/v0.5.1"), "assets": assets});
        parse_release(body.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn each_install_finds_its_own_installer_and_the_checksums() {
        let r = published();
        let name = |i, arch| r.installer(i, arch).map(|(f, s)| (f.name.clone(), s.name.clone()));
        let sums = || "SHA256SUMS".to_string();
        assert_eq!(name(Install::Setup, "x86_64"), Some(("ARutGIS-0.5.1-windows-x86_64-setup.exe".into(), sums())));
        assert_eq!(name(Install::Setup, "aarch64").unwrap().0, "ARutGIS-0.5.1-windows-aarch64-setup.exe");
        assert_eq!(name(Install::Msi, "x86_64").unwrap().0, "ARutGIS-0.5.1-windows-x86_64.msi");
        assert_eq!(name(Install::Dmg, "aarch64").unwrap().0, "ARutGIS-0.5.1-macos-aarch64.dmg");
        assert_eq!(name(Install::AppImage, "x86_64").unwrap().0, "ARutGIS-0.5.1-linux-x86_64.AppImage");
        assert_eq!(name(Install::Deb, "x86_64").unwrap().0, "rustgis_0.5.1_amd64.deb");
        assert_eq!(name(Install::Deb, "aarch64").unwrap().0, "rustgis_0.5.1_arm64.deb");
        assert_eq!(name(Install::Rpm, "aarch64").unwrap().0, "rustgis-0.5.1-1.aarch64.rpm");
        // Not published: no installer, so no Update Now.
        assert_eq!(name(Install::Msi, "aarch64"), None);
        assert_eq!(name(Install::Rpm, "x86_64"), None);
        // Without checksums nothing is installed.
        let mut bare = r.clone();
        bare.assets.retain(|a| a.name != SUMS);
        assert!(bare.installer(Install::Setup, "x86_64").is_none());
    }

    #[test]
    fn only_this_projects_release_files_are_taken() {
        let body = serde_json::json!({"tag_name": "v1.0.0", "assets": [
            {"name": "a.exe", "browser_download_url": "https://example.com/a.exe"},
            {"name": "b.exe", "browser_download_url": "http://github.com/alexrut99/ARutGIS/releases/download/v1.0.0/b.exe"},
            {"name": "../c.exe", "browser_download_url": format!("{DOWNLOADS}v1.0.0/c.exe")},
            {"name": "d.exe", "browser_download_url": format!("{DOWNLOADS}v1.0.0/d.exe"), "size": 5},
        ]});
        let r = parse_release(body.to_string().as_bytes()).unwrap();
        assert_eq!(r.assets, vec![Asset { name: "d.exe".into(), url: format!("{DOWNLOADS}v1.0.0/d.exe"), size: 5 }]);
        // A release without files is still a release (the release page is offered).
        assert!(parse_release(br#"{"tag_name":"v1.0.0"}"#).unwrap().assets.is_empty());
    }

    #[test]
    fn checksums_are_read_by_file_name() {
        let h = "26c405be26496d3a3202a2d3059a3eb26813373cdc4c44f528ab2042dc497619";
        let sums = format!("{h}  ARutGIS-0.5.1-linux-aarch64.AppImage\n{}  ARutGIS-0.5.1-linux-aarch64.AppImage.zsync\n", "a".repeat(64));
        assert_eq!(sum_for(&sums, "ARutGIS-0.5.1-linux-aarch64.AppImage").as_deref(), Some(h));
        assert_eq!(sum_for(&sums, "ARutGIS-0.5.1-linux-aarch64.AppImage.zsync"), Some("a".repeat(64)));
        // The binary-mode marker and capitals are read too; a short hash isn't one.
        assert_eq!(sum_for(&format!("{}  *x.dmg", h.to_uppercase()), "x.dmg").as_deref(), Some(h));
        assert_eq!(sum_for("abc  x.dmg", "x.dmg"), None);
        assert_eq!(sum_for(&sums, "missing"), None);
    }

    mod window {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;

        use super::*;

        fn harness(install: Option<Install>) -> egui_kittest::Harness<'static, RustGisApp> {
            let mut app = RustGisApp::with_sample();
            app.update.open = true;
            let mut r = published();
            r.version = "99.0.0".into();
            for a in &mut r.assets {
                a.name = a.name.replace("0.5.1", "99.0.0");
            }
            app.update.result = Some(Ok(r));
            app.update.install = Some(install);
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.run_steps(4);
            h
        }

        #[test]
        fn a_newer_release_offers_update_now_for_an_installed_copy() {
            let h = harness(Some(Install::Setup));
            assert!(h.query_by_role_and_label(Role::Button, "Update Now").is_some());
            assert!(h.query_by_role_and_label(Role::Button, "Open the Release Page").is_some());
            // A portable or store copy can't replace itself: the release page only.
            let h = harness(None);
            assert!(h.query_by_role_and_label(Role::Button, "Update Now").is_none());
            assert!(h.query_by_label_contains("can't update itself").is_some());
        }

        #[test]
        fn a_downloaded_update_waits_for_the_app_to_close() {
            let mut h = harness(Some(Install::Setup));
            h.state_mut().update.launch = Some(Launch { install: Install::Setup, path: "setup.exe".into() });
            h.run_steps(2);
            assert!(h.query_by_label_contains("its installer opens").is_some());
            h.get_by_role_and_label(Role::Button, "Not Now").click();
            h.run_steps(2);
            assert!(h.state().update.launch.is_none());
            assert!(!h.state().quit_requested);
            h.state_mut().update.launch = Some(Launch { install: Install::Setup, path: "setup.exe".into() });
            h.run_steps(2);
            h.get_by_role_and_label(Role::Button, "Close and Install").click();
            h.run_steps(1);
            assert!(h.state().quit_requested);
            assert!(h.state().update.launch.is_some());
        }
    }
}
