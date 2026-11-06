# Proves scripts/install-swap.ps1 on a sandbox folder (never the real install): a clean swap leaves every
# file new; a file locked mid-swap (Recall reopened while installing) leaves every file old, with no new
# file left behind, and no .new/.old/.installing litter either way. Prints PASS/FAIL; exit 0 = all PASS.
$ErrorActionPreference = 'Stop'
. (Join-Path (Split-Path $PSScriptRoot -Parent) 'install-swap.ps1')
$root = Join-Path $env:TEMP "recall-swap-check-$PID"
$inst = Join-Path $root 'inst'; $stage = Join-Path $root 'stage'
$fail = 0
function Say([bool]$ok, [string]$what) { "$(if ($ok) { 'PASS' } else { 'FAIL' }) install swap: $what"; if (-not $ok) { $script:fail++ } }
function Reset {
  Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force $inst, $stage | Out-Null
  foreach ($n in 'a-native.exe', 'b-av.dll', 'c-recall.exe') { Set-Content (Join-Path $inst $n) "old $n" -NoNewline; Set-Content (Join-Path $stage $n) "new $n" -NoNewline }
  Set-Content (Join-Path $stage 'd-added.dll') 'new d-added.dll' -NoNewline   # a file the old version did not have
}
function State { (Get-ChildItem $inst -File | Sort-Object Name | ForEach-Object { "$($_.Name)=$(Get-Content $_.FullName -Raw)" }) -join '; ' }
function Litter { @('.new', '.old', '.installing' | Where-Object { Test-Path (Join-Path $inst $_) }) }
try {
  # 1. A clean swap: everything new.
  Reset
  Install-Swap $stage $inst
  $s = State
  Say ($s -eq 'a-native.exe=new a-native.exe; b-av.dll=new b-av.dll; c-recall.exe=new c-recall.exe; d-added.dll=new d-added.dll') "a clean swap leaves every file new ($s)"
  Say (-not (Litter)) "no litter after success ($((Litter) -join ','))"

  # 2. The third file is locked (opened with no sharing) after two have moved: everything old again.
  Reset
  $lock = [IO.File]::Open((Join-Path $inst 'c-recall.exe'), 'Open', 'Read', 'None')
  $err = ''
  try { Install-Swap $stage $inst } catch { $err = $_.Exception.Message } finally { $lock.Dispose() }
  $s = State
  Say ($err -like 'install rolled back*') "the locked file stops the swap with an error ($err)"
  Say ($s -eq 'a-native.exe=old a-native.exe; b-av.dll=old b-av.dll; c-recall.exe=old c-recall.exe') "a mid-swap failure leaves every file old, the added one removed ($s)"
  Say (-not (Litter)) "no litter after the rollback ($((Litter) -join ','))"

  # 3. A program still RUNNING (reopened mid-install) can be renamed: the swap completes, the running
  #    old copy stays in .old until the next install, and the folder is all new.
  Reset
  Copy-Item "$env:windir\System32\PING.EXE" (Join-Path $inst 'c-recall.exe') -Force
  $run = Start-Process (Join-Path $inst 'c-recall.exe') -ArgumentList '-n', '30', '127.0.0.1' -WindowStyle Hidden -PassThru
  try { Install-Swap $stage $inst; $err = '' } catch { $err = $_.Exception.Message }
  $s = State
  Say (-not $err -and $s -eq 'a-native.exe=new a-native.exe; b-av.dll=new b-av.dll; c-recall.exe=new c-recall.exe; d-added.dll=new d-added.dll') "a running program does not block the swap: all new ($s $err)"
  Say (Test-Path (Join-Path $inst '.old\c-recall.exe')) 'the running old copy waits in .old'
  Stop-Process -Id $run.Id -Force -ErrorAction SilentlyContinue; $run.WaitForExit(5000) | Out-Null
} finally {
  Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
exit $(if ($fail) { 1 } else { 0 })
