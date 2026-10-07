//! The backend's side of the jam: one connection to the jam server at a
//! time, on the backend's tokio runtime.

use jam_core::auth::ServerCode;
use jam_core::client::{self, JamHandle};
use jam_core::protocol::ClientMsg;
use jam_core::session::JamClock;
use jam_core::wire::Limits;

pub use jam_core::client::{EndReason, JamEvent, Sink};

/// What the app asks of the jam, through the backend.
pub enum JamCommand {
    Join {
        code: ServerCode,
        name: String,
        clock: JamClock,
    },
    Request(ClientMsg),
    Leave,
}

/// The backend's one jam connection. Joining ends whichever was running.
#[derive(Default)]
pub struct JamRunner {
    handle: Option<JamHandle>,
}

impl JamRunner {
    /// Must run inside a tokio runtime.
    pub fn command(&mut self, command: JamCommand, sink: Sink) {
        match command {
            JamCommand::Join { code, name, clock } => {
                self.leave();
                self.handle = Some(client::join(code, &name, clock, Limits::default(), sink));
            }
            JamCommand::Request(message) => {
                if let Some(handle) = &self.handle {
                    handle.request(message);
                }
            }
            JamCommand::Leave => self.leave(),
        }
    }

    fn leave(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.leave();
        }
    }
}
