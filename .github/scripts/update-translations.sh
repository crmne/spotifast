#!/usr/bin/env bash
# Updates assets/i18n with fastframe-i18n's script, from the checkout Cargo
# already has. Requires GNU gettext tools with Rust support; normal Cargo
# builds do not. Pass --check to fail on a stale template instead.
set -euo pipefail
cd "$(dirname "$0")/../.."
crate=$(cargo metadata --format-version 1 --locked |
    grep -o '"manifest_path":"[^"]*fastframe-i18n/Cargo.toml"' | head -n1 |
    sed 's/^"manifest_path":"//; s/Cargo.toml"$//')
exec bash "$crate/scripts/update-translations.sh" --package Spotifast --domain spotifast \
    --bugs 'https://github.com/crmne/spotifast/issues/new?template=translation.yml' "$@"
