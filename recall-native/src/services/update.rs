//! In-app updates from GitHub Releases (BakkesMod-style: check on launch, show the version and
//! its changelog, one click to install).
//!
//! Release gating (Rohan, 2026-11-06): only builds cut from `main` by `scripts/release.ps1`
//! carry a version (`RECALL_VERSION` at compile time). Dev builds carry none and never check,
//! so a tweak on the dev lane never turns into an update. GitHub's `releases/latest` skips
//! drafts and pre-releases, so only a published full release is ever offered.
//!
//! Network and hashing go through tools Windows ships (`curl.exe`, PowerShell): no new crates.
//! A private repo needs a token: `RECALL_GITHUB_TOKEN`, else the `gh` CLI login on this PC.
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

use serde::Deserialize;

/// owner/name of the repository whose releases are offered.
pub const REPO: &str = match option_env!("RECALL_UPDATE_REPO") {
    Some(repo) => repo,
    None => "LoneBeast27/recall",
};
/// The release zip and its checksum file, as `scripts/release.ps1` names them.
pub const ZIP_PREFIX: &str = "Recall-";
pub const ZIP_SUFFIX: &str = "-win64.zip";
pub const SUMS: &str = "SHA256SUMS.txt";
/// The check waits this long after launch: it is never on the startup path.
pub const CHECK_DELAY: Duration = Duration::from_secs(8);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// This build's version; `None` for a dev build (no update check).
pub fn current() -> Option<&'static str> {
    option_env!("RECALL_VERSION").filter(|v| parse_version(v).is_some())
}

/// "v1.2.3" or "1.2.3" (up to 3 numeric parts; missing parts are 0).
pub fn parse_version(text: &str) -> Option<[u64; 3]> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let core = text.split(['-', '+']).next()?;
    let mut out = [0u64; 3];
    let mut parts = 0;
    for (i, part) in core.split('.').enumerate() {
        if i >= 3 {
            return None;
        }
        out[i] = part.parse().ok()?;
        parts += 1;
    }
    (parts > 0).then_some(out)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Asset {
    pub name: String,
    /// API url: with `Accept: application/octet-stream` it downloads (also for a private repo).
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct ReleaseJson {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// A release this build can install.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub title: String,
    pub notes: String,
    pub zip: Asset,
    pub sums: Asset,
}

/// The `releases/latest` answer as an offer, or why it is not one.
pub fn offer(json: &str, current: &str) -> Result<Option<Release>, String> {
    let r: ReleaseJson = serde_json::from_str(json).map_err(|e| format!("unreadable release ({e})"))?;
    if r.draft || r.prerelease {
        return Ok(None);
    }
    let version = r.tag_name.trim().trim_start_matches(['v', 'V']).to_owned();
    if parse_version(&version).is_none() || !is_newer(&version, current) {
        return Ok(None);
    }
    let want = format!("{ZIP_PREFIX}{version}{ZIP_SUFFIX}");
    let zip = r.assets.iter().find(|a| a.name == want).cloned().ok_or_else(|| format!("release {version} has no {want}"))?;
    let sums = r.assets.iter().find(|a| a.name == SUMS).cloned().ok_or_else(|| format!("release {version} has no {SUMS}"))?;
    let title = r.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("Recall {version}"));
    Ok(Some(Release { version, title, notes: r.body.unwrap_or_default(), zip, sums }))
}

/// One changelog line as the card draws it.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteLine {
    Heading(String),
    Bullet(String),
    Text(String),
}

/// Markdown release notes as plain lines: `#` headings, `-`/`*` bullets, text; blanks dropped.
pub fn note_lines(notes: &str) -> Vec<NoteLine> {
    notes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let clean = |s: &str| s.replace("**", "").replace('`', "");
            if let Some(h) = l.strip_prefix('#') {
                NoteLine::Heading(clean(h.trim_start_matches('#').trim()))
            } else if let Some(b) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                NoteLine::Bullet(clean(b.trim()))
            } else {
                NoteLine::Text(clean(l))
            }
        })
        .collect()
}

pub fn updates_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Recall").join("updates")
}

fn skip_file() -> PathBuf {
    updates_dir().join("skipped.txt")
}

