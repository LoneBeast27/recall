<#
  Clean-Windows proof for Recall Setup in a disposable Hyper-V guest (developer tool; never shipped).
  Builds a fresh Windows guest from an official evaluation ISO by an unattended optical install, with no
  network adapter and no host shares, copies RecallSetup.exe in over PowerShell Direct, and runs
  VerifyGuest.ps1 there as the guest's test user (elevated): Visual C++ on a cold machine, then Recall's
  install, update and uninstall against the guest's REAL HKCU keys. The result lands in
  <WorkDir>\guest-result.json; the guest is shut down and left in place (Remove-CleanGuest.ps1 deletes it).

  Needs: an administrator PowerShell, Hyper-V enabled, the ISO already on disk (this script downloads
  nothing), and room in WorkDir for a dynamic VHDX of -DiskGiB (Windows plus Recall uses about 20 GiB).

  Usage (elevated):
    .\Start-CleanGuest.ps1 -WorkDir D:\RecallVm -IsoPath D:\Iso\Win11-LTSC-eval.iso -Installer .\dist\setup\RecallSetup.exe
#>
param(
    [Parameter(Mandatory)][string]$WorkDir,
    [Parameter(Mandatory)][string]$IsoPath,
    [Parameter(Mandatory)][string]$Installer,
    [string]$VmName = ('RecallSetupClean-' + (Get-Date -Format 'yyyyMMdd') + '-' + $PID),
    [ValidateRange(40, 256)][int]$DiskGiB = 64,
    [ValidateRange(5, 240)][int]$GuestMinutes = 60
)
$ErrorActionPreference = 'Stop'
$stage = (Resolve-Path -LiteralPath $WorkDir).ProviderPath.TrimEnd('\')
if ((Get-Item -LiteralPath $stage).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'WorkDir must be a plain folder' }
$iso = (Resolve-Path -LiteralPath $IsoPath).ProviderPath
$installer = (Resolve-Path -LiteralPath $Installer).ProviderPath
$verify = Join-Path $PSScriptRoot 'VerifyGuest.ps1'
$log = Join-Path $stage 'hyper-v.log'
function Log([string]$line) { ('{0} {1}' -f (Get-Date -Format s), $line) | Add-Content -LiteralPath $log }
$disk = Join-Path $stage ($VmName + '.vhdx')
$guestName = 'RECALL-CLEAN-VM'
$mountedIso = $null; $guest = $null; $session = $null; $answerDvd = $null; $answerIso = $null
try {
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run this from an administrator PowerShell' }
    if (!(Get-Command New-VM -ErrorAction SilentlyContinue)) { throw 'Hyper-V is not enabled on this PC' }
    if (Get-VM -Name $VmName -ErrorAction SilentlyContinue) { throw "A VM named $VmName already exists" }
    if (Test-Path -LiteralPath $disk) { throw "Expected a new guest-only disk: $disk exists" }
    $mountedIso = Mount-DiskImage -ImagePath $iso -StorageType ISO -Access ReadOnly -PassThru
    $letter = ($mountedIso | Get-Volume).DriveLetter
    if (!$letter) { throw 'The Windows ISO has no volume letter' }
    $wim = Join-Path "${letter}:\" 'sources\install.wim'
    if (!(Test-Path -LiteralPath $wim)) { $wim = Join-Path "${letter}:\" 'sources\install.esd' }
    $image = Get-WindowsImage -ImagePath $wim | Where-Object { $_.ImageName -match 'Enterprise.*LTSC|Pro' -and $_.ImageName -notmatch '\bN\b' } | Select-Object -First 1
    if (!$image) { throw 'No non-N Enterprise LTSC or Pro image in the ISO' }
    $index = [int]$image.ImageIndex
    Log "Selected Windows image $index ($($image.ImageName))"
    Dismount-DiskImage -ImagePath $iso | Out-Null
    $mountedIso = $null

    # A throwaway local admin for this guest only; the password lives in this process and the guest.
    $password = ([Guid]::NewGuid().ToString('N')) + 'aA1!'
    $answerRoot = Join-Path $stage ('answer-' + $PID)
    $answerIso = Join-Path $stage ('guest-answer-' + $PID + '.iso')
    New-Item -ItemType Directory -Path $answerRoot | Out-Null
    $answer = (Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'Autounattend.template.xml')).
        Replace('{{INDEX}}', [string]$index).Replace('{{PASSWORD}}', $password).Replace('{{COMPUTER}}', $guestName)
    $answerFile = Join-Path $answerRoot 'Autounattend.xml'
    [IO.File]::WriteAllText($answerFile, $answer)
    . (Join-Path $PSScriptRoot 'AnswerIso.ps1')
    Save-AnswerIso -Folder $answerRoot -Path $answerIso
    Remove-Item -LiteralPath $answerFile

    $guest = New-VM -Name $VmName -Path $stage -Generation 2 -MemoryStartupBytes 4GB -NewVHDPath $disk -NewVHDSizeBytes ($DiskGiB * 1GB)
    Set-VMProcessor -VM $guest -Count 4 -RelativeWeight 25 -Maximum 50
    Set-VM -VM $guest -AutomaticStartAction Nothing -AutomaticStopAction ShutDown
    Get-VMNetworkAdapter -VM $guest | Remove-VMNetworkAdapter
    Set-VMKeyProtector -VM $guest -NewLocalKeyProtector
    Enable-VMTPM -VM $guest
    $windowsDvd = Add-VMDvdDrive -VM $guest -Path $iso -Passthru
    $answerDvd = Add-VMDvdDrive -VM $guest -Path $answerIso -Passthru
    $guestDisk = Get-VMHardDiskDrive -VM $guest | Select-Object -First 1
    if ($guestDisk.Path -ine $disk) { throw 'The VM disk does not match the isolated disk' }
    Set-VMFirmware -VM $guest -BootOrder @($guestDisk, $windowsDvd)
    $user = "$guestName\RecallVmTest"
    $credential = [PSCredential]::new($user, (ConvertTo-SecureString $password -AsPlainText -Force))
    Start-VM -VM $guest
    Log "Started unattended setup in $VmName (no network adapter, no host shares)"
    # "Press any key to boot from CD": typed into this guest's virtual keyboard only.
    $computer = Get-CimInstance -Namespace 'root/virtualization/v2' -ClassName Msvm_ComputerSystem -Filter "Name='$($guest.Id)'"
    $keyboard = Get-CimAssociatedInstance -InputObject $computer -Association Msvm_SystemDevice -ResultClassName Msvm_Keyboard
    if (!$keyboard) { throw 'Could not address the guest-only virtual keyboard' }
    $accept = (Get-Date).AddSeconds(14)
    do {
        $null = Invoke-CimMethod -InputObject $keyboard -MethodName TypeKey -Arguments @{ keyCode = [uint32]13 } -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 450
    } while ((Get-Date) -lt $accept)
    function Open-Guest {
        $wait = (Get-Date).AddMinutes($GuestMinutes)
        do {
            try { return New-PSSession -VMName $VmName -Credential $credential -ErrorAction Stop } catch {}
            Start-Sleep -Seconds 8
        } until ((Get-Date) -gt $wait)
        throw 'The guest did not become ready for PowerShell Direct'
    }
    $session = Open-Guest
    Log 'Guest ready. Copying Setup and the guest checks over PowerShell Direct.'
    Invoke-Command -Session $session -ArgumentList $guestName -ScriptBlock {
        param($expected)
        if ($env:COMPUTERNAME -ne $expected) { throw 'Unexpected guest' }
        New-Item -ItemType Directory -Path 'C:\RecallCleanTest' | Out-Null
    }
    Copy-Item -LiteralPath $installer -ToSession $session -Destination 'C:\RecallCleanTest\RecallSetup.exe'
    Copy-Item -LiteralPath $verify -ToSession $session -Destination 'C:\RecallCleanTest\VerifyGuest.ps1'
    $hash = (Get-FileHash -LiteralPath $installer).Hash
    # The checks run as the test user (elevated), so HKCU is a real user's hive, not SYSTEM's.
    Invoke-Command -Session $session -ArgumentList $hash, $user, $password -ScriptBlock {
        param($expected, $user, $secret)
        Set-Content -LiteralPath 'C:\RecallCleanTest\expected-hash.txt' -Value $expected
        $action = New-ScheduledTaskAction -Execute "$env:windir\System32\WindowsPowerShell\v1.0\powershell.exe" `
            -Argument '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File C:\RecallCleanTest\VerifyGuest.ps1'
        Register-ScheduledTask -TaskName 'RecallCleanGuestTest' -Action $action -User $user -Password $secret -RunLevel Highest | Out-Null
        Start-ScheduledTask -TaskName 'RecallCleanGuestTest'
    }
    function Read-Result {
        $wait = (Get-Date).AddMinutes(20)
        do {
            $data = Invoke-Command -Session $session -ScriptBlock { Get-Content -LiteralPath 'C:\RecallCleanTest\result.json' -Raw -ErrorAction SilentlyContinue }
            if ($data) { try { $r = $data | ConvertFrom-Json -ErrorAction Stop; if ($r.status -ne 'running') { return $r } } catch {} }
            Start-Sleep -Seconds 3
        } until ((Get-Date) -gt $wait)
        throw 'The guest checks did not report a result'
    }
    $result = Read-Result
    if ($result.status -eq 'restart') {
        $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $stage 'guest-before-reboot.json')
        Log 'Setup asked for a restart: restarting only the disposable guest.'
        try {
            Invoke-Command -Session $session -ScriptBlock {
                Copy-Item -LiteralPath 'C:\RecallCleanTest\result.json' -Destination 'C:\RecallCleanTest\before-reboot.json'
                Move-Item -LiteralPath 'C:\RecallCleanTest\result.json' -Destination 'C:\RecallCleanTest\after-reboot.txt'
                Restart-Computer -Force
            }
        } catch {}
        Remove-PSSession $session
        Start-Sleep -Seconds 20
        $session = Open-Guest
        Invoke-Command -Session $session -ScriptBlock { Start-ScheduledTask -TaskName 'RecallCleanGuestTest' }
        $result = Read-Result
    }
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $stage 'guest-result.json')
    if ($result.status -ne 'passed') { throw "Guest result: $($result.failure)" }
    Log 'PASS Visual C++ on a cold guest, then Recall install, update and uninstall against the real HKCU keys.'
} catch {
    Log "FAILED $($_.Exception.Message)"
    exit 1
} finally {
    if ($session) { Remove-PSSession $session }
    if ($guest -and (Get-VM -Id $guest.Id -ErrorAction SilentlyContinue).State -eq 'Running') { Stop-VM -VM $guest -ErrorAction SilentlyContinue }
    if ($mountedIso) { Dismount-DiskImage -ImagePath $iso -ErrorAction SilentlyContinue | Out-Null }
    if ($guest -and $answerDvd) { Set-VMDvdDrive -VMDvdDrive $answerDvd -Path $null -ErrorAction SilentlyContinue }
    if ($answerIso -and (Test-Path -LiteralPath $answerIso)) { Remove-Item -LiteralPath $answerIso -Force }
    Log "Guest $VmName left shut down in $stage (Remove-CleanGuest.ps1 -WorkDir $stage -VmName $VmName deletes it)."
}
