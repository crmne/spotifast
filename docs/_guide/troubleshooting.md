---
title: When Something Goes Wrong
description: "What \"Spotify refused the audio key\" and \"Spotify is being slow\" mean, and what you can do about them."
nav_order: 7
---

Each section starts with the words you see in Spotifast or in its log, so you
can search this page for them.

## Spotify refused the audio key {#audio-key}

You may see this banner when you press play:

> **Spotify won't let this computer play this song right now.**

Or you may find lines like these in the log:

```
error audio key 0 1
Unable to load key, continuing without decryption
Spotify refused the audio key. Try again later
```

### What it means

Spotify sends music to your computer locked. To play a song, the player has
to ask Spotify for that song's key and use it to unlock the audio. Spotifast
plays music through [librespot](https://github.com/librespot-org/librespot),
and librespot asks for a key every time a song starts. Here, Spotify said no.
Your sign-in works and your library looks normal, but without the key there
is nothing to turn into sound.

This comes from Spotify, not from your computer. Other apps built on
librespot run into it too.

### Two different cases

It seems to happen in two ways
([librespot issue #1649](https://github.com/librespot-org/librespot/issues/1649),
summed up in
[this comment](https://github.com/librespot-org/librespot/issues/1649#issuecomment-6038311448)):

- **Newer accounts, refused every time.** If your Spotify username is a long
  string of random letters and numbers that Spotify made up for you, Spotify
  may refuse every key. Nothing in Spotifast or librespot can change that
  today.
- **Older accounts, refused in bursts.** Some older accounts play fine for a
  long time, then get a run of refusals. It looks like Spotify limits how fast
  keys can be asked for. It clears up after a while, so trying again later
  usually works.

### What won't help

Changing audio settings, clearing the cache, signing out and in, or
reinstalling won't fix it, because nothing on your computer is wrong. A
[personal Spotify app](/make-it-even-faster/#make-a-spotify-app) doesn't
change it either: that only speeds up library and search requests, not audio
keys.

### What works

Play the music on another Spotify device and keep using Spotifast as the
remote. **Play on another device** in the banner opens the device picker, the
same one as the devices button in the player bar. Pick your phone, a speaker,
or a computer with the official Spotify app open. The music plays there, and
you still choose songs, skip and change the volume from Spotifast. Those
devices get their own keys from Spotify, so they keep playing.

Close the banner with **×** and it stays away until a song plays on this
computer again. If it happens again before that, you just get a short
message.

Spotifast follows this in
[issue #283](https://github.com/crmne/spotifast/issues/283).

## Signing in fails {#sign-in}

If Spotify's page says **redirect_uri: Not matching configuration**, you are
on a version before 0.14.2, which signed in through a shared app that no
longer accepts new sign-ins
([#1057](https://github.com/crmne/spotifast/issues/1057)). Update Spotifast,
then follow the steps on the sign-in screen to make your own free Spotify app.

If Spotify's page says **Invalid redirect URI**, your own app is missing the
address `http://127.0.0.1:8989/login`, or has it with a difference, such as
`localhost` or a slash at the end. Add it exactly, save, and sign in again.
[Getting Started](/getting-started/#sign-in-with-it) covers the other
messages the sign-in screen can show.

## Spotify is being slow (rate limits) {#rate-limits}

You may see this banner at the top of the page:

> **Spotify is being slow on the shared app.**

Or you may find lines like these in the log:

```
Spotify rate limit source=personal wait=8s
Spotify cooldown source=personal duration_ms=8000
```

### What it means

Spotify limits how many requests an app can make, and tells an app that
asks too often to wait. Spotifast waits as told, then tries again. In the
log, `source=personal` means your own Spotify app is waiting;
`source=shared` means the shared app that versions before 0.14.2 signed
everyone in with is.

While it waits, searches and your library take longer to load, playback
controls can feel slow, and signing in can take a while. The top bar shows a
spinner; point at it to see what Spotifast is waiting for. While you sign
in, the sign-in screen says Spotify is being slow: sign-in finishes on its
own once Spotify allows it, and **Cancel** stops waiting. Your playlists and
Home keep what they showed last time until the update arrives.

### On the shared app

The banner appears only if you are still signed in through the shared app,
which everyone signed in that way shares, so it is often busy. The fix is a
free Spotify app of your own, which you will need anyway once Spotify stops
accepting the shared one. It takes a few minutes, needs no code, and Spotify
requires Premium for it. **Set up personal app** in the banner opens the
setup in Settings, and [Getting Started](/getting-started/#make-a-spotify-app)
has the steps. Your searches, library and playback controls then use your
own allowance.

### On your own app

Your own app's allowance is yours alone, so it is rarely the cause. Setting
up playback on this computer helps when it is: Spotifast then reads your
playlists, popular songs and related artists through its music connection,
which uses none of it. [How it connects](/how-it-connects/) has the details.

### "Development Mode quota is exhausted"

A personal app has limits of its own. If you see

```
Spotify's Development Mode quota is exhausted; try again after the quota resets
```

your personal app has used up the allowance Spotify gives it. Requests on it
fail until Spotify resets it, so try again later. If you made several personal
apps, they share one allowance for your account, so another app doesn't
help.
