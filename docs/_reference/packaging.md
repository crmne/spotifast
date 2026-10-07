---
title: Release packaging
description: Shared packaging automation and application-specific release definitions.
nav_order: 20
---

Spotifast keeps release asset definitions, nFPM configuration and native AUR and
Homebrew templates in `native-packages.yaml` and `packaging/`. Common automation comes from the pinned
[native-packages](https://github.com/crmne/native-packages) gem, installed with `gem install native-packages --version 0.6.0`.

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
and Windows installer configuration. nFPM does not replace these platform tools.
The Windows uninstaller asks you to quit Spotifast before removing files if its
executable is in use. Silent uninstall exits without removing files in that case.
See the repository's [maintainer packaging guide](https://github.com/crmne/spotifast/blob/main/PACKAGING.md)
for commands and the shared tool's [platform coverage](https://github.com/crmne/native-packages/blob/main/docs/platforms.md)
for the boundaries.

Linux and macOS share `native-packages.yaml`. The Linux workflow selects its two
architecture targets after release assets exist. The native Mac job selects
`macos-universal` with `--defer-recipes`, so it can package the app before the
Homebrew download exists, without requiring Linux inputs or AUR tooling.
Complete Apple CI secrets
enable shared signing, notarization and ticket validation automatically before
checksums are written. Earlier downloads retain their original signing status.
