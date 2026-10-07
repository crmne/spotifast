//! Keeps one jam running for every listener who has the server code.
//!
//! The server owns the jam's queue, its playing song and that song's
//! position on the server's clock. Every listener connects over TLS, proves
//! it knows the password, and then has the same rights as every other.
//! Each connection is its own task: it runs the handshakes, bounds and
//! rate-limits what the listener sends, and passes requests on to the one
//! task that changes the jam and broadcasts its state.
//!
//! The data directory holds the password (`secret`), the certificate and
//! its key (`cert.pem`, `key.pem`), and the jam itself (`state.json`), so a
//! restart keeps the queue.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jam_core::auth::{self, BINDING_BYTES, BINDING_LABEL, Fingerprint, Secret};
use jam_core::protocol::{
    self, ClientMsg, JamState, PROTOCOL_VERSION, ParticipantId, Rejection, ServerMsg, encode,
};
use jam_core::session::{JamClock, JamSession, Saved};
use jam_core::wire::{Limits, LineReader, TokenBucket, write_line};
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior, sleep, sleep_until, timeout};
use tokio_rustls::TlsAcceptor;

/// How often, at most, a changed jam is written to disk.
const SAVE_EVERY: Duration = Duration::from_secs(1);

/// The files in the data directory.
pub struct DataDir(PathBuf);

impl DataDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub fn secret(&self) -> PathBuf {
        self.0.join("secret")
    }

    pub fn certificate(&self) -> PathBuf {
        self.0.join("cert.pem")
    }

    pub fn key(&self) -> PathBuf {
        self.0.join("key.pem")
    }

    pub fn state(&self) -> PathBuf {
        self.0.join("state.json")
    }

    /// The password, drawn and kept on first use.
    pub fn load_or_create_secret(&self) -> io::Result<Secret> {
        match std::fs::read_to_string(self.secret()) {
            Ok(text) => Secret::from_text(&text).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "the secret file is damaged")
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&self.0)?;
                let secret = Secret::generate();
                write_private(&self.secret(), secret.to_text().as_bytes())?;
                Ok(secret)
            }
            Err(error) => Err(error),
        }
    }

    /// The TLS configuration, and the certificate's fingerprint for the
    /// server code.
    pub fn load_tls(&self) -> Result<(Arc<ServerConfig>, Fingerprint), String> {
        let missing = |path: &Path, error: &dyn std::fmt::Display| {
            format!(
                "Cannot read {}: {error}. Create the certificate with:\n  openssl req -x509 \
                 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
                 -subj /CN=spotifast-jam -keyout {} -out {}",
                path.display(),
                self.key().display(),
                self.certificate().display()
            )
        };
        let certificates = CertificateDer::pem_file_iter(self.certificate())
            .map_err(|error| missing(&self.certificate(), &error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| missing(&self.certificate(), &error))?;
        let key = PrivateKeyDer::from_pem_file(self.key())
            .map_err(|error| missing(&self.key(), &error))?;
        tls_config(certificates, key)
    }

    /// The code to give listeners, `password#fingerprint`. They enter the
    /// server's address beside it themselves.
    pub fn server_code(&self) -> Result<String, String> {
        let secret = self
            .load_or_create_secret()
            .map_err(|error| error.to_string())?;
        let (_, fingerprint) = self.load_tls()?;
        Ok(auth::access_code(&secret, fingerprint))
    }
}

/// A TLS configuration from a certificate chain and its key.
pub fn tls_config(
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<(Arc<ServerConfig>, Fingerprint), String> {
    let fingerprint = Fingerprint::of(
        certificates
            .first()
            .ok_or("the certificate file holds no certificate")?
            .as_ref(),
    );
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .map_err(|error| error.to_string())?;
    Ok((Arc::new(config), fingerprint))
}

/// Written beside its destination, then moved over it, so a crash never
/// leaves half a file. Readable by its owner only, where that is possible.
fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    let partial = path.with_extension("partial");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&partial)?;
        io::Write::write_all(&mut file, contents)?;
        file.sync_all()?;
    }
    std::fs::rename(&partial, path)
}

