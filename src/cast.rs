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
//! A Cast device's TLS certificate is its own, which no trust store holds, so
//! the connection accepts any certificate. Before anything else, the device
//! proves it is genuine the way Google's own senders ask it to: it signs a
//! fresh nonce and this connection's certificate with its device key, whose
//! certificate chains to one of Google's Cast roots. A device that fails gets
//! nothing more, so the token only goes to a genuine device. The token is
//! never written or logged.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use librespot_core::Session;
use md5::{Digest, Md5};
use protobuf::{CodedInputStream, CodedOutputStream};
use rustls::pki_types::{
    AlgorithmIdentifier, CertificateDer, InvalidSignature, SignatureVerificationAlgorithm,
    UnixTime, alg_id,
};
use serde_json::{Value, json};

pub(crate) const SERVICE: &str = "_googlecast._tcp.local.";
/// Spotify's Cast receiver app.
const SPOTIFY_APP: &str = "CC32E753";
/// Google's Cast roots, as Chromium and Open Screen trust them
/// (openscreen `cast/common/certificate/`). SHA-256 fingerprints:
///
/// - Cast Root CA:
///   `809af14700b3fe2611ad597eb1584d6354313b64ccdb3390f097fa3e5826d6ca`
/// - Eureka Root CA, for older devices:
///   `caf6d1e37b532203e01a76bf07187bb731ccd38801565ab2211a2c0ab7f3bc46`
const GOOGLE_ROOTS: [&[u8]; 2] = [
    include_bytes!("../assets/cast/cast-root-ca.der"),
    include_bytes!("../assets/cast/eureka-root-ca.der"),
];
const NS_DEVICE_AUTH: &str = "urn:x-cast:com.google.cast.tp.deviceauth";
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
        activate(&mut stream, &GOOGLE_ROOTS, name, deadline, &mut exchange)
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

/// One attempt of [`start`] over an open connection, until `deadline`. The
/// device must prove it has a certificate from one of `roots`.
fn activate<S: Stream>(
    stream: &mut S,
    roots: &[&[u8]],
    name: &str,
    deadline: Instant,
    exchange: impl FnOnce(&str, &str) -> Result<String>,
) -> Result<()> {
    let device_id = device_id(name);
    let mut channel = Channel { stream, deadline };
    channel
        .authenticate(roots)
        .context("the Cast device could not prove it is a genuine Google Cast device")?;
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
    /// Challenges the device to prove it has a certificate from one of
    /// `roots`, as Google's own senders do before anything else.
    fn authenticate(&mut self, roots: &[&[u8]]) -> Result<()> {
        let peer = self
            .stream
            .peer_certificate()
            .ok_or_else(|| anyhow!("the Cast device presented no certificate"))?
            .to_vec();
        let nonce: [u8; 16] = rand::random();
        self.send_payload(
            PLATFORM,
            NS_DEVICE_AUTH,
            Payload::Binary(challenge(&nonce)?),
        )?;
        let reply = self.wait_for(NS_DEVICE_AUTH, |message, _| match &message.payload {
            Payload::Binary(reply) => Some(Ok(reply.clone())),
            Payload::Text(_) => None,
        })?;
        verify(&reply, &nonce, &peer, roots, UnixTime::now())?;
        log::debug!("the Cast device proved it is genuine");
        Ok(())
    }

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
        self.send_payload(destination, namespace, Payload::Text(payload.to_string()))
    }

    fn send_payload(&mut self, destination: &str, namespace: &str, payload: Payload) -> Result<()> {
        let message = Message {
            source: SENDER.to_owned(),
            destination: destination.to_owned(),
            namespace: namespace.to_owned(),
            payload,
        };
        let bytes = message.encode()?;
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
        self.wait_for(namespace, |_, json| pick(json?))
    }

    /// [`Self::wait`] for a message with any payload. `pick` also gets the
    /// payload as JSON, if it is.
    fn wait_for<T>(
        &mut self,
        namespace: &str,
        mut pick: impl FnMut(&Message, Option<&Value>) -> Option<Result<T>>,
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
            let json = message.json();
            let kind = json.as_ref().and_then(|json| json["type"].as_str());
            log::debug!(
                "received {} from {} on {}",
                kind.unwrap_or("?"),
                message.source,
                message.namespace
            );
            if message.namespace == NS_HEARTBEAT && kind == Some("PING") {
                self.send(&message.source, NS_HEARTBEAT, json!({ "type": "PONG" }))?;
                continue;
            }
            if message.namespace == namespace
                && let Some(result) = pick(&message, json.as_ref())
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

    /// The certificate the device presented for this connection, in DER.
    fn peer_certificate(&self) -> Option<&[u8]>;
}

