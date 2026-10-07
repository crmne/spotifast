//! A listener's connection to the jam server, on a tokio runtime.
//!
//! The connection is TLS, and the server must show the certificate whose
//! fingerprint the server code names: no other is accepted, whoever signed
//! it. The listener then proves it knows the password, follows the states
//! the server broadcasts, measures the server's clock, and reconnects on its
//! own when the connection drops.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::io::AsyncRead;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior, sleep_until, timeout};
use tokio_rustls::TlsConnector;

use crate::auth::{self, BINDING_BYTES, BINDING_LABEL, Fingerprint, ServerCode};
use crate::protocol::{
    self, ClientMsg, JamState, MAX_NAME_CHARS, PROTOCOL_VERSION, ParticipantId, Refusal, Rejection,
    ServerMsg, clean_text, encode,
};
use crate::session::{ClockSync, Follower, JamClock};
use crate::wire::{Limits, LineReader, write_line};

/// What the connection tells the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JamEvent {
    /// The server let this listener in.
    Joined {
        you: ParticipantId,
    },
    State(JamState),
    Refused(Refusal),
    /// The server's clock minus this computer's [`JamClock`], in
    /// milliseconds.
    ClockOffset(i64),
    /// The connection dropped; trying again.
    Reconnecting {
        attempt: u32,
    },
    Ended(EndReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// This listener left.
    Left,
    Rejected(Rejection),
    /// The server stayed out of reach, or did not show its certificate.
    Unreachable(String),
}

/// Where events go: the app's backend forwards them to the interface.
pub type Sink = Arc<dyn Fn(JamEvent) + Send + Sync>;

enum Control {
    Request(ClientMsg),
    Stop,
}

/// A joined jam. Dropping it leaves the jam.
pub struct JamHandle {
    control: mpsc::UnboundedSender<Control>,
}

impl JamHandle {
    /// A request for the server. One made while reconnecting is dropped;
    /// the app keeps its additions to send again.
    pub fn request(&self, message: ClientMsg) {
        let _ = self.control.send(Control::Request(message));
    }

    pub fn leave(self) {
        let _ = self.control.send(Control::Stop);
    }
}

/// Joins the jam the server code names. Must run inside a tokio runtime.
pub fn join(
    code: ServerCode,
    name: &str,
    clock: JamClock,
    limits: Limits,
    sink: Sink,
) -> JamHandle {
    let (control, commands) = mpsc::unbounded_channel();
    let mut name = clean_text(name, MAX_NAME_CHARS);
    if name.is_empty() {
        name = "Listener".into();
    }
    tokio::spawn(run(code, name, clock, limits, sink, commands));
    JamHandle { control }
}

enum Outcome {
    Left,
    Rejected(Rejection),
    Lost { joined: bool, reason: String },
}

async fn run(
    code: ServerCode,
    name: String,
    clock: JamClock,
    limits: Limits,
    sink: Sink,
    mut control: mpsc::UnboundedReceiver<Control>,
) {
    let connector = TlsConnector::from(pinned_config(code.fingerprint));
    let mut attempt = 0;
    loop {
        let outcome =
            connection(&connector, &code, &name, clock, limits, &sink, &mut control).await;
        let reason = match outcome {
            Outcome::Left => {
                sink(JamEvent::Ended(EndReason::Left));
                return;
            }
            Outcome::Rejected(reason) => {
                sink(JamEvent::Ended(EndReason::Rejected(reason)));
                return;
            }
            Outcome::Lost { joined, reason } => {
                if joined {
                    attempt = 0;
                }
                reason
            }
        };
        attempt += 1;
        if attempt > limits.reconnect_attempts {
            sink(JamEvent::Ended(EndReason::Unreachable(reason)));
            return;
        }
        sink(JamEvent::Reconnecting { attempt });
        let wait = limits
            .reconnect_first
            .saturating_mul(1 << (attempt - 1).min(16))
            .min(limits.reconnect_max);
        let deadline = Instant::now() + wait;
        loop {
            tokio::select! {
                _ = sleep_until(deadline) => break,
                command = control.recv() => match command {
                    Some(Control::Stop) | None => {
                        sink(JamEvent::Ended(EndReason::Left));
                        return;
                    }
                    Some(Control::Request(_)) => {}
                },
            }
        }
    }
}

