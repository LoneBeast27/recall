//! The record controller's session status (`record_cmds`): the `status` value, its
//! broadcast with the tray line, the island notices it drives, and the late-joiner reply.

use std::time::Instant;

use serde_json::{json, Value};

use crate::log;
use crate::record_app::Controller;
use crate::record_notice::Situation;
use crate::record_status::{status_json, tray_line, StatusInputs};
use crate::supervisor::EngineHealth;

impl Controller {
    fn status_value(&mut self) -> Value {
        let wanted = self.wanted();
        let view = self.supervisor.engine_view();
        let hdr = self.hdr.snapshot(&self.cfg);
        let finishing = self.finishing_now(Instant::now());
        let mut status = status_json(&StatusInputs {
            wanted: &wanted,
            health: self.supervisor.health(),
            encoder: view.encoder.as_deref(),
            fresh_engine: self.supervisor.hello_count() > self.hello_at_retarget,
            cfg: &self.cfg,
            mic_muted: self.mic_muted,
            overlay_running: self.overlay.running(),
            fps_source: crate::frame_timing::fps_source(self.cfg.overlay.shows_stats(), self.can_trace).as_str(),
            hdr: &hdr,
            paused: self.paused,
            long_recording: self.activity.long_recording_json(),
            // Settings restart or a held retarget: both wait for idle.
            pending_restart: self.pending_restart.is_some() || self.retarget_held,
            drain: self.drain_clock.status(self.planner.drain(), Instant::now()),
            hotkey_problems: &self.hotkey_problems,
        });
        crate::record_quit_wait::merge_fields(&mut status, finishing);
        if crate::record_quit_wait::sign_out_only(self.quit, &self.quit_wait) {
            status["quit_for_sign_out"] = json!(true);
        }
        if let Some(owner) = self.setup_owner {
            status["setup_owner"] = json!(owner);
        }
        status["other_recorder"] = json!(self.supervisor.slot_held() != crate::engine_slot::SlotHeld::Free);
        // The last audio line sent: the Audio page says "Applying…" until the engine's
        // `status.audio_seq` reaches it (design-audio-mute P20).
        status["audio_sent_seq"] = json!(self.supervisor.last_audio_seq());
        // R1 + Rohan 22:00 UTC: an engine file older than the app asks for a Reinstall button.
        status["repair"] = crate::record_inputs_held::repair_json(self.supervisor.engine_below_floor());
        status
    }

    fn legacy_hdr_event(&self) -> Option<String> {
        let snap = self.hdr.snapshot(&self.cfg);
        snap.available
            .then(|| snap.legacy_event(crate::app::health_summary(self.supervisor.health())))
    }

    /// Broadcast the session status (and tray line) when anything changed.
    pub(crate) fn publish_status(&mut self) {
        let status = self.status_value();
        // The drain countdown / recording time tick every call: they do not count as a change.
        let key = crate::record_status::dedup_key(&status);
        if key != self.last_session {
            let state = status["state"].as_str().unwrap_or("waiting").to_string();
            self.tray.set_paused(self.paused);
            let slot = crate::engine_slot::slot_tray_line(&state, self.supervisor.slot_held());
            self.tray.set_status(&slot.map_or_else(|| tray_line(&state, &self.wanted()), str::to_owned));
            self.hub.event("session", &status.to_string());
            self.last_session = key;
            self.announce(&state);
        }
        if let Some(hdr) = self.legacy_hdr_event() {
            if hdr != self.last_hdr_event {
                self.hub.event("hdr", &hdr);
                self.last_hdr_event = hdr;
            }
        }
    }

    /// Recall Island notice + cue for a recording change. The tracker always
    /// follows the state; with notices off nothing is sent or heard.
    fn announce(&mut self, state: &str) {
        let gave_up = self.supervisor.health() == EngineHealth::GivenUp;
        let slot = crate::engine_slot::slot_situation(state, self.supervisor.slot_held());
        let next = match (state, self.planner.drain()) {
            ("draining", Some((_, _, len))) => Some(Situation::Draining { secs: len.as_secs_f32().round() as u64 }),
            _ => slot.or_else(|| Situation::of(state, &self.wanted(), gave_up)),
        };
        let Some(notice) = self.notices.observe(next, Instant::now()) else { return };
        if !self.cfg.overlay.shows_notices() {
            return;
        }
        log!("record: notice {}: {} {}", notice.kind(), notice.title(), notice.detail());
        self.hub.event("session", &self.notices.notice_body(&notice));
        if notice.is_failure() {
            crate::cues::failed();
        } else {
            crate::cues::tick();
        }
    }

    /// Late-joiner / on-demand snapshot for one client.
    pub(super) fn reply_status(&mut self, client: u64) {
        if !self.hdr.is_active() {
            self.hdr.refresh_live();
        }
        let status = self.status_value().to_string();
        self.hub.event_to(client, "session", &status);
        for line in self.supervisor.cached_engine_events() {
            self.hub.event_to(client, "engine", &line);
        }
        if let Some(hdr) = self.legacy_hdr_event() {
            self.hub.event_to(client, "hdr", &hdr);
        }
        // Every client that joins before the deadline gets it, not just the first: the app's own
        // status queries join too (and ignore notices), so a take() could hand it to the app and
        // leave the starting overlay without it. The overlay joins once per process.
        match &self.pending_notice {
            Some((notice, until)) if Instant::now() < *until => self.hub.event_to(client, "session", notice),
            Some(_) => self.pending_notice = None,
            None => {}
        }
    }
}
