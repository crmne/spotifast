---
title: Release packaging
description: Shared packaging automation and application-specific release definitions.
nav_order: 20
---

Spotifast keeps release asset definitions, nFPM configuration and native AUR and
Homebrew templates in `native-packages.yaml` and `packaging/`. Common automation comes from the pinned
[native-packages](https://github.com/crmne/native-packages) gem, installed with `gem install native-packages --version 0.9.0`.

Stable releases build the existing Linux, macOS and Windows artifacts first.
The shared packaging workflow then verifies their published checksums and attaches
Linux DEB/RPM packages and a recipe archive. AUR and Homebrew publication require
their configured repository variables and secrets. PRs build packages from a
pinned published release without publishing them. Installation checks cover
Ubuntu 24.04, Debian 13, Fedora 41 and current Fedora on amd64 and arm64, including
the GUI libraries loaded at runtime. They do not exercise desktop rendering or
Spotify playback. Release checks run after the packages are attached.

On main, after 0.8.0, Linux launcher and icon filenames use Spotifast, as does
the application's window identity. Historical release fixtures retain their
original matching filenames and window class because their binaries are
unchanged. Packaging regressions cover current and historical inputs;
installation checks require the exact identity expected for that version.

The application retains its Flatpak manifests, macOS bundle/signing configuration
and Windows installer sources. nFPM does not replace these platform tools.

Releases after 0.12.0 install on Windows through a per-user Windows Installer
package (`.msi`, built with the WiX Toolset from `packaging/windows/spotifast.wxs`)
that needs no administrator password. The first of them also ships the earlier
Inno Setup program (`-setup.exe`), which older versions update through. The
package takes over a copy the setup program installed: the same folder, links
and Start menu entry, with one entry under installed apps and settings kept.

If the installed `spotifast.exe` is running, uninstalling removes nothing until
it is closed. The package asks whether to close it (Yes), check again once you
have quit it (No) or cancel; the setup program offers Retry, Close Spotifast or
Cancel. Closing ends the installed copy immediately, so unsaved changes may be
lost. A silent uninstall stops without removing files, while a silent update
closes the installed copy first. A portable copy elsewhere is never closed and
never blocks either.
Uninstalling the package also removes the folders the updater stages updates
in (`.spotifast-update-*` inside the installation folder); an update keeps
them, since the updater runs the package from one.
See the repository's [maintainer packaging guide](https://github.com/crmne/spotifast/blob/main/PACKAGING.md)
for commands and the shared tool's [platform coverage](https://github.com/crmne/native-packages/blob/main/docs/platforms.md)
for the boundaries.

Linux, macOS and Windows share `native-packages.yaml`. The Linux workflow selects its two
architecture targets after release assets exist. The native Mac job selects
`macos-universal` with `--defer-recipes`, so it can package the app before the
Homebrew download exists, without requiring Linux inputs or AUR tooling. Each
Windows release job selects its `windows-*` target the same way to build the
Windows Installer package from the binary it built.
Complete Apple CI secrets
enable shared signing, notarization and ticket validation automatically before
checksums are written. Earlier downloads retain their original signing status.
