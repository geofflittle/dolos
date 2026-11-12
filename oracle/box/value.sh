# The value leg: the pallas conformance harness at the commit value/harness.rev names, judging the pallas commit the scratch tree pins with the Haskell ledger, its arms the switches value/pallas.diff adds over overlay/pallas.diff.
# Sourced by run.sh after lib.sh. Reads ROOT, OUT, SCRATCH, PALLAS, HERE, IN_IMAGE, LAST_VALUE, VALUE_RESULTS, VALUE_N, VALUE_SEED, VALUE_GEN_DIR and VALUE_HARNESS_URL.

VALUE_N=${VALUE_N:-100}
VALUE_SEED=${VALUE_SEED:-1}
# The generator binaries, each named for the sha256 of the oracle/Main.hs it was built from.
VALUE_GEN_DIR=${VALUE_GEN_DIR:-$ROOT/value-gen}
VALUE_HARNESS_URL=${VALUE_HARNESS_URL:-https://github.com/geofflittle/pallas.git}
LAST_VALUE=${LAST_VALUE:-$ROOT/runs/last-value}
# The output of each value tier step, in a dir named by the hash of what the step read.
VALUE_RESULTS=${VALUE_RESULTS:-$ROOT/value/results}
# The image the harness runs the generator and the Haskell ledger in.
VALUE_IMAGE=conformance-gen-hs:2
# The pallas tree the overlay probe reads.
VALUE_PALLAS=${VALUE_PALLAS:-scratch/value-pallas}
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

# The corpora of the scratch tree the value leg hands the harness tier, one path per line.
value_corpora() {
  for corpus in ic supp; do
    echo "$ROOT/$SCRATCH/test_data/ledger-oracle/$corpus.jsonl.gz"
  done
}

# The inputs of the value leg on the pallas pin $1 with the ledger image of id $2: the pin, the value count and seed, the image and the hash of its box files, corpora and rows of served.txt.
value_inputs() {
  echo "pin=$1 n=$VALUE_N seed=$VALUE_SEED image=$2 files=$({
    cat "$HERE/value.sh" "$HERE/value/harness.rev" "$HERE/value/pallas.diff" "$HERE/overlay/pallas.diff" $(value_corpora)
    grep -E '^value-' "$HERE/served.txt" || true
  } | sha256sum | cut -c1-12)"
}

# Whether the dir $1 of an arm's run holds a disagreement of the class $2, printing each with the name of its file.
value_caught() {
  (cd "$1" && grep -H -F "\"cls\": \"$2\"" -- *.dis.jsonl 2>/dev/null)
}

