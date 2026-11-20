<#
  Saves pictures of the clip library's scroll bar from a scripted, offscreen app: at rest, while
  hovered, and after a press on the track plus a drag (mouse messages posted to the app's own
  window, never the desktop). The library is a sandbox folder of empty made-up clips, so the
  person's clips, settings and recorder are never touched. Offscreen apps open no meters and
  play nothing.

  powershell -NoProfile -File scripts\dev\scrollbar-shot.ps1 -Exe <recall-native.exe> -OutDir <dir> [-Sizes 1280x900,1500x640] [-Route library|settings-recording]
#>
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$OutDir, [string[]]$Sizes = @('1280x900', '1500x640'), [string]$Route = 'library')
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class BarShot {
  [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, X, B; }
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out R r);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out R r);
  public static void Save(IntPtr h, string path) {
    SetProcessDPIAware(); R r; GetWindowRect(h, out r);
    int w = Math.Max(1, r.X - r.L), hh = Math.Max(1, r.B - r.T);
    using (var b = new Bitmap(w, hh)) using (var g = Graphics.FromImage(b)) {
      IntPtr dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); b.Save(path);
    }
  }
  public static void Mouse(IntPtr h, uint msg, int x, int y, int keys) {
    PostMessage(h, msg, (IntPtr)keys, (IntPtr)((y << 16) | (x & 0xFFFF)));
  }
}
'@
$WM_MOUSEMOVE = 0x200; $WM_LBUTTONDOWN = 0x201; $WM_LBUTTONUP = 0x202; $MK_LBUTTON = 1

$sandbox = Join-Path ([IO.Path]::GetTempPath()) "recall-scrollbar-shot-$PID"
$clips = Join-Path $sandbox 'clips'
New-Item -ItemType Directory -Force -Path $clips, $OutDir | Out-Null
# Made-up game names and dates: 12 days of 10 clips, enough rows to scroll at any size.
$games = @('ExampleGame', 'SampleArena', 'TestRacer')
for ($d = 0; $d -lt 12; $d++) {
  $day = (Get-Date '2026-10-01').AddDays($d).ToString('yyyy-MM-dd')
  for ($i = 0; $i -lt 10; $i++) {
    $name = '{0}__{1}__{2:D2}-{3:D2}-00.mp4' -f $games[$i % 3], $day, (10 + $i), (5 * $i)
    [IO.File]::WriteAllBytes((Join-Path $clips $name), [byte[]]@())
  }
}

function Shot([string]$size) {
  $tag = $size -replace 'x', '-'
  $vars = @{ RECALL_NATIVE_OFFSCREEN = '1'; RECALL_NATIVE_ROUTE = $Route; RECALL_NO_CONTROLLER = '1';
    RECALL_CONTROL_PIPE = "recall-scrollbar-shot-$PID-$tag"; RECALL_NATIVE_WINDOW_SIZE = $size;
    RECALL_INSTANCE_TAG = "scrollbar-shot-$PID-$tag"; RECALL_FOLDERS = (Join-Path $sandbox 'folders.json'); RECALL_LIBRARY_META = (Join-Path $sandbox 'library.json');
    RECALL_CACHE_DIR = (Join-Path $sandbox 'cache'); RECALL_OUT_DIR = $clips; APPDATA = $sandbox; LOCALAPPDATA = $sandbox }
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
    $h = $p.MainWindowHandle
    Start-Sleep -Seconds 5 # listing read, rows laid out
    [BarShot]::Save($h, (Join-Path $OutDir "$Route-$tag-rest.png"))
    $r = New-Object BarShot+R; [void][BarShot]::GetClientRect($h, [ref]$r)
    $scale = [BarShot]::GetDpiForWindow($h) / 96.0
    # The bar's middle: 8 px shell gap + 3 px bar gap + 4 px half thumb, from the right edge.
    $x = $r.X - [int](15 * $scale)
    [BarShot]::Mouse($h, $WM_MOUSEMOVE, $x, [int]($r.B / 2), 0)
    Start-Sleep -Milliseconds 600
    [BarShot]::Save($h, (Join-Path $OutDir "$Route-$tag-hover.png"))
    # Press on the track two thirds down (the thumb jumps there), then drag a little further.
    $y0 = [int]($r.B * 0.66); $y1 = [int]($r.B * 0.75)
    [BarShot]::Mouse($h, $WM_LBUTTONDOWN, $x, $y0, $MK_LBUTTON)
    Start-Sleep -Milliseconds 300
    foreach ($y in $y0..$y1) { if ($y % 8 -eq 0) { [BarShot]::Mouse($h, $WM_MOUSEMOVE, $x, $y, $MK_LBUTTON); Start-Sleep -Milliseconds 15 } }
    Start-Sleep -Milliseconds 400
    [BarShot]::Save($h, (Join-Path $OutDir "$Route-$tag-dragging.png"))
    [BarShot]::Mouse($h, $WM_LBUTTONUP, $x, $y1, 0)
    # Pointer off the bar: back to the resting look, still scrolled.
    [BarShot]::Mouse($h, $WM_MOUSEMOVE, [int]($r.X / 2), [int]($r.B / 2), 0)
    Start-Sleep -Milliseconds 600
    [BarShot]::Save($h, (Join-Path $OutDir "$Route-$tag-after.png"))
    "saved $Route-$tag-*.png (scale $scale)"
  } finally {
    if (-not $p.HasExited) { [void]$p.CloseMainWindow(); [void]$p.WaitForExit(5000) }
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } # our own instance only
  }
}

try {
  foreach ($s in $Sizes) { Shot $s }
} finally {
  Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue
}
