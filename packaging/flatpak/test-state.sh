#!/usr/bin/env bash
# Check durable state across separate sandboxes, using only dummy markers.
# Requires Flatpak, Ruby (for YAML), and an installed Platform runtime.
# Defaults match the manifests; an already installed runtime can be supplied:
#   packaging/flatpak/test-state.sh org.kde.Platform 6.9
set -euo pipefail
runtime="${1:-org.freedesktop.Platform}"
runtime_branch="${2:-24.08}"
here="$(cd "$(dirname "$0")" && pwd)"
probe_dir="$(mktemp -d)"
probe_id="rocks.spotizgeg.StateProbe.p$$"
probe_data="$HOME/.var/app/$probe_id"
test ! -e "$probe_data"
trap 'rm -rf -- "$probe_dir" "$probe_data"' EXIT

flatpak build-init "$probe_dir/build" "$probe_id" "$runtime" "$runtime" "$runtime_branch"
build=(flatpak build --runtime --with-appdir --nofilesystem=host --nofilesystem=home)

# Simulate older Flatpak's unset XDG_STATE_HOME inside each process. Current
# Flatpak supplies the variable even when build receives --unset-env.
write_marker='unset XDG_STATE_HOME; mkdir -p "$HOME/.local/state/spotizgeg"; printf "%s\n" state-probe > "$HOME/.local/state/spotizgeg/probe"'
read_marker='unset XDG_STATE_HOME; test "$(cat "$HOME/.local/state/spotizgeg/probe")" = state-probe'

"${build[@]}" "$probe_dir/build" sh -ec "$write_marker"
"${build[@]}" "$probe_dir/build" sh -ec 'test ! -e "$HOME/.local/state/spotizgeg/probe"'
echo 'PASS: without persistence, fallback state is lost on exit'

for manifest in "$here/rocks.spotizgeg.Spotizgeg.yml" "$here/rocks.spotizgeg.Spotizgeg.bundle.yml"; do
  ruby -ryaml -e 'puts YAML.load_file(ARGV.fetch(0)).fetch("finish-args").grep(/\A--persist=/)' "$manifest" > "$probe_dir/permissions"
  mapfile -t persistence < "$probe_dir/permissions"
  "${build[@]}" "${persistence[@]}" "$probe_dir/build" sh -ec "$write_marker"
  "${build[@]}" "${persistence[@]}" "$probe_dir/build" sh -ec "$read_marker"
  # Newer Flatpak must see the same data through its XDG_STATE_HOME too.
  "${build[@]}" "${persistence[@]}" "$probe_dir/build" sh -ec 'if test -n "${XDG_STATE_HOME:-}"; then test "$(cat "$XDG_STATE_HOME/spotizgeg/probe")" = state-probe; fi'
  "${build[@]}" "${persistence[@]}" "$probe_dir/build" sh -ec 'rm "$HOME/.local/state/spotizgeg/probe"'
  echo "PASS: $(basename "$manifest") retains state across launches"
done
