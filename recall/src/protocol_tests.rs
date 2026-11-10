//! `protocol.rs` tests (moved out to keep the protocol file under 500 lines).
use super::Outbound;

/// A pipe whose reader is gone (`ERROR_NO_DATA`).
struct ClosedPipe;
impl std::io::Write for ClosedPipe {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from_raw_os_error(232))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_line_to_a_closed_pipe_is_dropped_without_a_panic() {
    // R5-overlay-6: the controller crashed; the engine must not panic on its next line.
    let wrote = std::panic::catch_unwind(|| super::write_line(&mut ClosedPipe, "{\"type\":\"status\"}"));
    assert_eq!(wrote.ok(), Some(false), "dropped, reported, no panic");
    let mut ok = Vec::new();
    assert!(super::write_line(&mut ok, "x"));
    assert_eq!(ok, b"x
");
}

#[test]
fn set_source_parses_with_either_field() {
    use super::Command;
    let both: Command = serde_json::from_str(r#"{"cmd":"set_source","id":"mic","gain_db":-3.5,"name":"Voice"}"#).unwrap();
    assert!(matches!(both, Command::SetSource { ref id, gain_db: Some(g), name: Some(ref n), mic_mute: None, device_id: None, .. } if id == "mic" && g == -3.5 && n == "Voice"));
    let gain: Command = serde_json::from_str(r#"{"cmd":"set_source","id":"game","gain_db":6}"#).unwrap();
    assert!(matches!(gain, Command::SetSource { gain_db: Some(_), name: None, .. }));
    let line_in: Command = serde_json::from_str(r#"{"cmd":"set_source","id":"in-1","mic_mute":false}"#).unwrap();
    assert!(matches!(line_in, Command::SetSource { mic_mute: Some(false), gain_db: None, name: None, .. }));
}

#[test]
fn record_fail_parses_with_its_reason() {
    // The controller ends a running long recording early before restarting a hung engine.
    let v = serde_json::from_str::<super::Command>(r#"{"cmd":"record_fail","reason":"the encoder isn't responding"}"#);
    assert!(matches!(v, Ok(super::Command::RecordFail { ref reason }) if reason == "the encoder isn't responding"), "not a record_fail");
}

#[test]
fn session_locked_parses() {
    // The controller says when the Windows session locks / unlocks (audit U8).
    let v = serde_json::from_str::<super::Command>(r#"{"cmd":"session_locked","locked":true}"#);
    assert!(matches!(v, Ok(super::Command::SessionLocked { locked: true })), "not a session_locked");
}

fn wire(msg: &Outbound) -> String {
    serde_json::to_string(msg).unwrap()
}

#[test]
fn saved_keeps_its_fields_and_adds_staging() {
    let staged = Outbound::Saved {
        path: r"E:\Clips\G__x.mp4".into(),
        seconds: 120.0,
        frames: 7200,
        bytes: 400,
        elapsed_ms: 812,
        staged: true,
        staged_path: Some(r"F:\Recall\cache\staging\G__x.mp4".into()),
        video_end_us: Some(7),
        partial: None,
    };
    assert_eq!(
        wire(&staged),
        r#"{"type":"saved","path":"E:\\Clips\\G__x.mp4","seconds":120.0,"frames":7200,"bytes":400,"elapsed_ms":812,"staged":true,"staged_path":"F:\\Recall\\cache\\staging\\G__x.mp4"}"#
    );
    let direct = Outbound::Saved {
        path: "a.mp4".into(),
        seconds: 1.5,
        frames: 3,
        bytes: 10,
        elapsed_ms: 5,
        staged: false,
        staged_path: None,
        video_end_us: None,
        partial: Some("stalled".into()),
    };
    assert_eq!(
        wire(&direct),
        r#"{"type":"saved","path":"a.mp4","seconds":1.5,"frames":3,"bytes":10,"elapsed_ms":5,"staged":false,"partial":"stalled"}"#
    );
}

#[test]
fn clip_move_events_match_the_wire_shape() {
    let moved = Outbound::ClipMoved { path: r"E:\c\G.mp4".into(), elapsed_ms: 9549, kept_path: None };
    assert_eq!(wire(&moved), r#"{"type":"clip_moved","path":"E:\\c\\G.mp4","elapsed_ms":9549}"#);
    let kept = Outbound::ClipMoved { path: r"E:\c\G.mp4".into(), elapsed_ms: 9549, kept_path: Some(r"F:\s\kept\G.mp4".into()) };
    assert_eq!(wire(&kept), r#"{"type":"clip_moved","path":"E:\\c\\G.mp4","elapsed_ms":9549,"kept_path":"F:\\s\\kept\\G.mp4"}"#);
    let failed = Outbound::ClipMoveFailed {
        path: r"E:\c\G.mp4".into(),
        staged_path: r"F:\s\G.mp4".into(),
        reason: "disk full".into(),
    };
    assert_eq!(
        wire(&failed),
        r#"{"type":"clip_move_failed","path":"E:\\c\\G.mp4","staged_path":"F:\\s\\G.mp4","reason":"disk full"}"#
    );
}
