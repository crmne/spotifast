#!/usr/bin/env bash
# Writes the manifest Flathub builds from, for one release tag:
#
#   packaging/flatpak/flathub.sh v0.4.0 /path/to/flathub-checkout
#
# It pins the source to the tag and its commit, and generates
# cargo-sources.json from the lockfile with flatpak-cargo-generator, which
# needs python3 with aiohttp and toml (pip install aiohttp toml).
set -euo pipefail
tag="${1:?tag, like v0.4.0}"
out="${2:?directory to write into}"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
commit="$(git -C "$root" rev-list -n 1 "$tag")"
generator="$here/flatpak-cargo-generator.py"
if [ ! -f "$generator" ]; then
  curl -fsSL -o "$generator" \
    https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
fi
mkdir -p "$out"
python3 "$generator" "$root/Cargo.lock" -o "$out/cargo-sources.json"
python3 - "$here/rocks.spotizgeg.Spotizgeg.yml" "$out/rocks.spotizgeg.Spotizgeg.yml" "$tag" "$commit" <<'PY'
import sys
src, dst, tag, commit = sys.argv[1:]
text = open(src).read()
old = "      - type: dir\n        path: ../..\n"
new = f"      - type: git\n        url: https://github.com/crmne/spotifast.git\n        tag: {tag}\n        commit: {commit}\n"
assert old in text, "the source block moved"
open(dst, "w").write(text.replace(old, new))
PY
python3 "$here/prepare-metainfo.py" "$tag" "$out/rocks.spotizgeg.Spotizgeg.metainfo.xml"
echo "wrote $out/rocks.spotizgeg.Spotizgeg.yml and cargo-sources.json for $tag ($commit)"
