---
title: Download
description: Download the app for macOS, Windows, or Linux, with install instructions for each.
nav_order: 1
---

{% assign v = site.spotizgeg_version %}
{% assign base = "https://github.com/crmne/spotifast/releases/download/v" | append: v %}

The current version is **v{{ v }}**. You can use the
[download checksums]({{ base }}/checksums.txt) to check that a file has not
been damaged or changed. Older versions are on the
[releases page](https://github.com/crmne/spotifast/releases).

## macOS

One download for both Apple Silicon and Intel:

- [Download for Mac]({{ base }}/spotizgeg-v{{ v }}-macos-universal.dmg)

Open the downloaded file and drag **Spotizgeg** to **Applications**.

If you use [Homebrew](https://brew.sh), you can install it with:

```sh
brew install --cask crmne/tap/spotizgeg
```

### First open on macOS

Open **Spotizgeg** from Applications. If your Mac asks whether you want to
open an app downloaded from the internet, choose **Open**.

Starting with 0.8.0, the Mac download passes Apple's security checks. You do
not need to change security settings or run commands in Terminal to open it.

Quit the old app before opening Spotizgeg. Your saved settings and sign-ins
carry over. If an older version reports that its update helper exited before
it was ready, install the DMG above manually once. The helper changes take
effect for updates started from 0.9.1 onward.

## Windows

Download and run the installer. It adds Spotizgeg to the Start menu and does
not need an administrator password. Choose the first download for most PCs,
or the ARM version if your PC uses an ARM processor:

- [Windows installer (most PCs)]({{ base }}/spotizgeg-v{{ v }}-x86_64-pc-windows-msvc-setup.exe)
- [Windows installer (ARM PCs)]({{ base }}/spotizgeg-v{{ v }}-aarch64-pc-windows-msvc-setup.exe)

To run Spotizgeg without an installer, download a ZIP file, extract it, and
open `spotizgeg.exe`.

- [Windows ZIP (most PCs)]({{ base }}/spotizgeg-v{{ v }}-x86_64-pc-windows-msvc.zip)
- [Windows ZIP (ARM PCs)]({{ base }}/spotizgeg-v{{ v }}-aarch64-pc-windows-msvc.zip)

Either way, SmartScreen may warn about an unknown publisher on first run;
choose **More info**, then **Run anyway**.

Spotizgeg needs OpenGL 2.0 or newer, which every current Windows graphics
driver provides. In a virtual machine or remote session that only has the
Microsoft Basic Display Adapter, it may not start; install the machine's
graphics driver, or enable GPU acceleration for the virtual machine.

To choose which app opens Spotify links, use **Settings → Apps → Default apps**
in Windows.

## Linux

### Arch Linux

Spotizgeg is in the AUR, Arch's community package collection. Installing it
also adds it to your app launcher:

```sh
yay -S spotizgeg-bin      # the released build, ready made
yay -S spotizgeg          # the release, built from source
yay -S spotizgeg-git      # built from the latest commit
```

### Flatpak

Download the [Spotizgeg Flatpak]({{ base }}/spotizgeg-v{{ v }}-x86_64.flatpak?flatpak-id=rocks.spotizgeg.Spotizgeg).
It requires Flatpak and the Freedesktop 24.08 runtime, a set of shared
components used by Flatpak apps:

```sh
flatpak install --user ~/Downloads/spotizgeg-v{{ v }}-x86_64.flatpak
flatpak run rocks.spotizgeg.Spotizgeg
```

The application ID is **`rocks.spotizgeg.Spotizgeg`**.

To update this Flatpak installation, download and install the new release.
Flathub support is planned.

Packages from other stores are maintained by their publishers. Report
problems specific to those packages to their maintainers.

### AppImage

- [AppImage (Intel and AMD, 64-bit)]({{ base }}/spotizgeg-{{ v }}-x86_64.AppImage)
- [AppImage (ARM, 64-bit)]({{ base }}/spotizgeg-{{ v }}-aarch64.AppImage)

One file, no installation: make it executable and run it.

```sh
chmod +x ~/Downloads/spotizgeg-{{ v }}-x86_64.AppImage
~/Downloads/spotizgeg-{{ v }}-x86_64.AppImage
```

The AppImage bundles no libraries: like the DEB and RPM, it needs glibc 2.39
or newer and your desktop's own libraries. Running it needs FUSE; without
FUSE, start it with `--appimage-extract-and-run`. It does not update itself:
download the new file when Spotizgeg says a release is out.

### Other distributions

- [Linux archive (Intel and AMD, 64-bit)]({{ base }}/spotizgeg-v{{ v }}-x86_64-unknown-linux-gnu.tar.gz)
- [Linux archive (ARM, 64-bit)]({{ base }}/spotizgeg-v{{ v }}-aarch64-unknown-linux-gnu.tar.gz)

Unpack, put `spotizgeg` on your PATH, and copy the desktop entry and icon
from the bundled `packaging/` directory if you want it in your launcher and
handling `spotify:` links.
The binary needs ALSA, PulseAudio or PipeWire, and Wayland or X11.

Or build from source: see the [build instructions](/getting-started/#build-from-source).

## Nix

Add the repository [flake](https://github.com/crmne/spotifast) to your
inputs:

```nix
inputs.spotizgeg.url = "github:crmne/spotifast";
```

On NixOS, install the default package:

```nix
environment.systemPackages = [
  inputs.spotizgeg.packages."${pkgs.stdenv.hostPlatform.system}".default
];
```

### nix-darwin

On macOS, use the same `spotizgeg` package to install Spotizgeg as a Mac app:

```nix
environment.systemPackages = [
  inputs.spotizgeg.packages."${pkgs.stdenv.hostPlatform.system}".spotizgeg
];
environment.pathsToLink = [ "/Applications" ];
```

The bundle appears in `/Applications/Nix Apps`. With Home Manager,
`home.packages` is enough; its darwin support links app bundles into
`~/Applications`:

```nix
home.packages = [
  inputs.spotizgeg.packages."${pkgs.stdenv.hostPlatform.system}".spotizgeg
];
```
