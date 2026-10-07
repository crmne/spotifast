---
title: Privacy
description: What Spotifast stores on your computer, what it sends and to whom, and what it never collects.
nav_order: 4
---

Spotifast is a desktop app that runs entirely on your computer. It has no
account of its own, no server, no telemetry, no analytics, and no advertising.
Its author receives nothing about you or how you use it.

This page covers the Spotifast app, version 0.8.0 and later. Earlier versions
kept sign-ins in files instead of the system credential store; update to a
current release.

## What stays on your computer

- **Spotify sign-ins.** The grants Spotify issues when you sign in, and the
  reusable playback credential, are kept in the system credential store:
  Credential Manager on Windows, Keychain on macOS, and Secret Service on
  Linux. Your Spotify password never passes through Spotifast; you sign in
  on Spotify's own pages. A proxy password, if you set one, uses the same
  store.
- **Settings and history.** Settings, window positions, recent plays, the
  last session, skins, themes and MilkDrop presets live in the config
  directory.
- **Caches.** Downloaded audio, artwork, lyrics and library metadata live in
  the cache directory and can be deleted at any time.
- **Log.** `spotifast.log` records errors and diagnostics. It stays on your
  computer and never contains credentials; share it only if you choose to
  attach it to a bug report.

[Settings & Files](/settings-and-files/) lists every location and what is safe
to delete. **Sign out** in Settings removes the stored credentials.

## What is sent, and to whom

Spotifast connects only to the services below.

- **Spotify.** Sign-in, your library, search, playlists, playback and Spotify
  Connect all go to Spotify, under your account. Spotify's own
  [privacy policy](https://www.spotify.com/legal/privacy-policy/) applies to
  that data.
- **LRCLIB.** When the lyrics panel is open and Spotify has no lyrics for the
  song, Spotifast sends its artist, title, album and length to
  [lrclib.net](https://lrclib.net). Nothing identifying you is included.
- **GitHub.** Once a day, Spotifast asks GitHub for the latest release. You
  can turn automatic checks off in Settings. Downloading an update, and the
  first opening of MilkDrop, also fetch files from GitHub. No Spotify data is
  sent.
- **Your local network.** Spotifast looks for Spotify Connect speakers over
  mDNS and talks to the ones you choose.
- **The people in your jam.** Only while you host or join a
  [jam](/jam/), Spotifast connects directly to the other participants'
  computers. It sends your Spotify display name, the songs you add, and
  playback commands and positions. The connection is not encrypted; no
  credential or other account detail is sent.

Links you open from the app, such as the Winamp Skin Museum or this website,
open in your browser.

## This website

spotifast.rocks counts page visits with [Plausible](https://plausible.io/data-policy),
which uses no cookies and collects no personal data. The app itself contains
no analytics.

## Questions

Ask on [GitHub](https://github.com/crmne/spotifast/issues).
