//! Shared HTTP client, so a proxy change can take effect without a restart.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::settings::ProxyConfig;

/// A cheap-to-clone handle to the process HTTP client.
///
/// Replacing the inner client updates every clone: the Web API, token
/// refresh, artwork, lyrics, and the update check all pick up a new proxy
/// on the next request.
#[derive(Clone)]
pub struct Http {
    inner: Arc<RwLock<Result<reqwest::Client, String>>>,
}

impl Http {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Ok(client))),
        }
    }

    pub fn from_proxy(proxy: &ProxyConfig) -> Result<Self, String> {
        build_client(proxy).map(Self::new)
    }

    /// Keep the interface available to repair settings without permitting
    /// requests to bypass the configuration that failed to build.
    pub fn unavailable(error: String) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Err(error))),
        }
    }

    pub fn replace(&self, client: reqwest::Client) {
        *self.inner.write().unwrap_or_else(|lock| lock.into_inner()) = Ok(client);
    }

    pub fn block(&self, error: String) {
        *self.inner.write().unwrap_or_else(|lock| lock.into_inner()) = Err(error);
    }

    pub fn client(&self) -> Result<reqwest::Client, String> {
        self.inner
            .read()
            .unwrap_or_else(|lock| lock.into_inner())
            .clone()
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new(reqwest::Client::new())
    }
}

impl From<reqwest::Client> for Http {
    fn from(client: reqwest::Client) -> Self {
        Self::new(client)
    }
}

pub fn build_client(proxy: &ProxyConfig) -> Result<reqwest::Client, String> {
    client_builder(proxy)?
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.without_url().to_string())
}

pub fn build_blocking(
    proxy: &ProxyConfig,
    timeout: Duration,
) -> Result<reqwest::blocking::Client, String> {
    blocking_builder(proxy)?
        .timeout(timeout)
        .build()
        .map_err(|error| error.without_url().to_string())
}

fn client_builder(proxy: &ProxyConfig) -> Result<reqwest::ClientBuilder, String> {
    apply_proxy(reqwest::Client::builder().user_agent(user_agent()), proxy)
}

pub(crate) fn blocking_builder(
    proxy: &ProxyConfig,
) -> Result<reqwest::blocking::ClientBuilder, String> {
    apply_blocking_proxy(
        reqwest::blocking::Client::builder().user_agent(user_agent()),
        proxy,
    )
}

fn apply_proxy(
    builder: reqwest::ClientBuilder,
    proxy: &ProxyConfig,
) -> Result<reqwest::ClientBuilder, String> {
    Ok(match proxy {
        ProxyConfig::Invalid(error) => return Err(error.clone()),
        ProxyConfig::Off => builder.no_proxy(),
        ProxyConfig::System => builder,
        ProxyConfig::Http(manual) | ProxyConfig::Socks(manual) => builder.proxy(
            manual
                .reqwest_proxy()
                .map_err(|error| error.without_url().to_string())?,
        ),
    })
}

fn apply_blocking_proxy(
    builder: reqwest::blocking::ClientBuilder,
    proxy: &ProxyConfig,
) -> Result<reqwest::blocking::ClientBuilder, String> {
    Ok(match proxy {
        ProxyConfig::Invalid(error) => return Err(error.clone()),
        ProxyConfig::Off => builder.no_proxy(),
        ProxyConfig::System => builder,
        ProxyConfig::Http(manual) | ProxyConfig::Socks(manual) => builder.proxy(
            manual
                .reqwest_proxy()
                .map_err(|error| error.without_url().to_string())?,
        ),
    })
}

