#!/usr/bin/env bash
# A harmless synthetic load so TMOP's graphs move while the README screenshots are taken:
# compression with varying intensity, disk writes in a temp folder, downloads from github.com.
# Runs until it is killed. Usage: load.sh <scratch dir>
dir="${1:?scratch dir}"
mkdir -p "$dir"
i=0
while true; do
  i=$((i + 1))
  # 0..3 compressors for a few seconds, then a pause: the CPU graph rises and falls.
  jobs_n=$((i % 3))
  for _ in $(seq "$jobs_n"); do
    (head -c $((60 + (i % 5) * 40))000000 /dev/urandom | gzip -6 >/dev/null) &
  done
  # Disk: write and read back a file of varying size.
  head -c $((30 + (i % 4) * 60))000000 /dev/urandom >"$dir/blob.bin" 2>/dev/null
  cat "$dir/blob.bin" >/dev/null 2>&1
  rm -f "$dir/blob.bin"
  # Network: public downloads from github.com.
  if [ $((i % 2)) -eq 0 ]; then
    curl -sSL --max-time 20 -o /dev/null "https://github.com/pal1000/mesa-dist-win/archive/refs/tags/26.2.7.zip" &
  else
    curl -sSL --max-time 20 -o /dev/null "https://github.com/tekikaito/tmop/archive/refs/heads/main.zip" &
  fi
  sleep $((2 + i % 3))
  wait
done