/// A running server.
pub struct Running {
    pub address: SocketAddr,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    /// Saves the jam and closes every connection.
    pub async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

/// Starts serving at `listen`. With `state`, the jam is read from that file
/// and kept there.
pub async fn start(
    listen: SocketAddr,
    tls: Arc<ServerConfig>,
    secret: Secret,
    state: Option<PathBuf>,
    limits: Limits,
) -> io::Result<Running> {
    let listener = TcpListener::bind(listen).await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(run(
        listener,
        TlsAcceptor::from(tls),
        secret,
        state,
        limits,
        stopped,
    ));
    Ok(Running {
        address,
        stop,
        task,
    })
}

/// What a connection tells the jam task.
enum PeerEvent {
    Hello {
        hello: ClientMsg,
        nonce: String,
        binding: [u8; BINDING_BYTES],
        outgoing: mpsc::Sender<String>,
        reply: oneshot::Sender<Result<(ParticipantId, JamState), Rejection>>,
    },
    Request {
        from: ParticipantId,
        message: ClientMsg,
    },
    Left(ParticipantId),
}

async fn run(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    secret: Secret,
    state: Option<PathBuf>,
    limits: Limits,
    mut stopped: oneshot::Receiver<()>,
) {
    let clock = JamClock::new();
    let saved = state.as_deref().map(load_saved).unwrap_or_default();
    let mut server = Server {
        session: JamSession::restore(saved, clock.now_ms()),
        peers: HashMap::new(),
        clock,
        state,
        dirty: false,
        saved_at: Instant::now(),
    };
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let (to_server, mut from_peers) = mpsc::channel(256);
    let mut tick = tokio::time::interval(limits.tick_every);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    // Out of descriptors, say: wait instead of spinning.
                    sleep(Duration::from_millis(100)).await;
                    continue;
                };
                // Past the cap the connection is closed at once.
                let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
                    continue;
                };
                tokio::spawn(serve(
                    stream,
                    slot,
                    acceptor.clone(),
                    clock,
                    limits,
                    to_server.clone(),
                ));
            }
            Some(event) = from_peers.recv() => server.peer_event(event, &secret),
            _ = tick.tick() => {
                if server.session.tick(clock.now_ms()) {
                    server.broadcast();
                }
                server.save(false);
            }
            _ = &mut stopped => break,
        }
    }
    server.save(true);
}

