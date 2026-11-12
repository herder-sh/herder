#!/usr/bin/env bash
# Imports the team's Developer ID Application certificate into a temporary keychain on a CI
# runner, so build-mac-release.sh can sign the Mac app with it, and prints the keychain's path.
# Delete it with `security delete-keychain <path>` when the job ends.
# Needs MAC_P12_BASE64 (the certificate and its key as a base64 .p12) and MAC_P12_PASSWORD.
set -euo pipefail
# shellcheck disable=SC2046 # `security list-keychains` prints one path per line, none with spaces

: "${MAC_P12_BASE64:?}" "${MAC_P12_PASSWORD:?}" "${RUNNER_TEMP:?}"
keychain="$RUNNER_TEMP/herder-signing.keychain-db"
password=$(openssl rand -hex 24)
p12="$RUNNER_TEMP/developer-id.p12"
ca="$RUNNER_TEMP/DeveloperIDG2CA.cer"
key="$RUNNER_TEMP/developer-id.key.pem"
cert="$RUNNER_TEMP/developer-id.cert.pem"

printf '%s' "$MAC_P12_BASE64" | base64 --decode > "$p12"
# The .p12 holds an item twice, which `security import` refuses as "already exists": it is
# packed again with only its first private key and first certificate. macOS's own openssl
# reads the encryptions Keychain Access and OpenSSL write, and writes one `security` reads.
ssl=/usr/bin/openssl
"$ssl" pkcs12 -in "$p12" -passin env:MAC_P12_PASSWORD -nocerts -nodes |
  "$ssl" pkey -out "$key"
"$ssl" pkcs12 -in "$p12" -passin env:MAC_P12_PASSWORD -clcerts -nokeys |
  "$ssl" x509 -out "$cert"
rm -f "$p12"
[ "$("$ssl" pkey -in "$key" -pubout)" = "$("$ssl" x509 -in "$cert" -noout -pubkey)" ] || {
  rm -f "$key" "$cert"
  echo "the .p12's first key does not belong to its first certificate" >&2
  exit 1
}
"$ssl" x509 -in "$cert" -noout -subject >&2
"$ssl" pkcs12 -export -inkey "$key" -in "$cert" -passout env:MAC_P12_PASSWORD -out "$p12"
rm -f "$key" "$cert"
# The certificate's issuer, which a runner may lack: codesign needs the whole chain.
curl -fsSL https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer -o "$ca"

security create-keychain -p "$password" "$keychain" >&2
security set-keychain-settings -lut 21600 "$keychain" >&2
security unlock-keychain -p "$password" "$keychain" >&2
security import "$p12" -k "$keychain" -f pkcs12 -P "$MAC_P12_PASSWORD" -T /usr/bin/codesign \
  -T /usr/bin/security -T /usr/bin/xcodebuild >&2
rm -f "$p12"
# Imported only where no keychain has it yet: a second copy is refused as "already exists".
security find-certificate -c "Developer ID Certification Authority" "$keychain" \
  $(security list-keychains -d user | tr -d '"') > /dev/null 2>&1 ||
  security import "$ca" -k "$keychain" -t cert >&2
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" \
  "$keychain" > /dev/null
# Searched first, before the runner's own keychains.
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
security find-identity -v -p codesigning "$keychain" >&2
echo "$keychain"
