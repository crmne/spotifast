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
  unfollowing. The playlist view reports how many people saved a playlist.
  Custom playlist cover uploads are available since 0.9.0.
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
library views stay on the shared app, or come from the librespot session's
rootlist while the shared app cannot answer. Playlists other people own, and
every playlist when there is no personal app, are read over the librespot
session while local playback is signed in. See [How It Connects](/how-it-connects/).

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
- **Library playlists the Web API lists blank.** The library list gives
  some of Spotify's own playlists, "This Is" ones among them, no name,
  cover, description, or songs. Spotifast reads each one's header over the
  session once per sign-in to fill its row. Without local playback those
  rows stay blank.
- **Removing one copy of a repeated song.** The Web API removes a song from
  a playlist by URI, every copy at once, and ignores positions. When a list
  holds a song more than once and one copy is removed, Spotifast reads the
  list over the session and removes that row by index through the playlist
  service Spotify's own clients edit, so the other copies keep their place
  and dates. Without local playback it refuses that removal instead of taking
  every copy.
- **Lyrics** when Spotify has them.
- **Artist popular tracks.** The account's market-specific ranking and batched
  track details, with the shared Web API as fallback when the session cannot
  supply them. This uses librespot's existing metadata support.
- **Upcoming album pages.** Prerelease metadata supplies the real album mapping,
  cover and release time. Pathfinder supplies ordered preview rows and explicit
  playability; unpublished rows stay visible without entering playback. The
  session collection service reads pre-save state and writes it only after the
  user presses the button. No Web API grant implements this operation.
- **Display names and avatars** for the user IDs attached to songs in a
  playlist and for a playlist's owner, from the profile view Spotify's own
  clients read.
- **Precise EP types** for releases that the Web API groups with singles.
- **Made for you** through Spotify's personalized Home section. This private
  request uses the playback session and an already verified Web API grant;
  when it succeeds, it does not spend the shared app's playlist-search quota.
  If it fails, the shared Web API can supply approximate playlist-name search
  results in the same **Made for you** shelf. Spotify may change the private Home
  response without a Web API version change.
- **Your top mixes** through the same personalized Home read, from Spotify's
  top mixes section. Search cannot find these personal mixes, so there is no
  fallback: without local playback the shelf stays hidden.
- **New releases** from Spotify's own new-releases feed, the private
  Pathfinder query (`queryWhatsNewFeed`) behind the bell in Spotify's apps.
  It is only read; the request that marks its items as seen is never sent.
  When it fails, the Web API's followed artists and their latest albums and
  singles stand in, for up to 50 artists. The songs on the New releases page
  come from two batched metadata reads over the session.
- **Hide in this playlist** for songs in Spotify's own playlists, such as
  Discover Weekly and the other mixes made for you, through the session's
  collection service: the account's `playcontextbans` set, one item per
  hidden song and playlist, as Spotify's apps keep it. It is read once per
  playback session and written only when you hide or show a song. Hidden
  songs are greyed out and left out of what plays from that playlist.
- **Radio and autoplay** through Spotify's context resolver: stations seeded
  by a song, playlist, album, or artist. Each resolution is a fresh mix of 50
  songs, so a radio page plays the songs it shows rather than asking again.
- **Audiobook detection** for saved shows, which the Web API lists as podcasts.
- **Play counts** on album pages and an artist's popular songs, from the
  Pathfinder queries Spotify's own pages make. The Web API has none. When
  the read fails, the lists leave the counts out.
- **Listening activity**: what the people the account follows are playing,
  from the private feed behind Spotify's own panel. The Web API has no such
  request. Spotify may change this response without notice; when the feed
  does not answer, the older buddy list stands in, which reports a song only
  after it ends.
