//! Official updates come from a signed manifest (`update_manifest.rs`), checked against the
//! pinned keys before anything is read or downloaded: hosts add no trust. Tiers in order
//! (`update_sources.rs`); no verified manifest is "couldn't check", never "up to date". What it
//! offers: `update_offer.rs`. HTTPS through WinHTTP to pinned hosts (`update_net.rs`); Setup is
//! checked from a held handle and opened by the shell (`update_setup.rs`); automatic checks can
//! be turned off (`update_prefs.rs`). The Repair page (`repair.rs`) still reads GitHub's API.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};

use serde::Deserialize;

#[path = "update_hotfix.rs"]
pub(crate) mod hotfix;
#[path = "update_manifest.rs"]
pub(crate) mod manifest;
#[path = "update_offer.rs"]
mod offer;
#[path = "update_sources.rs"]
pub(crate) mod sources;
#[path = "update_trust.rs"]
pub(crate) mod trust;
#[path = "update_net.rs"]
pub(crate) mod net;
#[path = "update_notes.rs"]
mod notes;
#[path = "update_prefs.rs"]
mod prefs;
#[path = "update_words.rs"]
pub mod words;
#[path = "update_progress.rs"]
pub mod progress;
#[path = "update_resume.rs"]
pub mod resume;
#[path = "update_setup.rs"]
pub(crate) mod setup;

pub use notes::{NoteLine, note_lines};
pub use resume::{RecorderState, Resume};
use offer::{check, download_any};
pub(crate) use offer::{NOT_THIS_VERSION, TOO_OLD, this_version};

pub const REPO: &str = "LoneBeast27/recall";
pub(crate) const SETUP: &str = "RecallSetup.exe";
/// The confirm step's text (the maintainer's rule: an update says what happens to a recording).
pub const CONFIRM_TEXT: &str = "Updating closes Recall. A recording you started stops and is saved first. The replay buffer is not saved. Then Recall reopens.";
pub(crate) const MAX_SETUP: u64 = 128 * 1024 * 1024;
/// The first automatic check waits this long after start-up.
const FIRST_CHECK: Duration = Duration::from_secs(8);

pub fn current() -> Option<&'static str> {
    option_env!("RECALL_VERSION").filter(|v| version(v).is_some())
}

pub(crate) fn version(text: &str) -> Option<[u64; 3]> {
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
pub(crate) struct ReleaseJson {
    pub(crate) tag_name: String,
    name: Option<String>,
    body: Option<String>,
    pub(crate) draft: bool,
    pub(crate) prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub title: String,
    pub notes: String,
    pub size: u64,
    pub(crate) digest: String,
    /// A same-version hotfix: its source commit (`update_hotfix.rs`). `None`: a newer version.
    pub hotfix: Option<String>,
    /// Where Setup can be downloaded, tried in order; any is safe (size and SHA-256 are checked).
    pub(crate) urls: Vec<String>,
}

fn offer(json: &[u8], current: &str) -> Result<Option<Release>, String> {
    Ok(candidate(json, current, false)?.filter(|(_, same)| !same).map(|(r, _)| r))
}

/// The latest release when it is newer than `current`, or (with `same_ok`) the same version
/// (true: a possible hotfix, checked by build in `check`). Never an older version.
fn candidate(json: &[u8], current: &str, same_ok: bool) -> Result<Option<(Release, bool)>, String> {
    let r: ReleaseJson =
        serde_json::from_slice(json).map_err(|e| format!("The release information could not be read: {e}"))?;
    let candidate = r.tag_name.strip_prefix('v').unwrap_or_default().to_owned();
    let Some(candidate_version) = version(&candidate) else {
        return Ok(None);
    };
    let Some(ours) = version(current) else { return Ok(None) };
    let same = candidate_version == ours;
    if r.draft || r.prerelease || candidate_version < ours || (same && !same_ok) {
        return Ok(None);
    }
    release_of(r, &candidate).map(|release| Some((release, same)))
}

/// The release's verified Setup asset (name, upload state, SHA-256, size).
pub(crate) fn release_of(r: ReleaseJson, candidate: &str) -> Result<Release, String> {
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
        .ok_or("The release has no valid Setup checksum.")?;
    if asset.size == 0 || asset.size > MAX_SETUP {
        return Err("The reported Setup size is invalid.".into());
    }
    Ok(Release {
        version: candidate.into(),
        title: r
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Recall {candidate}")),
        notes: r.body.unwrap_or_default(),
        size: asset.size,
        digest: digest.to_ascii_lowercase(),
        hotfix: None,
        urls: vec![format!("https://github.com/{REPO}/releases/download/v{candidate}/{SETUP}")],
    })
}


fn skip_file() -> Option<PathBuf> {
    crate::user_dirs::local_appdata().map(|p| crate::user_dirs::checked(p.join("Recall").join("updates").join("skipped.txt")))
}

pub(super) fn skipped() -> Option<String> {
    std::fs::read_to_string(skip_file()?).ok()
}

fn skip(version: &str) {
    if let Some(path) = skip_file() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent).and_then(|_| std::fs::write(&path, version));
        }
    }
}

