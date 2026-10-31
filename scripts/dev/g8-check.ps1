# ADVERSITY G8: which fullscreen path a game is on while the island is up.
# Reads window lists and one shell state. Opens no handle to the game and injects nothing.
#   powershell -NoProfile -File scripts\dev\g8-check.ps1
# One line per change, written to the console and to -Log. Only lines with islandShown=True
# and the game in front count: islandShown=False means "not seen" proves nothing.
param([int]$Seconds = 600, [string]$Log = "$env:TEMP\recall-g8-check.log")

Add-Type -Name G8 -Namespace Check -MemberDefinition @'
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, string t);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, System.Text.StringBuilder s, int n);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint c);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
[DllImport("shell32.dll")] public static extern int SHQueryUserNotificationState(out int s);
'@

$states = @{ 1 = 'not present'; 2 = 'busy'; 3 = 'D3D full screen'; 4 = 'presentation'; 5 = 'accepts notifications'; 6 = 'quiet time'; 7 = 'store app' }
$last = ''
Write-Output "Logging to $Log for $Seconds s. Press Ctrl+C to stop."
for ($t = 0; $t -lt $Seconds; $t++) {
  $s = 0
  $null = [Check.G8]::SHQueryUserNotificationState([ref]$s)
  $front = [Check.G8]::GetForegroundWindow()
  $class = New-Object Text.StringBuilder 128
  $null = [Check.G8]::GetClassNameW($front, $class, 128)
  $owner = [uint32]0
  $null = [Check.G8]::GetWindowThreadProcessId($front, [ref]$owner)
  $name = (Get-Process -Id $owner -ErrorAction SilentlyContinue).ProcessName
  $proxy = [Check.G8]::FindWindowW('D3DProxyWindow', $null)
  $island = [Check.G8]::FindWindowW('RecallNativeStatusOverlay', $null)
  $above = $false
  $w = $island
  while ($w -ne [IntPtr]::Zero) {
    $w = [Check.G8]::GetWindow($w, 3)
    if ($w -eq $proxy -and $proxy -ne [IntPtr]::Zero) { $above = $true }
  }
  $line = "state=$($states[$s]) foreground=$name/$class proxy=$($proxy -ne [IntPtr]::Zero) islandShown=$([Check.G8]::IsWindowVisible($island)) proxyAboveIsland=$above"
  if ($line -ne $last) {
    $stamped = "$(Get-Date -Format 'HH:mm:ss') $line"
    Write-Output $stamped
    Add-Content -Path $Log -Value $stamped
    $last = $line
  }
  Start-Sleep 1
}
