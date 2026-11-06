# The value leg: the pallas conformance harness at the commit value/harness.rev names, judging the pallas commit the scratch tree pins with the Haskell ledger, its arms the switches value/pallas.diff adds over overlay/pallas.diff.
# Sourced by run.sh after lib.sh. Reads ROOT, OUT, SCRATCH, PALLAS, HERE, IN_IMAGE, LAST_VALUE, VALUE_N, VALUE_SEED, VALUE_GEN_DIR and VALUE_HARNESS_URL.

VALUE_N=${VALUE_N:-100}
VALUE_SEED=${VALUE_SEED:-1}
# The generator binaries, each named for the sha256 of the oracle/Main.hs it was built from.
VALUE_GEN_DIR=${VALUE_GEN_DIR:-$ROOT/value-gen}
VALUE_HARNESS_URL=${VALUE_HARNESS_URL:-https://github.com/geofflittle/pallas.git}
LAST_VALUE=${LAST_VALUE:-$ROOT/runs/last-value}
# The Dolos files whose change puts the value leg in the quick tier, beside the pallas pin files.
VALUE_FILES="oracle/box/value.sh oracle/box/value oracle/box/overlay/pallas.diff oracle/box/amaru.sh oracle/box/amaru"
# The arms of the value leg, each a switch of value/pallas.diff or of overlay/pallas.diff.
VALUE_ARMS="emptyguards value-guards-top value-guards-sub value-spend-redeemer value-withdrawal-order"

# The kind and source pairs an arm's plant changes a value of.
value_pairs() {
  case $1 in
    emptyguards) echo "tx:given tx:empty14 body:empty14" ;;
    value-guards-top) echo "tx:cddl-plain tx:cddl body:cddl-plain body:cddl" ;;
    value-guards-sub) echo "tx:subkeyhash" ;;
    value-spend-redeemer | value-withdrawal-order) echo "tx:given" ;;
    *) return 1 ;;
  esac
}

# The disagreement class an arm's plant must produce on its pairs.
value_class() {
  case $1 in
    emptyguards | value-guards-top | value-guards-sub) echo "pallas refuses ledger-valid input" ;;
    value-spend-redeemer | value-withdrawal-order) echo "v1 redeemer attachment differs" ;;
    *) return 1 ;;
  esac
}

# The name of an arm's verdict, the arm with the value prefix.
value_key() {
  case $1 in
    value-*) echo "$1" ;;
    *) echo "value-$1" ;;
  esac
}

# The inputs of the value and amaru legs on the pallas pin $1: the pin and the hash of their box files.
value_inputs() {
  echo "pin=$1 box=$(cat "$HERE/value.sh" "$HERE/value/harness.rev" "$HERE/value/pallas.diff" "$HERE/overlay/pallas.diff" "$HERE/amaru.sh" "$HERE"/amaru/*.diff | sha256sum | cut -c1-12)"
}

# Whether the value and amaru legs on the pallas pin $1 are due: their inputs differ from those of the last run in which both passed, or none did.
value_due() {
  [ ! -f "$LAST_VALUE" ] || [ "$(cat "$LAST_VALUE")" != "$(value_inputs "$1")" ]
}

