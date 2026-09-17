---
title: Settings & Files
description: Configuration, credential and cache locations, and what is safe to delete.
nav_order: 0
---

## Where things live

Spotifast was previously called Fastpotify. The existing `fastpotify` file
paths, credential-store IDs and desktop integration IDs stay the same, so the
rename does not require signing in again or moving your settings. Both
`spotifast` and `fastpotify` commands control the same running app. Saved Spotify
Connect device names remain unchanged; new settings default to `Spotifast`.

Spotifast follows each platform's conventions. On Linux:

| What | Where | Safe to delete? |
| --- | --- | --- |
| Settings | `~/.config/fastpotify/settings.json` | Yes, you lose preferences |
| Winamp skins | `~/.config/fastpotify/skins/` | Yes, you add them again |
| MilkDrop presets | `~/.config/fastpotify/milkdrop/` | Yes, you fetch them again |
| Spotify grants (available since 0.8.0) | System credential store | Use Sign out in Settings |
| Credential revocation markers (available since 0.8.0) | `~/.local/state/fastpotify/credential-storage/` | Keep after a failed sign-out deletion |
| Legacy shared Web API grant | `~/.local/state/fastpotify/shared_web_api_token.json` | Removed after migration or sign-out |
| Legacy personal Web API grant | `~/.local/state/fastpotify/personal_web_api_token.json` | Removed after migration or sign-out |
| Legacy playback credential | `~/.local/state/fastpotify/credentials/` | Removed after migration or sign-out |
| Proxy password | System credential store | Clear the password and apply the manual proxy settings |
| Legacy proxy password | `~/.local/state/fastpotify/proxy_password` | Removed after protected migration |
| Last session | `~/.local/state/fastpotify/session.json` | Yes |
| Play history | `~/.local/state/fastpotify/history.json` | Yes |
| Audio cache | `~/.cache/fastpotify/audio/` | Always |
| Artwork cache | `~/.cache/fastpotify/art/` | Always |
| Lyrics cache | `~/.cache/fastpotify/lyrics/` | Always |
| Account-scoped playlist page cache | `~/.cache/fastpotify/playlists/<account-id>/` | Always |
| Last run's log | `~/.local/state/fastpotify/fastpotify.log` | Always |
| Crash log | `~/.local/state/fastpotify/panic.log` | Always |

Clearing caches never signs you out. Sign-out from Settings covers the shared
and personal Web API grants and the independent playback credential.

The following credential storage is available since 0.8.0.

Durable grants use **Secret Service on Linux**, **Keychain on macOS**, and
**Credential Manager on Windows**, under the service name
`rocks.fastpotify.Fastpotify`. Entries are separated by application state
location and grant type; Web grants carry their Client ID and must verify as
the same account. Playback and receiver activation require that account too.
Non-secret settings and session data remain readable JSON. Native credential
protection reduces exposure from copying ordinary application files. It does
not protect a usable session from arbitrary code running as the same user;
Linux protection also depends on the desktop keyring's configuration.

On Linux, enable and unlock a Secret Service provider such as GNOME Keyring or
KWallet to remember a new sign-in. Flatpak is allowed to talk to
`org.freedesktop.secrets` for this purpose. An unavailable or locked store
produces an error without blocking the interface. A new sign-in can still be
used for this session, with no new plaintext fallback file.

On upgrade, each legacy grant is written to the protected store and read back
before its old file is removed. Valid grants migrate without signing in again.
If Spotify rejects a saved refresh grant, only that grant is forgotten so the
next launch cannot keep restoring it. If migration fails, Spotifast reports it and
keeps the original so migration can be retried. That grant can still serve the
current session. A successfully migrated grant is never replaced by a stale
legacy copy. Librespot's reusable grant stays in memory until Spotifast saves
it through this same store. Volume and disposable audio caches are independent.

