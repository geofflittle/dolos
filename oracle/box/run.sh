#!/bin/sh
# Runs the oracle on the build box, its quick, done or ship tier, its live leg alone or the regeneration of its committed manifests.
# Usage: ROOT=dir [REV=ref] [BASE=ref] run.sh quick|done|ship|live|regen|value
# The value tier runs the value leg of value.sh alone. The quick, done and ship tiers run it too when its inputs differ from those of the last passing value leg.
# The quick tier judges the head on both corpora, the control and each arm whose plant changes a file that differs between BASE and REV, a pallas arm when the pallas entries of Cargo.toml or Cargo.lock differ. BASE defaults to the commit of the last done or ship run that passed, and when there is none every arm runs. The done tier, run per change, judges the head, the control and every arm, and fails on a fix commit that arms.toml gives neither an arm nor an exclusion. The ship tier, run before a rollout of the dolos-serve image, is the done tier while the live leg syncs a candidate from genesis. Only the ship tier and the live leg read the live node. The done and ship tiers record their commit in runs/last-done when they pass, and the ship tier and the live leg record theirs in runs/last-ship when the live leg passes.
# The quick, done, ship and value tiers fail when a caught or held arm served other lines than its row of served.txt records, and a change that alters what an arm serves on purpose copies the arm's row from the run's served.txt into served.txt.
# The live leg reads ENDPOINT, NODE, NODE_DB, GRPCURL, SERVE_CONFIG and CANDIDATE_GRPC from $LIVE_ENV.
set -eu
# Prints the run's exit status as its last line, after releasing the box lock when this run holds it.
finish() {
  status=$?
  if command -v release_lock >/dev/null; then
    release_lock
  fi
  echo "run-exit=$status"
}
trap finish EXIT
: "${ROOT:?}"
BASE=${BASE:-}
CANDIDATE_NEED_G=${CANDIDATE_NEED_G:-12}
LAST_DONE="$ROOT/runs/last-done"
LAST_SHIP="$ROOT/runs/last-ship"
HERE=$(cd "$(dirname "$0")" && pwd)
LIVE_ENV=${LIVE_ENV:-$ROOT/live.env}
REGEN_DIR=${REGEN_DIR:-$ROOT/regen}
. "$HERE/lib.sh"
. "$HERE/value.sh"
. "$HERE/amaru.sh"
case $OUT in
  "$ROOT"/*) ;;
  *) echo "REFUSED OUT=$OUT is outside $ROOT" ; exit 2 ;;
esac
case ${1:-} in
  quick | done | ship | live | regen | value) ;;
  *) echo "usage: run.sh quick|done|ship|live|regen|value" ; exit 2 ;;
esac
if [ -z "${JUDGED:-}" ]; then
  REV=$(full_commit REV "$REV")
  if [ -n "$BASE" ]; then
    BASE=$(full_commit BASE "$BASE")
  fi
fi
take_lock
trap finish EXIT
# The tier runs the scripts and plants of the commit it judges, from that
# commit's oracle/box in $OUT/box.
if [ -z "${JUDGED:-}" ]; then
  JUDGED=$REV
  mkdir -p "$OUT/box"
  git -C "$ROOT/dolos.git" archive "$JUDGED" oracle/box | tar -x -C "$OUT/box"
  export JUDGED REV BASE ROOT OUT RUN_LOCK
  exec sh "$OUT/box/oracle/box/run.sh" "$@"
fi
export ROOT REV MIN_FREE_G CANDIDATE_NEED_G OUT RUN_LOCK

# Exits when the test of the box functions or of the field comparison fails.
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

# Exits when the build image cannot be built from the files of images/dolos-build.
require_build_image() {
  if ! build_image dolos-build >"$OUT/build-image.log" 2>&1; then
    echo "REFUSED build image=$(sh "$HERE/image-name.sh" dolos-build) log=$OUT/build-image.log"
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
  grep -E '^(LIVE-CANDIDATE|LIVE-IO|LIVE-FIELD|LIVE-FIELD-SPENT|LIVE-FIELDS|LIVE) ' "$OUT/candidate.log" |
    cut -c1-400 || true
  return "$fields"
}

# Judges the candidate's reads against the node with $OUT/bin/judge, logging to $OUT/live.log.
judge_live() {
  # The judge runs in the run's own dir, since the arms check other commits out in the scratch tree meanwhile.
  [ -f "$OUT/bin/judge" ] && [ -f "$OUT/live/candidate/image.txt" ] &&
    LIVE_DIR="/w/${OUT#"$ROOT"/}/live/candidate" sh "$IN_IMAGE" "${OUT#"$ROOT"/}" \
      "/w/${OUT#"$ROOT"/}/bin/judge" "$ORACLE_TESTS::live::live" --exact --ignored --nocapture \
      >"$OUT/live.log" 2>&1
}

# Prints the live judge's verdict lines from $OUT/live.log.
live_lines() {
  grep -E '^(LIVE|KNOWN|KNOWN_STALE|LIVE-DIFF|LIVE-GAP) ' "$OUT/live.log" | cut -c1-400 || true
}

# Judges a copy of the candidate's reads with the served era start and tip one second late, the reads of a system start one second late, and prints the verdict of the arm that requires both times to disagree with the node.
live_shift() {
  shifted="$OUT/live/shifted"
  rm -rf "$shifted" "$OUT/live-shift.log"
  if cp -R "$OUT/live/candidate" "$shifted" &&
    jq '.cardano.summaries[-1].start.time |= (tonumber + 1000 | tostring)' \
      "$OUT/live/candidate/era.json" >"$shifted/era.json" &&
    jq '.tip.timestamp |= (tonumber + 1000 | tostring)' "$OUT/live/candidate/tip.json" >"$shifted/tip.json"; then
    LIVE_DIR="/w/${OUT#"$ROOT"/}/live/shifted" sh "$IN_IMAGE" "${OUT#"$ROOT"/}" \
      "/w/${OUT#"$ROOT"/}/bin/judge" "$ORACLE_TESTS::live::live" --exact --ignored --nocapture \
      >"$OUT/live-shift.log" 2>&1 || true
    arm_verdict "$OUT/live-shift.log" '^test result: ' "" shifted_times_differ "$OUT/live-shift.log"
  else
    verdict=UNPLANTED
  fi
  echo "live-shift=$verdict"
}

# Whether the live log $1 holds the served era start and the tip each disagreeing with the node on its time.
shifted_times_differ() {
  times='slot=[0-9]+ time=[0-9]+ node_slot=[0-9]+ '
  grep -E "^LIVE-DIFF row=[^ ]* locus=era_summary era=[a-z]+ start_time $times" "$1" &&
    grep -E "^LIVE-DIFF row=[^ ]* locus=era_summary tip $times" "$1"
}

# Builds the candidate and the live judge, syncs the candidate and judges its reads.
live_leg() {
  build_candidate && build_live_judge || return 1
  fields=0
  sync_candidate || fields=$?
  judge_live && [ "$fields" -eq 0 ]
}

# Runs the live leg alone at REV, and records the commit in runs/last-ship when it passes.
live_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  require_build_image
  candidate_fsync >/dev/null || exit 2
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  live=FAIL
  if live_leg; then
    live=PASS
  fi
  live_lines
  shift_verdict=$(live_shift | tail -1)
  echo "LIVE-LEG live=$live $shift_verdict commit=$commit logs=$OUT"
  record_pass live "$commit" "live=$live $shift_verdict"
}

# Judges the head, the control and the arms the change from BASE touches, every arm when no BASE is known.
quick_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  require_build_image
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
    touched_arms "$base" "$commit" >"$OUT/arms"
    arms=$(paste -sd ' ' - <"$OUT/arms")
  fi
  echo "QUICK base=$base arms=\"$arms\""
  ARMS="head control${arms:+ $arms}" sh "$HERE/breaks.sh" | tee "$OUT/breaks.out"
  verdicts=$(sed -n 's/^ARMS \(.*\) logs=.*/\1/p' "$OUT/breaks.out")
  verdicts="$verdicts $(due_value_verdicts)"
  echo "DONE tier=quick $verdicts base=$base commit=$commit logs=$OUT"
  passed "$verdicts"
}

