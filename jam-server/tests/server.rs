//! The server and the app's own client, over real TLS on loopback.
//!
//! `fixtures/cert.pem` and `fixtures/key.pem` are a throwaway self-signed
//! certificate made for these tests only.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use jam_core::auth::{self, BINDING_BYTES, BINDING_LABEL, Fingerprint, Secret, ServerCode};
use jam_core::client::{self, EndReason, JamEvent, Sink};
use jam_core::protocol::{
    ClientMsg, JamState, MAX_LINE_BYTES, PROTOCOL_VERSION, Rejection, ServerMsg, decode_server,
    encode,
};
use jam_core::session::JamClock;
use jam_core::wire::{Limits, LineReader};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

const SONG: &str = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

fn tls() -> (Arc<rustls::ServerConfig>, Fingerprint) {
    let certificates = CertificateDer::pem_slice_iter(include_bytes!("fixtures/cert.pem"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("fixtures/key.pem")).unwrap();
    jam_server::tls_config(certificates, key).unwrap()
}

fn limits() -> Limits {
    Limits {
        handshake: Duration::from_millis(800),
        listener_idle: Duration::from_secs(2),
        server_idle: Duration::from_secs(2),
        ping_every: Duration::from_millis(100),
        tick_every: Duration::from_millis(50),
        reconnect_first: Duration::from_millis(50),
        reconnect_max: Duration::from_millis(200),
        ..Limits::default()
    }
}

struct Server {
    running: jam_server::Running,
    code: ServerCode,
}

async fn server_with(limits: Limits, state: Option<std::path::PathBuf>, port: u16) -> Server {
    let (tls, fingerprint) = tls();
    let secret = Secret::generate();
    let running = jam_server::start(
        SocketAddr::new(LOOPBACK, port),
        tls,
        secret.clone(),
        state,
        limits,
    )
    .await
    .unwrap();
    let code = ServerCode {
        host: LOOPBACK.to_string(),
        port: running.address.port(),
        secret,
        fingerprint,
    };
    Server { running, code }
}

async fn server() -> Server {
    server_with(limits(), None, 0).await
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

fn state_where(wanted: impl Fn(&JamState) -> bool) -> impl Fn(&JamEvent) -> bool {
    move |event| matches!(event, JamEvent::State(state) if wanted(state))
}

fn joined(event: &JamEvent) -> bool {
    matches!(event, JamEvent::Joined { .. })
}

fn ended(event: &JamEvent) -> bool {
    matches!(event, JamEvent::Ended(_))
}

fn add() -> ClientMsg {
    ClientMsg::Add {
        uri: SONG.into(),
        title: "Song".into(),
        artists: "Artist".into(),
        duration_ms: 60_000,
    }
}

fn join(
    code: &ServerCode,
    name: &str,
    limits: Limits,
) -> (client::JamHandle, mpsc::UnboundedReceiver<JamEvent>) {
    let (sink, events) = sink();
    (
        client::join(code.clone(), name, JamClock::new(), limits, sink),
        events,
    )
}

#[tokio::test]
async fn listeners_share_one_jam_with_the_same_rights() {
    let server = server().await;
    let (ana, mut ana_events) = join(&server.code, "Ana", limits());
    until(&mut ana_events, joined).await;
    let (bob, mut bob_events) = join(&server.code, "Bob", limits());
    until(&mut bob_events, joined).await;
    until(
        &mut ana_events,
        state_where(|state| state.participants.len() == 2),
    )
    .await;

    ana.request(add());
    ana.request(add());
    until(&mut bob_events, state_where(|state| state.queue.len() == 1)).await;
    // Bob skips Ana's song: nobody owns the jam.
    bob.request(ClientMsg::SetPlaying { playing: true });
    bob.request(ClientMsg::Skip);
    until(
        &mut ana_events,
        state_where(|state| state.playing && state.queue.is_empty() && state.current.is_some()),
    )
    .await;
    until(&mut bob_events, |event| {
        matches!(event, JamEvent::ClockOffset(_))
    })
    .await;

    bob.leave();
    assert_eq!(
        until(&mut bob_events, ended).await,
        JamEvent::Ended(EndReason::Left)
    );
    until(
        &mut ana_events,
        state_where(|state| state.participants.len() == 1),
    )
    .await;
    ana.leave();
    server.running.stop().await;
}

#[tokio::test]
async fn a_wrong_password_is_rejected_without_retrying() {
    let server = server().await;
    let mut code = server.code.clone();
    code.secret = Secret::generate();
    let (_eve, mut events) = join(&code, "Eve", limits());
    assert_eq!(
        until(&mut events, ended).await,
        JamEvent::Ended(EndReason::Rejected(Rejection::BadPassword))
    );
}

#[tokio::test]
async fn a_server_with_another_certificate_is_never_trusted() {
    let server = server().await;
    let mut code = server.code.clone();
    code.fingerprint = Fingerprint::of(b"another certificate");
    let (_ana, mut events) = join(
        &code,
        "Ana",
        Limits {
            reconnect_attempts: 1,
            ..limits()
        },
    );
    let event = until(&mut events, ended).await;
    assert!(
        matches!(event, JamEvent::Ended(EndReason::Unreachable(_))),
        "{event:?}"
    );
}

#[tokio::test]
async fn the_jam_outlives_a_restart_and_comes_back_paused() {
    let directory = std::env::temp_dir().join(format!("jam-server-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let state = directory.join("state.json");
    let _ = std::fs::remove_file(&state);

    let server = server_with(limits(), Some(state.clone()), 0).await;
    let (ana, mut events) = join(&server.code, "Ana", limits());
    until(&mut events, joined).await;
    ana.request(add());
    ana.request(add());
    ana.request(ClientMsg::SetPlaying { playing: true });
    until(
        &mut events,
        state_where(|state| state.playing && state.queue.len() == 1),
    )
    .await;
    ana.leave();
    until(&mut events, ended).await;
    server.running.stop().await;

    let server = server_with(limits(), Some(state.clone()), 0).await;
    let (ana, mut events) = join(&server.code, "Ana", limits());
    let JamEvent::State(state) =
        until(&mut events, |event| matches!(event, JamEvent::State(_))).await
    else {
        unreachable!()
    };
    assert_eq!(
        state.current.as_ref().map(|item| item.uri.as_str()),
        Some(SONG)
    );
    assert_eq!(state.queue.len(), 1);
    assert!(!state.playing);
    ana.leave();
    server.running.stop().await;
    let _ = std::fs::remove_dir_all(directory);
}

#[tokio::test]
async fn listeners_reconnect_when_the_server_goes_away() {
    let server = server().await;
    let (ana, mut events) = join(&server.code, "Ana", limits());
    until(&mut events, joined).await;
    server.running.stop().await;
    assert_eq!(
        until(&mut events, |event| matches!(
            event,
            JamEvent::Reconnecting { .. }
        ))
        .await,
        JamEvent::Reconnecting { attempt: 1 }
    );
    ana.leave();
    assert_eq!(
        until(&mut events, ended).await,
        JamEvent::Ended(EndReason::Left)
    );
}

/// A TLS connection that has read the challenge, with its nonce and the
/// value the proof must be bound to.
async fn raw(
    code: &ServerCode,
) -> (
    LineReader<tokio::io::ReadHalf<tokio_rustls::client::TlsStream<TcpStream>>>,
    tokio::io::WriteHalf<tokio_rustls::client::TlsStream<TcpStream>>,
    String,
    [u8; BINDING_BYTES],
) {
    let stream = TcpStream::connect((code.host.as_str(), code.port))
        .await
        .unwrap();
    let connector = tokio_rustls::TlsConnector::from(client::pinned_config(code.fingerprint));
    let tls = connector
        .connect(ServerName::try_from("localhost").unwrap(), stream)
        .await
        .unwrap();
    let binding = tls
        .get_ref()
        .1
        .export_keying_material([0; BINDING_BYTES], BINDING_LABEL, None)
        .unwrap();
    let (read, write) = tokio::io::split(tls);
    let mut lines = LineReader::new(read);
    let line = lines.next_line().await.unwrap().unwrap();
    let Ok(ServerMsg::Challenge { nonce, .. }) = decode_server(&line) else {
        panic!("no challenge");
    };
    (lines, write, nonce, binding)
}

/// Whether the server closed this connection within a few seconds.
async fn closed<R: tokio::io::AsyncRead + Unpin>(lines: &mut LineReader<R>) -> bool {
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
    let server = server().await;
    let (_watcher, mut events) = join(&server.code, "Watcher", limits());
    until(&mut events, joined).await;

    // Not TLS at all.
    let mut plain = TcpStream::connect((LOOPBACK, server.code.port))
        .await
        .unwrap();
    let _ = plain.write_all(b"GET / HTTP/1.1\r\n\r\n").await;
    // The server answers with a TLS alert, then closes.
    let mut answer = Vec::new();
    let read = timeout(Duration::from_secs(5), plain.read_to_end(&mut answer)).await;
    assert!(
        matches!(read, Ok(Ok(_)) | Ok(Err(_))) && answer.len() < 64,
        "plain TCP got {read:?} with {} bytes",
        answer.len()
    );

    // A line longer than the protocol allows.
    let (mut lines, mut write, _, _) = raw(&server.code).await;
    let _ = write.write_all(&vec![b'x'; MAX_LINE_BYTES + 10]).await;
    let _ = write.flush().await;
    assert!(
        closed(&mut lines).await,
        "an oversized line keeps the connection"
    );

    // No hello within the handshake time.
    let (mut lines, _write, _, _) = raw(&server.code).await;
    assert!(closed(&mut lines).await, "a silent connection is kept");

    // A proof made for another connection.
    let (mut lines, mut write, nonce, _) = raw(&server.code).await;
    let hello = encode(&ClientMsg::Hello {
        version: PROTOCOL_VERSION,
        name: "Relay".into(),
        proof: auth::proof(&server.code.secret, &nonce, "Relay", b"another connection"),
    });
    write.write_all(hello.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
    let line = lines.next_line().await.unwrap().unwrap();
    assert_eq!(
        decode_server(&line),
        Ok(ServerMsg::Rejected {
            reason: Rejection::BadPassword
        })
    );

    // A listener that floods once admitted.
    let (mut lines, mut write, nonce, binding) = raw(&server.code).await;
    let hello = encode(&ClientMsg::Hello {
        version: PROTOCOL_VERSION,
        name: "Flood".into(),
        proof: auth::proof(&server.code.secret, &nonce, "Flood", &binding),
    });
    write.write_all(hello.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
    until(
        &mut events,
        state_where(|state| state.participants.len() == 2),
    )
    .await;
    let ping = encode(&ClientMsg::Ping { t0: 0 }).repeat(200);
    let _ = write.write_all(ping.as_bytes()).await;
    let _ = write.flush().await;
    assert!(closed(&mut lines).await, "a flood keeps the connection");
    until(
        &mut events,
        state_where(|state| state.participants.len() == 1),
    )
    .await;

    // The jam still lets a proper listener in.
    let (_ana, mut ana_events) = join(&server.code, "Ana", limits());
    until(&mut ana_events, joined).await;
    server.running.stop().await;
}

#[tokio::test]
async fn connections_past_the_cap_are_closed_at_once() {
    let server = server_with(
        Limits {
            max_connections: 2,
            handshake: Duration::from_secs(5),
            ..limits()
        },
        None,
        0,
    )
    .await;
    let _first = raw(&server.code).await;
    let _second = raw(&server.code).await;
    let mut third = TcpStream::connect((LOOPBACK, server.code.port))
        .await
        .unwrap();
    let mut byte = [0; 1];
    let read = timeout(Duration::from_secs(5), third.read(&mut byte)).await;
    assert!(
        matches!(read, Ok(Ok(0)) | Ok(Err(_))),
        "the third connection got {read:?}"
    );
    server.running.stop().await;
}

#[test]
fn the_code_names_the_certificate_and_keeps_the_password() {
    let directory =
        std::env::temp_dir().join(format!("jam-server-code-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("cert.pem"),
        include_bytes!("fixtures/cert.pem"),
    )
    .unwrap();
    std::fs::write(
        directory.join("key.pem"),
        include_bytes!("fixtures/key.pem"),
    )
    .unwrap();
    let data = jam_server::DataDir::new(&directory);
    let first = data.server_code().unwrap();
    let second = data.server_code().unwrap();
    assert_eq!(first, second, "the password is kept, not drawn again");
    let joined = ServerCode::from_parts("jam.example.org", &first).unwrap();
    assert_eq!(joined.fingerprint, tls().1);
    assert_eq!(
        (joined.host.as_str(), joined.port),
        ("jam.example.org", 4070)
    );
    assert!(
        !first.contains("jam.example.org"),
        "the code no longer names the address"
    );
    let _ = std::fs::remove_dir_all(directory);
}
