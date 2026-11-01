//! Keep the Mac's redirected source at unity; listening volume lives after DSP.
//! This is never enabled for an observation-only capture or a direct output.
use super::OutputVolume;
use anyhow::{Result, bail};
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct SourceVolume {
    enabled: bool,
    source: Option<u8>,
    pending: Option<Instant>,
    /// A reset's 100% event can arrive after the confirming state snapshot.
    /// It must never be mistaken for a request to unmute the output.
    reset_echo: bool,
}
impl SourceVolume {
    /// Returns true when the output fader changed, so the UI can save it.
    pub fn observe(&mut self, message: &Value, output: &OutputVolume) -> bool {
        let event = message["type"] == "volume_changed";
        if !event && message["type"] != "playback_state" {
            return false;
        }
        let Some(percent) = message["volume"].as_u64().map(|v| v.min(100) as u8) else {
            return false;
        };
        let previous = self.source.replace(percent);
        if !self.enabled {
            return false;
        }
        let own_reset = event && percent == 100 && self.reset_echo;
        if own_reset {
            self.reset_echo = false;
        }
        if percent == 100 {
            self.pending = None;
        }
        // Snapshots confirm the source level, never replace a newer local
        // fader choice. Only actual external volume events change that fader.
        if event && !own_reset && (percent != 100 || previous != Some(100)) {
            output.set(percent);
            return true;
        }
        false
    }

    /// Ask for unity only after the native route has muted the dry source.
    /// Accepted commands are asynchronous, so wait for state confirmation.
    pub fn maintain(&mut self, redirected: bool, now: Instant) -> Result<bool> {
        if !redirected {
            self.enabled = false;
            self.pending = None;
            self.reset_echo = false;
            return Ok(false);
        }
        self.enabled = true;
        if let Some(sent) = self.pending {
            if now.duration_since(sent) >= Duration::from_secs(5) {
                bail!(
                    "Soloist did not confirm its audio level. Retry in Settings to reconnect it."
                );
            }
            return Ok(false);
        }
        if self.source == Some(100) {
            return Ok(false);
        }
        self.pending = Some(now);
        self.reset_echo = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(volume: u8) -> Value {
        json!({"type":"playback_state", "volume":volume})
    }
    fn event(volume: u8) -> Value {
        json!({"type":"volume_changed", "volume":volume})
    }

    #[test]
    fn low_source_is_raised_only_after_redirecting_and_keeps_saved_output() {
        let mut source = SourceVolume::default();
        let output = OutputVolume::default();
        output.set(35);
        let now = Instant::now();
        assert!(!source.observe(&state(5), &output));
        assert!(!source.maintain(false, now).unwrap());
        assert!(!source.observe(&event(10), &output));
        assert_eq!(output.percent(), 35);
        assert!(source.maintain(true, now).unwrap());
        assert!(!source.maintain(true, now).unwrap());
        assert!(!source.observe(&event(100), &output));
        assert_eq!(output.percent(), 35);
        assert!(!source.maintain(true, now).unwrap());
    }

    #[test]
    fn phone_volume_changes_output_and_automatic_reset_never_unmutes_it() {
        let mut source = SourceVolume::default();
        let output = OutputVolume::default();
        let now = Instant::now();
        source.observe(&state(100), &output);
        assert!(!source.maintain(true, now).unwrap());
        for percent in [40, 0, 70] {
            assert!(source.observe(&event(percent), &output));
            assert_eq!(output.percent(), percent);
            assert!(source.maintain(true, now).unwrap());
            assert!(!source.observe(&state(100), &output));
            assert!(!source.maintain(true, now).unwrap());
            assert!(!source.observe(&event(100), &output));
            assert_eq!(output.percent(), percent);
        }
        // Duplicate reset events must not turn a quiet or muted output up.
        assert!(!source.observe(&event(100), &output));
        assert_eq!(output.percent(), 70);
    }

    #[test]
    fn quick_phone_changes_and_lagging_snapshots_keep_the_latest_output() {
        let mut source = SourceVolume::default();
        let output = OutputVolume::default();
        let now = Instant::now();
        source.observe(&state(100), &output);
        source.maintain(true, now).unwrap();
        source.observe(&event(50), &output);
        assert!(source.maintain(true, now).unwrap());
        source.observe(&event(20), &output);
        assert!(!source.maintain(true, now).unwrap());
        source.observe(&event(100), &output);
        assert_eq!(output.percent(), 20);
        output.set(65); // A newer slider action while a state read was in flight.
        source.observe(&state(20), &output);
        assert_eq!(output.percent(), 65);
        assert!(source.maintain(true, now).unwrap());
        source.observe(&event(100), &output);
        assert_eq!(output.percent(), 65);
    }

    #[test]
    fn a_snapshot_before_the_phone_mute_event_must_not_hide_the_mute() {
        let mut source = SourceVolume::default();
        let output = OutputVolume::default();
        let now = Instant::now();
        source.observe(&state(100), &output);
        source.maintain(true, now).unwrap();
        assert!(!source.observe(&state(0), &output));
        assert!(source.maintain(true, now).unwrap());
        assert!(source.observe(&event(0), &output));
        assert_eq!(output.percent(), 0);
        assert!(!source.observe(&event(100), &output));
        assert_eq!(output.percent(), 0);
    }

    #[test]
    fn acknowledgements_do_not_confirm_volume_and_timeouts_are_bounded() {
        let mut source = SourceVolume::default();
        let output = OutputVolume::default();
        let now = Instant::now();
        assert!(source.maintain(true, now).unwrap());
        assert!(!source.observe(
            &json!({"type":"command_result", "command":"set_volume"}),
            &output
        ));
        assert!(!source.maintain(true, now + Duration::from_secs(4)).unwrap());
        assert!(source.maintain(true, now + Duration::from_secs(5)).is_err());
        assert!(
            !source
                .maintain(false, now + Duration::from_secs(6))
                .unwrap()
        );
        source.observe(&event(15), &output);
        assert_eq!(output.percent(), 100, "ordinary output remains independent");
    }
}
