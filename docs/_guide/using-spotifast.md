---
redirect_from: /using-fastpotify/
title: Everyday Use
description: Play music, arrange playlists, find lyrics, and make Spotifast your own.
nav_order: 3
---

## Middle-click autoscroll

On Windows, since 0.8.0, and on Linux once turned on, middle-click a scrolling
list or its empty background, then move the pointer away from the starting
point. That list follows the pointer, faster as the distance grows. Moving across another pane keeps the original list in control.
A small dead zone prevents an ordinary middle-click from moving the view.
Click again, press Esc, turn the wheel, or switch to another window to stop.
Buttons and text fields keep their normal middle-click behavior.

This works automatically on Windows, with no setting to enable. On Linux, turn
on **Middle-click autoscroll** under **Settings > Appearance**. It is off by
default, because Linux desktops usually paste the primary selection on middle
click. macOS retains its existing middle-click behavior.

## Scrolling shelves

Point at a horizontal shelf, such as Made for you or Recently played on
Home, and hold `Shift` while turning the mouse wheel. The shelf moves while
the surrounding page stays put. Release `Shift` to scroll the page normally.

## Podcasts on Home

Since 0.10.0, Home has a **Your podcasts** shelf below Recently
played. It lists episodes you have started and not finished, with the time
left, followed by each podcast's newest episode if it came out in the last 30
days and you have not started it, marked **New**. It covers the eight podcasts
you saved most recently and the five newest episodes of each. Audiobooks are
left out. The shelf is hidden when there is nothing to show. Click a card to
open the podcast, or use its play button to play the episode. An episode you
have started continues from where you left it. The same is true of Play on a
podcast page, a saved episode, or a search result.

## Dragging beyond the visible list

Since 0.8.0, hold a dragged song near the top or
bottom of an editable playlist's visible area to scroll. Scrolling gets faster
closer to the edge and stops when you move away or release the mouse. This
lets you move a song from the end to the beginning without dropping it along
the way. Clear filters and sorting before reordering playlist songs.

Since 0.8.0, drag a song from the player bar, the queue, or another
list into an open editable playlist. The line between rows marks its insertion
position. Dropping below the last row appends; the blank area of an empty
playlist accepts its first song. The source song stays in its list or queue,
and playback continues unchanged. Dragging a row within the same playlist
still moves that row.

Since 0.8.0, select several songs with `Ctrl`-click (`Cmd`-click
on macOS) or `Shift`-click, then drag any selected row. The whole selection
travels together in its displayed order, even if you selected the rows in
a different order. The preview names the first song and counts the rest.
Drop it on a sidebar playlist to append, between rows of another editable
playlist to insert, or on Liked Songs to save every selected song.
Selected rows have a translucent neutral highlight. Keyboard focus uses
the row highlight without an extra outline.

Dragging an unselected row copies just that song. Reordering within a
playlist still moves one song at a time.

## Copying and pasting songs

Since 0.10.0, press `Ctrl+A` (`Cmd+A` on macOS) in a playlist,
album or Liked Songs to select every song the list shows. A
filter narrows the selection to the matching songs. In a long playlist that
is still loading, it selects the songs loaded so far.

`Ctrl+C` (`Cmd+C`) copies the selected songs' `open.spotify.com` links, one
per line, ready to paste into another playlist, a message, or Spotify's own
apps. `Ctrl+V` (`Cmd+V`) in a playlist you can edit adds every song link on
the clipboard to its end. Links copied from Spotifast add their rows at
once; links from elsewhere appear as soon as Spotify names the songs. Songs
already in the playlist ask before being added twice, and links that are
not songs, such as albums, are skipped.

`Ctrl+X` (`Cmd+X`) in a playlist you can edit copies the selected songs'
links the same way and removes the songs from the playlist, so `Ctrl+V` in
another playlist moves them there.

These keys edit the text instead while a search, filter or other text field
has focus.

Clear any playlist filter or sort before placing songs between rows, so the
visible positions match Spotify's order. A duplicate confirmation keeps the
chosen position when you select **Add anyway**. Dragging near the top or bottom
of the playlist scrolls to positions beyond the visible rows.

