//! The core of Spotizgeg's jams, shared by the app and the jam server.
//!
//! A jam is one shared queue that a server keeps running, and that every
//! listener follows on their own Spotify account. No audio ever crosses the
//! wire: only song links, positions and timestamps. Every listener has the
//! same rights over the jam.
//!
//! - [`protocol`]: the messages, their size limits and their validation.
//!   Everything that arrives from a peer is untrusted, the server included.
//! - [`auth`]: the server code, and the password proof bound to the TLS
//!   session.
//! - [`session`]: the server's state machine, and how a listener reconciles
//!   that state with its own playback.
//! - [`sync`]: keeps a listener's player on the jam without fighting it.
//! - [`wire`]: bounded lines, writes that cannot hang, and rate limits.
//! - [`client`]: a listener's connection to the server, over pinned TLS.

pub mod auth;
pub mod client;
pub mod protocol;
pub mod session;
pub mod sync;
pub mod wire;
