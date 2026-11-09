//! design-audio-mute P10 (B1, B2, B3): an Audio-page change sends its privacy lines to the engine
//! first, whatever restart follows, a spawn before its hello included. A recorder stands in for
//! the engine's stdin; made-up device names.

use serde_json::json;

use super::*;
use crate::record_audio_sources::engine_value;
use crate::supervisor::Supervisor;

fn src(id: &str, kind: &str, device: &str, mic_mute: Option<bool>) -> AudioSource {
    AudioSource { id: id.into(), kind: kind.into(), name: id.into(), device: device.into(), mic_mute, ..AudioSource::default() }
}

/// B2: during a long recording (the restart is deferred) a patch removes an output AND drops a
/// line-in's flag (muted). Both go live at once; only the shape waits for the restart.
#[test]
fn a_privacy_change_during_a_long_recording_goes_live_and_defers_only_the_shape() {
    let old = vec![src("line", "input", "Line (Box)", Some(false)), src("pod", "output", "Speakers (Pod)", None)];
    let new = vec![src("line", "input", "Line (Box)", None)];
    let change = plan(&old, &new, crate::record_audio_sources::changed(&old, &new), (true, false), |_| false);
    assert!(change.restart, "the shape restart follows (and may wait)");
    assert_eq!(change.spawn_value.as_deref(), Some(engine_value(&new).as_str()));
    assert!(change.lines.contains(&json!({"cmd": "set_source", "id": "line", "mic_mute": true})), "{:?}", change.lines);
    assert!(change.lines.contains(&json!({"cmd": "set_source", "id": "pod", "retire": true})), "{:?}", change.lines);
    // The live path: queued to the engine before its hello (B1), not left to the restart.
    let (sup, rec) = Supervisor::recorded_for_test();
    assert!(sup.update_audio(change.spawn_value.as_deref(), &change.lines).queued());
    let lines = rec.finish(&sup);
    assert!(lines.iter().any(|l| l.contains(r#""retire":true"#)) && lines.iter().any(|l| l.contains(r#""mic_mute":true"#)), "{lines:?}");
    // apply_config_effects sends them before it decides the restart.
    let effects: String = include_str!("record_cmds.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    let send = effects.find("let sent = self.supervisor.update_audio(change.spawn_value.as_deref(), &change.lines);").expect("the live path sends them");
    let restart = effects.find("if change.restart { self.request_engine_restart();").expect("then the restart");
    assert!(send < restart, "lines first");
}

/// B3 (other-name re-pick, P16): the old device is retired live; the new one opens at the restart.
#[test]
fn a_re_pick_under_another_name_retires_the_old_device_at_once() {
    let old = vec![src("mic", "input", "Mic (Desk)", None)];
    let new = vec![src("mic", "input", "Mic (Other)", None)];
    let change = plan(&old, &new, crate::record_audio_sources::changed(&old, &new), (true, false), |_| false);
    assert_eq!(change.lines, vec![json!({"cmd": "set_source", "id": "mic", "retire": true})]);
    assert!(change.restart);
}

/// A gain on a sync-hold engine never restarts it; below the floor it does (it ignores set_source).
#[test]
fn only_an_engine_below_the_floor_restarts_for_a_live_field() {
    let old = vec![src("out", "output", "", None)];
    let new = vec![AudioSource { gain_db: 3.0, ..src("out", "output", "", None) }];
    assert!(!plan(&old, &new, false, (true, false), |_| true).restart, "at the floor: live");
    assert!(plan(&old, &new, false, (true, true), |_| true).restart, "below it: a restart");
}
