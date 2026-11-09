//! Official updates from stable releases whose tags are reachable from GitHub main.
//! HTTPS in-process through WinHTTP to pinned GitHub hosts (`update_net.rs`); Setup is
//! checked from a held handle and opened by the shell (`update_setup.rs`); automatic checks
//! can be turned off in Settings (`update_prefs.rs`).
use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};

use serde::Deserialize;

#[path = "update_net.rs"]
pub(crate) mod net;
#[path = "update_prefs.rs"]
mod prefs;
#[path = "update_setup.rs"]
mod setup;

pub const REPO: &str = "LoneBeast27/recall";
const SETUP: &str = "RecallSetup.exe";
const MAX_SETUP: u64 = 128 * 1024 * 1024;
const API_LIMIT: u64 = 1024 * 1024;
/// The first automatic check waits this long after start-up.
const FIRST_CHECK: Duration = Duration::from_secs(8);

pub fn current() -> Option<&'static str> {
    option_env!("RECALL_VERSION").filter(|v| version(v).is_some())
}

fn version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.split('.');
    let mut number = || {
        let part = parts.next()?;
        if part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        part.parse().ok()
    };
    let out = [number()?, number()?, number()?];
    parts.next().is_none().then_some(out)
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Asset {
    name: String,
    size: u64,
    state: String,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub title: String,
    pub notes: String,
    pub size: u64,
    digest: String,
}

fn offer(json: &[u8], current: &str) -> Result<Option<Release>, String> {
    let r: ReleaseJson =
        serde_json::from_slice(json).map_err(|e| format!("Invalid GitHub release: {e}"))?;
    let candidate = r.tag_name.strip_prefix('v').unwrap_or_default();
    let Some(candidate_version) = version(candidate) else {
        return Ok(None);
    };
    if r.draft || r.prerelease || version(current).is_none_or(|v| candidate_version <= v) {
        return Ok(None);
    }
    let asset = r
        .assets
        .iter()
        .find(|a| a.name == SETUP && a.state == "uploaded")
        .ok_or("The release has no Windows Setup asset.")?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("GitHub did not provide a valid Setup checksum.")?;
    if asset.size == 0 || asset.size > MAX_SETUP {
        return Err("The reported Setup size is invalid.".into());
    }
    Ok(Some(Release {
        version: candidate.into(),
        title: r
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Recall {candidate}")),
        notes: r.body.unwrap_or_default(),
        size: asset.size,
        digest: digest.to_ascii_lowercase(),
    }))
}

#[derive(Deserialize)]
struct Comparison {
    status: String,
    behind_by: u64,
}

fn on_main(json: &[u8]) -> Result<bool, String> {
    let comparison: Comparison =
        serde_json::from_slice(json).map_err(|e| format!("Invalid GitHub comparison: {e}"))?;
    Ok(comparison.behind_by == 0 && matches!(comparison.status.as_str(), "ahead" | "identical"))
}

fn api(net: &dyn net::Transport, path: &str) -> Result<(u16, Vec<u8>), String> {
    let mut body = Vec::new();
    let url = format!("https://api.github.com/repos/{REPO}/{path}");
    let status = net::fetch(net, &url, API_LIMIT, Duration::from_secs(20), &mut body)?;
    Ok((status, body))
}

fn check(net: &dyn net::Transport, current: &str, manual: bool) -> Result<Option<Release>, String> {
    let (status, body) = api(net, "releases/latest")?;
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        return Err(format!("GitHub returned HTTP {status}."));
    }
    let Some(release) = offer(&body, current)? else {
        return Ok(None);
    };
    if !manual && skipped().as_deref() == Some(release.version.as_str()) {
        return Ok(None);
    }
    // The first compare page includes changed files. Only the ancestry summary is needed.
    let (status, body) = api(net, &format!("compare/v{}...main?per_page=1&page=2", release.version))?;
    if status != 200 || !on_main(&body)? {
        return Err("The release tag could not be verified on main.".into());
    }
    Ok(Some(release))
}

fn skip_file() -> Option<PathBuf> {
    crate::user_dirs::local_appdata().map(|p| crate::user_dirs::checked(p.join("Recall").join("updates").join("skipped.txt")))
}

fn skipped() -> Option<String> {
    std::fs::read_to_string(skip_file()?).ok()
}

fn skip(version: &str) {
    if let Some(path) = skip_file() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent).and_then(|_| std::fs::write(&path, version));
        }
    }
}

/// Download Setup to the per-user folder, verify it from a held handle, and open it through
/// the shell with `--update <install folder>`. Returns the Setup process and its folder.
fn download_and_open(net: &dyn net::Transport, release: &Release) -> Result<(windows::Win32::Foundation::HANDLE, tempfile::TempDir), String> {
    let url = format!("https://github.com/{REPO}/releases/download/v{}/{SETUP}", release.version);
    let verified = setup::download(net, &url, release, &setup::download_root()?)?;
    let app = std::env::current_exe().map_err(|e| e.to_string())?;
    let install = app.parent().ok_or("Cannot locate the Recall installation.")?;
    setup::launch(verified, install)
}

