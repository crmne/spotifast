//! Google Cast speakers on the local network.
//!
//! Cast devices advertise `_googlecast._tcp`, not `_spotify-connect._tcp`, and
//! do not run Spotify until a sender starts it. Spotify's own apps do this:
//!
//! 1. Launch Spotify's Cast receiver app over Cast v2 (TLS; port 8009 for a
//!    speaker, another port for a group).
//! 2. Ask it for `getInfo`. The reply names the Spotify client it runs as.
//! 3. Exchange the session's token for one issued to that client.
//! 4. Send that token with `addUser`.
//!
//! After `addUser` the device is an ordinary Spotify Connect device with the
//! id from [`device_id`], so the normal transfer takes over.
//!
//! Cast devices present certificates from Google's device CA, which no system
//! trust store holds. The connection therefore accepts any certificate. It
//! only goes to a device found on the LAN, and the token sent over it is
//! issued for that receiver only. The token is never written or logged.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use librespot_core::Session;
use md5::{Digest, Md5};
use serde_json::{Value, json};

pub(crate) const SERVICE: &str = "_googlecast._tcp.local.";
/// Spotify's Cast receiver app.
const SPOTIFY_APP: &str = "CC32E753";
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_SPOTIFY: &str = "urn:x-cast:com.spotify.chromecast.secure.v1";
const SENDER: &str = "sender-0";
const PLATFORM: &str = "receiver-0";
/// For one attempt, from connecting to the answer to `addUser`, not counting
/// the token request, and for that request. A group once took longer than
/// 10 s to launch.
const TIMEOUT: Duration = Duration::from_secs(20);
/// Cast messages are at most 64 KiB.
const MAX_FRAME: usize = 64 * 1024;

/// The Spotify Connect device id a Cast device takes after `addUser`.
pub fn device_id(friendly_name: &str) -> String {
    format!("{:x}", Md5::digest(friendly_name))
}

/// Connects, starts Spotify on the device and signs it in.
///
/// `exchange` receives the receiver's client id and its device id, and
/// returns a token for that client.
///
/// Right after an earlier sign-in, a device can refuse `addUser` (409,
/// `ERROR-CANNOT-LOAD`). A second attempt succeeded in every test, so one
/// refusal is retried. A timeout is not: it would double the wait.
pub fn start(
    address: IpAddr,
    port: u16,
    name: &str,
    mut exchange: impl FnMut(&str, &str) -> Result<String>,
) -> Result<()> {
    let mut attempt = || {
        let deadline = Instant::now() + TIMEOUT;
        let mut stream = connect(address, port, deadline)?;
        activate(&mut stream, name, deadline, &mut exchange)
    };
    match attempt() {
        Err(error) if error.is::<Refused>() => {
            log::info!("retrying the Cast device after: {error:#}");
            attempt()
        }
        result => result,
    }
}

/// Exchanges the session's token for one issued to the receiver's client.
pub async fn device_token(session: &Session, client_id: &str, device_id: &str) -> Result<String> {
    let body = serde_json::to_vec(&json!({ "clientId": client_id, "deviceId": device_id }))?;
    let mut headers = reqwest::header::HeaderMap::new();
    // JSON labelled as text, as Spotify's web player sends it.
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("text/plain;charset=UTF-8"),
    );
    let request = session.spclient().request(
        &reqwest::Method::POST,
        "/device-auth/v1/refresh",
        Some(headers),
        Some(&body),
    );
    let reply = tokio::time::timeout(TIMEOUT, request)
        .await
        .map_err(|_| anyhow!("Spotify did not issue a token for the Cast device in time"))?
        .map_err(|error| anyhow!("Spotify refused a token for the Cast device: {error}"))?;
    let reply: Value = serde_json::from_slice(&reply)?;
    reply["accessToken"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Spotify's token reply has no accessToken"))
}

