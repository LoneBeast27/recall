<#
.SYNOPSIS
  One step from a verified release bundle to a published, signed update: checks the bundle, takes
  the release key from Infisical for one signature, verifies it, publishes, reads every copy back.
.DESCRIPTION
  The bundle (release.ps1 or release.yml output, plus make-manifest.ps1's recall-update.unsigned.json)
  must agree with itself and with git: SHA256SUMS.txt, SOURCE-REVISION.txt = the v<version> tag's
  commit on origin/main, file version <version>.0 on Setup and the four exes, and the manifest's
  Setup size and SHA-256. Then:
    1. the release secret key and its password come from Infisical (the CLI, logged in as the
       maintainer) straight into files in a private temporary folder, or, with no Infisical project
       named, from -KeyDir (release-1.key + release-1.password.txt, used in place); never into a
       variable, the console or a log; minisign reads the password from that file;
    2. the signature is checked with minisign against the release public key in update_keys.txt,
       and by the app's own verifier (an ignored recall-native test) with the keys it pins;
    3. the temporary folder is overwritten and deleted (also when anything fails);
    4. the GitHub release is created if missing, then publish.ps1 uploads to GitLab, GitHub, and the
       update site when the Cloudflare token can reach its Pages project;
    5. every public copy is downloaded again and compared with the bundle.
  Only the release key is ever fetched: a name that mentions the backup key is refused.
  -WhatIf runs the checks and prints the plan without fetching, signing or publishing.
  -TestKeyFile/-TestPasswordFile/-TestPublicKey sign a TEST-BUILD bundle with a local throwaway key:
  checks, sign and minisign verify only; nothing is published.
.EXAMPLE
  .\sign-and-publish.ps1 -Bundle L:\release\0.1.11 -Minisign <path to minisign.exe>
#>
param(
    [Parameter(Mandatory)][string]$Bundle,
    [string]$ProjectId = $env:RECALL_INFISICAL_PROJECT_ID,
    [string]$Environment = 'prod',
    [string]$SecretPath = '/recall',
    [string]$KeySecret = 'RECALL_RELEASE_KEY',
    [string]$PasswordSecret = 'RECALL_RELEASE_KEY_PASSWORD',
    # A self-hosted Infisical (its URL); empty: the CLI's default.
    [string]$Domain = $env:RECALL_INFISICAL_DOMAIN,
    [string]$Minisign = $(if ($env:RECALL_MINISIGN) { $env:RECALL_MINISIGN } else { 'minisign' }),
    # With no Infisical project: the folder holding release-1.key and release-1.password.txt, readable
    # only by this user (and SYSTEM / Administrators). The files are used in place, never copied.
    [string]$KeyDir = (Join-Path $env:USERPROFILE 'recall-signing-keys'),
    [string]$PreviousBundle,
    [switch]$WhatIf,
    [string]$TestKeyFile,
    [string]$TestPasswordFile,
    [string]$TestPublicKey
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$test = [bool]$TestKeyFile
foreach ($name in $KeySecret, $PasswordSecret, $TestKeyFile, $TestPasswordFile, $KeyDir) {
    if ($name -match 'backup') { throw "Only the release key signs here; refused: $name" }
}
if ($test -and !($TestPasswordFile -and $TestPublicKey)) { throw '-TestKeyFile needs -TestPasswordFile and -TestPublicKey.' }
$source = if ($test) { 'test' } elseif ($ProjectId) { 'infisical' } else { 'keydir' }

# The key folder and its two files may be readable only by this user, SYSTEM and Administrators.
function Assert-PrivateKeyDir([string]$Dir) {
    $allowed = @([Security.Principal.WindowsIdentity]::GetCurrent().User.Value, 'S-1-5-18', 'S-1-5-32-544')
    # The maintainer lets the local Codex sandbox group read the folder (2026-11-24); read only.
    $readers = @()
    try { $readers += (New-Object Security.Principal.NTAccount($env:COMPUTERNAME, 'CodexSandboxUsers')).Translate([Security.Principal.SecurityIdentifier]).Value } catch { }
    $readOnly = [int]([Security.AccessControl.FileSystemRights]::ReadAndExecute -bor [Security.AccessControl.FileSystemRights]::Synchronize)
    foreach ($p in @($Dir, (Join-Path $Dir 'release-1.key'), (Join-Path $Dir 'release-1.password.txt'))) {
        if (!(Test-Path -LiteralPath $p)) { throw "Missing $p" }
        foreach ($rule in (Get-Acl -LiteralPath $p).Access) {
            $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
            if ($rule.AccessControlType -eq 'Allow' -and $readers -contains $sid -and ([int]$rule.FileSystemRights -band -bnot $readOnly) -eq 0) { continue }
            if ($rule.AccessControlType -eq 'Allow' -and $allowed -notcontains $sid) { throw "$p is readable by $($rule.IdentityReference): restrict it to your account." }
        }
    }
}

# --- 1. The bundle agrees with itself and with git -------------------------------------------
$Bundle = (Resolve-Path -LiteralPath $Bundle).ProviderPath
$setup = Join-Path $Bundle 'RecallSetup.exe'
$unsigned = Join-Path $Bundle 'recall-update.unsigned.json'
$signed = Join-Path $Bundle 'recall-update.json'
foreach ($f in $setup, $unsigned, (Join-Path $Bundle 'SHA256SUMS.txt'), (Join-Path $Bundle 'SOURCE-REVISION.txt')) {
    if (!(Test-Path -LiteralPath $f -PathType Leaf)) { throw "The bundle lacks $f" }
}
$isTestBuild = Test-Path -LiteralPath (Join-Path $Bundle 'TEST-BUILD.txt')
if ($test -ne $isTestBuild) { throw $(if ($test) { 'A test key signs only a TEST-BUILD bundle.' } else { 'This is a test build (no update keys): it can never be released.' }) }
$m = [IO.File]::ReadAllText($unsigned) | ConvertFrom-Json
$version = $m.version
if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw "The manifest names an invalid version: $version" }
if ((Get-Item -LiteralPath $unsigned).Length -gt 256KB) { throw 'The manifest is over 256 KiB; the app would refuse it.' }
$sha = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
$listed = ((Get-Content -LiteralPath (Join-Path $Bundle 'SHA256SUMS.txt') -TotalCount 1) -split '\s+')[0].ToLowerInvariant()
if ($sha -ne $listed) { throw "RecallSetup.exe does not match SHA256SUMS.txt ($sha vs $listed)." }
if ($sha -ne $m.setup.sha256 -or (Get-Item -LiteralPath $setup).Length -ne $m.setup.size) { throw 'Setup does not match the manifest: make it again with make-manifest.ps1.' }
$revision = (Get-Content -LiteralPath (Join-Path $Bundle 'SOURCE-REVISION.txt') -TotalCount 1).Trim().ToLowerInvariant()
if ($revision -ne $m.revision) { throw "SOURCE-REVISION.txt ($revision) is not the manifest's revision ($($m.revision))." }
$exes = @($setup) + @(Get-ChildItem -LiteralPath (Join-Path $Bundle 'recall-app') -Filter '*.exe' | ForEach-Object FullName)
if ($exes.Count -ne 5) { throw "Expected Setup and 4 Recall executables, found $($exes.Count)." }
foreach ($exe in $exes) {
    # The numeric file version (the text field reads 0.1.11 on the Rust exes, 0.1.11.0 on Setup).
    $vi = (Get-Item -LiteralPath $exe).VersionInfo
    $v = '{0}.{1}.{2}.{3}' -f $vi.FileMajorPart, $vi.FileMinorPart, $vi.FileBuildPart, $vi.FilePrivatePart
    if ($v -ne "$version.0") { throw "$exe is version $v, not $version.0." }
}
if (!$test) {
    git -C $repo fetch --quiet origin main "refs/tags/v${version}:refs/tags/v${version}"
    if ($LASTEXITCODE -ne 0) { throw 'git fetch origin failed.' }
    $tagCommit = git -C $repo rev-parse "v$version^{commit}"
    if ($tagCommit -ne $revision) { throw "Tag v$version is $tagCommit, not the bundle's revision $revision." }
    git -C $repo merge-base --is-ancestor $revision origin/main
    if ($LASTEXITCODE -ne 0) { throw "Revision $revision is not on origin/main." }
}
$publicKey = if ($test) { $TestPublicKey } else {
    $line = Get-Content -LiteralPath (Join-Path $repo 'recall-native\src\services\update_keys.txt') | Where-Object { $_ -match '^\s*release\s+release-1\s+(\S+)' } | Select-Object -First 1
    if (!$line) { throw 'update_keys.txt has no release-1 key.' }
    $Matches[1]
}
"Recall $version  revision $revision"
"  Setup $($m.setup.size) bytes  SHA-256 $sha  (sequence $($m.sequence))"
if ($source -eq 'keydir') { Assert-PrivateKeyDir $KeyDir }
"  sign with: $(switch ($source) { 'test' { "test key $TestKeyFile" } 'infisical' { "Infisical $Environment$SecretPath/$KeySecret" } 'keydir' { "release-1.key in $KeyDir" } })"
if ($WhatIf) { 'WhatIf: checks passed; nothing fetched, signed or published.'; return }

# --- 2. Sign with a key that lives only in a private temporary folder ------------------------
function Remove-Private([string]$Dir) {
    if (!(Test-Path -LiteralPath $Dir)) { return }
    foreach ($f in Get-ChildItem -LiteralPath $Dir -File -Force) {
        $junk = New-Object byte[] ([Math]::Max($f.Length, 1)); [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($junk)
        [IO.File]::WriteAllBytes($f.FullName, $junk)
    }
    Remove-Item -LiteralPath $Dir -Recurse -Force
}
function Invoke-Quiet([string]$Exe, [string[]]$Arguments, [string]$StdIn, [string]$StdOut) {
    # Start-Process joins arguments with spaces: quote each one (none holds a quote character).
    $line = ($Arguments | ForEach-Object { if ($_ -match '"') { throw "Unexpected quote in $_" }; "`"$_`"" }) -join ' '
    $p = @{ FilePath = $Exe; ArgumentList = $line; NoNewWindow = $true; Wait = $true; PassThru = $true }
    if ($StdIn) { $p.RedirectStandardInput = $StdIn }
    if ($StdOut) { $p.RedirectStandardOutput = $StdOut }
    (Start-Process @p).ExitCode
}
$private = Join-Path ([IO.Path]::GetTempPath()) "recall-sign-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $private | Out-Null
try {
    # Only the current user (no inherited entries) may read the folder.
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetAccessRuleProtection($true, $false)
    $me = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($me, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')))
    Set-Acl -LiteralPath $private -AclObject $acl
    $keyFile = Join-Path $private 'release.key'
    $passFile = Join-Path $private 'password.txt'
    if ($source -eq 'keydir') {
        # Used in place: minisign reads the key file and takes the password file as its input.
        $keyFile = Join-Path $KeyDir 'release-1.key'
        $passFile = Join-Path $KeyDir 'release-1.password.txt'
    } elseif ($test) {
        Copy-Item -LiteralPath $TestKeyFile -Destination $keyFile
        Copy-Item -LiteralPath $TestPasswordFile -Destination $passFile
    } else {
        $common = @('secrets', 'get', '--plain', '--silent', "--projectId=$ProjectId", "--env=$Environment", "--path=$SecretPath")
        if ($Domain) { $common += "--domain=$Domain" }
        foreach ($pair in @($KeySecret, $keyFile), @($PasswordSecret, $passFile)) {
            $code = Invoke-Quiet 'infisical' ($common[0..1] + $pair[0] + $common[2..($common.Count - 1)]) $null $pair[1]
            if ($code -ne 0 -or (Get-Item -LiteralPath $pair[1]).Length -eq 0) { throw "Infisical gave no value for $($pair[0]) (exit $code); is the CLI logged in?" }
        }
    }
    Copy-Item -LiteralPath $unsigned -Destination $signed -Force
    $code = Invoke-Quiet $Minisign @('-S', '-s', $keyFile, '-m', $signed, '-t', "recall $version sequence $($m.sequence)") $passFile $null
    if ($code -ne 0) { throw "minisign did not sign (exit $code)." }
} finally {
    Remove-Private $private
}
if (Test-Path -LiteralPath $private) { throw "Could not remove ${private}: delete it now." }

# --- 3. Verify: minisign with the pinned public key, then the app's own verifier --------------
& $Minisign -V -P $publicKey -m $signed
if ($LASTEXITCODE -ne 0) { throw 'The signature does not verify with the release public key.' }
if ($test) { "Test signature verified; nothing published."; return }
$env:RECALL_CHECK_MANIFEST = $signed; $env:RECALL_CHECK_SHA256 = $sha
$savedPath = $env:PATH
$tools = Split-Path ([IO.Path]::GetFullPath((git -C $repo rev-parse --path-format=absolute --git-common-dir))) -Parent
$env:PATH = "$(Join-Path $tools 'tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin');$env:PATH"
$nice = Join-Path $env:USERPROFILE '.claude\tools\nice-build.ps1'
try {
    $cargo = if (Test-Path -LiteralPath $nice) { @('powershell', '-NoProfile', '-File', $nice, 'cargo') } else { @('cargo') }
    # cargo writes progress to stderr: under Windows PowerShell with Stop that would end the script.
    $ErrorActionPreference = 'Continue'
    $out = & $cargo[0] @($cargo | Select-Object -Skip 1) test --manifest-path (Join-Path $repo 'recall-native\Cargo.toml') --locked --offline a_signed_manifest_verifies_with_the_pinned_release_key -- --ignored --nocapture 2>&1 | ForEach-Object { "$_" }
    $ErrorActionPreference = 'Stop'
    if ($LASTEXITCODE -ne 0 -or !($out -match 'APP-VERIFIED')) { $out | Select-Object -Last 20; throw "The app's verifier refused the signed manifest." }
    $out -match 'APP-VERIFIED'
} finally { Remove-Item Env:RECALL_CHECK_MANIFEST, Env:RECALL_CHECK_SHA256 -ErrorAction SilentlyContinue; $env:PATH = $savedPath }

# --- 4. Publish ---------------------------------------------------------------------------------
# From here every native call is judged by its exit code or by the read-back: gh, wrangler and
# cargo write progress to stderr, which Windows PowerShell would turn into a terminating error.
$ErrorActionPreference = 'Continue'
$notes = Join-Path $repo "docs\release-notes\v$version.md"
$assets = @($setup, (Join-Path $Bundle 'SHA256SUMS.txt'), (Join-Path $Bundle 'SOURCE-REVISION.txt'))
gh release view "v$version" --repo LoneBeast27/recall *> $null
if ($LASTEXITCODE -ne 0) {
    gh release create "v$version" --verify-tag --repo LoneBeast27/recall --title "Recall $version" --notes-file $notes @assets
    if ($LASTEXITCODE -ne 0) { throw 'gh release create failed.' }
} else {
    # A same-version hotfix: the release stays, its Setup, checksums, revision and notes are replaced.
    gh release upload "v$version" --repo LoneBeast27/recall --clobber @assets
    if ($LASTEXITCODE -ne 0) { throw 'gh release upload of the hotfix Setup failed.' }
    gh release edit "v$version" --repo LoneBeast27/recall --notes-file $notes
    if ($LASTEXITCODE -ne 0) { throw 'gh release edit (notes) failed.' }
}
$hosts = @('gitlab', 'github')
$pagesReady = $false
if ($env:CLOUDFLARE_API_TOKEN -and $env:CLOUDFLARE_ACCOUNT_ID) {
    try {
        $h = @{ Authorization = "Bearer $env:CLOUDFLARE_API_TOKEN" }
        $r = Invoke-RestMethod -Headers $h -Uri "https://api.cloudflare.com/client/v4/accounts/$env:CLOUDFLARE_ACCOUNT_ID/pages/projects/recallupdates"
        $pagesReady = [bool]$r.success
    } catch { $pagesReady = $false }
}
if ($pagesReady) { $hosts += 'pages' } else { 'Update site: skipped (no Cloudflare token that can reach the recallupdates Pages project).' }
$publishArgs = @{ Bundle = $Bundle; Only = $hosts }
if ($PreviousBundle) { $publishArgs.PreviousBundle = $PreviousBundle }
$lines = & (Join-Path $PSScriptRoot 'publish.ps1') @publishArgs 2>&1
$lines

# --- 5. Read every copy back (anonymously where the host allows it) ----------------------------
$check = Join-Path ([IO.Path]::GetTempPath()) "recall-readback-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $check | Out-Null
$report = [ordered]@{}
try {
    $want = @{ 'recall-update.json' = (Get-FileHash $signed).Hash; 'recall-update.json.minisig' = (Get-FileHash "$signed.minisig").Hash; 'RecallSetup.exe' = $sha.ToUpperInvariant() }
    $urls = @{
        gitlab = @{ 'recall-update.json' = 'https://gitlab.com/api/v4/projects/89277322/packages/generic/recallupdates/current/recall-update.json'
                    'recall-update.json.minisig' = 'https://gitlab.com/api/v4/projects/89277322/packages/generic/recallupdates/current/recall-update.json.minisig'
                    'RecallSetup.exe' = $m.setup.urls[0] }
        pages  = @{ 'recall-update.json' = 'https://recallupdates.lonebeast.net/recall-update.json'
                    'recall-update.json.minisig' = 'https://recallupdates.lonebeast.net/recall-update.json.minisig'
                    'RecallSetup.exe' = "https://recallupdates.lonebeast.net/v$version/RecallSetup.exe" }
    }
    foreach ($hostName in $hosts) {
        $bad = @()
        if ($hostName -eq 'github') {
            gh release download "v$version" --repo LoneBeast27/recall --dir (Join-Path $check 'github') --pattern 'recall-update.json*' --pattern 'RecallSetup.exe' 2>&1 | Out-Null
            foreach ($n in $want.Keys) { $f = Join-Path $check "github\$n"; if (!(Test-Path $f) -or (Get-FileHash $f).Hash -ne $want[$n]) { $bad += $n } }
        } else {
            foreach ($n in $want.Keys) {
                $f = Join-Path $check "$hostName-$n"
                try { Invoke-WebRequest -Uri $urls[$hostName][$n] -OutFile $f -UseBasicParsing } catch { $bad += $n; continue }
                if ((Get-FileHash $f).Hash -ne $want[$n]) { $bad += $n }
            }
        }
        $report[$hostName] = if ($bad) { "MISMATCH or missing: $($bad -join ', ')" } else { 'all 3 files match' }
    }
} finally { Remove-Item -LiteralPath $check -Recurse -Force -ErrorAction SilentlyContinue }

''
"Recall $version published.  Setup SHA-256 $sha"
$report.GetEnumerator() | ForEach-Object { '  {0,-7} {1}' -f $_.Key, $_.Value }
if (!$pagesReady) { '  pages   skipped: run publish.ps1 -Only pages later (same signed files)' }
if ($report.Values | Where-Object { $_ -ne 'all 3 files match' }) { exit 1 }
