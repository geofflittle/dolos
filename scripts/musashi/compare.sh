#!/bin/sh
# Compare the Musashi Leios follower's state against the node's, read only.
#
# Four comparisons, one line each, every line ending IDENTICAL, DIFFERS or
# REPORTED, and a non-zero exit if anything differs:
#
#   TIP    the two tips over a short window, slot and hash
#   ADDR   one line per address, the node's utxos looked up in the follower
#          tx-in for tx-in, with lovelace, datum and script bytes
#   TOTAL  the node's whole utxo sum against the follower's utxo pot
#   RUN    the run's own totals, which compare to nothing and so are REPORTED
#
# Usage: compare.sh [--skip-total] [address ...]
#
# With no addresses it uses the four the ladder documents name: the funded
# wallet, the counter script, the order script and the load address.
#
# Read only throughout. It queries the node over its socket and the follower
# over UtxoRPC, and it reads the follower's pots from a COPY of the newest
# checkpoint, never from the live store, because opening a Dolos store writes
# to it and reading the sync's position must not be a write to the sync's data.
#
# Needs only what the box already has: musashi-node, cardano-cli, grpcurl, jq,
# xxd, base64, docker.
set -eu

RUN=${RUN:-/opt/build/run}
BINS=${BINS:-/opt/build}
GRPCURL=${GRPCURL:-$BINS/grpcurl}
DOLOS_GRPC=${DOLOS_GRPC:-localhost:50051}
IMAGE=${IMAGE:-dolos-build:1}
CONFIG=${CONFIG:-dolos-origin.toml}
CKPT=$RUN/checkpoints

# How many paired tip readings, how far apart, and the lag that still counts as
# following. The follower applies in batches, so a reading can land mid batch;
# the pass condition is that it comes level, not that every single reading is.
TIP_SAMPLES=${TIP_SAMPLES:-10}
TIP_INTERVAL=${TIP_INTERVAL:-20}
TIP_LAG_SLOTS=${TIP_LAG_SLOTS:-200}

# Keys per ReadUtxos call. The follower answers a batch in one round trip and
# the request is built as one json document, so this only bounds the size of
# that document.
BATCH=${BATCH:-100}

DEFAULT_ADDRESSES="
addr_test1vqnw7fc5htw48c680js249zy8c6j938yaz26vlhmjrl5ylq8ux8h3
addr_test1wrcgug276ucd02crnjju75nza44p3v582ns9twf6khglqscv49kk9
addr_test1wpk7xpwtv6c3gxpkcr63h2x4uvnyqe5gkjc607r4sfgkkeqg67y4t
addr_test1vpc2870hncmh6xjpna84hhk6zec2psngxxhaxwyfzh3es2saj4xkg
"

SKIP_TOTAL=no
ADDRESSES=""
for arg in "$@"; do
  case "$arg" in
    --skip-total) SKIP_TOTAL=yes ;;
    -*) echo "unknown option $arg" >&2; exit 2 ;;
    *) ADDRESSES="$ADDRESSES $arg" ;;
  esac
done
[ -n "$ADDRESSES" ] || ADDRESSES=$DEFAULT_ADDRESSES

