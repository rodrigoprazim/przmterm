#!/usr/bin/env bash
# Builds PrzmTerm.app from the przmterm and przm binaries and packs it in a
# disk image with a link to Applications. Runs on macOS (sips, iconutil,
# codesign, tiffutil, hdiutil). With dmgbuild (pip install dmgbuild, or its
# path in DMGBUILD) the image opens on a styled window
# (packaging/macos/dmg-settings.py); without it, on a plain one.
#
#   scripts/macos-app.sh VERSION BIN_DIR DMG
#
# BIN_DIR holds przmterm and przm (universal, or one architecture for a
# local test). The app is signed ad-hoc: notarising needs a paid Apple
# Developer account, so macOS asks once before opening it (README, Install).
set -euo pipefail

if [ $# -ne 3 ]; then
    echo "usage: $0 VERSION BIN_DIR DMG" >&2
    exit 2
fi
version=$1
bin=$2
dmg=$3
root=$(cd "$(dirname "$0")/.." && pwd)
# CFBundleShortVersionString takes numbers only: 1.0.0-rc.1 shows as 1.0.0.
short=${version%%-*}
id=io.github.rodrigoprazim.przmterm

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage="$work/stage"
app="$stage/PrzmTerm.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$bin/przmterm" "$bin/przm" "$app/Contents/MacOS/"
cp "$root/README.md" "$root/CHANGELOG.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" \
    "$app/Contents/Resources/"

# The icon, every size macOS asks for, from the 1024 px drawing.
iconset="$work/PrzmTerm.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$root/assets/icon-1024.png" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$root/assets/icon-1024.png" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/PrzmTerm.icns"

cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>PrzmTerm</string>
    <key>CFBundleExecutable</key>
    <string>przmterm</string>
    <key>CFBundleIconFile</key>
    <string>PrzmTerm</string>
    <key>CFBundleIdentifier</key>
    <string>$id</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>PrzmTerm</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$short</string>
    <key>CFBundleVersion</key>
    <string>$short</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.15</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>PrzmTerm $version, MIT OR Apache-2.0</string>
    <key>NSLocalNetworkUsageDescription</key>
    <string>PrzmTerm connects to the hosts you open on your local network over SSH and Telnet.</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Nested code first, then the bundle (which seals the resources).
codesign --force --sign - --identifier "$id.przm" "$app/Contents/MacOS/przm"
codesign --force --sign - "$app"
codesign --verify --strict "$app"

rm -f "$dmg"
volume="PrzmTerm $version"
dmgbuild=${DMGBUILD:-dmgbuild}
if command -v "$dmgbuild" >/dev/null 2>&1; then
    # The styled window: the background (both resolutions in one TIFF),
    # the icons where it shows them, and the app's icon on the volume.
    tiffutil -cathidpicheck "$root/packaging/macos/dmg-background.png" \
        "$root/packaging/macos/dmg-background@2x.png" \
        -out "$work/background.tiff" >/dev/null
    "$dmgbuild" -s "$root/packaging/macos/dmg-settings.py" \
        -D app="$app" -D background="$work/background.tiff" \
        -D icon="$app/Contents/Resources/PrzmTerm.icns" \
        "$volume" "$dmg" >/dev/null
else
    # Without dmgbuild (a local test): a plain window.
    echo "dmgbuild not found (pip install dmgbuild): plain disk image" >&2
    ln -s /Applications "$stage/Applications"
    hdiutil create -quiet -volname "$volume" -srcfolder "$stage" \
        -fs HFS+ -format UDZO "$dmg"
fi
echo "$dmg"
