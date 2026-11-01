# Clip card and editor exits (dot-sourced by viewer-check.ps1): every short-lived state ends the way
# a person expects, the keyboard comes back afterwards, and a press that ends one thing does not
# start another. The states are in the trace's `ui` block.

function UI($s) { (Settle $s).ui }
function Tile($name) { "tile:$name.mp4" }
function Hover($s, [string]$name) { $t = Target $s $name; Mouse $s 0x200 $t.X $t.Y 0; Start-Sleep -Milliseconds 80 }
function RightClick($s, [string]$name) {
  $t = Target $s $name
  Mouse $s 0x200 $t.X $t.Y 0; Start-Sleep -Milliseconds 40
  Mouse $s 0x204 $t.X $t.Y 2; Start-Sleep -Milliseconds 40
  Mouse $s 0x205 $t.X $t.Y 0; Start-Sleep -Milliseconds 60
}
function Press($s, [string]$name) { $t = Target $s $name; Mouse $s 0x200 $t.X $t.Y 0; Start-Sleep -Milliseconds 40; Mouse $s 0x201 $t.X $t.Y 1; Start-Sleep -Milliseconds 40 }
# Opens the rename field from the card's pencil.
function Begin-Rename($s, [string]$tile) {
  Hover $s $tile; Click $s 'tile-rename'
  [void](Wait-Trace $s { param($r) $r.ui.field -eq 'Rename' } 1500)
}
# The page answers keys again: / reaches the search box, Escape leaves it.
function Keys-Alive($s) {
  Key $s 'slash'
  $on = Wait-Trace $s { param($r) $r.ui.field -eq 'Search' } 1000
  Key $s 'escape'
  $off = Wait-Trace $s { param($r) $null -eq $r.ui.field } 1000
  Ok ([bool]($on -and $off)) 'the keyboard works on the page'
}
function Closed($u) { Ok ($null -eq $u.renaming -and $u.field -ne 'Rename') "field closed (renaming '$($u.renaming)', keys on '$($u.field)')" }
function In-Library($s) { $r = Settle $s 300; Ok ($r.route -eq 'library') "still in the library (route $($r.route))" }

Run-Scenario 'card: click away saves the title and only ends the rename' {
  $s = $script:current = Start-Viewer 'away' $clips.short -Library -Also 'away-b'
  Begin-Rename $s (Tile 'away')
  $open = UI $s
  Key $s 'a'; Key $s 'b'
  Click $s (Tile 'away-b')
  $u = UI $s
  Verdict @((Ok ($open.field -eq 'Rename') 'the pencil opens the field'), (Closed $u), (Ok ($u.titles -contains 'ab') "title saved ($($u.titles -join ', '))"), (In-Library $s), (Keys-Alive $s))
}

Run-Scenario 'card: Enter and Tab save, Escape keeps the old title' {
  $s = $script:current = Start-Viewer 'keys' $clips.short -Library -Also 'keys-b', 'keys-c'
  $before = (UI $s).titles
  Begin-Rename $s (Tile 'keys'); Key $s 'a'; Key $s 'escape'
  $esc = UI $s
  Begin-Rename $s (Tile 'keys'); Key $s 'a'; Key $s 'enter'
  $enter = UI $s
  Begin-Rename $s (Tile 'keys-b'); Key $s 'b'; Key $s 'tab'
  $tab = UI $s
  Verdict @((Closed $esc), (Ok ($esc.custom_titles -eq 0 -and $esc.titles[0] -eq $before[0]) 'Escape keeps the title'),
    (Closed $enter), (Ok ($enter.titles -contains 'a') 'Enter saves'),
    (Closed $tab), (Ok ($tab.titles -contains 'b') 'Tab saves'), (Keys-Alive $s))
}

Run-Scenario 'card: leaving the field untouched pins no title' {
  $s = $script:current = Start-Viewer 'plain' $clips.short -Library -Also 'plain-b'
  Begin-Rename $s (Tile 'plain')
  Click $s (Tile 'plain-b')
  $u = UI $s
  Begin-Rename $s (Tile 'plain'); Key $s 'enter'
  $v = UI $s
  Verdict @((Ok ($u.custom_titles -eq 0) "click away: $($u.custom_titles) custom titles"), (Ok ($v.custom_titles -eq 0) "Enter: $($v.custom_titles) custom titles"))
}

