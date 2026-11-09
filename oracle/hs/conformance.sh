#!/bin/sh
# Writes the protocol 12 V3 and V4 conformance directories of an amaru tree from its protocol 11 V3 programs, with the result and budget the ledger gives each text and flat program.
# Usage: conformance.sh <amaru tree> <store dir> <work dir> [check]
# With check it writes the protocol 11 V3 results instead, compares them with the tree's own, and prints one CONFORMANCE line.
# A scenario the amaru test build lists as out of budget is counted apart, since the test replaces its expected files.
set -eu
TREE=$1/crates/amaru-uplc/tests/conformance
STORE=$2
WORK=$3
sh "$(dirname "$0")/build.sh" Conformance "$STORE" "$WORK"

# Writes the programs with the language $1 at the protocol $2 and the cost model file $3 into the directory $4.
write() {
  rm -rf "$WORK/in" "$WORK/out" "$4"
  mkdir -p "$WORK/in" "$WORK/out" "$4"
  cp -R "$TREE/protocol-version-11/v3/." "$WORK/in/"
  (cd "$WORK/in" && find . -name '*.uplc' | sort) >"$WORK/list.txt"
  (cd "$WORK/in" && find . -type d) | (cd "$WORK/out" && xargs mkdir -p)
  (cd "$WORK/in" && find . \( -name '*.uplc' -o -name '*.flat' \) -exec cp {} "$WORK/out/{}" \;)
  cp "$WORK/list.txt" "$WORK/in/list.txt"
  cp "$3" "$WORK/in/costs.json"
  docker run --rm -v "$WORK":/probe conformance-gen-hs:2 /probe/bin/Conformance "$1" "$2" /probe/in/costs.json /probe/in/list.txt
  cp -R "$WORK/out/." "$4/"
}

# Prints the file $1 without whitespace, or the word absent when there is no such file.
content() {
  if [ -f "$1" ]; then tr -d ' \n' <"$1"; else echo absent; fi
}

if [ "${4:-}" = check ]; then
  write V3 11 "$TREE/cost-models/protocol-version-11/v3.json" "$WORK/check"
  # The flat bytes of this program hold a tag the ledger decodes, so its flat result is an evaluation failure where the suite gives the parse error of its text.
  LEDGER_DECODES=./term/constr/constr-07/constr-07.flat.expected
  same=0
  differ=0
  overridden=0
  ledger=0
  for program in $(cat "$WORK/list.txt"); do
    base=${program%.uplc}
    scenario=$(dirname "${program#./}" | tr -c '[:alnum:]\n' '_' | tr '[:upper:]' '[:lower:]')
    files="$program.expected $base.budget"
    if [ -f "$WORK/in/$base.flat" ]; then
      files="$files $base.flat.expected"
    fi
    for file in $files; do
      if [ "$(content "$WORK/check/$file")" = "$(content "$TREE/protocol-version-11/v3/$file")" ]; then
        same=$((same + 1))
      elif grep -q -F "\"$scenario\"," "$1/crates/amaru-uplc/build.rs"; then
        overridden=$((overridden + 1))
        echo "OVERRIDDEN $file"
      elif [ "$file" = "$LEDGER_DECODES" ]; then
        ledger=$((ledger + 1))
        echo "LEDGER $file"
      else
        differ=$((differ + 1))
        echo "DIFFER $file"
      fi
    done
  done
  echo "CONFORMANCE check protocol=11 language=V3 same=$same overridden=$overridden ledger=$ledger differ=$differ"
  [ "$differ" -eq 0 ] && [ "$ledger" -eq 1 ]
  exit
fi
write V3 12 "$TREE/cost-models/protocol-version-12/v3.json" "$TREE/protocol-version-12/v3"
write V4 12 "$TREE/cost-models/protocol-version-12/v4.json" "$TREE/protocol-version-12/v4"
for language in v3 v4; do
  echo "CONFORMANCE protocol=12 language=$language programs=$(find "$TREE/protocol-version-12/$language" -name '*.uplc' | wc -l)"
done