/// The hand-off: download Setup to the per-user folder and verify it from a held handle, write
/// the resume file, then open Setup through the shell with `--update <install> --resume <file>`.
/// Setup quits the recorder (saving first); this app quits itself (`take_quit`).
pub(crate) fn hand_off(net: &dyn net::Transport, release: &Release, install: &std::path::Path, resume: &Resume) -> Result<(), String> {
    let updates = resume::updates_dir().ok_or("LOCALAPPDATA is not set.")?;
    let verified = download_any(net, release, &setup::download_root()?)?;
    net::with_progress(|p| p.reach(progress::Step::Opening));
    // A hotfix: the reopened app checks it became this build (`after_update`).
    let resume = Resume { expect_revision: release.hotfix.clone(), ..resume.clone() };
    let file = resume.write_in(&updates)?;
    let (process, folder) = setup::launch(verified, install, Some(&file), release.hotfix.as_deref())?;
    // Setup runs from this folder: it stays (removed by a later start, `resume::sweep_downloads`).
    let _ = folder.keep();
    setup::close(process);
    Ok(())
}

/// The installed program's folder.
pub(crate) fn install_dir() -> Result<PathBuf, String> {
    let app = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(app.parent().ok_or("Cannot locate the Recall installation.")?.to_path_buf())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Offered(Release),
    /// "Updating closes Recall…": nothing is downloaded until "Save and update".
    Confirm(Release),
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
    /// This build's source commit (`None` in dev and local builds: no same-version hotfixes).
    revision: Option<&'static str>,
    net: Arc<dyn net::Transport>,
    /// The pinned keys, the tiers and the trust file (tests give their own).
    pub(crate) sources: Arc<sources::Sources>,
    /// The offer is an unsigned preview (`RECALL_UPDATE_PREVIEW`): Install does nothing.
    preview_only: bool,
    prefs: Option<PathBuf>,
    /// Setup has the update: the app quits itself (Root's poll).
    quit: bool,
    /// A hotfix (`version+commit`) that was installed but did not become this build: not offered
    /// again at start-up, also after a restart (`refused.txt`); "Check now" still shows it.
    refused: Option<String>,
    /// "Check now" or the Setup hand-off in flight: what the bar shows (none for the silent
    /// automatic check, which draws nothing).
    progress: Option<Arc<progress::Progress>>,
    /// Recall's window is in front (`set_front`): the bar animates only then. Behind or
    /// minimised it neither wakes the pass nor asks for a redraw; the activation wake draws
    /// it again at its real position.
    front: bool,
}

/// How often the bar redraws while an update step runs (only then).
const BAR_FRAME: Duration = Duration::from_millis(33);

