<#
  Saves a picture of the update prompt (services/update.rs, views/update_card.rs) from an offscreen
  app with a fixture release: no network, no recorder, no real config (sandboxed like audio-page-shot.ps1).

  powershell -NoProfile -File scripts\dev\update-card-shot.ps1 -Exe <recall-native.exe> -Out <png> [-Size 1280x900]
#>
#  -Route settings -NoCard: the Settings → General page with the About rows instead.
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Out, [string]$Size = '1280x900', [string]$Route = '', [switch]$NoCard)
$ErrorActionPreference = 'Stop'
$sandbox = Join-Path ([IO.Path]::GetTempPath()) "recall-update-shot-$PID"
New-Item -ItemType Directory -Force -Path $sandbox | Out-Null
$notes = "## New`n- **Recall Island** shows the clip length after a save`n- Alt+N saves start 40 ms sooner`n## Fixed`n- Exports no longer stall on a full SSD cache`n- The editor keeps its zoom after a trim"
$release = @{ tag_name = 'v1.1.0'; name = ''; body = $notes; draft = $false; prerelease = $false
  assets = @(@{ name = 'Recall-1.1.0-win64.zip'; url = 'https://example.invalid/zip'; size = 61865984 }, @{ name = 'SHA256SUMS.txt'; url = 'https://example.invalid/sums'; size = 90 }) }
$fixture = Join-Path $sandbox 'release.json'
$release | ConvertTo-Json -Depth 5 | Set-Content $fixture
$saved = @{}
$vars = @{ RECALL_NATIVE_OFFSCREEN = '1'; RECALL_NO_CONTROLLER = '1'; RECALL_CONTROL_PIPE = "recall-update-shot-$PID"; RECALL_NATIVE_WINDOW_SIZE = $Size;
  RECALL_UPDATE_PREVIEW = $fixture; LOCALAPPDATA = $sandbox; RECALL_INSTANCE_TAG = "update-shot-$PID"
  RECALL_FOLDERS = (Join-Path $sandbox 'folders.json'); RECALL_LIBRARY_META = (Join-Path $sandbox 'library.json'); RECALL_CACHE_DIR = (Join-Path $sandbox 'cache'); RECALL_OUT_DIR = (Join-Path $sandbox 'clips'); APPDATA = $sandbox }
if ($Route) { $vars.RECALL_NATIVE_ROUTE = $Route }
if ($NoCard) { $vars.RECALL_UPDATE_PREVIEW = '' }
foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
try { $p = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -NoNewWindow -PassThru; [void]$p.Handle }
finally { foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }

Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class UpdateShot {
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
  # timer-ok: same settle time audio-page-shot.ps1 uses for a first frame with fonts loaded.
  Start-Sleep -Seconds 4
  New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null
  [UpdateShot]::Save($p.MainWindowHandle, $Out)
  "saved $Out"
} finally {
  if (-not $p.HasExited) { [void]$p.CloseMainWindow(); [void]$p.WaitForExit(5000) }
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } # our own instance only
  Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue
}
