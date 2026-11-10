<#
  Deletes one disposable guest made by Start-CleanGuest.ps1: the VM and its VHDX/AVHDX files, but only
  when every disk sits directly in -WorkDir and carries the VM's name. The ISO and the result files stay.
  Usage (elevated): .\Remove-CleanGuest.ps1 -WorkDir D:\RecallVm -VmName RecallSetupClean-20261110-1234
#>
param([Parameter(Mandatory)][string]$WorkDir, [Parameter(Mandatory)][ValidatePattern('^RecallSetupClean-[0-9]{8}-[0-9]+$')][string]$VmName)
$ErrorActionPreference = 'Stop'
$stage = (Resolve-Path -LiteralPath $WorkDir).ProviderPath.TrimEnd('\')
$log = Join-Path $stage 'cleanup.log'
try {
    if ((Get-Item -LiteralPath $stage).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'WorkDir must be a plain folder' }
    $vm = Get-VM -Name $VmName -ErrorAction SilentlyContinue
    $disks = @()
    if ($vm) {
        $disks = @(Get-VMHardDiskDrive -VM $vm | Select-Object -ExpandProperty Path)
        if (!$vm.Path.StartsWith($stage, [StringComparison]::OrdinalIgnoreCase)) { throw "$VmName does not live in $stage" }
        foreach ($d in $disks) {
            if ([IO.Path]::GetDirectoryName($d) -ine $stage -or [IO.Path]::GetFileName($d) -notlike "$VmName*") { throw "Unexpected disk for ${VmName}: $d" }
        }
        if ($vm.State -ne 'Off') { Stop-VM -VM $vm }
        if ((Get-VM -Id $vm.Id).State -ne 'Off') { throw "$VmName did not shut down" }
        Remove-VM -VM $vm -Force
        "Removed VM $VmName" | Add-Content -LiteralPath $log
    }
    $linked = @(Get-VM | Get-VMHardDiskDrive | Select-Object -ExpandProperty Path)
    $files = @(Get-ChildItem -LiteralPath $stage -File | Where-Object { $_.Name -match ('^' + [regex]::Escape($VmName) + '(_[0-9a-f-]+)?\.a?vhdx$') })
    foreach ($f in $files) {
        if ($f.FullName -in $linked -or $f.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing to remove a disk in use: $($f.FullName)" }
        if ((Get-VHD -Path $f.FullName).Attached) { throw "Disk still attached: $($f.FullName)" }
        Remove-Item -LiteralPath $f.FullName -Force
        "Removed $($f.Name) ($($f.Length) bytes)" | Add-Content -LiteralPath $log
    }
} catch { "FAILED $($_.Exception.Message)" | Add-Content -LiteralPath $log; exit 1 }
