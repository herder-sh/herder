#!/usr/bin/env bash
# Archives the iOS app in Release and uploads it to TestFlight, signed through the App Store
# Connect API key with Xcode's cloud-managed certificates, so no certificate is kept anywhere.
# The version is the herder crate's, as for the CLI release.
# Needs what build-ffi.sh builds, XcodeGen, Xcode with its Metal toolchain, and in the
# environment: APPLE_TEAM_ID, ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_PATH (the key's .p8 file) and
# BUILD_NUMBER, which must be higher than that of every earlier upload of the same version.
set -euo pipefail

cd "$(dirname "$0")/../.."
version=$(cargo metadata --no-deps --format-version 1 |
  jq -r '.packages[] | select(.name == "herder") | .version')
work=$(mktemp -d)
auth=(-allowProvisioningUpdates -authenticationKeyPath "$ASC_KEY_PATH"
  -authenticationKeyID "$ASC_KEY_ID" -authenticationKeyIssuerID "$ASC_ISSUER_ID")

xcodegen generate --spec apple/project.yml
# Archived unsigned: signing the archive needs a development certificate, which a fresh runner
# lacks, so Xcode made a new one on every run until the account hit Apple's limit. The export
# below signs for distribution with a cloud-managed certificate. The iOS app has no entitlements
# for the unsigned archive to lose.
xcodebuild archive -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-iOS -configuration Release -destination "generic/platform=iOS" \
  -archivePath "$work/herder.xcarchive" COMPILER_INDEX_STORE_ENABLE=NO \
  MARKETING_VERSION="$version" CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
  CODE_SIGNING_ALLOWED=NO DEVELOPMENT_TEAM="$APPLE_TEAM_ID"

cat > "$work/ExportOptions.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>app-store-connect</string>
  <key>destination</key><string>upload</string>
  <key>teamID</key><string>$APPLE_TEAM_ID</string>
  <key>signingStyle</key><string>automatic</string>
  <key>manageAppVersionAndBuildNumber</key><false/>
</dict>
</plist>
PLIST
xcodebuild -exportArchive -archivePath "$work/herder.xcarchive" \
  -exportOptionsPlist "$work/ExportOptions.plist" -exportPath "$work/export" "${auth[@]}"
echo "uploaded herder $version ($BUILD_NUMBER) to App Store Connect"
