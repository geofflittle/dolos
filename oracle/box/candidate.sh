#!/bin/sh
# Syncs a candidate Dolos from the node beside the serving one, reads both and the node, prints each field the two serve differently, and removes the candidate.
# Usage: CANDIDATE=<binary path or image reference> COMMIT=sha candidate.sh <dir>
# Reads ROOT, RUN_LOCK, NODE, NODE_DB, ENDPOINT, SERVE_CONFIG, CANDIDATE_GRPC, CANDIDATE_NEED_G, CANDIDATE_FSYNC, the IO_ limits, SAMPLE_MINUTES and GRPCURL from the environment.
set -eu
DIR=$1
: "${CANDIDATE:?}" "${COMMIT:?}" "${ROOT:?}" "${NODE:?}" "${ENDPOINT:?}" "${SERVE_CONFIG:?}"
: "${CANDIDATE_GRPC:?}" "${CANDIDATE_NEED_G:?}"
NEED_G=$CANDIDATE_NEED_G
HERE=$(cd "$(dirname "$0")" && pwd)
NAME=${CANDIDATE_NAME:-dolos-candidate-1028}
SYNC_MINUTES=${SYNC_MINUTES:-150}
SAMPLE_MINUTES=${SAMPLE_MINUTES:-5}
FIXED_SLOTS="205422 368793 783572"
STORE="$ROOT/candidate/$(date -u +%Y%m%dT%H%M%SZ)"
. "$HERE/lib.sh"
enter_slice "$0" "$@"
take_lock

FSYNC=$(candidate_fsync) || exit 2
mkdir -p "$DIR/config"
write_config "$DIR/config"

if [ "$(free_g "$ROOT")" -lt $((NEED_G + 3)) ] || [ "$(free_g /)" -lt 10 ]; then
  echo "LIVE-CANDIDATE REFUSED free=$(free_g "$ROOT")G root_free=$(free_g /)G need=$((NEED_G + 3))G"
  exit 2
fi

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  rm -rf "$STORE"
  echo "LIVE-CANDIDATE removed name=$NAME store=$STORE"
  release_lock
}
trap cleanup EXIT

mkdir -p "$STORE"
cp "$DIR/config/"* "$STORE/"

