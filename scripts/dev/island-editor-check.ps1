<#
.SYNOPSIS
  Live check of the island editor's panel over a real foreground window.
.DESCRIPTION
  Opens a plain test window (the "game"), brings it to the front with a
  real click, starts recall-overlay in smoke mode with the editor allowed
  and capture exclusion off, opens the editor, then clicks the panel's real
  buttons with real mouse input and checks the overlay's diagnostics after
  each click: the size, the place, the island's box. Screenshots of the
  test window's monitor go to -OutDir. Nothing touches the user's Recall
  data (smoke runs keep no files) and nothing plays audio.
  Prints PASS/FAIL lines; exits 1 on any FAIL.
.EXAMPLE
  .\island-editor-check.ps1 -Exe L:\General\.tmp\island-panel-target\debug\recall-overlay.exe -OutDir L:\General\captures\recall\island-editor-live
#>
param(
    [Parameter(Mandatory)] [string]$Exe,
    [Parameter(Mandatory)] [string]$OutDir,
    [ValidateSet('Cancel', 'Done', 'Auto')] [string]$Exit = 'Cancel'
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type -Namespace EditorCheck -Name Native -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(System.IntPtr value);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, System.UIntPtr extra);
[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint pid);
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(System.IntPtr hwnd, System.Text.StringBuilder name, int size);
'@
[void][EditorCheck.Native]::SetProcessDpiAwarenessContext([IntPtr]-4)  # per-monitor v2: physical pixels
New-Item -ItemType Directory -Force $OutDir | Out-Null
$failures = 0
function Check([bool]$ok, [string]$what) {
    if ($ok) { Write-Output "PASS $what" } else { Write-Output "FAIL $what"; $script:failures++ }
}
function Pump([int]$ms) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.ElapsedMilliseconds -lt $ms) { [Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 15 }
}
function Click([int]$x, [int]$y) {
    [void][EditorCheck.Native]::SetCursorPos($x, $y); Pump 60
    [EditorCheck.Native]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Pump 40   # left down
    [EditorCheck.Native]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero); Pump 40   # left up
}

# The test "game": a plain dark window on the primary monitor.
$screen = [Windows.Forms.Screen]::PrimaryScreen
$form = New-Object Windows.Forms.Form
$form.Text = 'Recall editor check'
$form.BackColor = [Drawing.Color]::FromArgb(40, 60, 80)
$form.StartPosition = 'Manual'
$form.FormBorderStyle = 'None'
$form.Bounds = $screen.WorkingArea
$form.Show(); Pump 300
$cx = $screen.WorkingArea.Left + [int]($screen.WorkingArea.Width / 2)
$cy = $screen.WorkingArea.Top + [int]($screen.WorkingArea.Height * 0.55)
Click $cx $cy   # a real click puts it in front
Check ([EditorCheck.Native]::GetForegroundWindow() -eq $form.Handle) 'the test window is in front'