Run-Scenario 'card: a click in the field stays in the field and types on' {
  $s = $script:current = Start-Viewer 'inside' $clips.short -Library
  Begin-Rename $s (Tile 'inside')
  $text = (UI $s).text
  Click $s 'rename'
  Key $s 'a'
  $u = UI $s
  Verdict @((In-Library $s), (Ok ($u.field -eq 'Rename') "field keeps the keys ($($u.field))"), (Ok ($u.text -eq "${text}a") "text '$($u.text)' (was '$text')"))
}

Run-Scenario 'card: a second pencil saves the first rename' {
  $s = $script:current = Start-Viewer 'first' $clips.short -Library -Also 'second'
  Begin-Rename $s (Tile 'first'); Key $s 'a'
  Begin-Rename $s (Tile 'second')
  $u = UI $s
  Verdict @((Ok ($u.titles -contains 'a') "first title saved ($($u.titles -join ', '))"), (Ok ($u.renaming -like '*second.mp4') "now renaming '$($u.renaming)'"), (Ok ($u.field -eq 'Rename') 'keys on the new field'))
}

Run-Scenario 'card: a page change ends the rename' {
  $s = $script:current = Start-Viewer 'pages' $clips.short -Library
  Begin-Rename $s (Tile 'pages'); Key $s 'a'
  Click $s 'nav-settings'
  $u = UI $s
  Key $s 'escape'
  $back = Wait-Trace $s { param($r) $r.route -eq 'library' } 1000
  $v = UI $s
  Verdict @((Closed $u), (Ok ($u.titles -contains 'a') 'title saved'), (Ok ([bool]$back) 'Escape on Settings still works'), (Closed $v), (Keys-Alive $s))
}

Run-Scenario 'card: the title bar ends the rename too' {
  $s = $script:current = Start-Viewer 'bar' $clips.short -Library
  Begin-Rename $s (Tile 'bar'); Key $s 'a'
  Click $s 'save-replay'
  $u = UI $s
  Verdict @((Closed $u), (Ok ($u.titles -contains 'a') 'title saved'), (Keys-Alive $s))
}

Run-Scenario 'card: a clip that vanishes takes its rename along' {
  $s = $script:current = Start-Viewer 'gone' $clips.short -Library -Also 'stays'
  Begin-Rename $s (Tile 'gone'); Key $s 'a'
  # The hover's scrub frames read the file for a moment: another program's delete waits for that.
  $t = Get-Date
  while (Test-Path $s.Clip) {
    try { Remove-Item $s.Clip -ErrorAction Stop } catch { if (((Get-Date) - $t).TotalSeconds -gt 15) { throw }; Start-Sleep -Milliseconds 250 }
  }
  $left = Wait-Trace $s { param($r) $r.clips -eq 1 } 6000
  $u = UI $s
  Verdict @((Ok ([bool]$left) 'the tile left'), (Closed $u), (Keys-Alive $s))
}

Run-Scenario 'card: scrolling the tile off screen ends the rename' {
  $names = 1..89 | ForEach-Object { "scrolled-$_" } # more rows than the window holds
  $s = $script:current = Start-Viewer 'a-scrolled' $clips.short -Library -Also $names # listed first: it must be on screen
  $first = (Last $s).targets.PSObject.Properties | Where-Object { $_.Name -like 'tile:*' } | Sort-Object { $_.Value[1] }, { $_.Value[0] } | Select-Object -First 1
  Hover $s $first.Name; Click $s 'tile-rename'
  [void](Wait-Trace $s { param($r) $r.ui.field -eq 'Rename' } 1500)
  Key $s 'a'
  $t = Target $s $first.Name
  $still = UI $s
  Mouse $s 0x200 $t.X ($t.Y + 40) 0; Start-Sleep -Milliseconds 80
  Wheel $s $t.X ($t.Y + 40) -240 12
  $ended = Wait-Trace $s { param($r) $null -eq $r.ui.renaming } 2000
  $u = UI $s
  Verdict @((Ok ($still.field -eq 'Rename') 'open before the scroll'), (Ok ([bool]$ended) 'ended with the tile off screen'), (Ok ($u.titles -contains 'a') 'title saved'), (Keys-Alive $s))
}

