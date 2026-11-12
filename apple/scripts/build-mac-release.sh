#!/usr/bin/env bash
# Builds the macOS app for release and zips it into dist/herder-app-<version>-macos-arm64.zip,
# with a .sha256 beside it. The version is the herder crate's, as for the CLI release.
#
# With APPLE_TEAM_ID, ASC_KEY_ID, ASC_ISSUER_ID and ASC_KEY_PATH (the App Store Connect key's
# .p8 file) set, and the team's Developer ID Application certificate in a keychain
# (import-mac-cert.sh), as in the release workflow, the app is signed with that certificate and
# a Developer ID provisioning profile, which developer-id-profile.py finds or creates through
# the key (Xcode's automatic signing asks for cloud signing, which this team may not use) and
# which grants the herder.sh associated domains passkeys need, then notarized and stapled. The
# build fails unless the app carries the team, its application identifier and associated domains,
# and Gatekeeper accepts it as notarized. Without APPLE_TEAM_ID, as from a fork, the app is
# ad-hoc signed, without associated domains, and cannot use a passkey (see apple/README.md).
# Needs what build-ffi.sh builds, XcodeGen, and Xcode with its Metal toolchain.
set -euo pipefail

cd "$(dirname "$0")/../.."
version=$(cargo metadata --no-deps --format-version 1 |
  jq -r '.packages[] | select(.name == "herder") | .version')
name="herder-app-$version-macos-arm64"
work=$(mktemp -d)
build=(-quiet -skipPackagePluginValidation -project apple/herder.xcodeproj -scheme herder-macOS
  -configuration Release -destination "generic/platform=macOS" COMPILER_INDEX_STORE_ENABLE=NO
  MARKETING_VERSION="$version" CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY=-
  CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO)

xcodegen generate --spec apple/project.yml

if [ -z "${APPLE_TEAM_ID:-}" ]; then
  # Xcode refuses to sign associated domains ad hoc, and macOS to launch an app that claims them.
  xcodebuild build "${build[@]}" -derivedDataPath "$work/derived" DEVELOPMENT_TEAM= \
    HERDER_MAC_ENTITLEMENTS="$(apple/scripts/adhoc-entitlements.sh)"
  app="$work/derived/Build/Products/Release/herder.app"
  codesign --verify --strict --deep "$app"
else
  : "${ASC_KEY_ID:?}" "${ASC_ISSUER_ID:?}" "${ASC_KEY_PATH:?}"
  app_id="$APPLE_TEAM_ID.sh.herder.Herder"
  grep -q "\"$app_id\"" site/.well-known/apple-app-site-association || {
    echo "site/.well-known/apple-app-site-association does not list $app_id" >&2
    exit 1
  }
  # Archived with the ad-hoc entitlements, which Xcode accepts without a profile, then signed
  # ad hoc again with the full set, so the export, which keeps the archive's entitlements,
  # signs the associated domains in with the Developer ID profile.
  xcodebuild archive "${build[@]}" -archivePath "$work/herder.xcarchive" \
    DEVELOPMENT_TEAM="$APPLE_TEAM_ID" HERDER_MAC_ENTITLEMENTS="$(apple/scripts/adhoc-entitlements.sh)"
  codesign --force --sign - --options runtime --entitlements apple/App/herder.entitlements \
    "$work/herder.xcarchive/Products/Applications/herder.app"

  identity=$(security find-identity -v -p codesigning |
    awk '/"Developer ID Application: .*\('"$APPLE_TEAM_ID"'\)"/ { print $2; exit }')
  [ -n "$identity" ] || {
    echo "no keychain holds the team's Developer ID Application certificate" >&2
    exit 1
  }
  serial=$(security find-certificate -a -Z -p |
    awk -v id="$identity" '/^SHA-1 hash:/ { take = $3 == id; next }
      take && /BEGIN CERTIFICATE/ { pem = 1 } pem { print } pem && /END CERTIFICATE/ { exit }' |
    openssl x509 -noout -serial | cut -d= -f2)
  profile=$(apple/scripts/developer-id-profile.py sh.herder.Herder "$serial")

  cat > "$work/ExportOptions.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>developer-id</string>
  <key>teamID</key><string>$APPLE_TEAM_ID</string>
  <key>signingStyle</key><string>manual</string>
  <key>signingCertificate</key><string>$identity</string>
  <key>provisioningProfiles</key>
  <dict><key>sh.herder.Herder</key><string>$profile</string></dict>
</dict>
</plist>
PLIST
  xcodebuild -exportArchive -archivePath "$work/herder.xcarchive" \
    -exportOptionsPlist "$work/ExportOptions.plist" -exportPath "$work/export"
  app="$work/export/herder.app"

  codesign --verify --strict --deep "$app"
  codesign -dv "$app" 2>&1 | tee "$work/signature"
  grep -qx "TeamIdentifier=$APPLE_TEAM_ID" "$work/signature" || {
    echo "the app is not signed by the team" >&2
    exit 1
  }
  codesign -d --entitlements - --xml "$app" > "$work/entitlements.plist"
  plutil -p "$work/entitlements.plist"
  [ "$(/usr/libexec/PlistBuddy -c "Print :com.apple.application-identifier" \
    "$work/entitlements.plist")" = "$app_id" ] || {
    echo "the app's application identifier is not $app_id" >&2
    exit 1
  }
  /usr/libexec/PlistBuddy -c "Print :com.apple.developer.associated-domains" \
    "$work/entitlements.plist" | grep -q "webcredentials:herder.sh" || {
    echo "the app has no herder.sh associated domains, so passkeys would fail" >&2
    exit 1
  }

  ditto -c -k --keepParent "$app" "$work/notarize.zip"
  xcrun notarytool submit "$work/notarize.zip" --key "$ASC_KEY_PATH" --key-id "$ASC_KEY_ID" \
    --issuer "$ASC_ISSUER_ID" --wait --output-format json | tee "$work/notarization.json"
  echo
  if [ "$(jq -r .status "$work/notarization.json")" != Accepted ]; then
    xcrun notarytool log "$(jq -r .id "$work/notarization.json")" --key "$ASC_KEY_PATH" \
      --key-id "$ASC_KEY_ID" --issuer "$ASC_ISSUER_ID" || true
    echo "notarization failed" >&2
    exit 1
  fi
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  spctl -a -vv -t exec "$app" 2>&1 | tee "$work/gatekeeper"
  grep -q "source=Notarized Developer ID" "$work/gatekeeper" || {
    echo "Gatekeeper does not accept the app as notarized" >&2
    exit 1
  }
fi

mkdir -p dist
rm -f "dist/$name.zip" "dist/$name.zip.sha256"
ditto -c -k --keepParent "$app" "dist/$name.zip"
cd dist
shasum -a 256 "$name.zip" > "$name.zip.sha256"
cat "$name.zip.sha256"