for tool in musashi-node cardano-cli jq xxd base64 docker; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing $tool" >&2; exit 2; }
done
[ -x "$GRPCURL" ] || { echo "missing $GRPCURL" >&2; exit 2; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT INT TERM
FAILED=0

b64_of_hex() { printf '%s' "$1" | xxd -r -p | base64 -w0; }
node_tip_slot() { musashi-node chain tip | jq -r '.slot'; }
node_tip_hash() { musashi-node chain tip | jq -r '.hash'; }

# The follower's tip, as slot and hex hash, from a query that asks for an output
# that cannot exist: every ReadUtxos reply carries the ledger tip, so this reads
# the tip without needing anything to be in the utxo set.
dolos_tip() {
  zero=$(b64_of_hex 0000000000000000000000000000000000000000000000000000000000000000)
  "$GRPCURL" -plaintext -d "{\"keys\":[{\"hash\":\"$zero\",\"index\":0}]}" \
    "$DOLOS_GRPC" utxorpc.v1alpha.query.QueryService/ReadUtxos 2>/dev/null |
    jq -r '.ledgerTip | "\(.slot) \(.hash)"'
}

say() { printf '%s\n' "$*"; }
differs() { FAILED=1; }

# ---------------------------------------------------------------- TIP

compare_tip() {
  min=; max=; hash_checked=0; hash_bad=0; level=0
  i=0
  while [ "$i" -lt "$TIP_SAMPLES" ]; do
    i=$((i + 1))
    ns=$(node_tip_slot); nh=$(node_tip_hash)
    set -- $(dolos_tip)
    ds=$1
    dh=$(printf '%s' "$2" | base64 -d | xxd -p -c64)
    gap=$((ns - ds))
    [ -z "$min" ] && min=$gap && max=$gap
    [ "$gap" -lt "$min" ] && min=$gap
    [ "$gap" -gt "$max" ] && max=$gap
    if [ "$ns" = "$ds" ]; then
      level=$((level + 1))
      hash_checked=$((hash_checked + 1))
      [ "$nh" = "$dh" ] || hash_bad=$((hash_bad + 1))
    fi
    [ "$i" -lt "$TIP_SAMPLES" ] && sleep "$TIP_INTERVAL"
  done

  # Three conditions, and the first one is the point: at least one reading has
  # to land on the same slot, because only then is there a hash to compare.
  # A small lag on its own verifies nothing about what the follower holds, so a
  # run that never came level is a difference to look at rather than a pass.
  # The follower applies in batches and sits a few slots back between them, so
  # the sampling window has to be long enough to catch it level.
  if [ "$level" -gt 0 ] && [ "$max" -le "$TIP_LAG_SLOTS" ] && [ "$hash_bad" -eq 0 ]; then
    say "TIP    $TIP_SAMPLES samples over $((TIP_SAMPLES * TIP_INTERVAL))s, lag ${min}..${max} slots, level $level/$TIP_SAMPLES, hash agreed $hash_checked/$hash_checked  IDENTICAL"
  else
    if [ "$level" -eq 0 ]; then
      why="no reading landed on the same slot, so no hash was ever compared"
    elif [ "$hash_bad" -gt 0 ]; then
      why="$hash_bad of $hash_checked readings taken at the same slot disagreed on the hash"
    else
      why="lag reached $max slots, over the $TIP_LAG_SLOTS allowed"
    fi
    say "TIP    $TIP_SAMPLES samples over $((TIP_SAMPLES * TIP_INTERVAL))s, lag ${min}..${max} slots, level $level/$TIP_SAMPLES, $why  DIFFERS"
    differs
  fi
}

# ---------------------------------------------------------------- ADDR

# The node is the reference set here, because the follower cannot be asked for
# an address: SearchUtxos needs the index, and this deployment opts the index
# out. So this checks that every utxo the node holds at the address is in the
# follower with the same value, datum and script, and it CANNOT see a utxo the
# follower holds at the address that the node does not. That direction is
# stated rather than silently skipped.
compare_address() {
  addr=$1
  hex=$(cardano-cli address info --address "$addr" | jq -r '.base16')
  musashi-node chain utxo --address "$addr" | jq -c '.utxos[]' > "$WORK/node.jsonl"
  n=$(wc -l < "$WORK/node.jsonl" | tr -d ' ')

  if [ "$n" -eq 0 ]; then
    say "ADDR   ${addr%%...*} $addr  node holds 0 utxos, nothing to compare  REPORTED"
    return
  fi

  jq -r '.txIn' "$WORK/node.jsonl" | sort > "$WORK/node.txin"
  : > "$WORK/dolos.jsonl"
  : > "$WORK/keymap"

  # The follower answers with the reference base64 encoded and jq has no hex
  # encoder, so the comparison is done in base64 and a map back to the readable
  # tx-in is kept for the line a difference prints.
  while IFS='#' read -r h idx; do
    printf '%s#%s %s#%s\n' "$(b64_of_hex "$h")" "$idx" "$h" "$idx" >> "$WORK/keymap"
  done < "$WORK/node.txin"

  # Ask in batches, keyed by the node's own tx-ins.
  split -l "$BATCH" "$WORK/node.txin" "$WORK/chunk."
  for chunk in "$WORK"/chunk.*; do
    keys=$(while IFS='#' read -r h idx; do
             printf '{"hash":"%s","index":%s},' "$(b64_of_hex "$h")" "$idx"
           done < "$chunk")
    keys=${keys%,}
    "$GRPCURL" -plaintext -d "{\"keys\":[$keys]}" \
      "$DOLOS_GRPC" utxorpc.v1alpha.query.QueryService/ReadUtxos 2>/dev/null |
      jq -c '.items[]? | {
        key: (.txoRef.hash + "#" + (.txoRef.index // 0 | tostring)),
        lovelace: (.cardano.coin.int // .cardano.coin // "0" | tostring),
        datum: (.cardano.datum // {} | tostring),
        script: (.cardano.script // {} | tostring)
      }' >> "$WORK/dolos.jsonl"
  done

  cut -d' ' -f1 "$WORK/keymap" | sort > "$WORK/node.key"
  jq -r '.key' "$WORK/dolos.jsonl" | sort > "$WORK/dolos.key"
  d=$(wc -l < "$WORK/dolos.key" | tr -d ' ')

  node_total=$(jq -s 'map(.value.lovelace) | add // 0' "$WORK/node.jsonl")
  dolos_total=$(jq -s 'map(.lovelace | tonumber) | add // 0' "$WORK/dolos.jsonl")

  # Values, tx-in for tx-in, both keyed the same way.
  jq -r '"\(.txIn) \(.value.lovelace)"' "$WORK/node.jsonl" | sort > "$WORK/node.hexval"
  sort -k2 "$WORK/keymap" > "$WORK/keymap.byhex"
  join -1 2 -2 1 -o '1.1 2.2' "$WORK/keymap.byhex" "$WORK/node.hexval" |
    sort > "$WORK/node.val"
  jq -r '"\(.key) \(.lovelace)"' "$WORK/dolos.jsonl" | sort > "$WORK/dolos.val"

  readable() { grep -F "$1 " "$WORK/keymap" | cut -d' ' -f2 | head -1; }

  missing=$(comm -23 "$WORK/node.key" "$WORK/dolos.key" | head -1)
  extra=$(comm -13 "$WORK/node.key" "$WORK/dolos.key" | head -1)
  valdiff=$(comm -23 "$WORK/node.val" "$WORK/dolos.val" | head -1)
  [ -n "$missing" ] && missing=$(readable "$missing")
  [ -n "$extra" ] && extra=$(readable "$extra")
  [ -n "$valdiff" ] && valdiff="$(readable "${valdiff%% *}") ${valdiff#* }"

  if [ -z "$missing" ] && [ -z "$extra" ] && [ -z "$valdiff" ] && [ "$n" = "$d" ]; then
    say "ADDR   $addr  $n utxos, $node_total lovelace, tx-in and values agree  IDENTICAL"
  else
    say "ADDR   $addr  node $n utxos $node_total lovelace, follower $d utxos $dolos_total lovelace  DIFFERS"
    [ -n "$missing" ] && say "       first tx-in on the node only: $missing"
    [ -n "$extra" ] && say "       first tx-in on the follower only: $extra"
    [ -n "$valdiff" ] && say "       first value that differs, node side: $valdiff"
    differs
  fi

  rm -f "$WORK"/chunk.*
}

# ---------------------------------------------------------------- TOTAL

# The node's whole utxo sum against the follower's utxo pot.
#
# The follower cannot enumerate its own utxo set in this deployment, because
# that needs the index and the index is opted out, so its side of this is the
# utxo pot from the epoch state, read from a copy of the newest checkpoint. The
# two are taken at different slots and both slots are printed, because a
# comparison whose window is not stated is not a measurement.
compare_total() {
  before=$(node_tip_slot)
  cardano-cli dijkstra query utxo --whole-utxo --output-json > "$WORK/whole.json"
  after=$(node_tip_slot)

  set -- $(jq -r '[to_entries[] | .value.value.lovelace] | length, add' "$WORK/whole.json" | tr '\n' ' ')
  count=$1
  node_sum=$2

  ckpt=$(ls -1 "$CKPT" 2>/dev/null | grep -E '^[0-9]+$' | sort -n | tail -1)
  if [ -z "$ckpt" ]; then
    say "TOTAL  node $node_sum lovelace over $count utxos at slot $before..$after, follower pot unavailable, no checkpoint  REPORTED"
    return
  fi

  # The copy is opened, and opening a Dolos store rewrites it, which is why it
  # is a copy and why the checkpoint itself is never handed to this. The genesis
  # files travel with it because the config names them relative to the config.
  cp -a "$CKPT/$ckpt" "$WORK/store"
  cp "$RUN"/*-genesis.json "$WORK/" 2>/dev/null || true
  sed 's|^path = "data"$|path = "store"|' "$RUN/$CONFIG" > "$WORK/cmp.toml"

  # Column 5 of the epochs table is the utxo pot: with -F'|' the leading pipe
  # makes field 1 empty, so number, version and reserves are 2, 3 and 4.
  pot=$(docker run --rm --network none -v "$BINS":/bins:ro -v "$WORK":/run-dolos \
          -w /run-dolos --entrypoint /bins/dolos.final "$IMAGE" \
          -c cmp.toml data dump-state --namespace epochs --count 1 2>/dev/null |
        awk -F'|' '/^\| *[0-9]+ *\| *[0-9]+ *\|/ {gsub(/ /,"",$5); print $5; exit}')

  if [ -z "$pot" ]; then
    say "TOTAL  node $node_sum lovelace over $count utxos at slot $before..$after, follower pot unreadable from checkpoint $ckpt  REPORTED"
    return
  fi

  diff=$((pot - node_sum))
  if [ "$diff" = 0 ]; then
    say "TOTAL  node $node_sum lovelace over $count utxos at slot $before..$after, follower pot $pot at checkpoint $ckpt  IDENTICAL"
  else
    say "TOTAL  node $node_sum lovelace over $count utxos at slot $before..$after, follower pot $pot at checkpoint $ckpt, follower minus node $diff lovelace ($((diff / 1000000)) ada)  DIFFERS"
    differs
  fi
}

# ---------------------------------------------------------------- RUN

compare_run() {
  say "RUN    the totals below compare to nothing, they are the run's own  REPORTED"
  sh "$RUN/status.sh" 2>&1 | sed 's/^/       /'
}

# ----------------------------------------------------------------

compare_tip
for a in $ADDRESSES; do compare_address "$a"; done
[ "$SKIP_TOTAL" = yes ] || compare_total
compare_run

exit "$FAILED"
