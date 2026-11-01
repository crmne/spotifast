---
title: Make It Even Faster
description: "Scrolling performance and reducing loading delays with your own Spotify connection."
nav_order: 6
---

## Scrolling and large libraries

The sidebar is drawn alongside every page, so its sorting work can affect
scrolling elsewhere in the window. Library ordering now looks up each row's
position once per sort instead of repeatedly searching the full saved order.
This reduces the work each frame needs for large libraries while preserving
your chosen sort, pins, and folders.

The main window draws as needed and uses the platform's frame pacing. The
player bar and mini player visualizers draw at the **Frame rate** chosen in
Settings > Appearance, 60 by default; a lower rate uses less power. The
MilkDrop frame-rate setting applies to its separate visualizer window.

## API rate limits

If you got here from "Spotify is being slow on the shared sign-in", the short
version is in [Spotify is being slow](/troubleshooting/#rate-limits).

Spotify limits how often each Client ID can ask for information. The one you
[registered](/getting-started/#make-a-spotify-app) has an allowance that is
yours alone. If you register several, they share your account's allowance, following Spotify's
[July 2026 quota update](https://developer.spotify.com/blog/2026-07-23-web-api-quota-updates).
When it runs out for a while, Spotify asks Spotifast to wait. The top bar
shows a spinner while you wait; hover over it to see whether Spotifast is
updating your library, checking for updates, or waiting for Spotify. Your
playlists and Home show what they showed last time until the update arrives.

Setting up playback on this computer spends less of that allowance. Spotifast
then reads your playlist list, other people's playlists, popular songs and
related artists through its music connection, which uses none of it, and
which can read the playlists Spotify keeps from registrations like yours, its
own among them. [How it connects](/how-it-connects/) explains which connection
each feature uses.

Spotify allows a registration like yours ten search results at a time for each type.

## If you signed in before 0.14.2 {#shared-app}

Earlier versions signed everyone in through one shared sign-in, which the
ncspot project registered. It stopped taking new sign-ins in October 2026
([#1057](https://github.com/crmne/spotifast/issues/1057)), and Spotify no
longer gives a project like this one a registration everyone can share. Spotify's
own client, which playback uses, can't stand in either: Spotify turns its
requests for your library and the catalogue away as too many.

If you were signed in that way, you stay signed in until Spotify stops
accepting it. Then the sign-in screen tells you so and shows how to register
Spotifast. You can switch sooner in **Settings**, under **Spotify
registration**: paste your Client ID and choose **Authorize**. With Premium,
Spotifast suggests this once, and again with a banner when Spotify slows the
shared sign-in down, at most once a day and not again after you close it three
times.

While both are signed in, your Client ID answers what it can, and the shared
sign-in still lists your playlists and finds playlists in a search, which
Spotify keeps from registrations like yours.

## Register Spotifast with Spotify {#make-a-spotify-app}

The steps are in [Getting Started](/getting-started/#make-a-spotify-app).

![Settings, with Spotifast registered](/assets/images/make-it-even-faster.png)
