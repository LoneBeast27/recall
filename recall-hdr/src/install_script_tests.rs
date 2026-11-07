//! Static checks on the PowerShell scripts that talk to this controller (they are never run by
//! the tests: running the installer would replace the real install). They pin the decisions
//! the scripts must keep: autostart opt-in (audit S1/G1), the per-session pipe name and
//! session-filtered process checks (G3), the re-check after the build (G2), the unknown-drive
//! switch (G16) and the marker's start time (G12).

pub(crate) const INSTALL: &str = include_str!("../../scripts/install.ps1");
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
    for query in ["Get-InstalledApp", "Get-Installed", "Get-SessionController"] {
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
pub(crate) fn function_body<'a>(script: &'a str, name: &str) -> &'a str {
    let open = format!("function {name} {{");
    let at = script.find(&open).unwrap_or_else(|| panic!("{name} defined"));
    let end = script[at..].find("\n}").map(|e| at + e).unwrap_or(script.len());
    &script[at..end]
}

#[test]
fn stop_recorder_also_quits_a_pre_session_recorder() {
    // An install from before the per-session pipe runs a controller on the bare name; its
    // first update must still reach it (audit R2-deep-startup-1), and only from Stop-Recorder.
    assert!(INSTALL.contains("else { 'recall-hdr-control' }"), "the old name, kept for one release");
    let stop = function_body(INSTALL, "Stop-Recorder");
    let session = stop.find("Send-Recorder '{\"cmd\":\"quit\"}'").expect("quit on the session pipe");
    let legacy = stop.find("Send-Recorder '{\"cmd\":\"quit\"}' $legacyPipe").expect("quit on the legacy pipe");
    assert!(session < legacy, "the session pipe is asked first");
    // The legacy name is used nowhere else (status checks and the autostart patch stay per session).
    // Defined once, sent to once in Stop-Recorder, and named in Send-Recorder's retry on the
    // shared bare pipe (R5-startup-5).
    let uses: Vec<&str> = code_lines(INSTALL).filter(|l| l.contains("$legacyPipe")).collect();
    assert_eq!(uses.len(), 4, "{uses:?}");
    let send = function_body(INSTALL, "Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5)");
    assert_eq!(send.matches("if ($name -eq $legacyPipe) { Start-Sleep -Milliseconds 300; continue }").count(), 2, "{send}");
    // Send-Recorder takes the pipe name, defaulting to this session's.
    assert!(INSTALL.contains("function Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5) {"));
}

/// The single code line that assigns `$<var> = `.
fn assignment<'a>(script: &'a str, var: &str) -> &'a str {
    let head = format!("${var} = ");
    let mut hits = code_lines(script).filter(|l| l.starts_with(&head));
    let line = hits.next().unwrap_or_else(|| panic!("${var} assigned"));
    assert!(hits.next().is_none(), "${var} assigned once");
    line
}

