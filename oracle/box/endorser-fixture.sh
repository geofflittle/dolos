#!/bin/sh
# Writes the fixture of the endorser transaction judge, each ranking block at a slot and its parent fetched from the node's peer port, and the endorser block a certifying one certifies read from the node's leios store.
# Usage: ROOT=dir TREE=dir ENDPOINT=host:port GRPCURL=path PEER="host:port magic" LEIOS_DB=file endorser-fixture.sh <out .jsonl.gz> <slot>...
set -eu
: "${ROOT:?}" "${TREE:?}" "${ENDPOINT:?}" "${GRPCURL:?}" "${PEER:?}" "${LEIOS_DB:?}"
OUT=$1
shift
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/lib.sh"
enter_slice "$0" "$OUT" "$@"
WORK=fixture-work/endorser
DIR=$ROOT/$WORK
rm -rf "$DIR"
mkdir -p "$DIR"

# Writes one FetchBlock answer of the endpoint for a block reference.
fetch() {
  "$GRPCURL" -max-msg-sz 200000000 -d "{\"ref\":[$1]}" "$ENDPOINT" utxorpc.v1.sync.SyncService/FetchBlock >"$DIR/$2"
}

# The hex of a base64 field of a FetchBlock answer.
hex_of() {
  jq -r "$2" "$DIR/$1" | base64 -d | od -An -v -tx1 | tr -d ' \n'
}

"$GRPCURL" -d '{}' "$ENDPOINT" utxorpc.v1.sync.SyncService/ReadTip >"$DIR/tip.json"
tip=$(jq -er .tip.slot "$DIR/tip.json")
echo "$PEER" >"$DIR/peer"
: >"$DIR/points"
for slot in "$@"; do
  fetch "{\"slot\":$slot}" "block-$slot.json"
  height=$(jq -er .block.cardano.header.height "$DIR/block-$slot.json")
  fetch "{\"height\":$((height - 1))}" "parent-$slot.json"
  for name in parent block; do
    at=$(jq -er .block.cardano.header.slot "$DIR/$name-$slot.json")
    echo "$at $(hex_of "$name-$slot.json" .block.cardano.header.hash)" >>"$DIR/points"
    echo "$at" >"$DIR/$name-$slot.slot"
  done
done
FIXTURE_DIR=/w/$WORK sh "$HERE/in-image.sh" "$TREE" cargo test -p dolos --lib -- \
  serve::grpc::v1::ledger_oracle_tests::endorser::fetch_node_blocks --exact --ignored --nocapture
: >"$DIR/cases.jsonl"
for slot in "$@"; do
  parent=$DIR/node-$(cat "$DIR/parent-$slot.slot").cbor
  block=$DIR/node-$(cat "$DIR/block-$slot.slot").cbor
  if [ "$(jq -r '.block.cardano.header.bodyContainsLeiosCertificate // false' "$DIR/block-$slot.json")" = true ]; then
    eb=$(hex_of "parent-$slot.json" .block.cardano.header.endorserBlockAnnouncement.referencesHash)
    python3 -I "$HERE/endorser_fixture.py" node-eb "$LEIOS_DB" "$tip" "$slot" "$eb" >"$DIR/eb-$slot.json"
    python3 -I "$HERE/endorser_fixture.py" case "$parent" "$block" "$DIR/eb-$slot.json" >>"$DIR/cases.jsonl"
  else
    python3 -I "$HERE/endorser_fixture.py" case "$parent" "$block" >>"$DIR/cases.jsonl"
  fi
done
gzip -9 -n -c "$DIR/cases.jsonl" >"$OUT"
echo "FIXTURE cases=$(wc -l <"$DIR/cases.jsonl" | tr -dc 0-9) bytes=$(wc -c <"$OUT" | tr -dc 0-9) out=$OUT"
