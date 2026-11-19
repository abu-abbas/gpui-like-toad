#!/bin/sh
set -eu

assets=$(cd "$(dirname "$0")" && pwd)
resources=$1
mkdir -p "$resources"
info=$(mktemp)
log=$(mktemp)
trap 'rm -f "$info" "$log"' EXIT

if ! xcrun actool \
  --compile "$resources" \
  --platform macosx \
  --target-device mac \
  --minimum-deployment-target 11.0 \
  --app-icon Kilat \
  --output-partial-info-plist "$info" \
  "$assets/Kilat.icon" >"$log" 2>&1; then
  printf '::error::Asset catalog compilation failed (%s). %s\n' \
    "$(xcodebuild -version | head -n 1)" "$(tail -n 12 "$log" | tr '\n' ' ')"
  exit 1
fi
cat "$log"

if ! test -s "$resources/Assets.car"; then
  printf '::error::Asset catalog not generated. %s\n' "$(tail -n 12 "$log" | tr '\n' ' ')"
  exit 1
fi
# Keep the existing ICNS for macOS versions that use the legacy icon.
iconutil -c icns -o "$resources/AppIcon.icns" "$assets/AppIcon.iconset"
