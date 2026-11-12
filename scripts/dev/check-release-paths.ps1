<#
.SYNOPSIS
Refuses release executables that carry local build details: the Windows user name, the user
profile path, the folder that holds the checkout, or an "@gmail.com" address.
.DESCRIPTION
Scans the raw bytes of every .exe under -Path (files or folders, recursively), as ASCII/Latin-1
and as UTF-16LE, case-insensitively. A zip archive embedded in an executable (Setup carries the
Recall payload as a deflated zip resource) is opened and every .exe/.dll inside it is scanned
too. The user name matches only between non-alphanumeric characters, so a field name such as
"dsv_rohandle" does not count for a user "rohan". Each hit prints file, byte offset and context.
Exit code 0: clean. Exit code 1: at least one hit. Used by scripts/release.ps1 (local builds).
.EXAMPLE
.\scripts\dev\check-release-paths.ps1 -Path .tmp\release\0.1.5\RecallSetup.exe, .tmp\release\0.1.5\recall-app -ParentPath L:\General
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string[]]$Path,
    # Folders whose path must not appear (the parent of the checkout). Default: the parent of this repository.
    [string[]]$ParentPath,
    [string]$UserName = $env:USERNAME,
    [string]$UserProfile = $env:USERPROFILE
)
$ErrorActionPreference = 'Stop'
if (!$ParentPath) { $ParentPath = @(Split-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) -Parent) }
$latin1 = [Text.Encoding]::GetEncoding(28591)
$options = [Text.RegularExpressions.RegexOptions]::IgnoreCase -bor [Text.RegularExpressions.RegexOptions]::CultureInvariant

# A literal as a regex over one byte per character (ascii) or two (utf16); any run of / or \ matches a separator.
function ConvertTo-Needle([string]$Literal, [bool]$Utf16) {
    $nul = if ($Utf16) { '\x00' } else { '' }
    $separator = if ($Utf16) { '(?:[\\/]\x00)+' } else { '[\\/]+' }
    $parts = $Literal.TrimEnd('\', '/') -split '[\\/]+' | ForEach-Object {
        -join ($_.ToCharArray() | ForEach-Object { [regex]::Escape([string]$_) + $nul })
    }
    $parts -join $separator
}

$needles = [Collections.Generic.List[object]]::new()
function Add-Needle([string]$Name, [string]$Literal, [switch]$Boundary) {
    if (!$Literal) { return }
    foreach ($utf16 in $false, $true) {
        $body = ConvertTo-Needle $Literal $utf16
        if ($Boundary) {
            $alnum = if ($utf16) { '[A-Za-z0-9]\x00' } else { '[A-Za-z0-9]' }
            $body = "(?<!$alnum)$body(?!$alnum)"
        }
        $label = if ($utf16) { "$Name (UTF-16LE)" } else { $Name }
        $needles.Add([pscustomobject]@{ Name = $label; Regex = [regex]::new($body, $options) })
    }
}
Add-Needle 'user name' $UserName -Boundary
Add-Needle 'user profile' $UserProfile
foreach ($parent in $ParentPath | Where-Object { $_ } | Sort-Object -Unique) { Add-Needle 'checkout parent' $parent }
Add-Needle 'gmail address' '@gmail.com'

function Format-Context([byte[]]$Bytes, [int]$Offset, [int]$Length) {
    $start = [Math]::Max(0, $Offset - 24); $end = [Math]::Min($Bytes.Length, $Offset + $Length + 24)
    -join ($Bytes[$start..($end - 1)] | ForEach-Object { if ($_ -ge 0x20 -and $_ -lt 0x7f) { [char]$_ } else { '.' } })
}

# Every zip archive found inside the bytes, as (offset, archive bytes): located by its end-of-central-directory record.
function Get-EmbeddedZip([byte[]]$Bytes, [string]$Text) {
    foreach ($eocd in [regex]::Matches($Text, 'PK\x05\x06')) {
        $at = $eocd.Index
        if ($at + 22 -gt $Bytes.Length) { continue }
        $cdSize = [BitConverter]::ToUInt32($Bytes, $at + 12)
        $cdOffset = [BitConverter]::ToUInt32($Bytes, $at + 16)
        $comment = [BitConverter]::ToUInt16($Bytes, $at + 20)
        $start = [int64]$at - $cdSize - $cdOffset
        $end = [int64]$at + 22 + $comment
        if ($start -lt 0 -or $end -gt $Bytes.Length) { continue }
        if ($Bytes[$start] -ne 0x50 -or $Bytes[$start + 1] -ne 0x4B -or $Bytes[$start + 2] -ne 3 -or $Bytes[$start + 3] -ne 4) { continue }
        $archive = [byte[]]::new($end - $start)
        [Array]::Copy($Bytes, $start, $archive, 0, $archive.Length)
        [pscustomobject]@{ Offset = $start; Bytes = $archive }
    }
}

$hits = [Collections.Generic.List[string]]::new()
$scanned = [Collections.Generic.List[string]]::new()
function Test-Bytes([string]$Label, [byte[]]$Bytes, [int]$Depth) {
    $scanned.Add("$Label ($($Bytes.Length) bytes)")
    $text = $latin1.GetString($Bytes)
    foreach ($needle in $needles) {
        foreach ($m in $needle.Regex.Matches($text)) {
            $hits.Add(('{0} @ 0x{1:X} [{2}]: {3}' -f $Label, $m.Index, $needle.Name, (Format-Context $Bytes $m.Index $m.Length)))
        }
    }
    if ($Depth -ge 3) { return }
    Add-Type -AssemblyName System.IO.Compression
    foreach ($zip in Get-EmbeddedZip $Bytes $text) {
        $stream = [IO.MemoryStream]::new($zip.Bytes)
        try { $archive = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Read) } catch { continue }
        try {
            foreach ($entry in $archive.Entries | Where-Object { $_.FullName -match '\.(exe|dll)$' }) {
                $buffer = [IO.MemoryStream]::new()
                $reader = $entry.Open(); try { $reader.CopyTo($buffer) } finally { $reader.Dispose() }
                Test-Bytes ('{0} > zip@0x{1:X} > {2}' -f $Label, $zip.Offset, $entry.FullName) $buffer.ToArray() ($Depth + 1)
            }
        } finally { $archive.Dispose() }
    }
}

$files = foreach ($item in $Path) {
    $resolved = Get-Item -LiteralPath $item
    if ($resolved.PSIsContainer) { Get-ChildItem -LiteralPath $resolved.FullName -Recurse -File -Filter '*.exe' } else { $resolved }
}
if (!$files) { throw "No executables to scan in: $($Path -join ', ')" }
foreach ($file in $files) { Test-Bytes $file.FullName ([IO.File]::ReadAllBytes($file.FullName)) 0 }

$scanned | ForEach-Object { "scanned: $_" }
"needles: $(($needles | ForEach-Object Name | Sort-Object -Unique) -join ', ')"
if ($hits.Count) {
    $hits | Select-Object -First 200 | ForEach-Object { "LOCAL-PATH HIT: $_" }
    if ($hits.Count -gt 200) { "... and $($hits.Count - 200) more hits" }
    "FAIL: $($hits.Count) local-path hit(s) in the release executables."
    exit 1
}
"PASS: no user name, profile path, checkout parent or gmail address in $($scanned.Count) executable(s)."
exit 0
