#!/usr/bin/env bash
# Make the source RPM Fedora COPR builds Spotifast from.
#
#   packaging/rpm/build-srpm.sh VERSION OUTPUT_DIR
#
# Downloads the release's source archive, checks it against the release's
# checksums.txt, vendors every crate Cargo.lock names (COPR builds offline),
# renders spotifast.spec.in and packs both archives into
# OUTPUT_DIR/spotifast-VERSION-1.src.rpm. Needs curl, cargo, xz and rpmbuild;
# on Fedora: dnf install cargo rpm-build xz curl.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 VERSION OUTPUT_DIR" >&2
  exit 2
fi

version=${1#v}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
base="https://github.com/crmne/spotifast/releases/download/v${version}"
archive="spotifast-v${version}-source.tar.gz"

work=$(mktemp -d "${out}/.srpm.XXXXXX")
trap 'rm -rf "$work"' EXIT

curl --fail --silent --show-error --location --retry 3 \
  --output "$work/$archive" "$base/$archive"
curl --fail --silent --show-error --location --retry 3 \
  --output "$work/checksums.txt" "$base/checksums.txt"
line=$(grep -E "^[0-9a-f]{64}  ${archive//./\\.}\$" "$work/checksums.txt") || {
  echo "checksums.txt has no line for $archive" >&2
  exit 1
}
(cd "$work" && sha256sum --check --quiet <<<"$line")

tar -xzf "$work/$archive" -C "$work"
src="$work/spotifast-${version}"
rust_version=$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' "$src/Cargo.toml")
if [[ -z "$rust_version" ]]; then
  echo "Cargo.toml has no rust-version" >&2
  exit 1
fi

# cargo vendor prints the source replacement the build needs; the spec
# appends it to the archive's .cargo/config.toml.
(cd "$src" && cargo vendor --locked --versioned-dirs vendor >vendor-config.toml)

# Windows-only crates are most of the vendored bytes and never compile for
# Linux. Keep each one's Cargo.toml, which resolving Cargo.lock still reads,
# and an empty library; their checksum file keeps the package hash Cargo
# compares with Cargo.lock and lists no files.
for crate in "$src"/vendor/windows-[0-9]* "$src"/vendor/windows_* \
  "$src"/vendor/windows-sys-* "$src"/vendor/winapi-*-pc-windows-gnu-*; do
  [[ -d "$crate" ]] || continue
  package=$(sed -n 's/.*"package":"\([0-9a-f]*\)".*/\1/p' "$crate/.cargo-checksum.json")
  find "$crate" -mindepth 1 -maxdepth 1 ! -name Cargo.toml -exec rm -rf {} +
  mkdir "$crate/src"
  : >"$crate/src/lib.rs"
  printf '{"files":{},"package":"%s"}\n' "$package" >"$crate/.cargo-checksum.json"
done
XZ_OPT=-T0 tar -C "$src" -cJf "$work/spotifast-${version}-vendor.tar.xz" \
  vendor vendor-config.toml

sed -e "s/@VERSION@/${version}/g" \
  -e "s/@RUST_VERSION@/${rust_version}/g" \
  -e "s/@RPM_DATE@/$(LC_ALL=C date -u '+%a %b %d %Y')/g" \
  "$here/spotifast.spec.in" >"$work/spotifast.spec"
if grep -q '@[A-Z_]*@' "$work/spotifast.spec"; then
  echo "spotifast.spec.in has a placeholder this script does not fill" >&2
  exit 1
fi

# No dist tag: COPR rebuilds the source RPM once per chroot with its own.
rpmbuild -bs \
  --define "_sourcedir $work" \
  --define "_srcrpmdir $out" \
  --define "dist %{nil}" \
  "$work/spotifast.spec" >/dev/null
cp "$work/spotifast.spec" "$out/spotifast.spec"
echo "$out/spotifast-${version}-1.src.rpm"
