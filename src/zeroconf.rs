//! Spotify Connect receivers on the local network.
//!
//! Spotify's Web API lists only devices already signed in. Receivers waiting
//! for an account are therefore absent from `/me/player/devices`.
//!
//! Receivers advertise `_spotify-connect._tcp` over mDNS. `getInfo` returns
//! device details and a Diffie-Hellman public key; `addUser` sends encrypted
//! account data. After sign-in, the receiver appears in the normal device list.
//!
//! The transferred librespot credential is encrypted to the receiver's key,
//! never written, and never logged.

use std::collections::HashMap;
use std::time::Duration;

use aes::cipher::{BlockEncrypt, KeyInit, KeyIvInit, StreamCipher};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, Mac};
use librespot_core::diffie_hellman::DhLocalKeys;
use serde::Deserialize;
use sha1::{Digest, Sha1};

const SERVICE: &str = "_spotify-connect._tcp.local.";
/// Spotify's ZeroConf login uses this fixed initialisation vector.
const FIXED_IV: [u8; 16] = [
    253, 81, 222, 19, 70, 203, 45, 89, 141, 68, 210, 240, 93, 20, 76, 30,
];
const HTTP_TIMEOUT: Duration = Duration::from_secs(6);
/// Bound provisional Cast metadata from unauthenticated LAN advertisements.
const MAX_CAST_RECEIVERS: usize = 128;
const CAST_UPDATE_INTERVAL: Duration = Duration::from_millis(100);

/// A latest-only mailbox: at most one progress notification waits in the UI queue.
#[derive(Clone, Default)]
pub struct ReceiverUpdates(std::sync::Arc<std::sync::Mutex<Option<Vec<Receiver>>>>);

impl ReceiverUpdates {
    /// Replaces pending rows; true means the caller must enqueue a notification.
    pub fn publish(&self, receivers: Vec<Receiver>) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(receivers)
            .is_none()
    }

    /// Consumes the newest rows and permits the next publication to notify.
    pub fn take(&self) -> Option<Vec<Receiver>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

type HmacSha1 = Hmac<Sha1>;
type Aes128Ctr = ctr::Ctr128BE<aes::Aes128>;

/// A receiver found on the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receiver {
    /// The name it advertises, e.g. "House Spotify".
    pub name: String,
    /// The receiver's stable Connect identity, after `getInfo` answers.
    pub device_id: Option<String>,
    pub address: std::net::IpAddr,
    pub port: u16,
    /// Path its HTTP interface answers on, from the `CPath` TXT record.
    pub path: String,
    /// A Google Cast device, started over Cast v2 instead of ZeroConf
    /// (`crate::cast`). Its device id is known from its name alone.
    pub cast: bool,
}

impl Receiver {
    /// Waiting Cast rows use an endpoint-scoped preference, not their name-derived
    /// transfer ID. This is a ranking hint, never proof of device authenticity.
    pub fn preference_key(&self) -> Option<String> {
        self.device_id.as_ref().map(|id| {
            if self.cast {
                let endpoint = format!("{id}:{}:{}", self.address, self.port);
                format!("cast-endpoint:{:x}", Sha1::digest(endpoint.as_bytes()))
            } else {
                id.clone()
            }
        })
    }

    /// Builds a direct receiver URL, including brackets for IPv6 literals.
    fn url(&self, query: &str) -> String {
        let host = match self.address {
            std::net::IpAddr::V6(address) => format!("[{address}]"),
            std::net::IpAddr::V4(address) => address.to_string(),
        };
        let path = if self.path.starts_with('/') {
            self.path.clone()
        } else {
            format!("/{}", self.path)
        };
        format!("http://{host}:{}{path}{query}", self.port)
    }
}

/// What a receiver says about itself.
#[derive(Clone, Debug, Deserialize)]
pub struct Info {
    #[serde(rename = "deviceID", default)]
    pub device_id: String,
    #[serde(rename = "remoteName", default)]
    pub remote_name: String,
    #[serde(rename = "deviceType", default)]
    pub device_type: String,
    #[serde(rename = "publicKey", default)]
    pub public_key: String,
    #[serde(rename = "tokenType", default)]
    pub token_type: String,
    #[serde(rename = "activeUser", default)]
    pub active_user: String,
    #[serde(default)]
    pub version: String,
}

