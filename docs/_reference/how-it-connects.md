---
title: How It Connects
description: Independent Spotify grants, what is stored, and how API traffic is routed.
nav_order: 1
---

## Independent grants, once each

Spotifast uses separate credentials for Web API access, a personal app, and
local playback:

1. **The shared Web API app** keeps full catalogue and playlist coverage.
2. **Your optional personal Web API app** handles supported playback, library,
   catalog, catalogue search, playlist creation, and owned or collaborative
   playlist requests without using the shared app's quota. Complete
   playlist-library views and the playlist half of a search stay on the shared
   app so Spotify-owned results are not filtered out. Both Web API grants must
   verify as the same Spotify account.
3. **Local playback** uses
   [librespot](https://github.com/librespot-org/librespot). It needs one more
   browser approval and keeps an independent reusable credential. Spotify Premium
   is required. While it is signed in, its session also reads the playlists
   the shared app would otherwise be asked for: other people's, and the
   account's own when there is no personal app.

Local playback authorization stays separate from both Web API grants. Its
browser approval requests only the streaming permission and always shows the
consent dialog. The playback session uses the account ID verified by either
Web API grant. A verified personal app can complete sign-in while the shared
app's verification is still waiting.

Since 0.8.0, local playback retains the artist IDs
already supplied by librespot. Artist links in the player bar work before the
Web API's track metadata arrives, without an extra request.

Since 0.8.0, local seeks discard audio queued from the previous
position once librespot confirms the seek. This also applies when another
Spotify client controls playback on this computer. Natural track transitions
retain their queue for gapless playback. The seek still waits for librespot to
find and fetch the requested audio, and sound already handed to the device
cannot be recalled. Seeking adds no Web API request or full-track download.

Since 0.8.0, requests that need a grant still being
verified wait for it instead of showing "not signed in". Sign-out cancels
pending requests, and their late results cannot undo a new sign-in. If Spotify
rejects a saved refresh grant, Spotifast removes that grant and asks for a new
browser approval. Upgrading to protected storage does not itself require
signing in again.

By default, Spotifast uses the public app shared with spotify-player, ncspot,
and Omarchy Spotify. Spotify divides its quota among all users. A personal app
adds a separate Development Mode quota. See
[Use a Personal Spotify App](/make-it-even-faster/).

Since 0.8.0, a search runs as two requests when a
personal app is ready: songs, artists, albums, podcasts, and episodes on the
personal app, and playlists on the shared app. This moves catalogue search off the quota Spotify divides among every user
of the shared app. Each half is shown the moment it
arrives, so a shared app waiting out a rate limit no longer holds up the songs,
and playlists appear underneath when that wait ends. A half belonging to an
earlier search is discarded rather than shown beside a newer one. A newer or cleared search cancels the previous requests, including waits
for shared access. If either half fails, the successful results remain usable
with an error for the failed part. A new query starts a fresh result set; old
songs never appear under its name. The loading indicator remains until both
parts finish. A personal app answers
with ten results for each type where the shared app answers with twenty, because
Development Mode rejects the larger page. Without a personal app, one request
still asks for all six types and nothing about a search changes.

Since 0.8.0, verified Premium accounts using shared access see a
one-time introduction to that option. Setup and dismissal are remembered in
settings. The prompt uses the existing account profile and adds no request.

Since 0.8.0, explicitly sorting a Library section loads its remaining
pages through the existing Web API grant, one at a time, while loaded entries
stay visible. A failed page stops automatic loading. Spotify custom playlist
order uses the existing account-scoped rootlist from local playback; sorting
and dragging never write that order back to Spotify.

## What the client stores

- Since 0.8.0, shared and personal Web API grants
  and the reusable playback credential use the platform credential store:
  Secret Service on Linux, Keychain on macOS, and Credential Manager on
  Windows. Librespot retains its reusable credential in memory; Spotifast
  owns persistence. Flatpak can talk to `org.freedesktop.secrets` for this.
  Version 0.7.1 still uses the older unencrypted files.
  See [migration, sign-out, and storage protection](/settings-and-files/).
- Downloaded audio and artwork, in the cache directory, within the budget
  you set.
- The first time MilkDrop opens with an empty preset folder, the two projectM
  preset packs are downloaded from GitHub (about 26 MB) and stored in the
  config directory.
- On Windows and macOS, desktop media controls load the cover themselves and
  are given a file, so the full-size artwork is downloaded into that cache
  when a song starts, even when no view on screen is showing it. Linux MPRIS
  carries the Spotify artwork URL for the desktop to resolve and asks for
  nothing extra.
- Lyrics, in the cache directory, for a month.
- Liked Songs metadata, scoped to the verified account, in the cache directory.
  This behavior is available since 0.8.0.
  Cached pages less than 15 minutes old need no repeat request. Older cached
  prefixes refresh through the existing Web API grant, one page at a time,
  while the saved rows remain visible. Manual refresh starts immediately.
  Like and Unlike are kept over lagging reads until Spotify confirms them.
- Spotifast has no telemetry, analytics, or hosted service. When the lyrics
  panel is open and Spotify has no lyrics, it sends the track's artist, title,
  album, and length to [lrclib.net](https://lrclib.net). It also checks
  api.github.com once a day for updates. You can turn off automatic checks in
  Settings, or request one there at any time. On macOS, **Check for Updates**
  is also in the application menu.

  On Windows, macOS, and Linux, downloading an update fetches release metadata and
  `checksums.txt` from the project's GitHub release, then the matching binary
  archive, Windows installer, or universal macOS DMG. Spotifast checks the published SHA-256 digest
  and the portable executable's reported version before offering a restart.
  Automatic downloads are optional; installation always waits for your click.
  Checks and downloads do not open the update popup. The green update pill opens
  it on request; closing the popup does not cancel a download.
  No Spotify credential is sent. These are GitHub-hosted checksums, not a
  separate publisher signature.

  Updates stage their files in a private `.fastpotify-update-*` directory beside
  the application so replacement stays on the same filesystem. The directory
  retains the previous executable or Mac app bundle and `result.txt` for recovery and diagnosis.
  Settings, caches and credential stores are not replaced. Package-manager
  installs keep their package-manager update path. Mac updates verify the bundle
  identifier, version and code signature before replacing the whole app bundle.
  A Developer ID installation also requires the same signing team and a passing
  macOS security assessment. Apps running from a disk image or an App Translocation
  directory must be moved to a writable installation directory first.

Since 0.8.0, album and playlist scrollbars can request a distant track
page through the existing session or Web API read path, without fetching all
preceding tracks. These reads run one at a time per list and retain the existing
rate-limit handling. Unloaded
rows are placeholders until their page arrives; scrolling never starts playback.

## When Spotify pushes back

Each Web API session has separate concurrency and rate limits. A `Retry-After`
response pauses only that session. Spotifast routes each request once and
does not retry it through the other app. A playlist read the librespot session
refuses outright, because the playlist is gone or private, is shown as such. A
dropped connection, a read that takes longer than 30 seconds, or a page whose
song details Spotify did not supply in full, hands the read to the Web API
instead of caching rows without songs. A song Spotify no longer has, or
withholds for legal reasons, is an empty row, as the Web API shows it.

Spotify can also explicitly refuse the key needed to decrypt a track. When
that happens, Spotifast stops local playback and leaves the rest of the queue
alone instead of treating every following track as unavailable. This refusal
comes from Spotify; trying again later may work.

Before adding songs to an existing playlist, Spotifast checks the rows it
already holds. A known duplicate produces an immediate confirmation naming the
song. Only a playlist that has not been fully loaded needs a background scan to
rule out duplicates. Once confirmed, the new rows appear locally at once.
Since 0.8.0,
a drop into an open editable playlist sends its chosen insertion position
through the same Web API grant. Duplicate checks and confirmation retain that
position; partial loaded pages keep the correct continuation offset. A
successful write advances the cached playlist to Spotify's returned snapshot
instead of downloading the playlist again. If Spotify cannot answer the scan,
Spotifast preserves the requested edit and lets the write report its result.

Since 0.8.0, manually reloading an edited playlist waits for all
pending writes and confirmation of the returned Spotify revision before
requesting replacement rows. Current rows, filtering, sorting, and selection
stay visible while it loads. Automatic paging also waits for those edits.
If metadata still reports an older revision after three immediate rechecks,
or the request fails, the page keeps the edits and offers a retry. Refreshing
again retries confirmation without losing the local changes. This uses the
existing playlist requests and adds no periodic polling.

## Playlist cover uploads

On `main`, after 0.8.0, **Edit details → Change cover** opens the native file
picker. Spotifast reads only the selected JPEG or PNG, preserves its aspect ratio, flattens transparent
pixels onto white, and encodes a JPEG preview. Files must be smaller than 20 MB
and no larger than 8192 pixels per side, within a 128 MB decoding budget.
Encoding reduces the image to fit
Spotify's 256 KB Base64 request limit. **Upload cover** sends that preview to
Spotify; **Save** separately saves the name, description, and visibility.

Uploads use the same shared or personal app routing as playlist edits. Requests
are not retried through another app. The uploaded image stays visible while
Spotify propagates its artwork. A changed URL can still contain an earlier
upload, so Spotifast checks the largest returned image through its normal
artwork cache, off the UI thread. Only matching image bytes or decoded pixels
replace the temporary preview. It makes at most three immediate metadata
rechecks; if Spotify is still catching up or the check fails, the preview stays
and a later page refresh can check again. Once confirmed, later cover changes
from other clients can appear. The selected source file is not copied to the
cache or settings.

Image uploads require renewed Web API consent. If Spotifast asks you to sign
in again after updating, approve the image upload permission. Reconnect your
personal app in Settings too, if you use one. Local playback authorization is
unchanged. Spotify can refuse changes to playlists you do not own.

On Linux the file picker uses a desktop portal, with Zenity as a fallback.
Install your desktop's file chooser portal or Zenity if no picker opens.

## Receivers on the local network

Spotify's device list only shows signed-in receivers. A new librespot or
spotifyd receiver is therefore invisible to the Web API.

Receivers announce themselves over mDNS as `_spotify-connect._tcp` and answer
a small HTTP interface. Opening or refreshing the picker first reads
`getInfo` to find each receiver's name and device ID. These probes run off
the UI thread, four at a time, with a two-second limit per receiver and six
seconds overall after discovery. Only responding receivers with a name and
ID are offered. Matching IDs are combined; separate devices can have the
same name. These reads send no account credential.

When a receiver is selected, Spotifast encrypts the stored librespot credential
with a receiver-specific key and a key from a Diffie-Hellman exchange. The
encrypted value only works for that receiver and exchange. Spotifast does not
save another copy of the credential.

The receiver then signs in and appears in Spotify's device list. Spotifast
uses the Web API for subsequent control requests.

## The engine

Playback runs on a separate runtime. Librespot maintains the Spotify Connect
session, exposes this computer as a device, receives transfers, and reports
playback state. If the session drops, it reconnects with the stored credential.
On Linux, its PulseAudio or PipeWire stream is named **Spotifast**, with
**Spotify playback** as its description, so system mixers and audio processors
can identify and route it. Explicit `PULSE_PROP_application.name` and
`PULSE_PROP_stream.description` environment values take precedence.
The same session checks releases that the Web API calls `single`, so confirmed
EPs can carry their precise label. Spotifast deduplicates these checks while
the app session is active. If the engine reconnects, an interrupted check may
be tried again; if metadata is unavailable, its label stays `Single`.

The engine discovers access points through `apresolve.spotify.com` and
connects over TCP in the resolver's preference order: port 4070 first,
falling back to 443 and 80. Only outbound connections are needed; no
inbound ports have to be open.

Each access-point attempt gives socket setup and the handshake a combined
five seconds. A stalled TCP connection or HTTP proxy tunnel therefore lets
librespot retry and move on to another endpoint instead of waiting for the
operating system's longer connection timeout.

Since 0.8.0, the access-point and Dealer TCP connectors resolve
names off the playback runtime thread and try all returned addresses. If
the preferred IPv4 or IPv6 route stalls, the other family starts after
300 ms. DNS, TCP setup and any HTTP proxy tunnel share a five-second limit.
The access-point handshake still shares its existing five-second budget;
the Dealer's WebSocket TLS verification is unchanged.

When a proxy is configured, only its name is resolved locally. The target
name is sent through CONNECT, and a proxy failure never falls back to a
direct connection. Proxy URLs and credentials are not logged by this
connector. The change adds no destination or background polling.

## Proxy

On `main`, after 0.8.0, Settings → Proxy has four modes:

- **Off**: a direct connection. Environment proxy variables are ignored.
- **System**: `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY`, and on macOS and
  Windows the OS proxy. This is the default.
- **HTTP**: a configured HTTP proxy: host, port, and optional login.
- **SOCKS5**: a configured SOCKS5 proxy: host, port, and optional login.
  Spotify hostnames are resolved by that proxy.

The mode selects the protocol. Host and port are separate fields.
These settings apply to Spotifast's requests. The external browser used
for Spotify approval keeps its own network and proxy settings.

The Web API, artwork, lyrics, update checks and downloads, and MilkDrop preset
downloads follow that mode. Update downloads retain their release-host redirect
restrictions and checksum verification. Local receivers on the LAN are never sent through a proxy.

Local playback can only use an unauthenticated, plaintext HTTP proxy: that is
what librespot's CONNECT client supports. Proxy login still covers catalogue
and control, but the engine then connects to Spotify directly. SOCKS5 behaves
the same way for local playback. System uses the same proxy reqwest would
(environment variables, and the OS proxy on macOS and Windows) only when it
is an unauthenticated `http://` endpoint; authenticated, `https://`, and SOCKS
system proxies are ignored by the engine. Applying a proxy choice restarts
local playback when it changes the endpoint used by the current connection,
including when the system proxy has changed since that connection started.

Off and System apply immediately. HTTP and SOCKS5 apply when you press
**Apply settings**. The same choice is on the sign-in screen, so it can be
set before the first grant. The expanded sign-in form scrolls in short windows.
Manual edits remain drafts until the backend
accepts Apply or Sign in. Saving another preference does not save those drafts.
A configuration that cannot be built produces an error without silently
switching to a direct connection. Applying a valid configuration repairs it.

On startup, network work waits for the protected proxy password to be restored.
That lookup runs on the credential worker and does not block the interface or
shutdown. The password belongs to its host, port, and username; editing any of
these fields clears it. See [password storage and migration](/settings-and-files/).
