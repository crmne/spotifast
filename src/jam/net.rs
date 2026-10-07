//! The jam over TCP, on the backend's tokio runtime.
//!
//! The host listens at one address of this computer, never at every
//! address at once, and holds the [`HostSession`]. Each guest connection is
//! its own task: it runs the handshake, bounds and rate-limits what the
//! guest sends, and passes requests on to the host task, which alone
//! changes the jam and broadcasts its state.
//!
//! The channel is plain TCP: use it on a trusted network or through a
//! tunnel such as Tailscale, not with a port opened to the Internet. The
//! handshake keeps the secret off the wire, but nothing after it is hidden
//! or protected.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior, sleep, sleep_until, timeout};

use super::invite::{self, Invite, Secret};
use super::protocol::{
    self, ClientMsg, HOST_ID, HostMsg, JamState, MAX_LINE_BYTES, MAX_NAME_CHARS, PROTOCOL_VERSION,
    ParticipantId, Permissions, Refusal, Rejection, clean_text, encode,
};
use super::session::{ClockSync, Follower, HostSession, JamClock};

/// What the jam tells the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JamEvent {
    /// This computer hosts; `invite` is the code to share.
    Hosting {
        invite: Invite,
    },
    /// The host let this guest in.
    Joined {
        you: ParticipantId,
    },
    State(JamState),
    Refused(Refusal),
    /// The host's clock minus this computer's [`JamClock`], in milliseconds.
    ClockOffset(i64),
    /// The connection to the host dropped; trying again.
    Reconnecting {
        attempt: u32,
    },
    Ended(EndReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// This computer left, or stopped hosting.
    Left,
    /// The host ended the jam.
    HostClosed,
    Rejected(Rejection),
    /// Hosting could not start at the chosen address.
    CannotListen(String),
    /// The host stayed out of reach.
    Unreachable(String),
}

/// Where events go: the backend forwards them to the app.
pub type Sink = Arc<dyn Fn(JamEvent) + Send + Sync>;

/// Timings and limits. The defaults suit a jam; tests shorten them.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Connecting, and each step of the handshake.
    pub handshake: Duration,
    /// A guest silent this long is let go; guests ping well within it.
    pub guest_idle: Duration,
    /// A host silent this long is presumed gone.
    pub host_idle: Duration,
    pub ping_every: Duration,
    /// How often the host checks whether the song ended.
    pub tick_every: Duration,
    /// Connections at once, those still in the handshake included.
    pub max_connections: usize,
    /// Messages a guest may send at once, and per second after that.
    pub burst: u32,
    pub per_second: u32,
    pub reconnect_attempts: u32,
    pub reconnect_first: Duration,
    pub reconnect_max: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            guest_idle: Duration::from_secs(30),
            host_idle: Duration::from_secs(15),
            ping_every: Duration::from_secs(3),
            tick_every: Duration::from_millis(250),
            max_connections: 32,
            burst: 40,
            per_second: 20,
            reconnect_attempts: 8,
            reconnect_first: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(30),
        }
    }
}

pub struct HostConfig {
    pub name: String,
    /// One address of this computer, such as its LAN or Tailscale address.
    pub bind: IpAddr,
    /// Zero picks a free port.
    pub port: u16,
    pub limits: Limits,
}

enum Control {
    Request(ClientMsg),
    SetPermissions(Permissions),
    Stop,
}

/// A running jam, hosted or joined. Dropping it leaves the jam.
pub struct JamHandle {
    control: mpsc::UnboundedSender<Control>,
}

impl JamHandle {
    /// A request, applied by the host. A guest's request made while it is
    /// reconnecting is dropped.
    pub fn request(&self, message: ClientMsg) {
        let _ = self.control.send(Control::Request(message));
    }

    /// Host only; a guest ignores it.
    pub fn set_permissions(&self, permissions: Permissions) {
        let _ = self.control.send(Control::SetPermissions(permissions));
    }

    pub fn leave(self) {
        let _ = self.control.send(Control::Stop);
    }
}

