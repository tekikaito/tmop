#!/bin/sh
# Writes the Homebrew cask for a release to stdout:
#   packaging/macos/cask.sh <tag> <sha256 of TMOP-<tag>-macos-universal.zip>
# The release workflow publishes it to the tap tekikaito/homebrew-tmop, so that
#   brew install tekikaito/tmop/tmop
# installs TMOP and `brew upgrade` keeps it current.
set -eu
[ $# -eq 2 ] || { echo "usage: $0 <tag> <sha256>" >&2; exit 2; }
tag=$1
sha=$2
case $tag in v[0-9]*) ;; *) echo "tag must look like v1.2.3: $tag" >&2; exit 2 ;; esac
printf '%s' "$sha" | grep -Eq '^[0-9a-f]{64}$' || { echo "not a SHA-256: $sha" >&2; exit 2; }
version=${tag#v}
cat <<CASK
cask "tmop" do
  version "$version"
  sha256 "$sha"

  url "https://github.com/tekikaito/tmop/releases/download/v#{version}/TMOP-v#{version}-macos-universal.zip"
  name "TMOP"
  desc "Task Manager OverPowered: skinnable system monitor"
  homepage "https://tekikaito.github.io/tmop/"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: ">= :big_sur"

  app "TMOP.app"

  # TMOP is not notarized; like the install command, start it without the "could not verify" prompt.
  postflight do
    system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "#{appdir}/TMOP.app"], must_succeed: false
  end

  zap trash: "~/Library/Application Support/TMOP"
end
CASK