impl Info {
    /// Spotify's device kinds are capitalised; the Web API reports lowercase.
    pub fn kind(&self) -> String {
        self.device_type.to_lowercase()
    }
}

#[derive(Deserialize)]
struct AddUserReply {
    #[serde(default)]
    status: i64,
    #[serde(rename = "statusString", default)]
    status_string: String,
}

/// The account credential handed to a receiver.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub auth_type: i64,
    pub auth_data: Vec<u8>,
}

impl Credentials {
    /// Use the in-memory reusable grant already loaded from protected storage.
    pub fn from_playback(stored: &librespot_core::authentication::Credentials) -> Result<Self> {
        use protobuf::Enum;
        Ok(Self {
            username: stored
                .username
                .clone()
                .filter(|name| !name.is_empty())
                .context("playback account is missing")?,
            auth_type: i64::from(stored.auth_type.value()),
            auth_data: stored.auth_data.clone(),
        })
    }
}

/// Browses the network for receivers. Blocking, bounded by `timeout`.
pub fn discover(timeout: Duration) -> Result<Vec<Receiver>> {
    discover_with_updates(timeout, |_| {})
}

/// Publishes resolved Cast advertisements while the bounded browse continues.
/// Connect receivers still need their normal information probe before display.
pub fn discover_with_updates(
    timeout: Duration,
    on_cast_update: impl Fn(Vec<Receiver>) + Send + Sync,
) -> Result<Vec<Receiver>> {
    let daemon = mdns_sd::ServiceDaemon::new().context("cannot browse the local network")?;
    let connect = daemon
        .browse(SERVICE)
        .context("cannot browse for Spotify receivers")?;
    // Connect discovery must not depend on Cast discovery starting.
    let cast = daemon
        .browse(crate::cast::SERVICE)
        .inspect_err(|error| log::warn!("cannot browse for Cast devices: {error}"))
        .ok();
    let deadline = std::time::Instant::now() + timeout;
    // Each browse blocks on its own channel until the deadline.
    let mut receivers = std::thread::scope(|scope| {
        let cast = scope.spawn(move || {
            cast.map(|events| collect(&events, deadline, true, &on_cast_update))
                .unwrap_or_default()
        });
        let mut receivers = collect(&connect, deadline, false, |_| {});
        receivers.extend(cast.join().unwrap());
        receivers
    });
    let _ = daemon.shutdown();
    receivers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(receivers)
}

/// Reads one browse's resolved services until `deadline`.
fn collect(
    events: &mdns_sd::Receiver<mdns_sd::ServiceEvent>,
    deadline: std::time::Instant,
    cast: bool,
    publish: impl FnMut(Vec<Receiver>),
) -> Vec<Receiver> {
    collect_events(
        deadline,
        cast,
        |wait| events.recv_timeout(wait).ok(),
        publish,
    )
}

