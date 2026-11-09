//! The sync handshake (design-audio-mute §3.3 P3, P5-P7): every device track starts held and
//! records only after the controller's `sync` line was read AND every track is listed. A
//! channel pipe stands in for stdin and the rings are test inputs (no WASAPI, no device).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::read_early;
use super::tests::{restore, spawn, Reopen};
use crate::audio::{self, MuteNow, TrackRing};

const SYNC: &str = "{\"cmd\":\"sync\",\"seq\":1}\n";

/// An input as `audio::start(.., hold = true, ..)` leaves it: held and listed.
fn held_listed(id: &'static str) -> Arc<TrackRing> {
    let ring = Arc::new(TrackRing::input_for_test(id));
    ring.hold_startup();
    audio::sources::register_for_test(id, &ring);
    ring
}

fn close_staging() {
    audio::sources::close_staging_for_test(|id, change| {
        audio::sources::set_live(id, change);
    });
}

fn wait_until(what: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !what() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    what()
}

fn silent(ring: &TrackRing) -> bool {
    ring.push_live(&[0.5; 960], MuteNow::read());
    ring.snapshot(0.01).iter().all(|s| *s == 0.0)
}

/// R-fresh-2: the pipe is empty when the tracks start (the controller's writer is slow), the
/// Alt+M arrives 50 ms later, then `sync`. No packet before the mute is applied is live.
#[test]
fn a_mute_written_after_the_start_then_sync_mutes_the_first_stored_packet() {
    let _restore = restore();
    let _reopen = Reopen;
    audio::set_mic_muted(false);
    let ring = held_listed("sync-mic");
    close_staging(); // every track listed; no sync yet
    let (lines, _commands) = spawn(Arc::new(AtomicBool::new(false)));
    std::thread::sleep(Duration::from_millis(50));
    assert!(ring.startup_held(), "an empty pipe releases nothing");
    assert!(silent(&ring), "held: silence");
    lines.send(b"{\"cmd\":\"set_mic_mute\",\"muted\":true}\n".to_vec()).unwrap();
    assert!(wait_until(audio::mic_muted), "the mute is applied");
    assert!(ring.startup_held(), "a mute line alone is no sync");
    lines.send(SYNC.as_bytes().to_vec()).unwrap();
    assert!(wait_until(|| !ring.startup_held()), "the sync releases it");
    assert!(silent(&ring), "the first live packet is the muted mic: silence");
}

/// P5: `sync` read before the tracks are listed, a newer set_source after it. The release waits
/// for the listing, and comes after both staged lines are on the track (the newer one wins).
#[test]
fn a_sync_before_the_listing_releases_after_the_staged_lines() {
    let _restore = restore();
    let _reopen = Reopen;
    let lines = format!("{}{SYNC}{}", "{\"cmd\":\"set_source\",\"id\":\"sync-line\",\"mic_mute\":false}\n", "{\"cmd\":\"set_source\",\"id\":\"sync-line\",\"mic_mute\":true,\"device_id\":\"{B}\"}\n");
    read_early(std::io::Cursor::new(lines), |_| {});
    let ring = held_listed("sync-line");
    ring.set_mic_mute(false); // the env said line-in
    assert!(ring.startup_held(), "synced but not listed yet: held");
    close_staging();
    assert!(!ring.startup_held(), "listed after the sync: released");
    assert!(ring.mic_mute_flag(), "the newer line won before the release");
    assert_eq!(ring.saved_id_for_test(), "{B}");
}

/// P6/P7: no `sync` ever (an older controller, or the end of stdin first): held for the run.
#[test]
fn no_sync_or_an_end_of_stdin_before_it_never_releases() {
    let _restore = restore();
    let _reopen = Reopen;
    read_early(std::io::Cursor::new("{\"cmd\":\"set_mic_mute\",\"muted\":false}\n"), |_| {}); // then EOF
    let ring = held_listed("sync-none");
    close_staging();
    assert!(ring.startup_held() && silent(&ring), "no sync: held, silence");
    assert!(!audio::sources::startup_released());
}

