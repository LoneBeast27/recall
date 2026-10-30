# viewer-check scenarios: play/pause by every input, and play at every kind of end.
# Dot-sourced by viewer-check.ps1 ($clips, driver functions from viewer-check-lib.ps1).

Run-Scenario 'open + play from 0' {
  $s = $script:current = Start-Viewer 'play-start' $clips.base
  $c = Press-Play $s 'space' 3 0
  Verdict @((Ok ($s.OpenMs -lt 15000) "open $($s.OpenMs) ms"), $c)
}

Run-Scenario 'play/pause by the play button' {
  $s = $script:current = Start-Viewer 'button' $clips.base
  Key $s 'l' 4; $at = (Settle $s 300).pos
  $a = Press-Play $s 'button' 1.5 $at
  $p = Press-Pause $s 'button'
  $b = Press-Play $s 'button' 1.5 $p.At
  Verdict @($a, $p, $b)
}

Run-Scenario 'play/pause by Space' {
  $s = $script:current = Start-Viewer 'space' $clips.base
  Key $s 'l' 4; $at = (Settle $s 300).pos
  $a = Press-Play $s 'space' 1.5 $at
  $p = Press-Pause $s 'space'
  $b = Press-Play $s 'space' 1.5 $p.At
  Verdict @($a, $p, $b)
}

Run-Scenario 'pause by K, resume by Space' {
  $s = $script:current = Start-Viewer 'pause-resume' $clips.base
  Key $s 'l' 6; [void](Settle $s 300)
  $a = Press-Play $s 'space' 1.5
  $p = Press-Pause $s 'k'
  $b = Press-Play $s 'space' 2 $p.At
  Verdict @($a, $p, $b)
}

Run-Scenario 'play at the clip end (End, then Space) starts over' {
  $s = $script:current = Start-Viewer 'end-space' $clips.short
  Key $s 'end'
  $pic = Check-Picture $s 'at End:'
  Verdict @($pic, (Press-Play $s 'space' 1.5 0))
}

Run-Scenario 'play at the clip end by the play button starts over' {
  $s = $script:current = Start-Viewer 'end-button' $clips.short
  Key $s 'end'; [void](Settle $s 200)
  Verdict @((Press-Play $s 'button' 1.5 0))
}

# User 2026-10-30 12:06: "completing the clip watch doesnt play again".
foreach ($how in 'space', 'button') {
  Run-Scenario "clip finishes on its own, then play again ($how)" {
    $s = $script:current = Start-Viewer "finish-$how" $clips.short
    Key $s 'l' 2; [void](Settle $s 300)
    $a = Press-Play $s $how 0.2
    $end = Wait-Stopped $s 3000
    $stop = Ok ([bool]$end -and [double]$end.pos -gt [double]$end.duration - 0.06) "stopped at $(if ($end) { Fmt $end.pos } else { '(still playing)' })s of $(Fmt (Last $s).duration)s"
    Verdict @($a, $stop, (Press-Play $s $how 1.5 0))
  }
}

# User 2026-10-30 11:25: "trimmed and player stopped working" (0.91 s kept, playhead at its end).
Run-Scenario 'trimmed to 0.91 s by the out handle, play at its end' {
  $s = $script:current = Start-Viewer 'trim-091' $clips.short
  DragHandle $s 'trim-end' 0.91
  $r = Settle $s 200
  $cut = Ok ([math]::Abs([double]$r.ranges[0][1] - 0.91) -lt 0.05) "kept 0-$(Fmt $r.ranges[0][1])s, playhead $(Fmt $r.pos)s"
  $a = Press-Play $s 'button' 0.5 0
  $end = Wait-Stopped $s 2000
  $stop = Ok ([bool]$end -and [math]::Abs([double]$end.pos - [double]$r.ranges[0][1]) -lt 0.04) "stopped at $(if ($end) { Fmt $end.pos } else { '(still playing)' })s"
  Verdict @($cut, $a, $stop, (Press-Play $s 'space' 0.5 0))
}

Run-Scenario 'play to the end, trim to 0.91 s, play (the 11:25 sequence)' {
  $s = $script:current = Start-Viewer 'end-then-trim' $clips.short
  Key $s 'l' 2; [void](Settle $s 300)
  $a = Press-Play $s 'space' 0.2
  [void](Wait-Stopped $s 3000)
  DragHandle $s 'trim-end' 0.91
  Verdict @($a, (Check-Picture $s 'after the trim:'), (Press-Play $s 'button' 0.5 0))
}

Run-Scenario 'play at a segment end goes to the next segment; at the last one starts over' {
  $s = $script:current = Start-Viewer 'segments' $clips.short
  Key $s 'l'; [void](Settle $s 250); Key $s 'c'; Key $s 'l'; [void](Settle $s 250); Key $s 'c'
  Scrub $s 7.5 7.5 1; Key $s 'delete'
  $r = Settle $s 250
  $made = Ok ($r.ranges.Count -eq 2 -and [math]::Abs([double]$r.ranges[1][0] - 10.0) -lt 0.02) ("segments " + (($r.ranges | ForEach-Object { '{0}-{1}' -f (Fmt $_[0]), (Fmt $_[1]) }) -join ', ') + ", playhead $(Fmt $r.pos)s")
  $inCut = Press-Play $s 'space' 0.2 10.0
  [void](Wait-Stopped $s 2000)
  $last = Press-Play $s 'space' 1 0
  Key $s 'k'; Key $s 'home'; Key $s 'l'; [void](Settle $s 300)
  $atEnd = Press-Play $s 'button' 0.2 10.0
  Verdict @($made, $inCut, $last, $atEnd)
}

Run-Scenario 'segment ending exactly at the clip end: play at its end, L to the end, play again' {
  $s = $script:current = Start-Viewer 'tail-segment' $clips.short
  Key $s 'l'; [void](Settle $s 250); Key $s 'i'; Key $s 'end'
  $r = Settle $s 250
  $made = Ok ([math]::Abs([double]$r.ranges[0][0] - 5.0) -lt 0.02 -and [math]::Abs([double]$r.ranges[0][1] - [double]$r.duration) -lt 0.001) "kept $(Fmt $r.ranges[0][0])-$(Fmt $r.ranges[0][1])s"
  $a = Press-Play $s 'button' 1 5.0
  Key $s 'l'
  $end = Wait-Stopped $s 1500
  $stop = Ok ([bool]$end) "L while playing ran to the end: $(if ($end) { 'stopped at ' + (Fmt $end.pos) + 's' } else { 'still playing' })"
  Verdict @($made, $a, $stop, (Check-Picture $s 'at the end:'), (Press-Play $s 'space' 1 5.0))
}

Run-Scenario 'J/L and arrows at both ends' {
  $s = $script:current = Start-Viewer 'ends-keys' $clips.short
  Key $s 'j'; Key $s 'left'
  $r = Settle $s 250
  $start = Ok ([double]$r.pos -eq 0) "J and Left at 0 stay at $(Fmt $r.pos)s"
  $a = Press-Play $s 'space' 1 0
  Key $s 'k'; Key $s 'end'; Key $s 'l'; Key $s 'right' 2
  $r = Settle $s 250
  $end = Ok ([math]::Abs([double]$r.pos - [double]$r.duration) -lt 0.001) "End, L and Right stay at the end ($(Fmt $r.pos)s)"
  Verdict @($start, $a, $end, (Check-Picture $s 'at the end:'), (Press-Play $s 'space' 1 0))
}
