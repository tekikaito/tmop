#!/bin/sh
# Installs or updates TMOP for the current user on Linux (x86_64, aarch64) or macOS. Run:
#
#   curl -fsSL https://tekikaito.github.io/tmop/install.sh | sh
#   curl -fsSL https://tekikaito.github.io/tmop/install.sh | sh -s -- --appimage
#   curl -fsSL https://tekikaito.github.io/tmop/install.sh | sh -s -- --uninstall
#
# It downloads the newest release from GitHub and checks its SHA-256. No root permission is
# needed and nothing outside your home folder is touched.
#
# Linux:  ~/.local/bin/tmop, ~/.local/share/applications/tmop.desktop and the tmop icons under
#         ~/.local/share/icons/hicolor (the share folder follows $XDG_DATA_HOME). By default
#         the program comes from the tar.gz; with --appimage ~/.local/bin/tmop is the AppImage.
#         The installed files are listed in ~/.local/share/tmop/installed-files, and
#         --uninstall removes exactly those.
# macOS:  ~/Applications/TMOP.app. curl sets no quarantine flag, so Gatekeeper does not block
#         it the way it blocks a download from the browser. --uninstall removes the app.
#
# Settings and recordings are never removed: ~/.config/tmop and ~/.local/share/tmop on Linux,
# ~/Library/Application Support/TMOP on macOS.
#
# TMOP_INSTALL_BASE_URL replaces the release download URL (for testing against a local server).
#
# Everything runs from main at the very end, so a download cut short never runs half a script.

set -eu

REPO=tekikaito/tmop
BASE_URL=${TMOP_INSTALL_BASE_URL:-https://github.com/$REPO/releases/latest/download}
# HTTPS only, redirects included; a test URL may also be http:// or file://.
PROTO='=https'
[ -z "${TMOP_INSTALL_BASE_URL:-}" ] || PROTO='=https,http,file'
BUNDLE_ID=io.github.tekikaito.tmop

say() { printf '%s\n' "$*"; }
fail() { printf 'tmop install: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Installs or updates TMOP for the current user.

Usage: install.sh [--appimage] [--uninstall]

  --appimage   Linux: install the AppImage instead of the plain program
  --uninstall  remove what this script installed (settings are kept)
  --help       show this help
EOF
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "this needs '$1', which is not installed."
}

# The SHA-256 of file $1 in lower-case hex.
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{ print $1 }'
    else
        fail "this needs 'sha256sum' or 'shasum' to check the download."
    fi
}

# Downloads release asset $1 and its .sha256 into $2 and checks the sum.
download() {
    say "Downloading $1..."
    curl -fL --retry 3 --proto "$PROTO" --proto-redir "$PROTO" --progress-bar -o "$2/$1" "$BASE_URL/$1" \
        || fail "could not download $BASE_URL/$1"
    curl -fsSL --retry 3 --proto "$PROTO" --proto-redir "$PROTO" -o "$2/$1.sha256" "$BASE_URL/$1.sha256" \
        || fail "could not download $BASE_URL/$1.sha256"
    expected=$(awk 'NR == 1 { print tolower($1) }' "$2/$1.sha256" | tr -d '\r')
    actual=$(sha256 "$2/$1")
    if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
        fail "the download is damaged (SHA-256 does not match). Nothing was installed; please try again."
    fi
}

