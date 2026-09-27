# Packaging

[`native-packages.yaml`](native-packages.yaml) is the packaging configuration:
it pins the shared CLI and nFPM versions and declares Linux amd64/arm64 inputs,
DEB/RPM contents, dependencies, recipe templates and downstream repositories.
Application assets and native recipes stay in `packaging/`.

Public AUR, Homebrew, DEB/RPM and release download names use Spotifast.
AUR and native Linux packages declare the old package replacements, and the
Homebrew tap contains a `fastpotify` to `spotifast` cask rename mapping. Linux packages install `spotifast` as the executable and keep `fastpotify` as a compatibility alias. See [rename compatibility](docs/_reference/renaming.md)
for the macOS bundle and updater requirements.

On main, after 0.8.0, Linux builds provide `spotifast.desktop` and
`spotifast.svg`, matching the native window and MPRIS desktop-entry ID.
Flatpak installs those assets under its full application ID and sets the
window class to match. Version 0.9.1 migrates main-window state to the new profile.
Historical release payloads keep their original launcher/icon names and
window class: the package-only 0.8.0 rename did not rebuild those executables.
AUR recipes and nFPM take the identity from the input payload; the git recipe
requires the current Spotifast assets. Existing release files are not rewritten.
`python3 packaging/test-launchers.py` exercises both input generations through
the actual AUR and Flatpak installation commands, using Ruby to read YAML.

Linux packages also install the optional Omarchy template and hook under
`share/spotifast/omarchy` in their installation prefix. The normal application
launch registers missing per-user files and prepares the current palette on an
Omarchy desktop, without changing existing files or the selected theme. Setup
runs in the background; package-manager scripts do not write into user homes.
DEB/RPM packages take these small assets from the checked-out configuration,
so package validation can still use the pinned older binary fixture. AUR and
portable Linux release archives carry the assets with their source/binary
payloads. Source and binary AUR recipes also accept older releases that predate
the integration; the git recipe requires the current files. Nix installs the
Linux desktop assets beside its binaries and packages the signed macOS
`Spotifast.app` bundle in the same derivation. Flatpak does not install host
desktop hooks.

```sh
gem install native-packages --version 0.6.0
native-packages validate
native-packages doctor --target linux-amd64 --target linux-arm64
native-packages build --release v1.2.3 --target linux-amd64 --target linux-arm64
```

Replace `v1.2.3` with an existing stable application release. Local use also
requires nFPM 2.47.0, `bsdtar` and `readelf`; AUR generation needs `makepkg`
or Docker. CI installs its tooling. To package local release archives, put
the selected target inputs and recipe assets under `dist/`, then run
`native-packages build --version 1.2.3 --target linux-amd64 --target linux-arm64`.
Outputs go to `dist/packages/1.2.3`; use `--output` for a fresh destination when rebuilding.

Before tagging, commit written notes at `packaging/release-notes/vVERSION.md`.
CI requires notes for the current Cargo version, and the release workflow
publishes that file immediately instead of a generated PR-only summary.
Verify the notes and their download links after the artifacts are published.

Stable tags run the existing native build jobs first. After binaries and
`checksums.txt` are published, the shared workflow verifies their hashes,
builds the configured packages, and attaches them to the GitHub release.
Configured recipes are attached as an archive. Package checksums are separate
from the original binary checksums. PR validation never publishes.

The Linux release runners are pinned to Ubuntu 24.04 (glibc 2.39). The native
packages target Ubuntu 24.04 / Debian 13 and newer, and Fedora 41 and newer.
Runtime-loaded GUI libraries must be declared explicitly in the YAML; ELF
inspection only discovers linked dependencies. The ALSA library mapping also
uses the Debian/Ubuntu `libasound2t64` name for this baseline.

