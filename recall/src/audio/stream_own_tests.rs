//! p9 gate item 4: the own-device decision of `pick_device` (`own_decision`), apart from WASAPI.
//! Made-up names and ids.

use super::own_decision;
use crate::audio::rebind::Key;

#[test]
fn the_saved_id_on_the_opened_endpoint_is_own() {
    assert_eq!(own_decision("Mic (Box)", "{a}", "Mic (Box)", || Some("{a}".into())), (Key::Id("{a}".into()), true));
    assert_eq!(own_decision("3- Mic (Box)", "{a}", "4- Mic (Box)", || Some(" {a} ".into())), (Key::Id("{a}".into()), true), "renumbered, same id");
}

#[test]
fn another_id_no_id_or_no_device_is_not_own() {
    assert_eq!(own_decision("Mic (Box)", "{a}", "Mic (Box)", || Some("{b}".into())), (Key::Id("{b}".into()), false), "another endpoint id");
    assert_eq!(own_decision("Mic (Box)", "", "Mic (Box)", || panic!("no saved id: never read")), (Key::Name("Mic (Box)".into()), false));
    assert_eq!(own_decision("Mic (Box)", "{a}", "Mic (Box)", || None), (Key::Name("Mic (Box)".into()), false), "unreadable id");
    assert!(!own_decision("", "{a}", "Mic (Test)", || Some("{a}".into())).1, "the test mic (no saved device) stands in for the default");
}
