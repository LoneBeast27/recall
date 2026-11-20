<#
  Full screen check: the preview's button, F, Escape and F11 really move the window onto its
  monitor and back. A private, silent Recall instance (viewer-check-lib.ps1) is allowed to leave
  its off-screen spot (RECALL_NATIVE_FULL_LIVE); it stays at the bottom of the z-order and never
  activates, so it fills the monitor behind every other window.

    powershell -NoProfile -File scripts\dev\fullscreen-check.ps1 -Clip <mp4> -ShowsOnMonitor [-Exe <recall-native.exe>] [-Shots <dir>]

  VISIBLE: the window fills a real monitor. Never part of a gate or a routine run.
#>
param([Parameter(Mandatory)] [string]$Clip, [string]$Exe = '', [string]$Shots = '', [switch]$ShowsOnMonitor)
$ErrorActionPreference = 'Stop'
# This check fills a real monitor with a test window. It runs only for a run the user said yes to, and the switch says so.
if (-not $ShowsOnMonitor) { throw 'fullscreen-check fills a real monitor: run it only with the owner''s yes for this run, and pass -ShowsOnMonitor' }
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
$script:vk.f = 0x46; $script:vk.f11 = 0x7A
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class FullWin {
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, X, B; }
  [StructLayout(LayoutKind.Sequential)] struct MI { public int Size; public R Monitor, Work; public uint Flags; }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr h, uint flags);
  [DllImport("user32.dll")] static extern bool GetMonitorInfo(IntPtr m, ref MI info);
  [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  public static string Rect(IntPtr h) { SetProcessDPIAware(); R r; GetWindowRect(h, out r); return r.L + "," + r.T + "," + r.X + "," + r.B; }
  public static string Monitor(IntPtr h) { SetProcessDPIAware(); var i = new MI(); i.Size = Marshal.SizeOf(i); GetMonitorInfo(MonitorFromWindow(h, 2), ref i); return i.Monitor.L + "," + i.Monitor.T + "," + i.Monitor.X + "," + i.Monitor.B; }
  public static IntPtr Foreground() { return GetForegroundWindow(); }
  public static void Save(IntPtr h, string path) {
    SetProcessDPIAware(); R r; GetWindowRect(h, out r);
    using (var b = new Bitmap(r.X - r.L, r.B - r.T)) using (var g = Graphics.FromImage(b)) { IntPtr dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); b.Save(path); }
  }
}
'@
$env:RECALL_NATIVE_FULL_LIVE = '1'
$script:windowSize = '1600x1000'
$s = Start-Viewer 'full-live' $Clip
$fg = [FullWin]::Foreground()
function State([string]$label) {
  Start-Sleep -Milliseconds 700
  $r = Last $s
  [pscustomobject]@{ Label = $label; Rect = [FullWin]::Rect($s.Hwnd); Monitor = [FullWin]::Monitor($s.Hwnd)
    TitleBar = $null -ne $r.targets.'crumb-library'; Button = $null -ne $r.targets.'video-full'; Route = $r.route }
}
function Shot([string]$name) { if ($Shots) { New-Item -ItemType Directory -Force $Shots | Out-Null; [FullWin]::Save($s.Hwnd, (Join-Path $Shots "$name.png")) } }
try {
  $start = State 'start'
  $checks = @((Ok ($start.Rect -ne $start.Monitor -and $start.TitleBar) "starts windowed with a title bar ($($start.Rect))"))
  Click $s 'video-full'; $v = State 'button'; Shot 'video-full'
  $checks += Ok ($v.Rect -eq $v.Monitor -and -not $v.TitleBar -and $v.Button) "button: fills the monitor $($v.Monitor), no title bar ($($v.Rect))"
  Key $s 'escape'; $e = State 'escape'
  $checks += Ok ($e.Rect -eq $start.Rect -and $e.TitleBar -and $e.Route -eq 'editor') "Escape: back to $($e.Rect), still in the editor"
  Key $s 'f'; $f1 = State 'f'
  $checks += Ok ($f1.Rect -eq $f1.Monitor -and -not $f1.TitleBar) "F: video full screen"
  Key $s 'f'; $f2 = State 'f again'
  $checks += Ok ($f2.Rect -eq $start.Rect -and $f2.TitleBar) "F again: back to $($f2.Rect)"
  Key $s 'f11'; $w = State 'f11'; Shot 'window-full'
  $checks += Ok ($w.Rect -eq $w.Monitor -and $w.TitleBar) "F11: window fills the monitor with its title bar"
  Key $s 'f'; $wv = State 'f11 then f'
  $checks += Ok ($wv.Rect -eq $wv.Monitor -and -not $wv.TitleBar) "F from F11: video only, still full"
  Key $s 'f'; $wb = State 'back to f11'
  $checks += Ok ($wb.Rect -eq $wb.Monitor -and $wb.TitleBar) "F again: back to the F11 window full screen"
  Key $s 'f11'; $end = State 'f11 off'
  $checks += Ok ($end.Rect -eq $start.Rect -and $end.TitleBar) "F11 again: back to $($end.Rect)"
  $checks += Ok ([FullWin]::Foreground() -eq $fg) "the window in front never changed"
  $checks | ForEach-Object { '{0}  {1}' -f $(if ($_.Pass) { 'PASS' } else { 'FAIL' }), $_.Detail }
  if ($checks | Where-Object { -not $_.Pass }) { exit 1 }
} finally { Stop-Viewer $s; Remove-Item Env:RECALL_NATIVE_FULL_LIVE -ErrorAction SilentlyContinue }
