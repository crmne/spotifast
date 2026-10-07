---
title: Jams
description: Listen together through your own jam server, each person on their own Spotify account.
nav_order: 7
---

A jam is one shared queue that everyone listens to at the same time. It runs
on a small jam server that you keep on a VPS, so it is always there: people
join and leave whenever they like, and the music carries on for the others.
Everyone in the jam has the same rights.

This is Spotifast's own feature, not Spotify's Jam: it works between people
who use Spotifast, and people in the Spotify app cannot join it.

## What you need

- **A jam server.** One person runs it on a VPS; see
  [Run the jam server](#run-the-jam-server) below.
- **The server's address and its code.** The address is the VPS's host name
  or IP address, such as `jam.example.org`; add `:port` only if the server
  does not listen on 4070. The code, which the server prints for you, looks
  like `…#…` and holds the server's password: share it privately, with the
  people you want in the jam.
- **Spotify Premium for everyone.** No sound travels through the server.
  Each person plays the songs on their own account, the way they play
  anything else in Spotifast.
- **Playback on this computer.** A jam plays through Spotifast's own player,
  not through a speaker or phone picked in the device list.

## Join the jam

Open the queue panel and pick the **Jam** tab. Enter the server's address
and paste its code, then choose **Join**. Spotifast keeps both, so next time
**Join** is all it takes. The tab then shows who is listening, the song
playing and the shared queue, with who added each song.

**Leave the jam** takes you out; the jam plays on for the others. When the
last person leaves, the jam pauses where it was, and picks up from there.

## During a jam

- **Add songs** by right-clicking a song, or a selection, and choosing
  **Add to jam**. The song appears at once and the server confirms it a
  moment later. Local files cannot be shared.
- **Anyone can do anything.** Play, pause, next, previous (back to the start
  of the song) and the progress bar act on the jam for everyone, and anyone
  can remove any song with the cross beside it.
- **Starting a playlist or album is held back**, since the jam decides what
  plays. Add its songs to the jam instead.
- **Volume stays your own.**
- If the connection drops, Spotifast reconnects on its own and sends again
  the songs you added meanwhile.

Every computer stays within about a second of the server. A correction is a
short jump in the song, so small differences are left alone. A jam suits
listening together from different places; in one room, the speakers will
not be exactly in step.

When you leave, your repeat setting comes back as it was.

## Run the jam server

The server is a small program in this repository, `jam-server`, which needs
neither Spotify nor an account. Any Linux VPS with Docker will do.

```sh
git clone <your repository> spotifast
cd spotifast/jam-server
docker compose up -d
docker compose run --rm jam code
```

The last command prints the server code. Give it, with the VPS's public
address or host name, to the people you want in the jam. Open TCP port 4070
in the VPS's firewall.

The first start creates the server's password and its certificate in the
`jam-data` volume, and the jam is saved there too, so restarts and updates
keep the queue. Back up that volume to keep the same code; deleting it makes
a new password and certificate, and everyone needs the new code.

To change the password, stop the server, delete `secret` from the volume,
start it again, and hand out the new code. See `jam-server/README.md` for
running without Docker.

## Security

The connection to the server is encrypted with TLS. Spotifast accepts only
the certificate that the server code names, so nobody can stand in for your
server, even without a domain name. Listeners prove they know the password
without sending it, and the proof only works for that one connection.

Anyone with the server code can join, add and remove songs, and control
playback: share it only with the people you want in the jam. The server
sees your Spotify display name and the songs played, never any Spotify
credential or other account detail.