/// Receives through an injected channel so streaming is tested without a LAN.
fn collect_events(
    deadline: std::time::Instant,
    cast: bool,
    mut receive: impl FnMut(Duration) -> Option<mdns_sd::ServiceEvent>,
    mut publish: impl FnMut(Vec<Receiver>),
) -> Vec<Receiver> {
    let mut found: HashMap<String, Receiver> = HashMap::new();
    let mut changed = false;
    let mut next_publication = std::time::Instant::now();
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        if cast && changed && std::time::Instant::now() >= next_publication {
            publish(cast_snapshot(&found));
            changed = false;
            next_publication = std::time::Instant::now() + CAST_UPDATE_INTERVAL;
        }
        let wait = if cast && changed {
            remaining.min(next_publication.saturating_duration_since(std::time::Instant::now()))
        } else {
            remaining
        };
        match receive(wait) {
            Some(mdns_sd::ServiceEvent::ServiceResolved(service)) => {
                // Prefer IPv4: link-local IPv6 needs a scope id to dial.
                let mut addresses: Vec<std::net::IpAddr> = service
                    .get_addresses()
                    .iter()
                    .map(|scoped| scoped.to_ip_addr())
                    .collect();
                addresses.sort_by_key(|address| address.is_ipv6());
                let Some(address) = addresses.into_iter().find(|address| match address {
                    std::net::IpAddr::V6(address) => !address.is_unicast_link_local(),
                    std::net::IpAddr::V4(_) => true,
                }) else {
                    continue;
                };
                let path = service
                    .get_property_val_str("CPath")
                    .filter(|path| !path.is_empty())
                    .unwrap_or("/")
                    .to_string();
                let (name, device_id) = if cast {
                    // Cast instance names are opaque ids; `fn` is the name.
                    let Some(name) = service.get_property_val_str("fn") else {
                        continue;
                    };
                    (name.to_string(), Some(crate::cast::device_id(name)))
                } else {
                    let instance = service.get_fullname().split('.').next();
                    (unescape_instance(instance.unwrap_or_default()), None)
                };
                let fullname = service.get_fullname();
                if cast && found.len() >= MAX_CAST_RECEIVERS && !found.contains_key(fullname) {
                    continue;
                }
                let receiver = Receiver {
                    name,
                    device_id,
                    address,
                    port: service.get_port(),
                    path,
                    cast,
                };
                if found.get(fullname) == Some(&receiver) {
                    continue;
                }
                found.insert(fullname.to_string(), receiver);
                changed = true;
            }
            Some(mdns_sd::ServiceEvent::ServiceRemoved(_, fullname)) => {
                changed |= found.remove(&fullname).is_some();
            }
            Some(_) => {}
            None => {
                // A short receive timeout publishes a pending burst, then keeps
                // browsing until the original deadline. A closed channel exits.
                if cast && changed && std::time::Instant::now() >= next_publication {
                    continue;
                }
                break;
            }
        }
    }
    if cast && changed {
        publish(cast_snapshot(&found));
    }
    found.into_values().collect()
}

/// Copies only the bounded Cast browse, in deterministic display order.
fn cast_snapshot(found: &HashMap<String, Receiver>) -> Vec<Receiver> {
    let mut snapshot: Vec<_> = found.values().cloned().collect();
    snapshot.sort_by(|a, b| a.name.cmp(&b.name));
    snapshot
}

/// Adds live results without discarding rows from the previous completed scan.
/// A known Connect endpoint keeps precedence over its Cast advertisement.
pub fn merge_discovered(current: &mut Vec<Receiver>, discovered: Vec<Receiver>) {
    for receiver in discovered {
        if receiver.cast
            && current.iter().any(|known| {
                !known.cast && known.address == receiver.address && known.name == receiver.name
            })
        {
            continue;
        }
        if let Some(known) = current.iter_mut().find(|known| {
            known.cast == receiver.cast
                && known.address == receiver.address
                && known.port == receiver.port
        }) {
            *known = receiver;
        } else if !receiver.cast
            || current.iter().filter(|known| known.cast).count() < MAX_CAST_RECEIVERS
        {
            current.push(receiver);
        }
    }
}

/// Asks a receiver to describe itself.
pub fn get_info(http: &reqwest::blocking::Client, receiver: &Receiver) -> Result<Info> {
    get_info_with_timeout(http, receiver, HTTP_TIMEOUT)
}

fn get_info_with_timeout(
    http: &reqwest::blocking::Client,
    receiver: &Receiver,
    timeout: Duration,
) -> Result<Info> {
    let response = http
        .get(receiver.url("?action=getInfo"))
        .timeout(timeout)
        .send()
        .context("receiver did not answer")?;
    if !response.status().is_success() {
        bail!("receiver answered {}", response.status());
    }
    response.json().context("receiver sent an unreadable reply")
}

