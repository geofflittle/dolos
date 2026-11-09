#!/bin/sh
# Runs the mutation judge tests once per variant of mutation.rs in /opt/build/mutation-1109/repro, each mounted over the tree's copy.
# Usage: repro.sh <variant...>, output in /opt/build/mutation-1109/logs/repro-<variant>.log.
set -u
D=/opt/build/mutation-1109
for v in "$@"; do
  touch "$D/repro/$v.rs"
  docker run --rm --name "mutation-1109-repro-$v" --memory 6g --memory-swap 6g \
    -v mutation-1109:/v -v "$D/dolos:/src" -v "$D/repro/$v.rs:/src/crates/ledger-oracle-judge/src/mutation.rs:ro" -w /src \
    -e CARGO_HOME=/v/cargo-home -e CARGO_TARGET_DIR=/v/target-judge -e CARGO_INCREMENTAL=0 \
    -e CARGO_BUILD_JOBS=2 -e CARGO_TERM_COLOR=never -e CARGO_NET_GIT_FETCH_WITH_CLI=true \
    dolos-build:1 sh -c 'export PATH=/usr/local/cargo/bin:$PATH; cargo test -p ledger-oracle-judge --lib mutation::' >"$D/logs/repro-$v.log" 2>&1
  echo "repro $v exit=$?" | tee -a "$D/logs/repro-$v.log"
done
