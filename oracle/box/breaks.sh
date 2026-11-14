#!/bin/sh
# Judges the oracle branch head on both corpora, the control (the head on every arm's records) and each arm on the records plants/<arm>.records names, one oracle run each.
# Usage: ROOT=dir [REV=ref] [ARMS="head control colret coin ..."] breaks.sh
# The branch is read from the bare repository $ROOT/dolos.git.
# An arm whose switch overlay/ adds runs on one build of the overlay with ORACLE_PLANT naming it, and an arm with a plants/<arm>.deselect judges the coverage of the head's counts without the counts of the records it names.
set -eu
: "${ROOT:?}"
export ROOT
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/lib.sh"
TEST=$ORACLE_TESTS::ledger_oracle
mkdir -p "$OUT"
enter_slice "$0" "$@"
# The number of switched arms judged at once.
ARM_JOBS=${ARM_JOBS:-$(arm_jobs)}
take_lock
ARMS=$(arm_order ${ARMS:-head control $PLANTED_ARMS} | paste -sd ' ' -)
# The arms of ARMS whose crate test the control and the overlay build.
CRATE_ARMS=$(crate_arms $ARMS)
unset ORACLE_PLANT ORACLE_DESELECT ORACLE_HEAD_COUNTS
require_free

for arm in $ARMS; do
  case " head control $PLANTED_ARMS " in
    *" $arm "*) ;;
    *) echo "unknown arm $arm" ; exit 2 ;;
  esac
done
mismatches=$(arm_mismatches)
if [ -n "$mismatches" ]; then
  echo "$mismatches"
  exit 2
fi

# The source cargo resolves pallas-validate from, as the scratch tree's build sees it.
pallas_source() {
  sh "$IN_IMAGE" "$SCRATCH" cargo tree -q -i pallas-validate --depth 0 -e normal 2>/dev/null |
    head -1 | sed -n 's/.*(\(.*\)).*/\1/p'
}

# Runs the crate test of an arm with the executable $3, or one it builds over the scratch trees as they stand, its output in $2, failing when cargo refused the build.
run_crate_test() {
  test_bin=${3:-}
  if [ -z "$test_bin" ]; then
    if ! built=$(build_checked "$2.build" test --release $(crate_test "$1") --no-run --locked); then
      echo "BUILD_REFUSED $built" >"$2"
      return 1
    fi
    test_bin=$(executable "$2.build" "$(crate_test_target "$1")")
  fi
  RUST_BACKTRACE=0 sh "$IN_IMAGE" "$SCRATCH" "$test_bin" $(crate_test_filter "$1") --nocapture >"$2" 2>&1 || true
}

