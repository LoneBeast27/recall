//! Non-UI services: controller pipe client, audio meters, MIDI, clip library index.
pub mod app_icons;
pub mod audio_apps;
pub mod audio_devices;
pub mod audio_mute;
pub mod audio_pick;
pub mod audio_sources;
pub mod endpoint_choice;
pub mod endpoint_form;
pub mod daemon;
pub mod durable;
pub mod json_keep;
pub mod lenient_json;
pub mod ordered_write;
pub mod editor_prefs;
pub mod game_apps;
// Dev builds only (the user, 2026-11-21): a release build neither has nor writes the counts.
#[cfg(not(recall_release))]
pub mod hover_count;
pub mod game_apps_win;
pub mod library;
pub mod meters;
pub mod process_tree;
pub mod midi;
pub mod pre_encode;
pub mod pre_encode_room;
mod pre_encode_copy;
pub mod storage;
pub mod endpoint_events;
pub mod os_events;
pub mod ui_wake;
pub mod win_event;