- **Google Cast speakers and groups.** librespot has no Cast sender, so
  Spotifast starts Spotify's receiver on the device itself. The session then
  obtains a token for that receiver, as Spotify's own apps do. See
  [how it connects](/how-it-connects/#google-cast-devices).

### Private requests

Where no public API covers a feature, Spotifast may read the private
endpoints Spotify's own apps use, through the librespot session. These
requests only read; the one exception is pre-save on upcoming album pages,
which writes only when the user presses the button. Requests to Spotify's
client API go into librespot first: a pull request upstream, carried in
Spotifast's librespot fork until a librespot release includes it. Pathfinder
queries, which librespot does not make, stay in Spotifast. Spotify can change
any of these without notice, so a feature that uses one falls back to the Web
API where it has an answer, and otherwise leaves out what it could not read.

## librespot playback

librespot provides:

- Spotify catalogue playback at up to 320 kbps.
- Gapless playback, normalisation, and a local audio cache. Spotifast's fork
  also prepares the start of the next two songs, so a skip plays at once.
- Spotify Connect, so another Spotify client can transfer playback to this
  computer.
- Shuffle, repeat, seek, and volume.
- Control of restricted Connect devices, including Sonos, through the signed-in
  playback session. Sonos receivers are discovered on the local network;
  the Web API alone cannot start or control them.
- Songs and podcast episodes.
- Local files that a playlist lists, played from the local files folder chosen
  in Settings and its subfolders. Spotify keeps only a local file's artist,
  album, title and length, so the player matches each `spotify:local:` entry
  to the MP3, MP4, M4P or FLAC file whose tags and length give the same.
  Files at any sample rate from 8 to 384 kHz play, mono and surround
  included: librespot resamples them to 44.1 kHz stereo
  ([librespot#1840](https://github.com/librespot-org/librespot/pull/1840)).
  The player reads the folder when local playback starts, so songs added
  later play after the next restart. See
  [issue #3](https://github.com/crmne/spotifast/issues/3). Spotify's queue
  refuses a `spotify:local:` entry, so such a song is queued on this
  computer's own player, and only while it plays
  ([issue #1002](https://github.com/crmne/spotifast/issues/1002)). Spotify
  has no cover or lyrics for a local file either, so Spotifast reads them
  from the file: the picture in its tags (an MP3's ID3 `APIC` frame or a
  FLAC `PICTURE` block), and the words in a `.lrc` file of the same name
  beside it or in its tags (ID3 `USLT` or a `LYRICS` comment), before
  asking LRCLIB
  ([issue #1001](https://github.com/crmne/spotifast/issues/1001)).

Spotify Premium is required. librespot cannot play audio with a free account.

Spotifast uses a small librespot fork: librespot's development branch with a
few patches that are also proposed upstream, such as normalisation data for the
visualisers and an event for rejected audio keys. They are listed in
`Cargo.toml` and dropped as librespot merges them.

## Not available

The Web API and librespot do not provide these features:

- **Pins shared with the Spotify app.** Spotify stores pins in its private
  `your-library` service. There is no Web API for it, and librespot does not
  support its protocol. Spotifast's pins are local and are stored in
  `settings.json`. See [issue #31](https://github.com/crmne/spotifast/issues/31).
- **Editing playlist folders.** librespot can only read them.
- **Smart Shuffle, Jam, Blend, and similar Spotify features.** Spotify
  generates these for its own clients. Spotifast only has plain shuffle.
  A Blend that a Spotify app already made opens like any playlist, with
  who added each song, but Spotifast cannot start or manage one.
- **Spotify DJ.** Its special playlist context does not expose playable tracks
  to librespot, so Spotifast leaves its card out of Made for you. See
  [librespot issue #1604](https://github.com/librespot-org/librespot/issues/1604).
- **Lossless audio.** librespot does not receive lossless streams. Spotifast
  will reconsider this if librespot gains lawful support, but it will not
  bypass Spotify's DRM.
- **Soloist as the player.** Spotify's Soloist plays lossless and works with
  Spotify Connect, but Spotify ships it only for Linux and does not allow apps
  to bundle it. Each listener would need their own developer key and pairing,
  builds stop working after 90 days, and it has no API for the library,
  playlists or search, and no way to feed the equalizer and visualisers.
  Spotifast will reconsider it if Spotify offers a supported desktop path.
  See [issue #965](https://github.com/crmne/spotifast/issues/965).
- **Local files from Spotify's Local Files playlist.** librespot cannot play
  that playlist; add the songs to another playlist first. Local files on other
  computers or phones are not shared either: Spotify keeps only their names.
- **Audiobooks.** librespot does not play them. Spotify lists some
  audiobooks among saved shows; since 0.10.0, Spotifast asks the
  librespot session which ones and leaves them out of the Podcasts shelf.
- **Offline listening and downloads.** Spotify's DRM and the project's scope
  rule these out.
- **Playback speed and crossfade.** librespot supports neither. Spotifast
  would have to add them to its own audio path.
- **Free-account playback.** Replacing Spotify audio with another source is
  also out of scope. See the
  [contribution guide](https://github.com/crmne/spotifast/blob/main/CONTRIBUTING.md).
- **Private-session status and similar social features.** Spotify has no
  public API for them.
- **Canvas videos and video podcasts.** librespot does not provide them.