# Runs the value and amaru legs over a reset scratch tree, printing their lines, returning their verdicts on the last line and recording their inputs in LAST_VALUE when both pass.
value_verdicts() {
  reset_scratch >&2
  pin=$(pallas_pin)
  value_leg >"$OUT/value.out" 2>&1 || true
  amaru_leg "$pin" >"$OUT/amaru.out" 2>&1 || true
  cat "$OUT/value.out" "$OUT/amaru.out" >&2
  reset_scratch >&2
  legs="$(sed -n 's/^VALUE \(value-head=.*\) pin=.*/\1/p' "$OUT/value.out" | grep . || echo value-head=FAIL) $(
    sed -n 's/^AMARU \(amaru-head=[A-Z]*\( amaru-[a-z0-9-]*=[A-Z]*\)*\).*/\1/p' "$OUT/amaru.out" | grep . || echo amaru-head=FAIL)"
  served_check "$legs" >&2
  legs=$checked
  case " $legs " in
    *" value-head=PASS "*" amaru-head=PASS "*)
      if passed "head=PASS $legs"; then
        value_inputs "$pin" >"$LAST_VALUE"
      fi
      ;;
  esac
  echo "$legs"
}

# Runs the value and amaru legs when their inputs differ from those of the last run in which both passed, printing their verdicts, or the inputs that passed.
due_value_verdicts() {
  pin=$(pallas_pin)
  if value_due "$pin"; then
    value_verdicts
  else
    echo "VALUE due=no $(cat "$LAST_VALUE")" >&2
    echo "value=passed-on-these-inputs"
  fi
}

# Judges the pinned pallas with the value leg alone.
value_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  require_build_image
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  verdicts=$(value_verdicts)
  echo "DONE tier=value $verdicts commit=$commit logs=$OUT"
  case " $verdicts " in
    *" value-head=PASS "*) ;;
    *) return 1 ;;
  esac
  passed "head=PASS $verdicts"
}

# Judges the head, the control and every arm, and records the commit when every verdict passes.
done_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  require_build_image
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  fix_arms=fix-arms=PASS
  fix_arms_both >"$OUT/fix-arms.out" 2>&1 || fix_arms=fix-arms=FAIL
  cat "$OUT/fix-arms.out"
  sh "$HERE/breaks.sh" | tee "$OUT/breaks.out"
  verdicts=$(sed -n 's/^ARMS \(.*\) logs=.*/\1/p' "$OUT/breaks.out")
  verdicts="$verdicts $fix_arms $(due_value_verdicts)"
  echo "DONE tier=done $verdicts commit=$commit logs=$OUT"
  record_pass done "$commit" "$verdicts"
}

# Builds the candidate, syncs it while the arms run, judges its reads, and records the commit in runs/last-done and runs/last-ship when every verdict passes.
ship_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  require_build_image
  candidate_fsync >/dev/null || exit 2
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
  verdicts="$verdicts $(due_value_verdicts)"
  live_lines
  shift_verdict=$(live_shift | tail -1)
  echo "DONE tier=ship $verdicts $shift_verdict live=$live commit=$commit logs=$OUT"
  record_pass ship "$commit" "$verdicts $shift_verdict live=$live"
}

regen_run() {
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
}

"${1}_run"