case $CANDIDATE in
  /*)
    cp "$CANDIDATE" "$STORE/dolos-candidate"
    IMAGE=$(sh "$HERE/image-name.sh" dolos-build)
    ENTRY="--entrypoint /run-dolos/dolos-candidate"
    ;;
  *)
    IMAGE=$CANDIDATE
    ENTRY=
    ;;
esac
PRELOAD=
if [ "$FSYNC" = off ]; then
  PRELOAD="-v /usr/lib/x86_64-linux-gnu/libeatmydata.so.1:/opt/eatmydata.so:ro -e LD_PRELOAD=/opt/eatmydata.so"
fi
docker run -d --name "$NAME" --network host --log-opt max-size=100m --log-opt max-file=2 $PRELOAD \
  -v "$STORE:/run-dolos" -w /run-dolos $ENTRY "$IMAGE" -c dolos.toml daemon >/dev/null
echo "LIVE-CANDIDATE started name=$NAME commit=$COMMIT candidate=$CANDIDATE grpc=$CANDIDATE_GRPC"
pid=$(docker inspect -f '{{.State.Pid}}' "$NAME")
state=$(fsync_state "/proc/$pid/maps")
tries=1
while [ "$state" = fsync=unknown ] && [ "$tries" -lt 10 ]; do
  sleep 1
  state=$(fsync_state "/proc/$pid/maps")
  tries=$((tries + 1))
done
echo "LIVE-CANDIDATE $state candidate_fsync=$FSYNC pid=$pid tries=$tries"

candidate_slot() {
  "$GRPCURL" -plaintext -d '{}' "$CANDIDATE_GRPC" utxorpc.v1.sync.SyncService/ReadTip 2>/dev/null |
    jq -r '.tip.slot // 0'
}

node_slot() {
  docker exec "$NODE" cardano-cli latest query tip | jq -r .slot
}

start=$(date +%s)
sampled=$start
io_sampled=0
io_samples=0
streak=0
while :; do
  slot=$(candidate_slot || true)
  slot=${slot:-0}
  node=$(node_slot)
  if [ "$slot" -ge $((node - 100)) ]; then
    break
  fi
  if [ "$(free_g "$ROOT")" -lt 3 ]; then
    echo "LIVE-CANDIDATE REFUSED free=$(free_g "$ROOT")G during sync slot=$slot node=$node store=$(du -sh "$STORE" | cut -f1)"
    exit 2
  fi
  if [ $(($(date +%s) - sampled)) -ge $((SAMPLE_MINUTES * 60)) ]; then
    echo "LIVE-CANDIDATE wal slot=$slot node=$node wal_bytes=$(du -sb "$STORE/data/wal" | cut -f1)"
    sampled=$(date +%s)
  fi
  if [ $(($(date +%s) - io_sampled)) -ge 60 ]; then
    io_sampled=$(date +%s)
    line=$(io_line)
    echo "$line"
    io_samples=$((io_samples + 1))
    streak=$(io_streak "$streak" "$line")
    if io_breached "$streak"; then
      echo "LIVE-IO breach samples=$streak node_lag_s_limit=$IO_NODE_LAG_S serve_lag_slots_limit=$IO_SERVE_LAG_SLOTS serve_ms_limit=$IO_SERVE_MS candidate_fsync=$FSYNC slot=$slot node=$node"
      exit 1
    fi
  fi
  if [ $(($(date +%s) - start)) -gt $((SYNC_MINUTES * 60)) ]; then
    echo "LIVE-CANDIDATE sync_timeout slot=$slot node=$node minutes=$SYNC_MINUTES"
    exit 1
  fi
  sleep 30
done
echo "LIVE-CANDIDATE synced slot=$slot node=$node seconds=$(($(date +%s) - start)) store=$(du -sh "$STORE" | cut -f1) io_samples=$io_samples"
# The bytes of each part of the store and the height of its tip, which divide into bytes per block.
height=$("$GRPCURL" -plaintext -max-msg-sz 200000000 \
  -d "{\"ref\":[{\"slot\":$slot}],\"fieldMask\":{\"paths\":[\"cardano.header\"]}}" \
  "$CANDIDATE_GRPC" utxorpc.v1.sync.SyncService/FetchBlock | jq -r '.block.cardano.header.height // 0')
du -sb "$STORE"/data/archive/* "$STORE/data/state" "$STORE/data/wal" | while read -r bytes path; do
  echo "LIVE-CANDIDATE store part=${path#"$STORE"/data/} bytes=$bytes height=$height"
done

# The last block whose body holds a certificate among the candidate's blocks from 6000 to 3000 slots back, which the node holds in its immutable database.
recent=$("$GRPCURL" -plaintext -max-msg-sz 200000000 \
  -d "{\"startToken\":{\"slot\":$((slot - 6000))},\"maxItems\":100,\"fieldMask\":{\"paths\":[\"cardano.header\"]}}" \
  "$CANDIDATE_GRPC" utxorpc.v1.sync.SyncService/DumpHistory |
  jq -r --argjson end $((slot - 3000)) \
    '[.block[].cardano.header | select(.bodyContainsLeiosCertificate and (.slot | tonumber) < $end) | .slot] | last // empty')
if [ -z "$recent" ]; then
  echo "LIVE-CANDIDATE no_certifying_block from=$((slot - 6000)) to=$((slot - 3000))"
  exit 1
fi
echo "LIVE-CANDIDATE certifying_slot=$recent"

SLOTS="$FIXED_SLOTS $recent"
ENDPOINT=$CANDIDATE_GRPC PLAINTEXT=1 LABEL="candidate commit=$COMMIT" BATCH_SLOTS=$SLOTS GRPCURL=$GRPCURL \
  sh "$HERE/live.sh" "$DIR/candidate"
LABEL="serving endpoint=$ENDPOINT" BATCH_SLOTS=$SLOTS GRPCURL=$GRPCURL sh "$HERE/live.sh" "$DIR/serve"
echo "LIVE-CANDIDATE read slots=\"$SLOTS\" commit=$COMMIT"
sh "$HERE/fields.sh" "$DIR"
