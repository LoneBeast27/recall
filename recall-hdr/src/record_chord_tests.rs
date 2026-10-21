use super::*;
use std::collections::HashSet;

/// Records every (un)registration; `refuse` ids fail to register.
#[derive(Default)]
struct FakeRegistrar {
    live: HashSet<u32>,
    log: Vec<(bool, u32)>,
    refuse: HashSet<u32>,
}

impl Registrar for FakeRegistrar {
    fn register(&mut self, hk: HotKey) -> Result<(), String> {
        if self.refuse.contains(&hk.id()) {
            return Err(format!("{hk:?} is taken"));
        }
        assert!(self.live.insert(hk.id()), "registered twice: {hk:?}");
        self.log.push((true, hk.id()));
        Ok(())
    }
    fn unregister(&mut self, hk: HotKey) {
        assert!(self.live.remove(&hk.id()), "unregistered a key that was not live: {hk:?}");
        self.log.push((false, hk.id()));
    }
}

const ALT: u16 = 0x12;
const BACKQUOTE: u16 = 0xC0;
const WINDOW: Duration = Duration::from_millis(1_500);

fn id(mods: Option<Modifiers>, code: Code) -> u32 {
    HotKey::new(mods, code).id()
}

fn chord() -> Chord {
    Chord::new(WINDOW, Modifiers::ALT)
}

#[test]
fn leader_then_key_fires_once_and_releases_every_temporary_key() {
    let mut reg = FakeRegistrar::default();
    let mut c = chord();
    let t0 = Instant::now();
    assert!(c.open(t0, vec![ALT, BACKQUOTE], &mut reg).is_empty());
    assert!(c.is_open());
    assert_eq!(reg.live.len(), 9, "1-4 bare + Alt+1-4 + Esc");
    assert!(reg.live.contains(&id(None, Code::Digit1)) && reg.live.contains(&id(Some(Modifiers::ALT), Code::Digit4)));
    assert!(!reg.live.contains(&id(Some(Modifiers::ALT), Code::Escape)), "Alt+Esc stays Windows'");
    assert_eq!(c.deadline(), Some(t0 + WINDOW));
    assert_eq!(c.press(id(None, Code::Digit1), &mut reg), Some(ChordKey::Bookmark));
    assert!(!c.is_open() && reg.live.is_empty(), "closed and released");
    assert_eq!(c.press(id(None, Code::Digit1), &mut reg), None, "a digit after close is not ours");
    assert_eq!(reg.log.iter().filter(|(on, _)| *on).count(), 9);
    assert_eq!(reg.log.iter().filter(|(on, _)| !*on).count(), 9);
}

#[test]
fn every_follow_up_key_maps_to_its_action_with_or_without_alt_held() {
    let t0 = Instant::now();
    for (mods, code, want) in [
        (None, Code::Digit2, ChordKey::SaveShort),
        (Some(Modifiers::ALT), Code::Digit3, ChordKey::Screenshot),
        (None, Code::Digit4, ChordKey::Record),
        (Some(Modifiers::ALT), Code::Digit1, ChordKey::Bookmark),
        (None, Code::Escape, ChordKey::Cancel),
    ] {
        let mut reg = FakeRegistrar::default();
        let mut c = chord();
        c.open(t0, vec![], &mut reg);
        assert_eq!(c.press(id(mods, code), &mut reg), Some(want));
        assert!(reg.live.is_empty());
    }
}

#[test]
fn timeout_closes_and_unregisters() {
    let mut reg = FakeRegistrar::default();
    let mut c = chord();
    let t0 = Instant::now();
    c.open(t0, vec![ALT, BACKQUOTE], &mut reg);
    assert!(!c.tick(t0 + WINDOW - Duration::from_millis(1), &[ALT, BACKQUOTE], &mut reg));
    assert!(c.tick(t0 + WINDOW, &[], &mut reg), "1.5 s later it closes");
    assert!(reg.live.is_empty() && !c.is_open());
    assert!(!c.tick(t0 + WINDOW * 2, &[], &mut reg), "a closed chord stays closed");
}