Run-Scenario 'card: closing a menu by clicking a tile does not open the tile' {
  $s = $script:current = Start-Viewer 'menu' $clips.short -Library -Also 'menu-b'
  RightClick $s (Tile 'menu')
  $open = UI $s
  Click $s (Tile 'menu-b')
  $u = UI $s
  Click $s 'sort'
  $sort = UI $s
  Click $s (Tile 'menu-b')
  $v = UI $s
  Verdict @((Ok ($open.tile_menu -eq $true) 'right-click opens the menu'), (Ok ($u.tile_menu -eq $false) 'click closes it'), (Ok ($sort.sort_open -eq $true) 'sort menu opens'), (Ok ($v.sort_open -eq $false) 'click closes it'), (In-Library $s))
}

Run-Scenario 'card: the sort menu closes every way it can' {
  $s = $script:current = Start-Viewer 'sorter' $clips.short -Library
  Click $s 'sort'; $a = (UI $s).sort_open
  Key $s 'escape'; $b = (UI $s).sort_open
  Click $s 'sort'; $c = (UI $s).sort_open
  Click $s 'sort'; $d = (UI $s).sort_open
  Verdict @((Ok ($a -eq $true) 'opens'), (Ok ($b -eq $false) 'Escape closes it'), (Ok ($c -eq $true) 'opens again'), (Ok ($d -eq $false) 'its button closes it'))
}

Run-Scenario 'card: Escape steps back from the delete question' {
  $s = $script:current = Start-Viewer 'ask' $clips.short -Library -Also 'ask-b'
  RightClick $s (Tile 'ask'); Click $s 'tile-menu-delete'
  $asked = UI $s
  Key $s 'slash'
  $slash = UI $s
  Key $s 'escape'; $kept = UI $s
  Key $s 'escape'; $clear = UI $s
  RightClick $s (Tile 'ask'); Click $s 'tile-menu-delete'
  Click $s 'nav-settings'; Key $s 'escape'
  [void](Wait-Trace $s { param($r) $r.route -eq 'library' } 1000)
  $left = UI $s
  Verdict @((Ok ($asked.confirm_delete -eq $true) 'the question shows'), (Ok ($slash.field -ne 'Search') "no search box to focus ($($slash.field))"),
    (Ok ($kept.confirm_delete -eq $false -and $kept.selected -eq 1) "first Escape is Keep ($($kept.selected) selected)"), (Ok ($clear.selected -eq 0) 'second clears the selection'),
    (Ok ($left.confirm_delete -eq $false) 'leaving the page drops the question'))
}

Run-Scenario 'card: the second click of a double-click hits nothing in the editor' {
  $names = 1..23 | ForEach-Object { "dbl-$_" }
  $s = $script:current = Start-Viewer 'dbl' $clips.short -Library -Also $names
  # A tile low on the page: the editor's timeline is under it.
  $tile = (Last $s).targets.PSObject.Properties | Where-Object { $_.Name -like 'tile:*' -and $_.Value[1] -gt 420 } | Sort-Object { -$_.Value[0] } | Select-Object -First 1
  if (-not $tile) { throw 'no tile low enough on the page' }
  $x = [int]($tile.Value[0] + $tile.Value[2] / 2); $y = [int]($tile.Value[1] + $tile.Value[3] * 0.8)
  Mouse $s 0x200 $x $y 0; Start-Sleep -Milliseconds 40
  Mouse $s 0x201 $x $y 1; Start-Sleep -Milliseconds 20; Mouse $s 0x202 $x $y 0; Start-Sleep -Milliseconds 90
  Mouse $s 0x203 $x $y 1; Start-Sleep -Milliseconds 20; Mouse $s 0x201 $x $y 1; Start-Sleep -Milliseconds 20; Mouse $s 0x202 $x $y 0
  $r = Wait-Trace $s { param($r) $r.route -eq 'editor' -and $r.ready -and $r.clip } 30000
  $r = Settle $s 200
  $tracks = Target $s 'tracks'
  Verdict @((Ok ([bool]$r) 'one double-click opens the clip'), (Ok ($y -gt $tracks.Top) "the click point ($y) is on the timeline (from $($tracks.Top))"), (Ok ([double]$r.pos -lt 0.01 -and -not $r.drag) "playhead stayed at $(Fmt $r.pos)s"))
}