/// What the app asks of the jam, through the backend.
pub enum JamCommand {
    Host {
        name: String,
        bind: IpAddr,
        port: u16,
        clock: JamClock,
    },
    Join {
        invite: Invite,
        name: String,
        clock: JamClock,
    },
    Request(ClientMsg),
    SetPermissions(Permissions),
    Leave,
}

/// The backend's one jam at a time. Hosting or joining ends whichever jam
/// was running.
#[derive(Default)]
pub struct JamRunner {
    handle: Option<JamHandle>,
}

impl JamRunner {
    /// Must run inside a tokio runtime.
    pub fn command(&mut self, command: JamCommand, sink: Sink) {
        match command {
            JamCommand::Host {
                name,
                bind,
                port,
                clock,
            } => {
                self.leave();
                let config = HostConfig {
                    name,
                    bind,
                    port,
                    limits: Limits::default(),
                };
                self.handle = Some(start_host(config, clock, sink));
            }
            JamCommand::Join {
                invite,
                name,
                clock,
            } => {
                self.leave();
                self.handle = Some(start_guest(invite, &name, clock, Limits::default(), sink));
            }
            JamCommand::Request(message) => {
                if let Some(handle) = &self.handle {
                    handle.request(message);
                }
            }
            JamCommand::SetPermissions(permissions) => {
                if let Some(handle) = &self.handle {
                    handle.set_permissions(permissions);
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

/// This computer's address on its main network, the one a guest on the
/// same network reaches it at. Connecting a UDP socket only picks a route:
/// nothing is sent.
pub fn lan_address() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// Starts hosting with a fresh secret. Must run inside a tokio runtime.
pub fn start_host(config: HostConfig, clock: JamClock, sink: Sink) -> JamHandle {
    let (control, commands) = mpsc::unbounded_channel();
    tokio::spawn(run_host(config, Secret::generate(), clock, sink, commands));
    JamHandle { control }
}

/// Joins the jam at `invite`. Must run inside a tokio runtime.
pub fn start_guest(
    invite: Invite,
    name: &str,
    clock: JamClock,
    limits: Limits,
    sink: Sink,
) -> JamHandle {
    let (control, commands) = mpsc::unbounded_channel();
    let mut name = clean_text(name, MAX_NAME_CHARS);
    if name.is_empty() {
        name = "Guest".into();
    }
    tokio::spawn(run_guest(invite, name, clock, limits, sink, commands));
    JamHandle { control }
}

/// What a guest connection tells the host task.
enum PeerEvent {
    Hello {
        hello: ClientMsg,
        nonce: String,
        outgoing: mpsc::Sender<String>,
        reply: oneshot::Sender<Result<(ParticipantId, JamState), Rejection>>,
    },
    Request {
        from: ParticipantId,
        message: ClientMsg,
    },
    Left(ParticipantId),
}

async fn run_host(
    config: HostConfig,
    secret: Secret,
    clock: JamClock,
    sink: Sink,
    mut control: mpsc::UnboundedReceiver<Control>,
) {
    if config.bind.is_unspecified() {
        sink(JamEvent::Ended(EndReason::CannotListen(
            "Choose one address of this computer rather than all of them.".into(),
        )));
        return;
    }
    let listener = match TcpListener::bind((config.bind, config.port)).await {
        Ok(listener) => listener,
        Err(error) => {
            sink(JamEvent::Ended(EndReason::CannotListen(error.to_string())));
            return;
        }
    };
    let port = match listener.local_addr() {
        Ok(address) => address.port(),
        Err(error) => {
            sink(JamEvent::Ended(EndReason::CannotListen(error.to_string())));
            return;
        }
    };
    sink(JamEvent::Hosting {
        invite: Invite {
            host: config.bind.to_string(),
            port,
            secret: secret.clone(),
        },
    });
    let limits = config.limits;
    let mut host = Host {
        session: HostSession::new(&config.name, clock.now_ms()),
        peers: HashMap::new(),
        clock,
        sink: sink.clone(),
    };
    host.broadcast();
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let (to_host, mut from_peers) = mpsc::channel(256);
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
                tokio::spawn(serve_guest(stream, slot, clock, limits, to_host.clone()));
            }
            Some(event) = from_peers.recv() => host.peer_event(event, &secret),
            command = control.recv() => match command {
                Some(Control::Request(message)) => host.request(HOST_ID, message),
                Some(Control::SetPermissions(permissions)) => {
                    host.session.set_permissions(permissions);
                    host.broadcast();
                }
                Some(Control::Stop) | None => break,
            },
            _ = tick.tick() => {
                if host.session.tick(clock.now_ms()) {
                    host.broadcast();
                }
            }
        }
    }
    // Each connection writes what it was given, then closes as its
    // channel goes.
    let closed = encode(&HostMsg::Closed);
    for peer in host.peers.values() {
        let _ = peer.try_send(closed.clone());
    }
    drop(host);
    sink(JamEvent::Ended(EndReason::Left));
}

struct Host {
    session: HostSession,
    peers: HashMap<ParticipantId, mpsc::Sender<String>>,
    clock: JamClock,
    sink: Sink,
}

impl Host {
    fn peer_event(&mut self, event: PeerEvent, secret: &Secret) {
        match event {
            PeerEvent::Hello {
                hello,
                nonce,
                outgoing,
                reply,
            } => match self.session.admit(&hello, secret, &nonce, b"") {
                Ok(you) => {
                    let state = self.session.state(self.clock.now_ms());
                    if reply.send(Ok((you, state))).is_ok() {
                        self.peers.insert(you, outgoing);
                    } else {
                        self.session.leave(you);
                    }
                    self.broadcast();
                }
                Err(rejection) => {
                    let _ = reply.send(Err(rejection));
                }
            },
            PeerEvent::Request { from, message } => self.request(from, message),
            PeerEvent::Left(you) => {
                self.peers.remove(&you);
                if self.session.leave(you) {
                    self.broadcast();
                }
            }
        }
    }

    fn request(&mut self, from: ParticipantId, message: ClientMsg) {
        match self.session.apply(from, message, self.clock.now_ms()) {
            Ok(()) => self.broadcast(),
            Err(reason) if from == HOST_ID => (self.sink)(JamEvent::Refused(reason)),
            Err(reason) => {
                if let Some(peer) = self.peers.get(&from) {
                    let _ = peer.try_send(encode(&HostMsg::Refused { reason }));
                }
            }
        }
    }

    fn broadcast(&mut self) {
        let state = self.session.state(self.clock.now_ms());
        let line = encode(&HostMsg::State {
            state: state.clone(),
        });
        // A guest too slow to keep up is let go rather than buffered
        // without end; its connection notices and leaves.
        self.peers
            .retain(|_, peer| peer.try_send(line.clone()).is_ok());
        (self.sink)(JamEvent::State(state));
    }
}

async fn serve_guest(
    stream: TcpStream,
    _slot: OwnedSemaphorePermit,
    clock: JamClock,
    limits: Limits,
    host: mpsc::Sender<PeerEvent>,
) {
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let mut lines = LineReader::new(read);
    let nonce = invite::new_nonce();
    let challenge = HostMsg::Challenge {
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
        outgoing,
        reply,
    };
    if host.send(hello).await.is_err() {
        return;
    }
    let you = match answer.await {
        Ok(Ok((you, state))) => {
            let welcome = encode(&HostMsg::Welcome { you, state });
            if write_line(&mut write, &welcome, limits).await.is_err() {
                let _ = host.send(PeerEvent::Left(you)).await;
                return;
            }
            you
        }
        Ok(Err(reason)) => {
            let rejected = encode(&HostMsg::Rejected { reason });
            let _ = write_line(&mut write, &rejected, limits).await;
            return;
        }
        Err(_) => return,
    };
    let mut bucket = TokenBucket::new(limits.burst, limits.per_second);
    let mut idle = Instant::now() + limits.guest_idle;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                idle = Instant::now() + limits.guest_idle;
                if !bucket.take(Instant::now()) {
                    break;
                }
                match protocol::decode_client(&line) {
                    Ok(ClientMsg::Ping { t0 }) => {
                        let pong = encode(&HostMsg::Pong {
                            t0,
                            host_time_ms: clock.now_ms(),
                        });
                        if write_line(&mut write, &pong, limits).await.is_err() {
                            break;
                        }
                    }
                    Ok(message) => {
                        if host.send(PeerEvent::Request { from: you, message }).await.is_err() {
                            break;
                        }
                    }
                    // A peer that does not speak the protocol is let go.
                    Err(_) => break,
                }
            }
            line = out.recv() => {
                let Some(line) = line else {
                    // The host is done with this guest. Closing with the
                    // guest's pings still unread would reset the connection,
                    // and a reset can discard the farewell before the guest
                    // reads it: say so first, then wait for the guest to go.
                    let _ = write.shutdown().await;
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
    let _ = host.send(PeerEvent::Left(you)).await;
}

enum Outcome {
    Left,
    HostClosed,
    Rejected(Rejection),
    Lost { joined: bool, reason: String },
}

async fn run_guest(
    invite: Invite,
    name: String,
    clock: JamClock,
    limits: Limits,
    sink: Sink,
    mut control: mpsc::UnboundedReceiver<Control>,
) {
    let mut attempt = 0;
    loop {
        let reason =
            match guest_connection(&invite, &name, clock, limits, &sink, &mut control).await {
                Outcome::Left => {
                    sink(JamEvent::Ended(EndReason::Left));
                    return;
                }
                Outcome::HostClosed => {
                    sink(JamEvent::Ended(EndReason::HostClosed));
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
                    Some(_) => {}
                },
            }
        }
    }
}

async fn guest_connection(
    invite: &Invite,
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
    let connect = TcpStream::connect((invite.host.as_str(), invite.port));
    let stream = match timeout(limits.handshake, connect).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return lost(false, &error.to_string()),
        Err(_) => return lost(false, "The host did not answer."),
    };
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let mut lines = LineReader::new(read);
    let nonce = match read_host(&mut lines, limits).await {
        Some(HostMsg::Challenge { nonce, .. }) => nonce,
        _ => return lost(false, "The host did not start the handshake."),
    };
    let hello = encode(&ClientMsg::Hello {
        version: PROTOCOL_VERSION,
        name: name.to_string(),
        proof: invite::proof(&invite.secret, &nonce, name, b""),
    });
    if write_line(&mut write, &hello, limits).await.is_err() {
        return lost(false, "The connection dropped during the handshake.");
    }
    let (you, state) = match read_host(&mut lines, limits).await {
        Some(HostMsg::Welcome { you, state }) => (you, state),
        Some(HostMsg::Rejected { reason }) => return Outcome::Rejected(reason),
        _ => return lost(false, "The host did not finish the handshake."),
    };
    sink(JamEvent::Joined { you });
    let mut follower = Follower::default();
    follower.receive(state.clone());
    sink(JamEvent::State(state));
    let mut clock_sync = ClockSync::default();
    let mut ping = tokio::time::interval(limits.ping_every);
    ping.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut idle = Instant::now() + limits.host_idle;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => return lost(true, "The host closed the connection."),
                    Err(error) => return lost(true, &error.to_string()),
                };
                idle = Instant::now() + limits.host_idle;
                match protocol::decode_host(&line) {
                    Ok(HostMsg::State { state }) => {
                        if follower.receive(state.clone()) {
                            sink(JamEvent::State(state));
                        }
                    }
                    Ok(HostMsg::Refused { reason }) => sink(JamEvent::Refused(reason)),
                    Ok(HostMsg::Pong { t0, host_time_ms }) => {
                        clock_sync.sample(t0, host_time_ms, clock.now_ms());
                        if let Some(offset) = clock_sync.offset() {
                            sink(JamEvent::ClockOffset(offset));
                        }
                    }
                    Ok(HostMsg::Closed) => return Outcome::HostClosed,
                    Ok(_) => {}
                    Err(_) => return lost(true, "The host sent something unexpected."),
                }
            }
            command = control.recv() => match command {
                Some(Control::Request(message)) => {
                    if write_line(&mut write, &encode(&message), limits).await.is_err() {
                        return lost(true, "The connection dropped.");
                    }
                }
                Some(Control::SetPermissions(_)) => {}
                Some(Control::Stop) | None => return Outcome::Left,
            },
            _ = ping.tick() => {
                let ping = encode(&ClientMsg::Ping { t0: clock.now_ms() });
                if write_line(&mut write, &ping, limits).await.is_err() {
                    return lost(true, "The connection dropped.");
                }
            }
            _ = sleep_until(idle) => return lost(true, "The host stopped answering."),
        }
    }
}

