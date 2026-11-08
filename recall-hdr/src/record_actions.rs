//! The record mode's user actions on the pump thread: hotkeys (one direct
//! combo per action), the capture actions they and the pipe trigger
//! (bookmark, screenshot, long recording), pause / resume, engine-event
//! bookkeeping, the engine plan with deferred restarts / retargets, and the
//! storage schedule.

use crate::record_dispatch::{Effect, Sent};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::log;
use crate::record_activity::{LongRec, PendingPause, Trigger, PAUSE_STOP_WAIT};
use crate::record_app::Controller;
use crate::record_engine::{capture_refusal, EngineTarget, Step};
use crate::record_hotkeys::{bindings, hotkey_command, HotkeyAction};
use crate::record_notice::shortcut_notice_json;
use crate::record_status::error_event;
use crate::record_transition::Transitions;

/// Minimum gap between two runs of the same action (key repeat / bounce).
fn debounce_of(action: HotkeyAction) -> (&'static str, Duration) {
    match action {
        HotkeyAction::Save { .. } => ("save", Duration::from_secs(2)),
        HotkeyAction::Mic => ("mic", Duration::from_millis(250)),
        HotkeyAction::Bookmark => ("bookmark", Duration::from_millis(300)),
        HotkeyAction::Screenshot => ("screenshot", Duration::from_millis(500)),
        HotkeyAction::Record => ("record", Duration::from_secs(1)),
        HotkeyAction::Stats => ("stats", Duration::from_millis(250)),
        HotkeyAction::Overlay => ("overlay", Duration::from_millis(250)),
        HotkeyAction::Editor => ("editor", Duration::from_millis(300)),
        HotkeyAction::ToggleHdr => ("hdr", Duration::from_millis(250)),
    }
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl Controller {
    /// (Re)register every hotkey from config; conflicts and OS refusals are
    /// `session` error events, never fatal.
    pub(crate) fn rebind_hotkeys(&mut self) {
        let hdr_key = self.hdr.is_active().then(|| self.cfg.hotkey.toggle_hdr.clone());
        let list = bindings(&self.cfg.recording, hdr_key.as_deref());
        if self.ensure_hotkeys().is_none() {
            return;
        }
        let Some(hk) = self.hotkeys.as_mut() else { return };
        let warnings = hk.bind(list);
        let bound = hk.bound();
        for warning in &warnings {
            log!("record: hotkey unavailable: {warning}");
            self.hub.event("session", &error_event(warning));
        }
        // Kept in the status too: an app that connects later still sees them.
        self.hotkey_problems = warnings;
        log!("record: hotkeys bound {bound:?}");
    }

    /// A shortcut another app held at bind time is tried again every 10 s (`record_hotkey_retry`).
    pub(crate) fn hotkey_retry_tick(&mut self, now: Instant) {
        let Some(hk) = self.hotkeys.as_mut() else { return };
        let Some(won) = hk.retry_refused(now) else { return };
        if !won.is_empty() {
            log!("record: hotkeys registered on retry: {}", won.join(", "));
            self.hotkey_problems = hk.problems();
        }
    }

    pub(crate) fn on_hotkeys(&mut self, fired: Vec<HotkeyAction>) {
        let now = Instant::now();
        for action in fired {
            self.on_action(action, now);
        }
    }

    fn on_action(&mut self, action: HotkeyAction, now: Instant) {
        let (key, gap) = debounce_of(action);
        if self.last_fired.get(key).is_some_and(|t| now.duration_since(*t) < gap) {
            return;
        }
        self.last_fired.insert(key, now);
        if self.paused {
            // Resume is the tray / app. Capture shortcuts say why they did
            // nothing; mic, stats and overlay still toggle.
            let paused = crate::record_engine::PAUSED_REASON;
            match action {
                HotkeyAction::Save { .. } => return self.refuse_capture("save_clip", paused),
                HotkeyAction::Screenshot => return self.refuse_capture("screenshot", paused),
                HotkeyAction::Record => return self.refuse_capture("record_toggle", paused),
                HotkeyAction::Bookmark => return self.refuse_bookmark(paused),
                _ => {}
            }
        }
        match action {
            HotkeyAction::Save { seconds } => {
                log!("record: save hotkey ({})", crate::record_hotkeys::save_label(seconds));
                self.save_clip(Some(self.cfg.recording.save_seconds(seconds)));
            }
            HotkeyAction::Mic => self.set_mic(!self.mic_muted),
            HotkeyAction::Bookmark | HotkeyAction::Screenshot | HotkeyAction::Record => {
                // Queued first: the tick only for a command the engine got (as a save).
                if let Some((cmd, hotkey)) = hotkey_command(action) {
                    self.engine_command_as(cmd, None, hotkey);
                }
            }
            HotkeyAction::Stats => {
                self.toggle_stats(None);
                let (title, detail) = if self.cfg.overlay.stats {
                    ("Performance stats on", "Shows over your game")
                } else {
                    ("Performance stats off", "")
                };
                self.shortcut_notice(title, detail);
            }
            HotkeyAction::Overlay => self.toggle_overlay(),
            HotkeyAction::Editor => self.island_editor(),
            HotkeyAction::ToggleHdr => {
                if let Err(e) = self.hdr.toggle() {
                    log!("record: HDR hotkey ignored: {e}");
                }
            }
        }
    }

    /// Forward `bookmark` / `screenshot` / `record_start|stop|toggle` to the
    /// engine, or report why it cannot run (paused, no game, starting; a
    /// long recording or a screenshot cannot start while a closed game drains).
    pub(crate) fn engine_command(&mut self, cmd: &'static str, client: Option<u64>) {
        self.engine_command_as(cmd, client, false);
    }

    /// As [`engine_command`](Self::engine_command); `hotkey`: a shortcut, which ticks once
    /// the engine has the command, never on a refusal.
    fn engine_command_as(&mut self, cmd: &'static str, client: Option<u64>, hotkey: bool) {
        let draining = self.planner.drain().is_some();
        let refused = capture_refusal(self.paused, self.engine_running(), &self.wanted())
            .or_else(|| self.activity.record_refusal(cmd, draining));
        if let Some(reason) = refused {
            if cmd == "bookmark" && client.is_none() {
                return self.refuse_bookmark(reason); // the hotkey
            }
            if cmd == "record_stop" || cmd == "bookmark" {
                return self.reply_error(client, &format!("{cmd}: {reason}"));
            }
            return self.refuse_capture(cmd, reason);
        }
        // Queued first: only a command the engine will see is marked in flight (and ticked).
        crate::record_dispatch::command(&mut ControllerCommand { ctl: self, cmd, client }, cmd, hotkey);
    }

    /// `record_stop` from the controller itself (pause, retarget, sleep, session end): queued
    /// or not, and when queued a stop is in flight (`EngineActivity::stop_sent`).
    pub(crate) fn send_record_stop(&mut self) -> Sent {
        let sent = self.supervisor.send_engine_line(r#"{"cmd":"record_stop"}"#);
        if sent.queued() {
            self.activity.stop_sent();
        }
        sent
    }

    /// A command the engine did not get: the same path a refusal takes, with why
    /// ("Recorder isn't responding", or "Switching games" while the engine quits for a retarget).
    fn command_not_sent(&self, cmd: &str, client: Option<u64>, reason: &'static str) {
        if cmd == "bookmark" && client.is_none() {
            return self.refuse_bookmark(reason);
        }
        if cmd == "record_stop" || cmd == "bookmark" {
            self.reply_error(client, &format!("{cmd}: {reason}"));
            return crate::supervisor::beep_failure();
        }
        self.refuse_capture(cmd, reason);
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

    /// A bookmark shortcut that could not mark anything: its pop-up + beep.
    fn refuse_bookmark(&self, reason: &str) {
        log!("record: bookmark refused -- {reason}");
        if self.cfg.overlay.enabled {
            self.hub.event(
                "session",
                &shortcut_notice_json("Bookmark not added", reason, true, false),
            );
        }
        crate::supervisor::beep_failure();
    }

    /// The pop-up for a shortcut with no engine event of its own (stats,
    /// overlay on); shown while notices (`overlay.enabled`) are on.
    pub(crate) fn shortcut_notice(&self, title: &str, detail: &str) {
        if self.cfg.overlay.enabled {
            self.hub.event(
                "session",
                &shortcut_notice_json(title, detail, false, false),
            );
        }
    }

    /// The overlay shortcut flips `overlay.enabled` (Settings "Show notices").
    /// "Overlay off" is forced through, so turning it off is seen too.
    fn toggle_overlay(&mut self) {
        let on = !self.cfg.overlay.enabled;
        let key = self.cfg.recording.overlay_hotkey.replace('+', " ");
        let was_running = self.overlay.running();
        if !on {
            self.overlay.linger(); // with stats off too, keep it up to show this
            let detail = format!("{key} shows it again");
            self.hub.event(
                "session",
                &shortcut_notice_json("Overlay off", &detail, false, true),
            );
        }
        self.apply_config_patch(&json!({"overlay": {"enabled": on}}), None);
        if on && was_running {
            self.shortcut_notice("Overlay on", "Notices show over any app");
        } else if on {
            // A fresh overlay process gets it with its first status.
            let notice =
                shortcut_notice_json("Overlay on", "Notices show over any app", false, false);
            self.pending_notice = Some((notice, Instant::now() + crate::overlay_proc::LINGER));
        }
    }

    /// The island editor shortcut: the overlay starts (or saves) it for the
    /// game in front. It only places the island in Dynamic mode, and only a
    /// running overlay can show it.
    fn island_editor(&mut self) {
        if self.cfg.overlay.corner != "dynamic" {
            let notice = shortcut_notice_json("Island editor needs Dynamic", "Set the island position to Dynamic", true, true);
            self.hub.event("session", &notice);
            return crate::supervisor::beep_failure();
        }
        if !self.overlay.running() {
            log!("record: island editor refused -- the overlay is not running (notices and stats off)");
            return crate::supervisor::beep_failure();
        }
        if crate::record_fullscreen::exclusive_now() {
            log!("record: island editor refused -- exclusive fullscreen (nothing can be dragged)");
            self.hub.event("session", &crate::record_editor_ask::borderless_notice_json());
            return crate::supervisor::beep_failure();
        }
        log!("record: island editor");
        self.editor_ask.ask(Instant::now());
        self.hub.event("session", &json!({"type": "island_editor"}).to_string());
    }

    /// The overlay's answer to the editor shortcut (`started` false: refused).
    pub(crate) fn island_editor_ack(&mut self, started: bool) {
        if self.editor_ask.ack(started) {
            log!("record: island editor refused by the overlay (see recall-overlay.log)");
            crate::supervisor::beep_failure();
        }
    }

    /// An overlay too old to know the editor never answers: beep once.
    pub(crate) fn island_editor_tick(&mut self, now: Instant) {
        if self.editor_ask.unanswered(now) {
            log!("record: island editor -- the overlay did not answer (an older overlay?)");
            crate::supervisor::beep_failure();
        }
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
                let sent = self.send_record_stop();
                for effect in crate::record_dispatch::pause_stop_effects(sent) {
                    match effect {
                        Effect::MarkPending => {
                            self.pending_pause = Some(wait);
                            return;
                        }
                        Effect::Refused(reason) => {
                            log!("record: pause -- record_stop not sent ({reason}); pausing now");
                            if self.cfg.overlay.enabled {
                                self.hub.event("session", &shortcut_notice_json("Pausing now", reason, true, false));
                            }
                            crate::supervisor::beep_failure();
                        }
                        Effect::Proceed => break, // the pause applies below, at once
                        _ => {}
                    }
                }
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
        let now = Instant::now();
        if paused {
            log!("record: PAUSED -- engine stopped, games ignored until resume");
            if self.planner.pause() {
                // A line sent while the engine quits says "Recording is paused".
                self.apply_engine_target_as(&EngineTarget::Off, Sent::Paused);
            }
        } else {
            log!("record: resumed");
            self.supervisor.unpause(); // no game in front: no longer "Recording is paused"
            self.last_fed = None;
            self.probe_foreground(now, false);
        }
        self.tray.set_paused(paused);
        self.publish_status();
    }

    /// Engine events forwarded by the supervisor.
    pub(crate) fn drain_engine_events(&mut self, now: Instant) {
        while let Ok(ev) = self.engine_events.try_recv() {
            crate::record_audio_live::on_engine_event(self, &ev);
            match self.activity.on_event(&ev, unix_now()) {
                Some(Trigger::Saved) => self.janitor.saved(now),
                Some(Trigger::AutoStopped { game_closed }) => {
                    let detail = if game_closed { "the game closed" } else { "the recorded window changed" };
                    log!("record: long recording saved -- {detail}");
                    self.shortcut_notice("Long recording saved", detail);
                }
                Some(Trigger::LongRecChanged) | None => {}
            }
        }
    }

    /// Engine settings changed: restart now, or once no save / long recording / closed
    /// game's drain is in flight (`pending_restart` in the status meanwhile); a mid-game
    /// restart tells the island (`record_restart::request_settings_restart`).
    pub(crate) fn request_engine_restart(&mut self) {
        crate::record_restart::request_settings_restart(self);
    }

    /// Planner -> engine, every pump tick. A retarget that falls due while
    /// the long recording runs stops it first (one `record_stop`; the
    /// retarget applies once it reports stopping, or after
    /// [`PAUSE_STOP_WAIT`]); one due while a save is in flight waits for it.
    /// Meanwhile the status says `pending_restart`.
    pub(crate) fn plan_engine(&mut self, now: Instant) {
        let buffered = self.supervisor.engine_view().buffer_secs;
        self.planner.set_drain_len(crate::record_drain::drain_len(buffered, self.cfg.recording.buffer_secs));
        let wanted = self.wanted();
        let busy = self.activity.retarget_busy(now);
        let drained = self.running_drain(now);
        match self.planner.step(&wanted, now, busy) {
            Step::Apply(target) => {
                self.retarget_held = false;
                self.note_drain_cut(drained.as_ref(), &target);
                self.apply_engine_target(&target);
            }
            Step::Held => {
                if !self.retarget_held {
                    log!("record: engine retarget deferred until the save ends");
                }
                self.retarget_held = true;
            }
            Step::StopLongRec { game_closed } => {
                let game_closed = self.closed_game(game_closed);
                if self.activity.auto_stop(now, game_closed) {
                    let why = if game_closed { "the game closed" } else { "a new game is wanted" };
                    log!("record: {why} -- stopping the long recording before the retarget (up to {} s)", PAUSE_STOP_WAIT.as_secs());
                    // Not a user action: logged; the auto-stop deadline bounds the wait.
                    if !crate::record_dispatch::automatic_effects(self.send_record_stop()).is_empty() {
                        log!("record: record_stop not sent ({}); the retarget goes ahead after {} s", crate::record_dispatch::NOT_RESPONDING, PAUSE_STOP_WAIT.as_secs());
                    }
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
        crate::record_restart::pending_restart_tick(self);
        let (dir, cap) = (self.cfg.recording.output_dir.clone(), self.cfg.storage.cap_gb);
        let rec = &self.cfg.recording;
        let buffer = crate::record_free_space::buffer_bytes(rec.buffer_secs, rec.bitrate_mbps);
        self.janitor.tick(&dir, cap, buffer, now);
    }
}

/// `engine_command`'s port to `record_dispatch::command`: the live path, run in the order
/// `command` sets (queue, mark in flight, announce, tick).
struct ControllerCommand<'a> {
    ctl: &'a mut Controller,
    cmd: &'static str,
    client: Option<u64>,
}

impl crate::record_dispatch::CommandPort for ControllerCommand<'_> {
    fn queue(&mut self, line: &str) -> crate::record_dispatch::Sent {
        let sent = self.ctl.supervisor.send_engine_line(line);
        match sent.reason() {
            None => log!("record: engine {}", self.cmd),
            Some(reason) => log!("record: engine {} (not sent: {reason})", self.cmd),
        }
        sent
    }
    fn mark_pending(&mut self) {
        self.ctl.activity.command_sent(self.cmd, Instant::now());
    }
    fn announce(&mut self) {
        self.ctl.publish_status();
    }
    fn cue(&mut self) {
        crate::cues::tick();
    }
    fn refused(&mut self, reason: &'static str) {
        self.ctl.command_not_sent(self.cmd, self.client, reason);
    }
}
