//! The server code, and how each side proves itself to the other.
//!
//! A server code reads `host:port#password#fingerprint`. The password is 128
//! random bits drawn by the server, never chosen by a person, so a captured
//! handshake cannot be guessed back to it. The fingerprint is the SHA-256 of
//! the server's TLS certificate: the app accepts that certificate and no
//! other, which needs neither a domain name nor a certificate authority.
//!
//! A listener never sends the password. It answers the server's random nonce
//! with an HMAC keyed by it, over its name and a value exported from the TLS
//! session ([`BINDING_LABEL`]), so a proof cannot be relayed into another
//! connection.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const SECRET_BYTES: usize = 16;
const NONCE_BYTES: usize = 32;
/// Separates jam proofs from any other use of the same key.
const PROOF_LABEL: &[u8] = b"spotifast-jam/2 hello";
/// The TLS exporter label both sides derive the channel binding with.
pub const BINDING_LABEL: &[u8] = b"EXPORTER-spotifast-jam";
pub const BINDING_BYTES: usize = 32;

/// The jam server's password. Its `Debug` hides the value so it cannot
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

    /// The text kept in the server's data directory and in the code.
    pub fn to_text(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn from_text(text: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(text.trim()).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }
}

/// The SHA-256 of a TLS certificate, in DER.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of(certificate_der: &[u8]) -> Self {
        Self(Sha256::digest(certificate_der).into())
    }

    fn to_text(self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    fn from_text(text: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }
}

/// Where the jam server is, the certificate it must show, and the password
/// it expects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerCode {
    /// A host name or IP address; an IPv6 address without brackets.
    pub host: String,
    pub port: u16,
    pub secret: Secret,
    pub fingerprint: Fingerprint,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodeError {
    #[error("the server code is incomplete")]
    Incomplete,
    #[error("the server code's password is invalid")]
    BadSecret,
    #[error("the server code's fingerprint is invalid")]
    BadFingerprint,
    #[error("the server code's address is invalid")]
    BadAddress,
}

impl ServerCode {
    /// The code to give the people who may join. It grants entry: share it
    /// privately, and never write it to a log.
    pub fn code(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "{host}:{}#{}#{}",
            self.port,
            self.secret.to_text(),
            self.fingerprint.to_text()
        )
    }

    pub fn parse(code: &str) -> Result<Self, CodeError> {
        let mut parts = code.trim().split('#');
        let (Some(address), Some(secret), Some(fingerprint), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(CodeError::Incomplete);
        };
        let secret = Secret::from_text(secret).ok_or(CodeError::BadSecret)?;
        let fingerprint = Fingerprint::from_text(fingerprint).ok_or(CodeError::BadFingerprint)?;
        let (host, port) = if let Some(rest) = address.strip_prefix('[') {
            rest.split_once("]:").ok_or(CodeError::BadAddress)?
        } else {
            let (host, port) = address.rsplit_once(':').ok_or(CodeError::BadAddress)?;
            if host.contains(':') {
                return Err(CodeError::BadAddress);
            }
            (host, port)
        };
        let port: u16 = port.parse().map_err(|_| CodeError::BadAddress)?;
        let host_valid = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-:%".contains(&byte));
        if !host_valid || port == 0 {
            return Err(CodeError::BadAddress);
        }
        Ok(Self {
            host: host.to_string(),
            port,
            secret,
            fingerprint,
        })
    }
}

/// A fresh challenge for one connection.
pub fn new_nonce() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; NONCE_BYTES]>())
}

/// The listener's answer to `nonce`, for the name it joins under.
pub fn proof(secret: &Secret, nonce: &str, name: &str, binding: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(
        proof_mac(secret, nonce, name, binding)
            .finalize()
            .into_bytes(),
    )
}

/// Checks a listener's answer in constant time.
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

    fn code(host: &str) -> ServerCode {
        ServerCode {
            host: host.into(),
            port: 4070,
            secret: Secret::generate(),
            fingerprint: Fingerprint::of(b"certificate"),
        }
    }

    #[test]
    fn codes_round_trip_for_names_and_both_ip_families() {
        for host in ["203.0.113.7", "jam.example.org", "2001:db8::1", "vps"] {
            let code = code(host);
            assert_eq!(ServerCode::parse(&code.code()), Ok(code.clone()));
        }
    }

    #[test]
    fn malformed_codes_are_refused() {
        let secret = Secret::generate().to_text();
        let fingerprint = Fingerprint::of(b"x").to_text();
        for (text, error) in [
            ("host:4070".to_string(), CodeError::Incomplete),
            (format!("host:4070#{secret}"), CodeError::Incomplete),
            (
                format!("host:4070#{secret}#{fingerprint}#more"),
                CodeError::Incomplete,
            ),
            (
                format!("host:4070#short#{fingerprint}"),
                CodeError::BadSecret,
            ),
            (
                format!("host:4070#{secret}#short"),
                CodeError::BadFingerprint,
            ),
            (
                format!("host#{secret}#{fingerprint}"),
                CodeError::BadAddress,
            ),
            (
                format!("host:0#{secret}#{fingerprint}"),
                CodeError::BadAddress,
            ),
            (
                format!("2001:db8::1:4070#{secret}#{fingerprint}"),
                CodeError::BadAddress,
            ),
            (
                format!("ho st:4070#{secret}#{fingerprint}"),
                CodeError::BadAddress,
            ),
        ] {
            assert_eq!(ServerCode::parse(&text), Err(error), "{text}");
        }
    }

    #[test]
    fn the_password_is_random_and_never_printed() {
        let code = code("vps");
        assert_ne!(code.secret, Secret::generate());
        assert_eq!(
            Secret::from_text(&code.secret.to_text()),
            Some(code.secret.clone())
        );
        let debug = format!("{code:?}");
        assert!(!debug.contains(&code.secret.to_text()), "{debug}");
    }

    #[test]
    fn the_proof_holds_only_for_its_secret_nonce_name_and_channel() {
        let secret = Secret::generate();
        let nonce = new_nonce();
        assert_ne!(nonce, new_nonce());
        let good = proof(&secret, &nonce, "Ana", b"tls");
        assert!(verify(&secret, &nonce, "Ana", b"tls", &good));
        assert!(!verify(&Secret::generate(), &nonce, "Ana", b"tls", &good));
        assert!(!verify(&secret, &new_nonce(), "Ana", b"tls", &good));
        assert!(!verify(&secret, &nonce, "Bob", b"tls", &good));
        assert!(!verify(&secret, &nonce, "Ana", b"another channel", &good));
        assert!(!verify(&secret, &nonce, "Ana", b"tls", "not base64!"));
        assert!(!verify(&secret, &nonce, "Ana", b"tls", ""));
        // The length prefixes keep shifted boundaries apart.
        let shifted = proof(&secret, &format!("{nonce}A"), "na", b"tls");
        assert!(!verify(&secret, &nonce, "Ana", b"tls", &shifted));
    }
}