Sign-out invalidates pending authorization, refresh, and playback connections,
and cancels pending Spotify requests so their results cannot undo a new sign-in.
It records non-secret revocation markers before deleting the protected entries
and all legacy token files, including temporary copies. A locked store or
filesystem failure is reported. Revocation markers prevent a failed protected
entry deletion from restoring the session on restart; keep these markers when
a deletion failed. Removing or changing a personal Client ID clears that app's
old grant.

Version 0.7.1 and earlier use the legacy unencrypted files listed above. Their
Web API writer requests owner-only permissions for newly created Unix files;
Windows uses inherited permissions. Librespot's old writer uses the system's
file defaults. Keep these legacy files, their temporary copies, the
`credentials/` directory, and credential-store exports out of issue attachments
and diagnostic uploads.

On `main`, after 0.8.0, proxy passwords use a separate protected entry under the same service name.
They belong to the configured host, port, and username. Editing any of these
fields clears the old password; changing only HTTP/SOCKS5 mode keeps it. Off,
System, and Spotify sign-out retain the saved manual proxy password. Clear the
password and apply the manual settings to forget it. A revocation marker keeps
a failed protected deletion from restoring that password.

Older proxy passwords, whether in `settings.json` or the separate state file,
are migrated and read back before their plaintext copies are removed. If that
fails, the originals remain available for retry. Settings changes stay in
memory until migration succeeds, so a save cannot erase the only password or
associate it with a different proxy address. Spotifast reports this condition.
Newly entered passwords have no plaintext fallback. Settings store the confirmed
proxy mode, host, port, and username, never an unapplied draft or password.

Progress through a playlist is periodically cached as a contiguous prefix.
When the playlist has not changed on Spotify, reopening it resumes from that
prefix instead of requesting the same pages again. Spotifast validates the
cache against Spotify's playlist snapshot and reported song count before
showing it. A cache with a mismatched count is replaced by live rows even if
its snapshot matches, so stale cached songs cannot choose the playback order.
Successful playlist edits keep that loaded prefix and save it under Spotify's
new snapshot after all pending writes have succeeded. Pending edits remain
visible immediately, but are not saved as confirmed playlist rows. A failed
write reloads the playlist to reconcile the edit.

Since 0.8.0, playlist checkpoints stream their JSON to a temporary
file on a background file worker. Saving a large playlist no longer needs a
second complete JSON buffer in memory. The cache format and checkpoint order
are unchanged, and a failed write leaves the previous cache in place.

On `main`, for the release after 0.8.0, reading a playlist cache also uses
a small buffer on a background file worker. The full JSON file no longer
stays in memory alongside the loaded songs. Existing caches remain readable;
missing or invalid caches are ignored and fetched again as before.

On `main`, after 0.8.0, the artwork loader shares downloaded image bytes with
the background cache writer instead of making a separate copy. Visible library
cards and collection covers request 640-pixel artwork for sharper HiDPI output;
compact rows and softened placeholders keep using 64-pixel thumbnails. Up to 64
softened 256-pixel covers may be retained in addition to the existing artwork
budget, and their downloaded JPEG bytes are released after decoding. A failed
cache write does not prevent the downloaded image from being displayed.

The following Liked Songs caching behavior is on `main`, for the release
since 0.8.0.

Liked Songs metadata is stored separately under `liked-songs/` in the cache
directory, one JSON file per account. Only the verified account's rows are
shown. Fresh cached pages are reused for 15 minutes; older pages refresh in
the background. Refreshing keeps the last usable rows until their replacement
is ready, and a failed refresh leaves those rows visible. The refresh control
requests current data immediately. Partial caches resume from their next page.
Like and Unlike change the rows immediately, and confirmed edits survive a
restart even if Spotify's next read still reports the old state. This cache
contains metadata, not offline audio, and can be deleted without signing out.

The last good playlist folder tree is kept in `session.json`, scoped to the
account that supplied it. This keeps folders visible when local playback is
temporarily unavailable. Live session data is still required for edit grants.