/// One attempt of [`start`] over an open connection, until `deadline`.
fn activate<S: Stream>(
    stream: &mut S,
    name: &str,
    deadline: Instant,
    exchange: impl FnOnce(&str, &str) -> Result<String>,
) -> Result<()> {
    let device_id = device_id(name);
    let mut channel = Channel { stream, deadline };
    let transport = channel.launch_spotify()?;
    let client_id = channel.get_info(&transport, name, &device_id)?;

    log::debug!("exchanging a token for client {client_id}");
    let asked = Instant::now();
    let token = exchange(&client_id, &device_id)?;
    // The device waits while Spotify answers, so that time is not its own.
    channel.deadline += asked.elapsed();

    channel.add_user(&transport, &token)
}

/// A Cast v2 connection to one device, bounded by one deadline.
struct Channel<'a, S> {
    stream: &'a mut S,
    deadline: Instant,
}

/// The steps of an activation.
impl<S: Stream> Channel<'_, S> {
    /// Launches Spotify's receiver app and returns its transport id.
    fn launch_spotify(&mut self) -> Result<String> {
        self.send(PLATFORM, NS_CONNECTION, json!({ "type": "CONNECT" }))?;
        // One request per connection, so its id can be fixed.
        let launch = json!({ "type": "LAUNCH", "appId": SPOTIFY_APP, "requestId": 1 });
        self.send(PLATFORM, NS_RECEIVER, launch)?;
        let transport = self.wait(NS_RECEIVER, |message| match message["type"].as_str() {
            Some("LAUNCH_ERROR" | "INVALID_REQUEST") => Some(Err(anyhow!(
                "the Cast device could not start Spotify: {}",
                message["reason"].as_str().unwrap_or("no reason given")
            ))),
            Some("RECEIVER_STATUS") => message["status"]["applications"]
                .as_array()?
                .iter()
                .find(|app| app["appId"] == SPOTIFY_APP)
                .and_then(|app| app["transportId"].as_str())
                .map(|id| Ok(id.to_owned())),
            _ => None,
        })?;
        log::debug!("Spotify is running as {transport}");
        Ok(transport)
    }

    /// Introduces the device to Spotify and returns the client it runs as.
    fn get_info(&mut self, transport: &str, name: &str, device_id: &str) -> Result<String> {
        self.send(transport, NS_CONNECTION, json!({ "type": "CONNECT" }))?;
        let get_info = json!({
            "type": "getInfo",
            "payload": {
                "remoteName": name,
                "deviceID": device_id,
                // Groups accept false too, and spotcast sends it for them.
                "deviceAPI_isGroup": false,
            },
        });
        self.send(transport, NS_SPOTIFY, get_info)?;
        self.wait(NS_SPOTIFY, |message| match message["type"].as_str() {
            Some("getInfoResponse") => Some(
                message["payload"]["clientID"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("Spotify on the Cast device sent no clientID")),
            ),
            _ => None,
        })
    }

    /// Signs Spotify on the device in with `token`.
    fn add_user(&mut self, transport: &str, token: &str) -> Result<()> {
        log::debug!("sending addUser");
        let add_user = json!({
            "type": "addUser",
            "payload": { "blob": token, "tokenType": "accesstoken" },
        });
        self.send(transport, NS_SPOTIFY, add_user)?;
        self.wait(NS_SPOTIFY, |message| match message["type"].as_str() {
            Some("addUserResponse") => Some(Ok(())),
            Some("addUserError") => {
                let status = message["payload"]["statusString"].as_str();
                Some(Err(
                    Refused(status.unwrap_or("no reason given").into()).into()
                ))
            }
            _ => None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Spotify on the Cast device refused the account: {0}")]
struct Refused(String);

impl<S: Stream> Channel<'_, S> {
    fn send(&mut self, destination: &str, namespace: &str, payload: Value) -> Result<()> {
        let message = Message {
            source: SENDER.to_owned(),
            destination: destination.to_owned(),
            namespace: namespace.to_owned(),
            payload: payload.to_string(),
        };
        let bytes = message.encode();
        self.stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
        self.stream.write_all(&bytes)?;
        self.stream.flush()?;
        Ok(())
    }

    /// Reads messages until `pick` returns a result for one on `namespace`,
    /// answering heartbeats.
    fn wait<T>(
        &mut self,
        namespace: &str,
        mut pick: impl FnMut(&Value) -> Option<Result<T>>,
    ) -> Result<T> {
        let late = || anyhow!("the Cast device did not answer in time");
        // A zero timeout is an error, not an immediate timeout.
        while let Some(remaining) = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
        {
            self.stream.set_read_timeout(remaining)?;
            let message = self.receive().map_err(|error| match error.downcast_ref() {
                Some(io) if is_timeout(io) => late(),
                _ => error,
            })?;
            let Ok(payload) = serde_json::from_str::<Value>(&message.payload) else {
                continue;
            };
            log::debug!(
                "received {} from {} on {}",
                payload["type"].as_str().unwrap_or("?"),
                message.source,
                message.namespace
            );
            if message.namespace == NS_HEARTBEAT && payload["type"] == "PING" {
                self.send(&message.source, NS_HEARTBEAT, json!({ "type": "PONG" }))?;
                continue;
            }
            if message.namespace == namespace
                && let Some(result) = pick(&payload)
            {
                return result;
            }
        }
        Err(late())
    }

    fn receive(&mut self) -> Result<Message> {
        let mut length = [0; 4];
        self.stream.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_FRAME {
            bail!("the Cast device sent an oversized message");
        }
        let mut bytes = vec![0; length];
        self.stream.read_exact(&mut bytes)?;
        Message::decode(&bytes)
    }
}

/// A read timeout surfaces as `WouldBlock` on Unix and `TimedOut` on Windows.
fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// A connection whose reads can be bounded, so one deadline covers the
/// whole activation.
trait Stream: Read + Write {
    fn set_read_timeout(&mut self, timeout: Duration) -> std::io::Result<()>;
}

impl Stream for TlsStream {
    fn set_read_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.sock.set_read_timeout(Some(timeout))
    }
}

fn connect(address: IpAddr, port: u16, deadline: Instant) -> Result<TlsStream> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    let tls = rustls::ClientConnection::new(Arc::new(config), address.into())?;
    let tcp = TcpStream::connect_timeout(&SocketAddr::new(address, port), remaining)
        .with_context(|| format!("unable to reach the Cast device at {address}:{port}"))?;
    tcp.set_write_timeout(Some(remaining))?;
    tcp.set_read_timeout(Some(remaining))?;
    let mut stream = rustls::StreamOwned::new(tls, tcp);
    // Otherwise the handshake runs inside the first send, where its errors
    // read as the device's.
    stream
        .conn
        .complete_io(&mut stream.sock)
        .context("the Cast device did not complete a TLS handshake")?;
    Ok(stream)
}