#[test]
fn a_sandboxed_install_names_its_own_legacy_pipe_and_controller() {
    // A sandboxed update sets both env vars so it never quits the live recorder; without them
    // the values are today's (the bare legacy name, the install folder's recall-hdr.exe).
    let legacy = assignment(INSTALL, "legacyPipe");
    assert_eq!(legacy, "$legacyPipe = if ($env:RECALL_LEGACY_PIPE) { $env:RECALL_LEGACY_PIPE } else { 'recall-hdr-control' }");
    let path = assignment(INSTALL, "controllerPath");
    // The env value as resolved against the PowerShell location (R5-startup-4).
    assert!(path.contains("if ($envController) { $envController }"), "{path}");
    assert!(path.contains("else { Join-Path $InstallDir 'recall-hdr.exe' }"), "{path}");
    assert!(path.starts_with("$controllerPath = [RecallInstall.PathId]::Normalize("), "compared as a normalized path: {path}");
    // Both are set before the first Stop-Recorder call.
    let first_stop = INSTALL.find("
Stop-Recorder").expect("Stop-Recorder called");
    assert!(INSTALL.find(path).unwrap() < first_stop && INSTALL.find(legacy).unwrap() < first_stop);
}

#[test]
fn only_a_sandbox_limits_the_quit_to_the_controller_it_names() {
    // R4-startup-1: with RECALL_CONTROLLER_PATH unset (the real install) every recall-hdr.exe in
    // this session is quit and waited for, wherever it runs from. Only a sandbox filters by path.
    let ours = INSTALL.lines().find(|l| l.starts_with("function Test-OurController")).expect("path filter");
    assert!(
        ours.contains("[string]::Equals([RecallInstall.PathId]::Normalize($path), $controllerPath, [StringComparison]::OrdinalIgnoreCase)"),
        "case-insensitive normalized-path compare: {ours}"
    );
    assert!(ours.contains("$path = Get-ExePath $p"), "the image path, also for an elevated process: {ours}");
    let ctl = function_body(INSTALL, "Get-Controller");
    assert!(ctl.contains("$all = @(Get-SessionController)"), "{ctl}");
    assert!(
        ctl.contains("if ($env:RECALL_CONTROLLER_PATH) { $all | Where-Object { Test-OurController $_ } } else { $all }"),
        "the path filter applies only in a sandbox: {ctl}"
    );
    // Stop-Recorder reaches either pipe only behind Get-Controller and waits on it; a recorder
    // that won't quit makes the run throw, naming it.
    let stop = function_body(INSTALL, "Stop-Recorder");
    assert!(stop.contains("if (-not $sent -and (Get-Controller)) {"));
    assert!(stop.contains("Wait-ForQuit $quit"), "{stop}");
    let wait = include_str!("../../scripts/install-quit-wait.ps1");
    assert!(wait.contains("((Get-Installed) -or (Get-Controller))"), "waits on every controller it quits");
    assert!(
        stop.contains(r#"throw ("Recall is still running (" + (($left | ForEach-Object { Format-Recorder $_ }) -join ', ')"#),
        "names it: {stop}"
    );
    assert!(function_body(INSTALL, "Format-Recorder($p)").contains("Get-ExePath $p"), "with its path");
    // The "left running" note is a sandbox note: a real install leaves nothing running.
    let note = stop.find("Left running").unwrap();
    assert!(stop[..note].contains("if ($env:RECALL_CONTROLLER_PATH) {"), "{stop}");
    // The unfiltered list is never a send or a wait.
    for line in code_lines(INSTALL).filter(|l| l.contains("Get-SessionController")) {
        assert!(!line.contains("Send-Recorder") && !line.contains("while"), "{line}");
    }
    // The header comment says so.
    assert!(INSTALL.contains("#    A real install quits every recall-hdr.exe in this session"), "comment updated");
}

#[test]
fn an_old_recorder_is_named_whether_or_not_one_answered() {
    // R4-startup-1: the stray warning sat in the `else` of `if (-not $up)`, so it never showed
    // when the old recorder kept the new one from answering.
    let warn = INSTALL.find("An older recorder is still running").expect("stray warning");
    let did_not = INSTALL.find("if (-not $up) { Write-Warning 'The recorder did not answer").expect("no-answer warning");
    assert!(did_not < warn);
    let block = &INSTALL[did_not..warn];
    assert!(!block.contains("else"), "not inside the else branch: {block}");
    assert!(block.contains("if (-not $env:RECALL_CONTROLLER_PATH) {"), "a sandbox leaves the live recorder alone");
    assert!(block.contains("$stray = @(Get-SessionController | Where-Object { -not (Test-OurController $_) })"), "{block}");
}

#[test]
fn controller_paths_are_compared_by_what_they_name() {
    // R4-startup-1 (sandbox): 8.3 names, \\?\ prefixes, junctions and subst drives normalize to
    // one spelling; an elevated process's image path comes from QueryFullProcessImageNameW.
    let at = INSTALL.find("Add-Type -Name PathId -Namespace RecallInstall").expect("path type");
    let ty = &INSTALL[at..at + INSTALL[at..].find("'@").unwrap()];
    for api in ["GetFinalPathNameByHandleW", "GetLongPathNameW", "QueryFullProcessImageNameW", "CreateFileW", "System.IO.Path.GetFullPath"] {
        assert!(ty.contains(api), "{api}");
    }
    assert!(ty.contains(r#"p.StartsWith(@"\\?\UNC\")"#) && ty.contains(r#"p.StartsWith(@"\\?\")"#), "the long-path prefix is stripped");
    assert!(ty.contains("0x02000000"), "FILE_FLAG_BACKUP_SEMANTICS: folders open too");
    let exe = INSTALL.lines().find(|l| l.starts_with("function Get-ExePath")).expect("Get-ExePath");
    assert!(
        exe.contains("if ($p.ExecutablePath) { $p.ExecutablePath } else { [RecallInstall.PathId]::ImagePath([uint32]$p.ProcessId) }"),
        "{exe}"
    );
    // The type is loaded before the first use.
    assert!(at < INSTALL.find("$controllerPath = ").unwrap());
}

#[test]
fn a_sandbox_outside_its_install_dir_is_refused_before_anything_closes() {
    // R4-startup-4: RECALL_CONTROLLER_PATH outside -InstallDir would close the real app, wait on
    // the real recorder and swap the real install. Refused before any of that.
    let guard = INSTALL
        .find("if ($env:RECALL_CONTROLLER_PATH -and -not (Test-InInstallDir $controllerPath)) {")
        .expect("guard");
    let next = &INSTALL[guard..];
    let throw = &next[..next.find("\n}").unwrap()];
    assert!(throw.contains(r#"throw "RECALL_CONTROLLER_PATH ($controllerPath) is not inside -InstallDir"#), "{throw}");
    let first_close = INSTALL.find("\nAssert-AppClosed").expect("first app check");
    let first_stop = INSTALL.find("\nStop-Recorder").unwrap();
    assert!(guard < first_close && guard < first_stop);
    assert!(
        INSTALL.contains(r"$installRoot = [RecallInstall.PathId]::Normalize($InstallDir).TrimEnd('\') + '\'"),
        "a folder prefix, not a name prefix"
    );
    let inside = INSTALL.lines().find(|l| l.starts_with("function Test-InInstallDir")).unwrap();
    assert!(
        inside.contains("(Get-PathId $path).StartsWith($installRoot, [StringComparison]::OrdinalIgnoreCase)"),
        "{inside}"
    );
    // -CloseApp closes only what Get-InstalledApp lists: app processes inside -InstallDir.
    let app = INSTALL.lines().find(|l| l.starts_with("function Get-InstalledApp ")).unwrap();
    assert!(app.contains("Where-Object { Test-InInstallDir (Get-ExePath $_) }"), "{app}");
    let close = function_body(INSTALL, "Assert-AppClosed");
    assert!(close.contains("$app = @(Get-InstalledApp)") && close.contains("foreach ($p in $app)"));
}

#[test]
fn nothing_is_sent_unless_our_controller_serves_the_pipe() {
    // The live recorder owns this session's real pipe during a sandboxed install: the server
    // process of each connection must be the path-matched controller, else nothing is written.
    let send = function_body(INSTALL, "Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5)");
    let check = send.find("$server = Get-PipeServer $pipe").expect("server pid read after connect");
    let ours = send.find("$ours = @(Get-Controller) | Where-Object { [int]$_.ProcessId -eq $server } | Select-Object -First 1").expect("the server is ours");
    let refuse = send.find("if (-not $server -or -not $ours) {").expect("refuse a foreign server");
    assert!(ours < refuse && send[refuse..].contains("$pipe.Dispose()"));
    let write = send.find("$w.WriteLine($line)").expect("the write");
    assert!(send.find("$pipe.Connect(1500)").unwrap() < check && check < refuse && refuse < write);
    assert!(function_body(INSTALL, "Get-PipeServer($pipe)").contains("GetNamedPipeServerProcessId"));
}

#[test]
fn script_clients_refuse_a_pipe_served_by_another_account() {
    // deep-startup-p3 a: the pipe's server process must run as this account (token user SID).
    let send = function_body(INSTALL, "Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5)");
    let owner = send.find("$owner = Get-ProcessUserSid $server").expect("installer: owner SID read");
    let refuse = send.find("if ($owner -ne $mySid) {").expect("refused unless it is this account");
    assert!(owner < refuse && refuse < send.find("$w.WriteLine($line)").unwrap(), "checked before anything is written");
    assert!(INSTALL.contains("$mySid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value"));
    let sid = function_body(INSTALL, "Get-TokenUserSid([int]$id)");
    assert!(sid.contains("OpenProcessToken($process, 8, [ref]$token)") && sid.contains("([Security.Principal.WindowsIdentity]::new($token)).User.Value"));
    // Start-Recall.ps1 -Stop: the same check before the quit line.
    let check = START.find("function Get-RecallPipeOwner($pipe){").expect("Start-Recall: owner check");
    assert!(START[check..].contains("GetNamedPipeServerProcessId($pipe.SafePipeHandle.DangerousGetHandle(),[ref]$server)"));
    let refuse = START
        .find("$owner=Get-RecallPipeOwner $pipe;if($owner -ne [Security.Principal.WindowsIdentity]::GetCurrent().User.Value){$pipe.Dispose();")
        .expect("-Stop refuses");
    let quit = START.find(r#"$writer.WriteLine('{"cmd":"quit"}')"#).unwrap();
    assert!(check < refuse && refuse < quit);
}

#[test]
fn an_unreadable_token_falls_back_to_the_session_list_and_warns_once() {
    // R4-startup-2: a same-account elevated recorder's token can't be opened from a
    // non-elevated script; WTSEnumerateProcessesW still lists its SID (as pipe_owner.rs does).
    for (name, script) in [("install.ps1", INSTALL), ("Start-Recall.ps1", START)] {
        assert!(script.contains("WTSEnumerateProcessesW(IntPtr.Zero, 0, 1, out list, out count)"), "{name}: WTS list");
        assert!(script.contains("WTSFreeMemory(list)"), "{name}: freed");
        assert!(script.contains("new System.Security.Principal.SecurityIdentifier(info.pUserSid).Value"), "{name}: SID text");
    }
    // Up to the next function: the C# here-string has `}` at column 0, which ends function_body.
    let at = INSTALL.find("function Get-ProcessUserSid([int]$id) {").expect("Get-ProcessUserSid");
    let sid = &INSTALL[at..at + INSTALL[at..].find("\nfunction Get-TokenUserSid").expect("next function")];
    assert!(
        sid.contains("$sid = Get-TokenUserSid $id; if ($sid) { $sid } else { [RecallInstall.Account]::WtsSid([uint32]$id) }"),
        "{sid}"
    );
    assert!(START.contains("if($sid){$sid}else{[RecallStart.Account]::WtsSid($server)}"));
    // Unreadable is not "another user", and each reason is warned once.
    let send = function_body(INSTALL, "Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5)");
    assert!(send.contains(r#"else { "can't tell which account runs the Recall pipe ($name, pid $server)" }"#), "{send}");
    assert!(send.contains("if ($script:ownerWarned -notcontains $why) { $script:ownerWarned += $why; Write-Warning"), "{send}");
    assert!(send.contains("$script:ownerRefused = $true"));
    assert!(START.contains(r#"throw "can't tell which account runs the Recall pipe: quit not sent.""#));
    // The status probe stops at the first refusal instead of repeating it every 500 ms.
    assert!(INSTALL.contains("while (-not $up -and -not $script:ownerRefused -and (Get-Date) -lt $deadline) {"));
}