$fixture = Join-Path $OutDir 'fixture.json'
$diag = Join-Path $OutDir 'diagnostics.json'
Remove-Item $diag -ErrorAction SilentlyContinue
$target = @{ name = 'Editor check'; exe = 'powershell.exe'; pid = $PID; hwnd = [int64]$form.Handle }
$status = @{ type = 'status'; state = 'recording'; overlay = @{ enabled = $true; stats_style = 'minimal' }; overlay_stats = $true; fps_source = 'etw'; save_seconds = 120; target = $target }
$events = @(
    @{ mod = 'session'; event = $status },
    @{ mod = 'engine'; event = @{ type = 'status'; source = 'Editor check'; fps = 144; buffer_secs = 120.0; encoder = 'h264_nvenc' } },
    @{ at_ms = 2500; mod = 'session'; event = @{ type = 'island_editor' } }
)
$json = @{ editor = $true; hold = $true; exit_ms = 45000; events = $events; stats = @{ fps = 144; low1 = 98; gpu = 71; gpu_temp = 64; cpu = 23 } } | ConvertTo-Json -Depth 8
[IO.File]::WriteAllText($fixture, $json, (New-Object Text.UTF8Encoding $false))  # no BOM: serde rejects one
$overlay = Start-Process -FilePath $Exe -ArgumentList @('--smoke-state', "`"$fixture`"", '--diagnostics', "`"$diag`"", '--smoke-allow-capture', '--corner', 'dynamic') -WindowStyle Hidden -PassThru
# A debug build is a console program: its console starts hidden, and the test window is clicked
# again before the editor shortcut (2.5 s) in case anything took the foreground.
Pump 900; Click $cx $cy
function Front {
    $h = [EditorCheck.Native]::GetForegroundWindow(); $fpid = 0
    [void][EditorCheck.Native]::GetWindowThreadProcessId($h, [ref]$fpid)
    $name = New-Object Text.StringBuilder 256; [void][EditorCheck.Native]::GetClassName($h, $name, 256)
    $proc = (Get-Process -Id $fpid -ErrorAction SilentlyContinue).ProcessName
    return "$h $($name.ToString()) $proc"
}
Check ([EditorCheck.Native]::GetForegroundWindow() -eq $form.Handle) "the test window is in front after the overlay started (front: $(Front))"

function Read-Diag { try { Get-Content -Raw $diag | ConvertFrom-Json } catch { $null } }
function Wait-Diag([scriptblock]$until, [int]$ms = 4000) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.ElapsedMilliseconds -lt $ms) {
        $d = Read-Diag
        if ($d -and (& $until $d)) { return $d }
        Pump 100
    }
    return (Read-Diag)
}
function Shot([string]$name) {
    Pump 400
    $b = $screen.Bounds
    $bitmap = New-Object Drawing.Bitmap $b.Width, $b.Height
    $g = [Drawing.Graphics]::FromImage($bitmap)
    $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bitmap.Size); $g.Dispose()
    $bitmap.Save((Join-Path $OutDir "$name.png")); $bitmap.Dispose()
}
function Button($d, [string]$action) {
    foreach ($pair in $d.editor.buttons) { if ($pair[0] -eq $action) { return $pair[1] } }
    return $null
}
function Press($d, [string]$action) {
    Pump 500; $fresh = Read-Diag; if ($fresh -and $fresh.editor) { $d = $fresh }   # the panel may have just moved
    $r = Button $d $action
    if (-not $r) { Check $false "button $action is on the panel"; return }
    Click ([int](($r[0] + $r[2]) / 2)) ([int](($r[1] + $r[3]) / 2))
}
function Height($d) { $b = $d.island.bounds_px; return $b[3] - $b[1] }