The Library sidebar also scrolls near its edges when you drag a song toward a
playlist or reorder its entries. Only the list under the pointer scrolls.

## Starting a playlist and resuming

Since 0.8.0, with Shuffle off and the playlist in its original
order, its **Play** button explicitly starts at the first available song.
The selected song appears in the player immediately, including while local
playback reconnects. The full playlist remains the playback source, even
when only its first page is loaded.

Double-click a song to start at that row. Sorting the table plays its chosen
order when Shuffle is off. Shuffle chooses a random starting song unless you
choose a specific row. To resume the current song at its paused position,
use **Play** in the bottom player bar or press `Space`.

The **Shuffle** button beside **Play** selects the playback order without
starting the playlist. Select Shuffle first, then press **Play**.

When you start a song from a sorted playlist or Liked Songs, it appears in
the player bar straight away while playback connects. Sorting changes the
order you hear without rearranging the playlist saved on Spotify.

Sorted and filtered views omit unavailable songs and local files from playback.
The rows stay visible, and selecting a repeated song starts that occurrence. Filtering a playlist or Liked Songs plays only the matching songs,
including repeated entries. Play is disabled when the view has no playable
songs; it never falls back to the unfiltered playlist in that case. Clearing
the filter restores the original view. Existing Shuffle behavior is unchanged.

Right-clicking a song in an editable playlist offers **Remove from this
playlist** even when the list is sorted or filtered, since removal does not
depend on position. **Move up**, **Move down**, and drag-reorder stay on the
default order, where the rows on screen match the order saved on Spotify.

## Refreshing a playlist

Since 0.8.0, choose **Refresh** in a playlist's **…** menu to reload
its details and songs, including changes made in another Spotify client. The
menu item reads **Refreshing…** and is disabled while loading. Current songs,
filtering and sorting stay visible. Spotifast finishes saving your edits before
loading changes from Spotify. If loading fails, your songs stay visible and
you can choose **Retry**.

## Playing from the sidebar

Double-click a playlist, Liked Songs, album, artist, or podcast row in the
Library sidebar to start playing it. A single click still opens the row's page.
Pointing at a row's cover art also shows a play button, but only when the
sidebar is not in compact mode.

## Search from a launcher

**In development, not included in 0.8.0:** a Spotify search link opens Search
and fills the box without starting playback. To try it in a development build,
set a launcher or keyboard shortcut to run:

```sh
spotifast 'https://open.spotify.com/search/here%20comes%20the%20sun'
```

This starts the app if needed or opens its existing window. When signed out,
the latest search link waits for sign-in. Use `https://open.spotify.com/search`
without a query to open an empty, focused search box.

On Linux, `playerctl --player=spotifast open` accepts the same search link
when the app is running. Links to songs, albums, and playlists still start
playback as before.

## Finding a setting

The Settings page has its own search field under the title. Type to narrow
the page to matching rows; sections without matches disappear. Clear the
field to see everything again.

## Keyboard and screen readers

Right-click a search, filter, settings or playlist-editing text field for
**Cut**, **Copy**, **Paste** and **Select all**. Cut and Copy require a text
selection. The usual keyboard shortcuts, including Undo, still work.
Since 0.8.0, Ctrl, Cmd and Alt arrow keys move the caret while
a text field has focus. Playback and navigation shortcuts on those keys
remain available from song rows and other controls.

The main window provides screen-reader names for playback controls, library
and song rows, menus, sliders, and settings switches. `Tab` and `Shift+Tab`
move keyboard focus, shown by an outline. `Enter` activates the
focused control; on a song row, it plays that song. The row's **More** button
opens its menu from the keyboard too.

In a playlist, album or Liked Songs, focus a song row and use the up and down
arrows to move between whole rows in the displayed order. Rows scroll into
view as you move; Enter plays the focused song. Tab still reaches artist
links and each row's Like and More controls.

