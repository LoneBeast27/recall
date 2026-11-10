//! Static checks on scripts\install.ps1 and install-quit-wait.ps1 for the round-6 startup audit
//! (R6-startup-1..6): one quit at most, a wait that follows finishing_clip with a grace and an
//! optional cap, retried CIM queries, the wider sandbox guard and two warnings. Text and order
//! checks only; the decisions themselves are run with stubs by scripts\dev\install-quit-wait-check.ps1.

use crate::install_script_tests::{function_body, INSTALL};

const QUIT_WAIT: &str = include_str!("../../scripts/install-quit-wait.ps1");
const CHECK: &str = include_str!("../../scripts/dev/install-quit-wait-check.ps1");

#[test]
fn no_install_script_sends_a_forced_quit() {
    // F-startup-3: a plain quit never forces a save in the controller (overlay-p7); only "force":true
    // does. No installer script may send it, and the skip's comment says it is for older builds.
    let swap = include_str!("../../scripts/install-swap.ps1");
    let start = include_str!("../../Start-Recall.ps1");
    for (name, text) in [("install.ps1", INSTALL), ("install-quit-wait.ps1", QUIT_WAIT), ("install-swap.ps1", swap), ("Start-Recall.ps1", start)] {
        assert!(!text.contains("\"force\""), "{name} sends a forced quit");
    }
    assert!(QUIT_WAIT.contains("# quit (only a force flag or a second tray Quit does, and no script sends one); the skip is kept"));
    assert!(!QUIT_WAIT.contains("run: a second quit could force it"), "the stale reason is gone");
}

#[test]
fn a_recorder_already_finishing_a_clip_gets_no_second_quit() {
    // R6-startup-1: on an older recorder a second pipe quit during the save could force it and lose the clip.
    let req = function_body(QUIT_WAIT, "Request-RecorderQuit");
    let pending = req.find("$pending = Get-PendingQuit").expect("checks first");
    let join = req.find("return $pending").expect("joins the wait");
    let send = req.find("Send-Recorder '{\"cmd\":\"quit\"}'").unwrap();
    assert!(pending < join && join < send, "{req}");
    assert!(req.contains("no second quit sent"), "says so: {req}");
    // Only a controller of this account, on this session's pipe, and only finishing_clip = true.
    let p = function_body(QUIT_WAIT, "Get-PendingQuit");
    let owner = p.find("if ((Get-ProcessUserSid ([int]$c.ProcessId)) -ne $mySid) { continue }").expect("owner");
    let read = p.find("$s = Get-RecorderStatus $q").unwrap();
    assert!(owner < read, "{p}");
    assert!(p.contains("pipe = $pipeName }") && p.contains("if ($s -and $s.finishing_clip -eq $true) { return $q }"), "{p}");
    // install.ps1 sends no quit of its own any more.
    assert!(!INSTALL.contains("Send-Recorder '{\"cmd\":\"quit\"}'"));
    assert!(function_body(INSTALL, "Stop-Recorder").contains("$quit = Request-RecorderQuit"));
}

#[test]
fn the_long_wait_follows_finishing_clip_with_a_grace_and_an_optional_cap() {
    // R6-startup-2: a live recorder that is not saving (or not answering) no longer holds the run forever.
    let param = INSTALL.lines().find(|l| l.starts_with("param(")).expect("param line");
    assert!(param.contains(", [int]$QuitWaitMinutes = 0,") || param.contains(", [int]$QuitWaitMinutes = 0)"), "the switch, default no cap: {param}");
    assert!(INSTALL.lines().take(45).any(|l| l.contains("[-QuitWaitMinutes N]")), "usage line");
    assert!(INSTALL.lines().take(45).any(|l| l.contains("-QuitWaitMinutes N caps that wait")), "documented in the header");
    assert!(function_body(INSTALL, "Stop-Recorder").contains("Wait-ForQuit $quit $QuitWaitMinutes"));
    let w = function_body(QUIT_WAIT, "Wait-ForQuit($quit, [int]$capMinutes = 0)");
    assert!(w.contains("$s = Get-RecorderStatus $quit; $poll = (Get-Date).AddSeconds(2)"), "status every 2 s: {w}");
    assert!(w.contains("if ($step -eq 'timeout') { throw (Format-QuitTimeout $quit $w) }"), "{w}");
    assert!(!w.contains("while (Test-QuitPidAlive $quit) {"), "no wait keyed on the pid alone: {w}");
    let new = function_body(QUIT_WAIT, "New-QuitWait([datetime]$since, [int]$capMinutes)");
    assert!(new.contains("grace = [timespan]::FromMinutes(2)") && new.contains("$since.AddMinutes($capMinutes)"), "{new}");
    let step = function_body(QUIT_WAIT, "Get-QuitWaitStep($w, [datetime]$now, [bool]$alive, $finishing)");
    let clear = step.find("if ($finishing -eq $true) { $w.graceFrom = $null }").expect("finishing clears the grace");
    let grace = step.find("if ($w.graceFrom -and ($now - $w.graceFrom) -ge $w.grace) { $w.why = 'grace'; return 'timeout' }").unwrap();
    let cap = step.find("if ($w.cap -and $now -ge $w.cap) { $w.why = 'cap'; return 'timeout' }").unwrap();
    assert!(clear < cap && clear < grace, "{step}");
    let t = function_body(QUIT_WAIT, "Format-QuitTimeout($quit, $w)");
    assert_eq!(t.matches("Recall (pid $($quit.pid)) got the quit").count(), 2, "both limits name the pid: {t}");
}

