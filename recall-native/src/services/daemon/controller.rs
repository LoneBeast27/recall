//! Locating and launching the recording controller (`recall-hdr.exe --record`).
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};

pub(super) const CONTROLLER_EXE: &str = "recall-hdr.exe";
const CONTROLLER_ARG: &str = "--record";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

pub(super) fn start() -> Result<bool, String> {
    if process_running(CONTROLLER_EXE)? {
        return Ok(false);
    }
    let executable = locate()?;
    spawn_detached(&executable)?;
    Ok(true)
}

/// `RECALL_CONTROLLER`, then a sibling of this executable, then the repo's dev build.
pub(super) fn locate() -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os("RECALL_CONTROLLER") {
        let path = PathBuf::from(value);
        return if path.is_file() { Ok(path) } else { Err("RECALL_CONTROLLER does not name recall-hdr.exe".into()) };
    }
    let exe = std::env::current_exe().map_err(|error| format!("locate Recall: {error}"))?;
    if let Some(sibling) = exe.parent().map(|dir| dir.join(CONTROLLER_EXE)).filter(|path| path.is_file()) {
        return Ok(sibling);
    }
    repo_controller(&exe).ok_or_else(|| "recall-hdr.exe not found. Build recall-hdr or set RECALL_CONTROLLER.".into())
}

/// Walks up from `start` to the workspace whose members include `recall-hdr`, then
/// prefers `target-dev/release` over `target-dev/debug`.
pub(super) fn repo_controller(start: &Path) -> Option<PathBuf> {
    let root = start.ancestors().find(|dir| is_recall_workspace(&dir.join("Cargo.toml")))?;
    ["release", "debug"].iter().map(|profile| root.join("target-dev").join(profile).join(CONTROLLER_EXE)).find(|path| path.is_file())
}

fn is_recall_workspace(manifest: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(manifest) else { return false };
    workspace_lists_controller(&text)
}

/// True when the manifest has a `[workspace]` whose `members` array names "recall-hdr".
pub(super) fn workspace_lists_controller(manifest: &str) -> bool {
    let Some(section) = manifest.split("[workspace]").nth(1) else { return false };
    let Some(members) = section.split("members").nth(1) else { return false };
    let Some(open) = members.find('[') else { return false };
    let Some(close) = members[open..].find(']') else { return false };
    members[open + 1..open + close].split(',').any(|member| member.trim().trim_matches('"') == "recall-hdr")
}

/// Hidden, no console, own process group; escapes our job object when allowed so the
/// recorder outlives this window.
fn spawn_detached(executable: &Path) -> Result<(), String> {
    let launch = |flags: u32| {
        let mut command = Command::new(executable);
        command.arg(CONTROLLER_ARG).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(flags);
        if let Some(dir) = executable.parent() {
            command.current_dir(dir);
        }
        command.spawn()
    };
    let base = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    match launch(base | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(_) => Ok(()),
        // Access denied when our job forbids breakaway; launch inside it instead.
        Err(_) => launch(base).map(|_| ()).map_err(|error| format!("start recall-hdr: {error}")),
    }
}

/// Whether any process with this executable name runs (case-insensitive).
pub(super) fn process_running(exe_name: &str) -> Result<bool, String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(|error| format!("list processes: {error}"))?;
    let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut found = false;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let len = entry.szExeFile.iter().position(|unit| *unit == 0).unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(exe_name) {
            found = true;
            break;
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    Ok(found)
}
