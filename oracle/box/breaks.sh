#!/bin/sh
# Judges the oracle branch head on both corpora, the control (the head on every arm's records) and each arm on the records plants/<arm>.records names, one oracle run each.
# Usage: ROOT=dir [REV=ref] [ARMS="head control colret coin ..."] breaks.sh
# The branch is read from the bare repository $ROOT/dolos.git.
# An arm whose switch overlay/ adds runs on one build of the overlay with ORACLE_PLANT naming it, and any other arm builds its plants/<arm>.diff.
set -eu
: "${ROOT:?}"
export ROOT
REV=${REV:-feat/ledger-oracle}
MIN_FREE_G=${MIN_FREE_G:-6}
HERE=$(cd "$(dirname "$0")" && pwd)
IMAGE="$HERE/in-image.sh"
SCRATCH=${SCRATCH:-scratch/dolos}
TEST=serve::grpc::v1::ledger_oracle_tests::ledger_oracle
OUT=${OUT:-"$ROOT/runs/$(date -u +%Y%m%dT%H%M%SZ)"}
mkdir -p "$OUT"
. "$HERE/lib.sh"
take_lock
ARMS=${ARMS:-head control $PLANTED_ARMS}
unset ORACLE_PLANT
require_free

for arm in $ARMS; do
  case " head control $PLANTED_ARMS " in
    *" $arm "*) ;;
    *) echo "unknown arm $arm" ; exit 2 ;;
  esac
done
for switch in $(switches "$HERE"/overlay/*.diff); do
  case " $PLANTED_ARMS " in
    *" $switch "*) ;;
    *) echo "REFUSED overlay switch $switch names no planted arm" ; exit 2 ;;
  esac
  if [ -f "$HERE/plants/$switch.diff" ]; then
    echo "REFUSED arm $switch has both an overlay switch and plants/$switch.diff"
    exit 2
  fi
done

# The source cargo resolves pallas-validate from, as the scratch tree's build sees it.
pallas_source() {
  sh "$IMAGE" "$SCRATCH" cargo tree -q -i pallas-validate --depth 0 -e normal 2>/dev/null |
    head -1 | sed -n 's/.*(\(.*\)).*/\1/p'
}

# Whether an arm runs on the overlay build, each switch but colret while COLRET_FIX names the commit whose revert plants it.
switched() {
  if [ "$1" = colret ] && [ -n "${COLRET_FIX:-}" ]; then
    return 1
  fi
  overlay_arm "$1"
}

plant_colret_fix() {
  git -C "$ROOT/$SCRATCH" diff "$COLRET_FIX^" "$COLRET_FIX" -- . ':(exclude)**/*tests*' |
    git -C "$ROOT/$SCRATCH" apply -R
}

# Applies the named Dolos plant over the scratch tree.
plant_dolos() {
  git -C "$ROOT/$SCRATCH" apply "$HERE/plants/$1.diff"
}

# Builds and runs the crate test of an arm over the scratch trees as they stand, its output in $2.
run_crate_test() {
  if ! built=$(build_checked "$2.build" test --release $(crate_test "$1") --no-run $locked); then
    echo "BUILD_REFUSED $built" >"$2"
    return
  fi
  RUST_BACKTRACE=0 sh "$IMAGE" "$SCRATCH" "$(executable "$2.build")" $(crate_test_filter "$1") --nocapture >"$2" 2>&1 || true
}

# Checks pallas out at a commit into the scratch pallas tree and points the scratch tree's pallas crates at it.
pallas_tree() {
  if [ ! -d "$ROOT/$PALLAS/.git" ]; then
    git init -q "$ROOT/$PALLAS" || return
  fi
  for db in "$ROOT"/cargo-home/git/db/pallas-*; do
    if git -C "$db" cat-file -e "$1^{commit}" 2>/dev/null; then
      git -C "$ROOT/$PALLAS" fetch -q "$db" '+refs/*:refs/db/*' || return
      break
    fi
  done
  git -C "$ROOT/$PALLAS" checkout -q -f --detach "$1" &&
    git -C "$ROOT/$PALLAS" clean -q -fdx &&
    sed -E -i 's#^(pallas[a-z-]*) = \{ git = "https://github.com/[^/]+/pallas", rev = "[0-9a-f]+"#\1 = { path = "/w/'"$PALLAS"'/\1"#' \
      "$ROOT/$SCRATCH/Cargo.toml"
}