#[test]
fn cim_errors_in_the_wait_are_retried_and_never_raised() {
    // R6-startup-3: under $ErrorActionPreference = 'Stop' one WMI hiccup used to end the run mid-wait.
    let retry = function_body(QUIT_WAIT, "Invoke-CimRetry([scriptblock]$query, $fallback)");
    assert!(retry.contains("for ($try = 1; $try -le 3; $try++) {") && retry.contains("try { return (& $query) } catch {"), "{retry}");
    assert!(function_body(QUIT_WAIT, "Test-QuitPidAlive($quit)").contains("Invoke-CimRetry {"));
    assert!(QUIT_WAIT.contains("function Test-AnyRecorder { Invoke-CimRetry { [bool]((Get-Installed) -or (Get-Controller)) } $true }"));
    let w = function_body(QUIT_WAIT, "Wait-ForQuit($quit, [int]$capMinutes = 0)");
    assert!(!w.contains("Get-CimInstance") && !w.contains("(Get-Installed)"), "every query in the wait goes through the retry: {w}");
}

#[test]
fn the_dev_check_runs_the_wait_with_every_outside_call_stubbed() {
    // The behavioural proof: stubs for every process, pipe and WMI call, and a virtual clock.
    for stub in [
        "function Get-Date {",
        "function Start-Sleep(",
        "function Get-CimInstance(",
        "function Get-Installed {",
        "function Get-Controller {",
        "function Get-ProcessUserSid(",
        "function Send-Recorder(",
        "function Get-PipeServer { throw",
        "function Get-RecorderStatus($quit) {",
    ] {
        assert!(CHECK.contains(stub), "{stub}");
    }
    // The real Get-RecorderStatus (a pipe) is replaced after the script is loaded.
    let load = CHECK.find(". $WaitScript").unwrap();
    assert!(load < CHECK.find("function Get-RecorderStatus($quit) {").unwrap());
    assert!(load > CHECK.find("function Get-CimInstance(").unwrap());
}

#[test]
fn a_sandbox_cannot_name_a_folder_that_holds_the_real_install() {
    // R6-startup-4: %LOCALAPPDATA%\Programs passed the equality guard and let -CloseApp close the real app.
    let guard = INSTALL
        .find("if ($env:RECALL_CONTROLLER_PATH -and $defaultRoot.StartsWith($installRoot, [StringComparison]::OrdinalIgnoreCase)) {")
        .expect("a prefix guard: equal or holding it");
    assert!(!INSTALL.contains("[string]::Equals($installRoot, $defaultRoot"), "the equality-only guard is gone");
    // Both roots end in '\', so the prefix test is by folder (Programs\Recall2 is not Programs\Recall).
    assert!(INSTALL.contains(r"$installRoot = [RecallInstall.PathId]::Normalize($InstallDir).TrimEnd('\') + '\'"));
    assert!(INSTALL.contains(r"'Programs\Recall')).TrimEnd('\') + '\'"));
    assert!(guard < INSTALL.find("\nAssert-AppClosed").unwrap());
    // -CloseApp closes only Get-InstalledApp's list: processes whose exe is inside -InstallDir.
    let app = INSTALL.lines().find(|l| l.starts_with("function Get-InstalledApp ")).unwrap();
    assert!(app.contains("Where-Object { Test-InInstallDir (Get-ExePath $_) }"), "{app}");
}

#[test]
fn no_start_with_autostart_says_start_with_windows_was_not_set() {
    // R6-startup-5: -NoStart returns before the recorder could take the setting.
    let at = INSTALL.find("if ($NoStart) {").unwrap();
    let block = &INSTALL[at..at + INSTALL[at..].find("\n  return").unwrap()];
    assert!(
        block.contains("if ($Autostart) { Write-Warning 'Start with Windows was not set (-NoStart). Turn it on in Recall > Settings > General.' }"),
        "{block}"
    );
}