Since 0.8.0, memory caches retain the open page,
the playing context, and a limited set of recently used playlist, album,
artist, and show pages. Older pages reload when revisited, using the saved
playlist prefix when its snapshot still matches. Pending playlist edits and
their rows stay in memory until the write and its snapshot are confirmed,
even if this temporarily exceeds the usual page limit. Track metadata is
limited to 800 cached tracks; navigation and periodic cleanup trim old entries.

The session remembers separate positions for the main window and the Winamp
mini player. The shade modes are kept in `settings.json`. Wayland compositors
may ignore saved positions. On Windows, a position
whose title bar is no longer on an available monitor's work area is discarded
when reopening the window, keeping its initial on-screen placement instead.

Since 0.8.0, a main window left maximized or full
screen reopens that way, and comes back that way from the mini player. The
remembered size and position describe an ordinary window and are not applied
to one that already fills the screen, because sizing or moving such a window
restores it down.

Since 0.8.0, album and playlist scrollbars reserve the full track count
as soon as Spotify reports it. Dragging to an unloaded section shows placeholders and requests
that section directly. Loaded windows stay in memory while the page is retained;
returning to one does not download it again. Unavailable entries keep their row
positions. Playlist edits and refreshes invalidate other cached windows because
their server positions may have changed. Only contiguous playlist prefixes are
saved on disk.

Large playlist pages also have a **Go to song** control. Entering a song
number scrolls to that row and loads its 50-item page if needed. Filtering or
sorting returns to the beginning and loads remaining pages as needed, since
local search and ordering require the track metadata. A failed window stops
automatic requests and shows a Retry button in the reserved row space.

