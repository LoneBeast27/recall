<#
.SYNOPSIS
  Signs recall-update.unsigned.json with the release (or backup) key. Run it with the PC
  offline (network unplugged or Wi-Fi off) and the key on its USB stick.
.DESCRIPTION
  Refuses unless Setup still matches the manifest's size and SHA-256, the manifest's revision
  is the tag v<version> and is on main in this checkout's git (no network asked), and the
  network is off. Then it shows a one-screen summary and runs minisign, which asks for the key
  password. Writes recall-update.json and recall-update.json.minisig next to Setup.
.EXAMPLE
  .\sign-manifest.ps1 -Bundle .tmp\release\0.1.11 -SecretKey E:\keys\release-1.key
#>
param(
    [Parameter(Mandatory)][string]$Bundle,
    [Parameter(Mandatory)][string]$SecretKey,
    [string]$Minisign = 'minisign',
    # For a dry run on a throwaway key only (the checks still run).
    [switch]$AllowOnline
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$unsigned = Join-Path $Bundle 'recall-update.unsigned.json'
$m = Get-Content -LiteralPath $unsigned -Raw | ConvertFrom-Json
$setup = Join-Path $Bundle 'RecallSetup.exe'
$sha = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
if ($sha -ne $m.setup.sha256 -or (Get-Item -LiteralPath $setup).Length -ne $m.setup.size) {
    throw 'Setup does not match the manifest: make it again with make-manifest.ps1.'
}
$tagCommit = (git -C $repo rev-parse "v$($m.version)^{commit}" 2>$null)
if ($LASTEXITCODE -ne 0 -or $tagCommit -ne $m.revision) { throw "Tag v$($m.version) is not the manifest's revision $($m.revision)." }
git -C $repo merge-base --is-ancestor $m.revision main
if ($LASTEXITCODE -ne 0) { throw "Revision $($m.revision) is not on main." }
$online = @(Get-NetAdapter -Physical -ErrorAction SilentlyContinue | Where-Object Status -eq 'Up')
if ($online.Count -gt 0 -and !$AllowOnline) {
    throw "A network adapter is up ($($online.Name -join ', ')). Unplug the network or turn Wi-Fi off, then run this again."
}
''
"  Version     $($m.version)   (sequence $($m.sequence))"
"  Revision    $($m.revision)"
"  Supersedes  $(if ($m.supersedes) { $m.supersedes -join ', ' } else { 'none' })"
"  Setup       $($m.setup.size) bytes, SHA-256 $($m.setup.sha256)"
"  Addresses   $($m.setup.urls -join "`n              ")"
"  Revokes     $(if ($m.revoke_keys) { $m.revoke_keys -join ', ' } else { 'none' })"
''
$answer = Read-Host 'Sign this update? Type yes'
if ($answer -ne 'yes') { throw 'Not signed.' }
$signed = Join-Path $Bundle 'recall-update.json'
Copy-Item -LiteralPath $unsigned -Destination $signed -Force
& $Minisign -S -s $SecretKey -m $signed -t "recall $($m.version) sequence $($m.sequence)"
if ($LASTEXITCODE -ne 0) { throw 'minisign did not sign.' }
"Signed: $signed and $signed.minisig"
