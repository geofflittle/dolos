#!/bin/sh
# Put the Musashi Leios sync back on a checkpoint taken by checkpoint.sh.
#
# Usage: rewind.sh <tip-slot>
#
# The store that was in place is moved aside rather than deleted, because it is
# the evidence for whatever stopped the sync, and the checkpoint is copied
# rather than moved, so the same checkpoint can be used again if the first
# attempt walks into the same wall.
set -eu

RUN=/opt/build/run
DATA=$RUN/data
CKPT=$RUN/checkpoints
CONTAINER=dolos-sync
LOG=$RUN/checkpoint.log
LOCK=$RUN/checkpoint.lock
DISKGUARD=$RUN/diskguard.sh
STOP_TIMEOUT=120

say() { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') $*"; }
logline() { say "$*" | tee -a "$LOG"; }
die() { say "REFUSED $*" >&2; exit 2; }

measure() { find "$1" -type f -printf '%s\n' | awk '{n++; s+=$1} END {printf "%d %d\n", n+0, s+0}'; }

ensure_diskguard() {
  if pgrep -f "$DISKGUARD" >/dev/null 2>&1; then
    say "disk guard still running"
  else
    nohup sh "$DISKGUARD" >/dev/null 2>&1 &
    say "disk guard was not running, restarted as pid $!"
  fi
}

SLOT=${1:-}
case "$SLOT" in
  ''|*[!0-9]*) die "usage: rewind.sh <tip-slot>, one of $(ls -1 "$CKPT" 2>/dev/null | grep -E '^[0-9]+$' | sort -n | tr '\n' ' ')" ;;
esac

SRC=$CKPT/$SLOT
[ -d "$SRC" ] || die "$SRC is not a directory"
[ -d "$SRC/state" ] || die "$SRC/state is not a directory, that checkpoint is not a Dolos store"
[ -f "$SRC/wal" ] || die "$SRC/wal is not a file, that checkpoint is not a Dolos store"
[ -d "$RUN" ] || die "$RUN is not a directory"
[ -d "$DATA" ] || die "$DATA is not a directory"
[ -x "$DISKGUARD" ] || die "$DISKGUARD is not executable"
docker inspect "$CONTAINER" >/dev/null 2>&1 || die "container $CONTAINER does not exist"

if ! mkdir "$LOCK" 2>/dev/null; then
  die "$LOCK exists, a checkpoint or rewind is in progress"
fi
trap 'rmdir "$LOCK" 2>/dev/null || true' EXIT INT TERM

read -r FILES BYTES <<EOF
$(measure "$SRC")
EOF
[ "$FILES" -gt 0 ] || die "$SRC holds no files"

SRC_KB=$(du -sk "$SRC" | cut -f1)
FREE_KB=$(df -k --output=avail "$RUN" | tail -1)
[ $((FREE_KB - SRC_KB)) -gt 0 ] || die "restoring $SRC_KB K would fill the filesystem, $FREE_KB K free"

STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
BROKEN=$RUN/data.broken.$STAMP

STOPPED_AT=$(date +%s)
say "stopping $CONTAINER"
docker stop -t "$STOP_TIMEOUT" "$CONTAINER" >/dev/null
running=$(docker inspect -f '{{.State.Running}}' "$CONTAINER")
[ "$running" = false ] || die "$CONTAINER is still running after docker stop"

mv "$DATA" "$BROKEN"
say "moved the store that was in place to $BROKEN"

cp -a "$SRC" "$DATA"
read -r RFILES RBYTES <<EOF
$(measure "$DATA")
EOF
if [ "$RFILES" != "$FILES" ] || [ "$RBYTES" != "$BYTES" ]; then
  say "restored store holds $RFILES files and $RBYTES bytes, checkpoint held $FILES and $BYTES"
  rm -rf "$DATA"
  mv "$BROKEN" "$DATA"
  docker start "$CONTAINER" >/dev/null
  ensure_diskguard
  logline "rewind to $SLOT FAILED, the copy was incomplete, the previous store was put back and the container restarted"
  exit 5
fi

say "restored $FILES files and $BYTES bytes from $SRC"
say "starting $CONTAINER"
docker start "$CONTAINER" >/dev/null
running=$(docker inspect -f '{{.State.Running}}' "$CONTAINER")
[ "$running" = true ] || die "$CONTAINER did not start again"
STOP_SECONDS=$(($(date +%s) - STOPPED_AT))
ensure_diskguard

logline "rewind to $SLOT  $BYTES bytes in $FILES files  container stopped $STOP_SECONDS s  previous store kept at $BROKEN"
say "the checkpoint at $SRC was copied and is still there, so this rewind can be repeated"