# Applies the named pallas plant over the pinned pallas commit.
plant_pallas() {
  pallas_tree "$pin" &&
    git -C "$ROOT/$PALLAS" apply "$HERE/plants/$1.diff"
}

overlay=
# Builds the judge and each switched crate test once, from the scratch tree and the pinned pallas with overlay/ applied, and copies them to $OUT/bin, since a later arm's build of the same units writes the same target files.
build_overlay() {
  case $overlay in
    built) return 0 ;;
    failed) return 1 ;;
  esac
  overlay=failed
  mkdir -p "$OUT/bin"
  if ! { pallas_tree "$pin" && git -C "$ROOT/$PALLAS" apply "$HERE/overlay/pallas.diff" &&
    git -C "$ROOT/$SCRATCH" apply "$HERE/overlay/dolos.diff"; } >"$OUT/overlay.plant" 2>&1; then
    echo "OVERLAY PLANT_FAILED log=$OUT/overlay.plant"
    return 1
  fi
  source=$(pallas_source)
  if [ "$source" != "/w/$PALLAS/pallas-validate" ]; then
    echo "OVERLAY REFUSED pallas_source=$source"
    return 1
  fi
  overlay_started=$(date +%s)
  if ! built=$(build_judge "$OUT/overlay.build"); then
    echo "OVERLAY BUILD_REFUSED $built"
    return 1
  fi
  built=$(executable "$OUT/overlay.build")
  cp "$ROOT${built#/w}" "$OUT/bin/overlay-judge"
  for switch in $(switches "$HERE"/overlay/*.diff); do
    if crate_test "$switch" >/dev/null; then
      if ! built=$(build_checked "$OUT/overlay-$switch.build" test --release $(crate_test "$switch") --no-run); then
        echo "OVERLAY BUILD_REFUSED $built"
        return 1
      fi
      built=$(executable "$OUT/overlay-$switch.build")
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
    ORACLE_PLANT=$2 RUST_BACKTRACE=0 sh "$IMAGE" "$SCRATCH" "$bin/overlay-$1" $(crate_test_filter "$1") --nocapture >"$3" 2>&1 || true
  else
    ORACLE_PLANT=$2 ORACLE_RECORDS=$4 RUST_BACKTRACE=0 sh "$IMAGE" "$SCRATCH" "$bin/overlay-judge" "${TEST}_records" \
      --exact --ignored --nocapture >"$3" 2>&1 || true
  fi
}

# Whether an arm's log holds the line its plant must produce and the head log lacks.
caught() {
  arm=$1
  case $arm in
    head) return 1 ;;
    colret) pattern='^(DIFF utxo .* ledger=none dolos=|DISAGREE [a-z]+#[0-9]+ |KNOWN_STALE [a-z]+#[0-9]+ row=)' ;;
    coin) pattern='^DIFF utxo .* fields=([a-z_]+,)*coin(,|$)' ;;
    inflight) pattern='^DISAGREE [a-z]+#[0-9]+ held=' ;;
    rodrigo-reverted) pattern='^DIFF as_output [a-z]+#[0-9]+ ' ;;
    rob-reverted) pattern='^(UNSUPPORTED_UNEXPECTED|DISAGREE|REASON_MISMATCH|STALE_MANIFEST) [a-z]+#[0-9]+ ' ;;
    cost) pattern='^EVAL-DIFF pallas supp#[0-9]+ top [A-Za-z]+#[0-9]+ ledger=\(ran ' ;;
    eval-purpose) pattern='^EVAL-DIFF pallas [a-z]+#[0-9]+ top Vote#[0-9]+ ledger=\(none\) dolos=\((ran|failed)' ;;
    eval-index) pattern='^EVAL-DIFF pallas [a-z]+#[0-9]+ top Spend#[1-9][0-9]* ledger=\(none\) dolos=\((ran|failed)' ;;
    eval-mem) pattern='^EVAL-DIFF pallas [a-z]+#[0-9]+ top [A-Za-z]+#[0-9]+ ledger=\(ran mem=[0-9]+ steps=([0-9]+)\) dolos=\(ran mem=[0-9]+ steps=\1\)$' ;;
    eval-steps) pattern='^EVAL-DIFF pallas [a-z]+#[0-9]+ top [A-Za-z]+#[0-9]+ ledger=\(ran mem=([0-9]+) steps=[0-9]+\) dolos=\(ran mem=\1 steps=[0-9]+\)$' ;;
    eval-ran) pattern='^EVAL-DIFF pallas [a-z]+#[0-9]+ top [A-Za-z]+#[0-9]+ ledger=\(ran mem=[0-9]+ steps=[0-9]+\) dolos=\(failed\)$' ;;
    v4-v3-context) pattern='^EVAL-DIFF pallas [a-z]+#[0-9]+ top [A-Za-z]+#[0-9]+ ledger=\(ran mem=[0-9]+ steps=[0-9]+\) dolos=\((ran mem=[0-9]+ steps=[0-9]+|failed)\)$' ;;
    guarding-purpose) pattern='^EVAL-DIFF pallas [a-z]+#[0-9]+ top Guarding#[0-9]+ ledger=\(ran mem=[0-9]+ steps=[0-9]+\) dolos=\((ran mem=[0-9]+ steps=[0-9]+|failed)\)$' ;;
    eval-level) pattern='^EVAL-DIFF pallas [a-z]+#[0-9]+ sub[0-9]+ [A-Za-z]+#[0-9]+ ledger=\((ran|failed)[^)]*\) dolos=\(none\)$' ;;
    v1-guarding) pattern="^thread 'serve::grpc::v1::submit::mapping_tests::guarding_result_keeps_identity_and_budget' \([0-9]+\) panicked at src/serve/grpc/v1/submit.rs:286:" ;;
    revalidate) pattern='^DIFF revalidate [a-z]+#[0-9]+ kept_after_rollback_to=' ;;
    revalidate-drops) pattern='^DIFF revalidate [a-z]+#[0-9]+ dropped_after_rollback_to=' ;;
    subouts) pattern='^DIFF (undo|as_output) [a-z]+#[0-9]+ ' ;;
    byron-attrs | byron-signer | byron-guard | chaincode | emptyguards | tag24 | tag24-ref | unreg-refund | drep-deposit | upper-inclusive | slot-off-by-one | horizon-removed | system-start) pattern='^DISAGREE [a-z]+#[0-9]+ ' ;;
    cost-models) pattern="^thread 'musashi_bootstrap_uses_the_published_cost_models' \([0-9]+\) panicked at crates/cardano/tests/musashi_cost_models.rs:32:" ;;
    byron-out) pattern='^UNSUPPORTED_UNEXPECTED [a-z]+#[0-9]+ label=(bootstrap or reward address|output address)' ;;
    minfee-subtx) pattern='^DIFF fee [a-z]+#[0-9]+ ledger=[0-9]+ dolos=[0-9]+$' ;;
    v1v2) pattern='^UNSUPPORTED_UNEXPECTED [a-z]+#[0-9]+ label=new script language ' ;;
    native-ref-reencoded | v2-ref-dropped | v1-ref-exposed | script-payment-swap | signatories-unsorted | withdrawal-order | cert-unwrapped | script-guard-admitted) pattern='^DISAGREE [a-z]+#[0-9]+ ' ;;
    v3-cert-wrapped | v2-withdrawal-unwrapped | top-guards-admitted) pattern='^DISAGREE [a-z]+#[0-9]+ ' ;;
    guard-ref-reverted) pattern='^UNSUPPORTED_UNEXPECTED [a-z]+#[0-9]+ label=native script guards$' ;;
    v4-ref-refused) pattern='^UNSUPPORTED_UNEXPECTED [a-z]+#[0-9]+ label=reference script language ' ;;
    pointer-refused) pattern='^UNSUPPORTED_UNEXPECTED [a-z]+#[0-9]+ label=pointer address ' ;;
    collateral-unsupported) pattern='^UNSUPPORTED_UNEXPECTED [a-z]+#[0-9]+ label=collateral without Plutus$' ;;
    *)
      echo "ARM $arm NO_PATTERN"
      return 1
      ;;
  esac
  if [ ! -f "$OUT/head.log" ]; then
    echo "ARM $arm NO_HEAD_LOG"
    return 1
  fi
  log=$2
  at=$3
  grep -E "$pattern" "$log" >"$at.expected" || true
  # Keeps the lines the head run lacks that name one of the arm's records.
  sed 's/^/ /; s/$/ /' "$HERE/plants/$arm.records" >"$at.ids"
  grep -E "$pattern" "$OUT/head.log" | grep -v -x -F -f - "$at.expected" |
    sed 's/$/ /' | grep -F -f "$at.ids" | sed 's/ $//' >"$at.new" || true
  # The subouts arm counts only records that no known row names.
  if [ "$arm" = subouts ]; then
    sed -n 's/^[^ ]* [^ ]* \([a-z]*#[0-9]*\).*/ \1 /p' "$ROOT/$SCRATCH/test_data/ledger-oracle/known.txt" >"$OUT/known.ids"
    grep -v -F -f "$OUT/known.ids" "$at.new" >"$at.unknown" || true
    mv "$at.unknown" "$at.new"
  fi
  head -4 "$at.new"
  [ -s "$at.new" ]
}