impl Stream for TlsStream {
    fn set_read_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.sock.set_read_timeout(Some(timeout))
    }

    fn peer_certificate(&self) -> Option<&[u8]> {
        Some(self.conn.peer_certificates()?.first()?.as_ref())
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

/// Accepts the certificate of a Cast device, which only the device vouches
/// for. The handshake signature is still checked, and
/// [`Channel::authenticate`] then checks the device.
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

/// The challenge for [`Channel::authenticate`]: a `DeviceAuthMessage` whose
/// challenge (1) holds sender_nonce (2) and asks for a SHA-256 signature
/// (3 hash_algorithm = SHA256), as Open Screen sends it.
fn challenge(nonce: &[u8]) -> Result<Vec<u8>> {
    let challenge = encode(|writer| {
        writer.write_bytes(2, nonce)?;
        writer.write_enum(3, 1)
    })?;
    encode(|writer| writer.write_bytes(1, &challenge))
}

/// Checks the device's reply to [`challenge`] with `nonce`, sent over the
/// connection whose certificate is `peer`, as Open Screen does
/// (`cast/sender/channel/cast_auth_util.cc`): the device certificate must
/// chain to one of `roots` and be valid at `now`, and must sign the nonce the
/// device echoes followed by `peer`. Older devices echo no nonce and sign
/// `peer` alone.
///
/// The reply is a `DeviceAuthMessage` with a response (2) or an error (3).
/// The response holds 1 signature, 2 client_auth_certificate,
/// 3 intermediate_certificate (repeated), 5 sender_nonce and
/// 6 hash_algorithm (0 SHA-1, the default, or 1 SHA-256).
fn verify(reply: &[u8], nonce: &[u8], peer: &[u8], roots: &[&[u8]], now: UnixTime) -> Result<()> {
    let mut response = None;
    let mut input = CodedInputStream::from_bytes(reply);
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match tag {
            0x12 => response = Some(input.read_bytes()?),
            0x1a => bail!("it refused the challenge"),
            _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
        }
    }
    let response = response.ok_or_else(|| anyhow!("it did not answer the challenge"))?;

    let (mut signature, mut device, mut echoed, mut sha256) =
        (Vec::new(), Vec::new(), Vec::new(), false);
    let mut intermediates = Vec::new();
    let mut input = CodedInputStream::from_bytes(&response);
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match tag {
            0x0a => signature = input.read_bytes()?,
            0x12 => device = input.read_bytes()?,
            0x1a => intermediates.push(CertificateDer::from(input.read_bytes()?)),
            0x2a => echoed = input.read_bytes()?,
            0x30 => sha256 = input.read_int32()? == 1,
            _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
        }
    }
    // Open Screen lets a mismatch through, and the signature over this
    // connection's certificate already rules out a replay; a different
    // nonce still means the reply is not to this challenge.
    if !echoed.is_empty() && echoed != nonce {
        bail!("it answered a different challenge");
    }

    let roots: Vec<_> = roots
        .iter()
        .map(|&root| CertificateDer::from(root))
        .collect();
    let anchors = roots
        .iter()
        .map(webpki::anchor_from_trusted_cert)
        .collect::<Result<Vec<_>, _>>()?;
    let device = CertificateDer::from(device);
    let device = webpki::EndEntityCert::try_from(&device)
        .map_err(|error| anyhow!("its certificate is unreadable ({error})"))?;
    device
        .verify_for_usage(
            CHAIN_ALGORITHMS,
            &anchors,
            &intermediates,
            now,
            AnyUsage,
            None,
            None,
        )
        .map_err(|error| {
            anyhow!("its certificate is not valid under Google's Cast roots ({error})")
        })?;

    let algorithm: &dyn SignatureVerificationAlgorithm = if sha256 {
        webpki::ring::RSA_PKCS1_2048_8192_SHA256
    } else {
        &RsaPkcs1Sha1
    };
    device
        .verify_signature(algorithm, &[&echoed[..], peer].concat(), &signature)
        .map_err(|error| anyhow!("its signature does not match this connection ({error})"))?;
    Ok(())
}

