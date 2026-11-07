//! Static checks on the PowerShell scripts that talk to this controller (they are never run by
//! the tests: running the installer would replace the real install). They pin the decisions
//! the scripts must keep: autostart opt-in (audit S1/G1), the per-session pipe name and
//! session-filtered process checks (G3), the re-check after the build (G2), the unknown-drive
//! switch (G16) and the marker's start time (G12).

const INSTALL: &str = include_str!("../../scripts/install.ps1");
const SWAP: &str = include_str!("../../scripts/install-swap.ps1");
const START: &str = include_str!("../../Start-Recall.ps1");

fn code_lines(script: &str) -> impl Iterator<Item = &str> {
    script.lines().map(str::trim).filter(|l| !l.starts_with('#'))
}

#[test]
fn autostart_is_sent_only_behind_the_autostart_switch() {
    assert!(INSTALL.contains("[switch]$Autostart"), "an opt-in -Autostart switch");
    let sends: Vec<&str> = INSTALL.lines().filter(|l| l.contains(r#""autostart":true"#)).collect();
    assert_eq!(sends.len(), 1, "one place sends the patch");
    // The send sits in the block opened by `if ($up -and $Autostart) {`.
    let at = INSTALL.find(r#""autostart":true"#).unwrap();
    let opener = INSTALL[..at].rfind("if (").unwrap();
    let guard = &INSTALL[opener..at];
    assert!(guard.starts_with("if ($up -and $Autostart) {"), "guard: {guard}");
    assert!(!INSTALL.contains("-not $NoAutostart"), "the old opt-out default is gone");
    // Header usage names the opt-in switch.
    assert!(INSTALL.lines().take(40).any(|l| l.contains("[-Autostart]")));
}

#[test]
fn every_pipe_and_process_check_is_scoped_to_this_session() {
    let quoted_bare = "'recall-hdr-control'";
    for (name, script) in [("install.ps1", INSTALL), ("Start-Recall.ps1", START)] {
        // The one exception: Stop-Recorder's quit to a pre-session recorder (checked below).
        let opens = code_lines(script).filter(|l| !l.starts_with("$legacyPipe = ")).any(|l| l.contains(quoted_bare));
        assert!(!opens, "{name} still opens the machine-wide pipe");
        assert!(script.contains("(Get-Process -Id $PID).SessionId"), "{name} reads its session");
    }
    assert!(INSTALL.contains(r#"$pipeName = "recall-hdr-control-$session""#));
    assert_eq!(crate::control_pipe::pipe_name(Some(7)), "recall-hdr-control-7", "the same form as the scripts");
    assert!(START.contains(r#""recall-hdr-control-$mySession""#));
    for query in ["Get-InstalledApp", "Get-Installed", "Get-Controller"] {
        let def = INSTALL.lines().find(|l| l.starts_with(&format!("function {query} "))).unwrap_or_else(|| panic!("{query}"));
        assert!(def.contains("SessionId = $session"), "{query}: {def}");
    }
}

#[test]
fn the_app_and_recorder_are_checked_again_after_the_build() {
    let build = INSTALL.find("# 3. Build.").expect("build step");
    let swap = INSTALL.find("Install-Swap $stage $InstallDir").expect("swap call");
    let between = &INSTALL[build..swap];
    let again = between.find("Assert-AppClosed").expect("app check after the build");
    assert!(between[again..].contains("Stop-Recorder"), "recorder quit after the build");
}

#[test]
fn an_unclassified_drive_can_be_allowed_but_an_hdd_never() {
    assert!(INSTALL.contains("[switch]$AllowUnknownDrive"));
    assert!(INSTALL.contains("if ($drive.kind -eq 'HDD' -or ($drive.kind -ne 'SSD' -and -not $AllowUnknownDrive)) {"));
}

#[test]
fn the_install_marker_carries_the_installer_start_time() {
    assert!(SWAP.contains(r#"{ "$PID $created" }"#), "marker holds pid and start time");
    assert!(SWAP.contains("GetProcessTimes"));
}

/// The body of `function <name> {` up to its closing `}` at column 0.
fn function_body<'a>(script: &'a str, name: &str) -> &'a str {
    let open = format!("function {name} {{");
    let at = script.find(&open).unwrap_or_else(|| panic!("{name} defined"));
    let end = script[at..].find("\n}").map(|e| at + e).unwrap_or(script.len());
    &script[at..end]
}

#[test]
fn stop_recorder_also_quits_a_pre_session_recorder() {
    // An install from before the per-session pipe runs a controller on the bare name; its
    // first update must still reach it (audit R2-deep-startup-1), and only from Stop-Recorder.
    assert!(INSTALL.contains("$legacyPipe = 'recall-hdr-control'"), "the old name, kept for one release");
    let stop = function_body(INSTALL, "Stop-Recorder");
    let session = stop.find("Send-Recorder '{\"cmd\":\"quit\"}'").expect("quit on the session pipe");
    let legacy = stop.find("Send-Recorder '{\"cmd\":\"quit\"}' $legacyPipe").expect("quit on the legacy pipe");
    assert!(session < legacy, "the session pipe is asked first");
    // The legacy name is used nowhere else (status checks and the autostart patch stay per session).
    let uses = INSTALL.lines().filter(|l| l.contains("$legacyPipe")).count();
    assert_eq!(uses, 2, "defined once, used once in Stop-Recorder");
    // Send-Recorder takes the pipe name, defaulting to this session's.
    assert!(INSTALL.contains("function Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5) {"));
}
