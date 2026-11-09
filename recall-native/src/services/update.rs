//! Official updates from stable releases whose tags are reachable from GitHub main.
use std::{
    fs::File,
    io,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const REPO: &str = "LoneBeast27/recall";
const SETUP: &str = "RecallSetup.exe";
const MAX_SETUP: u64 = 128 * 1024 * 1024;
const API_LIMIT: u64 = 1024 * 1024;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

fn curl(url: &str, file: Option<&Path>, limit: u64) -> Result<(u16, Vec<u8>), String> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut system = [0u16; 260];
    let len = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if len == 0 || len >= system.len() {
        return Err("Cannot locate Windows curl.exe.".into());
    }
    let exe = PathBuf::from(String::from_utf16_lossy(&system[..len])).join("curl.exe");
    let mut cmd = Command::new(exe);
    cmd.creation_flags(CREATE_NO_WINDOW)
        .args([
            "-q",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "5",
            "--connect-timeout",
            "10",
            "--max-time",
            if file.is_some() { "180" } else { "20" },
            "--user-agent",
            "Recall-updater",
            "--write-out",
            "\n%{http_code}",
            "--max-filesize",
        ])
        .arg(limit.to_string());
    if let Some(path) = file {
        cmd.arg("--output").arg(path);
    }
    let output = cmd
        .arg(url)
        .output()
        .map_err(|e| format!("Windows download failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Download failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let split = output
        .stdout
        .iter()
        .rposition(|&b| b == b'\n')
        .ok_or("Missing HTTP status.")?;
    let status = std::str::from_utf8(&output.stdout[split + 1..])
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or("Invalid HTTP status.")?;
    Ok((status, output.stdout[..split].to_vec()))
}

fn api(path: &str) -> Result<(u16, Vec<u8>), String> {
    curl(
        &format!("https://api.github.com/repos/{REPO}/{path}"),
        None,
        API_LIMIT,
    )
}

fn check(current: &str, manual: bool) -> Result<Option<Release>, String> {
    let (status, body) = api("releases/latest")?;
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
    let (status, body) = api(&format!("compare/v{}...main?per_page=1&page=2", release.version))?;
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

fn verify_setup(path: &Path, release: &Release) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| format!("Setup is unavailable: {e}"))?;
    let mut hash = Sha256::new();
    let bytes =
        io::copy(&mut file, &mut hash).map_err(|e| format!("Setup could not be read: {e}"))?;
    if bytes != release.size || format!("{:x}", hash.finalize()) != release.digest {
        return Err("Setup did not match the GitHub checksum and size.".into());
    }
    Ok(())
}

fn download_and_open(release: &Release) -> Result<(Child, tempfile::TempDir), String> {
    let folder = tempfile::Builder::new()
        .prefix("Recall-Update-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let path = folder.path().join(SETUP);
    let url = format!(
        "https://github.com/{REPO}/releases/download/v{}/{SETUP}",
        release.version
    );
    let (status, _) = curl(&url, Some(&path), release.size)?;
    if status != 200 {
        return Err(format!("GitHub download returned HTTP {status}."));
    }
    verify_setup(&path, release)?;
    let app = std::env::current_exe().map_err(|e| e.to_string())?;
    let install = app
        .parent()
        .ok_or("Cannot locate the Recall installation.")?;
    let child = Command::new(path)
        .arg("--update")
        .arg(install)
        .spawn()
        .map_err(|e| format!("Windows could not open Setup: {e}"))?;
    Ok((child, folder))
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
}

enum Msg {
    Checked(Result<Option<Release>, String>, bool),
    Opened(Result<(), String>),
    Closed,
}

impl UpdateUi {
    pub fn new() -> Self {
        let offscreen = crate::offscreen::enabled();
        if offscreen && !crate::TESTING {
            if let Some(release) = std::env::var_os("RECALL_UPDATE_PREVIEW")
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|json| offer(&json, "0.0.0").ok().flatten())
            {
                return Self {
                    phase: Phase::Offered(release),
                    check_at: None,
                    rx: None,
                    notice: None,
                };
            }
        }
        let check_at = (current().is_some() && !offscreen && !crate::TESTING)
            .then(|| Instant::now() + Duration::from_secs(8));
        Self {
            phase: Phase::Idle,
            check_at,
            rx: None,
            notice: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    pub fn check_now(&mut self) {
        if !crate::TESTING && !crate::offscreen::enabled() && self.rx.is_none() {
            self.start_check(true);
        }
    }

    fn start_check(&mut self, manual: bool) {
        let Some(current) = current() else { return };
        self.check_at = None;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Checked(check(current, manual), manual));
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
        std::thread::spawn(move || match download_and_open(&job) {
            Ok((mut child, folder)) => {
                let _ = tx.send(Msg::Opened(Ok(())));
                let _ = child.wait();
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
    use super::*;

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

    #[test]
    fn a_tampered_download_never_passes_verification() {
        let r = offer(fixture().to_string().as_bytes(), "1.9.0")
            .unwrap()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(SETUP);
        std::fs::write(&exe, b"abc").unwrap();
        verify_setup(&exe, &r).unwrap();
        std::fs::write(&exe, b"abd").unwrap();
        assert!(verify_setup(&exe, &r).is_err());
        std::fs::write(&exe, b"ab").unwrap();
        assert!(verify_setup(&exe, &r).is_err());
    }
}
