#!/bin/sh
# Compares every block a utxorpc v1 server holds, field by field, against the ledger oracle over the same bytes.
# usage: [PLANT=kind:key:index] [FROM=dir] run-e2e.sh GRPC OUT NODE_PPARAMS SLOT...
# FROM names an earlier OUT whose dump, cut and oracle lines are linked, so a planted run differs only in the served facts.
# An OUT that already holds blocks.jsonl keeps that dump.
set -eu
D=$(cd "$(dirname "$0")" && pwd)
W=/opt/build/conformance-gen-1024
GRPC=$1
OUT=$2
NODE=$3
shift 3
HS="docker run --rm -i --user $(id -u):$(id -g) -v $W:/work:ro conformance-gen-hs:2 /work/bin/conformance-gen"
mkdir -p "$OUT"
if [ -n "${FROM:-}" ]; then
  for f in slim.jsonl fetched.jsonl index.jsonl block.oracle.jsonl tx.oracle.jsonl ppu.oracle.jsonl; do ln -sf "$FROM/$f" "$OUT/$f"; done
else
  [ -s "$OUT/blocks.jsonl" ] || python3 -I "$D/served.py" dump "$GRPC" "$OUT"
  python3 -I "$D/served.py" fetch "$GRPC" "$OUT" "$@"
  python3 -I "$D/served.py" cut "$GRPC" "$OUT" "$@"
  for k in block tx ppu; do
    $HS oracle "$k" < "$OUT/$k.hex" > "$OUT/$k.oracle.jsonl"
    echo "oracle $k: $(wc -l < "$OUT/$k.hex") inputs, $(wc -l < "$OUT/$k.oracle.jsonl") lines"
  done
fi
python3 -I "$D/served.py" compare "$OUT" > "$OUT/summary.txt"
cat "$OUT/summary.txt"
python3 -I "$D/served.py" params "$GRPC" "$NODE" "$OUT" > "$OUT/params.txt"
cat "$OUT/params.txt"
