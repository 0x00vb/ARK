#!/bin/sh
# A Linux build of one of ARK's programs for a person's server (ADR 0052), for this computer's
# own architecture, on Debian 11 (glibc 2.31, as Ubuntu 20.04):
#
#   scripts/linux-build.sh PACKAGE BIN      e.g. scripts/linux-build.sh writing-agent writer
#
# Writes target/linux/BIN-<arch>-unknown-linux-gnu. The build cache stays in the Docker volume
# `ark-linux`; `docker volume rm ark-linux` removes it. Needs Docker.
set -eu
cd "$(dirname "$0")/.."
pkg="${1:?usage: scripts/linux-build.sh PACKAGE BIN}"
bin="${2:?usage: scripts/linux-build.sh PACKAGE BIN}"
out="$PWD/target/linux"
mkdir -p "$out"
docker run --rm \
  -v "$PWD:/src:ro" -v "$out:/out" -v ark-linux:/cache \
  -e CARGO_HOME=/cache/cargo -e CARGO_TARGET_DIR=/cache/target \
  -e PKG="$pkg" -e BIN="$bin" \
  rust:1-bullseye sh -eu -c '
    cd /src
    cargo build --release --locked -p "$PKG" --bin "$BIN"
    arch="$(uname -m)"
    cp "/cache/target/release/$BIN" "/out/$BIN-$arch-unknown-linux-gnu"
    echo "built /out/$BIN-$arch-unknown-linux-gnu"
  '
