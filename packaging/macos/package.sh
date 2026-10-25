#!/usr/bin/env bash
# Packages built TMOP macOS binaries as a universal TMOP.app for release. Runs on macOS only
# (lipo, sips, iconutil, codesign, ditto, hdiutil).
#
# Usage: packaging/macos/package.sh <tag> <aarch64 binary> <x86_64 binary> <out dir>
# Writes into <out dir>, each with a .sha256 ("<hex>  <file>", for shasum -a 256 -c):
#   TMOP-<tag>-macos-universal.zip   TMOP.app (ditto, keeps the bundle's attributes)
#   TMOP-<tag>-macos-universal.dmg   TMOP.app plus an Applications link to drag it onto
#   TMOP-macos-universal.zip and TMOP-macos-universal.dmg, the same files version-less
#
# Signing: ad-hoc unless MACOS_SIGN_IDENTITY names a Developer ID Application identity in the
# keychain. Notarization runs only with that identity plus APPLE_ID, APPLE_TEAM_ID and
# APPLE_APP_PASSWORD (an app-specific password); otherwise it is skipped.
set -euo pipefail

usage() {
    echo "usage: packaging/macos/package.sh <tag> <aarch64 binary> <x86_64 binary> <out dir>" >&2
    exit 2
}

[ $# -eq 4 ] || usage
tag=$1
arm=$2
intel=$3
out=$4

case "$tag" in
    v[0-9]*) ;;
    *) echo "tag must look like v0.1.0, got: $tag" >&2; exit 2 ;;
esac
# CFBundleVersion must be numbers and dots, so a pre-release suffix (v0.6.0-rc1) is dropped.
version=${tag#v}
version=${version%%[-+]*}

root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

checksum() {
    (cd "$out" && hash=$(shasum -a 256 "$1" | awk '{ print $1 }') && printf '%s  %s\n' "$hash" "$1" > "$1.sha256")
}

identity=${MACOS_SIGN_IDENTITY:-}
notarize=
if [ -n "$identity" ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
    notarize=1
fi

sign() {
    if [ -n "$identity" ]; then
        codesign --force --deep --options runtime --timestamp -s "$identity" "$1"
    else
        codesign --force --deep -s - "$1"
    fi
}

submit() {
    xcrun notarytool submit "$1" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
        --password "$APPLE_APP_PASSWORD" --wait
}

app=$work/TMOP.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create "$arm" "$intel" -output "$app/Contents/MacOS/TMOP"
lipo "$app/Contents/MacOS/TMOP" -verify_arch arm64 x86_64
chmod 755 "$app/Contents/MacOS/TMOP"
sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp "$root/README.md" "$root/LICENSE" "$root/assets/fonts/Michroma-OFL.txt" "$app/Contents/Resources/"

# The icon set iconutil expects: every size at 1x and 2x, from the 1024 px master where no
# drawn size exists.
iconset=$work/tmop.iconset
mkdir -p "$iconset"
icon() { # <pixels> <iconset name>
    local src=$root/assets/icon/tmop-$1.png
    if [ -f "$src" ]; then
        cp "$src" "$iconset/$2.png"
    else
        sips -z "$1" "$1" "$root/assets/icon/tmop-1024.png" --out "$iconset/$2.png" >/dev/null
    fi
}
icon 16 icon_16x16
icon 32 icon_16x16@2x
icon 32 icon_32x32
icon 64 icon_32x32@2x
icon 128 icon_128x128
icon 256 icon_128x128@2x
icon 256 icon_256x256
icon 512 icon_256x256@2x
icon 512 icon_512x512
icon 1024 icon_512x512@2x
iconutil -c icns "$iconset" -o "$app/Contents/Resources/tmop.icns"

sign "$app"
codesign --verify --deep --strict --verbose=2 "$app"

if [ -n "$notarize" ]; then
    ditto -c -k --keepParent "$app" "$work/notarize.zip"
    submit "$work/notarize.zip"
    xcrun stapler staple "$app"
else
    echo "notarization skipped (no Developer ID identity or Apple credentials)"
fi

name=TMOP-$tag-macos-universal
ditto -c -k --keepParent "$app" "$out/$name.zip"

stage=$work/dmg
mkdir -p "$stage"
ditto "$app" "$stage/TMOP.app"
ln -s /Applications "$stage/Applications"
# hdiutil now and then fails with "Resource busy" on CI runners, so it gets three tries.
for try in 1 2 3; do
    hdiutil create -volname TMOP -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$out/$name.dmg" && break
    [ "$try" -lt 3 ] || exit 1
    sleep 5
done
if [ -n "$identity" ]; then
    codesign --force --timestamp -s "$identity" "$out/$name.dmg"
fi
if [ -n "$notarize" ]; then
    submit "$out/$name.dmg"
    xcrun stapler staple "$out/$name.dmg"
fi

for ext in zip dmg; do
    checksum "$name.$ext"
    cp "$out/$name.$ext" "$out/TMOP-macos-universal.$ext"
    checksum "TMOP-macos-universal.$ext"
done

ls -l "$out"
