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
Songs, or a radio named after the song, playlist, album, or artist it is
based on, which opens the radio's page. The line shows only while a song is
playing from somewhere Spotify reports.

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

An artist's discography page, opened with **Show all**, plays the page
itself. A double-clicked song, or a release's play button, plays the releases
from that one to the end of the page in the order shown, newest or oldest
first, with each release's songs in album order and unavailable songs and
local files left out. **Next up** is that list. While one of a release's songs
is the current song, its play button pauses and resumes that song where it
is, as the player bar does, instead of starting the release over. The page's
list is not kept when Spotifast closes, so after a restart that song resumes
without the rest of the page. A song on
two releases starts from the copy that was chosen. The list begins with the chosen release's
first song, so **Previous** steps back within it, and Shuffle and Repeat keep
to the releases from the chosen one on. When the songs after the chosen one
have not all loaded, the play waits for them and shows the chosen song as
playing; Pause, Next, Previous, a queue row, another device or another play
cancels the wait, and a seek meanwhile moves where the chosen song will
start. A play chosen while a sleeping speaker wakes is meant for that
speaker: it keeps waiting when the speaker appears and starts there. Until
the chosen release's own songs arrive there is no song to show yet, so the
player bar keeps the song from before, and Play or Pause there cancels the
wait and acts on that song. What was playing goes on during the wait. If it
comes within two seconds of its end, if the songs stop arriving for ten
seconds once the chosen release is in, or after thirty seconds in all, or if
the song lists stop (a rate limit, an exhausted quota, an expired sign-in, a
network failure or a Spotify server error), the play starts with the songs
that came, up to the first release that did not, and says the queue ends
early. Starting before the old song ends keeps it from moving on to its next
song, perhaps one of yours under **Playing next**, which the play would then
cut off, so the play starts then even if the chosen release's own songs have
not come. A song on Repeat one starts over instead, and the wait goes on.
Offline the page's songs and release
buttons cannot be used, as in other lists of songs. If the chosen release's own songs
never arrive, that release plays on its own, as Spotify knows it, and the
play says the queue ends early when that leaves out later releases with
songs. Once Spotify cannot list the chosen release's songs, this happens at
once rather than after the wait. A play holds
at most 500 songs, counted from the chosen release's first song; a song chosen
further down than that starts the 500 from itself. Like other lists of songs,
it shows no **Playing from** line. The tests in `src/app/discography.rs`, and
the discography tests in `src/demo.rs` for the page's buttons, check these
rules.

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
   at the front of **Playing next**, before the songs already queued. It
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
   below it stay, and the playlist keeps going afterwards.

6. **Starting a playlist or album keeps your songs.** The rows underneath
   change to the new collection; your songs stay under **Playing next**
   and play before the collection continues. If you queue an album and then
   start it from the Library, its first song plays now and the separately
   queued album still follows. **Clear queue** removes those manual copies
   while keeping the playing album's remaining songs.

7. **Clear only removes your songs.** The trash button sits beside the
   *Playing next* heading and empties that section; the playlist's rows
   below stay. It only shows while this computer is the player, because
   that is the only queue the app can actually clear.

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

11. **Dragging within *Playing next* reorders it, only on this computer.**
    Neither the Web API nor librespot can reorder or insert into a live
    queue; the only way to change one is to clear it and re-add its songs
    in the new order, which reaches nothing but the engine actually playing
    them. So dropping a song, dragged from elsewhere, at a position in
    *Playing next* inserts it there, and dragging a row already in
    *Playing next* elsewhere in the same section moves it, only while this
    computer is the active player. Otherwise every drop still just adds to
    the end, exactly like **Add to queue**. *Next up* is never a drop
    target: it plays from the current context, not from a list Spotifast
    can rewrite. While *Playing next* is empty, drop the song on the player
    bar's Queue button instead.

12. **A song you queued can be removed on its own, only on this
    computer.** Right-click a row under *Playing next*, or open its **…**
    menu, and choose **Remove from queue**. Only that row leaves: other
    copies of the song stay, and so does *Next up*. Like a move, this
    rewrites the queue by clearing it and re-adding the rest, so it is
    offered only while this computer is the active player. *Next up* rows
    have no such entry; they play from the current context.
