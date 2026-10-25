#!/usr/bin/env bash
# End-to-end test of docs/install.sh on Linux against packaged assets (packaging/linux/package.sh).
#
# Usage: packaging/linux/test-install.sh <dist dir> [base url]
# <dist dir> holds TMOP-linux-<arch>.tar.gz and .AppImage with their .sha256 files. The assets
# are fetched from [base url] when given (e.g. http://127.0.0.1:8000 from python3 -m http.server
# in <dist dir>), else from file://<dist dir>. Every run uses throwaway home folders and never
# touches the real one.
set -euo pipefail

[ $# -ge 1 ] || { echo "usage: packaging/linux/test-install.sh <dist dir> [base url]" >&2; exit 2; }
root=$(cd "$(dirname "$0")/../.." && pwd)
dist=$(cd "$1" && pwd)
base=${2:-file://$dist}
script=$root/docs/install.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }

# Runs install.sh with $1 as HOME and no XDG overrides; the remaining arguments go to it.
run() {
    local home=$1
    shift
    env -u XDG_DATA_HOME -u XDG_CONFIG_HOME HOME="$home" TMOP_INSTALL_BASE_URL="$base" sh "$script" "$@"
}

check_installed() { # <home> <what>
    local home=$1 share=$1/.local/share
    [ -x "$home/.local/bin/tmop" ] || fail "$2: no executable ~/.local/bin/tmop"
    [ -f "$share/applications/tmop.desktop" ] || fail "$2: no tmop.desktop"
    [ "$(grep -c '^Exec=' "$share/applications/tmop.desktop")" = 1 ] || fail "$2: the desktop file needs one Exec line"
    grep -qx 'StartupWMClass=tmop' "$share/applications/tmop.desktop" || fail "$2: desktop file lost StartupWMClass"
    for size in 64 256 512 1024; do
        [ -f "$share/icons/hicolor/${size}x${size}/apps/tmop.png" ] || fail "$2: no ${size}px icon"
    done
    [ "$(wc -l < "$share/tmop/installed-files")" -eq 6 ] || fail "$2: the manifest should list 6 files"
}

# 1. tar.gz install into a fresh home, then run the program it installed.
home=$work/home1
mkdir -p "$home"
run "$home" > "$work/out1"
grep -q 'TMOP is installed' "$work/out1" || fail "no success message"
check_installed "$home" "tar.gz"
grep -qx "Exec=\"$home/.local/bin/tmop\"" "$home/.local/share/applications/tmop.desktop" || fail "Exec does not point at the installed file"
"$home/.local/bin/tmop" --dump | grep -q '^CPU' || fail "the installed tmop --dump printed no CPU line"
ok "tar.gz install"

# 2. Running it again updates in place and says so.
run "$home" > "$work/out2"
grep -q 'TMOP is updated' "$work/out2" || fail "a second run should report an update"
check_installed "$home" "update"
ok "update"

# 3. --appimage replaces the program with the AppImage and keeps the launcher and icons.
run "$home" --appimage > "$work/out3"
check_installed "$home" "appimage"
[ "$(head -c 11 "$home/.local/bin/tmop" | tail -c 3 | od -An -tx1 | tr -d ' \n')" = "414902" ] \
    || fail "\$HOME/.local/bin/tmop is not a type 2 AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 "$home/.local/bin/tmop" --dump | grep -q '^CPU' || fail "the installed AppImage --dump printed no CPU line"
ok "appimage install"

# 4. --uninstall removes exactly the listed files and keeps user data.
mkdir -p "$home/.config/tmop" && echo '{}' > "$home/.config/tmop/settings.json"
touch "$home/.local/share/applications/other.desktop"
run "$home" --uninstall > "$work/out4"
[ ! -e "$home/.local/bin/tmop" ] || fail "uninstall left the program"
[ ! -e "$home/.local/share/applications/tmop.desktop" ] || fail "uninstall left the launcher"
[ -z "$(find "$home/.local/share/icons" -name tmop.png)" ] || fail "uninstall left icons"
[ ! -e "$home/.local/share/tmop/installed-files" ] || fail "uninstall left the manifest"
[ -f "$home/.config/tmop/settings.json" ] || fail "uninstall removed the settings"
[ -f "$home/.local/share/applications/other.desktop" ] || fail "uninstall removed a file it did not install"
run "$home" --uninstall | grep -q 'not installed by this script' || fail "a second uninstall should find nothing"
ok "uninstall"

# 5. XDG_DATA_HOME and a home path with a space, quote and dollar sign: the Exec line escapes them.
home="$work/my \"odd\" \$home"
mkdir -p "$home"
env HOME="$home" XDG_DATA_HOME="$home/data" TMOP_INSTALL_BASE_URL="$base" sh "$script" > /dev/null
[ -f "$home/data/applications/tmop.desktop" ] || fail "XDG_DATA_HOME was not used"
want="Exec=\"${home//\"/\\\"}/.local/bin/tmop\""
want=${want//\$/\\\$}
grep -qxF "$want" "$home/data/applications/tmop.desktop" || fail "Exec line not escaped: $(grep '^Exec=' "$home/data/applications/tmop.desktop")"
env HOME="$home" XDG_DATA_HOME="$home/data" sh "$script" --uninstall > /dev/null
if [ -e "$home/.local/bin/tmop" ] || [ -e "$home/data/applications/tmop.desktop" ]; then
    fail "uninstall with XDG_DATA_HOME"
fi
ok "XDG_DATA_HOME and quoting"

# 6. A bad checksum installs nothing.
bad=$work/bad
mkdir -p "$bad" "$work/home6"
cp "$dist"/TMOP-linux-*.tar.gz "$dist"/TMOP-linux-*.tar.gz.sha256 "$bad/"
for f in "$bad"/*.sha256; do printf '%064d  x\n' 0 > "$f"; done
if env -u XDG_DATA_HOME HOME="$work/home6" TMOP_INSTALL_BASE_URL="file://$bad" sh "$script" 2> "$work/err6"; then
    fail "a bad checksum should fail"
fi
grep -q 'SHA-256 does not match' "$work/err6" || fail "no checksum message"
[ ! -e "$work/home6/.local" ] || fail "a failed install left files behind"
ok "bad checksum"

# 7. Unknown options and missing assets fail cleanly.
if run "$work/home6" --frobnicate 2> /dev/null; then fail "an unknown option should fail"; fi
if env HOME="$work/home6" TMOP_INSTALL_BASE_URL="file://$work/nowhere" sh "$script" 2> "$work/err7"; then
    fail "a missing asset should fail"
fi
grep -q 'could not download' "$work/err7" || fail "no download message"
ok "errors"

# 8. A ~/.local/bin/tmop this script did not install is left alone (and so never uninstalled).
home=$work/home8
mkdir -p "$home/.local/bin"
echo 'mine' > "$home/.local/bin/tmop"
if run "$home" 2> "$work/err8"; then fail "an existing foreign tmop should stop the install"; fi
grep -q 'was not installed by this script' "$work/err8" || fail "no message about the existing file"
[ "$(cat "$home/.local/bin/tmop")" = mine ] || fail "the existing tmop was replaced"
[ ! -e "$home/.local/share/tmop/installed-files" ] || fail "a refused install wrote a manifest"
ok "existing file kept"

echo "all install.sh tests passed"
