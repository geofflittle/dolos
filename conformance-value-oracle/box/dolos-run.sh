#!/bin/sh
# Runs one command in the Dolos build image against a tree under W, with the pinned toolchain installed under W.
# usage: [W=dir] [TARGET=name] dolos-run.sh TREE COMMAND...
W=${W:-/opt/build/value-oracle-1025}
TREE=$1
shift
exec docker run --rm \
  --user "$(id -u):$(id -g)" \
  -v "$W:/w" \
  -e HOME=/w/home \
  -e CARGO_HOME=/w/dolos-cargo-home \
  -e RUSTUP_HOME=/w/rustup \
  -e PATH=/w/dolos-cargo-home/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  -e CARGO_TARGET_DIR="/w/${TARGET:-dolos-target}" \
  -e CARGO_INCREMENTAL=0 \
  -e CARGO_BUILD_JOBS=4 \
  -e CARGO_TERM_COLOR=never \
  -e RUST_BACKTRACE=short \
  -w "/w/$TREE" \
  dolos-build:1 \
  "$@"