/// One handshake message from the host, in time and well formed.
async fn read_host<R: AsyncRead + Unpin>(
    lines: &mut LineReader<R>,
    limits: Limits,
) -> Option<HostMsg> {
    let line = timeout(limits.handshake, lines.next_line())
        .await
        .ok()?
        .ok()??;
    protocol::decode_host(&line).ok()
}

/// A peer that stops reading must not hold a writer forever.
async fn write_line<W: AsyncWrite + Unpin>(
    write: &mut W,
    line: &str,
    limits: Limits,
) -> io::Result<()> {
    timeout(limits.handshake, write.write_all(line.as_bytes()))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "write timed out"))?
}

/// Reads newline-terminated lines no longer than the protocol allows.
///
/// Cancel safe, unlike `read_line`: a line read in part waits in `line`
/// for the next call, so it can sit in a `select!`.
struct LineReader<R> {
    reader: BufReader<R>,
    line: Vec<u8>,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            line: Vec::new(),
        }
    }

    /// The next line, `None` at the end of the stream. A line over the
    /// limit is an error, found before it is held in full.
    async fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                return Ok(None);
            }
            let (taken, complete) = match available.iter().position(|&byte| byte == b'\n') {
                Some(end) => (end + 1, true),
                None => (available.len(), false),
            };
            // Room for the line, a carriage return and the newline.
            if self.line.len() + taken > MAX_LINE_BYTES + 2 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
            self.line.extend_from_slice(&available[..taken]);
            self.reader.consume(taken);
            if complete {
                let line = std::mem::take(&mut self.line);
                return String::from_utf8(line)
                    .map(Some)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"));
            }
        }
    }
}