# Whether the unplanted head judges every planted arm's records with a pass and no arm's line, the case where each arm must not fire.
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
      log=$OUT/control-$a.log
      if ! grep -q '^test result: ok\. 1 passed' "$log"; then
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

# Whether the overlay build with no switch set prints the head's control logs, on every planted arm's records and in each switched crate test.
overlay_control_clean() {
  if ! build_overlay; then
    echo "ARM control OVERLAY_UNBUILT"
    return 1
  fi
  clean=0
  run_overlay control "" "$OUT/overlay-control.log" "$records"
  if ! same_log "$OUT/control.log" "$OUT/overlay-control.log"; then
    echo "ARM control OVERLAY_DIFFERS log=$OUT/overlay-control.log"
    clean=1
  fi
  for a in $(switches "$HERE"/overlay/*.diff); do
    if crate_test "$a" >/dev/null; then
      run_overlay "$a" "" "$OUT/overlay-control-$a.log"
      if ! same_log "$OUT/control-$a.log" "$OUT/overlay-control-$a.log"; then
        echo "ARM control OVERLAY_DIFFERS log=$OUT/overlay-control-$a.log"
        clean=1
      fi
    fi
  done
  return $clean
}

overlay_arms=
for arm in $ARMS; do
  if switched "$arm"; then
    overlay_arms="$overlay_arms $arm"
  fi
done

head_result=SKIPPED
verdicts=""
for arm in $ARMS; do
  reset_scratch
  pin=$(sed -n 's/^pallas-validate = { git = "[^"]*", rev = "\([0-9a-f]*\)".*/\1/p' "$ROOT/$SCRATCH/Cargo.toml" | head -1)
  pin_short=$(echo "$pin" | cut -c1-7)
  dolos_rev=$(git -C "$ROOT/$SCRATCH" rev-parse --short HEAD)
  if switched "$arm"; then
    if ! build_overlay; then
      echo "$arm=MISSED"
      verdicts="$verdicts $arm=MISSED"
      continue
    fi
    echo "ARM $arm dolos=$dolos_rev pallas=$pin_short-overlay switch=$arm"
    started=$(date +%s)
    run_overlay "$arm" "$arm" "$OUT/$arm.log" "$(paste -sd, "$HERE/plants/$arm.records")"
    echo "ARM $arm records=$(grep -c . "$HERE/plants/$arm.records" || true) judge_seconds=$(($(date +%s) - started))"
    if ! grep -q -x -F "ORACLE_PLANT reached $arm" "$OUT/$arm.log"; then
      echo "ARM $arm UNREACHED log=$OUT/$arm.log"
      echo "$arm=MISSED"
      verdicts="$verdicts $arm=MISSED"
    elif caught "$arm" "$OUT/$arm.log" "$OUT/$arm"; then
      echo "$arm=CAUGHT"
      verdicts="$verdicts $arm=CAUGHT"
    else
      echo "$arm=MISSED"
      verdicts="$verdicts $arm=MISSED"
    fi
    continue
  fi
  locked=--locked
  if pallas_arm "$arm"; then
    locked=
  fi
  case $arm in
    head | control) plant=true ;;
    colret) plant=plant_colret_fix ;;
    *)
      if pallas_arm "$arm"; then
        plant="plant_pallas $arm"
      else
        plant="plant_dolos $arm"
      fi
      ;;
  esac
  if ! $plant >"$OUT/$arm.plant" 2>&1; then
    echo "ARM $arm PLANT_FAILED log=$OUT/$arm.plant"
    verdicts="$verdicts $arm=MISSED"
    continue
  fi
  source=$(pallas_source)
  pallas_rev=$pin_short
  if pallas_arm "$arm"; then
    pallas_rev="$pin_short-$arm"
  fi
  if [ -z "$locked" ] && [ "$source" != "/w/$PALLAS/pallas-validate" ]; then
    echo "ARM $arm REFUSED pallas_source=$source"
    verdicts="$verdicts $arm=MISSED"
    continue
  fi
  echo "ARM $arm dolos=$dolos_rev pallas=$pallas_rev pallas_source=$source"
  started=$(date +%s)
  if crate_test "$arm" >/dev/null; then
    run_crate_test "$arm" "$OUT/$arm.log"
    echo "ARM $arm test=$(crate_test "$arm" | sed 's/.* //')$(crate_test_filter "$arm" | sed 's/^/:/; s/ .*//') seconds=$(($(date +%s) - started))"
    if caught "$arm" "$OUT/$arm.log" "$OUT/$arm"; then
      echo "$arm=CAUGHT"
      verdicts="$verdicts $arm=CAUGHT"
    else
      echo "$arm=MISSED"
      verdicts="$verdicts $arm=MISSED"
    fi
    continue
  fi
  if ! built=$(build_judge "$OUT/$arm.build" $locked); then
    echo "ARM $arm BUILD_REFUSED $built"
    verdicts="$verdicts $arm=MISSED"
    continue
  fi
  judge=$(executable "$OUT/$arm.build")
  case $arm in
    head) test=$TEST records= ;;
    control) test=${TEST}_records records=$(for a in $PLANTED_ARMS; do crate_test "$a" >/dev/null || cat "$HERE/plants/$a.records"; done | sort -u | paste -sd, -) ;;
    *) test=${TEST}_records records=$(paste -sd, "$HERE/plants/$arm.records") ;;
  esac
  echo "ARM $arm test=${test##*::} records=$(echo "$records" | tr , '\n' | grep -c . || true) build_seconds=$(($(date +%s) - started))"
  started=$(date +%s)
  ORACLE_RECORDS=$records RUST_BACKTRACE=0 sh "$IMAGE" "$SCRATCH" "$judge" "$test" \
    --exact --ignored --nocapture >"$OUT/$arm.log" 2>&1 || true
  echo "ARM $arm judge_seconds=$(($(date +%s) - started))"
  if [ "$arm" = control ]; then
    for a in $PLANTED_ARMS; do
      if crate_test "$a" >/dev/null; then
        run_crate_test "$a" "$OUT/control-$a.log"
      fi
    done
    control=CLEAN
    control_clean || control=FIRED
    # The overlay arms' verdicts hold only if the overlay build with no switch set is the head.
    if [ -n "$overlay_arms" ]; then
      started=$(date +%s)
      overlay_control_clean || control=FIRED
      echo "ARM control overlay_seconds=$(($(date +%s) - started))"
    fi
    echo "control=$control"
    verdicts="$verdicts control=$control"
  elif [ "$arm" = head ]; then
    grep -E '^(CORPUS|VALIDATION|DEPENDENT|KNOWN|STATE|COST|EVAL-ORACLE|EVAL-DIFF|EVAL-TOTAL|EVAL-UNNAMED|EVAL-FEE|REVALIDATE|BUILTIN|BUILTIN_UNCOVERED|ROLL_FAIL|RESULT|DISAGREE|REASON_MISMATCH|CASCADE|UNSUPPORTED_UNEXPECTED|STALE_MANIFEST|KNOWN_STALE|UNKNOWN_REFUSAL) ' "$OUT/$arm.log" |
      cut -c1-400 || true
    if grep -q '^RESULT PASS' "$OUT/$arm.log" && ! grep -q '^RESULT FAIL' "$OUT/$arm.log"; then
      head_result=PASS
    else
      head_result=FAIL
    fi
    verdicts="$verdicts head=$head_result"
  elif caught "$arm" "$OUT/$arm.log" "$OUT/$arm"; then
    echo "$arm=CAUGHT"
    verdicts="$verdicts $arm=CAUGHT"
  else
    echo "$arm=MISSED"
    verdicts="$verdicts $arm=MISSED"
  fi
done
reset_scratch

echo "ARMS$verdicts logs=$OUT"
case "$verdicts" in
  *MISSED* | *FAIL* | *FIRED* | *SKIPPED*) exit 1 ;;
esac
[ "$head_result" = PASS ]
