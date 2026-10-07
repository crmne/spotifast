//! The messages a jam exchanges, one JSON object per line.
//!
//! Peers are untrusted: the server treats every listener so, and every
//! listener treats the server so. Every line is bounded before it is parsed,
//! and every decoded message is validated and its text cleaned before it is
//! used. A message carries song URIs and short display text only: never a
//! URL, which would let a peer make a computer fetch an address of its
//! choosing, and never a Spotify credential.

use serde::{Deserialize, Serialize};

/// Bumped on any incompatible change to the messages below.
pub const PROTOCOL_VERSION: u32 = 2;
/// The longest line accepted from a peer, newline excluded. A full state
/// with [`MAX_QUEUE`] songs fits well within it.
pub const MAX_LINE_BYTES: usize = 256 * 1024;
pub const MAX_NAME_CHARS: usize = 32;
pub const MAX_TITLE_CHARS: usize = 200;
/// Songs waiting in the shared queue, the playing one excluded.
pub const MAX_QUEUE: usize = 500;
/// Listeners in the jam at once.
pub const MAX_PARTICIPANTS: usize = 32;
/// Songs one listener may have waiting in the queue at once.
pub const MAX_QUEUED_PER_LISTENER: usize = 50;
/// A song longer than a day is not a song; durations are clamped to this.
const MAX_DURATION_MS: u32 = 24 * 60 * 60 * 1000;
/// The handshake's nonce and proof, base64url without padding.
const MAX_TOKEN_CHARS: usize = 64;

pub type ItemId = u64;
pub type ParticipantId = u32;

/// A song in the jam. `id` tells two copies of one song apart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JamItem {
    pub id: ItemId,
    pub uri: String,
    pub title: String,
    pub artists: String,
    pub duration_ms: u32,
    pub added_by: ParticipantId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub id: ParticipantId,
    pub name: String,
}

/// Everything a listener needs to show the jam and follow its playback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JamState {
    /// Grows with every change, so a late state never replaces a newer one.
    pub seq: u64,
    pub current: Option<JamItem>,
    pub queue: Vec<JamItem>,
    pub playing: bool,
    /// Position in `current` at `server_time_ms`.
    pub position_ms: u32,
    /// The server's clock when this state was taken, in milliseconds since
    /// it started.
    pub server_time_ms: u64,
    pub participants: Vec<Participant>,
}

/// A listener's message to the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// The answer to [`ServerMsg::Challenge`]; see [`crate::auth::proof`].
    Hello {
        version: u32,
        name: String,
        proof: String,
    },
    Add {
        uri: String,
        title: String,
        artists: String,
        duration_ms: u32,
    },
    Remove {
        item: ItemId,
    },
    Move {
        item: ItemId,
        to: usize,
    },
    Skip,
    SetPlaying {
        playing: bool,
    },
    Seek {
        position_ms: u32,
    },
    /// This listener's player reached the end of `item`. The first such
    /// report moves the jam on; later ones name a song no longer playing.
    Ended {
        item: ItemId,
    },
    /// Clock synchronisation: `t0` is the listener's clock when sent.
    Ping {
        t0: u64,
    },
}

/// The server's message to a listener.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The first message of every connection, with a fresh random nonce.
    Challenge {
        version: u32,
        nonce: String,
    },
    Welcome {
        you: ParticipantId,
        state: JamState,
    },
    Rejected {
        reason: Rejection,
    },
    State {
        state: JamState,
    },
    /// A request the server would not apply. The state is unchanged.
    Refused {
        reason: Refusal,
    },
    Pong {
        t0: u64,
        server_time_ms: u64,
    },
}

/// Why a listener was not let in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    /// The proof did not match: wrong server code.
    BadPassword,
    Full,
    IncompatibleVersion,
}

/// Why a request was not applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    QueueFull,
    /// This listener already has [`MAX_QUEUED_PER_LISTENER`] songs waiting.
    QuotaReached,
    /// The song or row named no longer exists, usually because it played.
    NotFound,
    /// A handshake or clock message, which is not a request.
    Unexpected,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("message exceeds {MAX_LINE_BYTES} bytes")]
    TooLong,
    #[error("malformed message")]
    Malformed,
    #[error("invalid song link")]
    InvalidUri,
    #[error("empty name")]
    EmptyName,
    #[error("invalid token")]
    InvalidToken,
    #[error("too many entries")]
    TooMany,
}

/// One line to write to a peer, newline included. JSON escapes the
/// newlines inside strings, so the message never spans two lines.
pub fn encode<T: Serialize>(message: &T) -> String {
    let mut line = serde_json::to_string(message).expect("jam messages always serialize");
    line.push('\n');
    line
}

