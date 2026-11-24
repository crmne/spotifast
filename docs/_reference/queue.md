---
title: The Queue's Rules
description: What the queue shows, what Add to queue does, and what the app promises about both.
nav_order: 2
---

The queue is the list of what plays next. Under **Now playing** is the
song playing. Below it, under **Next in queue**, are the songs queued by
hand, here or from another app; the heading shows only while there are
some. Last come the songs that follow in whatever playlist or album is
playing. Queued songs always play first.

That last section is headed **Next from:** and the name of where the
playing song came from: the playlist, album, artist, or podcast, which
opens when clicked, Liked Songs, or a radio named after the song, playlist,
album, or artist it is based on, which opens the radio's page. When Spotify
reports no such place, the heading is **Next up**.

On another device, Spotify's Connect state says which of its upcoming songs
were queued by hand, and **Next in queue** follows it, so every queued song
stays there however Spotify's queue list orders them.

These are the rules the app follows. The queue tests in `src/app.rs`
check every one of them.

Starting and resuming are separate actions. With Shuffle off, a playlist's
**Play** button starts at its first available song in the selected order.
Double-clicking a row starts there, including with Shuffle on. **Play** in
the player bar resumes the current song at its paused position.

Since 0.10.0, the Shuffle button on a collection page changes the
global playback mode without starting that collection. It can be selected
before a playback device is active; the next **Play** uses the selection.
While another collection plays, toggling Shuffle changes that playback's mode
without switching to the page's collection. The queue then reflects the new
play order.

Since 0.8.0, starting a playlist in its original order explicitly
names its first available song from the loaded prefix. If that prefix is not
loaded, it requests playlist position zero. A page loaded from the middle
never becomes the beginning. This keeps the full Spotify playlist context;
the app does not replace it with a shortened list of loaded songs. A request
waiting for local playback to reconnect keeps the song chosen at the click.

Sorted and filtered views omit unavailable songs, and local files the local
files folder does not hold, from their playback requests. The displayed rows keep their positions, and selecting a
repeated song starts that occurrence. In a playlist's original order, a song
that appears more than once among the loaded rows starts at the chosen copy's
playlist position, provided the rows load from the beginning and no missing
or local entry comes before it; otherwise it starts by name, which plays the
first copy. A song started from a row marks only that row as playing and
loading; other copies in the same list look like ordinary rows until another
song plays. Another playlist that also holds the playing song shows it as an
ordinary row, since the song is not playing from there. A filtered playlist or
Liked Songs view
plays its matching songs in displayed order, including duplicates. An empty or entirely
unplayable view disables Play instead of starting the unfiltered context.

Since 0.9.0, starting another song on this computer keeps the
selected Repeat mode, with Shuffle on or off. Loading a playlist no longer
silently disables repeat in the playback engine.

Since 0.9.0, silencing the old song during a local track change
keeps the decoder paced while the replacement loads. Cached audio no longer
races to the end and causes an unwanted extra skip during that handoff.

Since 0.9.0, an unexpected local playback disconnect retains the
engine's in-memory playback state before closing the session. Reconnection
restores the same song and position, paused or playing, with its playlist
context, exact shuffle order, manually queued songs (including duplicates),
repeat settings, and pending context pages. This also works when the current
song came from the manual queue rather than the playlist. Playback started
on the replacement engine takes precedence over recovery.

Since 0.9.0, switching from another Connect device back to this
computer transfers that device's current song, position, and queue, including
manually queued copies. A paused session stays paused. The handoff itself does
not consume a queue row or restore an older queue saved on this computer.

1. **The list shows the play order.** The top row plays next, followed by the
   rows below it.

   A long queue can show more songs than Spotify includes in its first
   response. Their titles, covers and durations fill in as the details arrive.

   The built-in and Connect player lists are used as reported, including
   repeated songs. Only the Web API fallback needs padding removed.

   Spotify pads a short queue by going round it again, back to the playing
   song, even with Repeat off. With Repeat off, Spotifast leaves out the
   rows from where the list comes back round, so a song started on its own,
   as a Recent row starts it, is not listed again under itself. Near the end
   of an album, Spotify may go round to the album's first song without
   showing where the album ends, so a few rows that will not play can stay.

2. **Add to queue adds a song to your part of the queue.** It goes after
   the songs you queued earlier and before the playlist's songs. Queue
   the same song twice and it plays twice. A double-click only counts
   once.

   **Play next** puts a song, or a selection of songs in displayed order,
   at the front of **Next in queue**, before the songs already queued. It
   leaves the current song playing and keeps repeated songs as separate
   entries. Right-click a song or selection to find it beside **Add to queue**.
   It is enabled only while this computer is the active player, including
   when paused. Spotify does not let Spotifast insert at the front of another
   device's queue.

   A local file from a playlist queues like any other song, but only while
   this computer is the active player and the local files folder holds it:
   Spotify's queue refuses local files, so this computer's player queues
   them itself. Otherwise **Add to queue** and **Play next** are disabled
   for it, and a selection queues its other songs.

   Since 0.9.0, an album's **Add to queue** adds its playable songs
   in album order, including repeated songs. This also works for singles and
   EPs. A fully loaded album appears immediately. Otherwise, a loading notice
   appears while all its track pages are fetched, then its songs are appended
   together after the songs already queued. A failed fetch adds no partial
   album. Clear queue and sign-out cancel an album still being fetched;
   changing playback devices asks you to add it again on the new device.

   On another device, a rate limit delays additions instead of dropping them.
   Spotifast retries automatically after Spotify's requested wait and keeps
   later songs behind the album. Pending rows retain their known titles and
   durations. If Spotify permanently rejects an addition, only the rejected
   song and any unsent remainder of its album disappear; accepted songs stay.