/// The version the user chose to skip, if any.
pub fn skipped() -> Option<String> {
    std::fs::read_to_string(skip_file()).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

pub fn skip(version: &str) {
    let _ = std::fs::create_dir_all(updates_dir());
    let _ = std::fs::write(skip_file(), version);
}

fn hidden(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// GitHub token: `RECALL_GITHUB_TOKEN`, else `gh auth token`; `None` works for a public repo.
fn token() -> Option<String> {
    if let Some(t) = std::env::var("RECALL_GITHUB_TOKEN").ok().filter(|t| !t.trim().is_empty()) {
        return Some(t.trim().to_owned());
    }
    let out = hidden("gh").args(["auth", "token"]).output().ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !t.is_empty()).then_some(t)
}

/// `curl.exe` with headers in a file (`-H @file`), so the token never shows on a command line.
fn curl(url: &str, accept: &str, out: Option<&Path>) -> Result<Vec<u8>, String> {
    let dir = updates_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let headers = dir.join(format!("headers-{}.txt", std::process::id()));
    let mut text = format!("Accept: {accept}\r\nUser-Agent: Recall-updater\r\nX-GitHub-Api-Version: 2022-11-28\r\n");
    if let Some(t) = token() {
        text.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    std::fs::write(&headers, text).map_err(|e| e.to_string())?;
    let mut command = hidden("curl.exe");
    command.args(["-sSfL", "--max-time", "600", "-H"]).arg(format!("@{}", headers.display()));
    if let Some(out) = out {
        command.arg("-o").arg(out);
    }
    let result = command.arg(url).output();
    let _ = std::fs::remove_file(&headers);
    let output = result.map_err(|e| format!("curl.exe: {e}"))?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if why.is_empty() { format!("download failed ({})", output.status) } else { why });
    }
    Ok(output.stdout)
}

/// Asks GitHub for the latest full release; `Ok(None)` = up to date (or skipped).
pub fn check(current: &str) -> Result<Option<Release>, String> {
    let body = curl(&format!("https://api.github.com/repos/{REPO}/releases/latest"), "application/vnd.github+json", None)?;
    let offer = offer(&String::from_utf8_lossy(&body), current)?;
    Ok(offer.filter(|r| skipped().as_deref() != Some(r.version.as_str())))
}

/// Downloads the zip and its checksums, then starts the updater script, which waits for this
/// app to exit, verifies the checksum, swaps the install all-or-nothing and reopens Recall.
pub fn download_and_launch(release: &Release) -> Result<(), String> {
    let dir = updates_dir().join(&release.version);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let zip = dir.join(&release.zip.name);
    let sums = dir.join(SUMS);
    curl(&release.zip.url, "application/octet-stream", Some(&zip))?;
    curl(&release.sums.url, "application/octet-stream", Some(&sums))?;
    let install = std::env::current_exe().map_err(|e| e.to_string())?.parent().ok_or("no install folder")?.to_path_buf();
    // Run a copy: the swap replaces the installed script while it runs.
    let script = dir.join("apply-update.ps1");
    std::fs::copy(install.join("apply-update.ps1"), &script).map_err(|e| format!("apply-update.ps1 missing from the install ({e})"))?;
    hidden("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-Zip")
        .arg(&zip)
        .arg("-Sums")
        .arg(&sums)
        .arg("-InstallDir")
        .arg(&install)
        .arg("-AppPid")
        .arg(std::process::id().to_string())
        .arg("-Version")
        .arg(&release.version)
        .spawn()
        .map_err(|e| format!("could not start the updater ({e})"))?;
    Ok(())
}

