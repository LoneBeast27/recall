//! `overlay.hidden` (set by the overlay shortcut) never outlives an explicit enable.

use crate::config::Config;
use crate::record_config::apply_patch;
use serde_json::json;

fn hidden_config() -> Config {
    let mut cfg = Config::default();
    cfg.overlay.enabled = true;
    cfg.overlay.hidden = true;
    cfg
}

#[test]
fn turning_notices_on_in_settings_clears_the_shortcut_hide() {
    let mut cfg = hidden_config();
    cfg.overlay.enabled = false;
    let out = apply_patch(&cfg, &json!({"overlay": {"enabled": true}})).unwrap();
    assert!(!out.config.overlay.hidden, "Show notices On must show them");
    assert!(out.config.overlay.shows_notices());
}

#[test]
fn a_notices_on_patch_unhides_even_when_already_on() {
    let out = apply_patch(&hidden_config(), &json!({"overlay": {"enabled": true}})).unwrap();
    assert!(!out.config.overlay.hidden);
    assert_eq!(out.changed, ["overlay.hidden"]);
}

#[test]
fn turning_stats_on_clears_the_shortcut_hide() {
    let out = apply_patch(&hidden_config(), &crate::record_cmds::stats_patch(true)).unwrap();
    assert!(!out.config.overlay.hidden, "Alt+P on must show the stats island");
    assert!(out.config.overlay.shows_stats());
}

#[test]
fn turning_a_surface_off_or_moving_the_corner_keeps_the_hide() {
    let cfg = hidden_config();
    for patch in [json!({"overlay": {"enabled": false}}), crate::record_cmds::stats_patch(false), json!({"overlay": {"corner": "top-left"}})] {
        let out = apply_patch(&cfg, &patch).unwrap();
        assert!(out.config.overlay.hidden, "{patch} is no explicit enable");
    }
}

#[test]
fn an_explicit_hidden_in_the_same_patch_wins() {
    let mut cfg = Config::default();
    cfg.overlay.enabled = true;
    let out = apply_patch(&cfg, &json!({"overlay": {"enabled": true, "hidden": true}})).unwrap();
    assert!(out.config.overlay.hidden);
}

#[test]
fn hiding_pops_up_how_to_show_it_again_even_with_notices_hidden() {
    let v: serde_json::Value = serde_json::from_str(&crate::record_actions::overlay_hidden_notice("Alt+O")).unwrap();
    assert_eq!((v["title"].as_str(), v["detail"].as_str()), (Some("Overlay hidden"), Some("Alt+O to show")));
    assert_eq!(v["always"], true, "notices are hidden now: only a forced pop-up shows");
    let unbound: serde_json::Value = serde_json::from_str(&crate::record_actions::overlay_hidden_notice("")).unwrap();
    assert_eq!(unbound["detail"], "Settings shows it again");
}