/// The jam as saved last, or a new one when there is none or it cannot be
/// read: a damaged file must not keep the server down.
fn load_saved(path: &Path) -> Saved {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

struct Server {
    session: JamSession,
    peers: HashMap<ParticipantId, mpsc::Sender<String>>,
    clock: JamClock,
    state: Option<PathBuf>,
    dirty: bool,
    saved_at: Instant,
}

impl Server {
    fn peer_event(&mut self, event: PeerEvent, secret: &Secret) {
        match event {
            PeerEvent::Hello {
                hello,
                nonce,
                binding,
                outgoing,
                reply,
            } => match self.session.admit(&hello, secret, &nonce, &binding) {
                Ok(you) => {
                    let state = self.session.state(self.clock.now_ms());
                    if reply.send(Ok((you, state))).is_ok() {
                        self.peers.insert(you, outgoing);
                    } else {
                        self.session.leave(you, self.clock.now_ms());
                    }
                    self.broadcast();
                }
                Err(rejection) => {
                    let _ = reply.send(Err(rejection));
                }
            },
            PeerEvent::Request { from, message } => {
                // Every listener reports the same ending; all but the first
                // name a song already gone, which needs no answer.
                let report = matches!(message, ClientMsg::Ended { .. });
                match self.session.apply(from, message, self.clock.now_ms()) {
                    Ok(()) => self.broadcast(),
                    Err(_) if report => {}
                    Err(reason) => {
                        if let Some(peer) = self.peers.get(&from) {
                            let _ = peer.try_send(encode(&ServerMsg::Refused { reason }));
                        }
                    }
                }
            }
            PeerEvent::Left(you) => {
                self.peers.remove(&you);
                if self.session.leave(you, self.clock.now_ms()) {
                    self.broadcast();
                }
            }
        }
    }

    fn broadcast(&mut self) {
        let state = self.session.state(self.clock.now_ms());
        let line = encode(&ServerMsg::State { state });
        // A listener too slow to keep up is let go rather than buffered
        // without end; its connection notices and leaves.
        self.peers
            .retain(|_, peer| peer.try_send(line.clone()).is_ok());
        self.dirty = true;
    }

    /// Keeps the jam on disk, at most once a second unless `now`.
    fn save(&mut self, now: bool) {
        let Some(path) = &self.state else {
            return;
        };
        if !self.dirty || (!now && self.saved_at.elapsed() < SAVE_EVERY) {
            return;
        }
        let saved = self.session.save(self.clock.now_ms());
        let text = serde_json::to_vec_pretty(&saved).expect("a jam always serializes");
        match write_private(path, &text) {
            Ok(()) => {
                self.dirty = false;
                self.saved_at = Instant::now();
            }
            Err(error) => eprintln!("jam-server: cannot save {}: {error}", path.display()),
        }
    }
}

async fn serve(
    stream: TcpStream,
    _slot: OwnedSemaphorePermit,
    acceptor: TlsAcceptor,
    clock: JamClock,
    limits: Limits,
    server: mpsc::Sender<PeerEvent>,
) {
    let _ = stream.set_nodelay(true);
    let Ok(Ok(tls)) = timeout(limits.handshake, acceptor.accept(stream)).await else {
        return;
    };
    let Ok(binding) =
        tls.get_ref()
            .1
            .export_keying_material([0; BINDING_BYTES], BINDING_LABEL, None)
    else {
        return;
    };
    let (read, mut write) = tokio::io::split(tls);
    let mut lines = LineReader::new(read);
    let nonce = auth::new_nonce();
    let challenge = ServerMsg::Challenge {
        version: PROTOCOL_VERSION,
        nonce: nonce.clone(),
    };
    if write_line(&mut write, &encode(&challenge), limits)
        .await
        .is_err()
    {
        return;
    }
    let hello = match timeout(limits.handshake, lines.next_line()).await {
        Ok(Ok(Some(line))) => match protocol::decode_client(&line) {
            Ok(hello @ ClientMsg::Hello { .. }) => hello,
            _ => return,
        },
        _ => return,
    };
    let (outgoing, mut out) = mpsc::channel(64);
    let (reply, answer) = oneshot::channel();
    let hello = PeerEvent::Hello {
        hello,
        nonce,
        binding,
        outgoing,
        reply,
    };
    if server.send(hello).await.is_err() {
        return;
    }
    let you = match answer.await {
        Ok(Ok((you, state))) => {
            let welcome = encode(&ServerMsg::Welcome { you, state });
            if write_line(&mut write, &welcome, limits).await.is_err() {
                let _ = server.send(PeerEvent::Left(you)).await;
                return;
            }
            you
        }
        Ok(Err(reason)) => {
            let rejected = encode(&ServerMsg::Rejected { reason });
            let _ = write_line(&mut write, &rejected, limits).await;
            let _ = timeout(limits.handshake, write.shutdown()).await;
            return;
        }
        Err(_) => return,
    };
    let mut bucket = TokenBucket::new(limits.burst, limits.per_second);
    let mut idle = Instant::now() + limits.listener_idle;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                idle = Instant::now() + limits.listener_idle;
                if !bucket.take(Instant::now()) {
                    break;
                }
                match protocol::decode_client(&line) {
                    Ok(ClientMsg::Ping { t0 }) => {
                        let pong = encode(&ServerMsg::Pong {
                            t0,
                            server_time_ms: clock.now_ms(),
                        });
                        if write_line(&mut write, &pong, limits).await.is_err() {
                            break;
                        }
                    }
                    Ok(message) => {
                        if server.send(PeerEvent::Request { from: you, message }).await.is_err() {
                            break;
                        }
                    }
                    // A peer that does not speak the protocol is let go.
                    Err(_) => break,
                }
            }
            line = out.recv() => {
                let Some(line) = line else {
                    // The server is done with this listener: close the TLS
                    // session properly, then give the listener a moment to go.
                    let _ = timeout(limits.handshake, write.shutdown()).await;
                    let _ = timeout(limits.handshake, async {
                        while let Ok(Some(_)) = lines.next_line().await {}
                    })
                    .await;
                    break;
                };
                if write_line(&mut write, &line, limits).await.is_err() {
                    break;
                }
            }
            _ = sleep_until(idle) => break,
        }
    }
    let _ = server.send(PeerEvent::Left(you)).await;
}
