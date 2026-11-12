<#
  Stand-in engines for slow-drive-check.ps1. A build that does not know a command ignores it and
  runs for good; these do exactly that, under names and in a folder of their own, so a check never
  starts anything that records. -Caps stamps the file the way a real engine is stamped.

  Each run appends "<pid> <arguments>" to RECALL_STUB_LOG. --export-edit waits RECALL_STUB_EXPORT_MS
  and fails. --prepare-editor goes to the engine in RECALL_STUB_REAL when that is set; anything
  else stays alive until it is killed.
#>
$script:stubSource = @'
using System; using System.Diagnostics; using System.IO; using System.Threading;
static class Stub {
  static int Main(string[] a) {
    string log = Environment.GetEnvironmentVariable("RECALL_STUB_LOG");
    if (log != null) File.AppendAllText(log, Process.GetCurrentProcess().Id + " " + string.Join(" ", a) + "\n");
    string first = a.Length > 0 ? a[0] : "";
    if (first == "--export-edit") { Thread.Sleep(int.Parse(Environment.GetEnvironmentVariable("RECALL_STUB_EXPORT_MS") ?? "0")); return 1; }
    string real = Environment.GetEnvironmentVariable("RECALL_STUB_REAL");
    if (first == "--prepare-editor" && !string.IsNullOrEmpty(real)) {
      string line = ""; foreach (string arg in a) line += "\"" + arg.TrimEnd('\\') + "\" ";
      Process p = Process.Start(new ProcessStartInfo(real, line) { UseShellExecute = false });
      p.WaitForExit(); return p.ExitCode;
    }
    Thread.Sleep(Timeout.Infinite); return 0;
  }
}
'@

function New-StubEngine([string]$name, [switch]$Caps) {
  $folder = Join-Path $script:work 'stubs'
  New-Item -ItemType Directory -Force -Path $folder | Out-Null
  $exe = Join-Path $folder "$name.exe"
  if (-not (Test-Path $exe)) {
    Add-Type -TypeDefinition $script:stubSource -OutputAssembly $exe -OutputType ConsoleApplication
    if ($Caps) {
      $stream = [IO.File]::Open($exe, 'Append')
      $stamp = [Text.Encoding]::ASCII.GetBytes('recall-engine-caps:prepare-editor,export-edit,upscale-notice;')
      $stream.Write($stamp, 0, $stamp.Length); $stream.Dispose()
    }
  }
  $exe
}

# The running stand-in that logged a line with $word, as a process; $null when none came.
function Wait-StubLog([string]$word, [int]$timeoutMs) {
  $t = Get-Date
  while (((Get-Date) - $t).TotalMilliseconds -lt $timeoutMs) {
    $line = @(Get-Content $env:RECALL_STUB_LOG -ErrorAction SilentlyContinue | Where-Object { $_ -like "*$word*" }) | Select-Object -Last 1
    if ($line) { $p = Get-Process -Id ([int]($line -split ' ')[0]) -ErrorAction SilentlyContinue; if ($p) { [void]$p.Handle; return $p } }
    Start-Sleep -Milliseconds 50
  }
  $null
}

# Ends stand-ins this run started (by the pids they logged), nothing else.
function Stop-Stubs([string]$name) {
  foreach ($line in @(Get-Content $env:RECALL_STUB_LOG -ErrorAction SilentlyContinue)) {
    $p = Get-Process -Id ([int]($line -split ' ')[0]) -ErrorAction SilentlyContinue
    if ($p -and $p.ProcessName -eq $name) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
  }
}

function Reset-Engine {
  $env:RECALL_ENGINE = $script:realEngine
  foreach ($v in 'RECALL_STUB_LOG', 'RECALL_STUB_REAL', 'RECALL_STUB_EXPORT_MS') { Remove-Item "Env:$v" -ErrorAction SilentlyContinue }
}
