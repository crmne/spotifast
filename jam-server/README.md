# jam-server

Keeps one Spotifast jam running for everyone who has its server code. It
holds the shared queue, the playing song and its position; each listener's
Spotifast plays the songs on their own Spotify account. No audio and no
Spotify credential ever reach the server. See the [Jams guide](../docs/_guide/jam.md)
for what listeners see.

## With Docker

From this directory, on the VPS:

```sh
docker compose up -d
docker compose run --rm jam code
```

The image compiles the server alone, not the app. Open TCP port 4070. The
`jam-data` volume holds:

| File | What it is |
|---|---|
| `secret` | The password, 128 random bits, drawn on first start |
| `cert.pem`, `key.pem` | The TLS certificate and its key, made on first start |
| `state.json` | The jam: the queue and the playing song, saved within a second of each change |

Keep the volume to keep the same server code. To update, pull the
repository and run `docker compose up -d --build`.

## Without Docker

```sh
cargo build --release -p jam-server
mkdir data
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -days 3650 -subj /CN=spotifast-jam -keyout data/key.pem -out data/cert.pem
./target/release/jam-server --data data serve --listen 0.0.0.0:4070
./target/release/jam-server --data data code
```

`serve` stops cleanly on Ctrl+C or SIGTERM, saving the jam first.

## The code

`jam-server code` prints `password#fingerprint`. Listeners enter it in
Spotifast beside the server's address, `host` or `host:port` (4070 unless
`serve --listen` says otherwise). The fingerprint is
the SHA-256 of the certificate: Spotifast trusts that certificate and no
other, so no domain name or certificate authority is needed. A new
certificate, or a new password, means a new code for everyone.

The server never writes the password to its log. Only `code`, run on
purpose, prints it.

## Limits

At most 64 connections at once and 32 listeners, 500 queued songs and 50
waiting songs per listener. A connection must finish its handshakes within
ten seconds; a listener silent for thirty seconds, or sending more than 20
messages a second after a burst of 40, is disconnected. Every message is
bounded and validated before use; see `crates/jam-core`.
