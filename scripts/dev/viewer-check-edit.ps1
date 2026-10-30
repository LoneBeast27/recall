# viewer-check scenarios: edits and navigation while playing, and every kind of clip.
# Dot-sourced by viewer-check.ps1 ($clips, driver functions from viewer-check-lib.ps1).

Run-Scenario 'trim start (I at ~105 s), play kept part' {
  $s = $script:current = Start-Viewer 'trim-start' $clips.base
  Key $s 'l' 21; [void](Settle $s 400); Key $s 'i'; Key $s 'home'
  $start = [double](Settle $s 400).ranges[0][0]
  Verdict @((Ok ($start -gt 100) "kept from $(Fmt $start)s"), (Press-Play $s 'space' 3 $start))
}

Run-Scenario 'drag trim handle to ~104 s, play' {
  $s = $script:current = Start-Viewer 'drag-trim' $clips.base
  DragHandle $s 'trim-start' 104
  $r = Settle $s 300
  Verdict @((Ok ([double]$r.ranges[0][0] -gt 90) "kept from $(Fmt $r.ranges[0][0])s"), (Press-Play $s 'space' 3 $r.ranges[0][0]))
}

Run-Scenario 'jump +5 s while playing' {
  $s = $script:current = Start-Viewer 'seek-playing' $clips.base
  Key $s 'l' 8; [void](Settle $s 400)
  $a = Press-Play $s 'space' 1
  $before = Last $s
  Key $s 'l'
  $hit = Wait-Trace $s { param($r) [double]$r.pos -gt [double]$before.pos + 4.5 -and [math]::Abs([double]$r.video - [double]$r.pos) -lt 0.15 } 700
  Verdict @($a, (Ok ([bool]$hit) "picture+sound at the new spot within 0.7 s: $([bool]$hit)"), (Check-Plays $s 1.5))
}

Run-Scenario 'set in while playing' {
  $s = $script:current = Start-Viewer 'in-playing' $clips.base
  Key $s 'l' 4; [void](Settle $s 300)
  $a = Press-Play $s 'space' 1
  $at = [double](Last $s).pos; Key $s 'i'
  $r = Settle $s 200
  Verdict @($a, (Ok ([math]::Abs([double]$r.ranges[0][0] - $at) -lt 0.3) "in point $(Fmt $r.ranges[0][0])s (pressed at ~$(Fmt $at)s)"), (Check-Plays $s 1.5))
}

Run-Scenario 'set out while playing stops there; play starts over' {
  $s = $script:current = Start-Viewer 'out-playing' $clips.base
  Key $s 'l' 4; [void](Settle $s 300)
  $a = Press-Play $s 'space' 1
  Key $s 'o'
  $end = Wait-Stopped $s 800
  $r = Last $s
  $stop = Ok ([bool]$end -and [math]::Abs([double]$r.pos - [double]$r.ranges[0][1]) -lt 0.05) "out point $(Fmt $r.ranges[0][1])s, stopped at $(Fmt $r.pos)s"
  Verdict @($a, $stop, (Press-Play $s 'space' 1.5 0))
}

Run-Scenario 'split, then delete the playing segment while playing' {
  $s = $script:current = Start-Viewer 'split-delete' $clips.base
  Key $s 'l' 4; [void](Settle $s 300)
  $a = Press-Play $s 'space' 1
  Key $s 'c'
  $b = Check-Plays $s 1
  $split = Ok ((Last $s).ranges.Count -eq 2) "split while playing: $((Last $s).ranges.Count) segments, still playing"
  Click $s 't-del'
  $end = Wait-Stopped $s 800
  $r = Last $s
  $del = Ok ([bool]$end -and $r.ranges.Count -eq 1) "deleted the playing (last) segment: stopped at $(Fmt $r.pos)s, kept 0-$(Fmt $r.ranges[0][1])s"
  Verdict @($a, $split, $b, $del, (Press-Play $s 'button' 1.5 0))
}

Run-Scenario 'undo and redo while playing' {
  $s = $script:current = Start-Viewer 'undo-playing' $clips.base
  Key $s 'l' 4; [void](Settle $s 300); Key $s 'c'
  $a = Press-Play $s 'space' 1
  Click $s 't-undo'; $u = Check-Plays $s 1
  $n1 = (Last $s).ranges.Count
  Click $s 't-redo'; $r = Check-Plays $s 1
  $n2 = (Last $s).ranges.Count
  Verdict @($a, (Ok ($n1 -eq 1 -and $n2 -eq 2) "segments after undo $n1, after redo $n2"), $u, $r)
}

