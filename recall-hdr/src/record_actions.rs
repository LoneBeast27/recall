//! The record mode's user actions on the pump thread: hotkeys (direct combos
//! and the advanced chord), the capture actions they and the pipe trigger
//! (bookmark, screenshot, long recording), pause / resume, engine-event
//! bookkeeping, the engine plan with deferred restarts / retargets, and the
//! storage schedule.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::hotkey::Pressed;
use crate::log;
use crate::record_activity::{LongRec, PendingPause, Trigger, PAUSE_STOP_WAIT};
use crate::record_app::Controller;
use crate::record_chord::{self, ChordKey};
use crate::record_engine::{capture_refusal, EngineTarget, Step};
use crate::record_hotkeys::{bindings, HotkeyAction};
use crate::record_status::error_event;

/// Minimum gap between two runs of the same action (key repeat / bounce).
fn debounce_of(action: HotkeyAction) -> (&'static str, Duration) {
    match action {
        HotkeyAction::Save { .. } => ("save", Duration::from_secs(2)),
        HotkeyAction::Mic => ("mic", Duration::from_millis(250)),
        HotkeyAction::Bookmark => ("bookmark", Duration::from_millis(300)),
        HotkeyAction::Screenshot => ("screenshot", Duration::from_millis(500)),
        HotkeyAction::Record => ("record", Duration::from_secs(1)),
        HotkeyAction::ToggleHdr => ("hdr", Duration::from_millis(250)),
        HotkeyAction::Leader => ("leader", Duration::ZERO),
    }
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl Controller {
    /// (Re)register every hotkey from config; conflicts and OS refusals are
    /// `session` error events, never fatal. Re-arms the chord for the leader.
    pub(crate) fn rebind_hotkeys(&mut self) {
        let hdr_key = self.hdr.is_active().then(|| self.cfg.hotkey.toggle_hdr.clone());
        let list = bindings(&self.cfg.recording, &self.cfg.hotkeys, hdr_key.as_deref());
        let window = Duration::from_millis(self.cfg.hotkeys.window_ms as u64);
        if self.ensure_hotkeys().is_none() {
            return;
        }
        let Some(hk) = self.hotkeys.as_mut() else { return };
        self.chord.close(hk);
        let warnings = hk.bind(list);
        let leader = hk.bound_hotkey(HotkeyAction::Leader);
        let bound = hk.bound();
        if let Some(leader) = leader {
            self.chord.configure(window, leader.mods, hk);
        }
        self.leader_label = leader.map(|l| record_chord::leader_label(&l, key_char(&l))).unwrap_or_default();
        for warning in warnings {
            log!("record: hotkey unavailable: {warning}");
            self.hub.event("session", &error_event(&warning));
        }
        log!("record: hotkeys bound {bound:?}; advanced shortcuts via {:?}", self.leader_label);
    }

    pub(crate) fn on_hotkeys(&mut self, fired: Vec<Pressed>) {
        let now = Instant::now();
        for pressed in fired {
            match pressed {
                Pressed::Action(action) => self.on_action(action, now),
                Pressed::Other(id) => {
                    let Some(hk) = self.hotkeys.as_mut() else { continue };
                    if let Some(key) = self.chord.press(id, hk) {
                        self.publish_chord(false);
                        self.on_chord_key(key);
                    }
                }
            }
        }
    }

    fn on_action(&mut self, action: HotkeyAction, now: Instant) {
        let (key, gap) = debounce_of(action);
        if self.last_fired.get(key).is_some_and(|t| now.duration_since(*t) < gap) {
            return;
        }
        self.last_fired.insert(key, now);
        if self.paused && action != HotkeyAction::Leader {
            // Resume is the tray / app; the chord still opens (5 toggles the
            // stats island, its capture keys get the paused refusal).
            if let HotkeyAction::Save { .. } = action {
                self.refuse_capture("save_clip", crate::record_engine::PAUSED_REASON);
            } else {
                log!("record: {key} hotkey ignored -- recording is paused");
            }
            return;
        }
        match action {
            HotkeyAction::Save { seconds } => {
                log!("record: save hotkey ({})", crate::record_hotkeys::save_label(seconds));
                self.save_clip(Some(self.cfg.recording.save_seconds(seconds)));
            }
            HotkeyAction::Mic => self.set_mic(!self.mic_muted),
            HotkeyAction::Bookmark | HotkeyAction::Screenshot | HotkeyAction::Record => {
                let cmd = match action {
                    HotkeyAction::Bookmark => "bookmark",
                    HotkeyAction::Screenshot => "screenshot",
                    _ => "record_toggle",
                };
                if self.engine_running() {
                    crate::cues::tick();
                }
                self.engine_command(cmd, None);
            }
            HotkeyAction::ToggleHdr => {
                if let Err(e) = self.hdr.toggle() {
                    log!("record: HDR hotkey ignored: {e}");
                }
            }
            HotkeyAction::Leader => self.open_chord(now),
        }
    }

    fn open_chord(&mut self, now: Instant) {
        if !self.cfg.hotkeys.advanced_enabled {
            return;
        }
        let held = record_chord::win::keys_down();
        let Some(hk) = self.hotkeys.as_mut() else { return };
        for warning in self.chord.open(now, held, hk) {
            log!("record: {warning}");
        }
        self.publish_chord(true);
    }

    fn on_chord_key(&mut self, key: ChordKey) {
        log!("record: advanced shortcut {key:?}");
        match key {
            ChordKey::Bookmark => self.engine_command("bookmark", None),
            ChordKey::SaveShort => self.save_clip(Some(self.cfg.recording.save_seconds(self.cfg.hotkeys.save_seconds))),
            ChordKey::Screenshot => self.engine_command("screenshot", None),
            ChordKey::Record => self.engine_command("record_toggle", None),
            ChordKey::Stats => self.toggle_stats(None),
            ChordKey::Cancel => {}
        }
    }

    /// Timeout / stray key closes the chord window (and tells the overlay).
    pub(crate) fn chord_tick(&mut self, now: Instant) {
        if !self.chord.is_open() {
            return;
        }
        let down = record_chord::win::keys_down();
        let Some(hk) = self.hotkeys.as_mut() else { return };
        if self.chord.tick(now, &down, hk) {
            self.publish_chord(false);
        }
    }

    fn publish_chord(&self, active: bool) {
        let save = self.cfg.recording.save_seconds(self.cfg.hotkeys.save_seconds);
        let options = record_chord::options(save, self.activity.is_recording());
        self.hub.event("session", &record_chord::event_json(active, &options, &self.leader_label));
    }

    /// Forward `bookmark` / `screenshot` / `record_start|stop|toggle` to the
    /// engine, or report why it cannot run (paused, no game, starting).
    pub(crate) fn engine_command(&mut self, cmd: &'static str, client: Option<u64>) {
        if let Some(reason) = capture_refusal(self.paused, self.engine_running(), &self.wanted()) {
            if cmd == "record_stop" || cmd == "bookmark" {
                return self.reply_error(client, &format!("{cmd}: {reason}"));
            }
            return self.refuse_capture(cmd, reason);
        }
        let starts = cmd == "record_start" || (cmd == "record_toggle" && !self.activity.is_recording());
        if starts {
            self.activity.record_requested(Instant::now());
        }
        log!("record: engine {cmd}");
        self.supervisor.send_engine_line(&json!({ "cmd": cmd }).to_string());
        self.publish_status();
    }

    /// The failure event + beep a refused capture action gets (as if the
    /// engine had failed it), so the overlay and the app show the reason.
    pub(crate) fn refuse_capture(&self, cmd: &str, reason: &str) {
        log!("record: {cmd} refused -- {reason}");
        let event = match cmd {
            "screenshot" => json!({"type": "screenshot_failed", "reason": reason}),
            "save_clip" => json!({"type": "save_failed", "reason": reason}),
            _ => json!({"type": "recording_failed", "reason": reason, "path": null}),
        };
        self.hub.event("engine", &event.to_string());
        crate::supervisor::beep_failure();
    }

    /// Pause stops the engine now (no linger) and ignores games until resume;
    /// resume re-reads the foreground so the current game starts at once. A
    /// long recording is stopped (and saved) first: the pause applies when the
    /// engine reports it ended, or after [`PAUSE_STOP_WAIT`]. Resume cancels a
    /// pause still waiting.
    pub(crate) fn set_paused(&mut self, paused: bool) {
        if !paused {
            self.pending_pause = None;
        } else if !self.paused && self.engine_running() {
            if self.pending_pause.is_some() {
                return; // already stopping the recording
            }
            if let Some(wait) = PendingPause::needed(&self.activity, Instant::now()) {
                log!("record: pause -- stopping the long recording first (up to {} s)", PAUSE_STOP_WAIT.as_secs());
                self.supervisor.send_engine_line(r#"{"cmd":"record_stop"}"#);
                self.pending_pause = Some(wait);
                return;
            }
        }
        self.apply_pause(paused);
    }

    fn apply_pause(&mut self, paused: bool) {
        self.pending_pause = None;
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        if let Some(hk) = self.hotkeys.as_mut() {
            if self.chord.is_open() {
                self.chord.close(hk);
                self.publish_chord(false);
            }
        }
        let now = Instant::now();
        if paused {
            log!("record: PAUSED -- engine stopped, games ignored until resume");
            if self.planner.pause() {
                self.apply_engine_target(&EngineTarget::Off);
            }
        } else {
            log!("record: resumed");
            self.last_fed = None;
            self.probe_foreground(now, false);
        }
        self.tray.set_paused(paused);
        self.publish_status();
    }

    /// Engine events forwarded by the supervisor.
    pub(crate) fn drain_engine_events(&mut self, now: Instant) {
        while let Ok(ev) = self.engine_events.try_recv() {
            match self.activity.on_event(&ev, unix_now()) {
                Some(Trigger::Saved) => self.janitor.saved(now),
                Some(Trigger::LongRecChanged) | None => {}
            }
        }
    }

    /// Engine settings changed: restart now, or once no save / long
    /// recording is in flight (`pending_restart` in the status meanwhile).
    pub(crate) fn request_engine_restart(&mut self) {
        if *self.planner.applied() == EngineTarget::Off {
            self.pending_restart = false; // the next start uses the new settings
            return;
        }
        if self.activity.busy(Instant::now()) {
            if !self.pending_restart {
                log!("record: engine settings changed; restart deferred until the save / long recording ends");
            }
            self.pending_restart = true;
            return;
        }
        log!("record: engine settings changed; restarting the engine (the replay buffer restarts)");
        self.restart_engine_for_settings();
    }

    /// Planner -> engine, every pump tick. A retarget that falls due while a
    /// save or the long recording is in flight waits (status
    /// `pending_restart`) and applies on the first idle tick.
    pub(crate) fn plan_engine(&mut self, now: Instant) {
        let buffered = self.supervisor.engine_view().buffer_secs;
        self.planner.set_drain_len(crate::record_drain::drain_len(buffered, self.cfg.recording.buffer_secs));
        let wanted = self.wanted();
        let busy = self.activity.busy(now);
        match self.planner.step(&wanted, now, busy) {
            Step::Apply(target) => {
                self.retarget_held = false;
                self.apply_engine_target(&target);
            }
            Step::Held => {
                if !self.retarget_held {
                    log!("record: engine retarget deferred until the save / long recording ends");
                }
                self.retarget_held = true;
            }
            Step::Keep => self.retarget_held = false,
        }
    }

    /// Pending pause + deferred restart + storage schedule, every pump tick.
    pub(crate) fn activity_tick(&mut self, now: Instant) {
        if self.pending_pause.is_some_and(|p| p.ready(&self.activity, now)) {
            if self.activity.long != LongRec::Idle {
                log!("record: the long recording did not report stopping; pausing anyway");
            }
            self.apply_pause(true);
        }
        if self.pending_restart && !self.activity.busy(now) {
            self.request_engine_restart();
        }
        let (dir, cap) = (self.cfg.recording.output_dir.clone(), self.cfg.storage.cap_gb);
        self.janitor.tick(&dir, cap, now);
    }
}

/// The leader key's character on this layout (for "Alt `").
fn key_char(hk: &global_hotkey::hotkey::HotKey) -> Option<char> {
    record_chord::vk_for_code(hk.key).and_then(record_chord::win::vk_char)
}
