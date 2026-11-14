<#
  Saves pictures of Settings > General, Recording and Help from a scripted, offscreen app.
  A private control pipe serves an empty fixture config, so the real recorder and config are
  never touched. Help shows a fixture fix offer and an applied fix (RECALL_FIXES_PREVIEW, read
  only; offscreen builds never apply a fix). Offscreen apps open no meters and play nothing.

  powershell -NoProfile -File scripts\dev\settings-pages-shot.ps1 -Exe <recall-native.exe> -OutDir <dir> [-Size 1280x1000]
#>
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$OutDir, [string]$Size = '1280x1000')
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class SettingsShot {
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

$sandbox = Join-Path ([IO.Path]::GetTempPath()) "recall-settings-shot-$PID"
$preview = Join-Path $sandbox 'fixes'
New-Item -ItemType Directory -Force -Path (Join-Path $preview 'offers'), (Join-Path $preview 'applied'), $OutDir | Out-Null
# A made-up Java path: no real user or launcher folder.
$java = 'C:\Games\Launcher\java\jre25\bin\javaw.exe'
$plan = @{ fix = 'MinecraftOverlayNvidia'; steps = @(@{ change = @{ DriverAppSetting = @{ vendor = 'Nvidia'; exe = $java; setting_id = 0x20D690F8; setting_name = 'Vulkan/OpenGL present method'; new_profile = 'Recall: Minecraft 1a2b3c4d'; entry = 'jre25/bin/javaw.exe'; to = @(1, 'Prefer layered on DXGI Swapchain') } } }) }
$offer = @{ plan = $plan; target = $java; seen_unix = 1794500000 }
[IO.File]::WriteAllText((Join-Path $preview 'offers\minecraft-overlay-nvidia-preview.json'), ($offer | ConvertTo-Json -Depth 12))
$java2 = 'C:\Games\Other\java\jre21\bin\javaw.exe'
$plan2 = @{ fix = 'MinecraftOverlayNvidia'; steps = @(@{ change = @{ DriverAppSetting = @{ vendor = 'Nvidia'; exe = $java2; setting_id = 0x20D690F8; setting_name = 'Vulkan/OpenGL present method'; new_profile = 'Recall: Minecraft 5e6f7a8b'; entry = 'jre21/bin/javaw.exe'; to = @(1, 'Prefer layered on DXGI Swapchain') } } }) }
$receipt = @{ fix = 'MinecraftOverlayNvidia'; target = $java2; applied_unix = 1794490000; plan = $plan2; undo = @(@{ DriverOwnProfile = @{ vendor = 'Nvidia'; exe = $java2; setting_id = 0x20D690F8; value = 1; profile = 'Recall: Minecraft 5e6f7a8b' } }) }
[IO.File]::WriteAllText((Join-Path $preview 'applied\minecraft-overlay-nvidia-preview.json'), ($receipt | ConvertTo-Json -Depth 12))

function Shot([string]$route, [string]$name, [bool]$open, [string]$winSize = $Size) {
  $pipe = "recall-settings-shot-$PID-$name"
  $line = (@{ mod = 'session'; event = @{ type = 'config'; config = @{ recording = @{ buffer_secs = 120 }; midi = @{ input = '' } } } } | ConvertTo-Json -Depth 8 -Compress)
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
  $vars = @{ RECALL_NATIVE_OFFSCREEN = '1'; RECALL_NATIVE_ROUTE = $route; RECALL_NO_CONTROLLER = '1'; RECALL_CONTROL_PIPE = $pipe; RECALL_NATIVE_WINDOW_SIZE = $winSize;
    RECALL_INSTANCE_TAG = "settings-shot-$PID-$name"; RECALL_FOLDERS = (Join-Path $sandbox 'folders.json'); RECALL_LIBRARY_META = (Join-Path $sandbox 'library.json');
    RECALL_CACHE_DIR = (Join-Path $sandbox 'cache'); RECALL_OUT_DIR = (Join-Path $sandbox 'clips'); APPDATA = $sandbox; LOCALAPPDATA = $sandbox;
    RECALL_FIXES_PREVIEW = $preview; RECALL_FIXES_PREVIEW_OPEN = $(if ($open) { '1' } else { $null }) }
  $saved = @{}
  foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
  try { $p = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -NoNewWindow -PassThru; [void]$p.Handle }
  finally { foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }
  try {
    $t = Get-Date
    while (-not $p.MainWindowHandle -or $p.MainWindowHandle -eq 0) {
      Start-Sleep -Milliseconds 100; $p.Refresh()
      if ($p.HasExited -or ((Get-Date) - $t).TotalSeconds -gt 20) { throw "no window (exited: $($p.HasExited))" }
    }
    Start-Sleep -Seconds 6 # config over the pipe, device lists read
    $out = Join-Path $OutDir "$name.png"
    [SettingsShot]::Save($p.MainWindowHandle, $out)
    "saved $out"
  } finally {
    if (-not $p.HasExited) { [void]$p.CloseMainWindow(); [void]$p.WaitForExit(5000) }
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } # our own instance only
    Stop-Job $server -ErrorAction SilentlyContinue; Remove-Job $server -Force -ErrorAction SilentlyContinue
  }
}

try {
  Shot 'settings' 'settings-general' $false
  Shot 'settings-recording' 'settings-recording' $false '1280x2400'
  Shot 'settings-help' 'settings-help' $false
  Shot 'settings-help' 'settings-help-info-open' $true
} finally {
  Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue
}