Packaging CI builds both architectures using a pinned published release
(`v0.9.0`) for pushes and PRs, or the requested version for manual and release
runs. It then installs and removes each package in clean Ubuntu 24.04, Debian
13, Fedora 41 and current Fedora containers on native amd64 and arm64 runners.
Each case first installs the original Fastpotify 0.8.0 package, replaces it
with Spotifast, and verifies that the old package is gone and a settings
fixture survives installation and removal. The checks run both `spotifast --version` and `fastpotify --version`, load the GUI libraries with `dlopen`,
and verify the desktop entry and icon. They cover installation and library
resolution, not a running desktop or Spotify playback. On release runs these
checks follow artifact attachment; a failure marks the workflow as failed.
The generated Homebrew cask is also installed, launched with `--version`,
signature-checked, and uninstalled on a native macOS runner. Wait for this
check before publishing the staged cask to the tap.
The install script defaults to the new `spotifast` desktop ID; its fourth
argument is explicitly `fastpotify` only when checking the historical 0.8.0
fixture. Both cases reject a second launcher and check removal of both names.

To repeat a check locally on the matching architecture, with Docker and a C
compiler available:

```sh
bash packaging/test-install.sh ubuntu:24.04 dist/packages/1.2.3
bash packaging/test-install.sh fedora:latest dist/packages/1.2.3
```

Review or publish an existing build with the same installed CLI:

```sh
native-packages publish --from dist/packages/1.2.3 --to github --target linux-amd64 --target linux-arm64
native-packages repositories
native-packages status --offline
```

For applications with configured AUR or Homebrew destinations, stage the
recipes with `native-packages stage TARGET dist/packages/1.2.3/recipes`,
inspect `native-packages diff TARGET`, run native package validation, and
publish with `native-packages publish TARGET`. These destinations use ignored
managed Git clones, recorded in this application's YAML configuration.
AUR automation needs `PUBLISH_AUR=true`, `AUR_SSH_KEY` and `AUR_KNOWN_HOSTS`;
Homebrew automation needs `PUBLISH_HOMEBREW=true` and
`HOMEBREW_TAP_GITHUB_TOKEN`. Enable only configured destinations.