3. **When a song starts, its row leaves the queue.** It doesn't matter
   how it started: the song before it ended, you pressed Next, you
   clicked its queue row, or another device skipped to it. Only that
   occurrence leaves: another copy you queued remains until its own turn.

4. **Next removes the top row right away.** The app doesn't wait for
   Spotify to confirm it.

5. **Playing a row from the queue skips to it.** The rows above it are
   skipped and removed, as if you had pressed Next down to it. The rows
   below it stay, and the playlist keeps going afterwards. On this
   computer the rows above leave the queue without playing, so only the
   chosen song loads and no song in between is heard or named. Previous
   then returns to the song that was playing, not to a skipped row. Another
   device, Soloist included, cannot have a song taken out of its queue, so
   it skips down to the row: one skip naming the row where Spotify Connect
   takes one, as on a Sonos speaker, and otherwise one skip per row, each
   sent once the one before it is answered. Until that device reaches the
   song, the chosen row stays the playing one and the songs passed on the
   way show nowhere: not as the song playing, not in the player bar, and
   not as a queue in which the chosen song is still waiting. A second row
   clicked meanwhile is counted on the rows as shown. A skip that has not
   landed after twelve seconds gives way to what Spotify reports, and so
   does one whose device is no longer the one playing: another device
   chosen or reported playing, or this computer's player taking over.

6. **Starting a playlist or album keeps your songs.** The rows underneath
   change to the new collection; your songs stay under **Next in queue**
   and play before the collection continues. If you queue an album and then
   start it from the Library, its first song plays now and the separately
   queued album still follows. **Clear queue** removes those manual copies
   while keeping the playing album's remaining songs.

7. **Clear only removes your songs.** The **Clear queue** button sits
   beside the *Next in queue* heading and empties that section; the
   playlist's rows below stay. It shows while this computer is the player,
   and on another device whose Connect state lets the app rewrite its
   queued songs.

8. **Changes appear immediately.** Spotifast updates the queue before Spotify
   confirms the change. For local playback, it updates its own player directly.
   Toggling shuffle rechecks the queue so the new playback order appears
   promptly without waiting for the song to finish.

9. **Closing the app keeps the queue.** Spotifast saves it locally. When you
   resume the last song, it restores your queued songs and playlist position,
   and the playlist carries on after that song, even when it was paused or
   playing from a sorted view when you closed the app.

   A song the playlist no longer holds, such as a recommendation that followed
   its end, plays on its own at its saved position, followed by your queued
   songs and, with Autoplay on, Spotify's recommendations for that song.

10. **Old answers from Spotify are ignored.** Queue responses can be a few
    seconds late. Spotifast ignores stale responses and asks again. Your
    changes stay visible while it waits for confirmation.

Since 0.8.0, selecting several playlist rows and choosing
**Add to queue** preserves repeated occurrences in their selected order.
For example, selecting B, C, B adds all three rows. A repeated click still
counts once, and the notification reports only the rows actually added.

11. **Drag a song to change its place in the play order.** The green line
    shows where it will play. Drag between rows in *Next up* or *Next from*,
    or between that section and *Next in queue*. Moving a row moves that
    occurrence once, including when another occurrence has the same title.
    Dropping a Next up row on the Queue button moves it to the end of the
    manual queue. To start an empty manual queue, use the button or drop
    above the Next up heading. The order comes from the active player's
    queue and each drop appears immediately. You can keep dragging while a
    device confirms earlier drops. Spotifast sends them in order using the
    latest confirmed queue revision. Clear queue uses that same sequence.
    Confirmation requires a new revision with the edited occurrences in
    place; an addition appended meanwhile can stay. If playback advances or another app
    changes the affected rows, pick up the song again from the updated list.
    On the built-in player, the queue refreshes when a song starts, so you
    can reorder while it plays or while it is paused.

    Drag from a playlist into the queue to add songs to the listening
    session. Drop a song from either queue section onto an editable
    playlist to save it there. Changes to the play order affect the
    listening session only. Positional drops require queue editing support
    from the active player. On append-only devices, a playlist drag can
    still add songs to the end of Next in queue.

12. **Remove an upcoming song from the play order.** Right-click anywhere
    on a row under *Next in queue*, *Next from* or *Next up*, or open its
    **…** menu, and choose **Remove from queue**. Only the selected occurrence
    leaves the current play order. Saved playlists and albums stay intact.
    The option is available on this computer's built-in player and on
    compatible Connect devices that allow removal.
