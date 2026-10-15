//! Local, account-scoped ordering of outputs explicitly chosen in the picker.

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};

const RECENT_CHOICES: usize = 32;

/// Only preference keys are saved, never raw receiver addresses or credentials.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceHistory {
    account_id: String,
    recent: Vec<String>,
}

impl DeviceHistory {
    /// Remembers a selection, bounding both storage and the popularity window.
    pub fn record(&mut self, account: &str, device: &str) {
        if account.is_empty() || device.is_empty() {
            return;
        }
        if self.account_id != account {
            self.account_id = account.to_owned();
            self.recent.clear();
        }
        self.recent.insert(0, device.to_owned());
        self.recent.truncate(RECENT_CHOICES);
    }

    /// Frequent choices first, then the most recent choice among equal counts.
    pub fn rank(&self, account: Option<&str>, device: Option<&str>) -> (Reverse<usize>, usize) {
        let device = device.filter(|_| account == Some(self.account_id.as_str()));
        let count = self
            .recent
            .iter()
            .filter(|id| Some(id.as_str()) == device)
            .count();
        let latest = self
            .recent
            .iter()
            .position(|id| Some(id.as_str()) == device);
        (Reverse(count), latest.unwrap_or(usize::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequent_choices_win_and_recent_choices_break_ties() {
        let mut history = DeviceHistory::default();
        for device in ["speaker", "phone", "speaker", "tv"] {
            history.record("alice", device);
        }
        let rank = |device| history.rank(Some("alice"), Some(device));
        assert!(rank("speaker") < rank("tv"));
        assert!(rank("tv") < rank("phone"));
        assert!(rank("phone") < rank("unknown"));
    }

    #[test]
    fn old_choices_age_out_and_accounts_never_share_a_ranking() {
        let mut history = DeviceHistory::default();
        history.record("alice", "old");
        for _ in 0..RECENT_CHOICES {
            history.record("alice", "new");
        }
        assert_eq!(history.recent.len(), RECENT_CHOICES);
        assert_eq!(
            history.rank(Some("alice"), Some("old")),
            history.rank(None, None)
        );
        assert_eq!(
            history.rank(Some("bob"), Some("new")),
            history.rank(None, None)
        );
        history.record("bob", "phone");
        assert_eq!(history.recent, ["phone"]);
        assert_eq!(history.account_id, "bob");
    }

    #[test]
    fn missing_history_is_compatible_and_choices_survive_serialization() {
        let mut history: DeviceHistory = serde_json::from_str("{}").unwrap();
        history.record("alice", "speaker-id");
        let encoded = serde_json::to_string(&history).unwrap();
        assert_eq!(
            serde_json::from_str::<DeviceHistory>(&encoded).unwrap(),
            history
        );
        history.record("", "ignored");
        history.record("alice", "");
        assert_eq!(history.recent, ["speaker-id"]);
    }
}