#[test]
fn any_other_new_key_closes_but_modifiers_mouse_and_held_keys_do_not() {
    let mut reg = FakeRegistrar::default();
    let mut c = chord();
    let t0 = Instant::now();
    c.open(t0, vec![ALT, BACKQUOTE], &mut reg);
    let soon = t0 + Duration::from_millis(100);
    assert!(!c.tick(soon, &[ALT, BACKQUOTE], &mut reg), "the leader is still held");
    assert!(!c.tick(soon, &[0x01, 0xA0, 0x11], &mut reg), "mouse button, shift, ctrl");
    assert!(!c.tick(soon, &[0x31], &mut reg), "our own digit arrives as a hotkey");
    assert!(c.tick(soon, &[0x57], &mut reg), "W: an unknown key just closes the window");
    assert!(reg.live.is_empty());
    // A released leader key pressed again is a new press.
    c.open(t0, vec![ALT, BACKQUOTE], &mut reg);
    assert!(!c.tick(soon, &[ALT], &mut reg));
    assert!(c.tick(soon, &[ALT, BACKQUOTE], &mut reg));
}

#[test]
fn reopening_extends_the_window_without_registering_twice() {
    let mut reg = FakeRegistrar::default();
    let mut c = chord();
    let t0 = Instant::now();
    c.open(t0, vec![], &mut reg);
    c.open(t0 + Duration::from_secs(1), vec![], &mut reg); // FakeRegistrar panics on a double register
    assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(1) + WINDOW));
    assert_eq!(reg.live.len(), 9);
    c.configure(Duration::from_secs(2), Modifiers::empty(), &mut reg);
    assert!(!c.is_open() && reg.live.is_empty(), "reconfigure closes");
    c.open(t0, vec![], &mut reg);
    assert_eq!(reg.live.len(), 5, "a modifier-less leader needs only the bare keys");
}

#[test]
fn a_taken_bare_key_is_reported_and_skipped() {
    let mut reg = FakeRegistrar::default();
    reg.refuse.insert(id(None, Code::Digit3));
    reg.refuse.insert(id(Some(Modifiers::ALT), Code::Digit2));
    let mut c = chord();
    let warnings = c.open(Instant::now(), vec![], &mut reg);
    assert_eq!(warnings.len(), 1, "only the bare key matters: {warnings:?}");
    assert_eq!(reg.live.len(), 7);
    assert_eq!(c.press(id(None, Code::Digit3), &mut reg), None, "not registered, not ours");
    assert!(c.is_open());
}

#[test]
fn leader_follows_the_physical_key_left_of_one() {
    let leader = crate::hotkey::parse_accel("Alt+Backquote").unwrap();
    let alt = |code| HotKey::new(Some(Modifiers::ALT), code);
    assert_eq!(resolve_physical(leader, Some(0xC0)), (alt(Code::Backquote), None), "US");
    assert_eq!(resolve_physical(leader, Some(0xDC)), (alt(Code::Backslash), None), "German ^");
    assert_eq!(resolve_physical(leader, Some(0xDE)), (alt(Code::Quote), None), "French ²");
    let (uk, warning) = resolve_physical(leader, Some(0xDF));
    assert_eq!(uk, leader, "UK VK_OEM_8 has no code: falls back");
    assert!(warning.unwrap().contains("hotkeys.leader"));
    let other = crate::hotkey::parse_accel("Ctrl+F7").unwrap();
    assert_eq!(resolve_physical(other, Some(0xDC)), (other, None), "only Backquote is physical");
    assert_eq!(leader_label(&alt(Code::Backquote), Some('`')), "Alt `");
    assert_eq!(leader_label(&alt(Code::Backslash), Some('^')), "Alt ^");
    assert_eq!(leader_label(&other, None), "Ctrl F7");
    for vk in [0xC0u16, 0xBA, 0xDC, 0xDE] {
        assert_eq!(vk_for_code(code_for_vk(vk).unwrap()), Some(vk), "round trip {vk:#x}");
    }
    assert_eq!(vk_for_code(Code::KeyA), None, "letters read fine as their name");
}

#[test]
fn chord_event_and_hint_shapes() {
    let opts = options(30, false);
    let v: serde_json::Value = serde_json::from_str(&event_json(true, &opts, "Alt `")).unwrap();
    assert_eq!(
        v,
        json!({"type":"chord","active":true,"leader":"Alt `","options":[
            {"key":"1","label":"Bookmark"},{"key":"2","label":"Save 30 s"},
            {"key":"3","label":"Screenshot"},{"key":"4","label":"Record"}]})
    );
    assert_eq!(options(45, true)[3].1, "Stop recording");
    assert_eq!(options(45, true)[1].1, "Save 45 s");
    let off: serde_json::Value = serde_json::from_str(&event_json(false, &opts, "")).unwrap();
    assert_eq!(off, json!({"type":"chord","active":false}));
}
