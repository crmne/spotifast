---
title: Download
description: Download the app for macOS, Windows, or Linux, with install instructions for each.
nav_order: 1
---

{% assign v = site.spotifast_version %}
{% assign base = "https://github.com/crmne/spotifast/releases/download/v" | append: v %}
{% comment %} Releases after 0.12.0 ship a Windows Installer package (.msi). {% endcomment %}
{% assign version = v | split: "." %}
{% assign major = version[0] | plus: 0 %}
{% assign minor = version[1] | plus: 0 %}
{% assign patch = version[2] | plus: 0 %}
{% assign windows_installer = "-setup.exe" %}
{% if major > 0 or minor > 12 or minor == 12 and patch > 0 %}{% assign windows_installer = ".msi" %}{% endif %}

The current version is **v{{ v }}**. You can use the
[download checksums]({{ base }}/checksums.txt) to check that a file has not
been damaged or changed. Older versions are on the
[releases page](https://github.com/crmne/spotifast/releases).

## macOS

One download for both Apple Silicon and Intel:

- [Download for Mac]({{ base }}/spotifast-v{{ v }}-macos-universal.dmg)

Open the downloaded file and drag **Spotifast** to **Applications**.

If you use [Homebrew](https://brew.sh), you can install it with:

```sh
brew install --cask crmne/tap/spotifast
```

### First open on macOS

Open **Spotifast** from Applications. If your Mac asks whether you want to
open an app downloaded from the internet, choose **Open**.

Starting with 0.8.0, the Mac download passes Apple's security checks. You do
not need to change security settings or run commands in Terminal to open it.

Quit the old app before opening Spotifast. Your saved settings and sign-ins
carry over. If an older version reports that its update helper exited before
it was ready, install the DMG above manually once. The helper changes take
effect for updates started from 0.9.1 onward.

## Windows

Download and run the installer. It adds Spotifast to the Start menu and does
not need an administrator password. Choose the first download for most PCs,
or the ARM version if your PC uses an ARM processor:

- [Windows installer (most PCs)]({{ base }}/spotifast-v{{ v }}-x86_64-pc-windows-msvc{{ windows_installer }})
- [Windows installer (ARM PCs)]({{ base }}/spotifast-v{{ v }}-aarch64-pc-windows-msvc{{ windows_installer }})
{% if windows_installer == ".msi" %}
The installer is a Windows Installer package (`.msi`). Spotifast installed
with the earlier setup program (`-setup.exe`) moves to it when you run the new
installer, and keeps its settings, its folder and its Start menu entry.
Windows then lists one Spotifast under installed apps. Versions 0.13.0 and
later also move to it through their own updates; 0.12.x and earlier no longer
find updates, so run the installer once by hand.
{% endif %}
To run Spotifast without an installer, download a ZIP file, extract it, and
open `spotifast.exe`. Since 0.13.4, it keeps its settings, state and caches
in a `spotifast-data` folder next to it, so the folder carries everything
(see [portable copies](/settings-and-files/#portable-copies)).

- [Windows ZIP (most PCs)]({{ base }}/spotifast-v{{ v }}-x86_64-pc-windows-msvc.zip)
- [Windows ZIP (ARM PCs)]({{ base }}/spotifast-v{{ v }}-aarch64-pc-windows-msvc.zip)

Either way, SmartScreen may warn about an unknown publisher on first run;
choose **More info**, then **Run anyway**. The Windows files are not signed
with a publisher certificate, so Windows has no publisher name to show.

Spotifast needs OpenGL 2.0 or newer, which every current Windows graphics
driver provides. In a virtual machine or remote session that only has the
Microsoft Basic Display Adapter, it cannot open its window and says so in a
message that links to this page; install the machine's graphics driver, or
enable GPU acceleration for the virtual machine.

To choose which app opens Spotify links, use **Settings → Apps → Default apps**
in Windows.

Uninstalling while Spotifast runs removes nothing until it is closed. The
installer offers to close it for you; closing it that way ends it at once.

### Check a Windows download

To check a file against the [published checksums]({{ base }}/checksums.txt),
print its SHA-256 hash in PowerShell and compare it with that file's line,
ignoring letter case:

```powershell
Get-FileHash "$HOME\Downloads\spotifast-v{{ v }}-*-pc-windows-msvc*"
```

If Windows Security flags a download, open **Windows Security → Virus &
threat protection → Protection history** and note the threat name. Include it,
and the file's hash, when you
[report the problem](https://github.com/crmne/spotifast/issues/new?template=bug.yml).
To ask Microsoft to review the file, send it through
[Submit a file for malware analysis](https://www.microsoft.com/en-us/wdsi/filesubmission)
as incorrectly detected.

## Linux

### Arch Linux

Spotifast is in the AUR, Arch's community package collection. Installing it
also adds it to your app launcher:

```sh
yay -S spotifast-bin      # the released build, ready made
yay -S spotifast          # the release, built from source
yay -S spotifast-git      # built from the latest commit
```

### Flatpak

Download the [Spotifast Flatpak]({{ base }}/spotifast-v{{ v }}-x86_64.flatpak?flatpak-id=rocks.spotifast.Spotifast).
It requires Flatpak and the Freedesktop 25.08 runtime, a set of shared
components used by Flatpak apps:

```sh
flatpak install --user ~/Downloads/spotifast-v{{ v }}-x86_64.flatpak
flatpak run rocks.spotifast.Spotifast
```

The application ID is **`rocks.spotifast.Spotifast`**.

To update this Flatpak installation, download and install the new release.
Flathub support is planned.

Packages from other stores are maintained by their publishers. Report
problems specific to those packages to their maintainers.

### AppImage

- [AppImage (Intel and AMD, 64-bit)]({{ base }}/spotifast-{{ v }}-x86_64.AppImage)
- [AppImage (ARM, 64-bit)]({{ base }}/spotifast-{{ v }}-aarch64.AppImage)

One file, no installation: make it executable and run it.

```sh
chmod +x ~/Downloads/spotifast-{{ v }}-x86_64.AppImage
~/Downloads/spotifast-{{ v }}-x86_64.AppImage
```

The AppImage bundles no libraries: like the DEB and RPM, it needs glibc 2.39
or newer and your desktop's own libraries. Running it needs FUSE; without
FUSE, start it with `--appimage-extract-and-run`.

From the release after 0.13.3, the AppImage updates itself like the portable
downloads, as long as it sits in a folder of your own, such as
`~/Applications`: Spotifast replaces the file in place and restarts it. It
also carries update information, so AppImageUpdate, AppImageLauncher, AM and
Gear Lever can update it too, downloading only what changed. Copies of 0.13.3
and earlier need one more manual download.

### Other distributions

- [Linux archive (Intel and AMD, 64-bit)]({{ base }}/spotifast-v{{ v }}-x86_64-unknown-linux-gnu.tar.gz)
- [Linux archive (ARM, 64-bit)]({{ base }}/spotifast-v{{ v }}-aarch64-unknown-linux-gnu.tar.gz)

Unpack, put `spotifast` on your PATH, and copy the desktop entry and icon
from the bundled `packaging/` directory if you want it in your launcher and
handling `spotify:` links.
The binary needs the ALSA and PipeWire libraries, and Wayland or X11. It plays
through PipeWire, a PulseAudio server, or ALSA, whichever is running.

Or build from source: see the [build instructions](/getting-started/#build-from-source).

## Nix

Add the repository [flake](https://github.com/crmne/spotifast) to your
inputs:

```nix
inputs.spotifast.url = "github:crmne/spotifast";
```

On NixOS, install the default package:

```nix
environment.systemPackages = [
  inputs.spotifast.packages."${pkgs.stdenv.hostPlatform.system}".default
];
```

### nix-darwin

On macOS, use the same `spotifast` package to install Spotifast as a Mac app:

```nix
environment.systemPackages = [
  inputs.spotifast.packages."${pkgs.stdenv.hostPlatform.system}".spotifast
];
environment.pathsToLink = [ "/Applications" ];
```

The bundle appears in `/Applications/Nix Apps`. With Home Manager,
`home.packages` is enough; its darwin support links app bundles into
`~/Applications`:

```nix
home.packages = [
  inputs.spotifast.packages."${pkgs.stdenv.hostPlatform.system}".spotifast
];
```
