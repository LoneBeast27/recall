<#
  Saves the guest's console as <WorkDir>\guest-console.png (Hyper-V thumbnail, 800x600) and its state
  to console-status.txt, for a guest stuck during unattended setup. Read only.
  Usage (elevated): .\Get-GuestScreen.ps1 -WorkDir D:\RecallVm -VmName RecallSetupClean-20261110-1234
#>
param([Parameter(Mandatory)][string]$WorkDir, [Parameter(Mandatory)][string]$VmName)
$ErrorActionPreference = 'Stop'
$stage = (Resolve-Path -LiteralPath $WorkDir).ProviderPath.TrimEnd('\')
$log = Join-Path $stage 'console-status.txt'
try {
    $vm = Get-VM -Name $VmName
    if (!$vm.Path.StartsWith($stage, [StringComparison]::OrdinalIgnoreCase)) { throw "$VmName does not live in $stage" }
    "State $($vm.State), status $($vm.Status), uptime $($vm.Uptime)" | Set-Content -LiteralPath $log
    $space = 'root\virtualization\v2'
    $machine = Get-CimInstance -Namespace $space -ClassName Msvm_ComputerSystem -Filter "Name='$($vm.Id)'"
    $settings = Get-CimAssociatedInstance -InputObject $machine -ResultClassName Msvm_VirtualSystemSettingData | Select-Object -First 1
    $service = Get-CimInstance -Namespace $space -ClassName Msvm_VirtualSystemManagementService | Select-Object -First 1
    $reply = Invoke-CimMethod -InputObject $service -MethodName GetVirtualSystemThumbnailImage -Arguments @{
        TargetSystem = $settings; WidthPixels = [uint16]800; HeightPixels = [uint16]600 }
    if ($reply.ReturnValue -ne 0) { throw "Thumbnail returned $($reply.ReturnValue)" }
    $bytes = [byte[]]$reply.ImageData
    if ($bytes.Length -ne 800 * 600 * 2) { throw "Unexpected frame size $($bytes.Length)" }
    Add-Type -AssemblyName System.Drawing
    $image = [Drawing.Bitmap]::new(800, 600, [Drawing.Imaging.PixelFormat]::Format24bppRgb)
    try {
        for ($y = 0; $y -lt 600; $y++) {
            for ($x = 0; $x -lt 800; $x++) {
                $i = ($y * 800 + $x) * 2; $px = $bytes[$i] + 256 * $bytes[$i + 1]
                $image.SetPixel($x, $y, [Drawing.Color]::FromArgb((($px -shr 11) -band 31) * 255 / 31, (($px -shr 5) -band 63) * 255 / 63, ($px -band 31) * 255 / 31))
            }
        }
        $image.Save((Join-Path $stage 'guest-console.png'), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $image.Dispose() }
} catch { $_.Exception.Message | Add-Content -LiteralPath $log; exit 1 }
