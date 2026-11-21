#!/usr/bin/env bash
# Takes the README screenshots with TMOP's own --screenshot flag. Usage:
#   shoot.sh <os: windows|linux|macos> <TMOP executable> <output dir>
# The synthetic load runs in the background; a shot that fails is reported and skipped.
os="${1:?os}"; exe="${2:?exe}"; out="${3:?out}"
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$out"
scratch="${RUNNER_TEMP:-/tmp}/tmop-load"
bash "$here/load.sh" "$scratch" >/dev/null 2>&1 &
load_pid=$!
trap 'kill $load_pid 2>/dev/null' EXIT

WAIT="${SHOT_WAIT:-45}"
SIZE=1584x877
shot() {  # name, then TMOP arguments (a later --wait= wins)
  local name="$1"; shift
  echo "::group::$name"
  if "$exe" --screenshot="$out/$name.png" --window-size=$SIZE --wait="$WAIT" "$@"; then
    echo "ok: $name"
  else
    echo "::warning::shot $name failed"
  fi
  echo "::endgroup::"
}

shot summary --page=summary
shot performance-cpu --page=performance --device=cpu
shot performance-memory --page=performance --device=memory
shot performance-disk --page=performance --device=disk
shot processes --page=processes
if [ "$os" = windows ]; then
  shot services --page=services
  shot settings-graphs --page=settings --settings-section=graphs --wait=10
fi
if [ "$os" = linux ]; then
  shot docker --page=docker --docker-tab=containers
  shot summary-light --page=summary --theme=light
fi
if [ "$os" = windows ] || [ "$os" = linux ]; then
  # Frames for the animation, one per data update.
  shot anim --page=performance --device=cpu --frames=13 --every=1000
fi
ls -l "$out"
exit 0
