#!/bin/sh
# Builds and runs one corpus writer, then prints its line count and sha256.
# Usage: gen.sh <Extract|Pwc|Supplement|Uplc> <store dir> <work dir>
set -eu
STORE=$2
WORK=$3
MODE=
case $1 in
  Extract) M=Extract OUT=ic.jsonl ;;
  Pwc) M=Extract OUT=pwc.jsonl MODE=--pwc ;;
  Supplement) M=Supplement OUT=supp.jsonl ;;
  Uplc) M=Uplc OUT=uplc.jsonl ;;
  *) echo "unknown writer $1" >&2; exit 2 ;;
esac
sh "$(dirname "$0")/build.sh" "$M" "$STORE" "$WORK"
rm -f "$WORK/out/$OUT"
docker run --rm -v "$WORK":/probe conformance-gen-hs:2 "/probe/bin/$M" $MODE --seed 42
wc -l < "$WORK/out/$OUT"
sha256sum "$WORK/out/$OUT" | cut -c1-16