/// What the updater script reported about the last install (written to `last.txt`).
pub fn take_last_result() -> Option<Result<String, String>> {
    let path = updates_dir().join("last.txt");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let (status, rest) = text.trim().split_once(' ')?;
    match status {
        "ok" => Some(Ok(rest.trim().to_owned())),
        "failed" => Some(Err(rest.trim().to_owned())),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Offered(Release),
    Downloading(Release),
    Failed(Release, String),
}

/// The update card's state; `Root` polls it every tick.
pub struct UpdateUi {
    pub phase: Phase,
    check_at: Option<Instant>,
    rx: Option<Receiver<Msg>>,
    /// The download finished and the updater is waiting for the app to quit.
    pub quit_now: bool,
}

enum Msg {
    Checked(Result<Option<Release>, String>),
    Launched(Result<(), String>),
}

impl UpdateUi {
    pub fn new() -> Self {
        // Layout checks (offscreen only): RECALL_UPDATE_PREVIEW=<release json> shows that offer.
        if crate::offscreen::enabled() {
            if let Some(json) = std::env::var_os("RECALL_UPDATE_PREVIEW").and_then(|p| std::fs::read_to_string(p).ok()) {
                if let Ok(Some(release)) = offer(&json, "0.0.0") {
                    return Self { phase: Phase::Offered(release), check_at: None, rx: None, quit_now: false };
                }
            }
        }
        let off = std::env::var_os("RECALL_NO_UPDATE_CHECK").is_some() || crate::offscreen::enabled();
        Self { phase: Phase::Idle, check_at: (current().is_some() && !off).then(|| Instant::now() + CHECK_DELAY), rx: None, quit_now: false }
    }

    /// Starts the check when due and takes finished background results; true = redraw.
    pub fn tick(&mut self) -> bool {
        if self.check_at.is_some_and(|at| Instant::now() >= at) {
            self.check_at = None;
            if let Some(current) = current() {
                let (tx, rx) = channel();
                self.rx = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Checked(check(current)));
                });
            }
        }
        let Some(msg) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) else { return false };
        self.rx = None;
        match (msg, &self.phase) {
            (Msg::Checked(Ok(Some(release))), _) => self.phase = Phase::Offered(release),
            // A failed check stays silent: being offline is not worth a toast.
            (Msg::Checked(_), _) => return false,
            (Msg::Launched(Ok(())), _) => self.quit_now = true,
            (Msg::Launched(Err(error)), Phase::Downloading(release)) => self.phase = Phase::Failed(release.clone(), error),
            (Msg::Launched(Err(_)), _) => {}
        }
        true
    }

    pub fn install(&mut self) {
        let release = match &self.phase {
            Phase::Offered(r) | Phase::Failed(r, _) => r.clone(),
            _ => return,
        };
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let job = release.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Launched(download_and_launch(&job)));
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
    use super::*;

    fn release_json(tag: &str, prerelease: bool, assets: &[&str]) -> String {
        let assets: Vec<_> = assets
            .iter()
            .map(|n| serde_json::json!({ "name": n, "url": format!("https://api.github.com/assets/{n}"), "size": 10 }))
            .collect();
        serde_json::json!({ "tag_name": tag, "name": "", "body": "## New\n- Faster saves\n", "draft": false, "prerelease": prerelease, "assets": assets })
            .to_string()
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse_version("1.10"), Some([1, 10, 0]));
        assert_eq!(parse_version("1.2.3-rc1"), Some([1, 2, 3]));
        assert_eq!(parse_version("dev"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(!is_newer("1.2.0", "1.2.0"));
        assert!(!is_newer("garbage", "1.0.0"));
    }

    #[test]
    fn offers_only_a_newer_full_release_with_both_assets() {
        let ok = release_json("v1.1.0", false, &["Recall-1.1.0-win64.zip", "SHA256SUMS.txt"]);
        let r = offer(&ok, "1.0.0").unwrap().unwrap();
        assert_eq!(r.version, "1.1.0");
        assert_eq!(r.title, "Recall 1.1.0", "an empty release name falls back");
        assert_eq!(r.zip.name, "Recall-1.1.0-win64.zip");
        assert_eq!(offer(&ok, "1.1.0").unwrap(), None, "same version is no offer");
        assert_eq!(offer(&ok, "2.0.0").unwrap(), None, "older is no offer");
        let pre = release_json("v1.1.0", true, &["Recall-1.1.0-win64.zip", "SHA256SUMS.txt"]);
        assert_eq!(offer(&pre, "1.0.0").unwrap(), None, "pre-releases are the dev lane");
        let no_sums = release_json("v1.1.0", false, &["Recall-1.1.0-win64.zip"]);
        assert!(offer(&no_sums, "1.0.0").is_err(), "no checksum, no install");
        assert!(offer("{not json", "1.0.0").is_err());
    }

    #[test]
    fn notes_become_lines() {
        let lines = note_lines("## Features\n\n- **Island** shows saves\n* `Alt+N` faster\nPlain text\n");
        assert_eq!(
            lines,
            vec![
                NoteLine::Heading("Features".into()),
                NoteLine::Bullet("Island shows saves".into()),
                NoteLine::Bullet("Alt+N faster".into()),
                NoteLine::Text("Plain text".into()),
            ]
        );
    }

    #[test]
    fn dev_builds_never_check() {
        if option_env!("RECALL_VERSION").is_none() {
            assert_eq!(current(), None);
            assert!(UpdateUi::new().check_at.is_none());
        }
    }
}