/// Parses and validates a listener's line. Text comes back cleaned.
pub fn decode_client(line: &str) -> Result<ClientMsg, ProtocolError> {
    let message: ClientMsg = parse(line)?;
    Ok(match message {
        ClientMsg::Hello {
            version,
            name,
            proof,
        } => ClientMsg::Hello {
            version,
            name: participant_name(&name)?,
            proof: token(proof)?,
        },
        ClientMsg::Add {
            uri,
            title,
            artists,
            duration_ms,
        } => ClientMsg::Add {
            uri: song_uri(uri)?,
            title: clean_text(&title, MAX_TITLE_CHARS),
            artists: clean_text(&artists, MAX_TITLE_CHARS),
            duration_ms: duration_ms.min(MAX_DURATION_MS),
        },
        ClientMsg::Seek { position_ms } => ClientMsg::Seek {
            position_ms: position_ms.min(MAX_DURATION_MS),
        },
        other => other,
    })
}

/// Parses and validates the server's line. The server is a peer like any
/// other: its states are bounded and cleaned the same way.
pub fn decode_server(line: &str) -> Result<ServerMsg, ProtocolError> {
    let message: ServerMsg = parse(line)?;
    Ok(match message {
        ServerMsg::Challenge { version, nonce } => ServerMsg::Challenge {
            version,
            nonce: token(nonce)?,
        },
        ServerMsg::Welcome { you, state } => ServerMsg::Welcome {
            you,
            state: clean_state(state)?,
        },
        ServerMsg::State { state } => ServerMsg::State {
            state: clean_state(state)?,
        },
        other => other,
    })
}

fn parse<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, ProtocolError> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() > MAX_LINE_BYTES {
        return Err(ProtocolError::TooLong);
    }
    serde_json::from_str(line).map_err(|_| ProtocolError::Malformed)
}

fn clean_state(mut state: JamState) -> Result<JamState, ProtocolError> {
    if state.queue.len() > MAX_QUEUE || state.participants.len() > MAX_PARTICIPANTS {
        return Err(ProtocolError::TooMany);
    }
    state.current = state.current.map(clean_item).transpose()?;
    state.queue = state
        .queue
        .into_iter()
        .map(clean_item)
        .collect::<Result<_, _>>()?;
    for participant in &mut state.participants {
        participant.name = participant_name(&participant.name)?;
    }
    state.position_ms = state.position_ms.min(MAX_DURATION_MS);
    Ok(state)
}

fn clean_item(item: JamItem) -> Result<JamItem, ProtocolError> {
    Ok(JamItem {
        uri: song_uri(item.uri)?,
        title: clean_text(&item.title, MAX_TITLE_CHARS),
        artists: clean_text(&item.artists, MAX_TITLE_CHARS),
        duration_ms: item.duration_ms.min(MAX_DURATION_MS),
        ..item
    })
}

/// Accepts exactly `spotify:track:<id>` or `spotify:episode:<id>` with a
/// 22-character base62 id: anything else could name a playlist, a local
/// file, or text that is not a Spotify link at all.
pub fn song_uri(uri: String) -> Result<String, ProtocolError> {
    let id = uri
        .strip_prefix("spotify:track:")
        .or_else(|| uri.strip_prefix("spotify:episode:"))
        .ok_or(ProtocolError::InvalidUri)?;
    if id.len() == 22 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(uri)
    } else {
        Err(ProtocolError::InvalidUri)
    }
}

fn participant_name(name: &str) -> Result<String, ProtocolError> {
    let name = clean_text(name, MAX_NAME_CHARS);
    if name.is_empty() {
        Err(ProtocolError::EmptyName)
    } else {
        Ok(name)
    }
}

fn token(token: String) -> Result<String, ProtocolError> {
    if !token.is_empty()
        && token.len() <= MAX_TOKEN_CHARS
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        Ok(token)
    } else {
        Err(ProtocolError::InvalidToken)
    }
}

/// Display text from a peer: control characters and the invisible marks
/// that reorder or hide text are dropped, so a name cannot pass itself off
/// as another, and runs of whitespace become one space.
pub fn clean_text(text: &str, max_chars: usize) -> String {
    let mut cleaned = String::with_capacity(text.len().min(max_chars * 4));
    let mut count = 0;
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = !cleaned.is_empty();
            continue;
        }
        if ch.is_control() || invisible(ch) {
            continue;
        }
        if count + usize::from(space) + 1 > max_chars {
            break;
        }
        if space {
            cleaned.push(' ');
            count += 1;
            space = false;
        }
        cleaned.push(ch);
        count += 1;
    }
    cleaned
}

