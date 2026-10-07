//! Jams: listening together through a jam server, without Spotify's own
//! Jam service.
//!
//! A jam server keeps one shared queue running; every listener connects to
//! it with the server code, sends it requests, and follows the state it
//! broadcasts. No audio ever crosses the wire: only song URIs, positions and
//! timestamps. Every listener plays the songs on their own Spotify account
//! through librespot, and every listener has the same rights.
//!
//! The protocol, the state machines and the connection live in the
//! `jam-core` crate, shared with the server and re-exported here. [`net`]
//! runs the connection on the backend's runtime, and [`view`] is what the
//! app knows of the jam from the connection's events.

pub use jam_core::{auth, protocol, session, sync};

pub mod net;
pub mod view;
