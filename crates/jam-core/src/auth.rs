//! Where the jam server is, its code, and how each side proves itself to the
//! other.
//!
//! A listener enters two things: the server's address (`host` or
//! `host:port`), and the server's code, `password#fingerprint`. The password
//! is 128 random bits drawn by the server, never chosen by a person, so a
//! captured handshake cannot be guessed back to it. The fingerprint is the
//! SHA-256 of the server's TLS certificate: the app accepts that certificate
//! and no other, wherever the address points, which needs neither a domain
//! name nor a certificate authority.
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

/// The port a jam server listens on unless its address says otherwise.
pub const DEFAULT_PORT: u16 = 4070;

/// Everything needed to join: where the server is, the certificate it must
/// show, and the password it expects.
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
    #[error("the server's address is invalid")]
    BadAddress,
    #[error("the server code is incomplete")]
    Incomplete,
    #[error("the server code's password is invalid")]
    BadSecret,
    #[error("the server code's fingerprint is invalid")]
    BadFingerprint,
}

impl ServerCode {
    /// From what a listener entered: the server's address and its code.
    pub fn from_parts(address: &str, code: &str) -> Result<Self, CodeError> {
        let (host, port) = parse_address(address)?;
        let (secret, fingerprint) = parse_code(code)?;
        Ok(Self {
            host,
            port,
            secret,
            fingerprint,
        })
    }

    /// The code to give the people who may join, `password#fingerprint`.
    /// It grants entry: share it privately, and never write it to a log.
    pub fn code(&self) -> String {
        access_code(&self.secret, self.fingerprint)
    }

    /// The address as a listener would enter it.
    pub fn address(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{host}:{}", self.port)
    }
}

/// The server code for a password and a certificate fingerprint.
pub fn access_code(secret: &Secret, fingerprint: Fingerprint) -> String {
    format!("{}#{}", secret.to_text(), fingerprint.to_text())
}

/// `password#fingerprint`.
fn parse_code(code: &str) -> Result<(Secret, Fingerprint), CodeError> {
    let (secret, fingerprint) = code.trim().split_once('#').ok_or(CodeError::Incomplete)?;
    let secret = Secret::from_text(secret).ok_or(CodeError::BadSecret)?;
    let fingerprint = Fingerprint::from_text(fingerprint).ok_or(CodeError::BadFingerprint)?;
    Ok((secret, fingerprint))
}

/// `host`, `host:port`, `[ipv6]`, `[ipv6]:port`, or a bare IPv6 address.
/// Without a port, the server is taken to listen on [`DEFAULT_PORT`].
fn parse_address(address: &str) -> Result<(String, u16), CodeError> {
    let address = address.trim();
    let (host, port) = if let Some(rest) = address.strip_prefix('[') {
        match rest.split_once(']') {
            Some((host, "")) => (host, None),
            Some((host, port)) => (
                host,
                Some(port.strip_prefix(':').ok_or(CodeError::BadAddress)?),
            ),
            None => return Err(CodeError::BadAddress),
        }
    } else if address.matches(':').count() > 1 {
        // Several colons and no brackets: an IPv6 address alone.
        (address, None)
    } else {
        match address.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (address, None),
        }
    };
    let port = match port {
        Some(port) => port.parse().map_err(|_| CodeError::BadAddress)?,
        None => DEFAULT_PORT,
    };
    let host_valid = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-:%".contains(&byte));
    if !host_valid || port == 0 {
        return Err(CodeError::BadAddress);
    }
    Ok((host.to_string(), port))
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
    fn address_and_code_round_trip_for_names_and_both_ip_families() {
        for host in ["203.0.113.7", "jam.example.org", "2001:db8::1", "vps"] {
            let code = code(host);
            assert_eq!(
                ServerCode::from_parts(&code.address(), &code.code()),
                Ok(code.clone())
            );
        }
    }

    #[test]
    fn an_address_without_a_port_means_the_default_one() {
        let text = code("vps").code();
        for (address, host, port) in [
            ("jam.example.org", "jam.example.org", DEFAULT_PORT),
            ("  jam.example.org:5000 ", "jam.example.org", 5000),
            ("203.0.113.7", "203.0.113.7", DEFAULT_PORT),
            ("2001:db8::1", "2001:db8::1", DEFAULT_PORT),
            ("[2001:db8::1]", "2001:db8::1", DEFAULT_PORT),
            ("[2001:db8::1]:5000", "2001:db8::1", 5000),
        ] {
            let parsed = ServerCode::from_parts(address, &text).unwrap();
            assert_eq!(
                (parsed.host.as_str(), parsed.port),
                (host, port),
                "{address}"
            );
        }
    }

    #[test]
    fn malformed_addresses_and_codes_are_refused() {
        let good = code("vps").code();
        for address in [
            "",
            "host:0",
            "host:99999",
            "host:port",
            "ho st",
            "[::1]x",
            "[::1",
        ] {
            assert_eq!(
                ServerCode::from_parts(address, &good),
                Err(CodeError::BadAddress),
                "{address:?}"
            );
        }
        let secret = Secret::generate().to_text();
        let fingerprint = Fingerprint::of(b"x").to_text();
        for (text, error) in [
            (secret.clone(), CodeError::Incomplete),
            (format!("short#{fingerprint}"), CodeError::BadSecret),
            (format!("{secret}#short"), CodeError::BadFingerprint),
            (
                format!("{secret}#{fingerprint}#more"),
                CodeError::BadFingerprint,
            ),
            // The old combined form, address first, is not a code.
            (
                format!("vps:4070#{secret}#{fingerprint}"),
                CodeError::BadSecret,
            ),
        ] {
            assert_eq!(ServerCode::from_parts("vps", &text), Err(error), "{text}");
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