# A path quoted for the Exec key of a .desktop file: inside double quotes, \ " ` $ take a
# backslash, and % is doubled because it starts a field code.
desktop_quote() {
    printf '"%s"' "$(printf '%s' "$1" | sed -e 's/[\\"`$]/\\&/g' -e 's/%/%%/g')"
}

# Copies $1 to $2 through a temporary file and a rename, so a running TMOP keeps its old file.
put() {
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2.new.$$"
    chmod "$3" "$2.new.$$"
    mv -f "$2.new.$$" "$2"
}

linux_paths() {
    bin_dir=$HOME/.local/bin
    bin=$bin_dir/tmop
    share=${XDG_DATA_HOME:-$HOME/.local/share}
    case "$share" in /*) ;; *) share=$HOME/.local/share ;; esac
    desktop=$share/applications/tmop.desktop
    icons=$share/icons/hicolor
    manifest=$share/tmop/installed-files
}

# Tells the desktop about new or removed launchers and icons; neither tool is required.
linux_refresh() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$share/applications" 2>/dev/null || true
    fi
    if [ -d "$icons" ]; then
        touch "$icons" 2>/dev/null || true
        if [ -f "$icons/index.theme" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
            gtk-update-icon-cache -q -t "$icons" 2>/dev/null || true
        fi
    fi
}

linux_install() {
    appimage=$1
    case "$(uname -m)" in
        x86_64 | amd64) arch=x64 ;;
        aarch64 | arm64) arch=arm64 ;;
        *) fail "TMOP for Linux needs a 64-bit x86 or ARM processor; this one reports '$(uname -m)'." ;;
    esac
    linux_paths
    existed=
    [ ! -f "$manifest" ] || existed=1
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/tmop-install.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' HUP INT TERM

    if [ -n "$appimage" ]; then
        asset=TMOP-linux-$arch.AppImage
        download "$asset" "$tmp"
        put "$tmp/$asset" "$bin" 755
        # The launcher and the icons come out of the AppImage itself. Extracting runs from
        # ~/.local/bin, where execution is allowed even if the temp folder is mounted noexec.
        (cd "$tmp" && "$bin" --appimage-extract >/dev/null) \
            || fail "could not unpack the AppImage's launcher and icons."
        src=$tmp/squashfs-root
        src_desktop=$src/tmop.desktop
        src_icons=$src/usr/share/icons/hicolor
    else
        need tar
        asset=TMOP-linux-$arch.tar.gz
        download "$asset" "$tmp"
        mkdir "$tmp/x"
        tar -xzf "$tmp/$asset" -C "$tmp/x" || fail "could not unpack $asset."
        src=$(find "$tmp/x" -mindepth 1 -maxdepth 1 -type d | head -n 1)
        if [ -z "$src" ] || [ ! -f "$src/tmop" ]; then
            fail "$asset does not contain the tmop program."
        fi
        put "$src/tmop" "$bin" 755
        src_desktop=$src/tmop.desktop
        src_icons=$src/icons/hicolor
    fi
    [ -f "$src_desktop" ] || fail "the download has no tmop.desktop."

    # Point the launcher at the installed file: ~/.local/bin is often missing from the PATH a
    # desktop session starts programs with.
    exec_line="Exec=$(desktop_quote "$bin")"
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            Exec=*) printf '%s\n' "$exec_line" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "$src_desktop" > "$tmp/tmop.desktop"
    put "$tmp/tmop.desktop" "$desktop" 644

    files="$bin
$desktop"
    for png in "$src_icons"/*/apps/tmop.png; do
        [ -f "$png" ] || continue
        size=$(basename "$(dirname "$(dirname "$png")")")
        put "$png" "$icons/$size/apps/tmop.png" 644
        files="$files
$icons/$size/apps/tmop.png"
    done

    # Files an earlier install put in place that this one did not (an icon size that is gone).
    if [ -f "$manifest" ]; then
        while IFS= read -r old; do
            [ -n "$old" ] || continue
            printf '%s\n' "$files" | grep -Fqx -- "$old" || rm -f "$old"
        done < "$manifest"
    fi
    mkdir -p "$(dirname "$manifest")"
    printf '%s\n' "$files" > "$manifest"
    linux_refresh

    if [ -n "$existed" ]; then
        say "TMOP is updated: $bin"
    else
        say "TMOP is installed: $bin"
    fi
    say "Start it from your applications menu, or run: tmop"
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) say "Note: $bin_dir is not in your PATH, so the 'tmop' command needs the full path. The menu entry works either way." ;;
    esac
    say "Run this command again to update; add --uninstall to remove TMOP."
}

linux_uninstall() {
    linux_paths
    if [ ! -f "$manifest" ]; then
        say "TMOP was not installed by this script (there is no $manifest), so nothing was removed."
        return 0
    fi
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        rm -f "$file"
        say "Removed $file"
    done < "$manifest"
    rm -f "$manifest"
    linux_refresh
    say "TMOP is removed. Your settings stay in ${XDG_CONFIG_HOME:-$HOME/.config}/tmop and your recordings in $share/tmop; delete those folders to remove them too."
}

# The bundle id of the app at $1, empty if it has none.
bundle_id() {
    /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist" 2>/dev/null || true
}

macos_running() {
    command -v pgrep >/dev/null 2>&1 && pgrep -f "$1/Contents/MacOS/TMOP" >/dev/null 2>&1
}

macos_install() {
    major=$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)
    if [ -n "$major" ] && [ "$major" -lt 11 ] 2>/dev/null; then
        fail "TMOP needs macOS 11 (Big Sur) or newer; this Mac runs $(sw_vers -productVersion)."
    fi
    need ditto
    apps=$HOME/Applications
    app=$apps/TMOP.app
    if [ -e "$app" ] && [ "$(bundle_id "$app")" != "$BUNDLE_ID" ]; then
        fail "$app exists but is not TMOP; move it away and run this again."
    fi
    if macos_running "$app"; then
        fail "TMOP is running. Quit it (TMOP > Quit) and run this command again."
    fi
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/tmop-install.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' HUP INT TERM

    asset=TMOP-macos-universal.zip
    download "$asset" "$tmp"
    ditto -x -k "$tmp/$asset" "$tmp/x" || fail "could not unpack $asset."
    [ "$(bundle_id "$tmp/x/TMOP.app")" = "$BUNDLE_ID" ] || fail "$asset does not contain TMOP.app."
    # Belt and braces: nothing above sets the flag, but a proxy or a policy could.
    xattr -dr com.apple.quarantine "$tmp/x/TMOP.app" 2>/dev/null || true

    mkdir -p "$apps"
    existed=
    if [ -e "$app" ]; then
        existed=1
        rm -rf "$app"
    fi
    mv "$tmp/x/TMOP.app" "$app"

    if [ -n "$existed" ]; then
        say "TMOP is updated: $app"
    else
        say "TMOP is installed: $app"
    fi
    say "Start it from Launchpad or Spotlight, or run: open \"$app\""
    say "Run this command again to update; add --uninstall to remove TMOP."
}

macos_uninstall() {
    app=$HOME/Applications/TMOP.app
    if [ ! -e "$app" ]; then
        say "There is no $app, so nothing was removed."
        return 0
    fi
    [ "$(bundle_id "$app")" = "$BUNDLE_ID" ] || fail "$app is not TMOP; it was left alone."
    if macos_running "$app"; then
        fail "TMOP is running. Quit it (TMOP > Quit) and run this command again."
    fi
    rm -rf "$app"
    say "Removed $app"
    say "TMOP is removed. Your settings stay in ~/Library/Application Support/TMOP; delete that folder to remove them too."
}

main() {
    appimage=
    uninstall=
    for arg in "$@"; do
        case "$arg" in
            --appimage) appimage=1 ;;
            --uninstall) uninstall=1 ;;
            -h | --help) usage; return 0 ;;
            *) usage >&2; fail "unknown option: $arg" ;;
        esac
    done
    [ "$(id -u)" -ne 0 ] || fail "run this as your own user, not as root: TMOP installs into your home folder."
    if [ -z "${HOME:-}" ] || [ ! -d "$HOME" ]; then
        fail "\$HOME is not set to a folder."
    fi

    case "$(uname -s)" in
        Linux)
            if [ -n "$uninstall" ]; then linux_uninstall; else need curl; linux_install "$appimage"; fi
            ;;
        Darwin)
            [ -z "$appimage" ] || fail "--appimage is for Linux; on macOS TMOP installs as TMOP.app."
            if [ -n "$uninstall" ]; then macos_uninstall; else need curl; macos_install; fi
            ;;
        *)
            fail "this installer is for Linux and macOS. On Windows, see https://tekikaito.github.io/tmop/"
            ;;
    esac
}

main "$@"
