#!/bin/sh
# Prints each field the reads in <dir>/candidate and <dir>/serve hold differently, and fails when one differs.
# Usage: fields.sh <dir>
set -eu
DIR=$1
TAB=$(printf '\t')
export LC_ALL=C

# Each scalar of a read as one line, its dotted path, a tab, then its value.
# ReadUtxos answers in the order of a hash map, so its items are keyed by output reference.
leaves() {
  jq -r '(if has("items") then .items |= (map({key: "\(.txoRef.hash)#\(.txoRef.index // 0)", value: .}) | from_entries) else . end) |
    paths(scalars) as $p | "\($p | map(tostring) | join("."))\t\(getpath($p) | tojson)"' "$1" |
    sort -t "$TAB" -k1,1
}

# Each output one read holds and the other does not, when the node held it at the first read and not at the second, and the second read's ledger tip is later.
spent() {
  jq -r -n --rawfile refs "$DIR/candidate/refs.txt" \
    --slurpfile c "$DIR/candidate/utxos.json" --slurpfile s "$DIR/serve/utxos.json" \
    --slurpfile cn "$DIR/candidate/node-utxo.json" --slurpfile sn "$DIR/serve/node-utxo.json" '
    def held($read): [$read.items[] | {key: "\(.txoRef.hash)#\(.txoRef.index // 0)", value: true}] | from_entries;
    {candidate: {held: held($c[0]), node: $cn[0], tip: ($c[0].ledgerTip.slot | tonumber)},
     serve: {held: held($s[0]), node: $sn[0], tip: ($s[0].ledgerTip.slot | tonumber)}} as $side |
    $refs | split("\n")[] | select(. != "") | split(" ") |
    {ref: "\(.[0])#\(.[2])", node: "\(.[1])#\(.[2])"} as $r |
    select(($side.candidate.held | has($r.ref)) != ($side.serve.held | has($r.ref))) |
    (if $side.candidate.held | has($r.ref) then ["candidate", "serve"] else ["serve", "candidate"] end) as [$on, $off] |
    select(($side[$on].node | has($r.node)) and ($side[$off].node | has($r.node) | not)
      and $side[$off].tip > $side[$on].tip) |
    "\($r.ref)\t\($on)\t\($side[$on].tip)\t\($off)\t\($side[$off].tip)"' >"$DIR/spent.txt"
  while IFS="$TAB" read -r ref on on_tip off off_tip; do
    echo "LIVE-FIELD-SPENT utxos.json ref=$ref held_by=$on ledger_tip=$on_tip spent_for=$off ledger_tip=$off_tip node=held_then_spent"
  done <"$DIR/spent.txt"
}

spent
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
    awk -F '\t' -v name="$name" -v spentfile="$DIR/spent.txt" '
      BEGIN { while ((getline line <spentfile) > 0) { split(line, f, "\t"); spent[f[1]] = 1 } }
      { split($1, part, ".") }
      $2 != $3 && $1 !~ /^ledgerTip\./ && !(name == "utxos.json" && part[1] == "items" && spent[part[2]])' \
    >"$DIR/differing.txt"
  while IFS="$TAB" read -r path candidate serve; do
    differing=$((differing + 1))
    echo "LIVE-FIELD $name $path candidate=$candidate serve=$serve" | cut -c1-400
  done <"$DIR/differing.txt"
done
rm -f "$DIR/candidate.leaves" "$DIR/serve.leaves" "$DIR/differing.txt" "$DIR/spent.txt"
echo "LIVE-FIELDS compared=$compared differing=$differing"
[ "$differing" -eq 0 ]