try {
    $d = Wait-Diag { param($d) $d.editor -and $d.editor.panel_px } 8000
    Check ($null -ne $d.editor) 'the editor opened over the test window'
    Check ($null -ne $d.editor.panel_px) 'the panel is drawn'
    if (-not $d.editor.panel_px) { throw 'no panel: stopping' }
    $p = $d.editor.panel_px; $a = $screen.WorkingArea
    Check ($p[0] -ge $a.Left -and $p[1] -ge $a.Top -and $p[2] -le $a.Right -and $p[3] -le $a.Bottom) "the panel is inside the work area: $($p -join ',')"
    $pill = $d.island.bounds_px
    $overlap = -not ($p[2] -le $pill[0] -or $pill[2] -le $p[0] -or $p[3] -le $pill[1] -or $pill[3] -le $p[1])
    Check (-not $overlap) "the panel does not cover the pill ($($pill -join ','))"
    Check ($d.editor.size -eq 1) 'the size starts at 100%'
    $h0 = Height $d
    Shot '1-open'

    # Hover lights a button without pressing it.
    $r = Button $d 'Larger'
    [void][EditorCheck.Native]::SetCursorPos([int](($r[0] + $r[2]) / 2), [int](($r[1] + $r[3]) / 2))
    $d = Wait-Diag { param($d) $d.editor.hot }
    Check ($d.editor.hot -and $d.editor.hot[0] -eq 'Larger' -and -not $d.editor.hot[1]) "hovering + lights it (hot: $($d.editor.hot -join ','))"
    Check ($d.editor.size -eq 1) 'hovering changes nothing'
    Press $d 'Larger'; $d = Wait-Diag { param($d) $d.editor.size -gt 1.01 }
    Press $d 'Larger'; $d = Wait-Diag { param($d) $d.editor.size -gt 1.06 }
    Check ([math]::Abs($d.editor.size - 1.10) -lt 0.001) "two + clicks: 110% (got $($d.editor.size))"
    Pump 1500; $d = Read-Diag   # the spring settles
    $ratio = (Height $d) / [math]::Max(1, $h0)
    Check ([math]::Abs($ratio - 1.10) -lt 0.04) "the island grew with it: height $h0 -> $(Height $d) ($([math]::Round($ratio, 3))x)"
    Check ([EditorCheck.Native]::GetForegroundWindow() -eq $form.Handle) 'the game kept the focus after panel clicks'
    Shot '2-larger'

    Press $d 'Place(Right)'; $d = Wait-Diag { param($d) $d.editor.edge -eq 'right' }
    Check ($d.editor.edge -eq 'right') "Right puts the pill on the right edge (got $($d.editor.edge))"
    $d = Wait-Diag { param($d) $d.island.bounds_px[2] -gt ($a.Right - 200) }
    Check ($d.island.bounds_px[2] -gt ($a.Right - 200)) "the pill moved to the right: $($d.island.bounds_px -join ',')"
    $p2 = $d.editor.panel_px
    Check ($p2[2] -lt $d.island.bounds_px[0]) 'the panel moved clear of the pill'
    Shot '3-right'

    Press $d 'Place(Free)'; $d = Wait-Diag { param($d) $d.editor.edge -eq 'free' }
    Check ($d.editor.edge -eq 'free') 'Centre frees the pill'
    Press $d 'Smaller'; $d = Wait-Diag { param($d) $d.editor.size -lt 1.09 }
    Check ([math]::Abs($d.editor.size - 1.05) -lt 0.001) "one - click: 105% (got $($d.editor.size))"
    Shot '4-centre'

    # Drag the pill into the top-left corner and hold it there with 1 px moves:
    # it must keep one look (it used to flip between wide and upright on every move).
    Pump 500; $d = Read-Diag
    $pb = $d.island.bounds_px
    [void][EditorCheck.Native]::SetCursorPos([int](($pb[0] + $pb[2]) / 2), [int](($pb[1] + $pb[3]) / 2)); Pump 80
    [EditorCheck.Native]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero); Pump 80
    # The cursor decides the edge in a corner: hugging the top a top pill, hugging the side a side pill.
    function Hold([int]$hx, [int]$hy, [string]$want) {
        $cur = [Windows.Forms.Cursor]::Position
        for ($i = 1; $i -le 30; $i++) {
            [void][EditorCheck.Native]::SetCursorPos([int]($cur.X + ($hx - $cur.X) * $i / 30), [int]($cur.Y + ($hy - $cur.Y) * $i / 30)); Pump 25
        }
        Pump 400; $bd = Read-Diag; $before = $bd.editor
        $seen = @{}
        for ($i = 0; $i -lt 40; $i++) {
            [void][EditorCheck.Native]::SetCursorPos($hx + ($i % 2), $hy + ($i % 2)); Pump 50
            $now = Read-Diag; if ($now -and $now.editor) { $seen[[string]$now.editor.edge] = 1 }
        }
        Pump 400; $after = (Read-Diag).editor
        $flips = $after.edge_changes - $before.edge_changes
        Check ($flips -eq 0 -and $seen.Count -eq 1) "held at $hx,$hy the pill keeps one edge ($flips edge changes in 40 moves; seen: $($seen.Keys -join ','))"
        Check ($after.edge -eq $want) "the cursor near the $want edge makes a $want pill (got $($after.edge))"
    }
    Hold ($a.Left + 90) ($a.Top + 20) 'top'
    Shot '5-corner-top'
    Hold ($a.Left + 20) ($a.Top + 90) 'left'
    # Exactly on the corner's diagonal a 1 px wobble keeps the edge it has.
    Hold ($a.Left + 60) ($a.Top + 60) 'left'
    [EditorCheck.Native]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero); Pump 300
    Shot '5-corner-left'

    Press $d $Exit; $d = Wait-Diag { param($d) -not $d.editor }
    Check (-not $d.editor) "$Exit closes the editor"
    Check ($d.click_through) 'the island is click-through again'
    Pump 1500; $d = Read-Diag   # the spring settles
    $h1 = Height $d
    switch ($Exit) {
        'Cancel' { Check ([math]::Abs($h1 - $h0) -le 2) "Cancel puts the size back: height $h0 -> $h1" }
        default { Check ($h1 -gt $h0) "$Exit keeps the new size: height $h0 -> $h1" }
    }
    Shot '6-closed'
} finally {
    if (-not $overlay.HasExited) { Stop-Process -Id $overlay.Id }
    $form.Close(); $form.Dispose()
}
Write-Output "screenshots: $OutDir"
if ($failures) { Write-Output "$failures FAIL"; exit 1 }
Write-Output 'ALL PASS'
