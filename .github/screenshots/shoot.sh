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
# The Processes page in tree view with the new/exited highlight (held 5 s). The settings live in a
# private folder of their own; two harmless `sleep` processes are staged so a green row (a process
# that started) and a red one (one that exited a moment ago) are really there at the shot.
home="${RUNNER_TEMP:-/tmp}/tmop-shot-tree"
case "$os" in
  windows) cfg="$home/TMOP" ;;
  macos) cfg="$home/Library/Application Support/TMOP" ;;
  *) cfg="$home/config/tmop" ;;
esac
mkdir -p "$cfg"
echo '{"version":1,"general":{"highlightProcesses":true,"highlightSeconds":5},"window":{"processTreeView":true}}' >"$cfg/settings.json"
( sleep 22; sleep 4 & sleep 4; sleep 60 & wait ) >/dev/null 2>&1 &
stage_pid=$!
shot processes --page=processes --shot-home="$home" --wait=30
kill $stage_pid 2>/dev/null
shot startup --page=startup
shot power --page=power
shot settings-alerts --page=settings --settings-section=alerts --wait=10
if [ "$os" = windows ]; then
  shot services --page=services
  shot settings-graphs --page=settings --settings-section=graphs --wait=10
fi
if [ "$os" = linux ]; then
  shot docker --page=docker --docker-tab=containers
  shot summary-light --page=summary --theme=light
fi
if [ "$os" = windows ] || [ "$os" = linux ]; then
  # Frames for the animation, two per second.
  shot anim --page=performance --device=cpu --frames=25 --every=500
fi

# The popout alone (--popout). These PNGs keep the framebuffer's alpha, and TMOP checks the
# corner pixels against the corners it chose (#166): alpha 0 when rounded, the window colour
# when square. It proves what the app renders, not what a compositor shows. TMOP_POPOUT_CORNERS
# forces the rounded drawing where this runner gets square corners (llvmpipe on Windows, Xvfb
# without a compositor), to check the transparent pixels there as well.
popout_failed=0
popout() {  # name, corners override (or ""), then TMOP arguments
  local name="$1" corners="$2"; shift 2
  echo "::group::$name"
  local log="$out/$name.log"
  TMOP_POPOUT_CORNERS="$corners" "$exe" --popout --screenshot="$out/$name.png" --wait=8 "$@" 2>&1 | tee "$log"
  if grep -q "popout check ok" "$log"; then
    echo "ok: $name"
  else
    echo "::error::popout corners check failed or missing: $name"
    popout_failed=1
  fi
  echo "::endgroup::"
}
popout popout-classic ""
popout popout-neon "" --popout-skin=com.tmop.neon
if [ "$os" != macos ]; then
  popout popout-classic-rounded rounded
  popout popout-neon-rounded rounded --popout-skin=com.tmop.neon
fi

# The minimum window size (900x600): lists keep their rows, the sidebar scrolls with arrows, the
# Processes toolbar wraps. Calls the exe directly: shot() puts its own --window-size first and the
# parser takes the first match.
shot_min() {  # name, then TMOP arguments
  local name="$1"; shift
  echo "::group::$name"
  if "$exe" --screenshot="$out/$name.png" --window-size=900x600 --wait="${SHOT_MIN_WAIT:-12}" "$@"; then
    echo "ok: $name"
  else
    echo "::warning::shot $name failed"
  fi
  echo "::endgroup::"
}
shot_min min-processes --page=processes
shot_min min-users --page=users
shot_min min-app-history --page=app-history
shot_min min-benchmarks --page=benchmarks
shot_min min-power --page=power-and-freq
shot system-info --page=system-info
shot installed-apps --page=installed-apps
[ "$os" = windows ] && shot_min min-services --page=services
[ "$os" = linux ] && shot_min min-docker-containers --page=docker --docker-tab=containers
ls -l "$out"
exit $popout_failed
