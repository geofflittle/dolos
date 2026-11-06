#!/bin/sh
# Builds and runs one corpus writer, then prints its line count and sha256.
# Usage: gen.sh <Extract|Supplement|Phase1> <store dir> <work dir>
set -eu
M=$1
STORE=$2
WORK=$3
case $M in
  Extract) OUT=ic.jsonl ;;
  Supplement) OUT=supp.jsonl ;;
  Phase1) OUT=p1.jsonl ;;
  *) echo "unknown module $M" >&2; exit 2 ;;
esac
sh "$(dirname "$0")/build.sh" "$M" "$STORE" "$WORK"
rm -f "$WORK/out/$OUT"
docker run --rm -v "$WORK":/probe conformance-gen-hs:2 "/probe/bin/$M" --seed 42
wc -l < "$WORK/out/$OUT"
sha256sum "$WORK/out/$OUT" | cut -c1-16
