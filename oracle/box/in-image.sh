#!/bin/sh
# Runs one command in the Dolos build image against a tree under the build root.
# Usage: [TARGET=name] [MEMORY=size] in-image.sh <tree dir under root> <command...>
# The root holds HOME, CARGO_HOME, RUSTUP_HOME and every target dir.
# The container is held to MEMORY, so the kernel's out of memory kill ends a process of the container.
set -eu
ROOT=${ROOT:-/opt/build/ledger-oracle-1027}
MEMORY=${MEMORY:-6g}
TREE=$1
shift
exec docker run --rm --log-driver none --memory "$MEMORY" --memory-swap "$MEMORY" \
  --user "$(id -u):$(id -g)" -v "$ROOT:/w" \
  -e HOME=/w/home -e CARGO_HOME=/w/cargo-home -e RUSTUP_HOME=/w/rustup \
  -e GIT_CONFIG_GLOBAL=/w/home/gitconfig -e CARGO_NET_GIT_FETCH_WITH_CLI=true \
  -e CARGO_TARGET_DIR="/w/${TARGET:-target}" -e CARGO_INCREMENTAL=0 \
  -e CARGO_BUILD_JOBS=3 -e CARGO_PROFILE_DEV_DEBUG=0 -e CARGO_PROFILE_TEST_DEBUG=0 \
  -e CARGO_PROFILE_RELEASE_DEBUG=0 -e CARGO_TERM_COLOR=never -e RUST_BACKTRACE=short \
  ${LIVE_DIR:+-e "LIVE_DIR=$LIVE_DIR"} \
  -w "/w/$TREE" dolos-build:1 "$@"