overlay=
# The switches of overlay/ whose hunks did not apply.
overlay_unplanted=
# Builds the judge and each switched crate test ARMS selects once, the joint crate tests in the judge's cargo invocation, from the overlay trees, which hold the scratch tree and the pinned pallas with overlay/ applied, and copies them to $OUT/bin, setting overlay to built or to the verdict of every switched arm when it fails.
build_overlay() {
  case $overlay in
    built) return 0 ;;
    UNPLANTED | UNBUILT) return 1 ;;
  esac
  overlay=UNPLANTED
  mkdir -p "$OUT/bin"
  if ! pallas_history "$pin" >"$OUT/overlay.plant" 2>&1 ||
    ! unplanted=$({ plant_tree "$OVERLAY_PALLAS" "$ROOT/$PALLAS" "$pin" "" "$HERE/overlay/pallas.diff" &&
      plant_tree "$OVERLAY_DOLOS" "$ROOT/$SCRATCH" "$(git -C "$ROOT/$SCRATCH" rev-parse HEAD)" "$OVERLAY_PALLAS" \
        "$HERE/overlay/dolos.diff"; } 2>>"$OUT/overlay.plant"); then
    echo "OVERLAY PLANT_FAILED log=$OUT/overlay.plant"
    return 1
  fi
  sed -n 's/^REGEN /OVERLAY regen /p' "$OUT/overlay.plant"
  overlay_unplanted=$(echo $unplanted)
  if [ -n "$overlay_unplanted" ]; then
    echo "OVERLAY UNPLANTED switches=\"$overlay_unplanted\" log=$OUT/overlay.plant"
  fi
  source=$(in_overlay pallas_source)
  if [ "$source" != "/w/$OVERLAY_PALLAS/pallas-validate" ]; then
    echo "OVERLAY REFUSED pallas_source=$source"
    return 1
  fi
  overlay=UNBUILT
  overlay_started=$(date +%s)
  if ! built=$(in_overlay build_judge "$OUT/overlay.build" $(joint_packages $CRATE_ARMS)); then
    echo "OVERLAY BUILD_REFUSED $built"
    return 1
  fi
  built=$(executable "$OUT/overlay.build" dolos)
  cp "$ROOT${built#/w}" "$OUT/bin/overlay-judge"
  for switch in $CRATE_ARMS; do
    if overlay_arm "$switch" && joint_crate_test "$switch"; then
      built=$(executable "$OUT/overlay.build" "$(crate_test_target "$switch")")
      cp "$ROOT${built#/w}" "$OUT/bin/overlay-$switch"
    fi
  done
  for switch in $CRATE_ARMS; do
    if overlay_arm "$switch" && ! joint_crate_test "$switch"; then
      if ! built=$(in_overlay build_checked "$OUT/overlay-$switch.build" test --release $(crate_test "$switch") --no-run); then
        echo "OVERLAY BUILD_REFUSED $built"
        return 1
      fi
      built=$(executable "$OUT/overlay-$switch.build" "$(crate_test_target "$switch")")
      cp "$ROOT${built#/w}" "$OUT/bin/overlay-$switch"
    fi
  done
  echo "OVERLAY dolos=$(git -C "$ROOT/$SCRATCH" rev-parse --short HEAD) pallas=$pin_short-overlay pallas_source=$source build_seconds=$(($(date +%s) - overlay_started))"
  overlay=built
}

# Runs the overlay build on an arm's records, or its crate test, with ORACLE_PLANT set to $2 when it is given, its output in $3.
run_overlay() {
  bin="/w/${OUT#"$ROOT"/}/bin"
  if crate_test "$1" >/dev/null; then
    ORACLE_PLANT=$2 RUST_BACKTRACE=0 sh "$IN_IMAGE" "$OVERLAY_DOLOS" "$bin/overlay-$1" $(crate_test_filter "$1") --nocapture >"$3" 2>&1 || true
  else
    ORACLE_PLANT=$2 ORACLE_RECORDS=$4 RUST_BACKTRACE=0 sh "$IN_IMAGE" "$OVERLAY_DOLOS" "$bin/overlay-judge" "${TEST}_records" \
      --exact --ignored --nocapture >"$3" 2>&1 || true
  fi
}

# Whether an arm's log holds a line its catch selects on one of its records and the head log lacks, printing each such line.
caught() {
  arm=$1
  at=$3
  verdict_catch "$arm" "$2" "$HERE/plants/$arm.records" "$OUT/head.log" >"$at.new" || return 1
  # The subouts arm counts only records that no known row names.
  if [ "$arm" = subouts ]; then
    sed -n 's/^[^ ]* [^ ]* \([a-z]*#[0-9]*\).*/ record=\1 /p' "$ROOT/$SCRATCH/test_data/ledger-oracle/known.txt" >"$OUT/known.ids"
    grep -v -F -f "$OUT/known.ids" "$at.new" >"$at.unknown" || true
    mv "$at.unknown" "$at.new"
  fi
  cat "$at.new"
  [ -s "$at.new" ]
}

