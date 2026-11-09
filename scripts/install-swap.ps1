# All-or-nothing replacement of the installed files (dot-sourced by install.ps1; tested on a sandbox by
# scripts/dev/install-swap-check.ps1). A copy straight over the install folder could leave old and new
# programs mixed when one file is locked (Recall reopened mid-install), which is a broken app.
#  1. `.installing` marker for the whole swap: the app waits while it exists. It holds this pid and this
#     process's start time ("<pid> <FILETIME>"), so a marker left by a killed install is not mistaken
#     for a running one after the pid is reused (recall-native install_wait.rs).
#  2. Every staged file is copied into `<dir>\.new` (same volume, so the swap is a rename) and verified.
#  3. Each installed file moves to `<dir>\.old`, then its new copy moves in (MoveFileEx, replace-existing,
#     write-through). A running program can be renamed; a file opened without sharing cannot.
#  4. Any failure puts every file back from `.old` (new files that had no old one are removed): the folder
#     is all old or all new. Success removes `.old` (a file still in use stays there for the next install).
Add-Type -Namespace InstallSwap -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern bool MoveFileExW(string from, string to, uint flags);
[DllImport("kernel32.dll")]
public static extern bool GetProcessTimes(IntPtr process, out long created, out long exited, out long kernel, out long user);
'@

function Move-Replace([string]$from, [string]$to) {
  # MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
  if (-not [InstallSwap.Native]::MoveFileExW($from, $to, 0x1 -bor 0x8)) {
    throw "move $from -> ${to}: error $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
  }
}

function Get-Sha([string]$f) {
  $h = [System.Security.Cryptography.SHA256]::Create(); $s = [IO.File]::OpenRead($f)
  try { [BitConverter]::ToString($h.ComputeHash($s)) } finally { $s.Dispose(); $h.Dispose() }
}

function Install-Swap([string]$Stage, [string]$Dir) {
  New-Item -ItemType Directory -Force -Path $Dir | Out-Null
  $marker = Join-Path $Dir '.installing'
  $created = 0L; $e = 0L; $k = 0L; $u = 0L
  $me = [Diagnostics.Process]::GetCurrentProcess()
  $value = if ([InstallSwap.Native]::GetProcessTimes($me.Handle, [ref]$created, [ref]$e, [ref]$k, [ref]$u)) { "$PID $created" } else { "$PID" }
  Set-Content -Path $marker -Value $value -NoNewline
  $new = Join-Path $Dir '.new'; $old = Join-Path $Dir '.old'
  try {
    Remove-Item -LiteralPath $new -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path $old) { Get-ChildItem $old -File | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue } }
    New-Item -ItemType Directory -Force -Path $new, $old | Out-Null
    $files = @(Get-ChildItem $Stage -File)
    foreach ($f in $files) {
      Copy-Item -LiteralPath $f.FullName -Destination $new -Force
      if ((Get-Sha (Join-Path $new $f.Name)) -ne (Get-Sha $f.FullName)) { throw "staged copy of $($f.Name) does not match the build" }
    }
    $done = New-Object System.Collections.Generic.List[object]
    try {
      foreach ($f in $files) {
        $target = Join-Path $Dir $f.Name; $backup = Join-Path $old $f.Name
        $had = Test-Path -LiteralPath $target
        if ($had) { Move-Replace $target $backup }
        $done.Add([pscustomobject]@{ Target = $target; Backup = $backup; Had = $had })
        Move-Replace (Join-Path $new $f.Name) $target
      }
    } catch {
      $why = $_.Exception.Message
      for ($i = $done.Count - 1; $i -ge 0; $i--) {
        $d = $done[$i]
        if ($d.Had) { if (Test-Path -LiteralPath $d.Backup) { Move-Replace $d.Backup $d.Target } }
        elseif (Test-Path -LiteralPath $d.Target) { Remove-Item -LiteralPath $d.Target -Force }
      }
      throw "install rolled back, the previous version is unchanged ($why)"
    }
    Get-ChildItem $old -File | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
  } finally {
    Remove-Item -LiteralPath $new -Recurse -Force -ErrorAction SilentlyContinue
    if (-not (Get-ChildItem $old -Force -ErrorAction SilentlyContinue)) { Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue
  }
}
