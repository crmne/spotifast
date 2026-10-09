---
title: What Spotify Lets a Client Do
description: What Spotify's Web API and librespot provide, and what neither supports.
nav_order: 3
---

Spotifast uses Spotify's
[Web API](https://developer.spotify.com/documentation/web-api) for account and
catalogue data. It uses [librespot](https://github.com/librespot-org/librespot)
for a few extra details and for audio playback.

Features missing from both cannot be added to Spotifast. They may become
possible if Spotify adds an API or librespot adds lawful support.

## Web API

Spotifast uses the Web API for:

- **Account:** profile details, followed artists, top artists and tracks, and
  the last fifty plays. Spotifast keeps a longer local
  [history](/using-spotifast/#recent).
- **Library:** playlists, saved tracks, albums, shows, and episodes. It can
  also save and remove items.
- **Playlists:** reading, creating, renaming, changing the description and
  visibility, adding and removing songs, reordering songs, and following and
  unfollowing. Custom playlist cover uploads are available since 0.9.0.
- **Catalogue:** albums, artists, tracks, shows, episodes, search, and
  recommendations. Artist pages include top tracks, releases, and related
  artists.
- **Playback control:** listing devices, transferring playback, play, pause,
  next, previous, seek, shuffle, repeat, volume, and reading or adding to the
  queue.

Users of the default app share Spotify's quota. Spotifast limits heavy
requests and pauses a session when Spotify sends a `Retry-After` response.

Spotify also limits apps created since November 2024. These apps cannot access
Spotify-owned playlists, related artists, recommendations, or audio features.
This is why a personal app cannot handle every request. Complete playlist
library views stay on the shared app. Playlists other people own, and every
playlist when there is no personal app, are read over the librespot session
while local playback is signed in. See [How It Connects](/how-it-connects/).

## librespot session

librespot signs in to Spotify and uses the same protocol as Spotify's own
clients. Spotifast uses its session for:

- **Playlist folders and order.** Spotifast can read them from Spotify's
  rootlist. librespot cannot create, rename, or move folders.
- **Playlist permissions.** The rootlist shows when a playlist shared by
  invitation can be edited. The Web API's `collaborative` flag does not cover
  these playlists. Spotifast cannot manage collaborators.
- **Playlists the shared app would otherwise serve.** Title, cover, and
  songs of other people's playlists, and of the account's own when there is
  no personal app, so they open without the shared app's quota. Whether a
  playlist is public, the owner's name when the session gives none, and the
  mosaic of a playlist without a cover of its own, come from the Web API's
  library list. These rows carry
  no per-market availability flag. A song the Web API had already greyed
  out, on an earlier page or in the cache of an earlier visit, stays greyed
  out when the session reads the rows. This knowledge belongs to the signed-in
  account, and a newer Web API answer takes precedence over an older disk
  cache. A song the session reads first has unknown availability; if it cannot
  play, it is skipped when reached, as it would be anywhere else.
- **Lyrics** when Spotify has them.
- **Artist popular tracks.** The account's market-specific ranking and batched
  track details, with the shared Web API as fallback when the session cannot
  supply them. This uses librespot's existing metadata support.
- **Display names** for the user IDs attached to songs in a playlist.
- **Precise EP types** for releases that the Web API groups with singles.
- **Radio and autoplay** through Spotify's context resolver: stations seeded
  by a song, playlist, album, or artist. Each resolution is a fresh mix of 50
  songs, so a radio page plays the songs it shows rather than asking again.
- **Audiobook detection** for saved shows, which the Web API lists as podcasts.

## librespot playback

librespot provides:

- Spotify catalogue playback at up to 320 kbps.
- Gapless playback, normalisation, and a local audio cache.
- Spotify Connect, so another Spotify client can transfer playback to this
  computer.
- Shuffle, repeat, seek, and volume.
- Songs and podcast episodes.

Spotify Premium is required. librespot cannot play audio with a free account.

Spotifast uses a small librespot fork. Its patches add queue controls,
normalisation data for the visualisers, and an event for rejected audio keys.
They are listed in `Cargo.toml`. Larger changes go upstream first.

## Not available

The Web API and librespot do not provide these features:

- **Pins shared with the Spotify app.** Spotify stores pins in its private
  `your-library` service. There is no Web API for it, and librespot does not
  support its protocol. Spotifast's pins are local and are stored in
  `settings.json`. See [issue #31](https://github.com/crmne/spotifast/issues/31).
- **Editing playlist folders.** librespot can only read them.
- **Smart Shuffle, Jam, Blend, and similar Spotify features.** Spotify
  generates these for its own clients. Spotifast only has plain shuffle.
- **Lossless audio.** librespot does not receive lossless streams. Spotifast
  will reconsider this if librespot gains lawful support, but it will not
  bypass Spotify's DRM.
- **Local files.** librespot only streams Spotify's catalogue. It cannot fetch
  audio for a `spotify:local:` entry. Playing files from disk would require a
  separate player. See [issue #3](https://github.com/crmne/spotifast/issues/3).
- **Audiobooks.** librespot does not play them. Spotify lists some
  audiobooks among saved shows; since 0.10.0, Spotifast asks the
  librespot session which ones and leaves them out of the Podcasts shelf.
- **Google Cast.** Spotify's own apps find Cast speakers on the local
  network and start Spotify's receiver on them. librespot has no Cast
  sender, and the Web API only lists a Cast device once a Spotify app has
  already woken it, so Spotifast cannot discover or start one itself. See
  [issue #566](https://github.com/crmne/spotifast/issues/566).
- **Offline listening and downloads.** Spotify's DRM and the project's scope
  rule these out.
- **Playback speed and crossfade.** librespot supports neither. Spotifast
  would have to add them to its own audio path.
- **Free-account playback.** Replacing Spotify audio with another source is
  also out of scope. See the
  [contribution guide](https://github.com/crmne/spotifast/blob/main/CONTRIBUTING.md).
- **Friend activity, private-session status, and similar social features.**
  Spotify has no public API for them.
- **Canvas videos and video podcasts.** librespot does not provide them.
- **Play counts.** Spotify shows them only through a private endpoint its own
  apps use. The Web API has no play counts, and librespot does not provide
  them. See [issue #543](https://github.com/crmne/spotifast/issues/543).
