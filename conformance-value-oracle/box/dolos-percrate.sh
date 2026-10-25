#!/bin/sh
# Runs the Dolos tests one package at a time in one target dir, refusing to start below MINFREE GB free.
# The target dir is deleted before a package whenever free space is below MINFREE, and at the end.
# A watchdog kills the build container when free space falls below KILLFREE GB.
# usage: [MINFREE=30] [KILLFREE=12] sh dolos-percrate.sh TREE LOG PACKAGE...
set -u
W=/opt/build/value-oracle-1025
TREE=$1
LOG=$2
shift 2
MINFREE=${MINFREE:-30}
KILLFREE=${KILLFREE:-12}
T=target-dolos-pc
avail() { df -BG --output=avail / | tail -n 1 | tr -dc 0-9; }
(
  while [ -e "$W/$LOG.running" ]; do
    if [ "$(avail)" -lt "$KILLFREE" ]; then
      echo "WATCHDOG free $(avail)G below ${KILLFREE}G, killing the build" >> "$W/$LOG"
      docker ps -q --filter name=dolos-percrate | xargs -r docker kill
    fi
    sleep 5
  done
) &
touch "$W/$LOG.running"
: > "$W/$LOG"
for p in "$@"; do
  if [ "$(avail)" -lt "$MINFREE" ]; then
    rm -rf "${W:?}/$T"
  fi
  if [ "$(avail)" -lt "$MINFREE" ]; then
    echo "PACKAGE $p NOT STARTED free $(avail)G below ${MINFREE}G" >> "$W/$LOG"
    continue
  fi
  echo "PACKAGE $p start free $(avail)G" >> "$W/$LOG"
  docker run --rm --name dolos-percrate \
    --user "$(id -u):$(id -g)" \
    -v "$W:/w" \
    -e HOME=/w/home \
    -e CARGO_HOME=/w/dolos-cargo-home \
    -e RUSTUP_HOME=/w/rustup \
    -e PATH=/w/dolos-cargo-home/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    -e CARGO_TARGET_DIR="/w/$T" \
    -e CARGO_INCREMENTAL=0 \
    -e CARGO_BUILD_JOBS=4 \
    -e CARGO_PROFILE_DEV_DEBUG=0 \
    -e CARGO_PROFILE_TEST_DEBUG=0 \
    -e CARGO_TERM_COLOR=never \
    -w "/w/$TREE" \
    dolos-build:1 \
    cargo test --locked -p "$p" > "$W/$LOG.$p" 2>&1
  rc=$?
  echo "PACKAGE $p EXIT $rc free $(avail)G target $(du -sh "$W/$T" 2>/dev/null | cut -f1)" >> "$W/$LOG"
  /usr/bin/grep -E "^test result:" "$W/$LOG.$p" | awk -v p="$p" '{s+=$4; f+=$6; i+=$8; n++} END {print "PACKAGE " p " suites " n " passed " s " failed " f " ignored " i}' >> "$W/$LOG"
done
rm -rf "${W:?}/$T"
rm -f "$W/$LOG.running"
echo "DONE free $(avail)G" >> "$W/$LOG"
