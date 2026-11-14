#!/bin/sh
# Runs one tier of the oracle at REV, and oracle/README.md says what each tier checks and when to run it.
# Usage: ROOT=dir [REV=ref] [BASE=ref] run.sh quick|done|ship
# The quick tier judges the arms whose plants change a file that differs between BASE and REV, and BASE defaults to the commit of the last done or ship run that passed. The ship tier also syncs the image of REV from genesis against the node and judges its reads. The done and ship tiers record their commit in runs/last-done when they pass, and the ship tier records it and the image in runs/last-ship too.
# A tier fails when a caught or held arm served other lines than its row of served.txt records, and a change that alters what an arm serves on purpose copies the arm's row from the run's served.txt into served.txt.
set -eu
: "${ROOT:?}"
BASE=${BASE:-}
LAST_DONE="$ROOT/runs/last-done"
LAST_SHIP="$ROOT/runs/last-ship"
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/lib.sh"
. "$HERE/value.sh"
. "$HERE/amaru.sh"
trap finish EXIT
case ${1:-} in
  quick | done | ship) ;;
  *) echo "usage: run.sh quick|done|ship" ; exit 2 ;;
esac
start_run "$0" "$@"

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
    live_shift_verdict "$OUT/live-shift.log"
  else
    verdict=UNPLANTED
  fi
  echo "live-shift=$verdict"
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
  verdicts="$verdicts $(value_verdicts)"
  echo "DONE tier=quick $verdicts base=$base commit=$commit logs=$OUT"
  passed "$verdicts"
}

# Sets checked to the verdicts of the leg $1 whose run wrote $OUT/$1.out and whose verdict line the sed script $2 prints, its head failing when none does, records the inputs $4 in the file $3 when every verdict passes, and prints the leg's lines.
leg_verdicts() {
  cat "$OUT/$1.out"
  served_check "$(sed -n "$2" "$OUT/$1.out" | grep . || echo "$1-head=FAIL")"
  if passed "head=PASS $checked"; then
    echo "$4" >"$3"
  fi
}

# Runs over a reset scratch tree the value leg and the amaru leg, each when its inputs differ from those of its last passing run, printing their lines and returning their verdicts on the last line, a leg not run as its name and passed-on-these-inputs.
value_verdicts() {
  reset_scratch >&2
  pin=$(pallas_pin)
  inputs=$(value_inputs "$pin")
  if leg_due "$LAST_VALUE" "$inputs"; then
    value_leg >"$OUT/value.out" 2>&1 || true
    leg_verdicts value 's/^VALUE \(value-head=.*\) pin=.*/\1/p' "$LAST_VALUE" "$inputs" >&2
    legs=$checked
  else
    echo "VALUE due=no $inputs" >&2
    legs=value=passed-on-these-inputs
  fi
  pallas_history "$pin" >&2 || true
  inputs=$(amaru_inputs "$(amaru_pin "$pin")" "$(image_id "$BUILD_IMAGE")")
  if leg_due "$LAST_AMARU" "$inputs"; then
    amaru_leg "$pin" >"$OUT/amaru.out" 2>&1 || true
    leg_verdicts amaru 's/^AMARU \(amaru-head=[A-Z]*\( amaru-[a-z0-9-]*=[A-Z]*\)*\).*/\1/p' "$LAST_AMARU" "$inputs" >&2
    legs="$legs $checked"
  else
    echo "AMARU due=no $inputs" >&2
    legs="$legs amaru=passed-on-these-inputs"
  fi
  reset_scratch >&2
  echo "$legs"
}

# Judges the fix arms, the head, the control and every arm, prints their lines and sets verdicts to their verdicts and those of the value and amaru legs.
judge_arms() {
  fix_arms=fix-arms=PASS
  fix_arms_both >"$OUT/fix-arms.out" 2>&1 || fix_arms=fix-arms=FAIL
  cat "$OUT/fix-arms.out"
  sh "$HERE/breaks.sh" | tee "$OUT/breaks.out"
  verdicts=$(sed -n 's/^ARMS \(.*\) logs=.*/\1/p' "$OUT/breaks.out")
  verdicts="$verdicts $fix_arms $(value_verdicts)"
}

# Judges the head, the control and every arm, and records the commit when every verdict passes.
done_run() {
  mkdir -p "$OUT"
  require_free
  require_lib_test
  require_build_image
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  judge_arms
  echo "DONE tier=done $verdicts commit=$commit logs=$OUT"
  record_pass done "$commit" "$verdicts"
}

# Judges what the done tier judges and the reads of REV's image synced from genesis against the node, and records the commit in runs/last-done and runs/last-ship, and the image in runs/last-ship, when every verdict passes.
ship_run() {
  mkdir -p "$OUT"
  require_free
  require_image
  require_lib_test
  require_build_image
  reset_scratch
  commit=$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)
  rm -rf "$OUT/live"
  read_candidate
  build_live_judge >"$OUT/live-leg.out" 2>&1 || true
  judge_arms
  live=FAIL
  if judge_live && [ "$candidate" = PASS ]; then
    live=PASS
  fi
  cat "$OUT/live-leg.out"
  candidate_lines "$OUT/live"
  live_lines
  shift_verdict=$(live_shift | tail -1)
  echo "DONE tier=ship $verdicts $shift_verdict live=$live image=$image commit=$commit logs=$OUT"
  record_pass ship "$commit" "$verdicts $shift_verdict live=$live" "$image"
}

"${1}_run"
