<#
  Saves a picture of the Audio page (Sources strips + MIDI card) from a scripted, offscreen app.
  A private control pipe serves a fixture config, so the real recorder and config are never
  touched; the fixture names no real device or app. Offscreen apps open no meters and play nothing.

  powershell -NoProfile -File scripts\dev\audio-page-shot.ps1 -Exe <recall-native.exe> -Out <png> [-Size 1280x900]
#>
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Out, [string]$Size = '1280x900')
$ErrorActionPreference = 'Stop'
$pipe = "recall-audio-shot-$PID"
$sources = @(
  @{ id = 'game'; kind = 'game'; device = ''; exe = ''; name = 'Game'; enabled = $true; gain_db = 0 },
  @{ id = 'mic'; kind = 'input'; device = 'Microphone (USB Audio Device)'; exe = ''; name = 'Microphone'; enabled = $true; gain_db = 3.5 },
  @{ id = 'app-1'; kind = 'app'; device = ''; exe = 'player.exe'; name = 'Music'; enabled = $true; gain_db = -12 },
  @{ id = 'output-1'; kind = 'output'; device = 'Speakers (USB Audio Device)'; exe = ''; name = 'Desktop audio'; enabled = $false; gain_db = 0 }
)
$config = @{ recording = @{ audio_sources = $sources; buffer_secs = 120 }; midi = @{ input = '' } }
$line = (@{ mod = 'session'; event = @{ type = 'config'; config = $config } } | ConvertTo-Json -Depth 8 -Compress)

# The fixture pipe: answers the app's connection with the config, then stays open.
$server = Start-Job -ArgumentList $pipe, $line -ScriptBlock {
  param($pipe, $line)
  $deadline = (Get-Date).AddSeconds(40)
  while ((Get-Date) -lt $deadline) {
    $s = [IO.Pipes.NamedPipeServerStream]::new($pipe, [IO.Pipes.PipeDirection]::InOut, 4, [IO.Pipes.PipeTransmissionMode]::Byte, [IO.Pipes.PipeOptions]::Asynchronous)
    $wait = $s.WaitForConnectionAsync()
    if (-not $wait.Wait(20000)) { $s.Dispose(); break }
    $w = [IO.StreamWriter]::new($s); $w.AutoFlush = $true
    try { $w.WriteLine($line); Start-Sleep -Seconds 12 } catch { }
    $s.Dispose()
  }
}

$sandbox = Join-Path ([IO.Path]::GetTempPath()) "recall-audio-shot-$PID"
New-Item -ItemType Directory -Force -Path $sandbox | Out-Null
$saved = @{}
$vars = @{ RECALL_NATIVE_OFFSCREEN = '1'; RECALL_NATIVE_ROUTE = 'audio'; RECALL_NO_CONTROLLER = '1'; RECALL_CONTROL_PIPE = $pipe; RECALL_NATIVE_WINDOW_SIZE = $Size;
  RECALL_INSTANCE_TAG = "audio-shot-$PID"; RECALL_FOLDERS = (Join-Path $sandbox 'folders.json'); RECALL_LIBRARY_META = (Join-Path $sandbox 'library.json'); RECALL_CACHE_DIR = (Join-Path $sandbox 'cache'); RECALL_OUT_DIR = (Join-Path $sandbox 'clips'); APPDATA = $sandbox }
foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
try { $p = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -NoNewWindow -PassThru; [void]$p.Handle }
finally { foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }

Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class AudioShot {
  [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
  [StructLayout(LayoutKind.Sequential)] struct R { public int L, T, X, B; }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out R r);
  public static void Save(IntPtr h, string path) {
    SetProcessDPIAware(); R r; GetWindowRect(h, out r);
    int w = Math.Max(1, r.X - r.L), hh = Math.Max(1, r.B - r.T);
    using (var b = new Bitmap(w, hh)) using (var g = Graphics.FromImage(b)) {
      IntPtr dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); b.Save(path);
    }
  }
}
'@
try {
  $t = Get-Date
  while (-not $p.MainWindowHandle -or $p.MainWindowHandle -eq 0) {
    Start-Sleep -Milliseconds 100; $p.Refresh()
    if ($p.HasExited -or ((Get-Date) - $t).TotalSeconds -gt 20) { throw "no window (exited: $($p.HasExited))" }
  }
  Start-Sleep -Seconds 6 # config over the pipe, device and app lists read
  New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null
  [AudioShot]::Save($p.MainWindowHandle, $Out)
  "saved $Out"
} finally {
  if (-not $p.HasExited) { [void]$p.CloseMainWindow(); [void]$p.WaitForExit(5000) }
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } # our own instance only
  Stop-Job $server -ErrorAction SilentlyContinue; Remove-Job $server -Force -ErrorAction SilentlyContinue
  Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue
}
