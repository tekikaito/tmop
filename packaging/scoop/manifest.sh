#!/bin/sh
# Writes the Scoop manifest (bucket/tmop.json) for a release to stdout:
#   packaging/scoop/manifest.sh <tag> <sha256 of the windows-x64 zip> <sha256 of the windows-arm64 zip>
# The release workflow commits the output to bucket/tmop.json, so that
#   scoop bucket add tmop https://github.com/tekikaito/tmop && scoop install tmop
# installs the newest release. Scoop's own `checkver`/`autoupdate` keep working on the result.
set -eu
[ $# -eq 3 ] || { echo "usage: $0 <tag> <sha256 x64> <sha256 arm64>" >&2; exit 2; }
tag=$1
x64=$2
arm64=$3
case $tag in v[0-9]*) ;; *) echo "tag must look like v1.2.3: $tag" >&2; exit 2 ;; esac
for sha in "$x64" "$arm64"; do
    printf '%s' "$sha" | grep -Eq '^[0-9a-f]{64}$' || { echo "not a SHA-256: $sha" >&2; exit 2; }
done
version=${tag#v}
base=https://github.com/tekikaito/tmop/releases/download
cat <<MANIFEST
{
    "version": "$version",
    "description": "Task Manager OverPowered: a skinnable task manager and system monitor",
    "homepage": "https://tekikaito.github.io/tmop/",
    "license": "GPL-3.0-only",
    "notes": [
        "TMOP keeps its settings in %LOCALAPPDATA%\\\\TMOP, so they survive updates and uninstalls.",
        "TMOP is not code-signed; Windows SmartScreen may warn the first time it starts."
    ],
    "architecture": {
        "64bit": {
            "url": "$base/$tag/TMOP-$tag-windows-x64.zip",
            "hash": "$x64"
        },
        "arm64": {
            "url": "$base/$tag/TMOP-$tag-windows-arm64.zip",
            "hash": "$arm64"
        }
    },
    "bin": "TMOP.exe",
    "shortcuts": [
        [
            "TMOP.exe",
            "TMOP"
        ]
    ],
    "checkver": {
        "github": "https://github.com/tekikaito/tmop"
    },
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "$base/v\$version/TMOP-v\$version-windows-x64.zip"
            },
            "arm64": {
                "url": "$base/v\$version/TMOP-v\$version-windows-arm64.zip"
            }
        },
        "hash": {
            "url": "\$url.sha256"
        }
    }
}
MANIFEST