# Whether the dir $1 of an arm's run holds a disagreement of the class $2 and a probe line naming the plant $3.
value_caught() {
  cat "$1"/*.reached 2>/dev/null | grep -q -x -F "ORACLE_PLANT reached $3" &&
    cat "$1"/*.dis.jsonl 2>/dev/null | grep -q -F "\"cls\": \"$2\""
}

# Puts the harness of value/harness.rev in $ROOT/value/gen, the pinned pallas tree in $ROOT/value/conf and the generator for its oracle/Main.hs in its bin.
value_harness() {
  rev=$(cat "$HERE/value/harness.rev")
  repo=$ROOT/value/harness.git
  if [ ! -d "$repo" ]; then
    git init -q --bare "$repo" || return
  fi
  if ! git -C "$repo" cat-file -e "$rev^{commit}" 2>/dev/null; then
    git -C "$repo" fetch -q --filter=blob:none "$VALUE_HARNESS_URL" "$rev" || return
  fi
  rm -rf "$ROOT/value/gen" "$ROOT/value/conf"
  mkdir -p "$ROOT/value/gen"
  git -C "$repo" archive "$rev" conformance-value-oracle/gen | tar -x -C "$ROOT/value/gen" --strip-components 2 || return
  ln -s "../$PALLAS" "$ROOT/value/conf"
  main=$(sha256sum "$ROOT/value/gen/oracle/Main.hs" | cut -c1-12)
  gen=$VALUE_GEN_DIR/conformance-gen-$main
  if [ ! -x "$gen" ]; then
    echo "VALUE REFUSED generator=missing main_hs=$main dir=$VALUE_GEN_DIR" >&2
    return 1
  fi
  mkdir -p "$ROOT/value/gen/bin"
  ln -f "$gen" "$ROOT/value/gen/bin/conformance-gen" 2>/dev/null || cp "$gen" "$ROOT/value/gen/bin/conformance-gen"
  echo "$rev"
}

# Builds the probe over the scratch pallas tree with its current changes into $OUT/value/bin/$1, with the pallas tree's toolchain and the target $2.
value_probe() {
  cp "$ROOT/$PALLAS/rust-toolchain.toml" "$ROOT/value/gen/probe-value/"
  if ! TARGET=$2 sh "$IN_IMAGE" value/gen/probe-value cargo build --release >"$OUT/value/$1.build" 2>&1; then
    echo "VALUE BUILD_FAILED probe=$1 log=$OUT/value/$1.build"
    return 1
  fi
  cp "$ROOT/$2/release/pallas-probe-value" "$OUT/value/bin/$1"
}

# Runs the harness tier over the pairs $3, or every pair, with the probe $1 into the dir $2, reusing the values of the dir $4 when given.
value_tier() {
  probe=$1
  dir=$2
  pairs=$3
  from=${4:-}
  data=$ROOT/$SCRATCH/test_data/ledger-oracle
  fixtures=$ROOT/$PALLAS/test_data
  (
    cd "$ROOT/value/gen" &&
      PROBE=$OUT/value/bin/$probe GEN_FROM=$from ${pairs:+PAIRS="$pairs"} \
        TX_FILES="$data/ic.jsonl.gz $data/supp.jsonl.gz $(ls "$fixtures"/dijkstra-*.tx | tr '\n' ' ')" \
        BLOCK_FILES="$(ls "$fixtures"/dijkstra*.block | tr '\n' ' ')" \
        EB_FILES="$(ls "$fixtures"/*.ebbody | tr '\n' ' ')" \
        sh tier.sh "$dir" "$VALUE_N" "$VALUE_SEED"
  )
}

# Judges the pinned pallas with the head probe, the overlay probe with no switch set against the head's output and each arm, printing one VALUE line of verdicts.
value_leg() {
  started=$(date +%s)
  mkdir -p "$OUT/value/bin"
  pin=$(pallas_pin)
  verdicts=
  if ! { pallas_tree "$pin" && harness=$(value_harness); } >"$OUT/value/harness.out" 2>&1; then
    cat "$OUT/value/harness.out"
    echo "VALUE head=FAIL pin=$pin logs=$OUT/value"
    return 1
  fi
  head=FAIL
  if value_probe head target/value-head && value_tier head "$OUT/value/head" "" >"$OUT/value/head.out" 2>&1; then
    head=PASS
  fi
  grep -E '^(DECODE-ORACLE|V1-FIELDS|PRE-V1|ATTACH|EXPECTED) ' "$OUT/value/head.out" || true
  verdicts="value-head=$head"
  control=FIRED
  if { git -C "$ROOT/$PALLAS" apply "$HERE/overlay/pallas.diff" &&
    git -C "$ROOT/$PALLAS" apply "$HERE/value/pallas.diff"; } >"$OUT/value/overlay.plant" 2>&1 &&
    value_probe overlay target/value-overlay; then
    mkdir -p "$OUT/value/control"
    control=CLEAN
    values=0
    for gen in "$OUT"/value/head/*.gen.jsonl; do
      base=$(basename "$gen" .gen.jsonl)
      "$OUT/value/bin/overlay" "${base%%.*}" <"$gen" >"$OUT/value/control/$base.probe.out" 2>"$OUT/value/control/$base.err"
      if grep -q '^ORACLE_PLANT reached ' "$OUT/value/control/$base.probe.out"; then
        echo "VALUE control REACHED $base"
        control=FIRED
      fi
      if cmp -s "$OUT/value/head/$base.probe.jsonl" "$OUT/value/control/$base.probe.out"; then
        rm -f "$OUT/value/control/$base.probe.out"
      else
        echo "VALUE control DIFFERS $base"
        control=FIRED
      fi
      values=$((values + $(grep -c . "$gen")))
    done
    echo "VALUE control values=$values"
    if [ "$values" -eq 0 ]; then
      control=FIRED
    fi
    verdicts="$verdicts value-control=$control"
    for arm in $VALUE_ARMS; do
      ORACLE_PLANT=$arm value_tier overlay "$OUT/value/$arm" "$(value_pairs "$arm")" "$OUT/value/head" \
        >"$OUT/value/$arm.out" 2>&1 || true
      verdict=MISSED
      if value_caught "$OUT/value/$arm" "$(value_class "$arm")" "$arm"; then
        verdict=CAUGHT
      fi
      verdicts="$verdicts $(value_key "$arm")=$verdict"
    done
  else
    echo "VALUE OVERLAY_UNBUILT log=$OUT/value/overlay.plant"
    verdicts="$verdicts value-control=FIRED"
  fi
  # The values, the ledger results and the probe output are determined by the seed and the given files, so only the counts, the disagreements and the logs are kept.
  rm -f "$OUT"/value/*/*.gen.jsonl "$OUT"/value/*/*.seed.jsonl "$OUT"/value/*/*.oracle.jsonl \
    "$OUT"/value/*/*.reoracle.jsonl "$OUT"/value/*/*.probe.jsonl
  echo "VALUE $verdicts pin=$pin harness=$harness seconds=$(($(date +%s) - started)) logs=$OUT/value"
  case " $verdicts " in
    *MISSED* | *FAIL* | *FIRED*) return 1 ;;
  esac
}
