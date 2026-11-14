#!/bin/sh
# Writes the manifest lines the corpus judge and the live judge propose at REV into REGEN_DIR, and prints how each differs from the committed manifest, leaving the tree unchanged.
# Usage: ROOT=dir [REV=ref] regen.sh
# The live leg reads the node through $LIVE_ENV, so regen.sh runs where the node is reachable.
set -eu
: "${ROOT:?}"
HERE=$(cd "$(dirname "$0")" && pwd)
REGEN_DIR=${REGEN_DIR:-$ROOT/regen}
. "$HERE/lib.sh"
trap finish EXIT
start_run "$0" "$@"

# Builds the candidate and the live judge, syncs the candidate and judges its reads.
live_leg() {
  build_candidate && build_live_judge || return 1
  fields=0
  sync_candidate "$OUT/bin/dolos" || fields=$?
  judge_live && [ "$fields" -eq 0 ]
}

mkdir -p "$OUT"
require_free
require_lib_test
require_build_image
candidate_fsync >/dev/null || exit 2
reset_scratch
commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
if ! built=$(build_judge "$OUT/head.build" --locked); then
  echo "REGEN REFUSED the judge build $built"
  exit 1
fi
sh "$IN_IMAGE" "$SCRATCH" "$(executable "$OUT/head.build")" "$ORACLE_TESTS::ledger_oracle" \
  --exact --ignored --nocapture >"$OUT/head.log" 2>&1 || true
if ! grep -q '^RESULT ' "$OUT/head.log"; then
  echo "REGEN REFUSED the head run printed no RESULT log=$OUT/head.log"
  exit 1
fi
live_leg || true
if ! grep -q '^LIVE RESULT ' "$OUT/live.log"; then
  echo "REGEN REFUSED the live leg printed no LIVE RESULT log=$OUT/live.log"
  exit 1
fi
mkdir -p "$REGEN_DIR"
committed=test_data/ledger-oracle
manifests="expected-unsupported.txt known.txt eval-known.txt coverage.txt"
for file in $manifests; do
  cat "$OUT/head.log" "$OUT/live.log" | sed -n "s/^CANDIDATE $file //p" >"$REGEN_DIR/$file"
done
changed=$(cd "$ROOT/$SCRATCH/$committed" && git status --porcelain -- $manifests)
if [ -n "$changed" ]; then
  echo "REGEN FAIL the run changed a committed manifest: $changed"
  exit 1
fi
for file in $manifests; do
  lines=$(wc -l <"$REGEN_DIR/$file" | tr -dc 0-9)
  if git diff --no-index -- "$ROOT/$SCRATCH/$committed/$file" "$REGEN_DIR/$file"; then
    echo "REGEN $file lines=$lines same"
  else
    echo "REGEN $file lines=$lines differs"
  fi
done
echo "REGEN tree=unchanged candidates=$REGEN_DIR logs=$OUT"