/// Allows `burst` messages at once, then `per_second`.
struct TokenBucket {
    tokens: f64,
    burst: f64,
    per_second: f64,
    at: Instant,
}

impl TokenBucket {
    fn new(burst: u32, per_second: u32) -> Self {
        Self {
            tokens: f64::from(burst),
            burst: f64::from(burst),
            per_second: f64::from(per_second),
            at: Instant::now(),
        }
    }

    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::*;

    const SONG: &str = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";
    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    fn limits() -> Limits {
        Limits {
            handshake: Duration::from_millis(500),
            guest_idle: Duration::from_secs(2),
            host_idle: Duration::from_secs(2),
            ping_every: Duration::from_millis(100),
            tick_every: Duration::from_millis(50),
            reconnect_first: Duration::from_millis(50),
            reconnect_max: Duration::from_millis(200),
            ..Limits::default()
        }
    }

    fn sink() -> (Sink, mpsc::UnboundedReceiver<JamEvent>) {
        let (events, received) = mpsc::unbounded_channel();
        let sink: Sink = Arc::new(move |event| {
            let _ = events.send(event);
        });
        (sink, received)
    }

    /// The first event that `wanted` accepts, skipping the others.
    async fn until(
        events: &mut mpsc::UnboundedReceiver<JamEvent>,
        wanted: impl Fn(&JamEvent) -> bool,
    ) -> JamEvent {
        timeout(Duration::from_secs(5), async {
            loop {
                let event = events.recv().await.expect("the jam ended its events");
                if wanted(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("the awaited jam event never came")
    }

    async fn host(limits: Limits) -> (JamHandle, mpsc::UnboundedReceiver<JamEvent>, Invite) {
        let (sink, mut events) = sink();
        let config = HostConfig {
            name: "Host".into(),
            bind: LOOPBACK,
            port: 0,
            limits,
        };
        let handle = start_host(config, JamClock::new(), sink);
        let JamEvent::Hosting { invite } = until(&mut events, |event| {
            matches!(event, JamEvent::Hosting { .. })
        })
        .await
        else {
            unreachable!()
        };
        (handle, events, invite)
    }

    fn state_where(wanted: impl Fn(&JamState) -> bool) -> impl Fn(&JamEvent) -> bool {
        move |event| matches!(event, JamEvent::State(state) if wanted(state))
    }

    fn add() -> ClientMsg {
        ClientMsg::Add {
            uri: SONG.into(),
            title: "Song".into(),
            artists: "Artist".into(),
            duration_ms: 60_000,
        }
    }

    #[tokio::test]
    async fn a_guest_joins_adds_a_song_and_leaves() {
        let (host_handle, mut host_events, invite) = host(limits()).await;
        let (sink, mut guest_events) = sink();
        let guest = start_guest(invite, "Ana", JamClock::new(), limits(), sink);
        let JamEvent::Joined { you } = until(&mut guest_events, |event| {
            matches!(event, JamEvent::Joined { .. })
        })
        .await
        else {
            unreachable!()
        };
        until(
            &mut host_events,
            state_where(|state| state.participants.len() == 2),
        )
        .await;

        guest.request(add());
        until(
            &mut host_events,
            state_where(|state| state.current.is_some()),
        )
        .await;
        let JamEvent::State(state) = until(
            &mut guest_events,
            state_where(|state| state.current.is_some()),
        )
        .await
        else {
            unreachable!()
        };
        assert_eq!(state.current.unwrap().added_by, you);
        until(&mut guest_events, |event| {
            matches!(event, JamEvent::ClockOffset(_))
        })
        .await;

        // A refused request reaches only the guest that made it.
        guest.request(ClientMsg::Skip);
        assert_eq!(
            until(&mut guest_events, |event| matches!(
                event,
                JamEvent::Refused(_)
            ))
            .await,
            JamEvent::Refused(Refusal::NotAllowed)
        );

        guest.leave();
        assert_eq!(
            until(&mut guest_events, |event| matches!(
                event,
                JamEvent::Ended(_)
            ))
            .await,
            JamEvent::Ended(EndReason::Left)
        );
        until(
            &mut host_events,
            state_where(|state| state.participants.len() == 1),
        )
        .await;

        // The host's own requests apply directly.
        host_handle.request(ClientMsg::SetPlaying { playing: true });
        until(&mut host_events, state_where(|state| state.playing)).await;
        host_handle.leave();
        assert_eq!(
            until(&mut host_events, |event| matches!(
                event,
                JamEvent::Ended(_)
            ))
            .await,
            JamEvent::Ended(EndReason::Left)
        );
    }

    #[tokio::test]
    async fn a_wrong_secret_is_rejected_without_retrying() {
        let (_host, _host_events, mut invite) = host(limits()).await;
        invite.secret = Secret::generate();
        let (sink, mut events) = sink();
        let _guest = start_guest(invite, "Eve", JamClock::new(), limits(), sink);
        assert_eq!(
            until(&mut events, |event| matches!(event, JamEvent::Ended(_))).await,
            JamEvent::Ended(EndReason::Rejected(Rejection::BadInvite))
        );
    }

    #[tokio::test]
    async fn guests_learn_when_the_host_ends_the_jam() {
        let (host_handle, _host_events, invite) = host(limits()).await;
        let (sink, mut events) = sink();
        let _guest = start_guest(invite, "Ana", JamClock::new(), limits(), sink);
        until(&mut events, |event| {
            matches!(event, JamEvent::Joined { .. })
        })
        .await;
        host_handle.leave();
        assert_eq!(
            until(&mut events, |event| matches!(event, JamEvent::Ended(_))).await,
            JamEvent::Ended(EndReason::HostClosed)
        );
    }

    #[tokio::test]
    async fn hosting_on_every_address_at_once_is_refused() {
        let (sink, mut events) = sink();
        let config = HostConfig {
            name: "Host".into(),
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 0,
            limits: limits(),
        };
        let _host = start_host(config, JamClock::new(), sink);
        assert!(matches!(
            until(&mut events, |_| true).await,
            JamEvent::Ended(EndReason::CannotListen(_))
        ));
    }

    /// A raw connection that has read the challenge, and its nonce.
    async fn raw(
        invite: &Invite,
    ) -> (
        LineReader<tokio::net::tcp::OwnedReadHalf>,
        tokio::net::tcp::OwnedWriteHalf,
        String,
    ) {
        let stream = TcpStream::connect((invite.host.as_str(), invite.port))
            .await
            .unwrap();
        let (read, write) = stream.into_split();
        let mut lines = LineReader::new(read);
        let Some(HostMsg::Challenge { nonce, .. }) = read_host(&mut lines, limits()).await else {
            panic!("no challenge");
        };
        (lines, write, nonce)
    }

    /// Whether the host closed this connection within a few seconds.
    async fn closed<R: AsyncRead + Unpin>(lines: &mut LineReader<R>) -> bool {
        timeout(Duration::from_secs(5), async {
            loop {
                match lines.next_line().await {
                    Ok(Some(_)) => continue,
                    _ => return true,
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    #[tokio::test]
    async fn abusive_connections_are_dropped_and_the_jam_carries_on() {
        let (_host, mut host_events, invite) = host(limits()).await;

        // A line longer than the protocol allows.
        let (mut lines, mut write, _) = raw(&invite).await;
        let _ = write.write_all(&vec![b'x'; MAX_LINE_BYTES + 10]).await;
        assert!(
            closed(&mut lines).await,
            "an oversized line keeps the connection"
        );

        // No hello within the handshake time.
        let (mut lines, _write, _) = raw(&invite).await;
        assert!(closed(&mut lines).await, "a silent connection is kept");

        // Something that is not the protocol.
        let (mut lines, mut write, _) = raw(&invite).await;
        write.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        assert!(closed(&mut lines).await, "garbage keeps the connection");

        // A guest that floods once admitted.
        let (mut lines, mut write, nonce) = raw(&invite).await;
        let hello = encode(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            name: "Flood".into(),
            proof: invite::proof(&invite.secret, &nonce, "Flood", b""),
        });
        write.write_all(hello.as_bytes()).await.unwrap();
        until(
            &mut host_events,
            state_where(|state| state.participants.len() == 2),
        )
        .await;
        let ping = encode(&ClientMsg::Ping { t0: 0 }).repeat(200);
        let _ = write.write_all(ping.as_bytes()).await;
        assert!(closed(&mut lines).await, "a flood keeps the connection");
        until(
            &mut host_events,
            state_where(|state| state.participants.len() == 1),
        )
        .await;

        // The jam still lets a proper guest in.
        let (sink, mut events) = sink();
        let _guest = start_guest(invite, "Ana", JamClock::new(), limits(), sink);
        until(&mut events, |event| {
            matches!(event, JamEvent::Joined { .. })
        })
        .await;
    }

    #[tokio::test]
    async fn connections_past_the_cap_are_closed_at_once() {
        let (_host, _events, invite) = host(Limits {
            max_connections: 2,
            handshake: Duration::from_secs(5),
            ..limits()
        })
        .await;
        let _first = raw(&invite).await;
        let _second = raw(&invite).await;
        let mut third = TcpStream::connect((invite.host.as_str(), invite.port))
            .await
            .unwrap();
        let mut byte = [0; 1];
        let read = timeout(Duration::from_secs(5), third.read(&mut byte)).await;
        assert!(
            matches!(read, Ok(Ok(0)) | Ok(Err(_))),
            "the third connection got {read:?}"
        );
    }

    #[tokio::test]
    async fn a_guest_reconnects_after_losing_the_host_and_can_still_leave() {
        // A host that admits anyone and then drops the connection.
        let listener = TcpListener::bind((LOOPBACK, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (read, mut write) = stream.into_split();
                let mut lines = LineReader::new(read);
                let challenge = encode(&HostMsg::Challenge {
                    version: PROTOCOL_VERSION,
                    nonce: invite::new_nonce(),
                });
                let _ = write.write_all(challenge.as_bytes()).await;
                let _ = lines.next_line().await;
                let state = HostSession::new("Host", 0).state(0);
                let welcome = encode(&HostMsg::Welcome { you: 1, state });
                let _ = write.write_all(welcome.as_bytes()).await;
            }
        });
        let invite = Invite {
            host: LOOPBACK.to_string(),
            port,
            secret: Secret::generate(),
        };
        let (sink, mut events) = sink();
        let guest = start_guest(invite, "Ana", JamClock::new(), limits(), sink);
        until(&mut events, |event| {
            matches!(event, JamEvent::Joined { .. })
        })
        .await;
        assert_eq!(
            until(&mut events, |event| matches!(
                event,
                JamEvent::Reconnecting { .. }
            ))
            .await,
            JamEvent::Reconnecting { attempt: 1 }
        );
        // Each reconnection was admitted, so the count starts over.
        until(&mut events, |event| {
            matches!(event, JamEvent::Joined { .. })
        })
        .await;
        assert_eq!(
            until(&mut events, |event| matches!(
                event,
                JamEvent::Reconnecting { .. }
            ))
            .await,
            JamEvent::Reconnecting { attempt: 1 }
        );
        guest.leave();
        assert_eq!(
            until(&mut events, |event| matches!(event, JamEvent::Ended(_))).await,
            JamEvent::Ended(EndReason::Left)
        );
    }

    #[tokio::test]
    async fn an_unreachable_host_ends_the_jam_after_the_last_attempt() {
        // A port that was free a moment ago: nothing listens there.
        let port = TcpListener::bind((LOOPBACK, 0))
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let invite = Invite {
            host: LOOPBACK.to_string(),
            port,
            secret: Secret::generate(),
        };
        let (sink, mut events) = sink();
        let _guest = start_guest(
            invite,
            "Ana",
            JamClock::new(),
            Limits {
                reconnect_attempts: 2,
                ..limits()
            },
            sink,
        );
        for attempt in 1..=2 {
            assert_eq!(
                until(&mut events, |_| true).await,
                JamEvent::Reconnecting { attempt }
            );
        }
        assert!(matches!(
            until(&mut events, |_| true).await,
            JamEvent::Ended(EndReason::Unreachable(_))
        ));
    }

    #[tokio::test]
    async fn the_line_reader_survives_cancellation_mid_line() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut lines = LineReader::new(server);
        client.write_all(b"{\"type\":").await.unwrap();
        // The read is abandoned with half a line taken.
        assert!(
            timeout(Duration::from_millis(50), lines.next_line())
                .await
                .is_err()
        );
        client.write_all(b"\"skip\"}\n").await.unwrap();
        assert_eq!(
            lines.next_line().await.unwrap().as_deref(),
            Some("{\"type\":\"skip\"}\n")
        );
    }

    #[test]
    fn the_bucket_allows_a_burst_then_a_steady_rate() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(3, 10);
        bucket.at = start;
        assert!((0..3).all(|_| bucket.take(start)));
        assert!(!bucket.take(start));
        assert!(bucket.take(start + Duration::from_millis(100)));
        assert!(!bucket.take(start + Duration::from_millis(100)));
    }
}
