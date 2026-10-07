//! Invitation codes and the handshake proof.
//!
//! A code reads `host:port#secret`. The secret is 128 random bits drawn for
//! each jam, never chosen by a person, so a captured handshake cannot be
//! guessed back to it. A guest never sends the secret: it answers the
//! host's random nonce with an HMAC keyed by it.
//!
//! The proof also covers a transport binding. Over plain TCP it is empty;
//! once the channel is encrypted it becomes a value unique to that channel
//! (a TLS exporter, say), so a proof cannot be relayed into another
//! connection. Plain TCP is meant for a trusted network or a tunnel such as
//! Tailscale only: it neither hides nor protects what follows the handshake.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const SECRET_BYTES: usize = 16;
const NONCE_BYTES: usize = 32;
/// Separates jam proofs from any other use of the same key.
const PROOF_LABEL: &[u8] = b"spotifast-jam/1 hello";

/// The shared secret of one jam. Its `Debug` hides the value so it cannot
/// reach a log by accident; there is deliberately no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; SECRET_BYTES]);

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret(..)")
    }
}

impl Secret {
    pub fn generate() -> Self {
        Self(rand::random())
    }

    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    fn decode(text: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }
}

/// Where a jam is and the secret to enter it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    /// A host name or IP address; an IPv6 address without brackets.
    pub host: String,
    pub port: u16,
    pub secret: Secret,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InviteError {
    #[error("the invitation has no secret")]
    MissingSecret,
    #[error("the invitation's secret is invalid")]
    BadSecret,
    #[error("the invitation's address is invalid")]
    BadAddress,
}

impl Invite {
    /// The code to share. It grants entry to the jam: show it to the people
    /// invited, and never write it to a log.
    pub fn code(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{host}:{}#{}", self.port, self.secret.encode())
    }

    pub fn parse(code: &str) -> Result<Self, InviteError> {
        let (address, secret) = code
            .trim()
            .split_once('#')
            .ok_or(InviteError::MissingSecret)?;
        let secret = Secret::decode(secret).ok_or(InviteError::BadSecret)?;
        let (host, port) = if let Some(rest) = address.strip_prefix('[') {
            let (host, port) = rest.split_once("]:").ok_or(InviteError::BadAddress)?;
            (host, port)
        } else {
            let (host, port) = address.rsplit_once(':').ok_or(InviteError::BadAddress)?;
            if host.contains(':') {
                return Err(InviteError::BadAddress);
            }
            (host, port)
        };
        let port: u16 = port.parse().map_err(|_| InviteError::BadAddress)?;
        let host_valid = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-:%".contains(&byte));
        if !host_valid || port == 0 {
            return Err(InviteError::BadAddress);
        }
        Ok(Self {
            host: host.to_string(),
            port,
            secret,
        })
    }
}

/// A fresh challenge for one connection.
pub fn new_nonce() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; NONCE_BYTES]>())
}

/// The guest's answer to `nonce`, for the name it joins under.
pub fn proof(secret: &Secret, nonce: &str, name: &str, binding: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(
        proof_mac(secret, nonce, name, binding)
            .finalize()
            .into_bytes(),
    )
}

/// Checks a guest's answer in constant time.
pub fn verify(secret: &Secret, nonce: &str, name: &str, binding: &[u8], proof: &str) -> bool {
    let Ok(proof) = URL_SAFE_NO_PAD.decode(proof) else {
        return false;
    };
    proof_mac(secret, nonce, name, binding)
        .verify_slice(&proof)
        .is_ok()
}

fn proof_mac(secret: &Secret, nonce: &str, name: &str, binding: &[u8]) -> HmacSha256 {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&secret.0).expect("HMAC accepts any key");
    mac.update(PROOF_LABEL);
    // Length-prefix each field so no two different inputs read the same.
    for field in [nonce.as_bytes(), name.as_bytes(), binding] {
        mac.update(&(field.len() as u64).to_be_bytes());
        mac.update(field);
    }
    mac
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_for_names_and_both_ip_families() {
        for host in ["192.168.1.20", "jam.example", "fe80::1", "laptop"] {
            let invite = Invite {
                host: host.into(),
                port: 4070,
                secret: Secret::generate(),
            };
            assert_eq!(Invite::parse(&invite.code()), Ok(invite.clone()));
        }
    }

    #[test]
    fn malformed_codes_are_refused() {
        let secret = Secret::generate().encode();
        for (code, error) in [
            ("host:4070".to_string(), InviteError::MissingSecret),
            ("host:4070#short".to_string(), InviteError::BadSecret),
            (format!("host#{secret}"), InviteError::BadAddress),
            (format!("host:0#{secret}"), InviteError::BadAddress),
            (format!("host:99999#{secret}"), InviteError::BadAddress),
            (format!("fe80::1:4070#{secret}"), InviteError::BadAddress),
            (format!("ho st:4070#{secret}"), InviteError::BadAddress),
            (format!(":4070#{secret}"), InviteError::BadAddress),
        ] {
            assert_eq!(Invite::parse(&code), Err(error), "{code}");
        }
    }

    #[test]
    fn secrets_are_random_and_never_printed() {
        let secret = Secret::generate();
        assert_ne!(secret, Secret::generate());
        let invite = Invite {
            host: "h".into(),
            port: 1,
            secret: secret.clone(),
        };
        let debug = format!("{invite:?}");
        assert!(!debug.contains(&secret.encode()), "{debug}");
    }

    #[test]
    fn the_proof_holds_only_for_its_secret_nonce_name_and_channel() {
        let secret = Secret::generate();
        let nonce = new_nonce();
        assert_ne!(nonce, new_nonce());
        let good = proof(&secret, &nonce, "Ana", b"");
        assert!(verify(&secret, &nonce, "Ana", b"", &good));
        assert!(!verify(&Secret::generate(), &nonce, "Ana", b"", &good));
        assert!(!verify(&secret, &new_nonce(), "Ana", b"", &good));
        assert!(!verify(&secret, &nonce, "Bob", b"", &good));
        assert!(!verify(&secret, &nonce, "Ana", b"other channel", &good));
        assert!(!verify(&secret, &nonce, "Ana", b"", "not base64!"));
        assert!(!verify(&secret, &nonce, "Ana", b"", ""));
        // The length prefixes keep shifted boundaries apart.
        let shifted = proof(&secret, &format!("{nonce}A"), "na", b"");
        assert!(!verify(&secret, &nonce, "Ana", b"", &shifted));
    }
}
