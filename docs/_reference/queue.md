---
title: The Queue's Rules
description: What the queue shows, what Add to queue does, and what the app promises about both.
nav_order: 2
---

The queue is the list of what plays next. It has two parts. On top,
under **Playing next**, are the songs you queued yourself. Below them,
under **Next up**, are the songs that come next in whatever playlist or
album is playing. Your songs always play first.

Above both, **Playing from** names where the playing song came from: the
playlist, album, artist, or podcast, which opens when clicked, Liked
Songs, or a song radio named after its song. The queue’s radio name stays plain text; use a song’s **Go to song radio**
action to browse its station. The line shows only while a song is
playing from somewhere Spotify reports.

These are the rules the app follows. The queue tests in `src/app.rs`
check every one of them.

Starting and resuming are separate actions. With Shuffle off, a playlist's
**Play** button starts at its first available song in the selected order.
Double-clicking a row starts there, including with Shuffle on. **Play** in
the player bar resumes the current song at its paused position.

Since 0.8.0, starting a playlist in its original order explicitly
names its first available song from the loaded prefix. If that prefix is not
loaded, it requests playlist position zero. A page loaded from the middle
never becomes the beginning. This keeps the full Spotify playlist context;
the app does not replace it with a shortened list of loaded songs. A request
waiting for local playback to reconnect keeps the song chosen at the click.

Sorted and filtered views omit unavailable songs and local files from their
playback requests. The displayed rows keep their positions, and selecting a
repeated song starts that occurrence. A filtered playlist or Liked Songs view
plays its matching songs in displayed order, including duplicates. An empty or entirely
unplayable view disables Play instead of starting the unfiltered context.

1. **The list shows the play order.** The top row plays next, followed by the
   rows below it.

2. **Add to queue adds a song to your part of the queue.** It goes after
   the songs you queued earlier and before the playlist's songs. Queue
   the same song twice and it plays twice. A double-click only counts
   once.

3. **When a song starts, its row leaves the queue.** It doesn't matter
   how it started: the song before it ended, you pressed Next, you
   clicked it, or another device skipped to it. A song is never shown
   as playing and as next at the same time.

4. **Next removes the top row right away.** The app doesn't wait for
   Spotify to confirm it.

5. **Playing a row from the queue skips to it.** The rows above it are
   skipped and removed, as if you had pressed Next down to it. The rows
   below it stay, and the playlist keeps going afterwards.

6. **Starting a new playlist keeps your songs.** The rows underneath
   change to the new playlist; your songs stay on top and still play
   first.

7. **Clear only removes your songs.** The trash button sits beside the
   *Playing next* heading and empties that section; the playlist's rows
   below stay. It only shows while this computer is the player, because
   that is the only queue the app can actually clear.

8. **Changes appear immediately.** Spotifast updates the queue before Spotify
   confirms the change. For local playback, it updates its own player directly.
   Toggling shuffle rechecks the queue so the new playback order appears
   promptly without waiting for the song to finish.

9. **Closing the app keeps the queue.** Spotifast saves it locally. When you
   resume the last song, it restores your queued songs and playlist position.

10. **Old answers from Spotify are ignored.** Queue responses can be a few
    seconds late. Spotifast ignores stale responses and asks again. Your
    changes stay visible while it waits for confirmation.

Since 0.8.0, selecting several playlist rows and choosing
**Add to queue** preserves repeated occurrences in their selected order.
For example, selecting B, C, B adds all three rows. A repeated click still
counts once, and the notification reports only the rows actually added.