Run-Scenario 'speed 1/4x to 4x while playing' {
  $s = $script:current = Start-Viewer 'speed-playing' $clips.base
  Key $s 'l' 4; [void](Settle $s 300)
  $checks = @(Press-Play $s 'space' 1)
  foreach ($speed in '0.25', '0.5', '2', '4', '1') {
    Click $s "speed-$speed"
    $c = Check-Plays $s 0.6
    $c.Detail = "$speed x: $($c.Detail), segment speed $((Last $s).ranges[0][2])"
    $checks += $c
  }
  Verdict $checks
}

Run-Scenario 'speed change at the clip end, then play' {
  $s = $script:current = Start-Viewer 'speed-end' $clips.short
  Key $s 'end'; [void](Settle $s 200); Click $s 'speed-2'
  $r = Settle $s 200
  Verdict @((Ok ([double]$r.ranges[0][2] -eq 2) "speed $($r.ranges[0][2])x"), (Press-Play $s 'space' 1.5 0))
}

Run-Scenario 'scrub the timeline while playing' {
  $s = $script:current = Start-Viewer 'scrub-playing' $clips.base
  Key $s 'l' 4; [void](Settle $s 300)
  $a = Press-Play $s 'space' 1
  Scrub $s 30 60 16
  $hit = Wait-Trace $s { param($r) [double]$r.pos -gt 59 -and [double]$r.pos -lt 62 } 800
  Verdict @($a, (Ok ([bool]$hit) "released at ~60 s: clock at $(Fmt (Last $s).pos)s"), (Check-Plays $s 1.5))
}

Run-Scenario 'scrub while paused, then play' {
  $s = $script:current = Start-Viewer 'scrub-paused' $clips.base
  Scrub $s 10 40 16
  $r = Settle $s 300
  Verdict @((Check-Picture $s 'after the scrub:'), (Press-Play $s 'space' 1.5 $r.pos))
}

Run-Scenario 'Library and back while playing' {
  $s = $script:current = Start-Viewer 'lib-back' $clips.short -Library
  Click $s "tile:lib-back.mp4"
  $open = Wait-Trace $s { param($r) $r.route -eq 'editor' -and $r.ready -and $r.clip } 30000
  $a = Press-Play $s 'space' 1
  Click $s 'crumb-library'
  $lib = Wait-Trace $s { param($r) $r.route -eq 'library' -and -not $r.playing } 1000
  $at = [double](Last $s).pos
  Click $s "tile:lib-back.mp4"
  $back = Wait-Trace $s { param($r) $r.route -eq 'editor' -and $r.ready } 3000
  Verdict @((Ok ([bool]$open) 'opened from its tile'), $a, (Ok ([bool]$lib) "Library stops playback (at $(Fmt $at)s)"), (Ok ([bool]$back) 'back in the editor'), (Press-Play $s 'button' 1.5 $at))
}

Run-Scenario 'open another clip while one plays; play pressed while it loads' {
  $s = $script:current = Start-Viewer 'clip-a' $clips.short -Library -Also 'clip-b'
  Click $s "tile:clip-a.mp4"
  [void](Wait-Trace $s { param($r) $r.route -eq 'editor' -and $r.ready -and $r.clip } 30000)
  $a = Press-Play $s 'space' 1
  Key $s 'escape'
  [void](Wait-Trace $s { param($r) $r.route -eq 'library' } 1000)
  Click $s "tile:clip-b.mp4"; Key $s 'space'
  $b = Wait-Trace $s { param($r) $r.clip -like '*clip-b.mp4' -and $r.playing -and $r.video_state -eq 'playing' } 30000
  $c = if ($b) { Check-Plays $s 1.5 0 ($b.ms - 1) } else { Ok $false "clip B never played (status '$((Last $s).status)', clip $((Last $s).clip))" }
  Verdict @($a, $c)
}

