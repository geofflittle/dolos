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
    tip | protocol-parameters | utxo | era-history | slot-number) ;;
    *) echo "LIVE refused node query $query" >&2 ; exit 2 ;;
  esac
  docker exec "$NODE" cardano-cli latest query "$query" "$@" >"$DIR/$out"
}

# Writes the slot the node holds at a POSIX second, or past-horizon when it translates none, once per second.
node_slot() {
  out=node-slot-$1.txt
  [ -f "$DIR/$out" ] && return 0
  if ! query_node slot-number "$out" "$(date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ)" 2>"$DIR/node-slot-$1.err"; then
    grep -q 'Past horizon' "$DIR/node-slot-$1.err"
    echo past-horizon >"$DIR/$out"
  fi
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

# The base64 hash of each transaction an input of a batch transaction of a served block names, once each.
producers() {
  jq -r '.block.cardano.body.tx[] | select(.subTransactions) |
    (.common.inputs[]?, .common.referenceInputs[]?, .collateral.collateral[]?,
      (.subTransactions[] | .inputs[]?, .referenceInputs[]?)) | .txHash' "$1" | sort -u
}

CHUNK_SLOTS=$((10 *$(jq -r .securityParam "$SERVE_CONFIG/shelley-genesis.json")))
echo "$LABEL" >"$DIR/image.txt"
query_node tip node-tip-before.json
read_endpoint sync.SyncService/ReadTip '{}' tip.json
read_endpoint query.QueryService/ReadParams '{}' params.json
query_node protocol-parameters node-params.json
read_endpoint query.QueryService/ReadEraSummary '{}' era.json
query_node era-history node-era-history.json
for ms in $(jq -r '.cardano.summaries[-1].start.time // empty' "$DIR/era.json") \
  $(jq -r '.tip.timestamp // empty' "$DIR/tip.json"); do
  node_slot $((ms / 1000 - 1))
  node_slot $((ms / 1000))
  node_slot $((ms / 1000 + 1))
done
query_node tip node-tip-after.json

: >"$DIR/refs.txt"
for slot in $BATCH_SLOTS; do
  read_endpoint sync.SyncService/FetchBlock "{\"ref\":[{\"slot\":$slot}]}" "block-$slot.json"
  node_chunk "$slot"
  # A certifying block is judged against the endorser block its parent
  # announced, read from the node's leios store, and against its parent's bytes.
  if [ "$(jq -r '.block.cardano.header.bodyContainsLeiosCertificate // false' "$DIR/block-$slot.json")" = true ]; then
    height=$(jq -er .block.cardano.header.height "$DIR/block-$slot.json")
    read_endpoint sync.SyncService/FetchBlock "{\"ref\":[{\"height\":$((height - 1))}]}" "parent-$slot.json"
    node_chunk "$(jq -er .block.cardano.header.slot "$DIR/parent-$slot.json")"
    eb=$(jq -er .block.cardano.header.endorserBlockAnnouncement.referencesHash "$DIR/parent-$slot.json" |
      base64 -d | od -An -v -tx1 | tr -d ' \n')
    python3 -I "$(dirname "$0")/endorser_fixture.py" node-eb "$NODE_DB/leios.imm.db" \
      "$(jq -er .slot "$DIR/node-tip-before.json")" "$slot" "$eb" >"$DIR/node-eb-$slot.json" \
      2>"$DIR/node-eb-$slot.err" || true
  fi
  n=0
  for hash in $(jq -r '.block.cardano.body.tx[] | select(.subTransactions) | .common.hash' "$DIR/block-$slot.json"); do
    read_endpoint query.QueryService/ReadTx "{\"hash\":\"$hash\"}" "tx-$slot-$n.json"
    n=$((n + 1))
  done
  # The transactions that produced the outputs the batch transactions spend or
  # reference, whose bytes the judge checks against their hashes.
  for hash in $(producers "$DIR/block-$slot.json"); do
    hex=$(printf '%s' "$hash" | base64 -d | od -An -v -tx1 | tr -d ' \n')
    [ -f "$DIR/producer-$hex.json" ] ||
      read_endpoint query.QueryService/ReadTx "{\"hash\":\"$hash\"}" "producer-$hex.json" ||
      rm -f "$DIR/producer-$hex.json"
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
# Each try brackets both UTxO reads with the node tip and the endpoint ledger tip, until all four name one slot.
try=1
while :; do
  query_node tip node-utxo-tip-before.json
  read_endpoint query.QueryService/ReadUtxos '{"keys":[]}' utxos-tip-before.json
  read_endpoint query.QueryService/ReadUtxos "{\"keys\":[$keys]}" utxos.json
  txins=$(awk '{ printf "--tx-in %s#%s ", $2, $3 }' "$DIR/refs.txt")
  query_node utxo node-utxo.json --output-json $txins
  query_node utxo node-utxo.cbor --output-cbor-hex $txins
  query_node tip node-utxo-tip-after.json
  slots=$(jq -r '.slot // .ledgerTip.slot' "$DIR/node-utxo-tip-before.json" "$DIR/utxos-tip-before.json" \
    "$DIR/utxos.json" "$DIR/node-utxo-tip-after.json" | sort -u | wc -l | tr -dc 0-9)
  if [ "$slots" -eq 1 ] || [ "$try" -ge "${UTXO_TRIES:-10}" ]; then
    break
  fi
  try=$((try + 1))
done
echo "LIVE read dir=$DIR refs=$(wc -l <"$DIR/refs.txt" | tr -dc 0-9) utxo_tries=$try utxo_slots=$slots"
