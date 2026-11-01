#!/bin/bash
# CI-only signed audition bundle. Leaves the stable app and release feeds alone.
set -euo pipefail
: "${RUNNER_TEMP:?}" "${PREVIEW_VERSION:?}" "${APPLE_CERTIFICATE_P12:?}"
: "${APPLE_SIGNING_IDENTITY:?}" "${APPLE_ID:?}" "${APPLE_TEAM_ID:?}" "${APPLE_APP_PASSWORD:?}"
keychain="$RUNNER_TEMP/soloist-preview.keychain-db"
certificate="$RUNNER_TEMP/soloist-preview.p12"
cleanup() {
    security delete-keychain "$keychain" 2>/dev/null || true
    rm -f "$certificate"
}
trap cleanup EXIT
python3 - <<'PY'
import os, pathlib, base64
p = pathlib.Path(os.environ['RUNNER_TEMP']) / 'soloist-preview.p12'
p.write_bytes(base64.b64decode(os.environ['APPLE_CERTIFICATE_P12']))
p.chmod(0o600)
PY
password="$(openssl rand -hex 24)"
echo "::add-mask::$password"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$certificate" -P "$APPLE_CERTIFICATE_PASSWORD" -k "$keychain" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
security list-keychains -d user -s "$keychain" "$HOME/Library/Keychains/login.keychain-db"
mkdir -p dist
app='dist/Spotifast Preview.app'
bash packaging/macos/bundle.sh target/release/spotifast "$app" "$PREVIEW_VERSION"
# Retain the stable signing identity so its keychain grants remain accessible.
# macOS runs one Spotifast copy at a time; the preview shares its saved setup.
/usr/libexec/PlistBuddy -c 'Set :CFBundleName Spotifast Preview' "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Set :CFBundleDisplayName Spotifast Preview' "$app/Contents/Info.plist"
codesign --force --timestamp --options runtime --sign "$APPLE_SIGNING_IDENTITY" "$app"
codesign --verify --strict "$app"
zip='dist/Spotifast-Preview-Mac.zip'
ditto -c -k --keepParent "$app" "$zip"
xcrun notarytool submit "$zip" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" --wait
xcrun stapler staple "$app"
rm "$zip"
ditto -c -k --keepParent "$app" "$zip"
