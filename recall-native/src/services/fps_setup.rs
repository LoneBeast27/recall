//! "Set up FPS" under Settings › Overlay: the stats island's FPS reads the present events
//! Windows itself logs, which needs the local group Performance Log Users. The installed
//! `RecallSetup.exe` (beside this exe) already has the elevated mode for that,
//! `/add-perflog-user <sid>`: it adds exactly that account and exits. The app starts it with the
//! `runas` verb, so Windows shows its own administrator prompt; nothing is hidden, no script host.
//! The group takes effect at the next sign-in, which the line then says.
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "fps_setup_win.rs"]
mod win;

/// The installed Setup copy (Apps & Features runs it for an uninstall too).
pub const SETUP_EXE: &str = "RecallSetup.exe";
/// Setup's elevated mode (scripts/setup/SetupFrameTiming.cs `ElevatedArgument`).
pub const ELEVATED_ARGUMENT: &str = "/add-perflog-user";
/// Setup's elevated Undo mode (`RemoveArgument`): removes exactly that account again.
pub const REMOVE_ARGUMENT: &str = "/remove-perflog-user";
pub const BUTTON: &str = "Set up FPS";
pub const RUNNING: &str = "Waiting for Windows…";
/// In the group, not yet in this sign-in's token.
pub const DONE: &str = "Done. Sign out and back in to finish.";
/// ERROR_CANCELLED: the administrator prompt was declined.
const CANCELLED: i32 = 1223;
/// Setup's exit code for "already a member" (ERROR_MEMBER_IN_ALIAS, SetupFrameTiming.cs).
pub const ALREADY_MEMBER: u32 = 1378;

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// In the group now; FPS shows after the next sign-in.
    Added,
    /// Out of the group again (the fix notice's Undo); counts from the next sign-in.
    Removed,
    /// It was a member already (someone or something added it before): nothing changed, so the
    /// fix notice writes no receipt and its Undo can never remove a membership Recall did not add.
    AlreadyMember,
    Declined,
    Failed(String),
}

impl Outcome {
    /// The line under the stats switch after a click.
    pub fn text(&self) -> String {
        match self {
            Outcome::Added => DONE.into(),
            Outcome::Removed => "Removed. Sign out and back in to finish.".into(),
            Outcome::AlreadyMember => "Already set up. Sign out and back in to see FPS.".into(),
            Outcome::Declined => "The administrator prompt was declined. FPS shows — until you allow it.".into(),
            Outcome::Failed(why) => format!("FPS setup failed: {why}"),
        }
    }
}

static BUSY: AtomicBool = AtomicBool::new(false);
static LAST: Mutex<Option<Outcome>> = Mutex::new(None);
/// The account is in the group (read once; set after an added outcome).
static MEMBER: Mutex<Option<bool>> = Mutex::new(None);

/// Setup beside `exe` (an installed Recall); none in a dev build.
pub fn setup_beside(exe: &Path) -> Option<PathBuf> {
    let setup = exe.parent()?.join(SETUP_EXE);
    setup.is_file().then_some(setup)
}

/// Only a well-formed account SID goes on the elevated command line (Setup checks it again): a
/// local or domain account (`S-1-5-21-...`) or a Microsoft Entra ID account (`S-1-12-1-a-b-c-d`).
pub fn is_account_sid(sid: &str) -> bool {
    // Canonical decimal u32 sub-authorities only: no sign, no leading zero, no overflow.
    let number = |part: &str| part.bytes().all(|b| b.is_ascii_digit()) && part.parse::<u32>().is_ok_and(|n| n.to_string() == part);
    if let Some(rest) = sid.strip_prefix("S-1-5-21-") {
        return rest.split('-').all(number);
    }
    if let Some(rest) = sid.strip_prefix("S-1-12-1-") {
        return rest.split('-').count() == 4 && rest.split('-').all(number);
    }
    false
}

/// Setup's arguments for `sid`.
pub fn arguments(sid: &str) -> Result<String, String> {
    arguments_for(ELEVATED_ARGUMENT, sid)
}

/// Setup's arguments for `mode` (add or remove) and `sid`.
pub fn arguments_for(mode: &str, sid: &str) -> Result<String, String> {
    if !is_account_sid(sid) {
        return Err(format!("unexpected account id {sid}"));
    }
    Ok(format!("{mode} {sid}"))
}

