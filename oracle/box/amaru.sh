# The amaru leg: the amaru-uplc conformance test at the amaru commit the pinned pallas-validate builds against, its arms the patches in amaru/.
# Sourced by run.sh after lib.sh. Reads ROOT, OUT, PALLAS, HERE and IN_IMAGE.

AMARU=amaru
# The test the leg runs, the IntersectMBO plutus conformance programs.
AMARU_TEST="cargo test --locked -p amaru-uplc --test conformance"

# The url and commit of the amaru-uplc entry of the pallas-validate Cargo.toml at the commit $1 of the scratch pallas tree, on one line.
amaru_pin() {
  git -C "$ROOT/$PALLAS" show "$1:pallas-validate/Cargo.toml" |
    sed -n 's/^amaru-uplc-native = {.* git = "\([^"]*\)", rev = "\([0-9a-f]*\)".*/\1 \2/p' | head -1
}

# The number of tests the latest conformance build generated, from the generated source in the target $1, in either cargo build directory layout.
amaru_generated() {
  source=$(ls -t "$ROOT/$1"/debug/build/amaru-uplc-*/out/generated_tests.rs "$ROOT/$1"/debug/build/amaru-uplc/*/out/generated_tests.rs 2>/dev/null | head -1)
  if [ -z "$source" ]; then
    echo 0
    return
  fi
  grep -c '^#\[test\]' "$source"
}

# Whether the test log $1 ran every one of the $2 generated tests and each passed.
amaru_passed() {
  [ "$2" -gt 0 ] && grep -q -x -F "test result: ok. $2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in" "$1.result"
}

# Whether the test log $1 of an arm holds a failed test whose name holds $2.
amaru_caught() {
  grep -E "^test [a-z0-9_]*$2[a-z0-9_]* \.\.\. FAILED$" "$1" | grep -q .
}

# Puts the amaru tree at the url and commit $1 and $2 with no change.
amaru_tree() {
  if [ ! -d "$ROOT/$AMARU/.git" ]; then
    git init -q "$ROOT/$AMARU" || return
  fi
  if ! git -C "$ROOT/$AMARU" cat-file -e "$2^{commit}" 2>/dev/null; then
    git -C "$ROOT/$AMARU" fetch -q --filter=blob:none "$1" "$2" || return
  fi
  git -C "$ROOT/$AMARU" checkout -q -f --detach "$2" && git -C "$ROOT/$AMARU" clean -q -fdx
}

# Runs the test into the log $1 and writes its result line, without the run time, to $1.result.
amaru_run() {
  TARGET=target/amaru sh "$IN_IMAGE" "$AMARU" $AMARU_TEST >"$1" 2>&1 || true
  sed -n 's/^\(test result: .* filtered out; finished in\) [0-9.]*s$/\1/p' "$1" >"$1.result"
}

# Judges the amaru commit the pallas commit $1 pins with its head and each arm of amaru/, printing one AMARU line of verdicts.
amaru_leg() {
  started=$(date +%s)
  mkdir -p "$OUT/amaru"
  pallas=$1
  set -- $(amaru_pin "$pallas" 2>"$OUT/amaru/pin.out")
  verdicts=amaru-head=FAIL
  if [ $# -ne 2 ] || ! amaru_tree "$1" "$2" >"$OUT/amaru/tree.out" 2>&1; then
    echo "AMARU $verdicts pallas=$pallas amaru=\"$*\" logs=$OUT/amaru"
    return 1
  fi
  amaru_run "$OUT/amaru/head.log"
  tests=$(amaru_generated target/amaru)
  cat "$OUT/amaru/head.log.result"
  if amaru_passed "$OUT/amaru/head.log" "$tests"; then
    verdicts=amaru-head=PASS
  fi
  for plant in "$HERE"/amaru/*.diff; do
    arm=$(basename "$plant" .diff)
    if git -C "$ROOT/$AMARU" apply "$plant" >"$OUT/amaru/$arm.plant" 2>&1; then
      amaru_run "$OUT/amaru/$arm.log"
      arm_verdict "$OUT/amaru/$arm.log" '^test result: ' "" amaru_caught "$OUT/amaru/$arm.log" "$(sed -n 's/^# test: //p' "$plant")"
    else
      verdict=UNPLANTED
    fi
    git -C "$ROOT/$AMARU" checkout -q -f --detach "$2"
    verdicts="$verdicts amaru-$arm=$verdict"
  done
  programs=$(find "$ROOT/$AMARU/crates/amaru-uplc/tests/conformance" -name '*.uplc' | wc -l | tr -dc 0-9)
  echo "AMARU $verdicts rev=$2 programs=$programs tests=$tests seconds=$(($(date +%s) - started)) logs=$OUT/amaru"
}
