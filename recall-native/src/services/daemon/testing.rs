//! The setup rehearsal owns this config in memory. There is no pipe, file or recorder.
use std::sync::Arc;
use serde_json::{Value, json};

use super::{DaemonClient, DaemonCommand, Shared};

impl DaemonClient {
    pub(super) fn testing() -> Self {
        let shared = Arc::new(Shared::new());
        shared.update(|state| {
            state.set_connected(true);
            state.snapshot.config = Some(json!({
                "recording": {
                    "codec": "h264", "fps": 60, "bitrate_mbps": 25, "quality": "balanced",
                    "audio_sources": [
                        {"id": "mic", "kind": "input", "name": "Microphone"},
                        {"id": "system-output", "kind": "output", "name": "System output"},
                        {"id": "communications-output", "kind": "output", "name": "Communications", "communications": true}
                    ]
                },
                "storage": {"demo_drive": 0, "cap_gb": 0, "cache_gib": 2, "cached_mib": 512}
            }));
            true
        });
        Self { shared }
    }

    pub(super) fn send_testing(&self, command: DaemonCommand) -> bool {
        match command {
            DaemonCommand::Status | DaemonCommand::GetConfig => true,
            DaemonCommand::SetConfig(Value::Object(patch)) => {
                self.shared.update(|state| {
                    let Some(config) = state.snapshot.config.as_mut() else { return false };
                    for (section, updates) in patch {
                        if let (Some(values), Value::Object(updates)) =
                            (config.get_mut(section).and_then(Value::as_object_mut), updates)
                        {
                            values.extend(updates);
                        }
                    }
                    true
                });
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rehearsal_accepts_edits_only_in_its_own_snapshot() {
        let client = DaemonClient::testing();
        assert!(client.send_testing(DaemonCommand::SetConfig(json!({"storage":{"cached_mib": 0}}))));
        let snapshot = client.snapshot().config.unwrap();
        assert_eq!(snapshot["storage"]["cached_mib"], 0);
        assert_eq!(snapshot["storage"]["cache_gib"], 2);
        assert_eq!(snapshot["recording"]["audio_sources"].as_array().unwrap().len(), 3);
        for command in [DaemonCommand::Quit, DaemonCommand::Resume, DaemonCommand::RecordToggle, DaemonCommand::StoragePreview(1)] {
            assert!(!client.send_testing(command));
        }
        assert_eq!(DaemonClient::testing().snapshot().config.unwrap()["storage"]["cached_mib"], 512);
    }
}