Run-Scenario 'minimize and restore while playing' {
  $s = $script:current = Start-Viewer 'minimize' $clips.base
  Key $s 'l' 4; [void](Settle $s 300)
  $a = Press-Play $s 'space' 1
  $before = [double](Last $s).pos
  Minimize $s; Start-Sleep -Milliseconds 1500; Restore $s
  $r = Settle $s 300
  $kept = Ok ($r.playing -and [double]$r.pos - $before -gt 1.3) "kept playing while minimized (+$(Fmt ([double]$r.pos - $before))s)"
  Verdict @($a, $kept, (Check-Plays $s 1.5))
}

Run-Scenario 'very short clip (0.8 s): play, finish, play again' {
  $s = $script:current = Start-Viewer 'tiny-run' $clips.tiny
  $a = Press-Play $s 'space' 0.4 0
  $end = Wait-Stopped $s 1500
  Verdict @($a, (Ok ([bool]$end) "stopped at $(if ($end) { Fmt $end.pos } else { '?' })s"), (Press-Play $s 'button' 0.4 0))
}

Run-Scenario 'clip without sound: play to the end, play again' {
  $s = $script:current = Start-Viewer 'noaudio-run' $clips.noaudio
  Key $s 'l' 2; [void](Settle $s 300)
  $a = Press-Play $s 'space' 0.2
  [void](Wait-Stopped $s 3000)
  $r = Settle $s 300 # the playhead rests on the last frame, not back at 0
  $stop = Ok (-not $r.playing -and [double]$r.pos -gt [double]$r.duration - 0.06) "stopped, playhead at $(Fmt $r.pos)s of $(Fmt $r.duration)s"
  Verdict @($a, $stop, (Check-Picture $s 'at the end:'), (Press-Play $s 'space' 1.5 0))
}

Run-Scenario 'sound shorter than the picture: plays to the picture end, then again' {
  $s = $script:current = Start-Viewer 'shortsound-run' $clips.shortsound
  Key $s 'l' 2; Key $s 'left' 6; [void](Settle $s 300) # 9.9 s: where the sound ends
  $a = Press-Play $s 'space' 0.2
  $end = Wait-Stopped $s 3000
  $stop = Ok ([bool]$end -and [double]$end.pos -gt [double]$end.duration - 0.06) "stopped at $(if ($end) { Fmt $end.pos } else { '?' })s, picture ends at $(Fmt (Last $s).duration)s"
  Verdict @($a, $stop, (Press-Play $s 'space' 1.5 0))
}

Run-Scenario 'Moments clip plays continuous sound' {
  # User 2026-10-27 "audio playback sounds bad, very choppy": Moments audio stamps sit 1000/1100
  # ticks apart for 1024-sample frames. The stems must have no holes inside sound, and the output
  # must never run dry while playing.
  $s = $script:current = Start-Viewer 'moments-sound' $clips.moments
  $a = Press-Play $s 'space' 4
  $r = Last $s
  $holes = @(@($r.stems) | ForEach-Object { [VC]::Holes($_) })
  $worst = ($holes | Measure-Object -Maximum).Maximum
  Verdict @($a, (Ok (@($r.stems).Count -gt 0 -and $worst -eq 0 -and [int]$r.audio_underruns -eq 0) "$(@($r.stems).Count) stems, holes inside sound per stem $($holes -join '/'), output underruns $($r.audio_underruns)"))
}

Run-Scenario 'Moments clip: finishes, plays again' {
  $s = $script:current = Start-Viewer 'moments-end' $clips.moments
  Key $s 'end'; Key $s 'j'; [void](Settle $s 300)
  $a = Press-Play $s 'space' 0.5
  $end = Wait-Stopped $s 7000
  Verdict @($a, (Ok ([bool]$end) "stopped at $(if ($end) { Fmt $end.pos } else { '?' })s"), (Press-Play $s 'button' 1.5 0))
}

Run-Scenario 'AV1 clip: plays, finishes, plays again' {
  $s = $script:current = Start-Viewer 'av1-run' $clips.av1
  $src = (Last $s).source
  Key $s 'l' 2; [void](Settle $s 300)
  $a = Press-Play $s 'space' 0.2
  $end = Wait-Stopped $s 3000
  Verdict @((Ok $true "plays $(if (-not $src) { 'an unknown source' } elseif ($src -notlike '*av1-run.mp4') { 'a preview copy' } else { 'the AV1 file' })"), $a, (Ok ([bool]$end) "stopped at $(if ($end) { Fmt $end.pos } else { '?' })s"), (Press-Play $s 'space' 1.5 0))
}
