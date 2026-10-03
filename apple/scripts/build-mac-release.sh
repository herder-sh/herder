#!/usr/bin/env bash
# Builds the macOS app for release and zips it into dist/herder-app-<version>-macos-arm64.zip,
# with a .sha256 beside it. The version is the herder crate's, as for the CLI release. The app
# is ad-hoc signed: there is no Developer ID yet, so it is not notarized (see apple/README.md).
# Needs what build-ffi.sh builds, XcodeGen, and Xcode with its Metal toolchain.
set -euo pipefail

cd "$(dirname "$0")/../.."
version=$(cargo metadata --no-deps --format-version 1 |
  jq -r '.packages[] | select(.name == "herder") | .version')
name="herder-app-$version-macos-arm64"
derived=$(mktemp -d)

xcodegen generate --spec apple/project.yml
xcodebuild build -quiet -skipPackagePluginValidation -project apple/herder.xcodeproj \
  -scheme herder-macOS -configuration Release -destination "generic/platform=macOS" \
  -derivedDataPath "$derived" \
  MARKETING_VERSION="$version" CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM= \
  CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO

app="$derived/Build/Products/Release/herder.app"
codesign --verify --strict --deep "$app"

mkdir -p dist
rm -f "dist/$name.zip" "dist/$name.zip.sha256"
ditto -c -k --keepParent "$app" "dist/$name.zip"
cd dist
shasum -a 256 "$name.zip" > "$name.zip.sha256"
cat "$name.zip.sha256"
