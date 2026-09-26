---
title: The Rename
description: How existing installations, commands, settings and links move to Spotifast.
---

Fastpotify is now **Spotifast**, at [spotifast.rocks](https://spotifast.rocks/).
Version 0.9.1 completes the application and profile rename and is the last
release with old-named compatibility downloads.

## Existing installations

Quit the old app before upgrading. The first normal launch moves settings,
skins, MilkDrop presets, history, caches and window state to Spotifast's
directories. This uses directory renames, preserves permissions, and never
merges with or overwrites an existing destination profile. A failed move
stops startup so the app does not silently start with empty preferences.
Demo mode does not migrate your profile.
The updater's trial launch also keeps the old profile and credential service
until startup has succeeded. Migration happens on the next normal launch,
so a failed update can restore the old app with its settings and sign-ins intact.

Saved sign-ins and proxy passwords move from the old protected-store service
to the new one. Each replacement is read back before the old entry is deleted.
Unlock the system credential store if prompted. A non-secret
`credential-profile` file preserves the account identifier used by a migrated
profile; keep this file with the state directory. Revocation markers move too,
so a previous sign-out remains effective.

The old default Spotify Connect device name becomes Spotifast. Custom names,
volume, themes and local pin ordering are preserved. The local Liked Songs key
is migrated when settings are loaded.

Use `spotifast` for command-line controls. Packages retain `fastpotify` as a
compatibility command for existing scripts, and both commands reach one app.
Linux MPRIS now uses `org.mpris.MediaPlayer2.spotifast`; update custom
`playerctl --player=fastpotify` bindings to `playerctl --player=spotifast`.
The single-instance wire protocol retains its old identity for existing clients.

Linux launchers use `spotifast.desktop`, matching the window and icon.
Re-pin an old launcher shortcut if necessary. To open Spotify links:

```sh
xdg-mime default spotifast.desktop x-scheme-handler/spotify
```

## Package managers

AUR packages are `spotifast`, `spotifast-bin` and `spotifast-git`.
Install the matching replacement and accept removal of the old package:

```sh
yay -S spotifast-bin
```

Do not remove settings first. DEB/RPM packages also declare replacement of
the old package. The executable is now `spotifast`; the old command is an alias.

The Homebrew cask is `crmne/tap/spotifast`. Update the tap and use
`brew migrate --cask fastpotify` if Homebrew has not already migrated it.
Homebrew installations continue to update through Homebrew.

Nix's primary package and app attributes are `spotifast` and `spotifast-app`.
Old attribute names remain aliases for existing configurations.
Community-maintained packages may still use their previous package names.

For a previous Cargo installation, run `cargo install --path . --locked --force`
from the updated checkout. Cargo needs `--force` once because the package
owning the existing commands changed from `fastpotify` to `spotifast`.

## Updates and packaging

New clients request only `spotifast-*` downloads. Version 0.9.1 also publishes
old-named downloads and carries the original archive layout for updaters in
0.8.0 and 0.9.0. Both names are verified by the published checksums. Subsequent
releases publish only Spotifast downloads.

**Update to 0.9.1 before relying on automatic updates to later versions.**
An older client that skips this bridge needs a manual download from the
[download page](/download/). Installing the current version manually still
migrates its profile. Existing published releases and their checksums stay intact.

New macOS installations use `Spotifast.app`. For 0.9.1 only, its internal
executable and bundle ID retain the names older updaters validate, and its
disk image includes their hidden compatibility bundle. The 0.9.1 updater
accepts the Spotifast executable and bundle ID used afterward, while retaining
version, signature and signing-team checks. Updating preserves an existing
installation's chosen bundle location; from 0.10.3, an app still called
`Fastpotify.app` comes back from its next update as `Spotifast.app` in the
same folder.

From 0.10.3, every automatic update also moves a copy that still runs under
the old name onto the new one. A Windows installation that starts as
`fastpotify.exe` comes back as `spotifast.exe`, and the old file is removed.
Updates begun by 0.10.2 or earlier still relaunch `fastpotify.exe`, so the
installer keeps a copy under that name for them, and the app removes it the
next time it starts as `spotifast.exe`. A portable copy named `fastpotify` is
updated as `spotifast` beside it; on Linux the old name stays as a link to
the new file, so scripts that name it keep working. Shortcuts that start the
old file are pointed at the new one: Start menu, Startup, desktop and taskbar
shortcuts on Windows, and desktop entries and command links in your home
folder on Linux. Scripts are left alone.

Windows keeps the installer GUID so an upgrade remains the same installed
application. Fresh installs use `Programs\Spotifast`; upgrades preserve the
installation directory recorded by the existing installer. The app, shortcuts,
registered link handler and primary executable use Spotifast.

## Flatpak

The application ID is **`rocks.spotifast.Spotifast`**. Flatpak treats the old ID
as a separate application. Before first launching the new one, quit the old
one and copy its profile if wanted:

```sh
old_data="$HOME/.var/app/rocks.fastpotify.Fastpotify"
new_data="$HOME/.var/app/rocks.spotifast.Spotifast"
test -d "$old_data" && test ! -e "$new_data" && cp -a "$old_data" "$new_data"
```

This preserves the original and refuses to overwrite an existing new profile.
If you already launched the new app, keep that profile or move it aside before
copying. Sign in again after switching Flatpak IDs: protected credentials are
scoped to the original application data directory.

Install the bundle from the [download page](/download/), then run
`flatpak run rocks.spotifast.Spotifast`. After checking the new installation,
remove the old application with
`flatpak uninstall --user rocks.fastpotify.Fastpotify`.
Use `--system` for a system-wide installation. Omitting `--delete-data`
keeps its data for recovery.

## Website links

The guides live at `/using-spotifast/` and `/what-is-spotifast/`.
Redirects preserve their old URLs. The download page switches to a new stable
version only after its files have been published.