/// Resolve advertisements before offering them as playback targets. This
/// only reads device information; account credentials are sent on selection.
/// Four probes run at a time, with two seconds per probe and six overall.
pub fn resolve_receivers(receivers: Vec<Receiver>) -> Result<Vec<Receiver>> {
    // Cast devices answer no HTTP probe: asking one about itself means
    // launching Spotify on it, which waits until the user picks it.
    let (cast, receivers): (Vec<_>, Vec<_>) = receivers.into_iter().partition(|r| r.cast);
    let http = reqwest::blocking::Client::builder().no_proxy().build()?;
    let pending = std::sync::Mutex::new(receivers.into_iter());
    let found = std::sync::Mutex::new(HashMap::new());
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                while let Some(remaining) =
                    deadline.checked_duration_since(std::time::Instant::now())
                {
                    let Some(mut receiver) =
                        pending.lock().unwrap_or_else(|p| p.into_inner()).next()
                    else {
                        break;
                    };
                    let Ok(info) = get_info_with_timeout(
                        &http,
                        &receiver,
                        remaining.min(Duration::from_secs(2)),
                    ) else {
                        continue;
                    };
                    if info.device_id.trim().is_empty() || info.remote_name.trim().is_empty() {
                        continue;
                    }
                    receiver.name = info.remote_name;
                    receiver.device_id = Some(info.device_id.clone());
                    found
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .entry(info.device_id)
                        .or_insert(receiver);
                }
            });
        }
    });
    let mut receivers: Vec<_> = found
        .into_inner()
        .unwrap_or_else(|p| p.into_inner())
        .into_values()
        .collect();
    // A speaker that announces both Connect and Cast is offered once, over
    // Connect. It is the same address and name; a group hosted there has
    // its own name, and a speaker elsewhere its own address.
    let cast: Vec<_> = cast
        .into_iter()
        .filter(|device| {
            !receivers
                .iter()
                .any(|r| r.address == device.address && r.name == device.name)
        })
        .collect();
    receivers.extend(cast);
    receivers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(receivers)
}

/// Hands an account to a receiver so it logs in and joins Spotify Connect.
pub fn add_user(
    http: &reqwest::blocking::Client,
    receiver: &Receiver,
    info: &Info,
    credentials: &Credentials,
    our_name: &str,
) -> Result<()> {
    let (blob, client_key) = login_blob(credentials, &info.device_id, &info.public_key)?;
    let our_device_id = hex(&Sha1::digest(our_name.as_bytes()));
    let form = [
        ("action", "addUser".to_string()),
        (
            "version",
            if info.version.is_empty() {
                "2.9.0".into()
            } else {
                info.version.clone()
            },
        ),
        ("tokenType", "default".to_string()),
        ("clientKey", client_key),
        ("loginId", credentials.username.clone()),
        ("userName", credentials.username.clone()),
        ("blob", blob),
        ("deviceName", our_name.to_string()),
        ("deviceId", our_device_id),
    ];
    let response = http
        .post(receiver.url(""))
        .timeout(HTTP_TIMEOUT)
        .form(&form)
        .send()
        .context("receiver did not accept the connection")?;
    let reply: AddUserReply = response
        .json()
        .context("receiver sent an unreadable reply")?;
    // 101 is this interface's "OK".
    if reply.status == 101 {
        return Ok(());
    }
    let detail = if reply.status_string.is_empty() {
        format!("status {}", reply.status)
    } else {
        reply.status_string.clone()
    };
    Err(anyhow!("receiver refused the connection ({detail})"))
}

