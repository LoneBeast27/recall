//! "Set up FPS" under Settings › Overlay: the stats island's FPS reads the present events
//! Windows itself logs, which needs the local group Performance Log Users. The installed
//! `RecallSetup.exe` (beside this exe) already has the elevated mode for that,
//! `/add-perflog-user <sid>`: it adds exactly that account and exits. The app starts it with the
//! `runas` verb, so Windows shows its own administrator prompt; nothing is hidden, no script host.
//! The group takes effect at the next sign-in, which the line then says.
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The installed Setup copy (Apps & Features runs it for an uninstall too).
pub const SETUP_EXE: &str = "RecallSetup.exe";
/// Setup's elevated mode (scripts/setup/SetupFrameTiming.cs `ElevatedArgument`).
pub const ELEVATED_ARGUMENT: &str = "/add-perflog-user";
pub const BUTTON: &str = "Set up FPS";
pub const RUNNING: &str = "Waiting for Windows…";
/// ERROR_CANCELLED: the administrator prompt was declined.
const CANCELLED: i32 = 1223;

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// In the group now; FPS shows after the next sign-in.
    Added,
    Declined,
    Failed(String),
}

impl Outcome {
    /// The line under the stats switch after a click.
    pub fn text(&self) -> String {
        match self {
            Outcome::Added => "Added to Performance Log Users. FPS shows after you sign out and back in.".into(),
            Outcome::Declined => "The administrator prompt was declined. FPS shows — until you allow it.".into(),
            Outcome::Failed(why) => format!("FPS setup failed: {why}"),
        }
    }
}

static BUSY: AtomicBool = AtomicBool::new(false);
static LAST: Mutex<Option<Outcome>> = Mutex::new(None);

/// Setup beside `exe` (an installed Recall); none in a dev build.
pub fn setup_beside(exe: &Path) -> Option<PathBuf> {
    let setup = exe.parent()?.join(SETUP_EXE);
    setup.is_file().then_some(setup)
}

/// Only a well-formed account SID goes on the elevated command line (Setup checks it again).
pub fn is_account_sid(sid: &str) -> bool {
    let Some(rest) = sid.strip_prefix("S-1-5-21-") else { return false };
    !rest.is_empty() && rest.split('-').all(|part| !part.is_empty() && part.len() <= 10 && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Setup's arguments for `sid`.
pub fn arguments(sid: &str) -> Result<String, String> {
    if !is_account_sid(sid) {
        return Err(format!("unexpected account id {sid}"));
    }
    Ok(format!("{ELEVATED_ARGUMENT} {sid}"))
}

/// Setup's exit code as the outcome.
pub fn outcome_of(code: Result<u32, i32>) -> Outcome {
    match code {
        Ok(0) => Outcome::Added,
        Ok(code) => Outcome::Failed(format!("Setup exited with code {code}")),
        Err(CANCELLED) => Outcome::Declined,
        Err(error) => Outcome::Failed(format!("Windows could not start the administrator prompt (error {error})")),
    }
}

/// Never a prompt from RecallTesting, `cargo test` or an offscreen build.
fn blocked() -> bool {
    crate::TESTING || cfg!(test) || crate::offscreen::enabled()
}

/// The installed Setup can do it (the button shows); false in dev, test and offscreen builds.
pub fn available() -> bool {
    !blocked() && std::env::current_exe().ok().as_deref().and_then(setup_beside).is_some()
}

pub fn running() -> bool {
    BUSY.load(Ordering::SeqCst)
}

/// The last click's outcome (kept: the line shows it until the app restarts).
pub fn last() -> Option<Outcome> {
    LAST.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The click: Windows' prompt, then the wait, on its own thread. Never in a test build.
pub fn start() {
    if blocked() || BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let outcome = request();
        crate::trace::player_log("fps_setup", serde_json::json!({ "outcome": format!("{outcome:?}") }));
        *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
        BUSY.store(false, Ordering::SeqCst);
    });
}

fn request() -> Outcome {
    let setup = match std::env::current_exe().ok().as_deref().and_then(setup_beside) {
        Some(setup) => setup,
        None => return Outcome::Failed(format!("{SETUP_EXE} is not in Recall's folder; run Recall Setup instead")),
    };
    let params = match current_user_sid().and_then(|sid| arguments(&sid)) {
        Ok(params) => params,
        Err(why) => return Outcome::Failed(why),
    };
    outcome_of(run_elevated(&setup, &params))
}

/// `runas` through the shell (Windows' own prompt), then wait for the elevated Setup's exit code.
/// Err is the Win32 error of the start (1223: declined).
fn run_elevated(setup: &Path, params: &str) -> Result<u32, i32> {
    use windows::Win32::Foundation::{CloseHandle, GetLastError};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    let file = HSTRING::from(setup.as_os_str());
    let params = HSTRING::from(params);
    // SAFETY: the strings outlive the call; the process handle is closed below.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = w!("runas");
        info.lpFile = PCWSTR(file.as_ptr());
        info.lpParameters = PCWSTR(params.as_ptr());
        info.nShow = SW_SHOWNORMAL.0;
        if ShellExecuteExW(&mut info).is_err() {
            return Err(GetLastError().0 as i32);
        }
        if info.hProcess.is_invalid() {
            return Err(-1);
        }
        // Setup's elevated mode only calls NetLocalGroupAddMembers; 5 minutes is a hang.
        WaitForSingleObject(info.hProcess, 5 * 60 * 1000);
        let mut code = 0u32;
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        got.map(|_| code).map_err(|_| -1)
    }
}

