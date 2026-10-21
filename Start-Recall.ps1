param([switch]$Stop,[switch]$OpenUI,[switch]$Overlay,[string]$OutputDirectory='V:\ALL RECORDINGS')
$ErrorActionPreference='Stop'
$recallRoot=$PSScriptRoot
if($Stop){
 $pipe=[IO.Pipes.NamedPipeClientStream]::new('.','recall-hdr-control',[IO.Pipes.PipeDirection]::Out);$pipe.Connect(2000)
 $writer=[IO.StreamWriter]::new($pipe);$writer.AutoFlush=$true;$writer.WriteLine('{"cmd":"quit"}');$writer.Dispose()
 Write-Output 'Recall recording stopped.';return
}
$game=Get-Process -Name 'VALORANT-Win64-Shipping' -ErrorAction Stop|Select-Object -First 1
if($game.MainWindowHandle -eq 0){throw 'Valorant has no capture window.'}
if(Get-Process -Name recall,recall-hdr -ErrorAction SilentlyContinue){throw 'A recorder is already running. Stop that session before starting another.'}
$monitor=Join-Path $env:USERPROFILE '.claude\hooks\resource-monitor.sh'
$gitBash='C:\Program Files\Git\bin\bash.exe'
if(-not(Test-Path -LiteralPath $monitor)){throw 'The configured shared GPU resource monitor is unavailable.'}
& $gitBash $monitor status
& $gitBash $monitor list
$token=& $gitBash $monitor register 480 'Recall Valorant live NVENC replay'
$evidence=Join-Path $recallRoot '.planning\evidence\valorant';New-Item -ItemType Directory -Path $evidence -Force|Out-Null
$env:RECALL_CPU_MASK='ffff0000';$env:RECALL_BUFFER_SECS='120';$env:RECALL_CAPTURE_HWND=[string]$game.MainWindowHandle
$env:RECALL_CAPTURE_NAME='VALORANT';$env:RECALL_AUDIO_GAME_PID=[string]$game.Id
$env:RECALL_SAVE_HOTKEY='Alt+N';$env:RECALL_MIC_HOTKEY='Alt+M';$env:RECALL_OUT_DIR=$OutputDirectory
$env:RECALL_NATIVE_BACKGROUND='1';$env:RECALL_ENGINE=Join-Path $recallRoot 'target-dev\debug\recall.exe'
try{
 $controller=Start-Process -FilePath (Join-Path $recallRoot 'target-dev\debug\recall-hdr.exe') -ArgumentList '--record-only' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $evidence 'session.stdout.log') -RedirectStandardError (Join-Path $evidence 'session.stderr.log')
 $session=[ordered]@{controller_pid=$controller.Id;valorant_pid=$game.Id;capture_hwnd=[string]$game.MainWindowHandle;cpu_mask='0xffff0000';logical_processors=@(16..31);buffer_seconds=120;save_hotkey='Alt+N';microphone_hotkey='Alt+M';game_audio='process loopback';output_directory=$OutputDirectory;resource_token=$token;started_utc=[DateTime]::UtcNow.ToString('o');mode='record-only'}
 Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile','-File',('"{0}"' -f (Join-Path $recallRoot 'tools\watch-recording.ps1')),'-RecorderControllerPid',$controller.Id,'-ResourceToken',$token) -WindowStyle Hidden|Out-Null
 if($Overlay){
  $hud=Start-Process -FilePath (Join-Path $recallRoot 'target-dev\debug\recall-overlay.exe') -ArgumentList @('--parent',$controller.Id,'--diagnostics',(Join-Path $evidence 'overlay.json')) -WindowStyle Hidden -PassThru
  $hud.ProcessorAffinity=[IntPtr]0xffff0000L;$hud.PriorityClass='BelowNormal';$session['overlay_pid']=$hud.Id
 }
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

