<#
  Saves pictures of the update progress bar from a scripted, offscreen app: the update card
  mid-download, and Settings > Help with "Check now" in flight. Everything is private: its own
  instance tag, control pipe (no server: the real recorder is never reached), APPDATA,
  LOCALAPPDATA, TEMP and library files in a sandbox removed afterwards. Nothing is sent to
  GitHub or downloaded (the preview flags build the bar from a fixture release).

  powershell -NoProfile -File scripts\dev\update-progress-shot.ps1 -Exe <recall-native.exe> -OutDir <folder> [-Size 1280x900]
#>
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$OutDir, [string]$Size = '1280x900')
$ErrorActionPreference = 'Stop'
$sandbox = Join-Path ([IO.Path]::GetTempPath()) "recall-update-shot-$PID"
New-Item -ItemType Directory -Force -Path $sandbox, (Join-Path $sandbox 'Local'), (Join-Path $sandbox 'Roaming'), (Join-Path $sandbox 'Temp'), $OutDir | Out-Null
$release = @{ tag_name = 'v0.1.7'; name = 'Recall 0.1.7'; draft = $false; prerelease = $false
  body = "## New`n- **Update progress.** The update shows how far it is.`n## Fixed`n- **Check now** no longer times out on a slow GitHub server."
  assets = @(@{ name = 'RecallSetup.exe'; size = 15329280; state = 'uploaded'; digest = 'sha256:' + ('ab' * 32) }) }
$fixture = Join-Path $sandbox 'release.json'
[IO.File]::WriteAllText($fixture, ($release | ConvertTo-Json -Depth 5)) # no BOM: the app reads plain JSON

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

function Shot([string]$name, [hashtable]$extra) {
  $vars = @{ RECALL_NATIVE_OFFSCREEN = '1'; RECALL_NO_CONTROLLER = '1'; RECALL_CONTROL_PIPE = "recall-update-shot-none-$PID"; RECALL_NATIVE_WINDOW_SIZE = $Size
    RECALL_INSTANCE_TAG = "update-shot-$PID-$name"; RECALL_FOLDERS = (Join-Path $sandbox 'folders.json'); RECALL_LIBRARY_META = (Join-Path $sandbox 'library.json')
    RECALL_CACHE_DIR = (Join-Path $sandbox 'cache'); RECALL_OUT_DIR = (Join-Path $sandbox 'clips'); APPDATA = (Join-Path $sandbox 'Roaming')
    LOCALAPPDATA = (Join-Path $sandbox 'Local'); TEMP = (Join-Path $sandbox 'Temp'); TMP = (Join-Path $sandbox 'Temp') }
  foreach ($k in $extra.Keys) { $vars[$k] = $extra[$k] }
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
    Start-Sleep -Seconds 4
    $out = Join-Path $OutDir "$name.png"
    [UpdateShot]::Save($p.MainWindowHandle, $out)
    "saved $out"
  } finally {
    if (-not $p.HasExited) { [void]$p.CloseMainWindow(); [void]$p.WaitForExit(5000) }
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } # our own instance only
  }
}

try {
  Shot 'card-downloading' @{ RECALL_UPDATE_PREVIEW = $fixture; RECALL_UPDATE_PREVIEW_BYTES = '6400000' }
  Shot 'settings-check-now' @{ RECALL_NATIVE_ROUTE = 'settings-help'; RECALL_UPDATE_PREVIEW_ANSWERS = '1' }
  $left = @(Get-ChildItem -Recurse -File (Join-Path $sandbox 'Local') -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName.Substring($sandbox.Length) })
  "sandbox LOCALAPPDATA files: $($left.Count)"; $left | Select-Object -First 20
} finally {
  Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue
}
