---
title: Getting Started
description: Install the app, sign in through your browser, and enable playback on this computer.
nav_order: 2
---

## Install

Choose your system on the [Download page](/download/) and follow its
installation steps. Then open **Spotifast**.

## Sign in

Spotifast signs in through a Spotify app of your own. Spotify no longer lets
one app serve everyone, so each listener registers a free one. It takes a few
minutes and needs no code, and Spotify requires the account that creates it
to have Premium. The sign-in screen shows these same steps.

### Make a Spotify app {#make-a-spotify-app}

1. Open the [Spotify developer dashboard](https://developer.spotify.com/dashboard)
   and sign in with your Spotify account.
2. Click **Create app**. Any name and description will do; nobody else
   sees them.
3. Under **Redirect URIs**, add exactly this address. The sign-in screen
   has a **Copy** button for it.

   ```
   http://127.0.0.1:8989/login
   ```

4. Tick **Web API**, accept the terms, and save.
5. The app's page shows its **Client ID**. Copy it.

### Sign in with it

Paste the Client ID into the sign-in screen and press **Sign in with
Spotify**. Your browser opens Spotify's sign-in page for your app. Approve
access there, then return to Spotifast to see your library. Spotifast never
sees your Spotify password.

- If Spotify's page says **Invalid redirect URI**, the address from step 3
  is missing or differs by a character. Add it exactly, save, and sign in
  again.
- If the sign-in screen says Spotify doesn't know the Client ID, copy it
  again from your app's page. Spotifast asks Spotify before opening the
  browser.
- If it says Spotify didn't let the account use the app, sign in with the
  Spotify account that created it, and check that it has Premium.

Spotifast remembers your sign-in using your computer's protected storage,
so you normally do not need to sign in each time you open the app. It keeps
the Client ID after you sign out, so signing in again is one click.

If you signed in before version 0.14.2, you stay signed in as before until
Spotify stops accepting it; then the sign-in screen asks you to make your own
app. [Make It Even Faster](/make-it-even-faster/#shared-app) explains why.

## Enable playback on this computer

Since 0.15, Spotifast leads with **Set up Soloist** after
sign-in, including the first launch after updating. Follow the
[Soloist setup](/soloist/) or choose **Use the built-in player** for the
playback described below. Your choice is remembered. Soloist setup appears
only on platforms Spotify supports and is skipped for Spotify Free accounts.

**Playing music requires Spotify Premium.** To listen on this computer,
open the device menu in the bottom player bar and select **Set up playback
here**, or find the same option in Settings.

Spotify asks you to approve playback separately from library access. Follow
the browser prompt once; Spotifast remembers this approval too.
[Read more about the two sign-ins](/how-it-connects/).

The computer then appears as a Spotify Connect device named **Spotifast**.
You can rename it in Settings.

## Basics

- **Closing the window does not stop the music.** Spotifast keeps playing
  from the system tray; reopen it from the tray icon and quit from the tray
  menu or Ctrl+Q. On macOS you can also reopen it from the Dock. Settings can
  turn this off, and on macOS can keep Spotifast out of the Dock, in the menu
  bar only. On Linux, including Flatpak, a desktop with a working system
  tray is required for this behavior.
- **The tray icon can match your panel.** On Linux and Windows, **Settings >
  Playback on this computer > Tray icon** shows it in colour, white or black.
  The macOS menu bar already draws it black or white to match itself.
- **Spotifast can start with your computer.** Under **Settings > Playback on
  this computer**, **Start with your computer** opens it when you log in,
  **With the window** or quietly **In the tray** (**In the menu bar** on
  macOS).
- **Play and Pause fade.** With the default audio settings, music played on
  this computer fades in or out to avoid a hard cut.
- **Play buttons show progress.** The button spins until Spotify responds.
- **Pages keep their shape while they load.** Until Spotify answers, grey
  rows and cards in the shape of the songs, albums and playlists to come
  stand in for them. A list that is loading more at its end still shows
  **Loading…**.
- **Artist names are links.** Click a credited artist in the player bar to
  open their page, even while the rest of the song's details are loading.
- **Common actions have shortcuts.** Space plays and pauses, Ctrl+F or `/`
  searches, and `Q` opens the queue. Ctrl+/ shows the full list.
- **Right-click for more actions.** Right-click a song, playlist, album,
  artist, or podcast to see its menu. These menus are available in Home,
  Search, Library, and artist pages. Your own playlists include **Edit details**
  and **Delete**. Opening a menu does not start playback.
  In **Add to playlist**, type a playlist name to find it, or choose
  **New playlist**. Typing chooses the first match, so `Enter` adds to it;
  the up and down arrows choose another. You can add one song or a selection.
  If the playlist already contains the song, Spotifast asks before adding
  another copy.
- **The heart likes, then adds to a playlist.** In the player bar, the heart
  saves the playing song to Liked Songs. Once it is liked, the heart opens
  **Add to playlist** instead, as in Spotify, with **Remove from Liked Songs**
  below the playlists.
- **Spotify links open in Spotifast.** A `spotify:` link shared from another
  app opens its page, starting Spotifast if it is not running. Links to
  `open.spotify.com` go through the browser first, which hands them over the
  same way. `spotifast <link>` does the same from a terminal.

## Match your desktop theme

Open **Settings → Appearance → Theme** and choose **Light**, **Dark**, or
**Follow system**. Follow system matches your desktop's appearance.

With Follow system, **Light mode theme** and **Dark mode theme** below it
pick the theme for each: for example Catppuccin Latte while your computer is
light and Catppuccin while it is dark. Spotifast switches between them as
your computer does, without interrupting the music.

On Omarchy, Spotifast matches your desktop theme from the first time you open
it; the AUR package also installs the theme hook. Choose **Follow system** or **Omarchy**, then change your
desktop theme: Spotifast's colours follow while the music keeps playing.
New installations already use Follow system. Updating keeps your previous
theme choice.

For your own colours or a manual installation, see
[custom themes and Omarchy setup](/settings-and-files/#custom-themes).

## If song titles show empty boxes

Spotifast uses your computer's fonts to display titles in different languages.
macOS and Windows already include fonts for most languages. On Linux,
install `noto-fonts` and `noto-fonts-cjk` (Arch) or `fonts-noto` and
`fonts-noto-cjk` (Debian or Ubuntu) if letters are missing.
For colour emoji, install the desktop's colour emoji font
(`noto-fonts-emoji` on Arch, `fonts-noto-color-emoji` on Debian or Ubuntu).

Titles can mix languages, including those written from right to left.
Long titles are shortened with dots to fit the available space.

![Japanese, Chinese, and Korean titles in a playlist](/assets/images/scripts.png)

## Choosing the interface language

Spotifast uses your computer's language when it has a translation for it, and
English otherwise. To pick another language, open **Settings → Appearance →
Language**. Each language is listed under its own name, and the change applies
at once. Choose **System** to follow the computer again. Spanish, Turkish and
Ukrainian are complete; other translations are in progress, and anything not yet
translated appears in English. See [Translating Spotifast](/translating/) to help.

## Choosing which app opens Spotify links

On macOS, opening Spotifast makes it available for Spotify links. If Spotify's
own app is installed too, macOS uses whichever app last registered for them.
On Windows, choose the app in **Settings → Apps → Default apps**.

On Linux, after installing the current app launcher:

```sh
xdg-mime default spotifast.desktop x-scheme-handler/spotify
```

## If your network needs a proxy

**In development, not included in 0.8.0:** you can configure a proxy, a server
your network uses to reach the internet, on the sign-in screen or in
**Settings → Proxy**. Enter the address, port, and any login details supplied
by your network administrator. Spotifast protects the saved proxy password.
If it cannot save the password, it tells you and uses it only until you quit.

Playing music on this computer supports only HTTP proxies without a login.
With other proxy types, music connects directly to Spotify even though
browsing uses the proxy. Your browser has separate proxy settings, which may
also need configuring for sign-in.
See [proxy options and limits](/how-it-connects/#proxy).

## Build from source

Build the single binary with Rust 1.98 or newer:

```bash
cargo install --git https://github.com/crmne/spotifast --locked
```

MilkDrop uses libprojectM, which is built from source. This needs CMake, a C++
compiler, and libclang. To build without MilkDrop or those tools, add
`--no-default-features`. This also leaves out native Soloist audio capture on
Mac and Windows. To keep that capture without MilkDrop, add
`--no-default-features --features soloist-native-audio` (a C++ compiler is
still needed). On Linux, you also need the development packages for
ALSA and PipeWire, and the windowing libraries. On Arch:

```bash
sudo pacman -S --needed alsa-lib pipewire libxkbcommon wayland cmake clang
```

On Debian or Ubuntu:

```bash
sudo apt install libasound2-dev libpipewire-0.3-dev libxkbcommon-dev libwayland-dev \
  cmake clang libclang-dev
```

On Fedora:

```bash
sudo dnf install alsa-lib-devel pipewire-devel libxkbcommon-devel \
  wayland-devel cmake clang clang-devel
```

On Windows, libprojectM is built with Visual Studio 2022, CMake, LLVM, and
vcpkg (`vcpkg install glew:x64-windows-static`, with
`VCPKG_INSTALLATION_ROOT` pointing at the vcpkg folder).

With [Nix](https://nixos.org), `nix develop` in a checkout provides all of it,
along with the exact toolchain `rust-toolchain.toml` pins.
