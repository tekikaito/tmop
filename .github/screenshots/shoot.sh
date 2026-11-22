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

# ---- QA pass: every page, at the usual size (1584x877) and at the smallest the window allows (900x600).
qa() {  # name, size, wait, then TMOP arguments
  local name="$1" size="$2" wait="$3"; shift 3
  echo "::group::$name"
  if "$exe" --screenshot="$out/$name.png" --window-size="$size" --wait="$wait" "$@"; then echo "ok: $name"; else echo "::warning::shot $name failed"; fi
  echo "::endgroup::"
}
qa_all() {  # suffix, size
  local sfx="$1" size="$2" w=15
  [ "$sfx" = min ] && w=12
  for p in summary processes docker system-info app-history startup-apps users services power-and-freq flight-recorder connections installed-apps drivers disk-space benchmarks; do
    local pw=$w
    case "$p" in installed-apps|drivers|disk-space|services|app-history) pw=$((w+10));; esac
    qa "qa-$sfx-page-$p" "$size" "$pw" --page="$p"
  done
  for d in cpu memory gpu disk network thermal docker; do
    qa "qa-$sfx-perf-$d" "$size" $((w+8)) --page=performance --device="$d"
  done
  for t in engine containers stacks images volumes networks; do
    qa "qa-$sfx-docker-$t" "$size" "$w" --page=docker --docker-tab="$t"
  done
  for sec in Appearance Graphs General Alerts "Window management" "Other options" "Task Manager" "Skins and files" About; do
    qa "qa-$sfx-settings-$(echo "$sec" | tr 'A-Z ' 'a-z-')" "$size" 8 --page=settings --settings-section="$sec"
  done
  qa "qa-$sfx-popout" "$size" "$w" --popout
}
qa_all full $SIZE
qa_all min 900x600
for p in summary processes settings; do
  qa "qa-light-$p" $SIZE 15 --page="$p" --theme=light
done
ls -l "$out"
exit 0