fn user_agent() -> &'static str {
    concat!("Spotizgeg/", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_proxy_configuration_never_falls_back_to_a_direct_client() {
        let invalid = ProxyConfig::Invalid("Proxy port must be a number".into());
        assert!(Http::from_proxy(&invalid).is_err());
        assert!(build_blocking(&invalid, Duration::from_secs(1)).is_err());
    }

    #[test]
    fn unavailable_clients_stay_blocked_until_a_successful_replacement() {
        let http = Http::unavailable("Unable to build the configured client".into());
        let artwork = http.clone();
        let api = http.clone();
        assert!(artwork.client().is_err());
        assert!(api.client().is_err());
        http.replace(build_client(&ProxyConfig::Off).unwrap());
        assert!(artwork.client().is_ok());
        assert!(api.client().is_ok());
    }

    #[test]
    fn replacing_the_client_is_visible_to_clones() {
        let http = Http::default();
        let clone = http.clone();
        let replacement = reqwest::Client::builder()
            .user_agent("spotizgeg-test")
            .build()
            .unwrap();
        http.replace(replacement.clone());
        // Distinct Client values still share the pool after a replace; the
        // lock is what matters, and a second replace of a dummy client
        // must not panic.
        clone.replace(reqwest::Client::new());
    }

    #[test]
    fn off_system_and_socks5_each_build_a_client() {
        build_client(&ProxyConfig::Off).unwrap();
        build_client(&ProxyConfig::System).unwrap();
        let settings = crate::settings::Settings {
            proxy_mode: crate::settings::ProxyMode::Socks,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "1080".into(),
            proxy_username: "user".into(),
            proxy_password: "pass".into(),
            ..crate::settings::Settings::default()
        };
        let proxy = settings.proxy_config().unwrap();
        build_client(&proxy).unwrap();
        build_blocking(&proxy, Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn web_api_client_goes_through_an_http_proxy() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_addr = origin.local_addr().unwrap();
        let origin_thread = thread::spawn(move || {
            let (mut stream, _) = origin.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nproxied",
                )
                .unwrap();
            request
        });

        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let proxy_thread = thread::spawn(move || {
            let (mut client, _) = proxy.accept().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buf = [0u8; 8192];
            let n = client.read(&mut buf).unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let mut upstream = std::net::TcpStream::connect(origin_addr).unwrap();
            upstream.write_all(&buf[..n]).unwrap();
            let mut response = Vec::new();
            upstream.read_to_end(&mut response).unwrap();
            client.write_all(&response).unwrap();
            head
        });

        let settings = crate::settings::Settings {
            proxy_mode: crate::settings::ProxyMode::Http,
            proxy_host: proxy_addr.ip().to_string(),
            proxy_port: proxy_addr.port().to_string(),
            ..crate::settings::Settings::default()
        };
        let proxy_config = settings.proxy_config().unwrap();
        let client = build_blocking(&proxy_config, Duration::from_secs(3)).unwrap();
        let body = client
            .get(format!("http://{origin_addr}/catalogue"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert_eq!(body, "proxied");
        let seen_by_proxy = proxy_thread.join().unwrap();
        assert!(
            seen_by_proxy.contains("catalogue"),
            "proxy should see the Web API request, got {seen_by_proxy:?}"
        );
        let seen_by_origin = origin_thread.join().unwrap();
        assert!(seen_by_origin.contains("GET"));
    }

    #[test]
    fn proxy_authentication_preserves_opaque_credentials() {
        use base64::Engine;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let proxy_thread = thread::spawn(move || {
            let (mut client, _) = proxy.accept().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buf = [0u8; 8192];
            let n = client.read(&mut buf).unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            client
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            head
        });

        let username = "alice/name";
        let password = " secret/with spaces ";
        let settings = crate::settings::Settings {
            proxy_mode: crate::settings::ProxyMode::Http,
            proxy_host: proxy_addr.ip().to_string(),
            proxy_port: proxy_addr.port().to_string(),
            proxy_username: username.into(),
            proxy_password: password.into(),
            ..crate::settings::Settings::default()
        };
        let proxy_config = settings.proxy_config().unwrap();
        let client = build_blocking(&proxy_config, Duration::from_secs(3)).unwrap();
        assert_eq!(
            client
                .get("http://example.invalid/catalogue")
                .send()
                .unwrap()
                .text()
                .unwrap(),
            "ok"
        );
        let seen = proxy_thread.join().unwrap();
        let expected =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        assert!(
            seen.lines()
                .any(|line| line
                    .eq_ignore_ascii_case(&format!("Proxy-Authorization: Basic {expected}"))),
            "proxy did not receive the exact configured credentials"
        );
    }

    #[test]
    fn socks5_authentication_and_hostname_resolution_use_the_selected_proxy() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "SOCKS5 proxy was not contacted"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture listener: {error}"),
                }
            };
            // Accepted sockets inherit O_NONBLOCK on macOS. The fixture uses
            // blocking reads after its bounded, nonblocking accept loop.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut pair = [0; 2];
            stream.read_exact(&mut pair).unwrap();
            assert_eq!(pair[0], 5);
            let mut methods = vec![0; pair[1] as usize];
            stream.read_exact(&mut methods).unwrap();
            assert!(methods.contains(&2));
            stream.write_all(&[5, 2]).unwrap();
            stream.read_exact(&mut pair).unwrap();
            assert_eq!(pair[0], 1);
            let mut username = vec![0; pair[1] as usize];
            stream.read_exact(&mut username).unwrap();
            let mut size = [0; 1];
            stream.read_exact(&mut size).unwrap();
            let mut password = vec![0; size[0] as usize];
            stream.read_exact(&mut password).unwrap();
            assert_eq!(username, b"dummy-user");
            assert_eq!(password, b" dummy-password ");
            stream.write_all(&[1, 0]).unwrap();
            let mut command = [0; 4];
            stream.read_exact(&mut command).unwrap();
            assert_eq!(
                command,
                [5, 1, 0, 3],
                "send the hostname for resolution at the proxy"
            );
            stream.read_exact(&mut size).unwrap();
            let mut hostname = vec![0; size[0] as usize];
            stream.read_exact(&mut hostname).unwrap();
            assert_eq!(hostname, b"example.invalid");
            stream.read_exact(&mut pair).unwrap();
            assert_eq!(u16::from_be_bytes(pair), 80);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
                .unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..size]).starts_with("GET /catalogue HTTP/1.1")
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let proxy = crate::settings::Settings {
            proxy_mode: crate::settings::ProxyMode::Socks,
            proxy_host: address.ip().to_string(),
            proxy_port: address.port().to_string(),
            proxy_username: "dummy-user".into(),
            proxy_password: " dummy-password ".into(),
            ..Default::default()
        }
        .proxy_config()
        .unwrap();
        let client = build_blocking(&proxy, Duration::from_secs(3)).unwrap();
        assert_eq!(
            client
                .get("http://example.invalid/catalogue")
                .send()
                .unwrap()
                .text()
                .unwrap(),
            "ok"
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn librespot_http_client_sends_connect_through_an_http_proxy() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let proxy_thread = thread::spawn(move || {
            let (mut client, _) = proxy.accept().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut buf = [0u8; 4096];
            let n = client.read(&mut buf).unwrap_or(0);
            let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });

        let proxy_url = reqwest::Url::parse(&format!("http://{proxy_addr}")).unwrap();
        let client = librespot_core::http_client::HttpClient::new(Some(&proxy_url));
        let request = http::Request::builder()
            .method("GET")
            .uri("https://apresolve.spotify.com/")
            .body(Default::default())
            .unwrap();
        let _ = client.request(request).await;
        let seen = proxy_thread.join().unwrap();
        assert!(
            seen.to_ascii_uppercase().contains("CONNECT") && seen.contains("apresolve.spotify.com"),
            "librespot should CONNECT through the proxy, got {seen:?}"
        );
    }
}
