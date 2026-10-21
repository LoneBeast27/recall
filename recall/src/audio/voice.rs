//! Voice-chat track target: one app's process tree, captured by process
//! loopback (include-tree mode) so the app's children are recorded too —
//! Discord plays voice from a child `Discord.exe` (its audio service).
//!
//! `RECALL_AUDIO_VOICE_PID` wins; otherwise `RECALL_AUDIO_VOICE_EXE` (e.g.
//! `Discord.exe`, `.exe` optional) names the image and the root of its
//! largest running tree is used. Neither set, or the app not running, means
//! no voice track — not an error.

use std::collections::HashMap;
use std::ffi::c_void;

use super::{parse_pid, LoopbackTarget};

pub const PID_ENV: &str = "RECALL_AUDIO_VOICE_PID";
pub const EXE_ENV: &str = "RECALL_AUDIO_VOICE_EXE";

/// One row of a process snapshot.
#[derive(Clone, Debug)]
pub struct ProcEntry {
    pub pid: u32,
    pub parent: u32,
    /// Image file name, e.g. `Discord.exe`.
    pub exe: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    Target(LoopbackTarget),
    /// No voice track, with the reason to log.
    Off(String),
}

pub fn resolve_from_env(game_pid: Option<u32>) -> Result<Resolved, String> {
    resolve(|key| std::env::var(key).ok(), snapshot_processes, game_pid)
}

/// `resolve_from_env` with injected env lookup and process listing (tests).
pub fn resolve(
    get: impl Fn(&str) -> Option<String>,
    list: impl FnOnce() -> Result<Vec<ProcEntry>, String>,
    game_pid: Option<u32>,
) -> Result<Resolved, String> {
    let set = |key: &str| get(key).filter(|v| !v.trim().is_empty());
    let target = if let Some(raw) = set(PID_ENV) {
        let pid = parse_pid(PID_ENV, &raw)?;
        LoopbackTarget {
            pid,
            label: format!("process {pid} (include tree)"),
        }
    } else if let Some(raw) = set(EXE_ENV) {
        let exe = exe_name(&raw);
        let procs = list()?;
        let Some(pid) = tree_root(&procs, &exe) else {
            return Ok(Resolved::Off(format!("{exe} is not running")));
        };
        LoopbackTarget {
            pid,
            label: format!("{exe} pid {pid} (include tree)"),
        }
    } else {
        return Ok(Resolved::Off(format!(
            "no voice app chosen (set {EXE_ENV}=Discord.exe or {PID_ENV})"
        )));
    };
    if Some(target.pid) == game_pid {
        return Ok(Resolved::Off(format!(
            "voice app pid {} is the game process (already on the Game track)",
            target.pid
        )));
    }
    Ok(Resolved::Target(target))
}

/// File name with `.exe`: `Discord` / `C:\..\Discord.exe` -> `Discord.exe`.
fn exe_name(raw: &str) -> String {
    let raw = raw.trim();
    let name = raw.rsplit(['\\', '/']).next().unwrap_or(raw);
    if name.to_ascii_lowercase().ends_with(".exe") {
        name.to_string()
    } else {
        format!("{name}.exe")
    }
}

/// Root PID of the largest tree of processes whose image is `exe`
/// (case-insensitive). A root is a match whose parent is not itself a match;
/// ties go to the lowest PID. Walks are bounded, so stale parent IDs that
/// form a cycle (PID reuse) still terminate.
pub fn tree_root(procs: &[ProcEntry], exe: &str) -> Option<u32> {
    let want = exe_name(exe).to_lowercase();
    let parents: HashMap<u32, u32> = procs
        .iter()
        .filter(|p| p.exe.to_lowercase() == want)
        .map(|p| (p.pid, p.parent))
        .collect();
    let root_of = |mut pid: u32| {
        for _ in 0..parents.len() {
            match parents.get(&pid) {
                Some(&parent) if parent != pid && parents.contains_key(&parent) => pid = parent,
                _ => break,
            }
        }
        pid
    };
    let mut sizes: HashMap<u32, usize> = HashMap::new();
    for &pid in parents.keys() {
        *sizes.entry(root_of(pid)).or_default() += 1;
    }
    sizes
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|(pid, _)| pid)
}

/// `PROCESSENTRY32W` (tlhelp32.h). Declared here because the `windows`
/// crate's ToolHelp feature is not enabled for this crate.
#[repr(C)]
struct ProcessEntry32W {
    size: u32,
    usage: u32,
    pid: u32,
    default_heap_id: usize,
    module_id: u32,
    threads: u32,
    parent_pid: u32,
    priority_base: i32,
    flags: u32,
    exe_file: [u16; 260],
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
    fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// Every running process (pid, parent, image name).
pub fn snapshot_processes() -> Result<Vec<ProcEntry>, String> {
    const TH32CS_SNAPPROCESS: u32 = 0x2;
    // SAFETY: plain Win32 calls. The snapshot handle is checked before use
    // and closed exactly once; `entry` is a correctly sized, zeroed
    // PROCESSENTRY32W whose `size` field is set as the API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot as isize == -1 {
            return Err(format!(
                "process snapshot failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut out = Vec::new();
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry.exe_file.iter().position(|&c| c == 0).unwrap_or(260);
            out.push(ProcEntry {
                pid: entry.pid,
                parent: entry.parent_pid,
                exe: String::from_utf16_lossy(&entry.exe_file[..len]),
            });
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        Ok(out)
    }
}

#[cfg(test)]
#[path = "voice_tests.rs"]
mod tests;