# Sets verdict for the value arm $1 from its tier output and its probes' reached lines, which $OUT/value/$1.ran collects.
value_verdict() {
  cat "$OUT/value/$1.out" "$OUT/value/$1"/*.reached >"$OUT/value/$1.ran" 2>/dev/null || true
  arm_verdict "$(value_key "$1")" "$OUT/value/$1.ran" '^DECODE-ORACLE values=[1-9]' "$1" value_caught "$OUT/value/$1" "$(value_class "$1")"
}

# Puts the harness of value/harness.rev in $ROOT/value/gen and the generator for its oracle/Main.hs in its bin.
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

# Builds the probe over the pallas tree $3, which $ROOT/value/conf names, into $OUT/value/bin/$1, with that tree's toolchain and the target $2.
value_probe() {
  ln -sfn "../$3" "$ROOT/value/conf"
  cp "$ROOT/$3/rust-toolchain.toml" "$ROOT/value/gen/probe-value/"
  if ! TARGET=$2 sh "$IN_IMAGE" value/gen/probe-value cargo build --release >"$OUT/value/$1.build" 2>&1; then
    echo "VALUE BUILD_FAILED probe=$1 log=$OUT/value/$1.build"
    return 1
  fi
  cp "$ROOT/$2/release/pallas-probe-value" "$OUT/value/bin/$1"
}

# Runs the harness tier into the dir $1 with the probe $2 over the pairs $3, reading the values of the dir $4 when given and tallying with $5 when given.
value_run_tier() {
  (
    cd "$ROOT/value/gen" &&
      PROBE=$2 PAIRS=$3 GEN_FROM=$4 TALLY=${5:-} TX_FILES=$tx_files BLOCK_FILES=$block_files EB_FILES=$eb_files \
        sh tier.sh "$1" "$VALUE_N" "$VALUE_SEED"
  )
}

# The pairs the harness tier runs when handed none, one kind:source per line, read from the status files of a tier run in the dir $1 that runs no step.
value_all_pairs() {
  rm -rf "$1"
  (
    unset PAIRS
    cd "$ROOT/value/gen" && GEN_FROM=$1 VRUN=/dev/null TALLY=true sh tier.sh "$1" "$VALUE_N" "$VALUE_SEED"
  ) >/dev/null 2>&1 || true
  for status in "$1"/*.status; do
    [ -e "$status" ] || continue
    name=$(basename "$status" .status)
    echo "${name%%.*}:${name#*.}"
  done
  rm -rf "$1"
}

# The hash of the words $1 and of the bytes and paths of the files that follow.
value_hash() {
  {
    echo "$1"
    shift
    [ $# -eq 0 ] || sha256sum -- "$@"
  } | sha256sum | cut -c1-16
}

# The key of the values and ledger results of the pair $1: what the harness generates them from.
value_values_key() {
  given=
  if [ "${1#*:}" = given ]; then
    case ${1%%:*} in
      tx) given=$tx_files ;;
      block) given=$block_files ;;
      eb) given=$eb_files ;;
    esac
  fi
  # shellcheck disable=SC2086
  value_hash "values harness=$harness_rev generator=$generator image=$image n=$VALUE_N seed=$VALUE_SEED pair=$1" $given
}

# Keeps the files of the pair $3 in the dir $2 whose names end in the suffixes that follow, or all of them, under VALUE_RESULTS/$1.
value_keep() {
  part=$VALUE_RESULTS/$1.part
  files=$2/${3%%:*}.${3#*:}
  shift 3
  rm -rf "$part"
  mkdir -p "$part"
  if [ $# -eq 0 ]; then
    ln -f "$files".* "$part/"
  else
    for suffix; do
      ln -f "$files.$suffix" "$part/"
    done
  fi && mv "$part" "${part%.part}"
}

# Puts in the dir $2 the values and ledger results of each pair of $1, making those VALUE_RESULTS does not hold with a tier run whose probe prints nothing, and keeping each whose ledger results number its values, and its values N unless given.
value_values() {
  step_started=$(date +%s)
  made=
  mkdir -p "$2"
  for pair in $1; do
    key=$(value_values_key "$pair")
    echo "$key" >>"$OUT/value/results.used"
    [ -d "$VALUE_RESULTS/$key" ] || made="$made $pair"
  done
  if [ -n "$made" ]; then
    value_run_tier "$2.made" true "$made" "" true >"$2.made.out" 2>&1 || true
    for pair in $made; do
      base=$2.made/${pair%%:*}.${pair#*:}
      [ -f "$base.gen.jsonl" ] && [ -f "$base.oracle.jsonl" ] || continue
      values=$(grep -c . "$base.gen.jsonl")
      if [ "$values" -gt 0 ] && [ "$(grep -c . "$base.oracle.jsonl")" -eq "$values" ] &&
        { [ "${pair#*:}" = given ] || [ "$values" -eq "$VALUE_N" ]; }; then
        value_keep "$(value_values_key "$pair")" "$2.made" "$pair" gen.jsonl oracle.jsonl
      fi
    done
    rm -rf "$2.made"
  fi
  for pair in $1; do
    key=$(value_values_key "$pair")
    [ ! -d "$VALUE_RESULTS/$key" ] || ln -f "$VALUE_RESULTS/$key"/* "$2/"
  done
  echo "TIER values pairs=$(echo $1 | wc -w) made=$(echo $made | wc -w) seconds=$(($(date +%s) - step_started))"
}

# Puts in the dir $2 the tier output of each pair of $3 run with the probe $1 over the values of the dir $4, running the tier only on pairs whose values and probe output VALUE_RESULTS holds no output for and keeping each that passes.
value_compare() {
  step_started=$(date +%s)
  rerun=
  kept=
  keys=
  rm -rf "$2.keys"
  mkdir -p "$2" "$2.keys"
  for pair in $3; do
    base=${pair%%:*}.${pair#*:}
    if [ ! -f "$4/$base.gen.jsonl" ]; then
      rerun="$rerun $pair"
      continue
    fi
    "$1" "${pair%%:*}" <"$4/$base.gen.jsonl" >"$2.keys/$base" 2>/dev/null || true
    key=$(value_hash "compare harness=$harness_rev generator=$generator image=$image n=$VALUE_N pair=$pair" \
      "$4/$base.gen.jsonl" "$4/$base.oracle.jsonl" "$2.keys/$base")
    echo "$key" >>"$OUT/value/results.used"
    if [ -d "$VALUE_RESULTS/$key" ]; then
      ln -f "$VALUE_RESULTS/$key"/* "$2/"
      kept="$kept $pair"
    else
      rerun="$rerun $pair"
      keys="$keys $pair=$key"
    fi
  done
  rm -rf "$2.keys"
  # A blank PAIRS runs no pair, and the tier then only tallies the dir.
  status=0
  value_run_tier "$2" "$1" "${rerun:- }" "$4" || status=1
  for entry in $keys; do
    pair=${entry%%=*}
    if [ "$(cat "$2/${pair%%:*}.${pair#*:}.status" 2>/dev/null)" = 0 ]; then
      value_keep "${entry#*=}" "$2" "$pair"
    fi
  done
  for pair in $kept; do
    [ "$(cat "$2/${pair%%:*}.${pair#*:}.status")" = 0 ] || status=1
  done
  echo "TIER compare pairs=$(echo $3 | wc -w) kept=$(echo $kept | wc -w) ran=$(echo $rerun | wc -w) seconds=$(($(date +%s) - step_started))"
  return $status
}

# Runs the harness tier over the pairs $3, or every pair, with the probe $1 into the dir $2, reading the values of the dir $4 when given and otherwise those VALUE_RESULTS holds or the tier makes.
value_tier() {
  fixtures=$ROOT/$PALLAS/test_data
  tx_files="$(value_corpora | tr '\n' ' ')$(ls "$fixtures"/dijkstra-*.tx | tr '\n' ' ')"
  block_files=$(ls "$fixtures"/dijkstra*.block | tr '\n' ' ')
  eb_files=$(ls "$fixtures"/*.ebbody | tr '\n' ' ')
  harness_rev=$(cat "$HERE/value/harness.rev")
  generator=$(sha256sum <"$ROOT/value/gen/bin/conformance-gen" | cut -c1-16)
  image=$(image_id "$VALUE_IMAGE")
  pairs=${3:-$(value_all_pairs "$2.pairs")}
  from=${4:-}
  if [ -z "$from" ]; then
    from=$2.values
    value_values "$pairs" "$from"
  fi
  value_compare "$OUT/value/bin/$1" "$2" "$pairs" "$from"
}

# Sets control to the verdict of the probe $3 run over each value of the head dir $1 into the dir $2: UNRUN when it ran no value, FIRED when it reached a plant or printed other than the head probe printed, and CLEAN otherwise.
value_control() {
  mkdir -p "$2"
  clean=yes
  values=0
  for gen in "$1"/*.gen.jsonl; do
    [ -e "$gen" ] || continue
    base=$(basename "$gen" .gen.jsonl)
    "$3" "${base%%.*}" <"$gen" >"$2/$base.probe.out" 2>"$2/$base.err"
    if grep -q '^ORACLE_PLANT reached ' "$2/$base.probe.out"; then
      echo "VALUE control REACHED $base"
      clean=no
    fi
    if cmp -s "$1/$base.probe.jsonl" "$2/$base.probe.out"; then
      rm -f "$2/$base.probe.out"
    else
      echo "VALUE control DIFFERS $base"
      clean=no
    fi
    values=$((values + $(grep -c . "$gen")))
  done
  echo "VALUE control values=$values"
  if [ "$values" -eq 0 ]; then
    control=UNRUN
  elif [ "$clean" = yes ]; then
    control=CLEAN
  else
    control=FIRED
  fi
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
  : >"$OUT/value/results.used"
  if value_probe head target/value-head "$PALLAS" && value_tier head "$OUT/value/head" "" >"$OUT/value/head.out" 2>&1; then
    head=PASS
  fi
  grep -E '^(DECODE-ORACLE|V1-FIELDS|PRE-V1|ATTACH|EXPECTED) ' "$OUT/value/head.out" || true
  sed -n 's/^TIER /VALUE head /p' "$OUT/value/head.out"
  verdicts="value-head=$head"
  if ! unplanted=$(plant_tree "$VALUE_PALLAS" "$ROOT/$PALLAS" "$pin" "" "$HERE/overlay/pallas.diff" \
    "$HERE/value/pallas.diff" 2>"$OUT/value/overlay.plant"); then
    echo "VALUE OVERLAY_UNPLANTED log=$OUT/value/overlay.plant"
    overlay=UNPLANTED
  elif ! value_probe overlay target/value-overlay "$VALUE_PALLAS"; then
    overlay=UNBUILT
  else
    overlay=built
  fi
  sed -n 's/^REGEN /VALUE regen /p' "$OUT/value/overlay.plant"
  if [ "$overlay" != built ]; then
    verdicts="$verdicts value-control=$overlay"
    for arm in $VALUE_ARMS; do
      verdicts="$verdicts $(value_key "$arm")=$overlay"
    done
  else
    value_control "$OUT/value/head" "$OUT/value/control" "$OUT/value/bin/overlay"
    verdicts="$verdicts value-control=$control"
    unplanted=" $(echo $unplanted) "
    for arm in $VALUE_ARMS; do
      case $unplanted in
        *" $arm "*)
          echo "VALUE $arm UNPLANTED log=$OUT/value/overlay.plant"
          verdict=UNPLANTED
          ;;
        *)
          ORACLE_PLANT=$arm value_tier overlay "$OUT/value/$arm" "$(value_pairs "$arm")" "$OUT/value/head" \
            >"$OUT/value/$arm.out" 2>&1 || true
          sed -n "s/^TIER /VALUE $arm /p" "$OUT/value/$arm.out"
          value_verdict "$arm"
          ;;
      esac
      verdicts="$verdicts $(value_key "$arm")=$verdict"
    done
  fi
  # The run keeps the counts, the disagreements and the logs, and VALUE_RESULTS the values, ledger results and probe output.
  rm -rf "$OUT"/value/*.values
  rm -f "$OUT"/value/*/*.gen.jsonl "$OUT"/value/*/*.seed.jsonl "$OUT"/value/*/*.oracle.jsonl \
    "$OUT"/value/*/*.reoracle.jsonl "$OUT"/value/*/*.probe.jsonl
  if [ "$head" = PASS ]; then
    for kept in "$VALUE_RESULTS"/*; do
      [ -e "$kept" ] || continue
      grep -qxF "$(basename "$kept")" "$OUT/value/results.used" || rm -rf "$kept"
    done
  fi
  echo "VALUE $verdicts pin=$pin harness=$harness seconds=$(($(date +%s) - started)) logs=$OUT/value"
}