enum Msg {
    Checked(Result<Option<Release>, String>, bool),
    Opened(Result<(), String>),
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
                // An unsigned layout preview: its Install never runs, whatever the build.
                ui.preview_only = true;
            }
            ui.preview_progress();
        }
        ui
    }

    /// Layout checks (offscreen only): `RECALL_UPDATE_PREVIEW_BYTES=<n>` shows the offered
    /// release downloading with `n` bytes saved; `RECALL_UPDATE_PREVIEW_ANSWERS=<n>` shows
    /// "Check now" with `n` answers in. Nothing is sent or downloaded.
    fn preview_progress(&mut self) {
        let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
        if let (Some(bytes), Phase::Offered(release)) = (number("RECALL_UPDATE_PREVIEW_BYTES"), &self.phase) {
            let bar = progress::Progress::install(release.size, progress::timings());
            bar.bytes(bytes);
            self.progress = Some(bar);
            self.phase = Phase::Downloading(release.clone());
        } else if let Some(answers) = number("RECALL_UPDATE_PREVIEW_ANSWERS") {
            let bar = progress::Progress::check(progress::timings());
            (0..answers).for_each(|_| bar.answered());
            self.progress = Some(bar);
        }
    }

    /// With automatic checks off, nothing is scheduled: no request at start-up or later.
    fn with(net: Arc<dyn net::Transport>, version: Option<&'static str>, auto: bool, armed: bool, prefs: Option<PathBuf>) -> Self {
        let check_at = (auto && armed && version.is_some()).then(|| Instant::now() + FIRST_CHECK);
        Self { phase: Phase::Idle, check_at, rx: None, notice: None, auto, version, revision: hotfix::revision(), net, sources: Arc::new(sources::Sources::pinned()), preview_only: false, prefs, quit: false, refused: hotfix::load_refused(), progress: None, front: true }
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    /// What the progress bar shows now (a manual check or the hand-off).
    pub fn progress_view(&self) -> Option<progress::View> {
        self.progress.as_ref().map(|p| p.view(Instant::now()))
    }

    /// The next bar frame while it moves: the UI pass wakes then.
    pub fn next_frame(&self) -> Option<Instant> {
        self.progress.as_ref().filter(|_| self.front).map(|_| Instant::now() + BAR_FRAME)
    }

    /// Whether Recall's window is in front (Root sets it before each `tick`).
    pub fn set_front(&mut self, front: bool) {
        self.front = front;
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
        let revision = self.revision;
        self.check_at = None;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let net = self.net.clone();
        let sources = self.sources.clone();
        self.progress = manual.then(|| progress::Progress::check(progress::timings()));
        let bar = self.progress.clone();
        std::thread::spawn(move || {
            net::track(bar);
            let _ = tx.send(Msg::Checked(check(net.as_ref(), current, revision, manual, &sources), manual));
            crate::services::ui_wake::wake();
        });
    }

    /// When the next automatic check is due (the UI pass wakes then).
    pub fn next_check(&self) -> Option<Instant> {
        self.check_at
    }

    pub fn tick(&mut self) -> bool {
        if self.check_at.is_some_and(|at| Instant::now() >= at) {
            self.start_check(false);
            return true;
        }
        let Some(msg) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            // A moving bar draws each frame in front; nothing else changed.
            return self.progress.is_some() && self.front;
        };
        self.rx = None;
        // The local timing record (`update_progress::row`), then the bar ends, except on a
        // successful hand-off, where it stays until the app closes.
        if let Some(bar) = &self.progress {
            if !crate::TESTING && matches!(msg, Msg::Checked(Ok(_), _) | Msg::Opened(Ok(()))) {
                if let Some(dir) = resume::updates_dir() {
                    bar.record(&dir.join("timings.jsonl"));
                }
            }
        }
        if !matches!(msg, Msg::Opened(Ok(()))) {
            self.progress = None;
        }
        match msg {
            Msg::Checked(Ok(Some(release)), false) if release.hotfix.is_some() && Some(hotfix::skip_key(&release)) == self.refused => return false,
            Msg::Checked(Ok(Some(release)), _) => self.phase = Phase::Offered(release),
            Msg::Checked(Ok(None), true) => {
                self.notice = Some((true, words::UP_TO_DATE.into()))
            }
            Msg::Checked(Err(error), true) => {
                self.notice = Some((false, words::failure(&error).into()))
            }
            Msg::Checked(_, false) => return false,
            Msg::Opened(Ok(())) => {
                self.phase = Phase::Idle;
                self.quit = true;
            }
            Msg::Opened(Err(error)) => {
                if let Phase::Downloading(release) = &self.phase {
                    self.phase = Phase::Failed(release.clone(), error);
                }
            }

        }
        true
    }

    /// "Update now" / "Try again": the confirm step first, nothing downloaded yet.
    pub fn confirm(&mut self) {
        if let Phase::Offered(r) | Phase::Failed(r, _) = &self.phase {
            self.phase = Phase::Confirm(r.clone());
        }
    }

    /// "Cancel" on the confirm step: back to the offer, nothing downloaded.
    pub fn cancel_confirm(&mut self) {
        if let Phase::Confirm(r) = &self.phase {
            self.phase = Phase::Offered(r.clone());
        }
    }

    /// "Save and update": only from the confirm step. `resume` is what the app shows now.
    pub fn install(&mut self, resume: Resume) {
        if crate::TESTING || crate::offscreen::enabled() || self.preview_only {
            return;
        }
        let net = self.net.clone();
        self.install_with(move |release| hand_off(net.as_ref(), release, &install_dir()?, &resume));
    }

    /// [`install`](Self::install) with the hand-off given (tests pass their own).
    pub(crate) fn install_with(&mut self, job: impl FnOnce(&Release) -> Result<(), String> + Send + 'static) {
        let Phase::Confirm(release) = &self.phase else { return };
        if self.rx.is_some() {
            return;
        }
        let release = release.clone();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let task = release.clone();
        self.progress = Some(progress::Progress::install(release.size, progress::timings()));
        let bar = self.progress.clone();
        std::thread::spawn(move || {
            net::track(bar);
            let _ = tx.send(Msg::Opened(job(&task)));
            crate::services::ui_wake::wake();
        });
        self.phase = Phase::Downloading(release);
    }

    /// The start after an update: a hotfix whose build this app is not (Setup refused it, or
    /// the release's Setup is another build) is not offered again at start-up, kept on disk.
    pub fn after_update(&mut self, expected: Option<&str>) {
        let (Some(expected), Some(version)) = (expected, self.version) else { return };
        if self.revision != Some(expected) {
            let key = format!("{version}+{expected}");
            hotfix::save_refused(&key);
            self.refused = Some(key);
        }
    }

    /// Setup has the update: quit now (true once).
    pub fn take_quit(&mut self) -> bool {
        std::mem::take(&mut self.quit)
    }

    pub fn later(&mut self) {
        if !matches!(self.phase, Phase::Downloading(_)) {
            self.phase = Phase::Idle;
        }
    }

    pub fn skip_version(&mut self) {
        if let Phase::Offered(r) | Phase::Failed(r, _) | Phase::Confirm(r) = &self.phase {
            skip(&hotfix::skip_key(r));
            self.phase = Phase::Idle;
        }
    }
}

#[cfg(test)]
#[path = "update_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "update_hotfix_tests.rs"]
mod hotfix_tests;

#[cfg(test)]
#[path = "update_signed_tests.rs"]
pub(crate) mod signed_tests;

#[cfg(test)]
#[path = "update_compatibility_tests.rs"]
mod compatibility;
