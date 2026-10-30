#!/bin/sh
# Judges the oracle branch head and each named arm, one oracle run each.
# Usage: [REV=ref] [ARMS="head colret coin ..."] breaks.sh
# The branch is read from the bare repository $ROOT/dolos.git.
set -eu
ROOT=${ROOT:-/opt/build/ledger-oracle-1027}
REV=${REV:-feat/ledger-oracle}
ARMS=${ARMS:-head colret coin inflight rodrigo-reverted rob-reverted cost revalidate revalidate-drops subouts byron-attrs byron-signer byron-guard chaincode minfee-subtx emptyguards tag24 v1v2}
MIN_FREE_G=${MIN_FREE_G:-6}
HERE=$(cd "$(dirname "$0")" && pwd)
IMAGE="$HERE/in-image.sh"
SCRATCH=scratch/dolos
PALLAS=scratch/pallas
# The pallas commit the rob arm reverts f3419a3a on, one where no later change touches the lines of that merge.
ROB_BASE=${ROB_BASE:-362f5e0e2610686382752b1392fde02c84eb3b50}
TEST=serve::grpc::v1::ledger_oracle_tests::ledger_oracle
OUT=${OUT:-"$ROOT/runs/$(date -u +%Y%m%dT%H%M%SZ)"}
mkdir -p "$OUT"
. "$HERE/lib.sh"
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

# Checks pallas out at a commit into the scratch pallas tree and points the scratch tree's pallas crates at it.
pallas_tree() {
  rm -rf "${ROOT:?}/$PALLAS"
  for db in "$ROOT"/cargo-home/git/db/pallas-*; do
    if git -C "$db" cat-file -e "$1^{commit}" 2>/dev/null; then
      git clone -q --no-checkout "$db" "$ROOT/$PALLAS"
      break
    fi
  done
  git -C "$ROOT/$PALLAS" checkout -q --detach "$1"
  sed -E -i 's#^(pallas[a-z-]*) = \{ git = "https://github.com/[^/]+/pallas", rev = "[0-9a-f]+"#\1 = { path = "/w/'"$PALLAS"'/\1"#' \
    "$ROOT/$SCRATCH/Cargo.toml"
}

plant_rob() {
  pallas_tree "$ROB_BASE"
  git -C "$ROOT/$PALLAS" revert -m 1 --no-commit f3419a3a
  # Adds the pallas items Dolos names that this commit lacks, with every refusal message unchanged.
  git -C "$ROOT/$PALLAS" apply "$HERE/plants/rob-surface.diff"
}

plant_cost() {
  pallas_tree "$pin"
  git -C "$ROOT/$PALLAS" apply "$HERE/plants/cost.diff"
}

# Applies the named pallas plant over the pinned pallas commit.
plant_pallas() {
  pallas_tree "$pin"
  git -C "$ROOT/$PALLAS" apply "$HERE/plants/$1.diff"
}

# Whether an arm's log holds the line its plant must produce and the head log lacks.
caught() {
  arm=$1
  case $arm in
    head) return 1 ;;
    colret) pattern='^(DIFF utxo .* ledger=none dolos=|DISAGREE supp#9 |KNOWN_STALE supp#9 row=S3 )' ;;
    coin) pattern='^DIFF utxo .* fields=([a-z_]+,)*coin(,|$)' ;;
    inflight) pattern='^DISAGREE [a-z]+#[0-9]+ held=' ;;
    rodrigo-reverted) pattern='^STATE as_output disagree=' ;;
    rob-reverted) pattern='^(UNSUPPORTED_UNEXPECTED|DISAGREE|REASON_MISMATCH|STALE_MANIFEST) ic#(851|861|1215|1233) ' ;;
    cost) pattern='^DIFF units supp#[0-9]+ purpose=[0-9]+#[0-9]+ ledger=\(ran ' ;;
    revalidate) pattern='^DIFF revalidate [a-z]+#[0-9]+ kept_after_rollback_to=' ;;
    revalidate-drops) pattern='^DIFF revalidate [a-z]+#[0-9]+ dropped_after_rollback_to=' ;;
    subouts) pattern='^DIFF (undo|as_output) [a-z]+#[0-9]+ ' ;;
    byron-attrs) pattern='^DISAGREE supp#(59|72) ' ;;
    byron-signer) pattern='^DISAGREE supp#(55|68) ' ;;
    byron-guard) pattern='^DISAGREE supp#(63|76) ' ;;
    chaincode) pattern='^DISAGREE supp#(61|74) ' ;;
    minfee-subtx) pattern='^DIFF fee [a-z]+#[0-9]+ ledger=[0-9]+ dolos=[0-9]+$' ;;
    emptyguards) pattern='^DISAGREE supp#(80|82) ' ;;
    tag24) pattern='^DISAGREE supp#83 ' ;;
    v1v2) pattern='^UNSUPPORTED_UNEXPECTED ic#[0-9]+ label=new script language ' ;;
  esac
  if [ ! -f "$OUT/head.log" ]; then
    echo "ARM $arm NO_HEAD_LOG"
    return 1
  fi
  grep -E "$pattern" "$OUT/$arm.log" >"$OUT/$arm.expected" || true
  grep -E "$pattern" "$OUT/head.log" | grep -v -x -F -f - "$OUT/$arm.expected" >"$OUT/$arm.new" || true
  # The subouts arm counts only records that no known row names.
  if [ "$arm" = subouts ]; then
    sed -n 's/^[^ ]* [^ ]* \([a-z]*#[0-9]*\).*/ \1 /p' "$ROOT/$SCRATCH/test_data/ledger-oracle/known.txt" >"$OUT/known.ids"
    grep -v -F -f "$OUT/known.ids" "$OUT/$arm.new" >"$OUT/$arm.unknown" || true
    mv "$OUT/$arm.unknown" "$OUT/$arm.new"
  fi
  head -4 "$OUT/$arm.new"
  [ -s "$OUT/$arm.new" ]
}