/// Merge-gate F8 through the handshake: a set_source (line-in -> "Alt+M mutes it", a re-picked
/// id) then `sync`, read after the tracks are listed. Both are in force before the release.
#[test]
fn a_set_source_through_the_held_start_is_in_force_before_the_release() {
    let _restore = restore();
    let _reopen = Reopen;
    let ring = held_listed("f8-line");
    ring.set_mic_mute(false);
    close_staging();
    let (lines, _commands) = spawn(Arc::new(AtomicBool::new(false)));
    lines.send(b"{\"cmd\":\"set_source\",\"id\":\"f8-line\",\"mic_mute\":true,\"device_id\":\"{B}\"}\n".to_vec()).unwrap();
    lines.send(SYNC.as_bytes().to_vec()).unwrap();
    assert!(wait_until(|| !ring.startup_held()), "released by the sync");
    assert!(ring.mic_mute_flag(), "Alt+M mutes it before the first stream opens");
    assert_eq!(ring.saved_id_for_test(), "{B}", "and the stream opens on the device the user picked");
}

/// P3: a device track starts held whenever a controller drives the engine; a process-loopback
/// track (the game's process tree) does not, as no line can change what it captures.
#[test]
fn every_device_track_starts_held_and_process_loopback_does_not() {
    assert!(audio::starts_held(true, false), "a device track: held");
    assert!(!audio::starts_held(true, true), "process loopback: free");
    assert!(!audio::starts_held(false, false), "no controller: free");
    let audio_src = include_str!("audio.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(audio_src.contains("if starts_held(hold, target.is_some()) { ring.hold_startup();"), "start_track holds by it");
    let main_src = include_str!("main.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(main_src.contains("let hold = early_stdin.is_some() && engine_stdin::startup_hold();"), "main holds under a controller");
    assert!(main_src.contains("audio::start(buffer_secs, hold,"), "and passes it to the start");
}

/// R-fresh-4 / F7 (b955512 changed): the notice only after 2 s still held (fake clock), never
/// for a normal start that synced in time.
#[test]
fn inputs_held_is_said_only_after_2_s_still_held() {
    let waited = std::cell::Cell::new(Duration::ZERO);
    let mut said = Vec::new();
    assert!(super::watch_hold(|d| waited.set(d), || false, |o| said.push(serde_json::to_value(o).unwrap())));
    assert_eq!(waited.get(), Duration::from_secs(2));
    assert_eq!(said[0]["type"], "inputs_held");
    assert!(!super::watch_hold(|_| {}, || true, |_| panic!("synced in time: no notice")));
    let src = include_str!("engine_stdin.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(src.contains("std::thread::spawn(|| watch_hold(std::thread::sleep, audio::sources::startup_released, protocol::emit));"), "spawn_early starts the watch");
    let sources = include_str!("audio/sources.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(sources.contains("crate::protocol::emit(&crate::protocol::Outbound::InputsReleased);"), "the release says so");
}

/// P13: each audio line's `seq` is acked (`status.audio_seq`) once it is applied, in order.
#[test]
fn an_applied_line_is_acked_by_its_seq() {
    let _restore = restore();
    let _reopen = Reopen;
    read_early(std::io::Cursor::new("{\"cmd\":\"restate\",\"seq\":999999}
{\"cmd\":\"set_mic_mute\",\"muted\":true,\"seq\":1000000}
"), |_| {});
    assert!(audio::mic_muted());
    assert_eq!(audio::sources::audio_seq(), 1_000_000, "acked after it was applied");
    let tick = include_str!("app_tick.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(tick.contains("audio_seq: { #[cfg(feature=\"encode\")] { audio::sources::audio_seq() }"), "the status carries it");
}

/// P4 through the reader: a `retire` read after the listing silences the live track at once;
/// one read before it (staged) leaves the track held for good, the sync's release included.
#[test]
fn a_retire_line_holds_the_track_for_good() {
    let _restore = restore();
    let _reopen = Reopen;
    audio::set_mic_muted(false);
    let staged = held_listed("retire-staged");
    read_early(std::io::Cursor::new(format!("{}{SYNC}", "{\"cmd\":\"set_source\",\"id\":\"retire-staged\",\"retire\":true}\n")), |_| {});
    close_staging();
    assert!(staged.retired() && staged.startup_held(), "staged retire: held after the sync");
    let live = held_listed("retire-live");
    live.release_startup_hold();
    assert!(!silent(&live), "recording");
    read_early(std::io::Cursor::new("{\"cmd\":\"set_source\",\"id\":\"retire-live\",\"retire\":true}\n"), |_| {});
    assert!(live.retired() && silent(&live), "retired live: the next packet is silence");
    let opened = audio::sources::opened();
    let me = opened.iter().find(|o| o.id == "retire-live").unwrap();
    assert!(me.retired && me.held, "the status says so");
}

/// §3.2: the protocol marker is in the engine binary (the controller's floor check reads it),
/// and `hello` says `sync_hold`.
#[test]
fn the_binary_carries_the_protocol_marker_and_hello_says_sync_hold() {
    assert_eq!(super::PROTOCOL_MARKER.as_bytes(), &super::PROTOCOL_MARKER_BYTES);
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    assert!(exe.windows(super::PROTOCOL_MARKER.len()).any(|w| w == super::PROTOCOL_MARKER.as_bytes()), "the marker is in the built binary");
    let main_src = include_str!("main.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(main_src.contains("live_source_fields: true, sync_hold: true,"), "hello says it");
}

/// P17, the gate's 21:10 A4 conditions as invariants: (1) every device stream opens shared,
/// never exclusive; (2) a held track says so in the status (`held`); (3) only the user's press
/// counts toward adopting an id: a restatement's mute carries none.
#[test]
fn the_a4_sibling_conditions_hold() {
    let stream = include_str!("audio/stream.rs");
    assert_eq!(stream.matches("StreamMode::EventsShared").count(), 1, "one open mode: shared");
    assert!(!stream.contains("Exclusive"), "never exclusive");
    let _restore = restore();
    let _reopen = Reopen;
    let ring = held_listed("a4-mic");
    assert!(audio::sources::opened().iter().any(|o| o.id == "a4-mic" && o.held), "held shows as held");
    let presses = MuteNow::read().presses;
    read_early(std::io::Cursor::new("{\"cmd\":\"set_mic_mute\",\"muted\":true,\"seq\":5}\n{\"cmd\":\"set_mic_mute\",\"muted\":false,\"seq\":6}\n"), |_| {});
    assert_eq!(MuteNow::read().presses, presses, "a restatement is no press: nothing is adopted");
    drop(ring);
}

/// The quit-grace gap: after `quit` the engine reads no line, so a mute sent then could never
/// reach it. The reader holds every device track before `quit` is set: the long recording's
/// finalise stores silence for them; the game's process tree keeps recording.
#[test]
fn a_quit_holds_every_device_track_before_the_finalise() {
    let _restore = restore();
    let _reopen = Reopen;
    audio::set_mic_muted(false);
    let mic = held_listed("quit-mic");
    mic.release_startup_hold();
    assert!(!silent(&mic), "recording before the quit");
    let spec = crate::audio::sources::SourceSpec { communications: false, device_id: String::new(), id: "quit-game".into(), kind: "game".into(), device: String::new(), exe: String::new(), name: "Game".into(), gain_db: 0.0, mic_mute: None };
    let game = Arc::new(TrackRing::for_source_for_test(&spec, crate::edit_model::SOURCE_GAME_PROCESS));
    audio::sources::register_for_test("quit-game", &game);
    let quit = Arc::new(AtomicBool::new(false));
    let end = read_early(std::io::Cursor::new("{\"cmd\":\"quit\"}\n"), |_| {});
    assert_eq!(end, crate::stdin_reader::ReaderEnd::Quit);
    super::hold_at_end(); // `reader_end`, before `finish`
    crate::stdin_reader::reader_ended(end, &quit, &AtomicBool::new(false)); // what `finish` does next
    assert!(quit.load(std::sync::atomic::Ordering::SeqCst));
    assert!(mic.retired() && silent(&mic), "the mic stores silence for the finalise");
    game.push_live(&[0.5; 960], MuteNow::read());
    assert!(!game.retired() && game.snapshot(0.01).iter().any(|s| *s != 0.0), "the game's process tree records on");
    let src = include_str!("engine_stdin.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(src.contains("fn reader_end(end: stdin_reader::ReaderEnd, quit: &std::sync::atomic::AtomicBool) { hold_at_end(); stdin_reader::finish(end, quit); }"), "the reader holds before `finish` sets quit");
    assert!(src.contains("reader_end(end, &shared.quit);"), "the engine's reader ends through it");
}

/// N1: `sync`, then `quit`, both read before `audio::start` has listed the tracks. A track
/// listed after the quit starts retired, and the close never releases it.
#[test]
fn a_quit_before_the_listing_leaves_later_tracks_silent() {
    let _restore = restore();
    let _reopen = Reopen;
    audio::set_mic_muted(false);
    read_early(std::io::Cursor::new(SYNC), |_| {});
    audio::sources::hold_for_quit(); // what the reader does at its end (`reader_end`)
    let late = held_listed("n1-late");
    close_staging();
    assert!(late.retired() && late.startup_held(), "listed after the quit: retired, never released");
    assert!(silent(&late), "its first packet is silence");
    let sources = include_str!("audio/sources.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(sources.contains("if early.synced && !early.quitting && !early.gapped { release_startup_holds();"), "the close never releases after it");
}

/// A stdin that fails mid-read, as a broken pipe would.
struct Broken(bool);

impl std::io::Read for Broken {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if std::mem::replace(&mut self.0, false) {
            let line = b"{\"cmd\":\"bookmark\"}\n";
            out[..line.len()].copy_from_slice(line);
            return Ok(line.len());
        }
        Err(std::io::Error::other("the pipe broke"))
    }
}

/// N2: the reader ends on a read error (an orphan that may keep writing a long save). Its device
/// tracks are held the same way as at a quit, and its status says `stdin_ended`.
#[test]
fn a_read_error_or_the_end_of_stdin_holds_like_a_quit() {
    let _restore = restore();
    let _reopen = Reopen;
    audio::set_mic_muted(false);
    let mic = held_listed("n2-mic");
    mic.release_startup_hold();
    assert!(!silent(&mic), "recording");
    let end = read_early(std::io::BufReader::new(Broken(true)), |_| {});
    assert_eq!(end, crate::stdin_reader::ReaderEnd::Eof, "a read error ends the reader as an end of stdin");
    super::hold_at_end(); // `reader_end` on every end
    assert!(mic.retired() && silent(&mic), "held after a read error");
    assert!(super::stdin_ended(), "the status says stdin_ended");
    let tick = include_str!("app_tick.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(tick.contains("stdin_ended: crate::engine_stdin::stdin_ended(),"));
    let src = include_str!("engine_stdin.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(src.contains("fn reader_end(end: stdin_reader::ReaderEnd, quit: &std::sync::atomic::AtomicBool) { hold_at_end();"), "every end, not only quit");
}

/// F-1: line 13 is dropped by the controller's full queue, line 14 arrives. The ack never
/// passes 12, every device track is held from line 13 on, and only the restatement (with
/// the dropped change in it) and its `sync` release it.
#[test]
fn a_skipped_line_holds_until_the_restatement_and_is_never_acked_past() {
    let _restore = restore();
    let _reopen = Reopen;
    audio::set_mic_muted(false);
    let line = held_listed("f1-line");
    line.set_mic_mute(false);
    close_staging();
    let lines = |s: &str| read_early(std::io::Cursor::new(s.to_owned()), |_| {});
    lines("{\"cmd\":\"restate\",\"seq\":10}\n{\"cmd\":\"set_mic_mute\",\"muted\":false,\"seq\":11}\n{\"cmd\":\"sync\",\"seq\":12}\n");
    assert!(!line.startup_held() && !silent(&line), "recording");
    // 13 (`set_source line mic_mute:true`, Alt+M mutes the line-in) was dropped; 14 arrives.
    lines("{\"cmd\":\"set_source\",\"id\":\"f1-line\",\"gain_db\":1.0,\"seq\":14}\n");
    assert_eq!(audio::sources::audio_seq(), 12, "never acked past the gap");
    assert!(line.startup_held() && silent(&line), "held from the gap on");
    lines("{\"cmd\":\"sync\",\"seq\":15}\n");
    assert!(line.startup_held(), "a sync without the restatement releases nothing");
    // The controller re-sends the whole intent: the dropped change is in it.
    lines("{\"cmd\":\"restate\",\"seq\":20}\n{\"cmd\":\"set_source\",\"id\":\"f1-line\",\"mic_mute\":true,\"seq\":21}\n{\"cmd\":\"sync\",\"seq\":22}\n");
    assert!(!line.startup_held(), "released by the restatement's sync");
    assert!(line.mic_mute_flag(), "the dropped change is applied");
    assert_eq!(audio::sources::audio_seq(), 22);
}

/// F-2: a same-name twin re-pick (and `mic_mute:false` on the same line) while the old twin
/// streams: the next packet of the old stream is silence, not live; after the reopen (the
/// stream loop takes the re-pick) the new device records.
#[test]
fn a_twin_re_pick_silences_the_old_streams_next_packet() {
    let _restore = restore();
    let _reopen = Reopen;
    audio::set_mic_muted(true); // Alt+M is on
    let mic = held_listed("f2-mic");
    close_staging(); // listed: a set_source goes live
    mic.release_startup_hold();
    mic.push_live_read(&[0.5; 960], MuteNow::read);
    assert!(mic.snapshot(0.01).iter().all(|s| *s == 0.0), "muted by Alt+M");
    read_early(std::io::Cursor::new("{\"cmd\":\"set_source\",\"id\":\"f2-mic\",\"device_id\":\"{B}\",\"mic_mute\":false}\n"), |_| {});
    mic.push_live_read(&[0.5; 960], MuteNow::read); // the packet the old twin captured in flight
    assert!(mic.snapshot(0.01).iter().all(|s| *s == 0.0), "the un-picked twin is never stored");
    assert!(mic.take_repick_for_test(), "the stream loop ends that stream at its next turn");
    audio::set_mic_muted(false);
    mic.push_live_read(&[0.5; 960], MuteNow::read);
    assert!(mic.snapshot(0.01).iter().any(|s| *s != 0.0), "the reopened stream records");
}

/// F-3: a source removed during the spawn (the restatement's `retire`, read before the tracks
/// are listed): it gets no track, so an app track (never held at start) records nothing.
#[test]
fn a_retire_read_before_the_listing_starts_no_track() {
    let _restore = restore();
    let _reopen = Reopen;
    read_early(std::io::Cursor::new("{\"cmd\":\"set_source\",\"id\":\"app-1\",\"retire\":true}\n"), |_| {});
    let env = r#"[{"id":"game","kind":"game","name":"Game"},{"id":"app-1","kind":"app","name":"Chat","exe":"chat.exe"}]"#;
    let list = audio::sources::resolve_for_start(|key| (key == "RECALL_AUDIO_SOURCES").then(|| env.to_owned())).unwrap();
    assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["game"], "the removed app opens nothing");
}