/// Bidirectional overrides and isolates, and zero-width characters.
fn invisible(ch: char) -> bool {
    matches!(
        ch,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";

    fn item(id: ItemId, uri: &str) -> JamItem {
        JamItem {
            id,
            uri: uri.into(),
            title: "Song".into(),
            artists: "Artist".into(),
            duration_ms: 1000,
            added_by: 1,
        }
    }

    fn state() -> JamState {
        JamState {
            seq: 1,
            current: Some(item(1, TRACK)),
            queue: vec![item(2, TRACK)],
            playing: true,
            position_ms: 10,
            server_time_ms: 20,
            participants: vec![Participant {
                id: 1,
                name: "Ana".into(),
            }],
        }
    }

    #[test]
    fn messages_round_trip_on_one_line() {
        let messages = [
            ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                name: "Ana".into(),
                proof: "abc_-123".into(),
            },
            ClientMsg::Add {
                uri: TRACK.into(),
                title: "Two\nlines".into(),
                artists: "A".into(),
                duration_ms: 5,
            },
            ClientMsg::Skip,
            ClientMsg::Ended { item: 3 },
            ClientMsg::Ping { t0: 7 },
        ];
        for message in messages {
            let line = encode(&message);
            assert_eq!(line.matches('\n').count(), 1, "{line}");
            assert!(line.ends_with('\n'));
            let decoded = decode_client(&line).unwrap();
            if let ClientMsg::Add { title, .. } = &decoded {
                assert_eq!(title, "Two lines");
            } else {
                assert_eq!(decoded, message);
            }
        }
        let server = ServerMsg::State { state: state() };
        assert_eq!(decode_server(&encode(&server)).unwrap(), server);
    }

    #[test]
    fn oversized_and_malformed_lines_are_refused_before_use() {
        let long = format!(
            "{{\"type\":\"add\",\"uri\":\"{TRACK}\",\"title\":\"{}\",\"artists\":\"\",\"duration_ms\":1}}",
            "x".repeat(MAX_LINE_BYTES)
        );
        assert_eq!(decode_client(&long), Err(ProtocolError::TooLong));
        assert_eq!(decode_client("{"), Err(ProtocolError::Malformed));
        assert_eq!(
            decode_client("{\"type\":\"format_disk\"}"),
            Err(ProtocolError::Malformed)
        );
    }

    #[test]
    fn only_spotify_song_links_are_accepted() {
        assert!(song_uri(TRACK.into()).is_ok());
        assert!(song_uri("spotify:episode:4uLU6hMCjMI75M1A2tKUQC".into()).is_ok());
        for uri in [
            "spotify:playlist:4uLU6hMCjMI75M1A2tKUQC",
            "spotify:local:artist:album:title:180",
            "https://evil.example/4uLU6hMCjMI75M1A2tKUQC",
            "spotify:track:4uLU6hMCjMI75M1A2tKUQ",
            "spotify:track:4uLU6hMCjMI75M1A2tKUQ/",
            "spotify:track:../../../../etc/passwdxxxx",
        ] {
            assert_eq!(
                song_uri(uri.into()),
                Err(ProtocolError::InvalidUri),
                "{uri}"
            );
        }
    }

    #[test]
    fn display_text_loses_controls_and_invisible_reordering() {
        assert_eq!(clean_text("  Ana \t\n Bel  ", 32), "Ana Bel");
        assert_eq!(clean_text("Ana\u{202E}ana", 32), "Anaana");
        assert_eq!(clean_text("a\u{200B}b\u{0007}c", 32), "abc");
        assert_eq!(clean_text("abcdef", 3), "abc");
        assert_eq!(clean_text("ab cd", 3), "ab");
        assert_eq!(clean_text("ééééé", 2), "éé");
        let hello = encode(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            name: "\u{200B}\u{202E} ".into(),
            proof: "abc".into(),
        });
        assert_eq!(decode_client(&hello), Err(ProtocolError::EmptyName));
    }

    #[test]
    fn a_hostile_server_state_is_bounded_and_checked() {
        let mut flood = state();
        flood.queue = (0..=MAX_QUEUE as u64).map(|id| item(id, TRACK)).collect();
        assert_eq!(
            decode_server(&encode(&ServerMsg::State { state: flood })),
            Err(ProtocolError::TooMany)
        );
        let mut bad = state();
        bad.queue[0].uri = "https://evil.example/x".into();
        assert_eq!(
            decode_server(&encode(&ServerMsg::State { state: bad })),
            Err(ProtocolError::InvalidUri)
        );
        let challenge = encode(&ServerMsg::Challenge {
            version: PROTOCOL_VERSION,
            nonce: "not a token!".into(),
        });
        assert_eq!(decode_server(&challenge), Err(ProtocolError::InvalidToken));
    }
}
