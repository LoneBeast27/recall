# Reads this PC's hdrGammaFix.* values from .local/machine.json (gitignored), through
# scripts\dev\dev-paths.ps1. Written by scripts\dev\init-machine.ps1; template in
# .local.example\machine.example.json. A missing value stops the script with a clear error.
$script:hdrRepo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $script:hdrRepo 'scripts\dev\dev-paths.ps1')

# $given (a parameter) wins; else hdrGammaFix.<key> from machine.json; else an error.
function Resolve-HdrSetting([string]$given, [string]$key) {
    if ($given) { return $given }
    [string](Get-MachineValue $script:hdrRepo "hdrGammaFix.$key")
}
