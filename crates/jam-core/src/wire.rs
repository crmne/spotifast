//! What the server and the app share on the wire: bounded lines, writes
//! that cannot hang, a rate limit, and the timings that hold them.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::{Instant, timeout};

use crate::protocol::MAX_LINE_BYTES;

/// Timings and limits. The defaults suit a jam; tests shorten them.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Connecting, the TLS handshake, each step of the jam's handshake, and
    /// any single write.
    pub handshake: Duration,
    /// A listener silent this long is let go; listeners ping well within it.
    pub listener_idle: Duration,
    /// A server silent this long is presumed gone.
    pub server_idle: Duration,
    pub ping_every: Duration,
    /// How often the server checks whether the song ended.
    pub tick_every: Duration,
    /// Connections at once, those still in the handshake included.
    pub max_connections: usize,
    /// Messages a listener may send at once, and per second after that.
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
            listener_idle: Duration::from_secs(30),
            server_idle: Duration::from_secs(15),
            ping_every: Duration::from_secs(3),
            tick_every: Duration::from_millis(250),
            max_connections: 64,
            burst: 40,
            per_second: 20,
            reconnect_attempts: 8,
            reconnect_first: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(30),
        }
    }
}

/// Reads newline-terminated lines no longer than the protocol allows.
///
/// Cancel safe, unlike `read_line`: a line read in part waits in `line`
/// for the next call, so it can sit in a `select!`.
pub struct LineReader<R> {
    reader: BufReader<R>,
    line: Vec<u8>,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            line: Vec::new(),
        }
    }

    /// The next line, `None` at the end of the stream. A line over the
    /// limit is an error, found before it is held in full.
    pub async fn next_line(&mut self) -> io::Result<Option<String>> {
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

/// Writes one line and flushes it. A peer that stops reading must not hold
/// a writer forever, so the write gives up after `limits.handshake`.
pub async fn write_line<W: AsyncWrite + Unpin>(
    write: &mut W,
    line: &str,
    limits: Limits,
) -> io::Result<()> {
    timeout(limits.handshake, async {
        write.write_all(line.as_bytes()).await?;
        write.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "write timed out"))?
}

/// Allows `burst` messages at once, then `per_second`.
pub struct TokenBucket {
    tokens: f64,
    burst: f64,
    per_second: f64,
    at: Instant,
}

impl TokenBucket {
    pub fn new(burst: u32, per_second: u32) -> Self {
        Self {
            tokens: f64::from(burst),
            burst: f64::from(burst),
            per_second: f64::from(per_second),
            at: Instant::now(),
        }
    }

    pub fn take(&mut self, now: Instant) -> bool {
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
    use super::*;

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

    #[tokio::test]
    async fn an_endless_line_is_refused_before_it_is_held() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut lines = LineReader::new(server);
        tokio::spawn(async move {
            let chunk = vec![b'x'; 16 * 1024];
            while client.write_all(&chunk).await.is_ok() {}
        });
        let error = lines.next_line().await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
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
