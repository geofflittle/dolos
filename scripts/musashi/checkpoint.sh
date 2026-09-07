#!/bin/sh
# Take a restorable copy of the Musashi Leios sync's Dolos store.
#
# The sync walks the chain from origin over many hours. When a ledger rule
# defect stops it, the stored state is often past the last point the chain can
# be resumed from, and the only remedy so far has been a fresh sync from
# origin, which costs about twelve hours. A checkpoint is a whole copy of the
# data directory taken while the container is stopped, so a stop costs minutes
# instead.
#
# Usage:
#   checkpoint.sh              take a checkpoint now
#   checkpoint.sh --if-crossed take one only if the sync has crossed the next
#                              ten percent of the chain since the newest one
#
# Every path this touches is checked before anything is stopped or copied, and
# a refusal names the path that was wrong. Nothing here reports success by the
# absence of an error.
set -eu

RUN=/opt/build/run
DATA=$RUN/data
CKPT=$RUN/checkpoints
CONFIG=$RUN/dolos-origin.toml
CONTAINER=dolos-sync
IMAGE=dolos-build:1
LOG=$RUN/checkpoint.log
LOCK=$RUN/checkpoint.lock
DISKGUARD=$RUN/diskguard.sh
KEEP=4
FLOOR_KB=15728640       # 15 GB that must still be free after the copy
STOP_TIMEOUT=120        # seconds given to the sync to shut down cleanly
RESUME_TIMEOUT=600      # seconds waited for the sync to be applying again
TIP_SLOT=2595923        # tip of the node's own immutable db, from tipslot.py
# One checkpoint per ten percent of the chain. Overridable so that the branch
# which decides a step has been crossed can be exercised on demand, rather than
# only by waiting the hour or so it takes the sync to cover ten percent.
STEP=${CHECKPOINT_STEP:-$((TIP_SLOT / 10))}

IF_CROSSED=no
case "${1:-}" in
  "") ;;
  --if-crossed) IF_CROSSED=yes ;;
  *) echo "checkpoint.sh: unknown argument $1" >&2; exit 2 ;;
esac

say() { echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') $*"; }
logline() { say "$*" | tee -a "$LOG"; }
die() { say "REFUSED $*" >&2; exit 2; }

# Sum of every regular file's size and the count of them. Two directories that
# agree on both held the same files, which is what makes a copy complete rather
# than merely finished.
measure() { find "$1" -type f -printf '%s\n' | awk '{n++; s+=$1} END {printf "%d %d\n", n+0, s+0}'; }

# Waits until the apply stage says it is up again. The container being up is not
# the sync being up: a restart replays the write ahead log journals before it
# logs anything at all, and that replay has been measured at ninety seconds on a
# store this size, which is most of what a checkpoint really costs.
wait_for_resume() {
  since=$1
  waited=0
  while [ "$waited" -lt "$RESUME_TIMEOUT" ]; do
    if docker logs --since "$since" "$CONTAINER" 2>&1 |
        sed 's/\x1b\[[0-9;]*[A-Za-z]//g' |
        grep -qF 'stage{stage="apply"}: gasket::runtime: stage bootstrap ok'; then
      return 0
    fi
    sleep 5
    waited=$((waited + 5))
  done
  return 1
}

ensure_diskguard() {
  if pgrep -f "sh $DISKGUARD" >/dev/null 2>&1 || pgrep -f "$DISKGUARD" >/dev/null 2>&1; then
    say "disk guard still running"
  else
    nohup sh "$DISKGUARD" >/dev/null 2>&1 &
    say "disk guard was not running, restarted as pid $!"
  fi
}

# Refusals, all of them before anything is stopped.
[ -d "$RUN" ] || die "$RUN is not a directory"
[ -d "$DATA" ] || die "$DATA is not a directory"
[ -d "$DATA/state" ] || die "$DATA/state is not a directory, this is not a Dolos store"
[ -f "$DATA/wal" ] || die "$DATA/wal is not a file, this is not a Dolos store"
[ -r "$CONFIG" ] || die "$CONFIG is not readable"
grep -q '^path = "data"$' "$CONFIG" || die "$CONFIG does not point storage at data"
[ -x "$DISKGUARD" ] || die "$DISKGUARD is not executable"
docker inspect "$CONTAINER" >/dev/null 2>&1 || die "container $CONTAINER does not exist"
docker image inspect "$IMAGE" >/dev/null 2>&1 || die "image $IMAGE does not exist"
mkdir -p "$CKPT" || die "cannot create $CKPT"

if ! mkdir "$LOCK" 2>/dev/null; then
  die "$LOCK exists, another checkpoint is in progress"
fi
trap 'rmdir "$LOCK" 2>/dev/null || true' EXIT INT TERM

newest_checkpoint() {
  ls -1 "$CKPT" 2>/dev/null | grep -E '^[0-9]+$' | sort -n | tail -1
}

# The apply stage's own progress, read from the log. The line that names a slot
# when an endorser block is resolved is written by the pull stage, which runs
# ahead of apply by the depth of the queue, and reading it as the sync's
# position is the mistake that misled one diagnosis already. The write ahead
# log's pruning line is written by the apply stage and carries the cutoff it
# pruned to, which trails the applied tip by the retention window, so the
# applied tip is at least cutoff plus retention. That is a lower bound and it
# is used as one.
apply_tip_lower_bound() {
  # The log carries terminal colour codes in the middle of every field name, so
  # cutoff_slot= is not a literal substring of a raw line and stripping them
  # first is what makes these two numbers findable at all.
  logs=$(docker logs --since 60m "$CONTAINER" 2>&1 |
         tr '\r' '\n' | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' || true)
  cutoff=$(echo "$logs" | grep -o 'cutoff_slot=[0-9]*' | tail -1 | cut -d= -f2)
  window=$(echo "$logs" | grep -o 'max_slots=[0-9]*' | tail -1 | cut -d= -f2)
  if [ -z "$cutoff" ] || [ -z "$window" ]; then
    return 1
  fi
  echo $((cutoff + window))
}

if [ "$IF_CROSSED" = yes ]; then
  if ! bound=$(apply_tip_lower_bound); then
    logline "no checkpoint: the last hour of log carried no wal pruning line, so the applied tip could not be read"
    exit 3
  fi
  have=$(newest_checkpoint)
  if [ -z "$have" ]; then
    say "no checkpoint exists yet, applied tip at least $bound, taking the first one"
  elif [ $((bound / STEP)) -gt $((have / STEP)) ]; then
    say "applied tip at least $bound, newest checkpoint $have, crossed ten percent step $STEP"
  else
    say "no checkpoint: applied tip at least $bound is in the same ten percent step as checkpoint $have"
    exit 0
  fi
fi

# Size for the disk check only, taken while the sync still runs, so it is an
# estimate of what the copy will cost rather than the size of anything. The
# store the copy is compared against is measured after the container is down,
# because a running sync rotates journal files under the measurement.
DATA_KB=$(du -sk "$DATA" | cut -f1)
FREE_KB=$(df -k --output=avail "$RUN" | tail -1)
AFTER_KB=$((FREE_KB - DATA_KB))
if [ "$AFTER_KB" -lt "$FLOOR_KB" ]; then
  logline "no checkpoint: about $DATA_KB K copied would leave about $AFTER_KB K free, below the floor of $FLOOR_KB K"
  exit 4
fi
say "store is about $DATA_KB K, about $AFTER_KB K would remain free"

INCOMING=$CKPT/incoming.$$
rm -rf "$INCOMING"

STOPPED_AT=$(date +%s)
say "stopping $CONTAINER"
docker stop -t "$STOP_TIMEOUT" "$CONTAINER" >/dev/null
running=$(docker inspect -f '{{.State.Running}}' "$CONTAINER")
[ "$running" = false ] || die "$CONTAINER is still running after docker stop"
exitcode=$(docker inspect -f '{{.State.ExitCode}}' "$CONTAINER")
say "$CONTAINER stopped, exit code $exitcode"

read -r FILES BYTES <<EOF
$(measure "$DATA")
EOF

copy_failed=""
[ "$FILES" -gt 0 ] || copy_failed="$DATA holds no files"
if [ -z "$copy_failed" ]; then
  cp -a "$DATA" "$INCOMING" || copy_failed="cp -a failed"
fi
if [ -z "$copy_failed" ]; then
  read -r CFILES CBYTES <<EOF
$(measure "$INCOMING")
EOF
  if [ "$CFILES" != "$FILES" ] || [ "$CBYTES" != "$BYTES" ]; then
    copy_failed="copy holds $CFILES files and $CBYTES bytes, source held $FILES and $BYTES"
  fi
fi

say "starting $CONTAINER"
SINCE=$(date -u '+%Y-%m-%dT%H:%M:%S')
docker start "$CONTAINER" >/dev/null
running=$(docker inspect -f '{{.State.Running}}' "$CONTAINER")
[ "$running" = true ] || die "$CONTAINER did not start again"
STARTED_AT=$(date +%s)
STOP_SECONDS=$((STARTED_AT - STOPPED_AT))
ensure_diskguard

if [ -n "$copy_failed" ]; then
  logline "checkpoint FAILED after $STOP_SECONDS s stopped: $copy_failed, incomplete copy left at $INCOMING"
  exit 5
fi

# The tip is read from the copy rather than from the live store. Opening a
# Dolos store writes to it, measured as a changed redb file after a plain data
# summary, so opening the live one would make a read of the sync's position a
# write to the sync's data, and it would add the open to the time the container
# is down. The copy is the same bytes and answers the same question.
INCOMING_REL="checkpoints/incoming.$$"
TIPCONF=$CKPT/incoming.$$.toml
sed "s|^path = \"data\"\$|path = \"$INCOMING_REL\"|" "$CONFIG" > "$TIPCONF"
grep -q "^path = \"$INCOMING_REL\"\$" "$TIPCONF" || die "could not point a config at $INCOMING_REL"
SUMMARY=$(docker run --rm --network none \
  -v /opt/build:/bins:ro -v "$RUN":/run-dolos -w /run-dolos \
  --entrypoint /bins/dolos.final "$IMAGE" \
  -c "checkpoints/incoming.$$.toml" data summary 2>&1) || true
rm -f "$TIPCONF"
TIP=$(echo "$SUMMARY" | tr -d ' ,' | grep -A1 '"state"' | grep '"tip_slot"' | cut -d: -f2)
case "$TIP" in
  ''|*[!0-9]*)
    KEPT=$CKPT/incoming-$(date -u '+%Y%m%dT%H%M%SZ')
    mv "$INCOMING" "$KEPT"
    logline "checkpoint UNNAMED after $STOP_SECONDS s stopped: the store's tip slot could not be read, copy kept at $KEPT"
    say "data summary said: $SUMMARY"
    exit 6
    ;;
esac

# Reading the tip opened the copy, and opening a Dolos store replays its write
# ahead log journals into tables and rewrites them, so the copy on disk is no
# longer the bytes that were checked against the source. Both sizes are logged,
# because reporting only the checked one would describe something that is not
# there. Storing the recovered form is worth having: it is the replay a restore
# would otherwise pay for, done once here instead.
read -r RFILES RBYTES <<EOF
$(measure "$INCOMING")
EOF

if [ -d "$CKPT/$TIP" ]; then
  rm -rf "$CKPT/$TIP.superseded"
  mv "$CKPT/$TIP" "$CKPT/$TIP.superseded"
fi
mv "$INCOMING" "$CKPT/$TIP"
rm -rf "$CKPT/$TIP.superseded"

# The copy landed in the page cache and returned long before the disk had it.
# Flushing here rather than before the container was started keeps the cost off
# the stop window and covers the tip read's rewrite as well.
sync

if wait_for_resume "$SINCE"; then
  RESUME_NOTE="sync applying again $(($(date +%s) - STOPPED_AT)) s after the stop"
else
  RESUME_NOTE="the apply stage did not report bootstrap ok within $RESUME_TIMEOUT s"
fi

logline "checkpoint $TIP  $RBYTES bytes in $RFILES files after the tip read recovered it, $BYTES in $FILES copied and checked  container stopped $STOP_SECONDS s  $RESUME_NOTE"

# Keep the newest few and say which ones went, so a shrinking checkpoints
# directory is never a silent one.
for old in $(ls -1 "$CKPT" | grep -E '^[0-9]+$' | sort -n | head -n -"$KEEP"); do
  rm -rf "${CKPT:?}/$old"
  logline "removed checkpoint $old, keeping the newest $KEEP"
done
