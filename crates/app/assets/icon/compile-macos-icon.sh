#!/bin/sh
set -eu

assets=$(cd "$(dirname "$0")" && pwd)
resources=$1
mkdir -p "$resources"
info=$(mktemp)
trap 'rm -f "$info"' EXIT

xcrun actool \
  --compile "$resources" \
  --platform macosx \
  --minimum-deployment-target 11.0 \
  --app-icon Kilat \
  --output-partial-info-plist "$info" \
  "$assets/Kilat.icon"

test -s "$resources/Assets.car"
test "$(plutil -extract CFBundleIconName raw "$info")" = Kilat
# Keep the existing ICNS for macOS versions that use the legacy icon.
iconutil -c icns -o "$resources/AppIcon.icns" "$assets/AppIcon.iconset"
