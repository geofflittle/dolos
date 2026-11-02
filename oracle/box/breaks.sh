#!/bin/sh
# Judges the oracle branch head on both corpora, the control (the head on every arm's records) and each arm on the records plants/<arm>.records names, one oracle run each.
# Usage: [REV=ref] [ARMS="head control colret coin ..."] breaks.sh
# The branch is read from the bare repository $ROOT/dolos.git.
set -eu
ROOT=${ROOT:-/opt/build/ledger-oracle-1027}
export ROOT
REV=${REV:-feat/ledger-oracle}
MIN_FREE_G=${MIN_FREE_G:-6}
HERE=$(cd "$(dirname "$0")" && pwd)
IMAGE="$HERE/in-image.sh"
SCRATCH=scratch/dolos
TEST=serve::grpc::v1::ledger_oracle_tests::ledger_oracle
OUT=${OUT:-"$ROOT/runs/$(date -u +%Y%m%dT%H%M%SZ)"}
mkdir -p "$OUT"
. "$HERE/lib.sh"
ARMS=${ARMS:-head control $PLANTED_ARMS}
require_free

# The source cargo resolves pallas-validate from, as the scratch tree's build sees it.
pallas_source() {
  sh "$IMAGE" "$SCRATCH" cargo tree -q -i pallas-validate --depth 0 -e normal 2>/dev/null |
    head -1 | sed -n 's/.*(\(.*\)).*/\1/p'
}

plant_rodrigo() {
  git -C "$ROOT/$SCRATCH" apply "$HERE/plants/rodrigo.diff"
}

plant_colret() {
  if [ -n "${COLRET_FIX:-}" ]; then
    git -C "$ROOT/$SCRATCH" diff "$COLRET_FIX^" "$COLRET_FIX" -- . ':(exclude)**/*tests*' |
      git -C "$ROOT/$SCRATCH" apply -R
  else
    git -C "$ROOT/$SCRATCH" apply "$HERE/plants/colret.diff"
  fi
}

plant_coin() {
  git -C "$ROOT/$SCRATCH" apply "$HERE/plants/coin.diff"
}

plant_inflight() {
  git -C "$ROOT/$SCRATCH" apply "$HERE/plants/inflight.diff"
}

plant_revalidate() {
  git -C "$ROOT/$SCRATCH" apply "$HERE/plants/revalidate.diff"
}

plant_revalidate_drops() {
  git -C "$ROOT/$SCRATCH" apply "$HERE/plants/revalidate-drops.diff"
}

plant_subouts() {
  git -C "$ROOT/$SCRATCH" apply "$HERE/plants/subouts.diff"
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
  RUST_BACKTRACE=0 sh "$IMAGE" "$SCRATCH" "$(executable "$2.build")" --nocapture >"$2" 2>&1 || true
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

plant_drep_deposit() {
  git -C "$ROOT/$SCRATCH" apply "$HERE/plants/drep-deposit.diff"
}

plant_cost() {
  pallas_tree "$pin" &&
    git -C "$ROOT/$PALLAS" apply "$HERE/plants/cost.diff"
}

# Applies the named pallas plant over the pinned pallas commit.
plant_pallas() {
  pallas_tree "$pin" &&
    git -C "$ROOT/$PALLAS" apply "$HERE/plants/$1.diff"
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
    cost) pattern='^DIFF units supp#[0-9]+ purpose=[0-9]+#[0-9]+ ledger=\(ran ' ;;
    revalidate) pattern='^DIFF revalidate [a-z]+#[0-9]+ kept_after_rollback_to=' ;;
    revalidate-drops) pattern='^DIFF revalidate [a-z]+#[0-9]+ dropped_after_rollback_to=' ;;
    subouts) pattern='^DIFF (undo|as_output) [a-z]+#[0-9]+ ' ;;
    byron-attrs | byron-signer | byron-guard | chaincode | emptyguards | tag24 | tag24-ref | unreg-refund | drep-deposit | upper-inclusive | slot-off-by-one | horizon-removed | system-start) pattern='^DISAGREE [a-z]+#[0-9]+ ' ;;
    cost-models) pattern="^thread 'musashi_bootstrap_uses_the_published_cost_models' \([0-9]+\) panicked at crates/cardano/tests/musashi_cost_models.rs:32:" ;;
    byron-out) pattern='^UNSUPPORTED_UNEXPECTED [a-z]+#[0-9]+ label=(bootstrap or reward address|output address)' ;;
    minfee-subtx) pattern='^DIFF fee [a-z]+#[0-9]+ ledger=[0-9]+ dolos=[0-9]+$' ;;
    v1v2) pattern='^UNSUPPORTED_UNEXPECTED [a-z]+#[0-9]+ label=new script language ' ;;
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

head_result=SKIPPED
verdicts=""
for arm in $ARMS; do
  reset_scratch
  pin=$(sed -n 's/^pallas-validate = { git = "[^"]*", rev = "\([0-9a-f]*\)".*/\1/p' "$ROOT/$SCRATCH/Cargo.toml" | head -1)
  pin_short=$(echo "$pin" | cut -c1-7)
  locked=--locked
  if pallas_arm "$arm"; then
    locked=
  fi
  case $arm in
    head | control) plant=true ;;
    colret) plant=plant_colret ;;
    coin) plant=plant_coin ;;
    inflight) plant=plant_inflight ;;
    revalidate) plant=plant_revalidate ;;
    revalidate-drops) plant=plant_revalidate_drops ;;
    subouts) plant=plant_subouts ;;
    rodrigo-reverted) plant=plant_rodrigo ;;
    drep-deposit) plant=plant_drep_deposit ;;
    cost) plant=plant_cost ;;
    rob-reverted | byron-attrs | byron-signer | byron-guard | chaincode | minfee-subtx | emptyguards | tag24 | v1v2 | byron-out | tag24-ref | unreg-refund | upper-inclusive | slot-off-by-one | horizon-removed | cost-models) plant="plant_pallas $arm" ;;
    system-start) plant="plant_dolos $arm" ;;
    *) echo "unknown arm $arm" ; exit 2 ;;
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
  dolos_rev=$(git -C "$ROOT/$SCRATCH" rev-parse --short HEAD)
  echo "ARM $arm dolos=$dolos_rev pallas=$pallas_rev pallas_source=$source"
  started=$(date +%s)
  if crate_test "$arm" >/dev/null; then
    run_crate_test "$arm" "$OUT/$arm.log"
    echo "ARM $arm test=$(crate_test "$arm" | sed 's/.* //') seconds=$(($(date +%s) - started))"
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
    if control_clean; then
      echo "control=CLEAN"
      verdicts="$verdicts control=CLEAN"
    else
      echo "control=FIRED"
      verdicts="$verdicts control=FIRED"
    fi
  elif [ "$arm" = head ]; then
    grep -E '^(CORPUS|VALIDATION|DEPENDENT|KNOWN|STATE|COST|REVALIDATE|BUILTIN|BUILTIN_UNCOVERED|ROLL_FAIL|RESULT|DISAGREE|REASON_MISMATCH|CASCADE|UNSUPPORTED_UNEXPECTED|STALE_MANIFEST|KNOWN_STALE|UNKNOWN_REFUSAL) ' "$OUT/$arm.log" |
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