/// Builds the encrypted login blob and our Diffie-Hellman public key.
///
/// The inner blob carries the account and its reusable credential, wrapped in
/// a key derived from the receiver's own device id, so a blob captured from
/// the network is useless to anything but that receiver. That is then
/// encrypted again to the key both sides derive from the exchange.
fn login_blob(
    credentials: &Credentials,
    receiver_device_id: &str,
    receiver_public_key: &str,
) -> Result<(String, String)> {
    let remote_key = BASE64
        .decode(receiver_public_key)
        .context("receiver public key is malformed")?;
    if remote_key.is_empty() {
        bail!("receiver offered no public key");
    }

    let mut blob: Vec<u8> = Vec::new();
    write_int(&mut blob, 0x49);
    write_bytes(&mut blob, credentials.username.as_bytes());
    write_int(&mut blob, 0x50);
    write_int(&mut blob, credentials.auth_type as u32);
    write_int(&mut blob, 0x51);
    write_bytes(&mut blob, &credentials.auth_data);
    let padding = 16 - (blob.len() % 16) - 1;
    blob.extend(std::iter::repeat_n(0u8, padding));
    blob.push(padding as u8 + 1);
    // Spotify's own obfuscation pass over the padded blob.
    let length = blob.len();
    for index in (0..=length.saturating_sub(0x11)).rev() {
        blob[length - index - 1] ^= blob[length - index - 0x11];
    }

    let secret = Sha1::digest(receiver_device_id.as_bytes());
    let mut derived = [0u8; 20];
    pbkdf2::pbkdf2_hmac::<Sha1>(
        &secret,
        credentials.username.as_bytes(),
        0x100,
        &mut derived,
    );
    let mut blob_key = Sha1::digest(derived).to_vec();
    blob_key.extend_from_slice(&[0, 0, 0, 20]);

    // AES-192-ECB, because the key is the 20-byte digest plus its length.
    let cipher = aes::Aes192::new_from_slice(&blob_key)
        .map_err(|_| anyhow!("could not prepare the credential cipher"))?;
    let mut encrypted_blob = blob.clone();
    let (blocks, _) = encrypted_blob.as_chunks_mut::<16>();
    for block in blocks {
        cipher.encrypt_block(block.into());
    }
    let encoded_blob = BASE64.encode(&encrypted_blob);

    let keys = DhLocalKeys::random(&mut rand::rng());
    let shared = keys.shared_secret(&remote_key);
    let base_key = &Sha1::digest(&shared)[..16];
    let encryption_key = {
        let mut mac =
            <HmacSha1 as Mac>::new_from_slice(base_key).expect("HMAC accepts any key length");
        mac.update(b"encryption");
        mac.finalize().into_bytes()
    };
    let mut encrypted = encoded_blob.into_bytes();
    let mut stream = Aes128Ctr::new_from_slices(&encryption_key[..16], &FIXED_IV)
        .map_err(|_| anyhow!("could not prepare the transport cipher"))?;
    stream.apply_keystream(&mut encrypted);

    let checksum = {
        let checksum_key = {
            let mut mac =
                <HmacSha1 as Mac>::new_from_slice(base_key).expect("HMAC accepts any key length");
            mac.update(b"checksum");
            mac.finalize().into_bytes()
        };
        let mut mac =
            <HmacSha1 as Mac>::new_from_slice(&checksum_key).expect("HMAC accepts any key length");
        mac.update(&encrypted);
        mac.finalize().into_bytes()
    };

    let mut signed = FIXED_IV.to_vec();
    signed.extend_from_slice(&encrypted);
    signed.extend_from_slice(&checksum);
    Ok((BASE64.encode(signed), BASE64.encode(keys.public_key())))
}

/// mDNS escapes bytes in an instance name as `\\ddd`, so "House Spotify"
/// arrives as "House\\032Spotify".
fn unescape_instance(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut characters = name.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        let digits: String = characters.clone().take(3).collect();
        match digits.parse::<u8>() {
            Ok(byte) if digits.len() == 3 => {
                out.push(byte as char);
                characters.nth(2);
            }
            _ => out.push(character),
        }
    }
    out
}

