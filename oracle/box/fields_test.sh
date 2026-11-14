#!/bin/sh
# Checks that fields.sh passes a differing field only where the candidate holds the node's value, with canned reads.
# Usage: fields_test.sh <empty work dir>
set -eu
W=$(cd "$1" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)

# Writes one side's reads, its era summary names as a JSON list and the node's era before and after.
side() {
  s=$1
  mkdir -p "$s"
  echo '{}' >"$s/params.json"
  echo "{\"cardano\":{\"summaries\":$(echo "$2" | jq -c 'map({name: .})')}}" >"$s/era.json"
  echo '{"items":[],"ledgerTip":{"slot":"10"}}' >"$s/utxos.json"
  echo '{"ledgerTip":{"slot":"10"}}' >"$s/utxos-tip-before.json"
  echo '{"slot":10}' >"$s/node-utxo-tip-before.json"
  echo '{"slot":10}' >"$s/node-utxo-tip-after.json"
  echo '{}' >"$s/node-utxo.json"
  : >"$s/refs.txt"
  echo "{\"era\":\"$3\",\"slot\":10}" >"$s/node-tip-before.json"
  echo "{\"era\":\"$4\",\"slot\":10}" >"$s/node-tip-after.json"
}

# Runs fields.sh over a candidate and a serving side, printing its exit and its result lines.
fields() {
  dir=$W/$1
  rm -rf "$dir"
  side "$dir/candidate" "$2" "$4" "$5"
  side "$dir/serve" "$3" "$4" "$5"
  [ -z "${6:-}" ] || echo "$6" >"$dir/candidate/params.json"
  status=0
  out=$(sh "$HERE/fields.sh" "$dir" 2>&1) || status=$?
  echo "exit=$status $(echo "$out" | /usr/bin/grep -E '^LIVE-FIELD(S|-SETTLED)? ' | cut -d' ' -f1-3 | tr '\n' ' ')"
}

failed=0
expect() {
  case $2 in
    "$3") echo "FIELDS-TEST $1 PASS" ;;
    *)
      echo "FIELDS-TEST $1 FAIL got=\"$2\" want=\"$3\""
      failed=1
      ;;
  esac
}

# Prints the verdict of the cases run so far, and fails when one of them failed.
verdict() {
  if [ "$failed" -eq 0 ]; then
    echo "FIELDS-TEST RESULT PASS"
  else
    echo "FIELDS-TEST RESULT FAIL"
    return 1
  fi
}

# Must not fire: a passing case followed by data named failed.
expect harness_names_pass "$( (expect planted_pass a a >/dev/null && failed=1 && verdict) || true)" \
  "FIELDS-TEST RESULT PASS"
# Must fire: a failing case followed by data named failed.
expect harness_names_fail "$( (expect planted_fail a b >/dev/null && failed=0 && verdict) || true)" \
  "FIELDS-TEST RESULT FAIL"

# Must not fire: the same reads, and a current era name where the candidate holds the node's era.
expect same "$(fields same '["dijkstra"]' '["dijkstra"]' Dijkstra Dijkstra)" \
  "exit=0 LIVE-FIELDS compared=2 differing=0 "
expect settled "$(fields settled '["dijkstra"]' '["unknown"]' Dijkstra Dijkstra)" \
  "exit=0 LIVE-FIELD-SETTLED era.json cardano.summaries.0.name LIVE-FIELDS compared=2 differing=0 "

# Must fire: the serving side holds the node's era, the node changed era between its two reads, an earlier summary differs, and a field the node states nothing for differs.
expect serve_holds "$(fields serve_holds '["unknown"]' '["dijkstra"]' Dijkstra Dijkstra)" \
  "exit=1 LIVE-FIELD era.json cardano.summaries.0.name LIVE-FIELDS compared=2 differing=1 "
expect era_moved "$(fields era_moved '["dijkstra"]' '["unknown"]' Conway Dijkstra)" \
  "exit=1 LIVE-FIELD era.json cardano.summaries.0.name LIVE-FIELDS compared=2 differing=1 "
expect earlier "$(fields earlier '["dijkstra","dijkstra"]' '["unknown","dijkstra"]' Dijkstra Dijkstra)" \
  "exit=1 LIVE-FIELD era.json cardano.summaries.0.name LIVE-FIELDS compared=3 differing=1 "
expect params "$(fields params '["dijkstra"]' '["dijkstra"]' Dijkstra Dijkstra '{"minFeeCoefficient":"44"}')" \
  "exit=1 LIVE-FIELD params.json minFeeCoefficient LIVE-FIELDS compared=3 differing=1 "

verdict