# Sets verdict for the arm $1 from its log $2 against the head's log, requiring the reached line of the plant $3 when it is given.
judge_verdict() {
  log=$OUT/head.log
  if arm_ran "$log" '^test result: ' ""; then
    log=$2
    arm_verdict "$1" "$log" '^test result: ' "$3" "$(judged_pattern "$1")" caught "$1" "$log" "$OUT/$1"
  fi
  case $verdict in
    UN*) echo "ARM $1 $verdict log=$log" ;;
    *) head -4 "$OUT/$1.new" ;;
  esac
}

# Whether the unplanted head judges every planted arm's records, and runs each crate test ARMS selects, with a pass and no arm's line, the case where each arm must not fire.
control_clean() {
  if ! grep -q '^RESULT PASS' "$OUT/control.log"; then
    echo "ARM control RESULT not PASS log=$OUT/control.log"
    return 1
  fi
  clean=0
  mkdir -p "$OUT/control"
  for a in $PLANTED_ARMS; do
    log=$OUT/control.log
    if crate_test "$a" >/dev/null; then
      case " $CRATE_ARMS " in
        *" $a "*) ;;
        *) continue ;;
      esac
      log=$OUT/control-$a.log
      if ! crate_test_passed "$a" "$log"; then
        echo "ARM control CRATE_TEST_FAILED arm=$a log=$log"
        clean=1
      fi
    fi
    if caught "$a" "$log" "$OUT/control/$a" >/dev/null; then
      echo "ARM control FIRED arm=$a lines=$OUT/control/$a.new"
      clean=1
    fi
  done
  return $clean
}

# Sets verdict to UNRUN for the first of the logs given that holds no finished run, printing its name, and fails then.
logs_ran() {
  for log; do
    if ! arm_ran "$log" '^test result: ' ""; then
      echo "ARM control UNRUN log=$log"
      return 1
    fi
  done
}

# Sets control to the verdict of the overlay build with no switch set against the head's control logs, on every planted arm's records and in each switched crate test ARMS selects: the overlay's verdict when it is not built, UNRUN when a log holds no finished run, FIRED when a log differs and CLEAN when none does.
overlay_control() {
  if ! build_overlay; then
    echo "ARM control OVERLAY_$overlay"
    control=$overlay
    return
  fi
  run_overlay control "" "$OUT/overlay-control.log" "$records"
  logs=$OUT/overlay-control.log
  for a in $CRATE_ARMS; do
    if overlay_arm "$a"; then
      run_overlay "$a" "" "$OUT/overlay-control-$a.log"
      logs="$logs $OUT/overlay-control-$a.log"
    fi
  done
  if ! logs_ran $logs; then
    control=$verdict
    return
  fi
  control=CLEAN
  if ! same_log "$OUT/control.log" "$OUT/overlay-control.log"; then
    echo "ARM control OVERLAY_DIFFERS log=$OUT/overlay-control.log"
    control=FIRED
  fi
  for a in $CRATE_ARMS; do
    if overlay_arm "$a" && ! same_crate_log "$OUT/control-$a.log" "$OUT/overlay-control-$a.log"; then
      echo "ARM control OVERLAY_DIFFERS log=$OUT/overlay-control-$a.log"
      control=FIRED
    fi
  done
}

# Whether an arm's plant changes only how values are written, so it must hold rather than be caught.
encoding_arm() {
  [ "$1" = ctx-encoding ]
}

# Judges an encoding arm's records on the overlay with no switch set and sets verdict to the arm's verdict against that run, recording the arm's CONTEXT lines as what it served.
held() {
  arm_ran "$OUT/$1.log" '^test result: ' "$1" "$(judged_pattern "$1")" || return 0
  run_overlay "$1" "" "$OUT/$1.unplanted.log" "$(paste -sd, "$HERE/plants/$1.records")"
  grep -h '^CONTEXT ' "$OUT/$1.unplanted.log" "$OUT/$1.log" | sed "s/^/ARM $1 /" || true
  arm_ran "$OUT/$1.unplanted.log" '^test result: ' "" || return 0
  verdict=$(encoding_verdict "$OUT/$1.log" "$OUT/$1.unplanted.log")
  grep '^CONTEXT ' "$OUT/$1.log" | record_served "$1"
}