/// Accepts the certificate of a Cast device, which Google's private CA signs.
/// The handshake signature is still checked.
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// A Cast v2 `CastMessage` with a text payload.
///
/// Encoded by hand, as only six small fields are needed: 1 protocol_version,
/// 2 source_id, 3 destination_id, 4 namespace, 5 payload_type, 6 payload_utf8.
/// Binary payloads (field 7) are skipped.
#[derive(Debug, PartialEq)]
struct Message {
    source: String,
    destination: String,
    namespace: String,
    payload: String,
}

impl Message {
    fn encode(&self) -> Vec<u8> {
        let mut out = vec![0x08, 0]; // protocol_version = CASTV2_1_0
        for (tag, text) in [
            (0x12, &self.source),
            (0x1a, &self.destination),
            (0x22, &self.namespace),
        ] {
            put_string(&mut out, tag, text);
        }
        out.extend([0x28, 0]); // payload_type = STRING
        put_string(&mut out, 0x32, &self.payload);
        out
    }

    fn decode(mut bytes: &[u8]) -> Result<Self> {
        let mut message = Message {
            source: String::new(),
            destination: String::new(),
            namespace: String::new(),
            payload: String::new(),
        };
        while !bytes.is_empty() {
            let key = take_varint(&mut bytes)?;
            match key & 7 {
                0 => {
                    take_varint(&mut bytes)?;
                }
                2 => {
                    let length = take_varint(&mut bytes)? as usize;
                    if length > bytes.len() {
                        bail!("truncated Cast message");
                    }
                    let (field, rest) = bytes.split_at(length);
                    bytes = rest;
                    let text = || String::from_utf8_lossy(field).into_owned();
                    match key >> 3 {
                        2 => message.source = text(),
                        3 => message.destination = text(),
                        4 => message.namespace = text(),
                        6 => message.payload = text(),
                        _ => {}
                    }
                }
                _ => bail!("unexpected field in a Cast message"),
            }
        }
        Ok(message)
    }
}

