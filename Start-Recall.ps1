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
if($Stop){
 $pipe=[IO.Pipes.NamedPipeClientStream]::new('.',"recall-hdr-control-$mySession",[IO.Pipes.PipeDirection]::Out);$pipe.Connect(2000)
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
