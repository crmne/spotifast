# Spotifast

Previously **Fastpotify**. Same native Spotify client, now at
[spotifast.rocks](https://spotifast.rocks/). The new name starts with version 0.8.0;
your existing settings and sign-ins carry over, except when
[switching the Flatpak installation](docs/_reference/renaming.md#flatpak).

**Spotify, native and fast.** Spotifast is a Spotify client written in
Rust with [egui](https://github.com/emilk/egui). It plays music through
[librespot](https://github.com/librespot-org/librespot). It typically uses
100–250 MB of RAM, while Spotify's desktop app often uses 600 MB to over 1 GB.
It runs on Linux, macOS, and Windows, starts in well under a second, and has no
browser engine.

**Playback needs Spotify Premium.** Free accounts can browse and search, but
cannot play music through Spotifast on this computer or another device.

![Spotifast Home with the playlist library, recommendations, queue, and player visible](docs/screenshot.png)

See [spotifast.rocks](https://spotifast.rocks/) for installation, setup,
everyday use, and connection details.

`spotifast` is the main command and `fastpotify` remains available
for existing scripts. Starting with 0.9.1, the existing profile and protected
sign-ins migrate to Spotifast's names. Existing destination profiles are preserved.
AUR and Homebrew packages now use the Spotifast name. See [rename compatibility](docs/_reference/renaming.md).

## What it does

- **Plays music on this computer.** Spotifast appears as a Spotify Connect
  device. Select it from your phone or play music in the app. Playback is
  gapless and supports up to 320 kbps, with
  optional volume normalisation and an on-disk audio cache.
  Stalled Spotify connections time out after five seconds per attempt so
  playback can try another endpoint.
  Since 0.8.0, a confirmed local seek discards audio queued from
  the old position. Decoder, download, and device-buffer delays can still apply.
  Starting a sorted playlist or Liked Songs view shows the requested song
  immediately while playback connects, using its loaded metadata (available since 0.8.0). Sorted views start at their first playable row. Filtering a
  playlist or Liked Songs keeps playback within the shown songs and preserves
  repeated songs; Play is unavailable when no shown song can play.
- **Controls other devices.** Move playback to a speaker, a phone, or
  another computer from the device picker, and keep controlling it: play,
  pause, skip, seek, shuffle, repeat, volume. The picker expands upward to
  show several devices at once, fitting the window; longer lists scroll.
  On `main`, after 0.8.0, switching back to this computer transfers the active
  Connect session with its song, position, queue, and paused or playing state.
- **Finds speakers on your network.** Spotifast finds librespot, spotifyd,
  and supported hardware receivers over mDNS. Once connected, they appear as
  Spotify Connect devices. The picker uses responding receivers' names and
  combines entries with the same device ID.
- **Library.** Browse playlists, Liked Songs, saved albums, followed artists,
  podcasts, and saved episodes. Filter, pin, and reorder sidebar items.
  Since 0.8.0, double-click a playlist in Library to start playback;
  a single click opens it.
  Settings offers a compact track list with one line per song and spaced
  separators between its name, artists and added date.
  Since 0.8.0, choose name, recent plays, or saved-date order where
  available. Follow Spotify’s playlist order or keep a separate local arrangement.
  Move Liked Songs among your pins or unpin it and choose its local position;
  the placement survives restarts.
  With local playback enabled, releases that the Web API groups as singles
  are labelled EP when librespot confirms that type.
  Liked Songs reopens from an account-specific metadata cache. Older rows
  refresh in the background while Like and Unlike take effect immediately.
  Right-click album, artist, and podcast cards for their actions (available since 0.8.0).
- **Search** across songs, artists, albums, playlists, podcasts, and episodes,
  with a top result and per-type views. Right-click results and cards for their actions.
  When Spotify provides an artist profile, its name on a song opens that page,
  including from the top result.
  Text fields offer Cut, Copy, Paste and Select all from their right-click menu.
  Since 0.8.0, a personal app searches the catalogue while shared
  access finds playlists. Each part appears independently, even if the other fails.

  Since 0.8.0, the search field stays clear of the device and update
  badges in narrow windows; hover their icons to read the labels.
- **Home** with Made for you, Recently played, your top artists and songs, and
  recommendations. Right-click playlist shortcuts and shelf cards for their actions.
- **Artist pages** with popular songs, a filterable discography, and related
  artists. **Album**, **playlist**, and **podcast** pages support playback
  from any row. Since 0.8.0, album and playlist scrollbars represent the full track count;
  dragging to an unloaded section fetches that section directly.
  Discography and related-artist cards also have right-click menus (available since 0.8.0).
  Artist names in the player bar open their pages, including during local
  playback before Web API metadata arrives (available since 0.8.0).
  On `main`, after 0.8.0, an album, single, or EP's **Add to queue** adds all
  its playable songs in album order, on this computer or another device.
- **Edit your playlists.** Create, rename, describe, reorder, and delete them.
  Since 0.8.0, hold a dragged song near the playlist's top or bottom
  edge to scroll to rows beyond the screen. The Library sidebar scrolls while
  dragging toward offscreen playlists too.
  On `main`, after 0.8.0, upload a JPEG or PNG cover from
  **Edit details → Change cover**.
  Add songs from a row menu, or drag a row or the currently playing song to a
  playlist in the sidebar. Since 0.8.0, dragging a selected row
  copies the whole selection in displayed order; the preview shows its count.
  Drop the selection on Liked Songs to save every selected song. Selection
  uses a translucent neutral highlight, without a row outline.
  Drop a song from the player
  bar, queue, or another list between rows of an open editable playlist to
  insert it there. This adds a copy and leaves playback and the queue unchanged.
  Clear the playlist’s filter and sort to choose an insertion position.
  Drop it on an empty playlist to add its first song.
  A playlist a friend shared with you takes songs too,
  as Spotify's own apps allow. Filter the **Add to playlist** menu by name to
  find the destination quickly. On `main`, after 0.8.0, playlist folders and
  invitation permissions also load when the library finishes before local
  playback connects.
- **Opens Spotify links.** Spotifast registers for `spotify:` links, so a
  song, album, artist, playlist, or podcast shared from another app opens
  in it, whether it is running or not. `open.spotify.com` addresses go
  through the browser, which hands them to the same handler.
- **Queue** as a side panel or a page; it names what is playing from, and
  anything can be added to it from a row menu. **Add to queue** places songs
  after those already queued and before the context continues.
  On other devices, rate-limited additions retry automatically after Spotify's
  requested wait, preserving album and song order and their known details.
  A permanent failure removes the rejected additions and reports the error.
  Starting an album or playlist keeps those additions under **Playing next**,
  even when it starts a song you also queued. **Clear queue** removes the
  additions and keeps the playing collection's remaining songs.
  Selecting repeated playlist rows queues every occurrence in the selected
  order. A repeated click counts once, and the notification counts actual additions.
  Since 0.8.0, Recent keeps repeated short-song plays separate,
  including consecutive local repeats of the same song.
  Each Recent row starts the song it names and shows it in the player bar
  immediately while playback starts.
- Since 0.8.0, a playlist's **Play** button explicitly starts at
  its first available song when Shuffle is off and the original order is
  selected. Double-click a row to start there; use the player bar to resume.
  Cached playlists must match Spotify's revision and song count before their
  rows can determine playback order. Pending playlist edits stay visible and
  are saved to that cache only after all writes succeed.
  Refresh waits for pending edits and their Spotify revision to be confirmed;
  a failed refresh keeps the current rows and offers a retry.
  Choose **Refresh** in a playlist's **…** menu to pick up changes made in
  another Spotify client.
  Large playlist checkpoints read and write their JSON through a small background
  buffer, preserving the existing cache format without another full JSON copy.
- **Lyrics.** Follow synced lyrics in a side panel or full-screen view, or read
  unsynced lyrics when timestamps are unavailable. Full-screen lyrics scroll
  smoothly and highlight the playing line automatically.
- **Resumes the last session.** On startup, the last song is paused where it
  stopped. Play resumes it, and the other playback controls work before it
  starts.
- **Album-art colour.** Pages and the player bar take a tint from the cover
  of what you are looking at or listening to. Turn it off in Settings.
- **Light and dark**, or follow the system.
- **Repeat stays selected.** On `main`, after 0.8.0, starting another song
  locally preserves Repeat, with Shuffle on or off.
- **Song changes keep the selected song.** On `main`, after 0.8.0, the default
  audio output keeps discarded audio paced while a replacement loads, preventing
  the old cached song from racing to its end and causing an extra skip.
- **Reconnects keep the queue.** On `main`, after 0.8.0, recovering an
  interrupted local playback session restores its playlist position,
  queued songs, shuffle order, and Repeat mode.
- **Light and dark**, or follow the system.
- **Native window behaviour.** On macOS, double-click the top bar to use the
  Fill, Zoom, Minimise or Do Nothing action selected in Desktop & Dock.
- **Winamp mini player.** `Ctrl+M` opens a small player for classic `.wsz`
  skins, drawn at 1x to 4x scale. It includes a spectrum analyser, playlist,
  and equalizer. It keeps its shade mode and, where the desktop permits,
  its own position when switching views. Drop a skin from the
  [Winamp Skin Museum](https://skins.webamp.org) on either window to add it.
  On `main`, after 0.8.0, switching to the mini player preserves the main
  window's size even if its native close takes another frame.
  On Windows, since 0.8.0, a mini player saved on a disconnected monitor
  starts at a default position on the current desktop.
  Clicking or double-clicking the Windows tray icon brings the window forward;
  the tray menu still offers Show or hide.
  On Windows, since 0.8.0, hide its taskbar button from Settings or the mini
  player's options menu while keeping the window and tray controls available.
  On Wayland, use the desktop's Keep Above shortcut or rule; the app's
  Always on top controls are unavailable there.

  ![The mini player wearing the built-in skin](docs/assets/images/winamp.png)
- **Equalizer.** Winamp's ten bands and presets over the music played on
  this computer, in Settings and in the skin.
- **MilkDrop.** The visualiser, powered by
  [projectM](https://github.com/projectM-visualizer/projectm), runs in its own
  window and process. It supports fullscreen and automatically downloads more
  than 10,000 `.milk` presets on first use (about 26 MB).

  https://github.com/user-attachments/assets/12b31312-0e0c-4b34-9383-e8c66aabc58d
- **Keyboard-first.** Every common action has a shortcut (`Ctrl+/` or `?` lists
  them).
- **Keeps playing when you close the window.** Spotifast stays in the system
  tray. Use the tray icon or media controls to reopen it, and quit from the
  tray menu or with `Ctrl+Q`. You can make the close button quit in Settings.
  On macOS, the Dock icon also reopens the window.
- **Visible network activity.** Pages show a spinner while loading. The top
  bar also shows slow or rate-limited Spotify requests.
- **One instance.** Launching it again brings the existing window forward
  instead of starting a second copy, on every platform.
- **Desktop integration.** MPRIS on Linux, so media keys, the shell, and
  `playerctl` see Spotifast like any other player. On macOS and Windows,
  `spotifast next` and its siblings drive the running app from a terminal,
  a launcher, or a hotkey. On Windows, since 0.8.0, hover the taskbar button
  for Previous, Play/Pause, and Next under the window preview.

## Install

On Arch Linux, Spotifast is in the AUR:

```bash
yay -S spotifast-bin      # the released build, ready made
yay -S spotifast          # the release, built from source
yay -S spotifast-git      # built from the latest commit
```

Existing AUR installations can switch with the matching command above. Accept
the offer to replace the old package; saved settings and sign-ins are kept.

On macOS, with [Homebrew](https://brew.sh):

```sh
brew install --cask crmne/tap/spotifast
```

Or [download the Mac app](https://spotifast.rocks/download/#macos), open the
downloaded file, and drag **Spotifast** to **Applications**. Starting with 0.8.0,
the Mac download passes Apple's security checks. Open it from Applications
and confirm the normal downloaded-app prompt. No Terminal commands or changes
to security settings are needed.

On Gentoo, [niko-overlays](https://github.com/NikoMalik/niko-overlays) offers
an optional **community-maintained** package. Its current `0.7.1` ebuild
builds post-release snapshot `67b8dfb`, rather than the `v0.7.1` release, and
omits MilkDrop. Use the released binary or build instructions below if you
want the standard release and feature set.

To enable the overlay with `eselect-repository`, run as root:

```sh
emerge --ask app-eselect/eselect-repository
eselect repository add niko-overlays git https://github.com/NikoMalik/niko-overlays.git
emaint sync -r niko-overlays
emerge --ask --autounmask-write media-sound/fastpotify::niko-overlays
```

Review and apply any proposed keyword changes with `dispatch-conf`, then
repeat the final `emerge` command.

Everywhere else, build the single binary with Rust 1.95 or newer:

```bash
cargo install --path . --locked
```

Upgrading a previous Cargo installation requires `--force` once to transfer
the existing commands to the renamed package.

MilkDrop uses libprojectM, which is built from source. This needs CMake, a C++
compiler, and libclang. To build without MilkDrop or those tools, run
`cargo install --path . --locked --no-default-features`. On Linux, you also need the
development packages for ALSA, PulseAudio or PipeWire, and the windowing
libraries. On Arch:

```bash
sudo pacman -S --needed alsa-lib libpulse libxkbcommon wayland cmake clang
```

and on Debian or Ubuntu:

```bash
sudo apt install libasound2-dev libpulse-dev libxkbcommon-dev libwayland-dev \
  cmake clang libclang-dev
```

and on Fedora:

```bash
sudo dnf install alsa-lib-devel pulseaudio-libs-devel libxkbcommon-devel \
  wayland-devel cmake clang clang-devel
```

On Windows, libprojectM is built with Visual Studio 2022, CMake, LLVM, and
vcpkg (`vcpkg install glew:x64-windows-static`, with
`VCPKG_INSTALLATION_ROOT` pointing at the vcpkg folder).

With [Nix](https://nixos.org), `nix develop` provides all of it, along with
the exact toolchain `rust-toolchain.toml` pins.

An official public binary cache is not active yet. CI can publish its Linux
Nix builds once a maintainer configures Cachix; see
[Nix binary cache setup](docs/_reference/nix-cache.md).

On macOS, `packages.<system>.spotifast` contains the native binary and an
ad-hoc signed `Spotifast.app` bundle for the Dock, Launch Services, and
`spotify:` links. With nix-darwin, add `spotifast` to
`environment.systemPackages` and link `"/Applications"` through
`environment.pathsToLink`; with Home Manager, `home.packages` is enough, as
its darwin support links the bundle into `~/Applications`.

Since 0.8.0, system fallback fonts align with Latin text, including
Japanese titles drawn with Hiragino Sans on macOS. Yi characters used in
stylized artist names also use an installed fallback font instead of empty boxes.

Spotifast uses system fonts for scripts not covered by its interface font,
including Chinese, Japanese, Korean, Arabic, Hebrew, Thai, and Indic scripts.
On macOS it draws each of them with the face the system itself uses, in the
language order set in System Settings, so Chinese titles follow the
Traditional or Simplified preference set there. Windows includes common
fonts. On Linux, install `noto-fonts` and `noto-fonts-cjk` (Arch) or
`fonts-noto` and `fonts-noto-cjk` (Debian or Ubuntu) if titles appear as
empty boxes.

Since 0.8.0, long right-to-left titles in song rows and the player
bar end with an ellipsis inside their text area, including joined Arabic letters.

On `main`, after 0.8.0, the Linux launcher is
`packaging/applications/spotifast.desktop`. Its icon and window identity also
use Spotifast, while existing settings and window positions are preserved.
After installing it, `xdg-mime default spotifast.desktop x-scheme-handler/spotify`
chooses Spotifast for `spotify:` links. The published 0.8.0 packages still use
`fastpotify.desktop`; use that name with `xdg-mime` until updating.

## Sign in

Press **Sign in with Spotify**. Your browser opens Spotify's consent page
(Authorization Code with PKCE), so Spotifast never sees your password. The
app keeps its grants in the system credential store: Secret Service on Linux,
Keychain on macOS, and Credential Manager on Windows. You usually sign in once
per machine. If the store is unavailable or locked, a new sign-in works for
this session and Spotifast explains that it could not save it.

Playing music **on this computer** needs a second, one-time browser approval.
Spotify handles streaming separately from library access. Start it from the
device menu (**Set up playback here**) or Settings. It needs Spotify
Premium. Its reusable credential uses the same protected storage, independently
of the two Web API grants.

Since 0.8.0, local playback tries the other available server
addresses when one cannot connect, including a prompt IPv4/IPv6 fallback.
Socket and proxy tunnel setup have a five-second limit. See
[how it connects](docs/_reference/how-it-connects.md#the-engine).

Existing token files migrate after the protected write has been read back
successfully. A failed migration keeps the original for recovery and reports
an error. Sign-out removes shared, personal, and playback grants, including
legacy files and pending writes. Non-secret revocation markers prevent a
failed keychain deletion from silently restoring a signed-out session.
See [credential storage and file locations](docs/_reference/settings-and-files.md).
Since 0.8.0, Flatpak also preserves its fallback state directory
across full quits, including on older Flatpak versions.
New Flatpak builds use the application ID `rocks.spotifast.Spotifast`.
Existing Flatpak users install the new application and remove the old one;
see [switching Flatpak installations](docs/_reference/renaming.md#flatpak)
for retaining settings and history.

Playback approval requests Spotify's streaming permission separately. A
verified personal app can complete sign-in while the shared app is busy.

The Web API uses a shared app by default. You can add a personal Spotify
Development Mode app in Settings → Account for a separate quota. Spotifast
still uses the shared app for requests that personal apps do not support.
Since 0.8.0, Premium listeners using shared access see a one-time
prompt explaining the personal app option, with a button that opens setup.
Dismissal is remembered across restarts.
Playlists the shared app would serve are read over the local playback session
instead when it is signed in. If Spotify stays busy and no personal app is
configured, Spotifast points you to that setting at most once a day.

## Account safety

We are not aware of a Spotify account being suspended for using Spotifast
or another librespot player with Premium. Sign-in happens on Spotify's own
pages, audio uses the quality included with Premium, DRM stays intact, and
Spotifast does not rip tracks or block ads.

Reported suspensions usually involve modded apps that remove ads from free
accounts, track ripping, or stream manipulation. Spotifast does none of
those things, and [CONTRIBUTING.md](CONTRIBUTING.md) prohibits them.

## Keyboard shortcuts

Since 0.8.0, text fields keep their usual Ctrl, Cmd and Alt arrow
keys for moving the caret while you type.

Hold `Shift` while turning the mouse wheel to scroll horizontal shelves,
including Made for you and Recently played on Home.

The main window exposes named playback controls, library and song rows,
menus, sliders, and settings switches to screen readers. Use `Tab` and
`Shift+Tab` to move focus, then `Enter` to activate a control or
play a focused song. On `main`, after 0.8.0, `Space` always plays or pauses
the current song unless a text field has focus. In a playlist, album or Liked
Songs, up and down arrows move focus between whole song rows in the displayed order and scroll them
into view. Tab still reaches the artist links, Like and More controls.
Left and right arrows adjust a focused volume or seek
slider. Windows testing with NVDA and accessibility for Winamp skins are
still in progress.

| Shortcut | What it does |
| --- | --- |
| `Space` | Play or pause |
| `Ctrl+←` / `Ctrl+→` | Previous or next |
| `Shift+←` / `Shift+→` | Seek 10 seconds |
| `Ctrl+↑` / `Ctrl+↓` | Volume |
| `M` | Mute |
| `B` | Like or unlike the playing song |
| `S` / `R` | Shuffle / cycle repeat |
| `Q` | Queue panel |
| `Ctrl+F` or `/` | Search |
| `Ctrl+B` | Show or hide the sidebar |
| `Alt+←` / `Alt+→` | Back or forward |
| `Ctrl+H` / `Ctrl+L` | Home / Liked Songs |
| `Ctrl+Shift+A` / `Ctrl+Shift+B` | Playing artist / album |
| `Ctrl+M` | Winamp mini player |
| `Ctrl+Shift+K` | MilkDrop |
| `Ctrl+,` | Settings |
| `Ctrl+/` or `?` | All shortcuts |
| `Ctrl+Q` | Quit |

On macOS, `Cmd` replaces `Ctrl`.

On Windows, since 0.8.0, middle-click a scrolling list and move the pointer to
autoscroll. Click, press Esc, use the wheel or switch windows to stop.
It works automatically, with no Settings toggle. See [autoscroll](docs/_guide/using-spotifast.md#middle-click-autoscroll).

## Controlling it from outside

On Linux, Spotifast is an MPRIS player, so `playerctl --player=spotifast
play-pause` already works. `spotifast like` adds or removes the playing
track from your library.

macOS and Windows have no such bus, so the same verbs are subcommands. They
talk to the instance already running and print nothing on success:

```
spotifast play-pause          spotifast volume 40
spotifast play                spotifast volume-up [percent]
spotifast pause               spotifast volume-down [percent]
spotifast next                spotifast mute
spotifast previous            spotifast shuffle [on|off]
spotifast seek 15             spotifast repeat [off|context|track]
spotifast seek -- -15         spotifast like
spotifast seek-to 90          spotifast play-uri spotify:playlist:37i9…
spotifast show                spotifast transfer <device-id>
spotifast now-playing [--raw] spotifast devices [--raw]
```

`shuffle` and `repeat` toggle when used without an argument. Pass a state to
set it directly. `like` adds or removes the playing track from your library.

`now-playing` prints one readable line. `--raw` prints tab-separated fields:
state, title, artists, album, position_ms, duration_ms, volume, shuffle,
repeat, art_url, saved, and device. `saved` is `yes`, `no`, or `unknown` while
loading. New fields are appended to keep older scripts working.

`devices` lists Spotify Connect devices with the ID first and the active one
marked with `*`. `--raw` prints JSON. The command refreshes the device list,
so the first call after startup may be empty. Run it again if needed.

A verb exits non-zero when Spotifast is not running.

On every platform, `spotifast <link>` opens a Spotify link, a `spotify:`
URI or an `open.spotify.com` address, in the running app, or starts the
app on it. This is what the desktop runs when a link is clicked.

On `main`, after 0.8.0, search links open Search with the query filled in,
without starting playback. They work on a fresh launch or an existing window;
if signed out, the search waits for sign-in. For example:

```sh
spotifast 'https://open.spotify.com/search/here%20comes%20the%20sun'
# Linux, with Spotifast already running:
playerctl --player=spotifast open 'https://open.spotify.com/search/here%20comes%20the%20sun'
```

Use `spotify:search:here%20comes%20the%20sun` for the equivalent Spotify URI,
or `https://open.spotify.com/search` to open an empty search box. Encode spaces
as `%20`; a `+` in the path remains a literal plus. Ordinary MPRIS playback
URIs still start playback.

Launchers such as Raycast or Alfred can use these commands. The Stream Deck
plugin uses the same interface.

## Settings

Settings live in one readable JSON file (`~/.config/spotifast/settings.json`
on Linux). They include the Connect device name, bitrate, normalisation,
autoplay, gapless playback, the audio backend (PulseAudio/PipeWire or ALSA on
Linux), audio cache size, theme, sidebar state, whether pages take colour
from artwork, and the mini player's skin and size.
Since 0.8.0, you can hide Made for you and Recommended for you
from Home through JSON preferences; see
[Home shelves](docs/_reference/settings-and-files.md#home-shelves).
Custom JSON palettes go in a `themes` folder beside `settings.json`.
Select them in Appearance; `spotifast reload-themes` loads additions and updates
without interrupting playback. See [custom themes and Omarchy integration](docs/_reference/settings-and-files.md#custom-themes).
New installations default to **Follow system**. Linux packages include Omarchy
integration, set up its per-user template and hook automatically on first launch,
and follow the current palette. Existing theme choices and custom files stay
intact. The picker lists Follow system, Light and Dark first, then the available Omarchy
integration and local palettes. The **Open themes folder** button in Settings
opens the local JSON palette directory.
Playback settings apply when you press **Apply and restart playback**.
The Settings page has its own search: type under the title to narrow the
rows, clear the field to see everything again.
You can also check for a new release from Settings. On macOS, the same command
is in the application menu.

The Windows installer, Mac app, and portable Windows and Linux downloads
support updates from inside Spotifast. Click the green update button to
download a release, then choose when to restart and install it. Settings can
enable automatic background downloads; restarting always waits for your click.
Closing the update window keeps a download running.

Spotifast checks each download before installing it. An interrupted or damaged
download leaves the running app alone, and a failed startup restores the
previous installation. Updates keep your settings and sign-ins. On macOS,
move Spotifast to Applications before updating it. See
[how updates are checked and installed](docs/_reference/how-it-connects.md#what-the-client-stores)
for the technical details.

Package-managed installations continue to update through their package manager,
including Homebrew, Flatpak, apt, dnf, pacman, Nix, and Cargo. Unrecognized
installations use the download page. Portable archives identify themselves with
`spotifast-portable.txt`; older archives need one manual upgrade to an
update-enabled build.

On `main`, after 0.8.0, Off and System proxies apply immediately. HTTP and SOCKS5 apply
when you press **Apply settings**, and can also be set on the sign-in screen.
A proxy password uses the system credential store, separately from Spotify
sign-in. Only confirmed proxy settings are saved; editing a form does not
change the active connection until you apply it.
Proxy authentication covers Web requests. Local playback can use only an
unauthenticated HTTP proxy; with proxy login or SOCKS5 it connects directly.

Caches (audio, artwork) live under the cache directory and can be deleted at
any time without signing you out.
On `main`, after 0.8.0, downloaded artwork uses less temporary memory while
being saved to the cache. Visible cards and collection covers use sharper
640-pixel artwork. Loading transitions may retain up to 64 softened previews
in addition to the existing artwork-cache budget. These previews reuse decoded
artwork, so keeping a collection open does not repeatedly reload its cover.

For blank or incorrectly drawn windows, include `spotifast.log` in the bug
report. On `main`, after 0.8.0, it records the app version, platform and active
OpenGL renderer, plus window-creation errors even when launched without a console.

## How it is built

- `src/player.rs`: librespot playback, mixing, and Spotify Connect state.
- `src/api/`: shared and personal Web API sessions, routing, concurrency, and
  rate limits.
- `src/backend.rs`: the tokio runtime and channels used by the interface.
- `src/images.rs`: album art loading, caching, and accent-colour extraction.
- `src/app.rs`, `src/model.rs`, `src/ui/`: state, navigation, and views.
- `src/mpris.rs`: Linux media controls.

Spotifast pins its Rust toolchain in `rust-toolchain.toml`; `cargo test`
covers the API models, dual-session routing, PKCE, the player state machine,
and a headless render of every page, panel, and dialog.

To look at the interface without a Spotify account, build with the `demo`
feature and start it with sample data:

```bash
cargo run --features demo -- --demo --demo-page playlist:pl1 --demo-show queue
```

Demo mode never writes settings. `--demo-shot <PATH>` writes the window to a
PNG and exits, which is useful for reproducible interface screenshots.
`--demo-size WIDTHxHEIGHT` sets the window size in logical pixels for that shot.
Demo windows ignore saved window geometry and do not save window or interface state.
Use `--demo-data <DIRECTORY>` to keep demo caches and logs in a separate directory.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening an issue or pull
request. It covers project scope and required checks.

Translations use standard gettext `.po` files in `assets/i18n/`, with an English
`.pot` template. The current pilot translates navigation, Library labels,
player-bar controls, Queue and Lyrics panels, and shared loading/retry labels
in 12 languages, including
Portuguese and Chinese variants, in demo mode; the
production interface remains English. See
[Translating Spotifast](docs/_reference/translating.md) for editing with existing
translation tools, previewing, and reporting translation problems.

New and reopened issues and new discussions receive automated triage through
[Copilot Triage](https://github.com/crmne/copilot-triage). A party-popper reaction
marks a completed assessment; it does not promise a reply or a fix. See
[automated triage](CONTRIBUTING.md#automated-triage) for details.

## Acknowledgements

Spotifast uses [librespot](https://github.com/librespot-org/librespot),
[egui](https://github.com/emilk/egui), the [Inter](https://rsms.me/inter/)
typeface (OFL), and [Lucide](https://lucide.dev) icons (ISC).

Spotifast is an independent project and is not affiliated with Spotify.
Spotify is a trademark of Spotify AB.

Licensed under the [MIT License](LICENSE).

## Packaging maintenance

Release packaging uses the [native-packages](https://rubygems.org/gems/native-packages) gem. macOS release builds automatically sign and notarize when the Apple CI credentials are configured. `native-packages.yaml` declares packages and downstream repositories; native recipes and installation assets live in `packaging/`; see [PACKAGING.md](PACKAGING.md) for local commands and CI behavior.