/// The user SID of this process's token, as `S-1-5-21-...`.
fn current_user_sid() -> Result<String, String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PWSTR;
    let mut token = HANDLE::default();
    // SAFETY: our own process handle; the token handle is closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(|e| format!("open token: {e}"))?;
    let mut needed = 0u32;
    // SAFETY: size query (fails with ERROR_INSUFFICIENT_BUFFER and sets `needed`).
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    // u64 storage keeps the TOKEN_USER (pointer-aligned) aligned.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` holds `needed` bytes.
    let got = unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), needed, &mut needed) };
    // SAFETY: opened above.
    let _ = unsafe { CloseHandle(token) };
    got.map_err(|e| format!("read token user: {e}"))?;
    // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut text = PWSTR::null();
    // SAFETY: the SID points into `buf`, alive for the call; `text` is freed with LocalFree.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }.map_err(|e| format!("format SID: {e}"))?;
    // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
    let sid = unsafe { text.to_string() }.map_err(|e| format!("SID text: {e}"));
    // SAFETY: allocated by ConvertSidToStringSidW.
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    sid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_account_sids_reach_the_elevated_command_line() {
        assert_eq!(arguments("S-1-5-21-111-222-333-1001").as_deref(), Ok("/add-perflog-user S-1-5-21-111-222-333-1001"));
        for bad in ["", "S-1-5-32-559", "S-1-5-21-", "S-1-5-21-1-2-3-1001 /uninstall", "S-1-5-21-1--2", "S-1-5-21-1-2-x"] {
            assert!(arguments(bad).is_err(), "{bad:?} refused");
        }
    }

    #[test]
    fn the_elevated_argument_matches_setup() {
        let setup = include_str!("../../../scripts/setup/SetupFrameTiming.cs");
        assert!(setup.contains(&format!("ElevatedArgument = \"{ELEVATED_ARGUMENT}\"")), "Setup's elevated mode name");
        let registry = include_str!("../../../scripts/setup/SetupRegistry.cs");
        assert!(registry.contains(&format!("UninstallerName = \"{SETUP_EXE}\"")), "the installed Setup's file name");
    }

    #[test]
    fn exit_codes_read_as_outcomes() {
        assert_eq!(outcome_of(Ok(0)), Outcome::Added);
        assert_eq!(outcome_of(Err(1223)), Outcome::Declined);
        assert!(matches!(outcome_of(Ok(87)), Outcome::Failed(why) if why.contains("87")));
        assert!(matches!(outcome_of(Err(2)), Outcome::Failed(why) if why.contains("error 2")));
        assert!(Outcome::Added.text().contains("sign out and back in"));
    }

    #[test]
    fn setup_is_found_only_beside_the_exe() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("recall-native.exe");
        assert_eq!(setup_beside(&exe), None, "a dev build has no Setup copy");
        std::fs::write(dir.path().join(SETUP_EXE), b"x").expect("write");
        assert_eq!(setup_beside(&exe), Some(dir.path().join(SETUP_EXE)));
    }

    #[test]
    fn a_test_build_never_starts_setup() {
        assert!(!available(), "no button in a test build");
        start();
        assert!(!running() && last().is_none(), "the click does nothing in a test build");
    }
}
