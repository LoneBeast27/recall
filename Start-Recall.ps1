# Start-Recall.ps1 -- launch Recall's controller in the unified record mode.
#
#   .\Start-Recall.ps1                 records ANY game automatically (config.toml decides
#                                      buffer, hotkeys, output folder, CPU mask, overlay)
#   .\Start-Recall.ps1 -Valorant       legacy override: pin the running VALORANT window
#                                      (RECALL_CAPTURE_HWND/NAME/AUDIO_GAME_PID) until it exits
#   .\Start-Recall.ps1 -Overlay        show the in-game toast overlay this session (RECALL_OVERLAY=1)
#   .\Start-Recall.ps1 -OutputDirectory D:\Clips   override recording.output_dir this session
#   .\Start-Recall.ps1 -OpenUI         also open the native Recall window
#   .\Start-Recall.ps1 -Stop           ask the running controller to quit (control pipe)
#
# Env overrides apply at controller startup only; a change made in the app wins.
param([switch]$Stop,[switch]$OpenUI,[switch]$Overlay,[switch]$Valorant,[string]$OutputDirectory)
$ErrorActionPreference='Stop'
$recallRoot=$PSScriptRoot
# This Windows session's recorder only: the control pipe is per session (recall-hdr control_pipe.rs).
$mySession=(Get-Process -Id $PID).SessionId
# The account the pipe's server runs as (GetNamedPipeServerProcessId, then its token user SID,
# else the SID the session list WTSEnumerateProcessesW gives: a same-account elevated recorder's
# token can't be opened from a non-elevated shell), or $null when neither can be read.
function Get-RecallPipeOwner($pipe){
 if(-not('RecallStart.Account' -as [type])){Add-Type -Name Account -Namespace RecallStart -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)] public static extern bool GetNamedPipeServerProcessId(IntPtr h, out uint pid);
[DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
[DllImport("advapi32.dll", SetLastError = true)] public static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
[StructLayout(LayoutKind.Sequential)] public struct WtsProcessInfo { public uint SessionId; public uint ProcessId; public IntPtr pProcessName; public IntPtr pUserSid; }
[DllImport("wtsapi32.dll", SetLastError = true)] public static extern bool WTSEnumerateProcessesW(IntPtr server, uint reserved, uint version, out IntPtr list, out uint count);
[DllImport("wtsapi32.dll")] public static extern void WTSFreeMemory(IntPtr memory);
public static string WtsSid(uint pid) {
  IntPtr list; uint count;
  if (!WTSEnumerateProcessesW(IntPtr.Zero, 0, 1, out list, out count)) return null;
  try {
    int size = Marshal.SizeOf(typeof(WtsProcessInfo));
    for (long i = 0; i < count; i++) {
      var info = (WtsProcessInfo)Marshal.PtrToStructure(new IntPtr(list.ToInt64() + i * size), typeof(WtsProcessInfo));
      if (info.ProcessId == pid) return info.pUserSid == IntPtr.Zero ? null : new System.Security.Principal.SecurityIdentifier(info.pUserSid).Value;
    }
    return null;
  } finally { WTSFreeMemory(list); }
}
'@}
 $server=[uint32]0;if(-not[RecallStart.Account]::GetNamedPipeServerProcessId($pipe.SafePipeHandle.DangerousGetHandle(),[ref]$server)){return $null}
 $sid=$null;$process=[RecallStart.Account]::OpenProcess(0x1000,$false,$server)
 if($process -ne [IntPtr]::Zero){$token=[IntPtr]::Zero;$ok=[RecallStart.Account]::OpenProcessToken($process,8,[ref]$token);[void][RecallStart.Account]::CloseHandle($process)
  if($ok){try{$sid=([Security.Principal.WindowsIdentity]::new($token)).User.Value}finally{[void][RecallStart.Account]::CloseHandle($token)}}}
 if($sid){$sid}else{[RecallStart.Account]::WtsSid($server)}
}
if($Stop){
 $pipe=[IO.Pipes.NamedPipeClientStream]::new('.',"recall-hdr-control-$mySession",[IO.Pipes.PipeDirection]::Out);$pipe.Connect(2000)
 $owner=Get-RecallPipeOwner $pipe;if($owner -ne [Security.Principal.WindowsIdentity]::GetCurrent().User.Value){$pipe.Dispose();if($owner){throw "another user owns the Recall pipe (runs as $owner): quit not sent."};throw "can't tell which account runs the Recall pipe: quit not sent."}
 $writer=[IO.StreamWriter]::new($pipe);$writer.AutoFlush=$true;$writer.WriteLine('{"cmd":"quit"}');$writer.Dispose()
 Write-Output 'Recall recording stopped.';return
}
if(Get-Process -Name recall,recall-hdr -ErrorAction SilentlyContinue|Where-Object{$_.SessionId -eq $mySession}){throw 'A recorder is already running. Stop that session (-Stop) before starting another.'}
$controllerExe=Join-Path $recallRoot 'target-dev\debug\recall-hdr.exe'
if(-not(Test-Path -LiteralPath $controllerExe)){throw "Controller not built: $controllerExe"}
$session=[ordered]@{mode='record';started_utc=[DateTime]::UtcNow.ToString('o')}
if($Valorant){
 $game=Get-Process -Name 'VALORANT-Win64-Shipping' -ErrorAction Stop|Select-Object -First 1
 if($game.MainWindowHandle -eq 0){throw 'Valorant has no capture window.'}
 $env:RECALL_CAPTURE_HWND=[string]$game.MainWindowHandle;$env:RECALL_CAPTURE_NAME='VALORANT';$env:RECALL_AUDIO_GAME_PID=[string]$game.Id
 $session['pinned_target']=[ordered]@{name='VALORANT';pid=$game.Id;hwnd=[string]$game.MainWindowHandle}
}else{
 # Never inherit a stale pin from this shell.
 Remove-Item Env:RECALL_CAPTURE_HWND,Env:RECALL_CAPTURE_NAME,Env:RECALL_AUDIO_GAME_PID -ErrorAction SilentlyContinue
}
if($Overlay){$env:RECALL_OVERLAY='1';$session['overlay']='forced on'}
if($OutputDirectory){$env:RECALL_OUT_DIR=$OutputDirectory;$session['output_directory']=$OutputDirectory}
$env:RECALL_NATIVE_BACKGROUND='1';$env:RECALL_ENGINE=Join-Path $recallRoot 'target-dev\debug\recall.exe'
$monitor=Join-Path $env:USERPROFILE '.claude\hooks\resource-monitor.sh'
$gitBash='C:\Program Files\Git\bin\bash.exe'
if(-not(Test-Path -LiteralPath $monitor)){throw 'The configured shared GPU resource monitor is unavailable.'}
& $gitBash $monitor status
& $gitBash $monitor list
$token=& $gitBash $monitor register 480 'Recall live NVENC replay (any game)'
$session['resource_token']=$token
$evidence=Join-Path $recallRoot '.planning\evidence\recorder';New-Item -ItemType Directory -Path $evidence -Force|Out-Null
try{
 $controller=Start-Process -FilePath $controllerExe -ArgumentList '--record' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $evidence 'session.stdout.log') -RedirectStandardError (Join-Path $evidence 'session.stderr.log')
 $session['controller_pid']=$controller.Id
 Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile','-File',('"{0}"' -f (Join-Path $recallRoot 'tools\watch-recording.ps1')),'-RecorderControllerPid',$controller.Id,'-ResourceToken',$token) -WindowStyle Hidden|Out-Null
 if($OpenUI){
  $ui=Start-Process -FilePath (Join-Path $recallRoot 'target-gpui\debug\recall-native.exe') -WindowStyle Hidden -PassThru
  $ui.ProcessorAffinity=[IntPtr]0xffff0000L;$ui.PriorityClass='BelowNormal';$session['ui_pid']=$ui.Id
 }
 $session|ConvertTo-Json|Set-Content -LiteralPath (Join-Path $evidence 'session.json')
 $session|ConvertTo-Json -Compress
}catch{
 if($null -eq $controller){& $gitBash $monitor deregister $token|Out-Null}
 throw
}
