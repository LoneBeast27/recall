<#
  Lists every window that appears on a monitor while a check runs: dev proof that the check shows
  nothing to the person at the PC. Polls all top-level windows, of every process, until the
  -Stop file exists, then writes one line per new window (process, title, bounds) to -Log and a
  final count. Windows on screen at the start are the baseline. Cloaked and zero-size windows do
  not count. Other programs can appear too: read the process names.

    powershell -NoProfile -WindowStyle Hidden -File scripts\dev\window-poll.ps1 -Log <txt> -Stop <file> [-KillTree <pid>]

  -KillTree stops that process tree the moment a check's own kind of window shows.
#>
param([Parameter(Mandatory)][string]$Log, [Parameter(Mandatory)][string]$Stop, [int]$EveryMs = 100, [int]$KillTree = 0)
Add-Type @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices; using System.Text;
public static class WP {
  public delegate bool Each(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, X, Y; }
  [DllImport("user32.dll")] static extern bool EnumWindows(Each f, IntPtr l);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
  [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int n);
  public static List<string> Shown() {
    var found = new List<string>();
    int left = GetSystemMetrics(76), top = GetSystemMetrics(77), right = left + GetSystemMetrics(78), bottom = top + GetSystemMetrics(79);
    EnumWindows((h, l) => {
      R r; int cloaked; uint pid;
      if (!IsWindowVisible(h) || !GetWindowRect(h, out r)) return true;
      if (r.X <= r.L || r.Y <= r.T || r.X <= left || r.L >= right || r.Y <= top || r.T >= bottom) return true;
      if (DwmGetWindowAttribute(h, 14, out cloaked, 4) == 0 && cloaked != 0) return true;
      var title = new StringBuilder(120); GetWindowText(h, title, 120); GetWindowThreadProcessId(h, out pid);
      found.Add(h.ToInt64() + "|" + pid + "|" + title + "|" + r.L + "," + r.T + "," + r.X + "," + r.Y);
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@
$seen = @{}
foreach ($w in [WP]::Shown()) { $seen[$w.Split('|')[0]] = $true }
$new = 0
Set-Content -Path $Log -Value "baseline $($seen.Count) windows at $(Get-Date -Format 'HH:mm:ss')"
while (-not (Test-Path $Stop)) {
  foreach ($w in [WP]::Shown()) {
    $f = $w.Split('|'); if ($seen[$f[0]]) { continue }
    $seen[$f[0]] = $true; $new++
    $name = (Get-Process -Id ([int]$f[1]) -ErrorAction SilentlyContinue).ProcessName
    Add-Content -Path $Log -Value "$(Get-Date -Format 'HH:mm:ss.fff') NEW $name pid $($f[1]) '$($f[2])' at $($f[3])"
    if ($KillTree -and $name -match '^(recall-native|recall|powershell|pwsh|conhost|OpenConsole|WindowsTerminal|ffmpeg|ffprobe|WerFault|cargo)$') { & taskkill /PID $KillTree /T /F | Out-Null; Add-Content -Path $Log -Value 'stopped the check'; exit 1 }
  }
  Start-Sleep -Milliseconds $EveryMs
}
Add-Content -Path $Log -Value "new visible windows: $new"