/// Spotify's variable-length integer.
fn write_int(out: &mut Vec<u8>, value: u32) {
    if value < 0x80 {
        out.push(value as u8);
    } else {
        out.push(0x80 | (value & 0x7F) as u8);
        out.push((value >> 7) as u8);
    }
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    write_int(out, value.len() as u32);
    out.extend_from_slice(value);
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn live_updates_keep_previous_rows_and_merge_connect_cast_duplicates() {
        use super::*;
        let connect = Receiver {
            name: "Room".into(),
            device_id: Some("connect-id".into()),
            address: "192.0.2.1".parse().unwrap(),
            port: 80,
            path: "/".into(),
            cast: false,
        };
        let mut known = vec![connect.clone()];
        let cast = Receiver {
            device_id: Some(crate::cast::device_id("Room")),
            port: 8009,
            cast: true,
            ..connect.clone()
        };
        let other = Receiver {
            address: "192.0.2.2".parse().unwrap(),
            ..cast.clone()
        };
        let group = Receiver {
            name: "Group".into(),
            device_id: Some(crate::cast::device_id("Group")),
            port: 32000,
            ..cast.clone()
        };
        merge_discovered(&mut known, vec![cast, other.clone(), group.clone()]);
        assert_eq!(known, [connect, other.clone(), group]);
        let renamed = Receiver {
            name: "Renamed".into(),
            ..other
        };
        merge_discovered(&mut known, vec![renamed.clone()]);
        assert_eq!(known.len(), 3);
        assert_eq!(known[1], renamed);
    }

    /// A ready Cast row must be published before the next receive or scan deadline.
    #[test]
    fn cast_results_are_published_before_the_browse_finishes() {
        use super::*;
        let service = mdns_sd::ServiceInfo::new(
            crate::cast::SERVICE,
            "speaker",
            "speaker.local.",
            "192.0.2.1",
            8009,
            [("fn", "Living room")].as_slice(),
        )
        .unwrap()
        .as_resolved_service();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut next = Some(mdns_sd::ServiceEvent::ServiceResolved(Box::new(service)));
        let found = collect_events(
            std::time::Instant::now() + Duration::from_secs(30),
            true,
            |_| {
                if let Some(event) = next.take() {
                    return Some(event);
                }
                let ready: Vec<Receiver> = rx
                    .try_recv()
                    .expect("publish before waiting for more mDNS events");
                assert_eq!(ready[0].name, "Living room");
                assert!(ready[0].cast);
                None
            },
            |rows| tx.send(rows).unwrap(),
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn repeated_advertisements_update_and_removed_services_leave_the_final_list() {
        use super::*;
        let service = |port| {
            mdns_sd::ServiceInfo::new(
                crate::cast::SERVICE,
                "speaker",
                "speaker.local.",
                "192.0.2.1",
                port,
                [("fn", "Living room")].as_slice(),
            )
            .unwrap()
            .as_resolved_service()
        };
        let fullname = service(8009).fullname;
        let mut events = [
            mdns_sd::ServiceEvent::ServiceResolved(Box::new(service(8009))),
            mdns_sd::ServiceEvent::ServiceResolved(Box::new(service(8010))),
            mdns_sd::ServiceEvent::ServiceRemoved(crate::cast::SERVICE.into(), fullname),
        ]
        .into_iter();
        let mut snapshots = Vec::new();
        let found = collect_events(
            std::time::Instant::now() + Duration::from_secs(30),
            true,
            |_| events.next(),
            |rows| snapshots.push(rows),
        );
        assert_eq!(snapshots[0][0].port, 8009);
        assert!(snapshots.last().unwrap().is_empty());
        assert!(found.is_empty());
    }

    #[test]
    fn a_pending_burst_is_published_on_timeout_and_browsing_continues() {
        let service = |index| {
            mdns_sd::ServiceInfo::new(
                crate::cast::SERVICE,
                &format!("speaker-{index}"),
                "speaker.local.",
                "192.0.2.1",
                8009,
                [("fn", "Speaker")].as_slice(),
            )
            .unwrap()
            .as_resolved_service()
        };
        let updates = ReceiverUpdates::default();
        let mut step = 0;
        let found = collect_events(
            std::time::Instant::now() + Duration::from_secs(30),
            true,
            |wait| {
                step += 1;
                match step {
                    1 | 2 => Some(mdns_sd::ServiceEvent::ServiceResolved(Box::new(service(
                        step,
                    )))),
                    3 => {
                        assert_eq!(updates.take().unwrap().len(), 1, "first row is immediate");
                        assert!(wait <= CAST_UPDATE_INTERVAL);
                        std::thread::sleep(wait);
                        None
                    }
                    4 => {
                        assert_eq!(
                            updates.take().unwrap().len(),
                            2,
                            "burst did not wait for scan end"
                        );
                        Some(mdns_sd::ServiceEvent::ServiceResolved(Box::new(service(
                            step,
                        ))))
                    }
                    _ => None,
                }
            },
            |rows| {
                updates.publish(rows);
            },
        );
        assert_eq!(
            found.len(),
            3,
            "the publication timeout must not end discovery"
        );
        assert_eq!(updates.take().unwrap().len(), 3);
    }

    #[test]
    fn cast_bursts_are_bounded_and_identical_advertisements_do_not_republish() {
        let service = |index: usize| {
            mdns_sd::ServiceInfo::new(
                crate::cast::SERVICE,
                &format!("speaker-{index}"),
                "speaker.local.",
                "192.0.2.1",
                8009,
                [("fn", "Speaker")].as_slice(),
            )
            .unwrap()
            .as_resolved_service()
        };
        let mut events = (0..MAX_CAST_RECEIVERS * 4)
            .chain(std::iter::repeat_n(0, 1_000))
            .map(|index| mdns_sd::ServiceEvent::ServiceResolved(Box::new(service(index))));
        let updates = ReceiverUpdates::default();
        let mut notifications = 0;
        let found = collect_events(
            std::time::Instant::now() + Duration::from_secs(30),
            true,
            |_| events.next(),
            |rows| {
                assert!(rows.len() <= MAX_CAST_RECEIVERS);
                notifications += usize::from(updates.publish(rows));
            },
        );
        assert_eq!(found.len(), MAX_CAST_RECEIVERS);
        assert_eq!(notifications, 1, "a busy UI gets one pending notification");
        assert_eq!(updates.take().unwrap().len(), MAX_CAST_RECEIVERS);
        assert!(updates.take().is_none());
        assert!(
            updates.publish(found.clone()),
            "consuming permits a new notification"
        );
        let mut rows = found;
        let mut later = rows[0].clone();
        later.address = "192.0.2.2".parse().unwrap();
        merge_discovered(&mut rows, vec![later]);
        assert_eq!(
            rows.len(),
            MAX_CAST_RECEIVERS,
            "churn cannot grow provisional rows"
        );

        let mut duplicate_events =
            std::iter::repeat_with(|| mdns_sd::ServiceEvent::ServiceResolved(Box::new(service(0))))
                .take(1_000);
        let mut publications = 0;
        collect_events(
            std::time::Instant::now() + Duration::from_secs(30),
            true,
            |_| duplicate_events.next(),
            |_| publications += 1,
        );
        assert_eq!(publications, 1);
    }

    #[test]
    fn waiting_cast_preferences_do_not_follow_a_name_to_another_endpoint() {
        let speaker = Receiver {
            name: "Room".into(),
            device_id: Some(crate::cast::device_id("Room")),
            address: "192.0.2.1".parse().unwrap(),
            port: 8009,
            path: "/".into(),
            cast: true,
        };
        let other = Receiver {
            address: "192.0.2.2".parse().unwrap(),
            ..speaker.clone()
        };
        assert_eq!(speaker.device_id, other.device_id);
        assert_ne!(speaker.preference_key(), other.preference_key());
        let key = speaker.preference_key().unwrap();
        assert!(!key.contains("192.0.2.1"));
        assert_ne!(Some(&key), speaker.device_id.as_ref());
    }

    #[test]
    fn receiver_handoff_uses_the_in_memory_playback_grant() {
        let stored = librespot_core::authentication::Credentials {
            username: Some("dummy-account".into()),
            auth_type: librespot_protocol::authentication::AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
            auth_data: b"dummy-reusable-grant".to_vec(),
        };
        let handoff = super::Credentials::from_playback(&stored).unwrap();
        assert_eq!(handoff.username, "dummy-account");
        assert_eq!(handoff.auth_data, b"dummy-reusable-grant");
        assert!(
            super::Credentials::from_playback(
                &librespot_core::authentication::Credentials::with_access_token("dummy-token")
            )
            .is_err()
        );
    }

    use super::*;

    #[test]
    fn cast_devices_are_offered_without_an_http_probe() {
        // TEST-NET-1: nothing answers there, so a probe would drop it.
        let device = Receiver {
            name: "nest audio".into(),
            device_id: Some(crate::cast::device_id("nest audio")),
            address: "192.0.2.1".parse().unwrap(),
            port: 8009,
            path: "/".into(),
            cast: true,
        };
        assert_eq!(resolve_receivers(vec![device.clone()]).unwrap(), [device]);
    }

    #[test]
    fn variable_length_integers_match_the_wire_format() {
        let mut out = Vec::new();
        write_int(&mut out, 0x49);
        assert_eq!(out, vec![0x49]);
        out.clear();
        write_int(&mut out, 0x100);
        assert_eq!(out, vec![0x80, 0x02]);
        out.clear();
        write_bytes(&mut out, b"abc");
        assert_eq!(out, vec![3, b'a', b'b', b'c']);
    }

    /// The blob is deterministic apart from the ephemeral key, so the same
    /// account and receiver must always produce the same length and a
    /// well-formed envelope: fixed IV, ciphertext, then a 20-byte checksum.
    #[test]
    fn login_blob_has_the_expected_envelope() {
        let credentials = Credentials {
            username: "someone".into(),
            auth_type: 1,
            auth_data: vec![7u8; 80],
        };
        let receiver_key = BASE64.encode(vec![9u8; 96]);
        let (blob, client_key) =
            login_blob(&credentials, "abcdef0123456789", &receiver_key).unwrap();
        let decoded = BASE64.decode(blob).unwrap();
        assert_eq!(&decoded[..16], &FIXED_IV);
        assert!(decoded.len() > 16 + 20);
        assert!(!client_key.is_empty());
        assert!(BASE64.decode(client_key).is_ok());
    }

    #[test]
    fn instance_names_are_unescaped() {
        assert_eq!(unescape_instance("House\\032Spotify"), "House Spotify");
        assert_eq!(unescape_instance("Kitchen"), "Kitchen");
        assert_eq!(unescape_instance("odd\\x"), "odd\\x");
    }

    #[test]
    fn receiver_urls_bracket_ipv6() {
        let receiver = Receiver {
            name: "House".into(),
            device_id: None,
            address: "192.168.8.166".parse().unwrap(),
            port: 5907,
            path: "/".into(),
            cast: false,
        };
        assert_eq!(
            receiver.url("?action=getInfo"),
            "http://192.168.8.166:5907/?action=getInfo"
        );
        let receiver = Receiver {
            address: "fe80::1".parse().unwrap(),
            path: "zc".into(),
            cast: false,
            ..receiver
        };
        assert_eq!(receiver.url(""), "http://[fe80::1]:5907/zc");
    }
    #[test]
    fn receiver_probes_bypass_an_environment_proxy() {
        // Give only a subprocess the proxy environment. Other tests and the
        // user's environment remain untouched, including when tests run in parallel.
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        proxy.set_nonblocking(true).unwrap();
        let address = format!("http://{}", proxy.local_addr().unwrap());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args([
            "--exact",
            "zeroconf::tests::discovery_uses_receiver_names_and_ids_and_omits_dead_advertisements",
        ]);
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            child.env(name, &address);
        }
        child
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .env_remove("REQUEST_METHOD");
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            matches!(proxy.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "LAN discovery must send no request to the proxy"
        );
    }

    #[test]
    fn discovery_uses_receiver_names_and_ids_and_omits_dead_advertisements() {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..4 {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("receiver did not get its direct probe: {error}"),
                    }
                };
                // Accept polling is nonblocking; request reads must wait for
                // data, including on platforms that inherit the listener mode.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                std::io::BufReader::new(&mut stream)
                    .read_line(&mut line)
                    .unwrap();
                assert!(line.starts_with("GET "));
                assert!(line.contains("action=getInfo"));
                let (status, body) = if line.contains("/dead?") {
                    ("503 Service Unavailable", "{}")
                } else if line.contains("/other?") {
                    ("200 OK", r#"{"deviceID":"two","remoteName":"Living Room"}"#)
                } else {
                    ("200 OK", r#"{"deviceID":"one","remoteName":"Living Room"}"#)
                };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let mut receivers: Vec<_> = ["first", "duplicate", "other", "dead"]
            .map(|path| Receiver {
                name: format!("SpotifyConnect-{path}"),
                device_id: None,
                address: "127.0.0.1".parse().unwrap(),
                port,
                path: format!("/{path}"),
                cast: false,
            })
            .into();
        // Cast on a Connect receiver's address is that speaker again; the
        // same name elsewhere is another speaker.
        for address in ["127.0.0.1", "192.0.2.1"] {
            receivers.push(Receiver {
                name: "Living Room".into(),
                device_id: Some(crate::cast::device_id("Living Room")),
                address: address.parse().unwrap(),
                port: 8009,
                path: "/".into(),
                cast: true,
            });
        }
        let (cast, resolved): (Vec<_>, Vec<_>) = resolve_receivers(receivers)
            .unwrap()
            .into_iter()
            .partition(|receiver| receiver.cast);
        server.join().unwrap();
        assert_eq!(cast.len(), 1);
        assert_eq!(
            cast[0].address,
            "192.0.2.1".parse::<std::net::IpAddr>().unwrap()
        );
        assert_eq!(
            resolved.len(),
            2,
            "merge identities, not matching room names"
        );
        assert!(
            resolved
                .iter()
                .all(|receiver| receiver.name == "Living Room")
        );
        assert!(
            resolved
                .iter()
                .any(|receiver| receiver.device_id.as_deref() == Some("one"))
        );
        assert!(
            resolved
                .iter()
                .any(|receiver| receiver.device_id.as_deref() == Some("two"))
        );
    }
}
