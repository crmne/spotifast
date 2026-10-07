#!/usr/bin/env bash
# Check source-recipe directory handling without recompiling the release.
set -euo pipefail

archive=$(realpath "${1:?usage: test-arch-source.sh SOURCE_ARCHIVE VERSION}")
version=${2:?release version is required}
recipe=$(dirname "$(realpath "$0")")/arch/spotizgeg/PKGBUILD.in
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/src"
tar -xzf "$archive" -C "$work/src"
sed -e "s/@VERSION@/$version/g" -e 's/@PKGREL@/1/g' \
  -e 's/@SOURCE_SHA256@/unused/g' "$recipe" > "$work/PKGBUILD"

srcdir="$work/src"
expected="$srcdir/spotizgeg-$version"
test -f "$expected/Cargo.toml"
test -f "$expected/Cargo.lock"

# Exercise the real recipe phases; only the expensive Cargo subprocess is
# replaced. A wrong cd fails before this function, reproducing the 0.8.0 bug.
cargo() {
  test "$PWD" = "$expected"
  case "$1" in fetch|build|test) ;; *) return 1 ;; esac
}
# shellcheck source=/dev/null
source "$work/PKGBUILD"
prepare
build
check
printf 'Source recipe phases passed for %s\n' "$version"