async fn connection(
    connector: &TlsConnector,
    code: &ServerCode,
    name: &str,
    clock: JamClock,
    limits: Limits,
    sink: &Sink,
    control: &mut mpsc::UnboundedReceiver<Control>,
) -> Outcome {
    let lost = |joined: bool, reason: &str| Outcome::Lost {
        joined,
        reason: reason.to_string(),
    };
    let connect = TcpStream::connect((code.host.as_str(), code.port));
    let stream = match timeout(limits.handshake, connect).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return lost(false, &error.to_string()),
        Err(_) => return lost(false, "The server did not answer."),
    };
    let _ = stream.set_nodelay(true);
    // The name only matters to the TLS handshake; the fingerprint decides.
    let server_name = ServerName::try_from(code.host.clone())
        .unwrap_or_else(|_| ServerName::try_from("spotifast-jam").expect("a valid name"));
    let tls = match timeout(limits.handshake, connector.connect(server_name, stream)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(error)) => return lost(false, &error.to_string()),
        Err(_) => return lost(false, "The server did not finish the secure handshake."),
    };
    let Ok(binding) =
        tls.get_ref()
            .1
            .export_keying_material([0; BINDING_BYTES], BINDING_LABEL, None)
    else {
        return lost(false, "The secure connection could not be bound.");
    };
    let (read, mut write) = tokio::io::split(tls);
    let mut lines = LineReader::new(read);
    let nonce = match read_server(&mut lines, limits).await {
        Some(ServerMsg::Challenge { nonce, .. }) => nonce,
        _ => return lost(false, "The server did not start the handshake."),
    };
    let hello = encode(&ClientMsg::Hello {
        version: PROTOCOL_VERSION,
        name: name.to_string(),
        proof: auth::proof(&code.secret, &nonce, name, &binding),
    });
    if write_line(&mut write, &hello, limits).await.is_err() {
        return lost(false, "The connection dropped during the handshake.");
    }
    let (you, state) = match read_server(&mut lines, limits).await {
        Some(ServerMsg::Welcome { you, state }) => (you, state),
        Some(ServerMsg::Rejected { reason }) => return Outcome::Rejected(reason),
        _ => return lost(false, "The server did not finish the handshake."),
    };
    sink(JamEvent::Joined { you });
    let mut follower = Follower::default();
    follower.receive(state.clone());
    sink(JamEvent::State(state));
    let mut clock_sync = ClockSync::default();
    let mut ping = tokio::time::interval(limits.ping_every);
    ping.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut idle = Instant::now() + limits.server_idle;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => return lost(true, "The server closed the connection."),
                    Err(error) => return lost(true, &error.to_string()),
                };
                idle = Instant::now() + limits.server_idle;
                match protocol::decode_server(&line) {
                    Ok(ServerMsg::State { state }) => {
                        if follower.receive(state.clone()) {
                            sink(JamEvent::State(state));
                        }
                    }
                    Ok(ServerMsg::Refused { reason }) => sink(JamEvent::Refused(reason)),
                    Ok(ServerMsg::Pong { t0, server_time_ms }) => {
                        clock_sync.sample(t0, server_time_ms, clock.now_ms());
                        if let Some(offset) = clock_sync.offset() {
                            sink(JamEvent::ClockOffset(offset));
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return lost(true, "The server sent something unexpected."),
                }
            }
            command = control.recv() => match command {
                Some(Control::Request(message)) => {
                    if write_line(&mut write, &encode(&message), limits).await.is_err() {
                        return lost(true, "The connection dropped.");
                    }
                }
                Some(Control::Stop) | None => return Outcome::Left,
            },
            _ = ping.tick() => {
                let ping = encode(&ClientMsg::Ping { t0: clock.now_ms() });
                if write_line(&mut write, &ping, limits).await.is_err() {
                    return lost(true, "The connection dropped.");
                }
            }
            _ = sleep_until(idle) => return lost(true, "The server stopped answering."),
        }
    }
}

/// One handshake message from the server, in time and well formed.
async fn read_server<R: AsyncRead + Unpin>(
    lines: &mut LineReader<R>,
    limits: Limits,
) -> Option<ServerMsg> {
    let line = timeout(limits.handshake, lines.next_line())
        .await
        .ok()?
        .ok()??;
    protocol::decode_server(&line).ok()
}

/// A TLS client that accepts the one certificate `fingerprint` names.
pub fn pinned_config(fingerprint: Fingerprint) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint,
            provider,
        }))
        .with_no_client_auth();
    Arc::new(config)
}

/// Trusts a certificate by its fingerprint alone: the server code is where
/// that trust comes from, so no certificate authority or name is needed.
/// Signatures are still checked, so only the holder of the key passes.
#[derive(Debug)]
struct Pinned {
    fingerprint: Fingerprint,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if Fingerprint::of(end_entity.as_ref()) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the server's certificate does not match the server code".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
