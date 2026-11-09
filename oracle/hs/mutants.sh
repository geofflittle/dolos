#!/bin/sh
# Writes the mutant corpus. Each record writer runs with the mutants of its
# accepted records, its records must equal its committed corpus, and the
# mutants of lowest rank per corpus and operator are kept in generation order.
# Usage: mutants.sh <store dir> <work dir> [seed], then the corpus is
# test_data/ledger-oracle/mut.jsonl.gz and the last line gives its sha256.
set -eu
STORE=$1
WORK=$2
SEED=${3:-42}
PER=100
HERE=$(cd "$(dirname "$0")" && pwd)
DATA=$HERE/../../test_data/ledger-oracle
TAB=$(printf '\t')
mkdir -p "$WORK/out"
cp "$HERE/../../test_data/musashi-native-submission/registration-epoch64-parameters.json" "$WORK/"
rm -f "$WORK/mut.jsonl"
for writer in Extract:ic Supplement:supp Phase1:p1; do
  M=${writer%%:*}
  C=${writer#*:}
  sh "$HERE/build.sh" "$M" "$STORE" "$WORK"
  rm -f "$WORK/out/$C.jsonl" "$WORK/out/$C.jsonl.mutants" "$WORK/out/$C.jsonl.skipped"
  docker run --rm -v "$WORK":/probe conformance-gen-hs:2 "/probe/bin/$M" --mutants "$SEED" --seed 42 >"$WORK/$C.log" 2>&1
  made=$(sha256sum <"$WORK/out/$C.jsonl" | cut -c1-64)
  committed=$(gzip -dc "$DATA/$C.jsonl.gz" | sha256sum | cut -c1-64)
  if [ "$made" != "$committed" ]; then
    echo "REFUSED $C records sha=$made committed=$committed"
    exit 1
  fi
  touch "$WORK/out/$C.jsonl.skipped"
  awk -F "$TAB" -v OFS="$TAB" '{ print NR, $0 }' "$WORK/out/$C.jsonl.mutants" |
    LC_ALL=C sort -t "$TAB" -k2,2 -k3,3n -k1,1n |
    awk -F "$TAB" -v per="$PER" '{ if (++kept[$2] <= per) print }' |
    LC_ALL=C sort -t "$TAB" -k1,1n >"$WORK/$C.kept"
  for op in $(cut -f1 "$WORK/out/$C.jsonl.mutants" "$WORK/out/$C.jsonl.skipped" | LC_ALL=C sort -u); do
    made=$(awk -F "$TAB" -v op="$op" '$1 == op' "$WORK/out/$C.jsonl.mutants" | wc -l)
    skipped=$(awk -F "$TAB" -v op="$op" '$1 == op' "$WORK/out/$C.jsonl.skipped" | wc -l)
    kept=$(awk -F "$TAB" -v op="$op" '$2 == op' "$WORK/$C.kept" | wc -l)
    accepted=$(awk -F "$TAB" -v op="$op" '$2 == op' "$WORK/$C.kept" | /usr/bin/grep -c '"outcome":{"accepted"' || true)
    echo "MUTANTS corpus=$C op=$op made=$made skipped=$skipped kept=$kept ledger_accepted=$accepted ledger_refused=$((kept - accepted))"
  done
  cut -f4 "$WORK/$C.kept" >>"$WORK/mut.jsonl"
done
gzip -n -9 <"$WORK/mut.jsonl" >"$DATA/mut.jsonl.gz"
echo "MUTANTS records=$(wc -l <"$WORK/mut.jsonl") sha=$(sha256sum <"$WORK/mut.jsonl" | cut -c1-64)"
