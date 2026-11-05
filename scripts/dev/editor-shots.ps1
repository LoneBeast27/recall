<#
  Editor screenshots at given window sizes (layout checks): opens a clip in a private, off-screen,
  silent Recall instance (viewer-check-lib.ps1) per size and saves the window as a PNG.

    powershell -NoProfile -File scripts\dev\editor-shots.ps1 -Clip <mp4> -Out <dir>
      [-Exe <recall-native.exe>] [-Sizes 'portrait=1410x2468','landscape=2560x1440']
#>
param([Parameter(Mandatory)] [string]$Clip, [Parameter(Mandatory)] [string]$Out, [string]$Exe = '',
  [string[]]$Sizes = @('portrait=1410x2468', 'landscape=2560x1440'), [int]$SettleMs = 3000)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
New-Item -ItemType Directory -Force -Path $Out | Out-Null
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class EditorShot {
  [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
  [StructLayout(LayoutKind.Sequential)] struct R { public int L, T, X, B; }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out R r);
  public static string Save(IntPtr h, string path) {
    SetProcessDPIAware(); R r; GetWindowRect(h, out r);
    int w = r.X - r.L, ht = r.B - r.T;
    using (var b = new Bitmap(w, ht)) using (var g = Graphics.FromImage(b)) {
      IntPtr dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); b.Save(path);
    }
    return w + "x" + ht;
  }
}
'@
foreach ($entry in @($Sizes | ForEach-Object { $_ -split ',' } | Where-Object { $_ })) {
  $name, $size = $entry -split '=', 2
  $script:windowSize = $size
  $s = Start-Viewer "shot-$name" $Clip
  try {
    Start-Sleep -Milliseconds $SettleMs
    $path = Join-Path $Out "$name.png"
    $pixels = [EditorShot]::Save($s.Hwnd, $path)
    "${name}: window $size -> $path ($pixels px)"
  } finally { Stop-Viewer $s }
}
$script:windowSize = $null
