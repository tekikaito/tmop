#!/usr/bin/env bash
# Builds the TMOP engine WSL2 rootfs for one architecture.
#
# Usage: engine/build.sh <tag> <x64|arm64> <out dir>
# Writes tmop-engine-<tag>-<arch>.tar.gz and its .sha256 into <out dir>.
# Runs on Linux CI and in Git Bash with Docker Desktop.
# DOCKER_CLI_TAG and COMPOSE_TAG override the pins in the Dockerfile when set.
set -euo pipefail

usage() {
    echo "usage: engine/build.sh <tag> <x64|arm64> <out dir>" >&2
    exit 2
}

[ $# -eq 3 ] || usage
tag=$1
arch=$2
out=$3

case "$tag" in
    v[0-9]*) ;;
    *) echo "tag must look like v0.1.0, got: $tag" >&2; exit 2 ;;
esac
case "$arch" in
    x64) platform=linux/amd64 ;;
    arm64) platform=linux/arm64 ;;
    *) usage ;;
esac

engine_dir=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
name=tmop-engine-$tag-$arch
tar_file=$out/$name.tar

# Git Bash paths are not understood by the Windows docker CLI, so convert them.
if command -v cygpath >/dev/null 2>&1; then
    dest=$(cygpath -m "$tar_file")
else
    dest=$tar_file
fi

build_args=(--build-arg "TMOP_TAG=$tag")
if [ -n "${DOCKER_CLI_TAG:-}" ]; then
    build_args+=(--build-arg "DOCKER_CLI_TAG=$DOCKER_CLI_TAG")
fi
if [ -n "${COMPOSE_TAG:-}" ]; then
    build_args+=(--build-arg "COMPOSE_TAG=$COMPOSE_TAG")
fi

docker buildx build \
    --platform "$platform" \
    "${build_args[@]}" \
    --output "type=tar,dest=$dest" \
    -f "$engine_dir/Dockerfile" \
    "$engine_dir"

# gzip -n drops the timestamp and name so the output is reproducible. -f replaces an old file.
gzip -n -9 -f "$tar_file"
# Written as "<hex>  <file>" (two spaces, no binary-mode marker) so it works with sha256sum -c.
(cd "$out" && hash=$(sha256sum "$name.tar.gz" | awk '{ print $1 }') && printf '%s  %s\n' "$hash" "$name.tar.gz" > "$name.tar.gz.sha256")

echo "built $out/$name.tar.gz ($(wc -c < "$out/$name.tar.gz") bytes)"