Since 0.8.0, Flatpak also preserves the fallback
state directory used when `XDG_STATE_HOME` is unset. Session state, history,
logs, and credential revocation markers survive a full quit and relaunch under
`~/.var/app/rocks.spotifast.Spotifast/.local/state/fastpotify/` in newly named
Flatpak builds. Older bundles use `~/.var/app/rocks.fastpotify.Fastpotify/`
as their application data root. Configuration
and caches remain under the app's `config/` and `cache/` directories. State
already lost on quitting an older release cannot be recovered.
See [switching Flatpak installations](/renaming/#flatpak) to retain existing
settings and history when installing the new application ID.

On macOS, settings, state, and the logs are in
`~/Library/Application Support/me.paolino.fastpotify` and the caches in
`~/Library/Caches/me.paolino.fastpotify`. On Windows, settings are in
`%APPDATA%\paolino\fastpotify\config`, state and the logs in
`%LOCALAPPDATA%\paolino\fastpotify\data`, and the caches in
`%LOCALAPPDATA%\paolino\fastpotify\cache`.

## settings.json

Settings are stored in one readable JSON file and written atomically. Its
main fields are:

| Field | Default | Meaning |
| --- | --- | --- |
| `device_name` | `Spotifast` | Name on Spotify Connect |
| `bitrate` | `320` | 96, 160, or 320 kbps |
| `normalisation` | `false` | Volume normalisation |
| `autoplay` | `true` | Keep playing similar music at the end |
| `gapless` | `true` | Gapless playback |
| `audio_backend` | platform | `pulseaudio` or `rodio` on Linux |
| `audio_cache_mb` | `1024` | On-disk audio cache budget |
| `theme` | `system` | Follow the system appearance by default; explicit `dark` and `light` choices remain available |
| `custom_theme` | `null` | Selected JSON filename from the `themes` folder |
| `custom_theme_cache` | absent | Last accepted custom palette; preserves appearance if its file is missing or invalid |
| `system_theme_cache` | absent | Last accepted Omarchy palette for Follow system; retained across restarts |
| `accent_from_art` | `true` | Tint pages with album art |
| `library_sort` | `{}` | Per-section Library order overrides, since 0.8.0: `library`, `recently_played`, `name`, `recently_added`, `local`, or `spotify`, where supported |
| `sidebar_order` | `[]` | Saved local playlist arrangement, including an unpinned Liked Songs, retained when another sort is selected |
| `pinned_contexts` | `[]` | Local Library pin order; Liked Songs uses `fastpotify:liked-songs`, a local key never sent to Spotify |
| `liked_songs_pinned` | `true` | Keep Liked Songs in the pin block; older settings place it first until moved |
| `sidebar_compact` | `false` | Names only in the library sidebar, no covers |
| `tracklist_compact` | `false` | One-line track rows without covers |
| `winamp_window` | `false` | The window is the Winamp mini player |
| `winamp_show_taskbar` | `true` | Windows only, since 0.8.0: show the Winamp window's taskbar button; the main window always keeps its button |
| `skin` | none | File or folder name in the skins folder; blank uses the built-in skin |
| `skin_scale` | by display | Screen pixels per skin pixel, 1 to 4 |
| `winamp_on_top` | `false` | Keep the mini player above other windows |
| `vis` | `bars` | The mini player's visualiser: `bars`, `scope`, or `off` |
| `playlist_open` | `false` | The playlist window is open under the mini player |
| `playlist_height` | `174` | The playlist window's height in skin pixels |
| `eq_open` | `false` | The equalizer window is open under the mini player |
| `eq_on` | `false` | The equalizer shapes local playback |
| `eq_preamp_db` | `0` | The preamp, in decibels, -12 to 12 |
| `eq_bands_db` | ten zeros | The bands from 60 Hz to 16 kHz, in decibels, -12 to 12 |
| `balance` | `0` | Left to right, -1 to 1, for local playback |
| `mono` | `false` | Play both channels the same |
| `playlist_shaded` | `false` | The playlist window is rolled up to its title bar |
| `winamp_shaded` | `false` | The main window is rolled up to its title bar |
| `milkdrop_open` | `false` | The MilkDrop window is open |
| `milkdrop_seconds` | `30` | How long each MilkDrop preset plays |
| `milkdrop_fps` | `60` | MilkDrop frame rate; `0` is uncapped |
| `milkdrop_screen_hz` | `0` | Last reported display refresh rate |
| `milkdrop_fullscreen` | `false` | The MilkDrop window fills the screen |
| `milkdrop_size` | `640, 480` | The MilkDrop window's size in points |
| `keep_playing_in_background` | `true` | Close to tray |
| `check_for_updates` | `true` | Ask GitHub once a day for a newer release |
| `web_client_id` | none | Optional personal Spotify app id used alongside shared coverage |
| `personal_app_nudge_at` | none | Legacy daily-reminder timestamp, retained for older releases |
| `personal_app_intro_seen` | `false` | Whether the Premium personal-app introduction was dismissed or followed (available since 0.8.0) |
| `proxy_mode` | `system` | `off`, `system`, `http`, or `socks`. Older files without this field stay on `system` |
| `proxy_host` | none | Host of a manual HTTP or SOCKS5 proxy. Ignored when the mode is `off` or `system` |
| `proxy_port` | none | Port of a manual HTTP or SOCKS5 proxy |
| `proxy_username` | none | Optional proxy login for Web requests; authenticated local playback proxying is not supported |

## Command line

```
spotifast [OPTIONS] [LINK]

  LINK                  A Spotify link to open: spotify:track:…, or an
                        open.spotify.com address
  --device-name <NAME>  Spotify Connect name for this session
  -v, --verbose         More logs from librespot and the API client
```

A link goes to the running Spotifast when there is one, which then opens
the page and brings its window forward; otherwise the app starts on it. The
desktop's handler for `spotify:` links runs exactly this.

Attach `fastpotify.log` from the state directory to bug reports. It contains
the last run's output, including extra lines from `spotifast -v`. After a
crash, attach `panic.log` too.

## Demo mode

Builds made with `cargo build --features demo` accept `--demo`, which loads
sample data for screenshots and interface work. Demo mode never writes
settings.

`--demo-page` opens a page, such as `home`, `playlist:pl1`, or `artist:art0`,
and `--demo-show` adds surfaces on top of it: a comma separated list of
`queue`, `playing-next`, `devices`, `shortcuts`, `premium`, `create`, `duplicate`, `light`,
`focus`, `winamp`, `playlist`, `eq`, `eq-shade`, `compact`, `update`, `personal-app`,
and `collection-loading`. `update` shows a sample update badge for checking its
layout. `personal-app` shows the personal Spotify app introduction.
`collection-loading` keeps known collection metadata and placeholder artwork
visible while replacing the page content, with unfinished controls disabled.

`--demo-shot <PATH>` writes the window to a PNG and exits, which is useful for
making deterministic screenshots for these pages:

```
cargo run --release --features demo -- \
  --demo-shot docs/screenshot.png --demo-page playlist:pl1 --demo-show queue
```

The image uses the current window size. `--demo-size WIDTHxHEIGHT` sets that
size in logical pixels for a shot (for example `760x800` or `1240x800`).
`--demo-shot-delay <MS>` sets how long to wait for cover art before taking it.
Since 0.8.0, demo windows ignore saved window geometry and do not
read or save the normal window's framework state. Existing built-in appearance
settings still apply. `--demo-data <DIRECTORY>` keeps demo caches and logs under
that directory's `cache` and `state` folders, with settings read from `config`.
Custom palettes and their cache are used only with an explicit `--demo-data`
directory; the ordinary demo does not scan your real themes folder.

## Home shelves

Since 0.8.0, you can hide **Made for you** and **Recommended for you**
from Home independently. Quit Spotifast before editing `settings.json`, then
restart it. Add this field to hide both:

```json
"home": {
  "made_for_you": { "visible": false },
  "recommendations": { "visible": false }
}
```

Set either `visible` value to `true` to show that shelf again. Omitted
preferences keep both shelves visible. Other Home sections keep their normal
order and contents. This changes what is displayed; hidden shelves still
refresh in the background.

## Custom themes

Create a `themes` folder beside `settings.json` and put JSON files in it.
Run `spotifast reload-themes` if the app is already open, then select the
filename under **Settings → Appearance → Theme**.
The default is **Follow system**. It uses your desktop’s light/dark appearance,
or the current Omarchy palette on a packaged Omarchy installation. Saved Dark,
Light and custom choices are preserved when updating. The picker starts with
**Follow system**, **Light**, and **Dark**, then a separator. **Omarchy** comes
next when the integration is available, followed by the other local palettes.
Themes change colors and keep the app's existing fonts.
The **Open themes folder** icon button beside the picker creates the folder if
needed and opens it in your file
manager, using the same button style as the Winamp skins folder.
After adding or editing a JSON file, run `spotifast reload-themes` to refresh
the list and the selected palette without restarting playback.
Choosing a built-in theme clears the custom selection.

For example, `themes/gruvbox.json`:

```json
{
  "base": "dark",
  "colors": {
    "window": "#282828",
    "panel": "#1d2021",
    "surface": "#32302f",
    "text": "#ebdbb2",
    "accent": "#b8bb26"
  }
}
```

`base` is `dark` (the default) or `light`. Omitted colors inherit that palette.
Supported colors are `window`, `panel`, `surface`, `surface_hover`,
`surface_active`, `outline`, `text`, `secondary`, `dim`, `accent`,
`accent_hover`, `on_accent`, `danger`, `warning`, `overlay`, and `shadow`.
Values must be `#RRGGBB` or `#RRGGBBAA`.

Files are read in the background at launch and when `spotifast reload-themes`
is called. The command updates the selected palette without interrupting
playback, changing your selection or showing the window. It does not start a
stopped app. Repeated requests are combined while a scan is running, and no
continuous file watcher or polling timer is added. Use regular UTF-8 `.json` files, not symbolic links or
subdirectories. Each file is limited to 64 KiB. Keep at most 128 JSON files and
512 total entries in the themes folder; the saved selection is still checked
when a folder exceeds these limits.

Invalid files are skipped with a warning in the log. Spotifast remembers the
last accepted custom palette in `settings.json`. If the selected file is
removed or becomes invalid, that appearance stays in place, including after
a restart, and the Theme row explains the problem. Other preferences are
preserved. A custom selection without any usable cached colors uses the
built-in choice. Choosing Dark, Light or Follow system clears the custom
selection and its cache. Editing or deleting the optional cache does not
reset unrelated settings.

A custom palette's `base` controls both its inherited colors and the light or
dark styling of standard controls. Album-art tinting remains an independent
setting; turn it off for fixed colors throughout. Palettes apply to the main
window; Winamp skins remain separate. This first format controls colors only.

### Follow an Omarchy theme

Since 0.8.0, native Linux packages include the Omarchy integration.
The first normal launch on an Omarchy desktop installs its template and
theme-change hook in your user configuration, in the background. No copy
commands or desktop restart are needed. A palette for the current theme is
prepared without reapplying your desktop theme. New installations use
**Follow system**, so Omarchy colours apply automatically on the first launch
and track later theme changes. An existing explicit Dark, Light or custom
choice stays selected. Choose **Follow system** or **Omarchy** under
**Settings → Appearance → Theme** to follow Omarchy instead.

Setup never replaces an existing template, hook or palette, and never changes
your selected theme. Other users are set up independently when they launch the
app. Demo mode, portable archives and Cargo builds do not perform automatic
setup. A package uninstall removes the shared integration assets; your user
configuration remains, like the rest of your preferences.

For a manual installation, the repository includes an
[Omarchy template](https://github.com/crmne/spotifast/blob/main/contrib/omarchy/spotifast.json.tpl)
and a [theme-change hook](https://github.com/crmne/spotifast/blob/main/contrib/omarchy/spotifast-theme).
Omarchy resolves its light/dark mode and colors through its
[template system](https://omarchy.org/manual/making-your-own-theme/).
The hook copies the result atomically into `themes/omarchy.json`, then asks a
running Spotifast to reload it. It does not change your desktop theme or your
Spotifast selection itself.

For portable or Cargo installations, install the two files from a checkout:

```sh
mkdir -p ~/.config/omarchy/themed
install -m 644 contrib/omarchy/spotifast.json.tpl ~/.config/omarchy/themed/
omarchy hook install theme-set contrib/omarchy/spotifast-theme
```

Apply a theme through Omarchy's theme picker, then select **Omarchy** in
Spotifast's **Settings → Appearance → Theme** once. Later Omarchy changes update
that palette while music keeps playing. Turn off album-art tinting if every
page should keep the theme's fixed colors.

The hook uses Omarchy's current theme at
`~/.local/state/omarchy/current/theme` and Spotifast's existing
`${XDG_CONFIG_HOME:-~/.config}/fastpotify/themes` directory. The retained
`fastpotify` directory is intentional. A custom profile can set
`SPOTIFAST_THEMES_DIR` in the installed hook; this example uses the native
`spotifast` command, not a Flatpak launcher. Themes without `colors.toml` need
their own `spotifast.json` file. Missing or invalid palettes leave the last
accepted appearance in place.

To stop following Omarchy, choose Dark, Light or another custom theme in Spotifast.
For a manual installation, remove only
`~/.config/omarchy/hooks/theme-set.d/spotifast-theme` and
`~/.config/omarchy/themed/spotifast.json.tpl`. Other hooks remain in place.
Packaged launches recreate missing integration files. To disable the hook
while keeping the package installed, leave that hook file empty instead;
existing user files are preserved. Selecting Dark or Light is sufficient to stop following Omarchy's colors.
