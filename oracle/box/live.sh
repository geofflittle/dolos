#!/bin/sh
# Reads a live node and the Dolos endpoint that follows it into one directory, through queries only.
# Usage: ENDPOINT=host:port NODE=container NODE_DB=dir LABEL=text BATCH_SLOTS="slot..." SERVE_CONFIG=dir [PLAINTEXT=1] live.sh <dir>
set -eu
DIR=$1
GRPCURL=${GRPCURL:-grpcurl}
: "${ENDPOINT:?}" "${NODE:?}" "${NODE_DB:?}" "${LABEL:?}" "${BATCH_SLOTS:?}" "${SERVE_CONFIG:?}"
mkdir -p "$DIR"

# Calls one endpoint method from the fixed list of reads.
read_endpoint() {
  case $1 in
    sync.SyncService/ReadTip | sync.SyncService/FetchBlock) ;;
    query.QueryService/ReadParams | query.QueryService/ReadEraSummary | query.QueryService/ReadTx) ;;
    query.QueryService/ReadUtxos) ;;
    *) echo "LIVE refused method $1" >&2 ; exit 2 ;;
  esac
  printf '%s' "$2" | "$GRPCURL" ${PLAINTEXT:+-plaintext} -max-msg-sz 200000000 -d @ "$ENDPOINT" \
    "utxorpc.v1.$1" >"$DIR/$3"
}

# Runs one node query from the fixed list of reads.
query_node() {
  query=$1
  out=$2
  shift 2
  case $query in
    tip | protocol-parameters | utxo) ;;
    *) echo "LIVE refused node query $query" >&2 ; exit 2 ;;
  esac
  docker exec "$NODE" cardano-cli latest query "$query" "$@" >"$DIR/$out"
}

# Copies the immutable chunk holding a slot and its secondary index, once per chunk.
node_chunk() {
  chunk=$(printf '%05d' $(($1 / CHUNK_SLOTS)))
  for part in chunk secondary; do
    [ -f "$DIR/node-$chunk.$part" ] || cat "$NODE_DB/immutable/$chunk.$part" >"$DIR/node-$chunk.$part"
  done
}

# The base64 hash and output count of each transaction and sub transaction of a served block, one per line.
produced() {
  jq -r '.block.cardano.body.tx[] | (.common | "\(.hash) \(.outputs | length)"),
    (.subTransactions[]? | "\(.hash) \(.outputs | length)")' "$1"
}

CHUNK_SLOTS=$((10 * $(jq -r .securityParam "$SERVE_CONFIG/shelley-genesis.json")))
echo "$LABEL" >"$DIR/image.txt"
query_node tip node-tip-before.json
read_endpoint sync.SyncService/ReadTip '{}' tip.json
read_endpoint query.QueryService/ReadParams '{}' params.json
query_node protocol-parameters node-params.json
read_endpoint query.QueryService/ReadEraSummary '{}' era.json
query_node tip node-tip-after.json

: >"$DIR/refs.txt"
for slot in $BATCH_SLOTS; do
  read_endpoint sync.SyncService/FetchBlock "{\"ref\":[{\"slot\":$slot}]}" "block-$slot.json"
  node_chunk "$slot"
  n=0
  for hash in $(jq -r '.block.cardano.body.tx[] | select(.subTransactions) | .common.hash' "$DIR/block-$slot.json"); do
    read_endpoint query.QueryService/ReadTx "{\"hash\":\"$hash\"}" "tx-$slot-$n.json"
    n=$((n + 1))
  done
  produced "$DIR/block-$slot.json" | while read -r hash outputs; do
    hex=$(printf '%s' "$hash" | base64 -d | od -An -v -tx1 | tr -d ' \n')
    i=0
    while [ "$i" -lt "$outputs" ]; do
      echo "$hash $hex $i" >>"$DIR/refs.txt"
      i=$((i + 1))
    done
  done
done
keys=$(awk '{ printf "%s{\"hash\":\"%s\",\"index\":%s}", (NR > 1 ? "," : ""), $1, $3 }' "$DIR/refs.txt")
read_endpoint query.QueryService/ReadUtxos "{\"keys\":[$keys]}" utxos.json
query_node utxo node-utxo.json --output-json $(awk '{ printf "--tx-in %s#%s ", $2, $3 }' "$DIR/refs.txt")
echo "LIVE read dir=$DIR refs=$(wc -l <"$DIR/refs.txt" | tr -dc 0-9)"