overlay_arms=
for arm in $ARMS; do
  if overlay_arm "$arm"; then
    overlay_arms="$overlay_arms $arm"
  fi
done

# Judges the switched arm $1 on the overlay build, with the panic sites it served read from the overlay's Dolos tree, printing its lines and writing its verdict to $OUT/$1.verdict.
judge_switched() (
  SCRATCH=$OVERLAY_DOLOS
  arm=$1
  echo "ARM $arm dolos=$dolos_rev pallas=$pin_short-overlay switch=$arm"
  case " $overlay_unplanted " in
    *" $arm "*)
      echo "ARM $arm UNPLANTED log=$OUT/overlay.plant"
      verdict=UNPLANTED
      ;;
    *)
      started=$(date +%s)
      run_overlay "$arm" "$arm" "$OUT/$arm.log" "$(paste -sd, "$HERE/plants/$arm.records")"
      echo "ARM $arm records=$(grep -c . "$HERE/plants/$arm.records" || true) judge_seconds=$(($(date +%s) - started))"
      if encoding_arm "$arm"; then
        held "$arm"
        case $verdict in
          UN*) echo "ARM $arm $verdict log=$OUT/$arm.log" ;;
        esac
      else
        judge_verdict "$arm" "$OUT/$arm.log" "$arm"
      fi
      ;;
  esac
  echo "$arm=$verdict"
  echo "$verdict" >"$OUT/$arm.verdict"
)

switched_batch=
# Judges the switched arms of switched_batch on one overlay build, ARM_JOBS at a time, and adds their verdicts in their order, the overlay's verdict for each when it is not built and UNRUN for an arm that wrote none.
judge_batch() {
  if [ -z "$switched_batch" ]; then
    return 0
  fi
  reset_scratch
  pin=$(pallas_pin)
  pin_short=$(echo "$pin" | cut -c1-7)
  dolos_rev=$(git -C "$ROOT/$SCRATCH" rev-parse --short HEAD)
  for batch_arm in $switched_batch; do
    rm -f "$OUT/$batch_arm.verdict"
  done
  if build_overlay; then
    pool "$ARM_JOBS" judge_switched $switched_batch
  else
    for batch_arm in $switched_batch; do
      echo "$batch_arm=$overlay"
      echo "$overlay" >"$OUT/$batch_arm.verdict"
    done
  fi
  for batch_arm in $switched_batch; do
    verdict=$(cat "$OUT/$batch_arm.verdict" 2>/dev/null || echo UNRUN)
    verdicts="$verdicts $batch_arm=$verdict"
  done
  switched_batch=
}

