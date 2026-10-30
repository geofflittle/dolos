#!/bin/sh
# Runs the oracle on the build box, its done run, its live leg alone or the regeneration of its committed manifests.
# Usage: [REV=ref] run.sh done|live|regen
# The live leg reads ENDPOINT, NODE, NODE_DB, GRPCURL, SERVE_CONFIG and CANDIDATE_GRPC from $LIVE_ENV.
set -eu
ROOT=${ROOT:-/opt/build/ledger-oracle-1027}
REV=${REV:-feat/ledger-oracle}
MIN_FREE_G=${MIN_FREE_G:-6}
CANDIDATE_NEED_G=${CANDIDATE_NEED_G:-20}
HERE=$(cd "$(dirname "$0")" && pwd)
LIVE_ENV=${LIVE_ENV:-$ROOT/live.env}
REGEN_DIR=${REGEN_DIR:-$ROOT/regen}
SCRATCH=scratch/dolos
TEST=serve::grpc::v1::ledger_oracle_tests
OUT=${OUT:-"$ROOT/runs/$(date -u +%Y%m%dT%H%M%SZ)"}
case $OUT in
  "$ROOT"/*) ;;
  *) echo "REFUSED OUT=$OUT is outside $ROOT" ; exit 2 ;;
esac
export ROOT REV MIN_FREE_G CANDIDATE_NEED_G OUT
. "$HERE/lib.sh"

# Builds Dolos and its live judge from the scratch tree into $OUT/bin, freeing the target dir when the candidate store would not fit beside it.
build_candidate() {
  mkdir -p "$OUT/bin"
  if ! sh "$HERE/in-image.sh" "$SCRATCH" cargo build -q --release --locked --bin dolos \
    >"$OUT/bin/build.log" 2>&1; then
    echo "LIVE-CANDIDATE build_failed log=$OUT/bin/build.log"
    return 1
  fi
  cp "$ROOT/target/release/dolos" "$OUT/bin/dolos"
  judge=$(sh "$HERE/in-image.sh" "$SCRATCH" cargo test --release --locked -p dolos --lib --no-run 2>&1 |
    sed -n 's#.*Executable unittests src/lib.rs (/w/\(.*\))#\1#p')
  if [ -z "$judge" ]; then
    echo "LIVE-CANDIDATE judge_build_failed"
    return 1
  fi
  cp "$ROOT/$judge" "$OUT/bin/judge"
  free=$(df -BG --output=avail "$ROOT" | tail -1 | tr -dc 0-9)
  if [ "$free" -lt $((CANDIDATE_NEED_G + 3)) ]; then
    echo "LIVE-CANDIDATE freed target free=${free}G size=$(du -sh "$ROOT/target" | cut -f1)"
    rm -rf "${ROOT:?}/target"
  fi
}

# Syncs a candidate of the scratch tree's commit beside the serving Dolos, compares their reads field by field and judges the candidate's reads against the node, logging to $OUT/candidate.log and $OUT/live.log.
live_leg() {
  set -a
  . "$LIVE_ENV"
  set +a
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  build_candidate || return 1
  fields=0
  CANDIDATE="$OUT/bin/dolos" COMMIT=$commit sh "$HERE/candidate.sh" "$OUT/live" \
    >"$OUT/candidate.log" 2>&1 || fields=$?
  grep -E '^(LIVE-CANDIDATE|LIVE-FIELD|LIVE-FIELDS|LIVE) ' "$OUT/candidate.log" |
    cut -c1-400 || true
  [ -f "$OUT/live/candidate/image.txt" ] &&
    LIVE_DIR="/w/${OUT#"$ROOT"/}/live/candidate" sh "$HERE/in-image.sh" "$SCRATCH" \
      "/w/${OUT#"$ROOT"/}/bin/judge" "$TEST::live::live" --exact --ignored --nocapture \
      >"$OUT/live.log" 2>&1 &&
    [ "$fields" -eq 0 ]
}

# Runs the live leg alone at REV.
live_run() {
  mkdir -p "$OUT"
  require_free
  reset_scratch
  live=FAIL
  if live_leg; then
    live=PASS
  fi
  grep -E '^(LIVE|KNOWN|KNOWN_STALE|LIVE-DIFF|LIVE-GAP) ' "$OUT/live.log" | cut -c1-400 || true
  echo "LIVE-LEG live=$live commit=$commit logs=$OUT"
  [ "$live" = PASS ]
}

done_run() {
  mkdir -p "$OUT"
  sh "$HERE/breaks.sh" | tee "$OUT/breaks.out"
  arms=$(sed -n 's/^ARMS \(.*\) logs=.*/\1/p' "$OUT/breaks.out")
  live=FAIL
  if live_leg; then
    live=PASS
  fi
  grep -E '^(LIVE|KNOWN|KNOWN_STALE|LIVE-DIFF|LIVE-GAP) ' "$OUT/live.log" | cut -c1-400 || true
  echo "DONE $arms live=$live commit=$commit logs=$OUT"
  case " $arms " in
    *" head=PASS "*) ;;
    *) exit 1 ;;
  esac
  case "$arms live=$live" in
    *MISSED* | *FAIL* | *SKIPPED*) exit 1 ;;
  esac
}

regen() {
  mkdir -p "$OUT"
  require_free
  reset_scratch
  sh "$HERE/in-image.sh" "$SCRATCH" cargo test -q --release --locked -p dolos --lib \
    "$TEST::ledger_oracle" -- --exact --ignored --nocapture >"$OUT/head.log" 2>&1 || true
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
  for file in expected-unsupported.txt known.txt; do
    cat "$OUT/head.log" "$OUT/live.log" | sed -n "s/^CANDIDATE $file //p" >"$REGEN_DIR/$file"
  done
  changed=$(git -C "$ROOT/$SCRATCH" status --porcelain -- \
    "$committed/expected-unsupported.txt" "$committed/known.txt")
  if [ -n "$changed" ]; then
    echo "REGEN FAIL the run changed a committed manifest: $changed"
    exit 1
  fi
  for file in expected-unsupported.txt known.txt; do
    lines=$(wc -l <"$REGEN_DIR/$file" | tr -dc 0-9)
    if git diff --no-index -- "$ROOT/$SCRATCH/$committed/$file" "$REGEN_DIR/$file"; then
      echo "REGEN $file lines=$lines same"
    else
      echo "REGEN $file lines=$lines differs"
    fi
  done
  echo "REGEN tree=unchanged candidates=$REGEN_DIR logs=$OUT"
}

case ${1:-} in
  done) done_run ;;
  live) live_run ;;
  regen) regen ;;
  *) echo "usage: run.sh done|live|regen" ; exit 2 ;;
esac