/// The signatures Open Screen accepts in a Cast certificate chain.
const CHAIN_ALGORITHMS: &[&dyn SignatureVerificationAlgorithm] = &[
    &RsaPkcs1Sha1,
    webpki::ring::RSA_PKCS1_2048_8192_SHA256,
    webpki::ring::RSA_PKCS1_2048_8192_SHA384,
    webpki::ring::RSA_PKCS1_2048_8192_SHA512,
];

/// RSA PKCS #1 v1.5 with SHA-1, which rustls-webpki leaves out. Older Cast
/// devices sign with it, and Google's own senders still accept it.
#[derive(Debug)]
struct RsaPkcs1Sha1;

impl SignatureVerificationAlgorithm for RsaPkcs1Sha1 {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY,
            public_key,
        )
        .verify(message, signature)
        .map_err(|_| InvalidSignature)
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        // sha1WithRSAEncryption (1.2.840.113549.1.1.5), NULL parameters.
        AlgorithmIdentifier::from_slice(&[
            0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x05, 0x05, 0x00,
        ])
    }
}

/// Open Screen does not check a Cast certificate's extended key usage.
struct AnyUsage;

impl webpki::ExtendedKeyUsageValidator for AnyUsage {
    fn validate(&self, _: webpki::KeyPurposeIdIter<'_, '_>) -> Result<(), webpki::Error> {
        Ok(())
    }
}

/// A Cast v2 `CastMessage`.
///
/// The protocol crate has no Cast messages, so their documented wire fields
/// go through the existing codec: 1 protocol_version, 2 source_id,
/// 3 destination_id, 4 namespace, 5 payload_type, 6 payload_utf8,
/// 7 payload_binary.
#[derive(Debug, PartialEq)]
struct Message {
    source: String,
    destination: String,
    namespace: String,
    payload: Payload,
}

#[derive(Debug, PartialEq)]
enum Payload {
    Text(String),
    Binary(Vec<u8>),
}

impl Message {
    fn encode(&self) -> Result<Vec<u8>> {
        encode(|writer| {
            writer.write_enum(1, 0)?; // protocol_version = CASTV2_1_0
            writer.write_string(2, &self.source)?;
            writer.write_string(3, &self.destination)?;
            writer.write_string(4, &self.namespace)?;
            match &self.payload {
                Payload::Text(text) => {
                    writer.write_enum(5, 0)?; // payload_type = STRING
                    writer.write_string(6, text)
                }
                Payload::Binary(bytes) => {
                    writer.write_enum(5, 1)?; // payload_type = BINARY
                    writer.write_bytes(7, bytes)
                }
            }
        })
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let mut message = Message {
            source: String::new(),
            destination: String::new(),
            namespace: String::new(),
            payload: Payload::Text(String::new()),
        };
        let mut input = CodedInputStream::from_bytes(bytes);
        let text = |input: &mut CodedInputStream| -> Result<String> {
            Ok(String::from_utf8_lossy(&input.read_bytes()?).into_owned())
        };
        while let Some(tag) = input.read_raw_tag_or_eof()? {
            match tag {
                0x12 => message.source = text(&mut input)?,
                0x1a => message.destination = text(&mut input)?,
                0x22 => message.namespace = text(&mut input)?,
                0x32 => message.payload = Payload::Text(text(&mut input)?),
                0x3a => message.payload = Payload::Binary(input.read_bytes()?),
                _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
            }
        }
        Ok(message)
    }

