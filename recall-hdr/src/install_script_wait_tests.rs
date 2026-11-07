//! Static checks on scripts\install.ps1 for the round-5 startup audit (R5-startup-1..5): the
//! wait on a delivered quit, the sandbox guard against the real folder, the normalized wait
//! list, relative paths and per-phase owner refusals. Text and control-flow-order checks only:
//! the script is never run by the tests (it would replace the real install).

use crate::install_script_tests::{function_body, INSTALL};

const QUIT_WAIT: &str = include_str!("../../scripts/install-quit-wait.ps1");
const QUIT: &str = r#"'{"cmd":"quit"}'"#;

#[test]
fn a_delivered_quit_is_waited_for_while_that_recorder_lives() {
    // R5-startup-1: a quit can wait for a clip being saved for longer than any fixed cap, so after
    // the first 8 s the run waits only on the recorder that got the quit, with no cap.
    let stop = function_body(INSTALL, "Stop-Recorder");
    // Both sends keep their result; the legacy one is no longer thrown away.
    assert!(stop.lines().any(|l| l.trim() == format!("$sent = Send-Recorder {QUIT}")), "{stop}");
    assert!(stop.contains(&format!("if (-not $sent -and (Get-Controller)) {{ $sent = Send-Recorder {QUIT} $legacyPipe }}")), "{stop}");
    assert!(!stop.contains("[void](Send-Recorder"), "{stop}");
    // The recorder that got it is the one written to (recorded after the write, with its start time).
    let send = function_body(INSTALL, "Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5)");
    let write = send.find("$w.WriteLine($line)").unwrap();
    let rec = send.find("$script:sentTo = @{ pid = $server; created = $ours.CreationDate; pipe = $name }").expect("records the pid");
    assert!(write < rec, "{send}");
    // Order in Stop-Recorder: reset, sends, which recorder, wait, throw for what is left.
    let reset = stop.find("$script:sentTo = $null").expect("reset per call");
    let first_send = stop.find("Send-Recorder").unwrap();
    let pick = stop.find("$quit = if ($sent) { $script:sentTo } else { $null }").expect("only a delivered quit is waited on");
    let wait = stop.find("Wait-ForQuit $quit").expect("the wait");
    let throw = stop.find("throw (\"Recall is still running (").unwrap();
    assert!(reset < first_send && first_send < pick && pick < wait && wait < throw, "{stop}");
    // The wait: 8 s for everything, then only while THAT pid lives (no cap), then 8 s for the rest.
    // The wait lives in install-quit-wait.ps1, loaded before Stop-Recorder is defined.
    let load = INSTALL.find(". (Join-Path $PSScriptRoot 'install-quit-wait.ps1')").expect("dot-sourced");
    assert!(load < INSTALL.find("function Stop-Recorder {").unwrap());
    let w = function_body(QUIT_WAIT, "Wait-ForQuit($quit)");
    let all = w.find("while ((Get-Date) -lt $deadline -and ((Get-Installed) -or (Get-Controller))) {").expect("8 s for all");
    let gate = w.find("if (-not $quit -or -not (Test-QuitPidAlive $quit)) { return }").expect("nothing delivered: no long wait");
    let long = w.find("while (Test-QuitPidAlive $quit) {").expect("keyed on the quit pid only");
    let note = w.find("Write-Host (Format-QuitProgress $quit $since)").expect("progress line");
    let rest = w.rfind("while ((Get-Date) -lt $deadline -and ((Get-Installed) -or (Get-Controller))) {").unwrap();
    assert!(all < gate && gate < long && long < note && note < rest && all != rest, "{w}");
    assert!(w.contains("$next = (Get-Date).AddMinutes(1)"), "one line a minute: {w}");
    assert!(!INSTALL.contains("AddMinutes(15)") && !QUIT_WAIT.contains("AddMinutes(15)") && !w.contains("$longest"), "no fixed cap");
    // The pid is matched with its start time (a reused pid is not Recall).
    let alive = function_body(QUIT_WAIT, "Test-QuitPidAlive($quit)");
    assert!(alive.contains("\"ProcessId = $($quit.pid)\"") && alive.contains("$_.CreationDate -eq $quit.created"), "{alive}");
}

#[test]
fn the_progress_line_says_whether_a_clip_is_being_finished() {
    let p = function_body(QUIT_WAIT, "Format-QuitProgress($quit, [datetime]$since)");
    let status = p.find("$s = Get-RecorderStatus $quit").expect("asks the recorder");
    let finishing = p.find("if ($s -and $s.finishing_clip) {").expect("finishing_clip");
    assert!(status < finishing);
    assert!(p.contains("\"Recall is finishing a clip ($what, $mins min so far); waiting."), "{p}");
    assert!(p.contains("\"Waiting for Recall (pid $($quit.pid)) to exit ($mins min so far)"), "{p}");
    assert!(p.contains("MB written"), "{p}");
    assert!(p.contains("Ctrl+C"), "the way out is named: {p}");
    // The status read talks only to the recorder that got the quit, and cannot hang the run.
    let r = function_body(QUIT_WAIT, "Get-RecorderStatus($quit)");
    let connect = r.find("$pipe.Connect(1000)").unwrap();
    let same = r.find("if ((Get-PipeServer $pipe) -ne $quit.pid) { return $null }").expect("same server");
    let read = r.find("ReadLineAsync()").unwrap();
    assert!(connect < same && same < read, "{r}");
    assert!(r.contains("[IO.Pipes.PipeOptions]::Asynchronous"), "an overlapped read a dispose cancels: {r}");
    assert!(r.contains("$m.mod -eq 'session' -and $m.event.type -eq 'status'"), "{r}");
}

