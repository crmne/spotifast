//! What a skin sees of the player, and what it asks of it.
//!
//! The skin's `player` object reads a [`Deck`], a snapshot the app takes
//! every frame, and everything a script or control does comes out as a
//! [`Command`] for the app to perform. The skin engine itself touches
//! neither audio nor the network.

/// `player.playState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayState {
    #[default]
    Stopped = 1,
    Paused = 2,
    Playing = 3,
    Buffering = 6,
}

pub const OPEN_MEDIA_CHANGING: i32 = 8;
pub const OPEN_MEDIA_OPEN: i32 = 13;
pub const OPEN_NO_MEDIA: i32 = 6;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub uri: String,
    pub duration_s: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Deck {
    pub state: PlayState,
    /// The song on, if any.
    pub song: Option<Song>,
    pub position_s: f64,
    /// The song's bitrate in bits a second, where known.
    pub bitrate: u32,
    pub volume: u8,
    pub mute: bool,
    /// -100 all left, 100 all right.
    pub balance: i32,
    pub shuffle: bool,
    pub repeat: bool,
    pub can_control: bool,
    /// The list a skin's playlist shows: what plays, then the queue.
    pub queue: Vec<Song>,
    /// Where the playing song sits in `queue`.
    pub playing: Option<usize>,
    pub eq_on: bool,
    pub eq_bands: [f32; 10],
    pub eq_preamp: f32,
    pub eq_presets: Vec<String>,
    pub eq_preset: Option<usize>,
    /// The visualisation: 0 bars, 1 sound wave, 2 none.
    pub effect: usize,
}

impl Default for Deck {
    fn default() -> Self {
        Self {
            state: PlayState::Stopped,
            song: None,
            position_s: 0.0,
            bitrate: 0,
            volume: 100,
            mute: false,
            balance: 0,
            shuffle: false,
            repeat: false,
            can_control: true,
            queue: Vec::new(),
            playing: None,
            eq_on: false,
            eq_bands: [0.0; 10],
            eq_preamp: 0.0,
            eq_presets: Vec::new(),
            eq_preset: None,
            effect: 0,
        }
    }
}

impl Deck {
    pub fn status(&self) -> &'static str {
        match self.state {
            PlayState::Playing => "Playing",
            PlayState::Paused => "Paused",
            PlayState::Buffering => "Buffering",
            PlayState::Stopped => {
                if self.song.is_some() {
                    "Stopped"
                } else {
                    "Ready"
                }
            }
        }
    }

    pub fn open_state(&self) -> i32 {
        if self.song.is_some() {
            OPEN_MEDIA_OPEN
        } else {
            OPEN_NO_MEDIA
        }
    }

    /// Whether `player.controls.isAvailable(name)`.
    pub fn available(&self, name: &str) -> bool {
        let on = self.song.is_some() && self.can_control;
        let sounding = matches!(self.state, PlayState::Playing | PlayState::Buffering);
        match name.to_ascii_lowercase().as_str() {
            "play" => on && !sounding,
            "pause" => on && sounding,
            "stop" => on && self.state != PlayState::Stopped,
            "next" | "previous" => on,
            "fastforward" | "fastreverse" => on && sounding,
            "currentposition" | "currentitem" => self.song.is_some(),
            _ => false,
        }
    }
}

/// What a skin asks of the player and the window.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    /// Pause and rewind: Spotify has no stopped state.
    Stop,
    Next,
    Previous,
    /// To this many milliseconds in.
    Seek(u32),
    /// By this many milliseconds.
    SeekBy(i64),
    SetVolume(u8),
    /// A step of a drag: to be heard, with `SetVolume` to follow on release.
    PreviewVolume(u8),
    SetMute(bool),
    SetBalance(i32),
    SetShuffle(bool),
    SetRepeat(bool),
    /// A row of [`Deck::queue`].
    PlayQueued(usize),
    SetEqBand(usize, f32),
    SetEqPreamp(f32),
    SetEqOn(bool),
    SetEqPreset(usize),
    /// Reset all bands.
    ResetEq,
    /// Move the visualisation on or back.
    Effect(i32),
    /// The skin closed itself, or asked for the full player.
    BackToMain,
}

/// `mm:ss`, with hours only once there are any, as WMP writes a position.
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_are_available_as_the_state_allows() {
        let mut deck = Deck::default();
        assert!(!deck.available("play") && !deck.available("next"));
        deck.song = Some(Song::default());
        assert!(deck.available("Play") && !deck.available("pause"));
        assert!(!deck.available("stop"));
        deck.state = PlayState::Playing;
        assert!(!deck.available("play") && deck.available("PAUSE"));
        assert!(deck.available("stop") && deck.available("fastForward"));
        assert!(!deck.available("step"));
    }

    #[test]
    fn a_clock_shows_hours_only_when_there_are_some() {
        assert_eq!(clock(0.0), "00:00");
        assert_eq!(clock(83.9), "01:23");
        assert_eq!(clock(3725.0), "01:02:05");
        assert_eq!(clock(-4.0), "00:00");
    }
}
