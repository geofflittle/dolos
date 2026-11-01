#!/bin/sh
# Runs the oracle on the build box, its quick, done or ship tier, its live leg alone or the regeneration of its committed manifests.
# Usage: [REV=ref] [BASE=ref] run.sh quick|done|ship|live|regen
# The quick tier judges the head on both corpora, the control and each arm whose plant changes a file that differs between BASE and REV, a pallas arm when Cargo.toml or Cargo.lock differ. BASE defaults to the commit of the last done or ship run that passed, and when there is none every arm runs. The done tier judges the head, the control and every arm. The ship tier, run once before a rollout, is the done tier while the live leg runs. Only the ship tier and the live leg read the live node, and the done and ship tiers record their commit when they pass.
# The live leg reads ENDPOINT, NODE, NODE_DB, GRPCURL, SERVE_CONFIG and CANDIDATE_GRPC from $LIVE_ENV.
set -eu
ROOT=${ROOT:-/opt/build/ledger-oracle-1027}
REV=${REV:-feat/ledger-oracle}
BASE=${BASE:-}
MIN_FREE_G=${MIN_FREE_G:-6}
CANDIDATE_NEED_G=${CANDIDATE_NEED_G:-12}
LAST_DONE="$ROOT/runs/last-done"
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
# The tier runs the scripts and plants of the commit it judges, from that
# commit's oracle/box in $OUT/box.
if [ -z "${JUDGED:-}" ]; then
  JUDGED=$(git -C "$ROOT/dolos.git" rev-parse --verify "$REV^{commit}")
  mkdir -p "$OUT/box"
  git -C "$ROOT/dolos.git" archive "$JUDGED" oracle/box | tar -x -C "$OUT/box"
  REV=$JUDGED
  export JUDGED REV ROOT OUT
  exec sh "$OUT/box/oracle/box/run.sh" "$@"
fi
export ROOT REV MIN_FREE_G CANDIDATE_NEED_G OUT
. "$HERE/lib.sh"

# Exits when the test of the stale unit check or of the field comparison fails.
require_lib_test() {
  mkdir -p "$OUT/lib-test" "$OUT/fields-test"
  if ! sh "$HERE/lib_test.sh" "$OUT/lib-test" >"$OUT/lib-test.log" 2>&1; then
    echo "REFUSED lib_test log=$OUT/lib-test.log"
    exit 2
  fi
  if ! sh "$HERE/fields_test.sh" "$OUT/fields-test" >"$OUT/fields-test.log" 2>&1; then
    echo "REFUSED fields_test log=$OUT/fields-test.log"
    exit 2
  fi
}

# Builds Dolos from the scratch tree into $OUT/bin/dolos.
build_candidate() {
  mkdir -p "$OUT/bin"
  if ! built=$(build_checked "$OUT/bin/dolos.build" build --release --locked --bin dolos); then
    echo "LIVE-CANDIDATE build_failed $built"
    return 1
  fi
  dolos=$(executable "$OUT/bin/dolos.build")
  cp "$ROOT${dolos#/w}" "$OUT/bin/dolos"
}

# Builds the live judge from the scratch tree into $OUT/bin/judge.
build_live_judge() {
  mkdir -p "$OUT/bin"
  if ! built=$(build_judge "$OUT/bin/judge.build" --locked); then
    echo "LIVE-CANDIDATE judge_build_failed $built"
    return 1
  fi
  judge=$(executable "$OUT/bin/judge.build")
  cp "$ROOT${judge#/w}" "$OUT/bin/judge"
}

# Syncs the candidate in $OUT/bin/dolos from the node and compares its reads with the serving Dolos field by field, logging to $OUT/candidate.log.
sync_candidate() {
  set -a
  . "$LIVE_ENV"
  set +a
  fields=0
  CANDIDATE="$OUT/bin/dolos" COMMIT=$commit sh "$HERE/candidate.sh" "$OUT/live" \
    >"$OUT/candidate.log" 2>&1 || fields=$?
  grep -E '^(LIVE-CANDIDATE|LIVE-FIELD|LIVE-FIELD-SPENT|LIVE-FIELDS|LIVE) ' "$OUT/candidate.log" |
    cut -c1-400 || true
  return "$fields"
}

# Judges the candidate's reads against the node with $OUT/bin/judge, logging to $OUT/live.log.
judge_live() {
  # The judge runs in the run's own dir, since the arms check other commits out in the scratch tree meanwhile.
  [ -f "$OUT/bin/judge" ] && [ -f "$OUT/live/candidate/image.txt" ] &&
    LIVE_DIR="/w/${OUT#"$ROOT"/}/live/candidate" sh "$HERE/in-image.sh" "${OUT#"$ROOT"/}" \
      "/w/${OUT#"$ROOT"/}/bin/judge" "$TEST::live::live" --exact --ignored --nocapture \
      >"$OUT/live.log" 2>&1
}