head_result=SKIPPED
verdicts=""
for arm in $ARMS; do
  if overlay_arm "$arm"; then
    switched_batch="$switched_batch $arm"
    continue
  fi
  judge_batch
  reset_scratch
  pin=$(pallas_pin)
  pin_short=$(echo "$pin" | cut -c1-7)
  dolos_rev=$(git -C "$ROOT/$SCRATCH" rev-parse --short HEAD)
  echo "ARM $arm dolos=$dolos_rev pallas=$pin_short pallas_source=$(pallas_source)"
  started=$(date +%s)
  if crate_test "$arm" >/dev/null; then
    if run_crate_test "$arm" "$OUT/$arm.log"; then
      echo "ARM $arm test=$(crate_test "$arm" | sed 's/.* //')$(crate_test_filter "$arm" | sed 's/^/:/; s/ .*//') seconds=$(($(date +%s) - started))"
      judge_verdict "$arm" "$OUT/$arm.log" ""
    else
      echo "ARM $arm BUILD_REFUSED log=$OUT/$arm.log"
      verdict=UNBUILT
    fi
    echo "$arm=$verdict"
    verdicts="$verdicts $arm=$verdict"
    continue
  fi
  joint=
  if [ "$arm" = control ]; then
    joint=$(joint_packages $CRATE_ARMS)
  fi
  if ! built=$(build_judge "$OUT/$arm.build" $joint --locked); then
    echo "ARM $arm BUILD_REFUSED $built"
    verdicts="$verdicts $arm=UNBUILT"
    continue
  fi
  judge=$(executable "$OUT/$arm.build" dolos)
  deselect= head_counts=
  case $arm in
    head) test=$TEST records= ;;
    control) test=${TEST}_records records=$(for a in $PLANTED_ARMS; do crate_test "$a" >/dev/null || deselect_arm "$a" || cat "$HERE/plants/$a.records"; done | sort -u | paste -sd, -) ;;
    *)
      if deselect_arm "$arm"; then
        test=${TEST}_without records= deselect=$(paste -sd, "$HERE/plants/$arm.deselect")
        head_counts="/w/${OUT#"$ROOT"/}/head.counts"
      else
        test=${TEST}_records records=$(paste -sd, "$HERE/plants/$arm.records")
      fi
      ;;
  esac
  echo "ARM $arm test=${test##*::} records=$(echo "$records" | tr , '\n' | grep -c . || true)${deselect:+ deselect=$deselect} build_seconds=$(($(date +%s) - started))"
  started=$(date +%s)
  ORACLE_DESELECT=$deselect ORACLE_HEAD_COUNTS=$head_counts ORACLE_RECORDS=$records RUST_BACKTRACE=0 \
    sh "$IN_IMAGE" "$SCRATCH" "$judge" "$test" --exact --ignored --nocapture >"$OUT/$arm.log" 2>&1 || true
  echo "ARM $arm judge_seconds=$(($(date +%s) - started))"
  if [ "$arm" = control ]; then
    crate_tests_built=yes
    control_logs="$OUT/head.log $OUT/control.log"
    for a in $CRATE_ARMS; do
      if joint_crate_test "$a"; then
        run_crate_test "$a" "$OUT/control-$a.log" "$(executable "$OUT/$arm.build" "$(crate_test_target "$a")")"
      elif ! run_crate_test "$a" "$OUT/control-$a.log"; then
        echo "ARM control BUILD_REFUSED log=$OUT/control-$a.log"
        crate_tests_built=no
      fi
      control_logs="$control_logs $OUT/control-$a.log"
    done
    if [ "$crate_tests_built" = no ]; then
      control=UNBUILT
    elif ! logs_ran $control_logs; then
      control=$verdict
    elif ! control_clean; then
      control=FIRED
    elif [ -n "$overlay_arms" ]; then
      # The overlay arms' verdicts hold only if the overlay build with no switch set is the head.
      started=$(date +%s)
      overlay_control
      echo "ARM control overlay_seconds=$(($(date +%s) - started))"
    else
      control=CLEAN
    fi
    echo "control=$control"
    verdicts="$verdicts control=$control"
  elif [ "$arm" = head ]; then
    sed -n 's/^CANDIDATE coverage.txt //p' "$OUT/$arm.log" >"$OUT/head.counts"
    grep -E '^(MUTATION|VERDICT|VERDICTS|RESULT) ' "$OUT/$arm.log" | cut -c1-400 || true
    if ! arm_ran "$OUT/$arm.log" '^test result: ' ""; then
      echo "ARM head $verdict log=$OUT/$arm.log"
      head_result=$verdict
    elif grep -q '^RESULT PASS' "$OUT/$arm.log" && ! grep -q '^RESULT FAIL' "$OUT/$arm.log"; then
      head_result=PASS
    else
      head_result=FAIL
    fi
    verdicts="$verdicts head=$head_result"
  else
    judge_verdict "$arm" "$OUT/$arm.log" ""
    echo "$arm=$verdict"
    verdicts="$verdicts $arm=$verdict"
  fi
done
judge_batch
reset_scratch
served_check "$verdicts"
verdicts=" $checked"

echo "ARMS$verdicts logs=$OUT"
passed "$verdicts"
