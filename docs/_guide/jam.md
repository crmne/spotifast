---
title: Jams
description: Listen together with friends who also use Spotifast, each on their own Spotify account.
nav_order: 7
---

A jam lets a few people listen to the same songs at the same time. One person
hosts it; the others join with an invitation code. Everyone adds songs to one
shared queue, and every computer plays the same song at the same point.

This is Spotifast's own feature, not Spotify's Jam: it works between people
who use Spotifast, and people in the Spotify app cannot join it.

## What you need

- **Spotify Premium for everyone.** No sound travels between computers. Each
  person plays the songs on their own account, the way they play anything
  else in Spotifast.
- **The same network.** The host's computer must be reachable from the
  guests': the same home or office network, or a private network such as
  [Tailscale](https://tailscale.com) for friends elsewhere. Do not open a
  port on your router to the Internet for a jam: see
  [Security](#security) below.
- **Playback on this computer.** A jam plays through Spotifast's own player,
  not through a speaker or phone picked in the device list.

## Host a jam

Open the queue panel and pick the **Jam** tab, then **Host a jam**. The tab
shows an invitation code such as `192.168.1.20:54321#…`. Copy it with the
button beside it and send it to your friends privately: anyone with the code
can join while the jam lasts.

**Guests control playback** lets guests skip, pause, seek and remove any
song. Without it, only you do, and guests can add songs and remove their own.

**End the jam** closes it for everyone. Each jam has a new code.

## Join a jam

Paste the code in the **Jam** tab and choose **Join**. The tab then shows who
is listening, the song playing and the shared queue, with who added each
song.

## During a jam

- **Add songs** by right-clicking a song, or a selection, and choosing
  **Add to jam**. The song appears at once and the host confirms it a moment
  later. Local files cannot be shared.
- **The player controls act on the jam.** Play, pause, next, previous (back
  to the start of the song) and the progress bar go to the host, who applies
  them for everyone, or refuses them when guests do not control playback.
- **Starting a playlist or album is held back**, since the jam decides what
  plays. Add its songs to the jam instead.
- **Volume stays your own.**
- If the connection drops, Spotifast reconnects on its own and sends again
  the songs you added meanwhile.

Every computer stays within about a second of the host. A correction is a
short jump in the song, so small differences are left alone. A jam suits
listening together from different places; in one room, the speakers will
not be exactly in step.

When a jam ends, your repeat setting comes back as it was.

## Security

The connection between computers is not encrypted. The invitation code holds
a random secret that is never sent: guests prove they know it. But someone
who can watch your network can see which songs are played and who listens,
and could interfere with the jam once it is running.

So host jams on a network you trust, or through a private network such as
Tailscale, which encrypts everything. Spotifast listens only on your
computer's address on that network, and only while you host. No Spotify
credential or account detail other than your display name ever goes to the
other participants.