/// Setup's exit code as the outcome.
pub fn outcome_of(code: Result<u32, i32>) -> Outcome {
    match code {
        Ok(0) => Outcome::Added,
        Ok(ALREADY_MEMBER) => Outcome::AlreadyMember,
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

/// The account is already in Performance Log Users (it counts from the next sign-in), so the
/// line says to sign out instead of offering the prompt again. Read once (a local NetAPI call).
pub fn member() -> bool {
    if blocked() {
        return false;
    }
    let mut known = MEMBER.lock().unwrap_or_else(|p| p.into_inner());
    *known.get_or_insert_with(|| win::current_user_sid().and_then(|sid| win::is_member(&sid)).unwrap_or(false))
}

/// The click: Windows' prompt, owned by the app's window `owner` (so it opens in front), then the
/// wait, on its own thread. False when nothing started (a test build, or one already running).
pub fn start(owner: isize) -> bool {
    if blocked() || BUSY.swap(true, Ordering::SeqCst) {
        return false;
    }
    std::thread::spawn(move || {
        let outcome = request(owner);
        crate::trace::player_log("fps_setup", serde_json::json!({ "outcome": format!("{outcome:?}") }));
        if matches!(outcome, Outcome::Added | Outcome::AlreadyMember) {
            *MEMBER.lock().unwrap_or_else(|p| p.into_inner()) = Some(true);
        }
        *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
        BUSY.store(false, Ordering::SeqCst);
    });
    true
}

/// This user's account SID (the fix notice's target), `None` in a test build.
pub fn account() -> Option<String> {
    if blocked() {
        return None;
    }
    win::current_user_sid().ok()
}

/// The fix notice's Fix, on the caller's thread: Windows' prompt, then the wait. Shares the
/// Settings button's state, so the FPS line shows the same result.
pub fn add_now(owner: isize) -> Outcome {
    if blocked() || BUSY.swap(true, Ordering::SeqCst) {
        return Outcome::Failed("FPS setup is already running".into());
    }
    // Fresh, not the once-per-run value: the account may have joined since (Settings' button,
    // Setup's update offer, an administrator). A member already gets no prompt and no receipt.
    let fresh = win::current_user_sid().and_then(|sid| win::is_member(&sid));
    let outcome = if fresh == Ok(true) { Outcome::AlreadyMember } else { request(owner) };
    if matches!(outcome, Outcome::Added | Outcome::AlreadyMember) {
        *MEMBER.lock().unwrap_or_else(|p| p.into_inner()) = Some(true);
    }
    *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome.clone());
    BUSY.store(false, Ordering::SeqCst);
    outcome
}

/// The fix notice's Undo: Setup's elevated remove mode for `account`, on the caller's thread.
/// Only this Windows account's own receipt is undone here.
pub fn remove_now(account: &str, owner: isize) -> Outcome {
    if blocked() || BUSY.swap(true, Ordering::SeqCst) {
        return Outcome::Failed("FPS setup is already running".into());
    }
    if win::current_user_sid().ok().as_deref() != Some(account) {
        BUSY.store(false, Ordering::SeqCst);
        return Outcome::Failed("this fix was applied by another Windows account; sign in as that account to undo it".into());
    }
    let outcome = match outcome_of(elevated(REMOVE_ARGUMENT, owner)) {
        Outcome::Added => Outcome::Removed,
        other => other,
    };
    if outcome == Outcome::Removed {
        *MEMBER.lock().unwrap_or_else(|p| p.into_inner()) = Some(false);
        *LAST.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
    BUSY.store(false, Ordering::SeqCst);
    outcome
}

fn elevated(mode: &str, owner: isize) -> Result<u32, i32> {
    let Some(setup) = std::env::current_exe().ok().as_deref().and_then(setup_beside) else { return Err(-1) };
    let Ok(params) = win::current_user_sid().and_then(|sid| arguments_for(mode, &sid)) else { return Err(-1) };
    win::run_elevated(&setup, &params, owner)
}

fn request(owner: isize) -> Outcome {
    let setup = match std::env::current_exe().ok().as_deref().and_then(setup_beside) {
        Some(setup) => setup,
        None => return Outcome::Failed(format!("{SETUP_EXE} is not in Recall's folder; run Recall Setup instead")),
    };
    let params = match win::current_user_sid().and_then(|sid| arguments(&sid)) {
        Ok(params) => params,
        Err(why) => return Outcome::Failed(why),
    };
    outcome_of(win::run_elevated(&setup, &params, owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_account_sids_reach_the_elevated_command_line() {
        assert_eq!(arguments("S-1-5-21-111-222-333-1001").as_deref(), Ok("/add-perflog-user S-1-5-21-111-222-333-1001"));
        let entra = "S-1-12-1-1234567890-1234567890-1234567890-1234567890";
        assert_eq!(arguments(entra), Ok(format!("/add-perflog-user {entra}")), "Microsoft Entra ID accounts too");
        for bad in [
            "", "S-1-5-32-559", "S-1-5-21-", "S-1-5-21-1-2-3-1001 /uninstall", "S-1-5-21-1--2", "S-1-5-21-1-2-x",
            "S-1-12-1-1-2-3", "S-1-12-1-1-2-3-4-5", "S-1-12-1-1-2-x-4", "S-1-12-1-1-2-3-4 /uninstall", "S-1-12-2-1-2-3-4",
            "S-1-12-1-+1-2-3-4", "S-1-12-1-01-2-3-4", "S-1-12-1-4294967296-2-3-4",
        ] {
            assert!(arguments(bad).is_err(), "{bad:?} refused");
        }
    }

    #[test]
    fn setup_accepts_the_same_accounts() {
        let setup = include_str!("../../../scripts/setup/SetupFrameTiming.cs");
        assert!(setup.contains("\"S-1-5-21-\"") && setup.contains("EntraPrefix = \"S-1-12-1-\""), "Setup's elevated copy takes both kinds");
    }

    #[test]
    fn the_prompt_opens_over_the_app_and_the_line_clears() {
        let win = include_str!("fps_setup_win.rs");
        let co = win.find("CoInitializeEx(None").expect("COM first, as the updater does");
        assert!(co < win.find("ShellExecuteExW(&mut info)").expect("the shell call"), "COM before the shell call");
        assert!(win.contains("info.hwnd = HWND(owner as *mut _)"), "the app's window owns the prompt");
        let view = include_str!("../views/settings.rs");
        assert!(view.contains("crate::fps_setup::start(owner)") && view.contains("while crate::fps_setup::running()"), "the view waits for the end");
        assert!(view.contains("this.update(cx, |_, cx| cx.notify())"), "and redraws when it ends");
    }

    #[test]
    fn the_elevated_argument_matches_setup() {
        let setup = include_str!("../../../scripts/setup/SetupFrameTiming.cs");
        assert!(setup.contains(&format!("ElevatedArgument = \"{ELEVATED_ARGUMENT}\"")), "Setup's elevated mode name");
        assert!(setup.contains(&format!("RemoveArgument = \"{REMOVE_ARGUMENT}\"")), "Setup's elevated Undo mode name");
        assert_eq!(arguments_for(REMOVE_ARGUMENT, "S-1-5-21-1-2-3-1001").as_deref(), Ok("/remove-perflog-user S-1-5-21-1-2-3-1001"));
        assert!(arguments_for(REMOVE_ARGUMENT, "S-1-5-32-559").is_err(), "only account SIDs, for Undo too");
        for bad in ["S-1-5-21-1-2-3-1001 /add-perflog-user", "S-1-5-21-1-2-3-1001 S-1-5-21-1-2-3-1002", " S-1-5-21-1-2-3-1001", "S-1-5-21-1-2-3-1001 "] {
            assert!(arguments_for(REMOVE_ARGUMENT, bad).is_err(), "{bad:?}: no extra switch, account or space");
        }
        let registry = include_str!("../../../scripts/setup/SetupRegistry.cs");
        assert!(registry.contains(&format!("UninstallerName = \"{SETUP_EXE}\"")), "the installed Setup's file name");
    }

    #[test]
    fn exit_codes_read_as_outcomes() {
        assert_eq!(outcome_of(Ok(0)), Outcome::Added);
        assert_eq!(outcome_of(Err(1223)), Outcome::Declined);
        assert!(matches!(outcome_of(Ok(87)), Outcome::Failed(why) if why.contains("87")));
        assert_eq!(outcome_of(Ok(1378)), Outcome::AlreadyMember, "already a member is not \"added\"");
        let setup = include_str!("../../../scripts/setup/SetupFrameTiming.cs");
        assert!(setup.contains(&format!("MemberInAlias = {ALREADY_MEMBER}")), "the same code as Setup's");
        assert!(matches!(outcome_of(Err(2)), Outcome::Failed(why) if why.contains("error 2")));
        assert_eq!(Outcome::Added.text(), DONE);
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
        assert!(!member(), "no group lookup in a test build");
        assert!(!start(0), "the click starts nothing in a test build");
        assert!(matches!(add_now(0), Outcome::Failed(_)) && matches!(remove_now("S-1-5-21-1-2-3-1001", 0), Outcome::Failed(_)), "the notice starts nothing in a test build");
        assert_eq!(account(), None, "no account lookup in a test build");
        assert!(!running() && last().is_none(), "the click does nothing in a test build");
    }
}