#[test]
fn a_sandbox_naming_the_real_install_folder_is_refused() {
    // R5-startup-2: a sandbox -InstallDir that is (an alias of) the default folder is the real install.
    let real = INSTALL
        .find(r"$defaultRoot = [RecallInstall.PathId]::Normalize((Join-Path $env:LOCALAPPDATA 'Programs\Recall')).TrimEnd('\') + '\'")
        .expect("the default folder, normalized");
    let guard = INSTALL
        .find("if ($env:RECALL_CONTROLLER_PATH -and [string]::Equals($installRoot, $defaultRoot, [StringComparison]::OrdinalIgnoreCase)) {")
        .expect("guard");
    assert!(INSTALL[guard..].contains("throw \"A sandboxed install (RECALL_CONTROLLER_PATH) can't use the real install folder"));
    let first_close = INSTALL.find("\nAssert-AppClosed").unwrap();
    assert!(INSTALL.find("$installRoot = ").unwrap() < real && real < guard && guard < first_close);
}

#[test]
fn the_wait_list_is_matched_by_folder_like_the_app_list() {
    // R5-startup-3: no raw name prefix, alias spellings resolve, elevated processes are read.
    let def = INSTALL.lines().find(|l| l.starts_with("function Get-Installed ")).unwrap();
    assert!(def.contains("| Where-Object { Test-InInstallDir (Get-ExePath $_) } }"), "{def}");
    assert!(!INSTALL.contains(".StartsWith($InstallDir"), "no raw prefix left");
    // Each spelling is resolved once per run (the wait polls every 300 ms).
    let id = function_body(INSTALL, "Get-PathId([string]$path)");
    assert!(id.contains("$script:pathIds[$path] = [RecallInstall.PathId]::Normalize($path)"), "{id}");
    let inside = INSTALL.lines().find(|l| l.starts_with("function Test-InInstallDir")).unwrap();
    assert!(inside.contains("(Get-PathId $path).StartsWith($installRoot, [StringComparison]::OrdinalIgnoreCase)"), "{inside}");
}

#[test]
fn relative_paths_resolve_once_against_the_powershell_location() {
    // R5-startup-4: .NET resolves against the process folder, New-Item/Start-Process against $PWD.
    let dir = INSTALL
        .find("$InstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)")
        .expect("-InstallDir resolved");
    let env = INSTALL
        .find("$envController = if ($env:RECALL_CONTROLLER_PATH) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($env:RECALL_CONTROLLER_PATH) }")
        .expect("env path resolved");
    let first_use = INSTALL.find("$drive = Get-DriveKind $InstallDir").unwrap();
    assert!(dir < first_use && env < first_use);
    let path = INSTALL.lines().find(|l| l.starts_with("$controllerPath = ")).unwrap();
    assert!(path.contains("if ($envController) { $envController }"), "{path}");
    assert!(!path.contains("{ $env:RECALL_CONTROLLER_PATH }"), "the raw env value is not used: {path}");
}

#[test]
fn an_owner_refusal_only_ends_its_own_phase() {
    // R5-startup-5: reset per phase; the shared bare pipe retries; -Autostart is never dropped silently.
    let stop = function_body(INSTALL, "Stop-Recorder");
    assert!(stop.contains("$script:ownerRefused = $false"), "reset when Stop-Recorder starts: {stop}");
    let start = INSTALL.find("Start-Process -FilePath (Join-Path $InstallDir 'recall-hdr.exe')").unwrap();
    let probe = INSTALL.find("while (-not $up -and -not $script:ownerRefused -and (Get-Date) -lt $deadline) {").unwrap();
    let reset = INSTALL[start..probe].find("$script:ownerRefused = $false");
    assert!(reset.is_some(), "reset between the start and the status probe");
    let send = function_body(INSTALL, "Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5)");
    let refused = send.find("$script:ownerRefused = $true").unwrap();
    let retry = send.find("if ($name -eq $legacyPipe) { Start-Sleep -Milliseconds 300; continue }").expect("legacy retries");
    let give_up = send[refused..].find("return $false").map(|i| i + refused).unwrap();
    assert!(refused < retry && retry < give_up, "{send}");
    assert!(
        INSTALL.contains("if ($Autostart -and -not $up) { Write-Warning 'Start with Windows was not set: the recorder did not answer. Turn it on in Recall > Settings > General.' }"),
        "-Autostart warns when nothing answered"
    );
}
