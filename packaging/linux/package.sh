#!/usr/bin/env bash
# Packages a built TMOP Linux binary for release.
#
# Usage: packaging/linux/package.sh <tag> <x64|arm64> <binary> <out dir>
# Writes into <out dir>, each with a .sha256 ("<hex>  <file>", for sha256sum -c):
#   TMOP-<tag>-linux-<arch>.tar.gz    TMOP-<tag>-linux-<arch>/{tmop, tmop.desktop, icons/hicolor/..., docs}
#   TMOP-linux-<arch>.tar.gz          the same archive under a version-less name
#   TMOP-linux-<arch>                 the bare binary, version-less
#   TMOP-<tag>-linux-<arch>.AppImage  and the version-less TMOP-linux-<arch>.AppImage
# The version-less names let install.sh and the download page use releases/latest/download/...
# appimagetool and the AppImage runtime are pinned below by release and sha256 and cached in
# $TMOP_TOOLS_DIR (default ~/.cache/tmop-packaging). Needs bash, curl, tar, gzip and sha256sum,
# and libxkbcommon-x11 installed (Debian/Ubuntu: libxkbcommon-x11-0), which the AppImage bundles.
set -euo pipefail

APPIMAGETOOL_VERSION=1.9.1
RUNTIME_VERSION=20251108

usage() {
    echo "usage: packaging/linux/package.sh <tag> <x64|arm64> <binary> <out dir>" >&2
    exit 2
}

[ $# -eq 4 ] || usage
tag=$1
arch=$2
binary=$3
out=$4

case "$tag" in
    v[0-9]*) ;;
    *) echo "tag must look like v0.1.0, got: $tag" >&2; exit 2 ;;
esac
case "$arch" in
    x64)
        machine=x86_64
        ldconfig_arch=x86-64
        tool_sha=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
        runtime_sha=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
        ;;
    arm64)
        machine=aarch64
        ldconfig_arch=AArch64
        tool_sha=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
        runtime_sha=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
        ;;
    *) usage ;;
esac
[ -f "$binary" ] || { echo "no binary at $binary" >&2; exit 1; }

root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

checksum() {
    (cd "$out" && hash=$(sha256sum "$1" | awk '{ print $1 }') && printf '%s  %s\n' "$hash" "$1" > "$1.sha256")
}

# Copies $2 to $1 when its sha256 is $3, downloading it into the cache first if needed.
fetch() {
    local dest=$1 url=$2 sha=$3
    local cached=$tools/$sha
    if [ ! -f "$cached" ] || ! echo "$sha  $cached" | sha256sum -c --quiet - >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$cached.part" "$url"
        echo "$sha  $cached.part" | sha256sum -c --quiet - || { echo "sha256 mismatch for $url" >&2; exit 1; }
        mv "$cached.part" "$cached"
    fi
    cp "$cached" "$dest"
    chmod +x "$dest"
}

# Lays out the hicolor icon theme (64, 256, 512, 1024) under $1.
icons() {
    local size
    for size in 64 256 512 1024; do
        install -Dm644 "$root/assets/icon/tmop-$size.png" "$1/hicolor/${size}x${size}/apps/tmop.png"
    done
}

# tar.gz: one top-level folder, so `tar -xzf` never spills into the current directory.
name=TMOP-$tag-linux-$arch
dir=$work/$name
install -Dm755 "$binary" "$dir/tmop"
install -Dm644 "$root/packaging/linux/tmop.desktop" "$dir/tmop.desktop"
icons "$dir/icons"
install -Dm644 "$root/README.md" "$root/LICENSE" "$root/assets/fonts/Michroma-OFL.txt" -t "$dir"
# --owner/--group and a fixed mtime keep the archive free of the build user and time.
tar -C "$work" --owner=0 --group=0 --numeric-owner --mtime='2000-01-01 00:00Z' --sort=name \
    -cf - "$name" | gzip -n -9 > "$out/$name.tar.gz"
checksum "$name.tar.gz"
cp "$out/$name.tar.gz" "$out/TMOP-linux-$arch.tar.gz"
checksum "TMOP-linux-$arch.tar.gz"
install -m755 "$binary" "$out/TMOP-linux-$arch"
checksum "TMOP-linux-$arch"

# AppImage: AppRun, the .desktop file and its icon at the root, the binary under usr/bin.
tools=${TMOP_TOOLS_DIR:-$HOME/.cache/tmop-packaging}
mkdir -p "$tools"
fetch "$work/appimagetool" \
    "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-$machine.AppImage" \
    "$tool_sha"
fetch "$work/runtime" \
    "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VERSION/runtime-$machine" \
    "$runtime_sha"
appdir=$work/TMOP.AppDir
install -Dm755 "$binary" "$appdir/usr/bin/tmop"
install -Dm755 "$root/packaging/linux/AppRun" "$appdir/AppRun"
install -Dm644 "$root/packaging/linux/tmop.desktop" "$appdir/tmop.desktop"
install -Dm644 "$root/packaging/linux/tmop.desktop" "$appdir/usr/share/applications/tmop.desktop"
icons "$appdir/usr/share/icons"
install -Dm644 "$root/assets/icon/tmop-256.png" "$appdir/tmop.png"
ln -s tmop.png "$appdir/.DirIcon"
# winit loads libxkbcommon-x11 with dlopen on X11 and panics without it, and plenty of systems
# lack it (the AppImage catalog's test machine among them). So the AppImage brings it and the
# libraries it needs that aren't on the AppImage excludelist; AppRun puts usr/lib on the path.
for lib in libxkbcommon.so.0 libxkbcommon-x11.so.0 libxcb-xkb.so.1; do
    path=$(/sbin/ldconfig -p | awk -v lib="$lib" -v arch="$ldconfig_arch" '$1 == lib && index($0, arch) { print $NF; exit }')
    [ -n "$path" ] || { echo "no $lib for $machine; install libxkbcommon-x11-0" >&2; exit 1; }
    install -Dm644 "$(readlink -f "$path")" "$appdir/usr/lib/$lib"
done
for pkg in libxkbcommon0 libxkbcommon-x11-0 libxcb-xkb1; do
    if [ -f "/usr/share/doc/$pkg/copyright" ]; then
        install -Dm644 "/usr/share/doc/$pkg/copyright" "$appdir/usr/share/doc/$pkg/copyright"
    fi
done
install -Dm644 "$root/README.md" "$root/LICENSE" "$root/assets/fonts/Michroma-OFL.txt" -t "$appdir/usr/share/doc/tmop"
image=$name.AppImage
# Extract-and-run: CI runners and containers have no FUSE to mount appimagetool's own image.
# The runtime is passed in so appimagetool never fetches its unpinned "continuous" one.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$machine "$work/appimagetool" --no-appstream \
    --runtime-file "$work/runtime" "$appdir" "$out/$image"
chmod 755 "$out/$image"
checksum "$image"
cp "$out/$image" "$out/TMOP-linux-$arch.AppImage"
checksum "TMOP-linux-$arch.AppImage"

ls -l "$out"
