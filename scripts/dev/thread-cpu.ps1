<#
  Per-thread CPU of one process over a window: name (SetThreadDescription, e.g. Rust thread
  names), start module (for unnamed driver / pool threads), user + kernel CPU-ms, as a table
  and optionally JSON. No admin rights, nothing injected: it only reads thread times.

    powershell -NoProfile -File scripts\dev\thread-cpu.ps1 <pid> [seconds=30] [out.json]
  Or dot-source it and call Measure-ThreadCpu (no param block on purpose: dot-sourcing a
  script with params rebinds the caller's same-named variables).
#>

if (-not ('RecallDev.ThreadCpu' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
namespace RecallDev {
  public class ThreadSample {
    public int Tid; public string Name; public string Module;
    public long UserTicks; public long KernelTicks;
  }
  public static class ThreadCpu {
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenThread(int access, bool inherit, int tid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll")] static extern int GetThreadDescription(IntPtr h, out IntPtr desc);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
    [DllImport("kernel32.dll")] static extern bool GetThreadTimes(IntPtr h, out long c, out long e, out long k, out long u);
    [DllImport("ntdll.dll")] static extern int NtQueryInformationThread(IntPtr h, int cls, out IntPtr info, int len, IntPtr ret);
    const int QUERY_LIMITED = 0x0800, QUERY = 0x0040;

    public static List<ThreadSample> Sample(int pid) {
      var outp = new List<ThreadSample>();
      Process p;
      try { p = Process.GetProcessById(pid); } catch { return outp; }
      var mods = new List<Tuple<long, long, string>>();
      try { foreach (ProcessModule m in p.Modules) mods.Add(Tuple.Create((long)m.BaseAddress, (long)m.ModuleMemorySize, m.ModuleName)); } catch { }
      foreach (ProcessThread t in p.Threads) {
        var s = new ThreadSample { Tid = t.Id, Name = "", Module = "?" };
        IntPtr h = OpenThread(QUERY_LIMITED | QUERY, false, t.Id);
        if (h == IntPtr.Zero) h = OpenThread(QUERY_LIMITED, false, t.Id);
        if (h == IntPtr.Zero) continue;
        try {
          IntPtr d;
          if (GetThreadDescription(h, out d) >= 0 && d != IntPtr.Zero) { s.Name = Marshal.PtrToStringUni(d) ?? ""; LocalFree(d); }
          long c, e, k, u;
          if (GetThreadTimes(h, out c, out e, out k, out u)) { s.UserTicks = u; s.KernelTicks = k; }
          IntPtr start;
          if (NtQueryInformationThread(h, 9, out start, IntPtr.Size, IntPtr.Zero) == 0) {
            long a = (long)start;
            foreach (var m in mods) if (a >= m.Item1 && a < m.Item1 + m.Item2) { s.Module = m.Item3; break; }
          }
        } finally { CloseHandle(h); }
        outp.Add(s);
      }
      return outp;
    }
  }
}
'@
}

function Measure-ThreadCpu([int]$ProcessId, [double]$Seconds) {
  $a = @{}; foreach ($s in [RecallDev.ThreadCpu]::Sample($ProcessId)) { $a[$s.Tid] = $s }
  $proc = Get-Process -Id $ProcessId -ErrorAction Stop
  $cpu0 = $proc.TotalProcessorTime.TotalMilliseconds
  $t0 = [Diagnostics.Stopwatch]::StartNew()
  Start-Sleep -Milliseconds ([int]($Seconds * 1000))
  $b = [RecallDev.ThreadCpu]::Sample($ProcessId)
  $proc.Refresh(); $cpu1 = $proc.TotalProcessorTime.TotalMilliseconds
  $wall = $t0.Elapsed.TotalSeconds
  $rows = foreach ($s in $b) {
    $u0 = 0; $k0 = 0; if ($a.ContainsKey($s.Tid)) { $u0 = $a[$s.Tid].UserTicks; $k0 = $a[$s.Tid].KernelTicks }
    [pscustomobject]@{
      tid = $s.Tid; name = $s.Name; module = $s.Module
      user_ms = [math]::Round(($s.UserTicks - $u0) / 1e4, 1)
      kernel_ms = [math]::Round(($s.KernelTicks - $k0) / 1e4, 1)
    }
  }
  $rows = $rows | ForEach-Object { $_ | Add-Member total_ms ($_.user_ms + $_.kernel_ms) -PassThru } | Sort-Object total_ms -Descending
  [pscustomobject]@{
    wall_s = [math]::Round($wall, 2)
    process_cpu_ms = [math]::Round($cpu1 - $cpu0, 0)
    cores = [math]::Round(($cpu1 - $cpu0) / 1000 / $wall, 3)
    cpu_s_per_min = [math]::Round(($cpu1 - $cpu0) / 1000 / $wall * 60, 1)
    threads = @($rows)
  }
}

if ($MyInvocation.InvocationName -ne '.' -and $args.Count -ge 1) {
  $tcPid = [int]$args[0]; $tcSeconds = if ($args.Count -ge 2) { [double]$args[1] } else { 30 }
  $tcJson = if ($args.Count -ge 3) { [string]$args[2] } else { '' }
  $r = Measure-ThreadCpu $tcPid $tcSeconds
  "pid $tcPid over $($r.wall_s) s: $($r.process_cpu_ms) CPU-ms = $($r.cores) cores = $($r.cpu_s_per_min) CPU-s/min"
  # Group unnamed pool threads by start module so 16 rayon/driver workers read as one line.
  $r.threads | Group-Object { if ($_.name) { $_.name } else { "<$($_.module)>" } } | ForEach-Object {
    [pscustomobject]@{
      thread = $_.Name; count = $_.Count
      user_ms = [math]::Round(($_.Group | Measure-Object user_ms -Sum).Sum, 0)
      kernel_ms = [math]::Round(($_.Group | Measure-Object kernel_ms -Sum).Sum, 0)
      total_ms = [math]::Round(($_.Group | Measure-Object total_ms -Sum).Sum, 0)
    }
  } | Sort-Object total_ms -Descending | Format-Table -AutoSize | Out-String -Width 200
  if ($tcJson) { $r | ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 $tcJson }
}
