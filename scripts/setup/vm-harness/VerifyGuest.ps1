<#
  Runs INSIDE the disposable guest only (Start-CleanGuest.ps1 copies it to C:\RecallCleanTest and runs it
  as the guest's test user, elevated). Writes C:\RecallCleanTest\result.json.
   1. Visual C++: absent on the cold guest, installed by Setup, loadable (a restart is resumed).
   2. Install: the REAL HKCU Uninstall\Recall values, Apps & Features listing, no Run value, every
      install-manifest.json file present with its size and SHA-256, the Start menu shortcut.
   3. Update (same folder): Run value still absent, manifest rewritten, cache and a user file untouched.
   4. Uninstall (RecallSetup.exe /uninstall /quiet): only the manifest files go; the cache, a kept clip
      copy and the user file stay; key, shortcut and temporary copy gone.
#>
$ErrorActionPreference = 'Stop'
$root = 'C:\RecallCleanTest'
$resultFile = Join-Path $root 'result.json'
$exe = Join-Path $root 'RecallSetup.exe'
$destination = Join-Path $root 'app'
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Recall'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Recall.lnk'
$result = [ordered]@{ status = 'running'; guest = $env:COMPUTERNAME; user = "$env:USERDOMAIN\$env:USERNAME"; checks = [ordered]@{} }
function Save-Result { [IO.File]::WriteAllText($resultFile, ($result | ConvertTo-Json -Depth 8)) }
function Check([string]$name, [bool]$ok, $detail) {
    $result.checks[$name] = [ordered]@{ ok = $ok; detail = "$detail" }
    if (!$ok) { throw "check failed: $name ($detail)" }
}
function Runtime {
    $file = Join-Path ([Environment]::SystemDirectory) 'vcruntime140.dll'
    $version = if (Test-Path -LiteralPath $file) { (Get-Item -LiteralPath $file).VersionInfo.FileVersion } else { $null }
    $key = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64'
    $installed = (Get-ItemProperty -LiteralPath $key -Name Installed -ErrorAction SilentlyContinue).Installed -eq 1
    return @{ dll = (Test-Path -LiteralPath $file); fileVersion = $version; registryInstalled = $installed }
}
function Manifest { Get-Content -Raw -LiteralPath (Join-Path $destination 'install-manifest.json') | ConvertFrom-Json }
function Run-Value { (Get-ItemProperty -LiteralPath $runKey -ErrorAction SilentlyContinue).'recall-hdr' }
try {
    Save-Result
    $computer = Get-CimInstance Win32_ComputerSystem
    if ($env:COMPUTERNAME -ne 'RECALL-CLEAN-VM' -or $computer.Model -ne 'Virtual Machine' -or
        $computer.Manufacturer -ne 'Microsoft Corporation') { throw 'Refusing to run outside the dedicated Hyper-V guest' }
    $until = (Get-Date).AddMinutes(4)
    while ((Get-ItemProperty -LiteralPath 'Registry::HKEY_LOCAL_MACHINE\SYSTEM\Setup').SystemSetupInProgress -eq 1) {
        if ((Get-Date) -gt $until) { throw 'Windows setup did not finish in the clean guest' }
        Start-Sleep -Seconds 3
    }
    $os = Get-CimInstance Win32_OperatingSystem
    $result.os = "$($os.Caption) $($os.Version) build $($os.BuildNumber)"
    $result.installerHash = (Get-FileHash -LiteralPath $exe).Hash
    if ($result.installerHash -ne (Get-Content -LiteralPath (Join-Path $root 'expected-hash.txt')).Trim()) {
        throw 'The guest installer differs from the one the host sent'
    }
    $resume = Test-Path -LiteralPath (Join-Path $root 'after-reboot.txt')
    $result.before = Runtime
    if ($resume) {
        if (!$result.before.dll -or !$result.before.registryInstalled) { throw 'The runtime is still missing after the guest restart' }
    } elseif ($result.before.dll -or $result.before.registryInstalled) {
        throw 'The guest already has a Visual C++ x64 runtime. This is not a cold-install test.'
    }
    Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Reflection;
using System.Runtime.InteropServices;
public sealed class RecallProgress : IProgress<string> {
    public readonly List<string> Messages = new List<string>();
    public void Report(string value) { Messages.Add(value); }
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] private static extern IntPtr LoadLibrary(string path);
    [DllImport("kernel32.dll")] private static extern bool FreeLibrary(IntPtr module);
    public static bool CanLoad(string path) {
        var module = LoadLibrary(path);
        if (module == IntPtr.Zero) return false;
        FreeLibrary(module);
        return true;
    }
    // InstallOptions(path, shortcut, launch, frameTiming, update, autostart, allowUnknownDrive): the guest's
    // virtual disk reports no media type, so the unclassified-drive opt-in is set (never offered by the window).
    public static string Install(Assembly installer, string target, bool update, RecallProgress progress) {
        var options = Activator.CreateInstance(installer.GetType("Recall.Setup.InstallOptions", true),
            new object[] { target, true, false, false, update, false, true });
        var package = installer.GetType("Recall.Setup.SetupPackage", true);
        return (string)package.GetMethod("Install").Invoke(null, new object[] { options, progress });
    }
}
"@
    $asm = [Reflection.Assembly]::LoadFrom($exe)
    $progress = [RecallProgress]::new()
    $failure = $null
    try { $result.installNote = [RecallProgress]::Install($asm, $destination, $false, $progress) }
    catch { $failure = $_.Exception.GetBaseException() }
    $result.progress = @($progress.Messages)
    $result.after = Runtime
    if (!$resume -and !($result.progress -match '^Installing Microsoft Visual C\+\+')) { throw 'Setup did not run its Visual C++ step' }
    if ($failure -and $failure.GetType().FullName -eq 'Recall.Setup.SetupRestartRequired' -and !$resume) {
        $result.status = 'restart'
        return
    }
    if ($failure) { throw "Install failed: $($failure.Message)" }
    Check 'vc runtime installed and loadable' ($result.after.dll -and $result.after.registryInstalled -and
        [RecallProgress]::CanLoad((Join-Path ([Environment]::SystemDirectory) 'vcruntime140.dll'))) $result.after.fileVersion

    # --- Install: the real keys.
    $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).FileVersion -replace '\.0$', ''
    $u = Get-ItemProperty -LiteralPath $uninstallKey
    $expectedCommand = '"' + (Join-Path $destination 'RecallSetup.exe') + '" /uninstall'
    Check 'Uninstall key values' ($u.DisplayName -eq 'Recall' -and $u.Publisher -eq 'LoneBeast27' -and $u.DisplayVersion -eq $version -and
        $u.InstallLocation -eq $destination -and $u.UninstallString -eq $expectedCommand -and
        $u.QuietUninstallString -eq ($expectedCommand + ' /quiet') -and $u.DisplayIcon -eq (Join-Path $destination 'recall.ico') -and
        $u.EstimatedSize -gt 0 -and $u.NoModify -eq 1 -and $u.NoRepair -eq 1) ($u | Select-Object DisplayName, DisplayVersion, Publisher, InstallLocation, UninstallString, EstimatedSize | ConvertTo-Json -Compress)
    $listed = @(Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq 'Recall' })
    $programs = @(Get-Package -ProviderName Programs -Name 'Recall' -ErrorAction SilentlyContinue)
    Check 'Apps & Features lists Recall once' ($listed.Count -eq 1) "uninstall entries: $($listed.Count); Programs provider: $($programs.Count)"
    Check 'no Run value after install' ($null -eq (Run-Value)) (Run-Value)
    Check 'Start menu shortcut' (Test-Path -LiteralPath $shortcut) $shortcut
    $manifest = Manifest
    $bad = @($manifest.files | Where-Object {
        $p = Join-Path $destination $_.path
        !(Test-Path -LiteralPath $p -PathType Leaf) -or (Get-Item -LiteralPath $p).Length -ne $_.size -or (Get-FileHash -LiteralPath $p).Hash -ne $_.sha256 })
    Check 'every manifest file present and unchanged' ($manifest.files.Count -gt 10 -and $bad.Count -eq 0) "$($manifest.files.Count) files, $($bad.Count) mismatched"

    # --- Update: same folder; the cache, a kept clip copy and a user file must survive.
    $id = '0123456789abcdef'
    New-Item -ItemType Directory -Force (Join-Path $destination 'cache\staging\kept'), (Join-Path $destination 'cache\thumbs') | Out-Null
    $kept = Join-Path $destination 'cache\staging\kept\Game 2026-01-01 kept.mp4'
    $thumb = Join-Path $destination "cache\thumbs\$id.jpg"
    $userFile = Join-Path $destination 'my-notes.txt'
    Set-Content -LiteralPath $kept -Value 'kept clip copy'
    Set-Content -LiteralPath $thumb -Value 'thumb'
    Set-Content -LiteralPath $userFile -Value 'user file'
    $result.updateNote = [RecallProgress]::Install($asm, $destination, $true, [RecallProgress]::new())
    Check 'no Run value after update' ($null -eq (Run-Value)) (Run-Value)
    Check 'manifest rewritten by the update' ((Manifest).files.Count -eq $manifest.files.Count) "$((Manifest).files.Count) files"
    Check 'cache and user file untouched by the update' ((Test-Path $kept) -and (Test-Path $thumb) -and (Test-Path $userFile)) ''
    Check 'Uninstall key still present' (Test-Path -LiteralPath $uninstallKey) ''

    # --- Uninstall, quiet (cache kept): the copy in the install folder hands off to a %TEMP% copy and exits.
    $manifestPaths = @((Manifest).files | ForEach-Object { Join-Path $destination $_.path })
    $setupLog = Join-Path $env:TEMP 'RecallSetup.log'
    $before = if (Test-Path $setupLog) { @(Get-Content $setupLog).Count } else { 0 }
    Start-Process -FilePath (Join-Path $destination 'RecallSetup.exe') -ArgumentList '/uninstall', '/quiet' -Wait
    $wait = (Get-Date).AddMinutes(3); $end = $null
    while (!$end -and (Get-Date) -lt $wait) {
        Start-Sleep -Milliseconds 500
        if (Test-Path $setupLog) { $end = @(Get-Content $setupLog | Select-Object -Skip $before) -match 'Uninstall (finished|failed)' | Select-Object -First 1 }
    }
    $result.uninstallLog = $end
    Check 'uninstall finished' ($end -match 'Uninstall finished') $end
    Check 'only manifest files removed' (@($manifestPaths | Where-Object { Test-Path -LiteralPath $_ }).Count -eq 0 -and
        !(Test-Path (Join-Path $destination 'install-manifest.json'))) ''
    Check 'cache, kept clip copy and user file kept' ((Test-Path $kept) -and (Test-Path $thumb) -and (Test-Path $userFile)) ''
    Check 'install folder left in place (it still holds files)' (Test-Path -LiteralPath $destination) ''
    Check 'Uninstall key removed' (!(Test-Path -LiteralPath $uninstallKey)) ''
    Check 'Start menu shortcut removed' (!(Test-Path -LiteralPath $shortcut)) ''
    Check 'no Run value after uninstall' ($null -eq (Run-Value)) (Run-Value)
    $copyGone = (Get-Date).AddSeconds(90)
    while (@(Get-ChildItem $env:TEMP -Directory -Filter 'Recall-Uninstall-*').Count -and (Get-Date) -lt $copyGone) { Start-Sleep -Milliseconds 500 }
    Check 'temporary uninstaller copy removed' (@(Get-ChildItem $env:TEMP -Directory -Filter 'Recall-Uninstall-*').Count -eq 0) ''
    $result.status = 'passed'
} catch {
    $result.status = 'failed'
    $result.failure = $_.Exception.GetBaseException().Message
} finally { Save-Result }