# Builds the candidate and the live judge, syncs the candidate and judges its reads.
live_leg() {
  build_candidate && build_live_judge || return 1
  fields=0
  sync_candidate || fields=$?
  judge_live && [ "$fields" -eq 0 ]
}

# Runs the live leg alone at REV.
live_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  live=FAIL
  if live_leg; then
    live=PASS
  fi
  grep -E '^(LIVE|KNOWN|KNOWN_STALE|LIVE-DIFF|LIVE-GAP) ' "$OUT/live.log" | cut -c1-400 || true
  echo "LIVE-LEG live=$live commit=$commit logs=$OUT"
  [ "$live" = PASS ]
}

# The arms whose plant changes a file that differs between the commits $1 and $2 of the scratch tree, one per line.
touched_arms() {
  git -C "$ROOT/$SCRATCH" diff --name-only "$1" "$2" >"$OUT/changed"
  for arm in $PLANTED_ARMS; do
    if plant_files "$arm" | grep -q -x -F -f "$OUT/changed"; then
      echo "$arm"
    fi
  done
}

# Judges the head, the control and the arms the change from BASE touches, every arm when no BASE is known.
quick_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  if [ -z "$BASE" ] && [ -f "$LAST_DONE" ]; then
    BASE=$(cat "$LAST_DONE")
  fi
  base=none
  arms=$PLANTED_ARMS
  if [ -n "$BASE" ]; then
    git -C "$ROOT/$SCRATCH" fetch -q "$ROOT/dolos.git" "$BASE"
    base=$(git -C "$ROOT/$SCRATCH" rev-parse FETCH_HEAD)
    arms=$(touched_arms "$base" "$commit" | paste -sd ' ' -)
  fi
  echo "QUICK base=$base arms=\"$arms\""
  ARMS="head control${arms:+ $arms}" sh "$HERE/breaks.sh" | tee "$OUT/breaks.out"
  verdicts=$(sed -n 's/^ARMS \(.*\) logs=.*/\1/p' "$OUT/breaks.out")
  echo "DONE tier=quick $verdicts base=$base commit=$commit logs=$OUT"
  passed "$verdicts"
}

# Whether a verdict list holds a passing head and no failed arm or leg.
passed() {
  case " $1 " in
    *" head=PASS "*) ;;
    *) return 1 ;;
  esac
  case "$1" in
    *MISSED* | *FAIL* | *FIRED* | *SKIPPED*) return 1 ;;
  esac
}

# Judges the head, the control and every arm, and records the commit when every verdict passes.
done_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  sh "$HERE/breaks.sh" | tee "$OUT/breaks.out"
  verdicts=$(sed -n 's/^ARMS \(.*\) logs=.*/\1/p' "$OUT/breaks.out")
  echo "DONE tier=done $verdicts commit=$commit logs=$OUT"
  passed "$verdicts"
  echo "$commit" >"$LAST_DONE"
}

# Builds the candidate, syncs it while the arms run, judges its reads, and records the commit when every verdict passes.
ship_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  sync=
  if build_candidate >"$OUT/live-leg.out" 2>&1; then
    sync_candidate >>"$OUT/live-leg.out" 2>&1 &
    sync=$!
  fi
  build_live_judge >>"$OUT/live-leg.out" 2>&1 || true
  sh "$HERE/breaks.sh" | tee "$OUT/breaks.out"
  fields=1
  if [ -n "$sync" ] && wait "$sync"; then
    fields=0
  fi
  live=FAIL
  if judge_live && [ "$fields" -eq 0 ]; then
    live=PASS
  fi
  cat "$OUT/live-leg.out"
  verdicts=$(sed -n 's/^ARMS \(.*\) logs=.*/\1/p' "$OUT/breaks.out")
  grep -E '^(LIVE|KNOWN|KNOWN_STALE|LIVE-DIFF|LIVE-GAP) ' "$OUT/live.log" | cut -c1-400 || true
  echo "DONE tier=ship $verdicts live=$live commit=$commit logs=$OUT"
  passed "$verdicts live=$live"
  echo "$commit" >"$LAST_DONE"
}

regen() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  if ! built=$(build_judge "$OUT/head.build" --locked); then
    echo "REGEN REFUSED the judge build $built"
    exit 1
  fi
  sh "$IN_IMAGE" "$SCRATCH" "$(executable "$OUT/head.build")" "$TEST::ledger_oracle" \
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
  quick) quick_run ;;
  done) done_run ;;
  ship) ship_run ;;
  live) live_run ;;
  regen) regen ;;
  *) echo "usage: run.sh quick|done|ship|live|regen" ; exit 2 ;;
esac
