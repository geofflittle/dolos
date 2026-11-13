#!/bin/sh
# Builds and runs one corpus writer, then prints its line count and sha256.
# Usage: GEN=<pallas conformance-value-oracle/gen dir> gen.sh <Extract|Pwc|Supplement|Phase1|Uplc|Leaves> <store dir> <work dir>
set -eu
STORE=$2
WORK=$3
IMAGE=${IMAGE:-$(sh "${GEN:?}/build-image.sh" conformance-gen-hs)}
export IMAGE
MODE=
case $1 in
  Extract) M=Extract OUT=ic.jsonl ;;
  Pwc) M=Extract OUT=pwc.jsonl MODE=--pwc ;;
  Supplement) M=Supplement OUT=supp.jsonl ;;
  Phase1) M=Phase1 OUT=p1.jsonl ;;
  Uplc) M=Uplc OUT=uplc.jsonl ;;
  Leaves) M=Leaves OUT=leaves.txt ;;
  *) echo "unknown writer $1" >&2; exit 2 ;;
esac
sh "$(dirname "$0")/build.sh" "$M" "$STORE" "$WORK"
cp "$(dirname "$0")/../../test_data/musashi-native-submission/registration-epoch64-parameters.json" "$WORK/"
rm -f "$WORK/out/$OUT"
docker run --rm -v "$WORK":/probe "$IMAGE" "/probe/bin/$M" $MODE --seed 42
wc -l < "$WORK/out/$OUT"
sha256sum "$WORK/out/$OUT" | cut -c1-16
