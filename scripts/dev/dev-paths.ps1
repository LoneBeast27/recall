<#
  Machine-specific values for the dev tools. Nothing machine-specific lives in tracked files:
  each PC keeps its own values in .local/machine.json (gitignored), written by
  scripts/dev/init-machine.ps1. Template: .local.example/machine.example.json.

    . (Join-Path $PSScriptRoot 'dev-paths.ps1')
    $main = Get-MainCheckout $repo           # the main checkout (a worktree resolves to the one it hangs off)
    $dev  = Get-DevRoot $repo                # the folder that holds the main checkout
    $v    = Get-MachineValue $repo 'hdrGammaFix.stateDir'   # throws a clear error when missing

  Lookup of machine.json: $env:RECALL_MACHINE_JSON, else <repo>\.local\machine.json, else
  <main checkout>\.local\machine.json (so worktrees share the main checkout's file).
  Paths: RECALL_MAIN_CHECKOUT / RECALL_DEV_ROOT, else paths.mainCheckout / paths.devRoot in
  machine.json, else detected from git (the checkout that owns the shared .git folder).
#>
function Get-GitMainCheckout([string]$repo) {
  $common = & git -C $repo rev-parse --path-format=absolute --git-common-dir 2>$null
  if ($LASTEXITCODE -ne 0 -or -not $common) { throw "Not a git checkout: $repo (set RECALL_MAIN_CHECKOUT)" }
  Split-Path ($common -replace '/', '\') -Parent
}

function Find-MachineJson([string]$repo) {
  if ($env:RECALL_MACHINE_JSON) { return $env:RECALL_MACHINE_JSON }
  $own = Join-Path $repo '.local\machine.json'
  if (Test-Path $own) { return $own }
  $main = Join-Path (Get-GitMainCheckout $repo) '.local\machine.json'
  if (Test-Path $main) { return $main }
  $null
}

function Get-MachineConfig([string]$repo) {
  $path = Find-MachineJson $repo
  if (-not $path -or -not (Test-Path $path)) { return $null }
  Get-Content -Raw $path | ConvertFrom-Json
}

# Value at a dotted key ('hdrGammaFix.stateDir'), or $null.
function Find-MachineValue([string]$repo, [string]$key) {
  $node = Get-MachineConfig $repo
  foreach ($part in $key -split '\.') {
    if ($null -eq $node -or -not ($node.PSObject.Properties.Name -contains $part)) { return $null }
    $node = $node.$part
  }
  $node
}

# Value at a dotted key; a missing value is an error that says how to fix it (never a built-in default).
function Get-MachineValue([string]$repo, [string]$key) {
  $v = Find-MachineValue $repo $key
  if ($null -eq $v -or "$v" -eq '') {
    $where = Find-MachineJson $repo
    if (-not $where) { $where = Join-Path $repo '.local\machine.json (missing)' }
    throw "Machine value '$key' is not set in $where. Run scripts\dev\init-machine.ps1 on this PC (see .local.example\machine.example.json)."
  }
  $v
}

function Get-MainCheckout([string]$repo) {
  if ($env:RECALL_MAIN_CHECKOUT) { return $env:RECALL_MAIN_CHECKOUT }
  $v = Find-MachineValue $repo 'paths.mainCheckout'
  if ($v) { return $v }
  Get-GitMainCheckout $repo
}

function Get-DevRoot([string]$repo) {
  if ($env:RECALL_DEV_ROOT) { return $env:RECALL_DEV_ROOT }
  $v = Find-MachineValue $repo 'paths.devRoot'
  if ($v) { return $v }
  Split-Path (Get-MainCheckout $repo) -Parent
}