Run-Scenario 'editor: a press on a handle or the timeline is no edit' {
  $s = $script:current = Start-Viewer 'noedit' $clips.short
  Click $s 'trim-end'
  $a = Settle $s 900
  Scrub $s 3 5
  $b = Settle $s 900
  Verdict @((Ok (-not $a.undo -and $a.saved) "handle press: undo $($a.undo), saved $($a.saved)"), (Ok (-not $b.undo -and $b.saved) "scrub: undo $($b.undo), saved $($b.saved)"), (Ok (-not (Test-Path "$($s.Clip).recall-edit.json")) 'no edit file beside the clip'))
}

Run-Scenario 'editor: Escape during a trim puts it back and stays' {
  $s = $script:current = Start-Viewer 'trimesc' $clips.short
  $before = (Settle $s).ranges[0]
  Press $s 'trim-end'
  $h = Target $s 'trim-end'; $x = TrackX $s 5
  for ($n = 1; $n -le 10; $n++) { Mouse $s 0x200 ([int]($h.X + ($x - $h.X) * $n / 10)) $h.Y 1; Start-Sleep -Milliseconds 16 }
  $mid = Settle $s 60
  Key $s 'escape'
  $esc = Settle $s
  Mouse $s 0x202 $x $h.Y 0
  $r = Settle $s
  Verdict @((Ok ([double]$mid.ranges[0][1] -lt [double]$before[1] - 1) "the drag moved the end to $(Fmt $mid.ranges[0][1])s"), (Ok ($esc.route -eq 'editor' -and -not $esc.drag) 'Escape ends the drag and stays in the editor'),
    (Ok ([double]$r.ranges[0][1] -eq [double]$before[1]) "end back at $(Fmt $r.ranges[0][1])s"), (Ok (-not $r.undo) 'no undo step'))
}

Run-Scenario 'editor: a trim released over the title bar ends' {
  $s = $script:current = Start-Viewer 'trimbar' $clips.short
  Press $s 'trim-end'
  $h = Target $s 'trim-end'; $x = TrackX $s 5
  for ($n = 1; $n -le 10; $n++) { Mouse $s 0x200 ([int]($h.X + ($x - $h.X) * $n / 10)) $h.Y 1; Start-Sleep -Milliseconds 16 }
  $bar = Target $s 'save-replay'
  Mouse $s 0x200 $x $bar.Y 1; Start-Sleep -Milliseconds 40
  Mouse $s 0x202 $x $bar.Y 0
  $r = Settle $s
  Verdict @((Ok (-not $r.drag) 'the drag ended'), (Ok ($r.undo) 'the trim is one undo step'))
}

Run-Scenario 'editor: the fps field cancels, takes digits only, and leaves with the page' {
  $s = $script:current = Start-Viewer 'fpsfield' $clips.short
  Click $s 'mode-Re-encode'; Click $s 'fps-custom'
  $before = (Settle $s).fps
  Key $s 'a'; Key $s 'd1'; Key $s 'd2'; Key $s 'd3'
  $typed = Settle $s
  Key $s 'escape'
  $esc = Settle $s
  Click $s 'fps-custom'
  Click $s 'crumb-library'
  [void](Wait-Trace $s { param($r) $r.route -eq 'library' } 1000)
  Verdict @((Ok ([int]$typed.fps -eq 123) "digits count, letters don't (fps $($typed.fps))"), (Ok ([int]$esc.fps -eq [int]$before) "Escape puts $before back (fps $($esc.fps))"), (Ok ((UI $s).field -ne 'Fps') 'the field lets go of the keyboard'), (Keys-Alive $s))
}
