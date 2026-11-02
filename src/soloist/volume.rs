//! Keep the Mac's redirected source at unity; listening volume lives after DSP.
//! This is never enabled for an observation-only capture or a direct output.
use super::OutputVolume;
use anyhow::{Result, bail};
use serde_json::Value;
use std::time::{Duration, Instant};

/// How long a restored level may go unconfirmed before it is asked for again.
const RESTORE_RETRY: Duration = Duration::from_secs(2);
/// How many times a player taking commands may ignore it before its level stands.
const RESTORE_TRIES: u32 = 5;

/// Spotify's player starts at a level of its own every launch, so the level
/// it was left at is kept here and given back once it can take a command.
#[derive(Default)]
pub(super) struct ListeningVolume {
    restored: bool,
    asked: Option<Instant>,
    tries: u32,
}
impl ListeningVolume {
    /// Returns the level to send, and whether the level to keep changed.
    pub fn observe(
        &mut self,
        message: &Value,
        ready: bool,
        output: &OutputVolume,
        now: Instant,
    ) -> (Option<u8>, bool) {
        if !matches!(
            message["type"].as_str(),
            Some("playback_state" | "volume_changed")
        ) {
            return (None, false);
        }
        let Some(percent) = message["volume"].as_u64().map(|v| v.min(100) as u8) else {
            return (None, false);
        };
        if !ready {
            return (None, false);
        }
        if !self.restored {
            match output.remembered().filter(|wanted| *wanted != percent) {
                None => self.restored = true,
                Some(_) if self.tries >= RESTORE_TRIES => self.restored = true,
                Some(wanted) => {
                    if self
                        .asked
                        .is_some_and(|at| now.duration_since(at) < RESTORE_RETRY)
                    {
                        return (None, false);
                    }
                    self.asked = Some(now);
                    self.tries += u32::from(message["is_active"] == true);
                    output.lead(wanted);
                    return (Some(wanted), false);
                }
            }
        }
        (None, output.remember(percent))
    }
}

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

    fn listening(volume: u8) -> Value {
        json!({"type":"playback_state", "volume":volume, "is_active":true})
    }

    #[test]
    fn soloist_comes_back_at_the_level_it_was_left_at() {
        let mut restore = ListeningVolume::default();
        let output = OutputVolume::default();
        output.remember(70);
        let now = Instant::now();
        // Nothing is sent before login, and its own start level isn't kept.
        assert_eq!(
            restore.observe(&listening(33), false, &output, now),
            (None, false)
        );
        assert_eq!(
            restore.observe(&listening(33), true, &output, now),
            (Some(70), false)
        );
        assert_eq!(output.route_percent(), Some(70), "heard before it answers");
        // A state from before the command changes nothing.
        assert_eq!(
            restore.observe(&listening(33), true, &output, now),
            (None, false)
        );
        assert_eq!(output.remembered(), Some(70));
        assert_eq!(
            restore.observe(&listening(70), true, &output, now),
            (None, false)
        );
        // From there on the level is followed, so a phone's change is kept.
        assert_eq!(
            restore.observe(&event(40), true, &output, now),
            (None, true)
        );
        assert_eq!(output.remembered(), Some(40));
    }

    #[test]
    fn a_first_run_keeps_soloists_level_and_a_deaf_player_is_not_fought() {
        let mut restore = ListeningVolume::default();
        let output = OutputVolume::default();
        let now = Instant::now();
        assert_eq!(
            restore.observe(&listening(33), true, &output, now),
            (None, true)
        );
        assert_eq!(output.remembered(), Some(33));

        let mut restore = ListeningVolume::default();
        output.remember(70);
        let mut sent = 0;
        for second in 0..60 {
            let at = now + Duration::from_secs(second);
            sent += usize::from(
                restore
                    .observe(&listening(33), true, &output, at)
                    .0
                    .is_some(),
            );
        }
        assert_eq!(sent, RESTORE_TRIES as usize);
        assert_eq!(output.remembered(), Some(33));
    }

    #[test]
    fn an_idle_player_is_asked_until_it_plays() {
        let mut restore = ListeningVolume::default();
        let output = OutputVolume::default();
        output.remember(70);
        let now = Instant::now();
        let idle = json!({"type":"playback_state", "volume":33, "is_active":false});
        for minute in 0..30 {
            let at = now + Duration::from_secs(minute * 60);
            assert_eq!(restore.observe(&idle, true, &output, at).0, Some(70));
        }
        assert_eq!(output.remembered(), Some(70));
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