#[derive(Debug, Clone, PartialEq)]
pub enum NoteLine {
    Heading(String),
    Bullet(String),
    Text(String),
}

pub fn note_lines(notes: &str) -> Vec<NoteLine> {
    notes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(100)
        .map(|line| {
            let clean = |s: &str| s.replace("**", "").replace('`', "");
            if let Some(h) = line.strip_prefix('#') {
                NoteLine::Heading(clean(h.trim_start_matches('#').trim()))
            } else if let Some(b) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                NoteLine::Bullet(clean(b.trim()))
            } else {
                NoteLine::Text(clean(line))
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Offered(Release),
    Downloading(Release),
    Failed(Release, String),
}

pub struct UpdateUi {
    pub phase: Phase,
    check_at: Option<Instant>,
    rx: Option<Receiver<Msg>>,
    pub notice: Option<(bool, String)>,
    /// "Check for updates automatically" (Settings, default on).
    pub auto: bool,
    /// This build's release version (`None` in dev builds: nothing to compare).
    version: Option<&'static str>,
    net: Arc<dyn net::Transport>,
    prefs: Option<PathBuf>,
}

enum Msg {
    Checked(Result<Option<Release>, String>, bool),
    Opened(Result<(), String>),
    Closed,
}

impl UpdateUi {
    pub fn new() -> Self {
        let offscreen = crate::offscreen::enabled();
        let prefs = prefs::settings_file();
        let auto = prefs::load_auto(prefs.as_deref());
        // Real checks only in a versioned, on-screen, non-testing build.
        let armed = !offscreen && !crate::TESTING;
        let mut ui = Self::with(Arc::new(net::WinHttp), current(), auto, armed, prefs);
        if offscreen && !crate::TESTING {
            if let Some(release) = std::env::var_os("RECALL_UPDATE_PREVIEW")
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|json| offer(&json, "0.0.0").ok().flatten())
            {
                ui.phase = Phase::Offered(release);
            }
        }
        ui
    }

    /// With automatic checks off, nothing is scheduled: no request at start-up or later.
    fn with(net: Arc<dyn net::Transport>, version: Option<&'static str>, auto: bool, armed: bool, prefs: Option<PathBuf>) -> Self {
        let check_at = (auto && armed && version.is_some()).then(|| Instant::now() + FIRST_CHECK);
        Self { phase: Phase::Idle, check_at, rx: None, notice: None, auto, version, net, prefs }
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// The Settings switch: saved at once; off also cancels a pending automatic check.
    pub fn set_auto(&mut self, on: bool) {
        self.auto = on;
        if !on {
            self.check_at = None;
        }
        if let Some(file) = &self.prefs {
            if let Err(e) = prefs::save_auto(file, on) {
                self.notice = Some((false, format!("Couldn't save the update setting: {e}")));
            }
        }
    }

    /// "Check now": works whether or not automatic checks are on.
    pub fn check_now(&mut self) {
        if !crate::TESTING && !crate::offscreen::enabled() && self.rx.is_none() {
            self.start_check(true);
        }
    }

    fn start_check(&mut self, manual: bool) {
        let Some(current) = self.version else { return };
        self.check_at = None;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let net = self.net.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Checked(check(net.as_ref(), current, manual), manual));
        });
    }

    pub fn tick(&mut self) -> bool {
        if self.check_at.is_some_and(|at| Instant::now() >= at) {
            self.start_check(false);
            return true;
        }
        let Some(msg) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return false;
        };
        if !matches!(&msg, Msg::Opened(Ok(()))) {
            self.rx = None;
        }
        match msg {
            Msg::Checked(Ok(Some(release)), _) => self.phase = Phase::Offered(release),
            Msg::Checked(Ok(None), true) => {
                self.notice = Some((true, "No newer stable release is available.".into()))
            }
            Msg::Checked(Err(error), true) => {
                self.notice = Some((false, format!("Couldn't check for updates: {error}")))
            }
            Msg::Checked(_, false) => return false,
            Msg::Opened(Ok(())) => self.phase = Phase::Idle,
            Msg::Opened(Err(error)) => {
                if let Phase::Downloading(release) = &self.phase {
                    self.phase = Phase::Failed(release.clone(), error);
                }
            }
            Msg::Closed => {}
        }
        true
    }

    pub fn install(&mut self) {
        if self.rx.is_some() || crate::TESTING || crate::offscreen::enabled() {
            return;
        }
        let release = match &self.phase {
            Phase::Offered(r) | Phase::Failed(r, _) => r.clone(),
            _ => return,
        };
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let job = release.clone();
        let net = self.net.clone();
        std::thread::spawn(move || match download_and_open(net.as_ref(), &job) {
            Ok((process, folder)) => {
                let _ = tx.send(Msg::Opened(Ok(())));
                setup::wait(process);
                drop(folder);
                let _ = tx.send(Msg::Closed);
            }
            Err(error) => {
                let _ = tx.send(Msg::Opened(Err(error)));
            }
        });
        self.phase = Phase::Downloading(release);
    }

    pub fn later(&mut self) {
        if !matches!(self.phase, Phase::Downloading(_)) {
            self.phase = Phase::Idle;
        }
    }

    pub fn skip_version(&mut self) {
        if let Phase::Offered(r) | Phase::Failed(r, _) = &self.phase {
            skip(&r.version);
            self.phase = Phase::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{net::fake::FakeNet, *};

    fn fixture() -> serde_json::Value {
        serde_json::json!({
            "tag_name": "v1.10.0", "name": null, "body": null, "draft": false, "prerelease": false,
            "assets": [{"name": SETUP, "size": 3, "state": "uploaded",
                "digest": "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}]
        })
    }

    #[test]
    fn newer_stable_versions_with_verified_assets_are_offered() {
        let read = |v: serde_json::Value, current| offer(v.to_string().as_bytes(), current);
        assert_eq!(read(fixture(), "1.9.0").unwrap().unwrap().version, "1.10.0");
        assert_eq!(read(fixture(), "1.10.0").unwrap(), None);
        for key in ["draft", "prerelease"] {
            let mut bad = fixture();
            bad[key] = true.into();
            assert_eq!(read(bad, "1.9.0").unwrap(), None);
        }
        let mut bad = fixture();
        bad["tag_name"] = "v1.10.0-rc1".into();
        assert_eq!(read(bad, "1.9.0").unwrap(), None);
        let mut bad = fixture();
        bad["assets"][0]["digest"] = serde_json::Value::Null;
        assert!(read(bad, "1.9.0").is_err());
    }

    #[test]
    fn only_ancestors_of_main_pass_the_comparison() {
        assert!(on_main(br#"{"status":"ahead","behind_by":0}"#).unwrap());
        assert!(on_main(br#"{"status":"identical","behind_by":0}"#).unwrap());
        assert!(!on_main(br#"{"status":"diverged","behind_by":1}"#).unwrap());
        assert!(!on_main(br#"{"status":"behind","behind_by":1}"#).unwrap());
    }

    /// The fixture's release: Setup is the 3 bytes "abc".
    pub(super) fn fixture_release() -> Release {
        offer(fixture().to_string().as_bytes(), "1.9.0").unwrap().unwrap()
    }

    const LATEST: &str = "https://api.github.com/repos/LoneBeast27/recall/releases/latest";

    /// An updater on a fake network, armed like a versioned on-screen build.
    fn ui(net: &Arc<FakeNet>, auto: bool) -> UpdateUi {
        UpdateUi::with(net.clone(), Some("1.9.0"), auto, true, None)
    }

    /// Tick until the check thread answers (or 5 s pass).
    fn settle(ui: &mut UpdateUi) {
        let end = Instant::now() + Duration::from_secs(5);
        while ui.busy() && Instant::now() < end {
            ui.tick();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn automatic_checks_off_send_nothing_until_check_now() {
        let net = Arc::new(FakeNet::default());
        let mut off = ui(&net, false);
        assert_eq!(off.check_at, None, "nothing is scheduled");
        for _ in 0..3 {
            assert!(!off.tick());
        }
        assert!(net.calls().is_empty(), "off sends no request: {:?}", net.calls());
        off.start_check(true); // what "Check now" runs
        settle(&mut off);
        assert_eq!(net.calls(), [LATEST], "Check now always works");
        assert_eq!(off.notice, Some((true, "No newer stable release is available.".into())));
    }

    #[test]
    fn automatic_checks_on_run_once_due_and_turning_off_cancels() {
        let net = Arc::new(FakeNet::default());
        let mut on = ui(&net, true);
        assert!(on.check_at.is_some(), "on: a check is scheduled after start-up");
        on.check_at = Some(Instant::now());
        assert!(on.tick());
        settle(&mut on);
        assert_eq!(net.calls(), [LATEST]);
        let mut cancelled = ui(&net, true);
        cancelled.set_auto(false);
        assert_eq!(cancelled.check_at, None);
        assert!(!cancelled.auto);
        assert!(UpdateUi::with(net.clone(), None, true, true, None).check_at.is_none(), "dev builds never check");
        assert!(UpdateUi::with(net.clone(), Some("1.9.0"), true, false, None).check_at.is_none(), "tests and offscreen never check");
    }

    #[test]
    fn a_newer_release_on_main_is_offered_and_an_older_one_is_not() {
        let compare = "https://api.github.com/repos/LoneBeast27/recall/compare/v1.10.0...main?per_page=1&page=2";
        let net = FakeNet::default()
            .reply(LATEST, 200, None, fixture().to_string().as_bytes())
            .reply(compare, 200, None, br#"{"status":"ahead","behind_by":0}"#);
        assert_eq!(check(&net, "1.9.0", true).unwrap(), Some(fixture_release()));
        assert_eq!(check(&net, "1.10.0", true).unwrap(), None, "same version");
        assert_eq!(check(&net, "2.0.0", true).unwrap(), None, "never a downgrade");
        let off_main = FakeNet::default()
            .reply(LATEST, 200, None, fixture().to_string().as_bytes())
            .reply(compare, 200, None, br#"{"status":"diverged","behind_by":1}"#);
        assert!(check(&off_main, "1.9.0", true).is_err());
    }
}
