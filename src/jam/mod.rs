//! Jams: listening together between Spotifast instances, without Spotify's
//! own Jam service.
//!
//! One instance hosts and owns the shared queue; guests send it requests and
//! follow the state it broadcasts. No audio ever crosses the wire: only song
//! URIs, positions and timestamps. Every participant plays the songs on their
//! own Spotify account through librespot.
//!
//! This module holds the parts that need neither a socket nor a player, so
//! they can be tested on their own:
//!
//! - [`protocol`]: the messages, their size limits and their validation.
//!   Everything that arrives from a peer is untrusted, the host included.
//! - [`invite`]: the invitation code, its random secret, and the handshake
//!   proof that shows a guest knows that secret without sending it.
//! - [`session`]: the host's authoritative state machine and the guest's
//!   reconciliation of that state with its local playback.

pub mod invite;
pub mod protocol;
pub mod session;
