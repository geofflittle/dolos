#!/bin/sh
# Syncs a candidate Dolos from the node beside the serving one, reads both and the node, prints each field the two serve differently, and removes the candidate.
# Usage: CANDIDATE=<binary path or image reference> COMMIT=sha candidate.sh <dir>
# Reads ROOT, NODE, NODE_DB, ENDPOINT, SERVE_CONFIG, CANDIDATE_GRPC, CANDIDATE_NEED_G, PRUNE_MINUTES and GRPCURL from the environment.
set -eu
DIR=$1
: "${CANDIDATE:?}" "${COMMIT:?}" "${ROOT:?}" "${NODE:?}" "${ENDPOINT:?}" "${SERVE_CONFIG:?}"
: "${CANDIDATE_GRPC:?}" "${CANDIDATE_NEED_G:?}"
NEED_G=$CANDIDATE_NEED_G
HERE=$(cd "$(dirname "$0")" && pwd)
GRPCURL=${GRPCURL:-grpcurl}
NAME=${CANDIDATE_NAME:-dolos-candidate-1028}
SYNC_MINUTES=${SYNC_MINUTES:-90}
PRUNE_MINUTES=${PRUNE_MINUTES:-5}
FIXED_SLOTS="205422 368793 783572"
STORE="$ROOT/candidate/$(date -u +%Y%m%dT%H%M%SZ)"

free_g() {
  df -BG --output=avail "$1" | tail -1 | tr -dc 0-9
}

if [ "$(free_g "$ROOT")" -lt $((NEED_G + 3)) ] || [ "$(free_g /)" -lt 25 ]; then
  echo "LIVE-CANDIDATE REFUSED free=$(free_g "$ROOT")G root_free=$(free_g /)G need=$((NEED_G + 3))G"
  exit 2
fi

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  rm -rf "$STORE"
  echo "LIVE-CANDIDATE removed name=$NAME store=$STORE"
}
trap cleanup EXIT

mkdir -p "$STORE" "$DIR"
for genesis in byron shelley alonzo conway dijkstra; do
  cat "$SERVE_CONFIG/$genesis-genesis.json" >"$STORE/$genesis-genesis.json"
done
# The serving configuration without its listeners, then the candidate's own.
awk '/^\[/ { skip = ($0 ~ /^\[serve/) } !skip' "$SERVE_CONFIG/dolos.toml" >"$STORE/dolos.toml"
printf '[serve.grpc]\nlisten_address = "%s"\n' "$CANDIDATE_GRPC" >>"$STORE/dolos.toml"

case $CANDIDATE in
  /*)
    cp "$CANDIDATE" "$STORE/dolos-candidate"
    IMAGE=dolos-build:1
    ENTRY="--entrypoint /run-dolos/dolos-candidate"
    ;;
  *)
    IMAGE=$CANDIDATE
    ENTRY=
    ;;
esac
docker run -d --name "$NAME" --network host --log-opt max-size=100m --log-opt max-file=2 \
  -v "$STORE:/run-dolos" -w /run-dolos $ENTRY "$IMAGE" -c dolos.toml daemon >/dev/null
echo "LIVE-CANDIDATE started name=$NAME commit=$COMMIT candidate=$CANDIDATE grpc=$CANDIDATE_GRPC"

candidate_slot() {
  "$GRPCURL" -plaintext -d '{}' "$CANDIDATE_GRPC" utxorpc.v1.sync.SyncService/ReadTip 2>/dev/null |
    jq -r '.tip.slot // 0'
}

node_slot() {
  docker exec "$NODE" cardano-cli latest query tip | jq -r .slot
}

# Stops the candidate, prunes its wal to the rollback window and compacts it, and starts it again.
prune_wal() {
  docker stop -t 120 "$NAME" >/dev/null
  docker run --rm --network none -v "$STORE:/run-dolos" -w /run-dolos $ENTRY "$IMAGE" \
    -c dolos.toml data prune-wal >>"$DIR/prune-wal.log" 2>&1
  docker start "$NAME" >/dev/null
  echo "LIVE-CANDIDATE pruned_wal slot=$slot wal_bytes=$(du -sb "$STORE/data/wal" | cut -f1)"
}

start=$(date +%s)
pruned=$start
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
  if [ $(($(date +%s) - pruned)) -ge $((PRUNE_MINUTES * 60)) ]; then
    prune_wal
    pruned=$(date +%s)
  fi
  if [ $(($(date +%s) - start)) -gt $((SYNC_MINUTES * 60)) ]; then
    echo "LIVE-CANDIDATE sync_timeout slot=$slot node=$node minutes=$SYNC_MINUTES"
    exit 1
  fi
  sleep 30
done
echo "LIVE-CANDIDATE synced slot=$slot node=$node seconds=$(($(date +%s) - start)) store=$(du -sh "$STORE" | cut -f1)"
# The bytes of each part of the store and the height of its tip, which divide into bytes per block.
height=$("$GRPCURL" -plaintext -d '{}' "$CANDIDATE_GRPC" utxorpc.v1.sync.SyncService/ReadTip | jq -r '.tip.height // 0')
du -sb "$STORE"/data/archive/* "$STORE/data/state" "$STORE/data/wal" | while read -r bytes path; do
  echo "LIVE-CANDIDATE store part=${path#"$STORE"/data/} bytes=$bytes height=$height"
done

# The last block whose body holds a certificate among the candidate's blocks from 6000 to 3000 slots back, which the node holds in its immutable database.
recent=$("$GRPCURL" -plaintext -max-msg-sz 200000000 \
  -d "{\"startToken\":{\"slot\":$((slot - 6000))},\"maxItems\":200,\"fieldMask\":{\"paths\":[\"cardano.header\"]}}" \
  "$CANDIDATE_GRPC" utxorpc.v1.sync.SyncService/DumpHistory |
  jq -r --argjson end $((slot - 3000)) \
    '[.block[].cardano.header | select(.bodyContainsLeiosCertificate and (.slot | tonumber) < $end) | .slot] | last // empty')
if [ -z "$recent" ]; then
  echo "LIVE-CANDIDATE no_certifying_block from=$((slot - 6000)) to=$((slot - 3000))"
  exit 1
fi
echo "LIVE-CANDIDATE certifying_slot=$recent"

SLOTS="$FIXED_SLOTS $recent"
ENDPOINT=$CANDIDATE_GRPC PLAINTEXT=1 LABEL="candidate commit=$COMMIT" BATCH_SLOTS=$SLOTS \
  sh "$HERE/live.sh" "$DIR/candidate"
LABEL="serving endpoint=$ENDPOINT" BATCH_SLOTS=$SLOTS sh "$HERE/live.sh" "$DIR/serve"
echo "LIVE-CANDIDATE read slots=\"$SLOTS\" commit=$COMMIT"
sh "$HERE/fields.sh" "$DIR"