Left and right arrows adjust a focused volume slider by five percentage
points, or the seek slider by one percent of the song. Screen readers can
also read and set these sliders' values. `Ctrl+F` (`Cmd+F` on macOS) focuses
search. Since 0.9.0, `Space` plays or pauses the current song even
when a song row or control has focus. Text fields keep Space for typing.
Unmodified letter shortcuts still yield to the focused control.

This is the first part of screen-reader support. Windows testing with NVDA
remains tracked in [#262](https://github.com/crmne/spotifast/issues/262).
Winamp skins do not yet have equivalent accessibility coverage.

## Library order

Since 0.8.0, the menu below the Library filters selects an order
for each section. **Name** and **Recently played** are available throughout.
Albums and podcasts also offer **Recently added**, using their actual save
dates. Spotify does not supply equivalent dates for followed playlists or
artists, so those sections do not offer that choice. Entries with missing save
dates come last.

**Spotify custom order** follows your playlist order and folders from Spotify.
Set up playback on this computer to load that order. Your playlists stay
visible while it loads, and Spotifast remembers the last order for your account.
Items you pin in Spotifast remain at the top, including items from a closed
folder. Sorting or dragging Library items changes their order only in
Spotifast; it does not rearrange your Spotify library.

Drag playlists to choose **Local custom order**. New playlists appear below the
pinned group. Selecting **Name**, **Recently played** or **Spotify custom order**
keeps the saved arrangement, so selecting **Local custom order** restores it.
The playlist context menu's **Sort by recently played** also preserves it.

Upgrading keeps the previous default: a saved local playlist arrangement wins;
otherwise available Spotify folders keep their order, and a flat playlist list
uses recent plays. Other sections keep their supplied Library order until you
select a sort. Choosing a sort loads the rest of that Library section in the
background. If loading fails, choose the order again to retry.

Liked Songs starts pinned at the top. Drag it between pins to choose its
position, or below the pin block to unpin it and put it in **Local custom
order**. Other pins can sit above it. Its right-click menu also offers **Unpin**
and **Pin to top**; pinning adds it after your existing pins. The arrangement
survives restarting Spotifast and switching sort choices.

When unpinned, Liked Songs follows **Name** or **Recently played** like the other
rows. In **Spotify custom order**, it appears after the playlists because it
has no place in Spotify's playlist tree. Returning to **Local custom order**
restores its saved position. Dragging a song onto Liked Songs still saves that
song, wherever the row sits.

In **Settings > Appearance**, **Compact track list** puts each song on one
line. In narrow lists, the added date follows the artist credits with a spaced
bullet; each artist name remains a separate link.

## Windows taskbar controls

Since 0.8.0, hovering Spotifast's taskbar button offers **Previous**,
**Play/Pause**, and **Next** beneath its window preview. They control the same
playing device as the player bar, update immediately, and are disabled when
there is no song or the device refuses controls. The icons follow the system
appearance and display scaling.

Since 0.8.0, clicking or double-clicking the Windows tray icon shows
and raises Spotifast. Use **Show or hide Spotifast** in the tray menu to hide
it again.

Closing to the tray removes the window and its preview. Reopening the main
window or switching to the Winamp window creates its controls again. Media
keys and the system's now-playing controls continue working while the window
is closed.

For the Winamp mini player, turn off **Show in taskbar** under
**Settings > Winamp skins**, or **Show in taskbar** in its options menu.
The choice survives restarts. The mini player stays visible; the tray icon,
**Ctrl+M**, the skin logo, and launching Spotifast again remain ways to reach
the app. Returning to the main window always restores its taskbar button.
Changing the option while the mini player is open replaces that window while
playback continues. This setting is available on Windows and in Linux X11
sessions, where it hides the mini player from panels and task switchers that
follow the window manager's skip-taskbar state. Wayland has no standard way
for an app to leave the taskbar, so the option is not offered there; use your
desktop's window rules instead. It does not change the macOS Dock.

On Windows, since 0.8.0, the mini player starts on the current desktop if its
saved title bar is outside every connected monitor’s work area. Positions on
connected secondary monitors still restore. Reinstalling preserves settings;
it is not needed to recover a position left on an unplugged display.

## macOS Dock menu

Right-click or Control-click Spotifast's Dock icon for **Play** (or **Pause**
while music plays), **Next**, and **Previous**, above the standard Dock items.
They control the same playing device as the player bar and keep working while
the window is closed to the menu bar.