head_result=SKIPPED
verdicts=""
for arm in $ARMS; do
  reset_scratch
  pin=$(sed -n 's/^pallas-validate = { git = "[^"]*", rev = "\([0-9a-f]*\)".*/\1/p' "$ROOT/$SCRATCH/Cargo.toml" | head -1)
  pin_short=$(echo "$pin" | cut -c1-7)
  locked=--locked
  case $arm in
    head) ;;
    colret) plant_colret ;;
    coin) plant_coin ;;
    inflight) plant_inflight ;;
    revalidate) plant_revalidate ;;
    revalidate-drops) plant_revalidate_drops ;;
    subouts) plant_subouts ;;
    rodrigo-reverted) plant_rodrigo ;;
    rob-reverted) plant_rob ; locked= ;;
    cost) plant_cost ; locked= ;;
    byron-attrs | byron-signer | byron-guard | chaincode | minfee-subtx | emptyguards | tag24 | v1v2) plant_pallas "$arm" ; locked= ;;
    *) echo "unknown arm $arm" ; exit 2 ;;
  esac
  source=$(pallas_source)
  case $arm in
    rob-reverted) pallas_rev="$(echo "$ROB_BASE" | cut -c1-7)-reverted-f3419a3a" ;;
    cost | byron-attrs | byron-signer | byron-guard | chaincode | minfee-subtx | emptyguards | tag24 | v1v2) pallas_rev="$pin_short-$arm" ;;
    *) pallas_rev=$pin_short ;;
  esac
  if [ -z "$locked" ] && [ "$source" != "/w/$PALLAS/pallas-validate" ]; then
    echo "ARM $arm REFUSED pallas_source=$source"
    verdicts="$verdicts $arm=MISSED"
    continue
  fi
  dolos_rev=$(git -C "$ROOT/$SCRATCH" rev-parse --short HEAD)
  echo "ARM $arm dolos=$dolos_rev pallas=$pallas_rev pallas_source=$source"
  if ! sh "$IMAGE" "$SCRATCH" cargo check -q --release $locked -p dolos --lib --tests >"$OUT/$arm.check" 2>&1; then
    echo "ARM $arm CHECK_FAILED log=$OUT/$arm.check"
    verdicts="$verdicts $arm=MISSED"
    continue
  fi
  RUST_BACKTRACE=0 sh "$IMAGE" "$SCRATCH" cargo test -q --release $locked -p dolos --lib "$TEST" \
    -- --exact --ignored --nocapture >"$OUT/$arm.log" 2>&1 || true
  if [ "$arm" = head ]; then
    grep -E '^(CORPUS|VALIDATION|DEPENDENT|KNOWN|STATE|COST|REVALIDATE|BUILTIN|BUILTIN_UNCOVERED|ROLL_FAIL|RESULT|DISAGREE|REASON_MISMATCH|CASCADE|UNSUPPORTED_UNEXPECTED|STALE_MANIFEST|KNOWN_STALE|UNKNOWN_REFUSAL) ' "$OUT/$arm.log" |
      cut -c1-400 || true
    if grep -q '^RESULT PASS' "$OUT/$arm.log" && ! grep -q '^RESULT FAIL' "$OUT/$arm.log"; then
      head_result=PASS
    else
      head_result=FAIL
    fi
    verdicts="$verdicts head=$head_result"
  elif caught "$arm"; then
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
  *MISSED* | *FAIL* | *SKIPPED*) exit 1 ;;
esac
[ "$head_result" = PASS ]