fn put_string(out: &mut Vec<u8>, tag: u8, text: &str) {
    out.push(tag);
    let mut length = text.len();
    while length >= 0x80 {
        out.push(length as u8 | 0x80);
        length >>= 7;
    }
    out.push(length as u8);
    out.extend_from_slice(text.as_bytes());
}

fn take_varint(bytes: &mut &[u8]) -> Result<u64> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes
            .split_first()
            .ok_or_else(|| anyhow!("truncated Cast message"))?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("malformed Cast message")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn device_ids_are_the_md5_of_the_friendly_name() {
        assert_eq!(device_id("nest audio"), "c3d119e4cb0ba20fb718aa0b99857bf8");
    }

    #[test]
    fn messages_match_the_wire_format_and_round_trip() {
        let message = Message {
            source: "s".into(),
            destination: "d".into(),
            namespace: "n".into(),
            payload: "p".repeat(200),
        };
        let bytes = message.encode();
        assert_eq!(
            &bytes[..14],
            &[
                0x08, 0, 0x12, 1, b's', 0x1a, 1, b'd', 0x22, 1, b'n', 0x28, 0, 0x32
            ]
        );
        // 200 needs a two-byte varint.
        assert_eq!(&bytes[14..16], &[0xc8, 0x01]);
        assert_eq!(Message::decode(&bytes).unwrap(), message);
    }

    /// A Cast device that answers each frame from a script.
    struct FakeDevice {
        sent: Vec<Message>,
        inbox: VecDeque<u8>,
        partial: Vec<u8>,
        add_user_reply: &'static str,
    }

    impl FakeDevice {
        fn new(add_user_reply: &'static str) -> Self {
            Self {
                sent: Vec::new(),
                inbox: VecDeque::new(),
                partial: Vec::new(),
                add_user_reply,
            }
        }

        fn reply(&mut self, source: &str, namespace: &str, payload: Value) {
            let bytes = Message {
                source: source.into(),
                destination: SENDER.into(),
                namespace: namespace.into(),
                payload: payload.to_string(),
            }
            .encode();
            self.inbox.extend((bytes.len() as u32).to_be_bytes());
            self.inbox.extend(bytes);
        }

        fn handle(&mut self, message: Message) {
            let payload: Value = serde_json::from_str(&message.payload).unwrap();
            match payload["type"].as_str().unwrap() {
                "LAUNCH" => {
                    // A status for another app first, then a heartbeat, then Spotify.
                    self.reply(
                        PLATFORM,
                        NS_RECEIVER,
                        json!({"type": "RECEIVER_STATUS", "status": {"applications": [
                            {"appId": "E8C28D3C", "transportId": "other"}]}}),
                    );
                    self.reply(PLATFORM, NS_HEARTBEAT, json!({"type": "PING"}));
                    self.reply(
                        PLATFORM,
                        NS_RECEIVER,
                        json!({"type": "RECEIVER_STATUS", "status": {"applications": [
                            {"appId": SPOTIFY_APP, "transportId": "web-7"}]}}),
                    );
                }
                "getInfo" => self.reply(
                    "web-7",
                    NS_SPOTIFY,
                    json!({"type": "getInfoResponse", "payload": {"clientID": "client-1"}}),
                ),
                // An empty reply plays a device that stops answering.
                "addUser" if !self.add_user_reply.is_empty() => {
                    let reply = self.add_user_reply;
                    self.reply("web-7", NS_SPOTIFY, json!({"type": reply}));
                }
                _ => {}
            }
            self.sent.push(message);
        }
    }

    impl Write for FakeDevice {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.partial.extend_from_slice(buf);
            while self.partial.len() >= 4 {
                let length = u32::from_be_bytes(self.partial[..4].try_into().unwrap()) as usize;
                if self.partial.len() < 4 + length {
                    break;
                }
                let frame: Vec<u8> = self.partial.drain(..4 + length).skip(4).collect();
                self.handle(Message::decode(&frame).unwrap());
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Stream for FakeDevice {
        fn set_read_timeout(&mut self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Read for FakeDevice {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.inbox.is_empty() {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let n = buf.len().min(self.inbox.len());
            for (slot, byte) in buf.iter_mut().zip(self.inbox.drain(..n)) {
                *slot = byte;
            }
            Ok(n)
        }
    }

    fn summary(sent: &[Message]) -> Vec<(String, String, String)> {
        sent.iter()
            .map(|m| {
                let payload: Value = serde_json::from_str(&m.payload).unwrap();
                (
                    m.destination.clone(),
                    m.namespace.rsplit('.').next().unwrap().to_owned(),
                    payload["type"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn sign_in(device: &mut FakeDevice) -> Result<()> {
        activate(device, "nest audio", Instant::now() + TIMEOUT, |_, _| {
            Ok("token-1".into())
        })
    }

    #[test]
    fn activation_launches_spotify_and_hands_it_the_exchanged_token() {
        let mut device = FakeDevice::new("addUserResponse");
        let mut exchanged = None;
        let deadline = Instant::now() + TIMEOUT;
        activate(&mut device, "nest audio", deadline, |client, device| {
            exchanged = Some((client.to_owned(), device.to_owned()));
            Ok("token-1".into())
        })
        .unwrap();

        assert_eq!(
            exchanged,
            Some(("client-1".into(), device_id("nest audio")))
        );
        let steps: Vec<_> = summary(&device.sent)
            .into_iter()
            .map(|(to, ns, kind)| format!("{to} {ns} {kind}"))
            .collect();
        assert_eq!(
            steps,
            [
                "receiver-0 connection CONNECT",
                "receiver-0 receiver LAUNCH",
                "receiver-0 heartbeat PONG",
                "web-7 connection CONNECT",
                "web-7 v1 getInfo",
                "web-7 v1 addUser",
            ]
        );
        let get_info: Value = serde_json::from_str(&device.sent[4].payload).unwrap();
        assert_eq!(get_info["payload"]["remoteName"], "nest audio");
        assert_eq!(get_info["payload"]["deviceID"], device_id("nest audio"));
        let add_user: Value = serde_json::from_str(&device.sent[5].payload).unwrap();
        assert_eq!(
            add_user["payload"],
            json!({"blob": "token-1", "tokenType": "accesstoken"})
        );
    }

    #[test]
    fn a_slow_token_exchange_does_not_use_up_the_device_time() {
        let mut device = FakeDevice::new("addUserResponse");
        let deadline = Instant::now() + Duration::from_millis(50);
        activate(&mut device, "nest audio", deadline, |_, _| {
            std::thread::sleep(Duration::from_millis(100));
            Ok("token-1".into())
        })
        .unwrap();
    }

    #[test]
    fn a_refused_account_is_an_error() {
        let mut device = FakeDevice::new("addUserError");
        assert!(sign_in(&mut device).unwrap_err().is::<Refused>());
    }

    #[test]
    fn a_silent_device_is_reported_as_late_not_as_an_os_error() {
        let mut device = FakeDevice::new("");
        let error = sign_in(&mut device).unwrap_err();
        assert_eq!(error.to_string(), "the Cast device did not answer in time");
    }
}