## Keeping the mini player above other windows

**Always on top** works on Windows, macOS and X11. On Wayland the app's
controls are unavailable, because your desktop manages which windows stay above
others.
Use your desktop's window rule or shortcut instead. In KDE Plasma, configure
**Keep Window Above Others** under **Settings > Keyboard > Shortcuts >
Window Management**. Your saved preference remains available when you use
Spotifast on Windows, macOS, or X11 again.

Since 0.8.0, the top bar reserves room for the device and update
badges beside Search. In narrow windows those badges show only their icons.
The bar stays above the page. Library, Queue and Lyrics keep their full height.
Hover to read the device name or available version; click to open the device
picker or update window.

## Recent

The queue panel's second tab combines Spotify's history with tracks played
through Spotifast, which Spotify does not record.

Since 0.8.0, choosing any Recent row starts that song on its own,
and the player bar shows the selection immediately while playback starts.

A song is added after about 30 seconds, or halfway through a shorter song.
Paused time and seeking do not count.

Since 0.8.0, each repeat remains a separate play, including songs
shorter than a minute. A newly loaded local repeat earns its own listening
time; pausing or seeking the current play does not create another entry.
The same play reported both locally and by Spotify appears once.

The local list is stored in `history.json` and is never uploaded. Settings →
Storage shows its location and has a **Clear history** button.

On Windows, the main window's minimize, maximize, and close buttons share the
top bar with Spotifast's controls. Drag an empty part of that bar to move or
snap the window, and drag a window edge or corner to resize it.

## Radio

Since 0.10.0, **Go to song radio** in a song's menu opens a page
of songs Spotify picks to go with it, without starting playback. Playlist,
album, and artist menus have **Go to playlist radio**, **Go to album radio**,
and **Go to artist radio**.

Spotify mixes a radio afresh each time it is asked, so the page keeps the
songs it shows: **Play**, **Shuffle**, and a double-clicked row play those
songs, and the queue names the radio. Choose **Refresh** in the page's **…**
menu for a new mix. **Save as playlist** creates a private playlist named
after the radio with the songs on the page.

Radio comes from Spotify's playback service, so it needs playback on this
computer to be set up; the page can then play on any device.

## Playlist covers

**In development, not included in 0.8.0:** open a playlist you own and choose
**Edit details → Change cover**. Select a
JPEG or PNG, check the preview, then choose **Upload cover**. Cancelling the
picker leaves your previous selection intact. An upload error keeps the
preview so you can try again. Uploading the cover is separate from **Save**,
which saves the playlist name, description, and visibility. Spotify doesn't
let apps remove a description, so clearing the field keeps the current one.
You can replace it with other text instead.

If Spotify refuses permission, sign in again and approve image uploads. If you
use a personal Spotify app, reconnect it in Settings as well.

## Lyrics

Choose the microphone button in the player bar, or press **L**, to open lyrics.
Synced lyrics follow the playing line automatically. Scroll to pause following,
choose **Follow** to resume it, or choose a line to jump to that part of the song.

**In development, not included in 0.8.0:** the expand button opens lyrics in
full screen. Press **Esc** or choose the shrink button to return to your previous
window size. Full-screen lyrics scroll smoothly and highlight the playing line
automatically. Scrolling by hand pauses following; choose **Follow** to resume.
Since 0.10.0, quitting while lyrics are full screen no longer leaves
the next launch stuck in full screen: the window returns to its previous size.

| Dark theme | Light theme |
| --- | --- |
| ![Full-screen lyrics with the dark player bar](/assets/images/lyrics-fullscreen-dark.png) | ![Full-screen lyrics with the light player bar](/assets/images/lyrics-fullscreen-light.png) |