    /// The payload as JSON, if it is text that parses.
    fn json(&self) -> Option<Value> {
        match &self.payload {
            Payload::Text(text) => serde_json::from_str(text).ok(),
            Payload::Binary(_) => None,
        }
    }
}

/// The protobuf fields `write` writes.
fn encode(write: impl FnOnce(&mut CodedOutputStream) -> protobuf::Result<()>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut writer = CodedOutputStream::vec(&mut bytes);
    write(&mut writer)?;
    writer.flush()?;
    drop(writer);
    Ok(bytes)
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
            payload: Payload::Text("p".repeat(200)),
        };
        let bytes = message.encode().unwrap();
        assert_eq!(
            &bytes[..14],
            &[
                0x08, 0, 0x12, 1, b's', 0x1a, 1, b'd', 0x22, 1, b'n', 0x28, 0, 0x32
            ]
        );
        // 200 needs a two-byte varint.
        assert_eq!(&bytes[14..16], &[0xc8, 0x01]);
        assert_eq!(Message::decode(&bytes).unwrap(), message);

        let binary = Message {
            payload: Payload::Binary(vec![0, 0xff]),
            ..message
        };
        let bytes = binary.encode().unwrap();
        assert_eq!(&bytes[11..], &[0x28, 1, 0x3a, 2, 0, 0xff]);
        assert_eq!(Message::decode(&bytes).unwrap(), binary);
    }

    /// A test chain: root, then ICA, then a device certificate signed with
    /// SHA-256 and one signed with SHA-1, both for the key in device-key.pk8.
    /// Made with OpenSSL, and valid until 2126.
    const TEST_ROOTS: [&[u8]; 1] = [include_bytes!("testdata/cast/root.der")];
    const TEST_ICA: &[u8] = include_bytes!("testdata/cast/ica.der");
    const TEST_DEVICE: &[u8] = include_bytes!("testdata/cast/device.der");
    const TEST_DEVICE_SHA1: &[u8] = include_bytes!("testdata/cast/device-sha1.der");
    const TEST_DEVICE_KEY: &[u8] = include_bytes!("testdata/cast/device-key.pk8");
    /// What the fake device presents for the TLS connection.
    const PEER: &[u8] = b"this connection's certificate";
    /// The key's SHA-1 signature of [`PEER`] alone, as an older device signs.
    /// ring cannot sign with SHA-1, so OpenSSL made it.
    const LEGACY_SIGNATURE: &[u8] = include_bytes!("testdata/cast/legacy-signature.bin");

    /// A device's answer to [`challenge`]: its signature, certificate and ICA,
    /// the nonce it echoes (none if empty), and whether it signed with SHA-256.
    fn auth_reply(
        signature: &[u8],
        device: &[u8],
        ica: &[u8],
        nonce: &[u8],
        sha256: bool,
    ) -> Vec<u8> {
        let response = encode(|writer| {
            writer.write_bytes(1, signature)?;
            writer.write_bytes(2, device)?;
            writer.write_bytes(3, ica)?;
            if !nonce.is_empty() {
                writer.write_bytes(5, nonce)?;
            }
            if sha256 {
                writer.write_enum(6, 1)?;
            }
            Ok(())
        })
        .unwrap();
        encode(|writer| writer.write_bytes(2, &response)).unwrap()
    }

    /// A Cast device that answers each frame from a script.
    struct FakeDevice {
        sent: Vec<Message>,
        inbox: VecDeque<u8>,
        partial: Vec<u8>,
        add_user_reply: &'static str,
        /// Answers as an older device: its SHA-1 certificate, no nonce echoed,
        /// and a SHA-1 signature of the connection certificate alone.
        legacy: bool,
        /// The connection certificate it signs. Anything but [`PEER`] plays
        /// a relay passing on a genuine device's answer from another
        /// connection.
        signs_peer: &'static [u8],
    }

    impl FakeDevice {
        fn new(add_user_reply: &'static str) -> Self {
            Self {
                sent: Vec::new(),
                inbox: VecDeque::new(),
                partial: Vec::new(),
                add_user_reply,
                legacy: false,
                signs_peer: PEER,
            }
        }

        fn reply(&mut self, source: &str, namespace: &str, payload: Value) {
            self.reply_with(source, namespace, Payload::Text(payload.to_string()));
        }

        fn reply_with(&mut self, source: &str, namespace: &str, payload: Payload) {
            let bytes = Message {
                source: source.into(),
                destination: SENDER.into(),
                namespace: namespace.into(),
                payload,
            }
            .encode()
            .unwrap();
            self.inbox.extend((bytes.len() as u32).to_be_bytes());
            self.inbox.extend(bytes);
        }

        /// Answers a [`challenge`] as a device with the test chain does.
        fn answer(&self, challenge: &[u8]) -> Vec<u8> {
            if self.legacy {
                return auth_reply(LEGACY_SIGNATURE, TEST_DEVICE_SHA1, TEST_ICA, &[], false);
            }
            // challenge (0x0a, 20 bytes) { sender_nonce (0x12, 16 bytes), ... }
            let nonce = &challenge[4..20];

            let key = ring::signature::RsaKeyPair::from_pkcs8(TEST_DEVICE_KEY).unwrap();
            let mut signature = vec![0; key.public().modulus_len()];
            let signed = [nonce, self.signs_peer].concat();
            let random = ring::rand::SystemRandom::new();
            key.sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &random,
                &signed,
                &mut signature,
            )
            .unwrap();
            auth_reply(&signature, TEST_DEVICE, TEST_ICA, nonce, true)
        }

        fn handle(&mut self, message: Message) {
            if let Payload::Binary(challenge) = &message.payload {
                let answer = self.answer(challenge);
                self.reply_with(PLATFORM, NS_DEVICE_AUTH, Payload::Binary(answer));
                self.sent.push(message);
                return;
            }
            let payload = message.json().unwrap();
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

        fn peer_certificate(&self) -> Option<&[u8]> {
            Some(PEER)
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

    fn summary(sent: &[Message]) -> Vec<String> {
        sent.iter()
            .map(|m| {
                let kind = m.json().map_or("challenge".to_owned(), |json| {
                    json["type"].as_str().unwrap().to_owned()
                });
                let namespace = m.namespace.rsplit('.').next().unwrap();
                format!("{} {namespace} {kind}", m.destination)
            })
            .collect()
    }

    fn sign_in(device: &mut FakeDevice) -> Result<()> {
        activate(
            device,
            &TEST_ROOTS,
            "nest audio",
            Instant::now() + TIMEOUT,
            |_, _| Ok("token-1".into()),
        )
    }

    #[test]
    fn activation_launches_spotify_and_hands_it_the_exchanged_token() {
        let mut device = FakeDevice::new("addUserResponse");
        let mut exchanged = None;
        let deadline = Instant::now() + TIMEOUT;
        activate(
            &mut device,
            &TEST_ROOTS,
            "nest audio",
            deadline,
            |client, device| {
                exchanged = Some((client.to_owned(), device.to_owned()));
                Ok("token-1".into())
            },
        )
        .unwrap();

        assert_eq!(
            exchanged,
            Some(("client-1".into(), device_id("nest audio")))
        );
        assert_eq!(
            summary(&device.sent),
            [
                "receiver-0 deviceauth challenge",
                "receiver-0 connection CONNECT",
                "receiver-0 receiver LAUNCH",
                "receiver-0 heartbeat PONG",
                "web-7 connection CONNECT",
                "web-7 v1 getInfo",
                "web-7 v1 addUser",
            ]
        );
        let get_info = device.sent[5].json().unwrap();
        assert_eq!(get_info["payload"]["remoteName"], "nest audio");
        assert_eq!(get_info["payload"]["deviceID"], device_id("nest audio"));
        let add_user = device.sent[6].json().unwrap();
        assert_eq!(
            add_user["payload"],
            json!({"blob": "token-1", "tokenType": "accesstoken"})
        );
    }

    /// Activates `device` with Google's roots or the test root, and returns
    /// the error and whether a token was asked for.
    fn activation_error(device: &mut FakeDevice, roots: &[&[u8]]) -> (String, bool) {
        let mut exchanged = false;
        let deadline = Instant::now() + TIMEOUT;
        let error = activate(device, roots, "nest audio", deadline, |_, _| {
            exchanged = true;
            Ok("token-1".into())
        })
        .unwrap_err();
        (format!("{error:#}"), exchanged)
    }

    #[test]
    fn a_device_without_a_google_certificate_gets_nothing_more() {
        let mut device = FakeDevice::new("addUserResponse");
        let (error, exchanged) = activation_error(&mut device, &GOOGLE_ROOTS);

        assert!(
            error.contains("not valid under Google's Cast roots"),
            "{error}"
        );
        assert!(!exchanged);
        assert_eq!(summary(&device.sent), ["receiver-0 deviceauth challenge"]);
    }

    #[test]
    fn an_answer_signed_for_another_connection_is_refused() {
        let mut device = FakeDevice::new("addUserResponse");
        device.signs_peer = b"a genuine device's certificate on another connection";
        let (error, exchanged) = activation_error(&mut device, &TEST_ROOTS);

        assert!(error.contains("does not match this connection"), "{error}");
        assert!(!exchanged);
        assert_eq!(summary(&device.sent), ["receiver-0 deviceauth challenge"]);
    }

    #[test]
    fn older_devices_that_sign_with_sha1_are_genuine_too() {
        let mut device = FakeDevice::new("addUserResponse");
        device.legacy = true;
        sign_in(&mut device).unwrap();
    }

    /// A real first-generation Chromecast: its SHA-1 chain to the Eureka root
    /// and its SHA-1 signature of "STRING", from Open Screen's test data
    /// (`test/data/cast/common/certificate/`, BSD-3-Clause, The Chromium
    /// Authors): `certificates/chromecast_gen1.pem` and
    /// `signeddata/2ZZBG9_FA8FCA3EF91A.pem`.
    #[test]
    fn a_real_first_generation_chromecast_is_genuine() {
        let reply = auth_reply(
            include_bytes!("testdata/cast/chromecast-gen1-sha1.sig"),
            include_bytes!("testdata/cast/chromecast-gen1.der"),
            include_bytes!("testdata/cast/chromecast-gen1-ica.der"),
            &[],
            false,
        );
        // 2026-01-01. Its ICA expires at the end of 2032.
        let now = UnixTime::since_unix_epoch(Duration::from_secs(1_767_225_600));

        // The reply echoes no nonce, so the device signed `peer` alone.
        verify(&reply, b"any", b"STRING", &GOOGLE_ROOTS, now).unwrap();
        assert!(verify(&reply, b"any", b"STRINGS", &GOOGLE_ROOTS, now).is_err());
    }

    #[test]
    fn a_slow_token_exchange_does_not_use_up_the_device_time() {
        let mut device = FakeDevice::new("addUserResponse");
        let deadline = Instant::now() + Duration::from_millis(50);
        activate(&mut device, &TEST_ROOTS, "nest audio", deadline, |_, _| {
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
