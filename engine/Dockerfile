# syntax=docker/dockerfile:1
#
# TMOP engine WSL2 rootfs. Built by engine/build.sh; the final stage is exported as a tar.
# Build context is the engine/ directory.
#
# Base images are pinned by the digest of their multi-arch index. Tool versions and the
# docker/cli source hash are the defaults below; build.sh only overrides them when set.

ARG DOCKER_CLI_TAG=v28.5.2
ARG DOCKER_CLI_SHA256=5864ccdfceef3a9e8ddb29b4a555d5b82a551b5258067aac164b6156c6cfdc08
ARG COMPOSE_TAG=v2.40.3

# Cross-compiles the Windows Docker CLI and fetches the Windows compose plugin.
# Runs on the build machine's native platform and targets Windows for TARGETARCH.
FROM --platform=$BUILDPLATFORM golang:1.24-alpine@sha256:8bee1901f1e530bfb4a7850aa7a479d17ae3a18beb6e09064ed54cfd245b7191 AS wincli
ARG TARGETARCH
ARG DOCKER_CLI_TAG
ARG DOCKER_CLI_SHA256
ARG COMPOSE_TAG
WORKDIR /src
RUN apk add --no-cache ca-certificates \
 && wget -qO /tmp/docker-cli.tar.gz "https://github.com/docker/cli/archive/refs/tags/${DOCKER_CLI_TAG}.tar.gz" \
 && echo "${DOCKER_CLI_SHA256}  /tmp/docker-cli.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/docker-cli.tar.gz --strip-components=1 \
 && rm -f /tmp/docker-cli.tar.gz \
 && mkdir -p /out \
 && cp vendor.mod go.mod \
 && CGO_ENABLED=0 GOOS=windows GOARCH="${TARGETARCH}" go build -mod=vendor -modfile=vendor.mod -trimpath \
      -ldflags "-s -w -X github.com/docker/cli/cli/version.Version=${DOCKER_CLI_TAG#v}" \
      -o /out/docker.exe ./cmd/docker
RUN case "${TARGETARCH}" in amd64) cpu=x86_64;; arm64) cpu=aarch64;; *) echo "unsupported arch" >&2; exit 1;; esac \
 && f="docker-compose-windows-${cpu}.exe" \
 && base="https://github.com/docker/compose/releases/download/${COMPOSE_TAG}" \
 && wget -qO /out/docker-compose.exe "${base}/${f}" \
 && wget -qO /tmp/expected.sha256 "${base}/${f}.sha256" \
 && echo "$(awk '{print $1}' /tmp/expected.sha256)  /out/docker-compose.exe" | sha256sum -c -

FROM alpine:3.21@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507
ARG TARGETARCH
ARG TMOP_TAG=dev

RUN apk add --no-cache docker-engine docker-cli docker-cli-compose docker-cli-buildx \
      containerd runc socat iptables ip6tables ca-certificates

COPY rootfs/ /
RUN chmod 0755 /usr/local/bin/tmop-engine

# Windows CLI binaries, one directory per architecture.
COPY --from=wincli /out/ /tmp/wincli/
RUN case "${TARGETARCH}" in amd64) arch=x64;; arm64) arch=arm64;; *) echo "unsupported arch" >&2; exit 1;; esac \
 && mkdir -p "/usr/share/tmop/windows/${arch}" \
 && mv /tmp/wincli/docker.exe /tmp/wincli/docker-compose.exe "/usr/share/tmop/windows/${arch}/" \
 && rm -rf /tmp/wincli

# Version, persisted limits, default shell for root and the directories the daemons need.
RUN mkdir -p /etc/tmop /var/lib/docker /run /var/log/tmop \
 && printf '%s\n' "${TMOP_TAG}" > /etc/tmop/version \
 && printf 'CPUS=0\nMEMORY_MB=0\n' > /etc/tmop/limits \
 && sed -i '/^root:/s|:[^:]*$|:/bin/sh|' /etc/passwd \
 && grep -q '^root:.*:/bin/sh$' /etc/passwd \
 && test -f /etc/wsl.conf
