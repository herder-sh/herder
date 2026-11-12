#!/usr/bin/env bash
# Imports the team's Developer ID Application certificate into a temporary keychain on a CI
# runner, so build-mac-release.sh can sign the Mac app with it, and prints the keychain's path.
# Delete it with `security delete-keychain <path>` when the job ends.
# Needs MAC_P12_BASE64 (the certificate and its key as a base64 .p12) and MAC_P12_PASSWORD.
set -euo pipefail

: "${MAC_P12_BASE64:?}" "${MAC_P12_PASSWORD:?}" "${RUNNER_TEMP:?}"
keychain="$RUNNER_TEMP/herder-signing.keychain-db"
password=$(openssl rand -hex 24)
p12="$RUNNER_TEMP/developer-id.p12"
ca="$RUNNER_TEMP/DeveloperIDG2CA.cer"

printf '%s' "$MAC_P12_BASE64" | base64 --decode > "$p12"
# The certificate's issuer, which a runner may lack: codesign needs the whole chain.
curl -fsSL https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer -o "$ca"

security create-keychain -p "$password" "$keychain" >&2
security set-keychain-settings -lut 21600 "$keychain" >&2
security unlock-keychain -p "$password" "$keychain" >&2
security import "$p12" -k "$keychain" -P "$MAC_P12_PASSWORD" -f pkcs12 -T /usr/bin/codesign \
  -T /usr/bin/security -T /usr/bin/xcodebuild >&2
security import "$ca" -k "$keychain" >&2
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" \
  "$keychain" > /dev/null
# Searched first, before the runner's own keychains.
# shellcheck disable=SC2046 # one keychain path per line, none with spaces
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
rm -f "$p12"
security find-identity -v -p codesigning "$keychain" >&2
echo "$keychain"
