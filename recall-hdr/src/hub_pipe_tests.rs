//! Recall Hub (`recall-hub/src-tauri`, its own Tauri workspace) reaches this session's control
//! pipe (deep-startup-p3 b). The hub can't be built here (Tauri needs its frontend built), so
//! its name helper is compiled into these tests and its use is checked in the source.

#[path = "../../recall-hub/src-tauri/src/pipe_name.rs"]
mod hub_pipe_name;

const HUB_LIB: &str = include_str!("../../recall-hub/src-tauri/src/lib.rs");

#[test]
fn the_hub_uses_the_per_session_pipe_name() {
    assert!(!HUB_LIB.contains("\"recall-hdr-control\""), "the hub still names the bare machine-wide pipe");
    assert!(HUB_LIB.contains("mod pipe_name;"));
    let call = HUB_LIB.find("let pipe = pipe_name::current_pipe_name();").expect("the client loop takes the session's name");
    let connect = HUB_LIB.find("pipe.as_str().to_ns_name::<GenericNamespaced>()").expect("and connects to it");
    assert!(call < connect);
}

#[test]
fn the_hub_builds_the_same_name_as_the_controller() {
    for session in [None, Some(0), Some(1), Some(7), Some(u32::MAX)] {
        assert_eq!(hub_pipe_name::pipe_name(session), crate::control_pipe::pipe_name(session), "{session:?}");
    }
    assert_eq!(hub_pipe_name::current_pipe_name(), crate::control_pipe::current_pipe_name());
}