The macOS target, Windows and Flatpak build steps remain responsible
for their native artifacts. Additional nFPM formats require suitable platform
inputs and dependencies; adding a format does not port the application.
See the [shared CLI documentation](https://github.com/crmne/native-packages/tree/v0.6.0)
for commands and supported formats.

The manual **Flatpak from release** workflow can rebuild a missing Flatpak from
an existing tag. It verifies the published Linux archive against `checksums.txt`
and uses that tag's version and description with the current Flatpak app ID,
`rocks.spotifast.Spotifast`, without compiling or replacing binaries.
It uploads a workflow artifact only. After checking the bundle, attach it to the
existing release and add its hash to `checksums.txt`, preserving every existing
asset and checksum. Release tags stay immutable.

Check AUR source-directory compatibility against a downloaded source archive
with `bash packaging/test-arch-source.sh SOURCE_ARCHIVE VERSION`. It runs the
recipe's prepare/build/check directory handling with Cargo calls stubbed out;
it does not replace compilation or native package installation checks.

To upgrade the tool, change `tool.version` in `native-packages.yaml`, the matching
immutable workflow reference, and any release-job gem installation pin together.
Applications need no packaging Gemfile, lockfile or Ruby wrapper.

## Automatic macOS notarization

The macOS release job builds the app first, then selects
`macos-universal` from `native-packages.yaml` and uses `packaging/macos/dmg.rb`
to package it. Native Mac builds pass `--defer-recipes`, so they do not require
Linux inputs, AUR tooling or the not-yet-published DMG for Homebrew. The Linux
packaging job selects `linux-amd64,linux-arm64` from the same configuration
after the release assets exist and generates downstream recipes then.
The shared gem signs its owned input copy, notarizes the DMG, staples and validates
Apple's ticket, and only then records final checksums. Configure these repository
secrets, which the job exposes as environment variables:

- `APPLE_CERTIFICATE_P12`: base64 PKCS#12 Developer ID Application certificate and private key.
- `APPLE_CERTIFICATE_PASSWORD`: the export password.
- `APPLE_SIGNING_IDENTITY`: exact `Developer ID Application: Name (TEAMID)` identity.
- `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`: Apple email, Team ID and app-specific password.

A complete set enables notarization automatically. An incomplete set fails;
no values retain local builds without Developer ID signing. Application inputs
and the user's normal keychains remain unchanged. See the shared
[Apple setup and phase contract](https://github.com/crmne/native-packages/blob/v0.6.0/docs/apple-notarization.md).

After preparing `dist/macos-input` on a Mac, test packaging without publishing:

```sh
native-packages build \
  --version 1.2.3 --target macos-universal --defer-recipes --output dist/macos-packages-test
```

Secret configuration applies to future builds. Existing published DMGs retain
their original signatures; this setup does not replace release assets.

## Windows MSIX and universal bundle

In addition to the standard Inno Setup installer (`spotifast.iss`), Windows
packages can be built as a modern MSIX package or a unified multi-architecture
bundle (`.msixbundle`) through `packaging/windows/build-msix.ps1`.

Visual assets (including high-DPI tiles and unplated transparent taskbar icons)
are generated dynamically from `packaging/macos/icon-1024.png` during the build
through `packaging/windows/msix/generate-assets.ps1`, so no redundant image files
are kept in the repository.

By default, running `build-msix.ps1` resolves both `x86_64` and `aarch64` binaries,
packages both `.msix` files, and creates the unified `.msixbundle` installer.
The package version is resolved automatically from `Cargo.toml` or `GITHUB_REF_NAME`
(pass `-Version <version>` to override):

```powershell
pwsh packaging\windows\build-msix.ps1
```

To package a single architecture into `.msix` (for example, for target-specific CI jobs):

```powershell
pwsh packaging\windows\build-msix.ps1 -Arch aarch64 -Binary dist\spotifast.exe
```

To create the universal `.msixbundle` from explicit binary paths:

```powershell
pwsh packaging\windows\build-msix.ps1 `
    -X64Binary target\x86_64-pc-windows-msvc\release\spotifast.exe `
    -Arm64Binary target\aarch64-pc-windows-msvc\release\spotifast.exe
```

### Code signing options for MSIX

Double-click installation of `.msix` or `.msixbundle` packages outside Developer
Mode requires a valid signature. According to [Microsoft's code signing guidance](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options),
open source developers have multiple options:

1. **Microsoft Store (Free, recommended):** Submitting the MSIX package to the
   Microsoft Store through Partner Center is free. Microsoft automatically
   re-signs the package with trusted Microsoft roots upon ingestion, providing
   instant SmartScreen trust and automatic background updates without requiring
   any certificate purchase or renewal.
2. **SignPath Foundation (Free for open source):** For distributing packages
   outside the Store, SignPath Foundation provides free code signing certificates
   and automated signing pipelines for qualifying open-source projects.
3. **Microsoft Azure Artifact Signing (formerly Trusted Signing):** Microsoft
   also offers cloud-based signing (~$9.99/month) for non-Store distribution
   without hardware tokens. Pass Trusted Signing credentials to `build-msix.ps1`:

   ```powershell
   pwsh packaging\windows\build-msix.ps1 `
       -TrustedSigningEndpoint "https://eus.codesigning.azure.net" `
       -TrustedSigningAccount "Spotifast" `
       -TrustedSigningProfile "SpotifastOpenSource"
   ```

4. **Traditional certificate:** Standard PFX files can be passed via
   `-CertificatePath` and `-CertificatePassword`.

For local development and testing, an unsigned package can be registered directly
in Windows Developer Mode without any certificates or administrator rights:

```powershell
pwsh packaging\windows\build-msix.ps1 -Arch aarch64 -Binary dist\spotifast.exe -Register
```

`packaging/release-names.py DIST TAG` prepares the public `spotifast-` names
and checksums. Through 0.9.1 it also creates byte-identical compatibility
downloads; later versions reject old-named downloads. The 0.9.1 portable
archives contain both directory layouts for older updaters. Later archives
contain only the Spotifast layout and executable. The macOS bundle retains
the old executable and bundle ID for 0.9.1 only; that release's updater accepts
the new bundle identity used afterward. Run `python3 packaging/test-release-names.py`
when changing this step. Published historical downloads are never rewritten.

Native Linux package builds accept archives from 0.9.0 onward, which contain
the Spotifast executable. The directory-name wildcard accommodates the old
0.9.0 layout and the new layout. The 0.9.1 compatibility directory contains
only its executable, so it cannot duplicate packaged desktop files or licenses.
AUR and Flatpak also accept older fixtures; `packaging/test-launchers.py`
tests their installation commands.
