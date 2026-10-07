#!/usr/bin/env bash
# Usage: bash packaging/test-install.sh ubuntu:24.04 dist/native-packages
# Run on the matching architecture, with a C compiler and Docker available.
set -euo pipefail

image=${1:?Supply a Debian, Ubuntu or Fedora container image}
packages=$(realpath "${2:?Supply a native-packages output directory}")
case "$(uname -m)" in
  x86_64) target=linux-amd64 ;;
  aarch64) target=linux-arm64 ;;
  *) echo 'Unsupported test architecture' >&2; exit 1 ;;
esac
case "$image" in
  ubuntu:*|debian:*) format=deb ;;
  fedora:*) format=rpm ;;
  *) echo 'Unsupported test distribution' >&2; exit 1 ;;
esac
package_dir="$packages/packages/$target/$format"
test -d "$package_dir"
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
checks=$(mktemp -d)
trap 'rm -rf -- "$checks"' EXIT
cc -std=c99 -Wall -Wextra -Werror "$script_dir/check-runtime-libs.c" -ldl -o "$checks/check-runtime-libs"

docker run --rm \
  --volume "$package_dir:/packages:ro" \
  --volume "$checks:/checks:ro" \
  --env "FORMAT=$format" \
  "$image" sh -ec '
    set -- /packages/*."$FORMAT"
    test "$#" -eq 1
    test -f "$1"
    mkdir -p /root/.config/spotizgeg
    printf "%s\n" "preserve-existing-settings" > /root/.config/spotizgeg/settings-fixture
    if [ "$FORMAT" = deb ]; then
      apt-get update
      DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$1"
      dpkg-query -W spotizgeg
    else
      dnf install -y --setopt=install_weak_deps=False "$1"
      rpm -q spotizgeg
    fi
    # --version exercises linked libraries; the probe checks dlopen libraries
    # without installing a desktop, compiler, interpreter or test dependencies.
    # Trace the isolated fixture assertions so a failed check identifies itself.
    set -x
    spotizgeg --version
    test -f /usr/bin/spotizgeg
    test ! -L /usr/bin/spotizgeg
    test -f /usr/share/licenses/spotizgeg/LICENSE
    if [ "$FORMAT" = deb ]; then
      # Slim Debian/Ubuntu images exclude /usr/share/doc at installation time.
      # Verify the regular file in the package, not the intentionally stripped root.
      dpkg-deb --contents "$1" | grep -E "^-.* ./usr/share/doc/spotizgeg/README.md$"
    else
      test -f /usr/share/doc/spotizgeg/README.md
    fi
    /checks/check-runtime-libs
    test -s /usr/share/applications/spotizgeg.desktop
    test -s /usr/share/icons/hicolor/scalable/apps/spotizgeg.svg
    grep -qx "Icon=spotizgeg" /usr/share/applications/spotizgeg.desktop
    grep -qx "StartupWMClass=spotizgeg" /usr/share/applications/spotizgeg.desktop
    test -s /usr/share/spotizgeg/omarchy/spotizgeg.json.tpl
    test -x /usr/share/spotizgeg/omarchy/spotizgeg-theme
    test "$(cat /root/.config/spotizgeg/settings-fixture)" = preserve-existing-settings
    if [ "$FORMAT" = deb ]; then
      apt-get remove -y spotizgeg
    else
      dnf remove -y spotizgeg
    fi
    test ! -e /usr/bin/spotizgeg
    test ! -e /usr/share/applications/spotizgeg.desktop
    test ! -e /usr/share/icons/hicolor/scalable/apps/spotizgeg.svg
    test ! -e /usr/share/spotizgeg/omarchy/spotizgeg.json.tpl
    test ! -e /usr/share/spotizgeg/omarchy/spotizgeg-theme
    test "$(cat /root/.config/spotizgeg/settings-fixture)" = preserve-existing-settings
  '
