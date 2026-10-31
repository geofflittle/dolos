#!/bin/sh
# Prints each field the reads in <dir>/candidate and <dir>/serve hold differently, and fails when one differs.
# Usage: fields.sh <dir>
set -eu
DIR=$1
TAB=$(printf '\t')
export LC_ALL=C

# Each scalar of a read as one line, its dotted path, a tab, then its value.
# ReadUtxos answers in the order of a hash map, so its items are put in output reference order first.
leaves() {
  jq -r '(if has("items") then .items |= sort_by(.txoRef.hash, (.txoRef.index // 0)) else . end) |
    paths(scalars) as $p | "\($p | map(tostring) | join("."))\t\(getpath($p) | tojson)"' "$1" |
    sort -t "$TAB" -k1,1
}

differing=0
compared=0
for name in params.json era.json utxos.json $(cd "$DIR/candidate" && ls block-*.json tx-*.json 2>/dev/null); do
  if [ ! -f "$DIR/serve/$name" ]; then
    echo "LIVE-FIELD $name serve=absent"
    differing=$((differing + 1))
    continue
  fi
  leaves "$DIR/candidate/$name" >"$DIR/candidate.leaves"
  leaves "$DIR/serve/$name" >"$DIR/serve.leaves"
  compared=$((compared + $(wc -l <"$DIR/candidate.leaves")))
  # The ledger tip of a response is the tip each endpoint held when it answered.
  join -t "$TAB" -a 1 -a 2 -e none -o 0,1.2,2.2 "$DIR/candidate.leaves" "$DIR/serve.leaves" |
    awk -F '\t' '$2 != $3 && $1 !~ /^ledgerTip\./' >"$DIR/differing.txt"
  while IFS="$TAB" read -r path candidate serve; do
    differing=$((differing + 1))
    echo "LIVE-FIELD $name $path candidate=$candidate serve=$serve" | cut -c1-400
  done <"$DIR/differing.txt"
done
rm -f "$DIR/candidate.leaves" "$DIR/serve.leaves" "$DIR/differing.txt"
echo "LIVE-FIELDS compared=$compared differing=$differing"
[ "$differing" -eq 0 ]
