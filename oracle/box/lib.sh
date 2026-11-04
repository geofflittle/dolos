# Functions the oracle box scripts share, reading ROOT, SCRATCH, REV, MIN_FREE_G, HERE, TARGET, IN_IMAGE, LOCK_POLL_SECONDS, SERVE_CONFIG, CANDIDATE_GRPC, CANDIDATE_FSYNC, NODE, ENDPOINT, GRPCURL and the IO_ limits.

PALLAS=${PALLAS:-scratch/pallas}
IN_IMAGE=${IN_IMAGE:-$HERE/in-image.sh}
# One line for each unit of the target dir, the tree ids of the scratch trees its last build read and its output files.
BUILT="$ROOT/${TARGET:-target}/built-units"
# The arms that plant a defect, each judged on the records plants/<arm>.records names.
PLANTED_ARMS="colret coin inflight rodrigo-reverted rob-reverted cost revalidate revalidate-drops subouts byron-attrs byron-signer byron-guard chaincode minfee-subtx emptyguards tag24 v1v2 byron-out tag24-ref unreg-refund drep-deposit upper-inclusive slot-off-by-one horizon-removed system-start cost-models guard-ref-reverted native-ref-reencoded v2-ref-dropped v4-ref-refused v1-ref-exposed pointer-refused script-payment-swap signatories-unsorted withdrawal-order cert-unwrapped script-guard-admitted v3-cert-wrapped v2-withdrawal-unwrapped top-guards-admitted"

# The arms whose switch an overlay diff adds, one per line.
switches() {
  sed -n 's/^+.*oracle_plant("\([a-z0-9-]*\)").*/\1/p' "$@" | sort -u
}

# Whether an arm is a switch of the overlay, judged on the one overlay build with ORACLE_PLANT naming it.
overlay_arm() {
  switches "$HERE"/overlay/*.diff | grep -q -x -F "$1"
}

# Whether an arm plants its defect in pallas.
pallas_arm() {
  if overlay_arm "$1"; then
    switches "$HERE/overlay/pallas.diff" | grep -q -x -F "$1"
  else
    sed -n 's#^+++ b/##p' "$HERE/plants/$1.diff" 2>/dev/null | grep -q '^pallas'
  fi
}

# The cargo arguments of the crate test an arm runs in place of the judge, none for an arm the judge runs.
crate_test() {
  case $1 in
    cost-models) echo "-p dolos-cardano --features strict --test musashi_cost_models" ;;
    *) return 1 ;;
  esac
}

# The Dolos files that name the pallas commit Dolos builds against.
PALLAS_PIN_FILES="Cargo.toml Cargo.lock"

# The pallas entries of the file $3 at the commit $2 of the repository $1, its pallas lines for Cargo.toml and its pallas packages for Cargo.lock.
pallas_entries() {
  git -C "$1" show "$2:$3" 2>/dev/null | case $3 in
    Cargo.lock) awk -v RS= '/name = "pallas/' ;;
    *) grep '^pallas' ;;
  esac
}

# The files under the paths after $3 that differ between the commits $2 and $3 of the repository $1, a pallas pin file only when its pallas entries differ.
differing_files() {
  repo=$1
  from=$2
  to=$3
  shift 3
  names=$(git -C "$repo" diff --name-only "$from" "$to" -- "$@") || return
  echo "$names" | while read -r name; do
    case " $PALLAS_PIN_FILES " in
      *" $name "*)
        if [ "$(pallas_entries "$repo" "$from" "$name")" = "$(pallas_entries "$repo" "$to" "$name")" ]; then
          continue
        fi
        ;;
    esac
    if [ -n "$name" ]; then
      echo "$name"
    fi
  done
}

# The Dolos files whose change puts an arm in the quick tier, the pin files for a pallas arm.
plant_files() {
  if pallas_arm "$1"; then
    printf '%s\n' $PALLAS_PIN_FILES
    return
  fi
  if overlay_arm "$1"; then
    awk -v call="oracle_plant(\"$1\")" '/^\+\+\+ b\// { file = substr($0, 7) } /^\+/ && index($0, call) { print file }' \
      "$HERE/overlay/dolos.diff" | sort -u
    return
  fi
  sed -n 's#^+++ b/##p' "$HERE/plants/$1.diff"
}

# Whether two test logs are the same but for the test harness's run time.
same_log() {
  sed 's/; finished in [0-9.]*s$//' "$1" >"$1.untimed"
  sed 's/; finished in [0-9.]*s$//' "$2" >"$2.untimed"
  cmp -s "$1.untimed" "$2.untimed"
}

# The whole gibibytes free on the filesystem holding $1, rounded down.
free_g() {
  echo $(($(df -B1 --output=avail "$1" | tail -1 | tr -dc 0-9) / 1073741824))
}

# Exits when the build root has less than MIN_FREE_G gigabytes free.
require_free() {
  free=$(free_g "$ROOT")
  if [ "$free" -lt "$MIN_FREE_G" ]; then
    echo "REFUSED free=${free}G below ${MIN_FREE_G}G at $ROOT"
    exit 2
  fi
}

LOCK_POLL_SECONDS=${LOCK_POLL_SECONDS:-30}

# Takes the lock of the tiers on ROOT, a link run.lock naming this process, waiting while a running process holds it and refusing when the process it names has exited.
take_lock() {
  lock=$ROOT/run.lock
  waited=
  until ln -sn "$$" "$lock" 2>/dev/null; do
    holder=$(readlink "$lock" 2>/dev/null) || continue
    if ! kill -0 "$holder" 2>/dev/null; then
      echo "REFUSED lock=$lock lock_pid=$holder stale"
      exit 2
    fi
    if [ "$holder" != "$waited" ]; then
      echo "WAITING lock_pid=$holder"
      waited=$holder
    fi
    sleep "$LOCK_POLL_SECONDS"
  done
  trap release_lock EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
}

# Removes the lock of the tiers when it names this process.
release_lock() {
  if [ "$(readlink "$ROOT/run.lock" 2>/dev/null)" = "$$" ]; then
    rm -f "$ROOT/run.lock"
  fi
}

# The names of a candidate's config files in its store.
CONFIG_FILES="dolos.toml byron-genesis.json shelley-genesis.json alonzo-genesis.json conway-genesis.json dijkstra-genesis.json"

# Writes the candidate's config into the dir $1: the serving genesis files, and the serving dolos.toml without its serve sections followed by the candidate's grpc listener.
write_config() {
  for file in $CONFIG_FILES; do
    case $file in
      dolos.toml)
        awk '/^\[/ { skip = ($0 ~ /^\[serve/) } !skip' "$SERVE_CONFIG/dolos.toml" >"$1/dolos.toml"
        printf '[serve.grpc]\nlisten_address = "%s"\n' "$CANDIDATE_GRPC" >>"$1/dolos.toml"
        ;;
      *) cat "$SERVE_CONFIG/$file" >"$1/$file" ;;
    esac
  done
}

# The candidate's fsync mode from CANDIDATE_FSYNC, off when unset, where off runs it with libeatmydata preloaded so its sync calls return without writing, refusing any value but off or on.
candidate_fsync() {
  case ${CANDIDATE_FSYNC:-off} in
    off | on) echo "${CANDIDATE_FSYNC:-off}" ;;
    *)
      echo "LIVE-CANDIDATE REFUSED candidate_fsync=$CANDIDATE_FSYNC want=off,on" >&2
      return 2
      ;;
  esac
}

# The fsync state of a process from its maps in the file $1, off when they name libeatmydata, on when they name libc but not libeatmydata, which the loader maps before libc, and unknown when they name neither or cannot be read.
fsync_state() {
  if grep -q eatmydata "$1" 2>/dev/null; then
    echo fsync=off
  elif grep -q '/libc\.so' "$1" 2>/dev/null; then
    echo fsync=on
  else
    echo fsync=unknown
  fi
}

# The slot of the wall clock, from the system start and slot length of $SERVE_CONFIG/shelley-genesis.json.
wall_slot() {
  genesis=$SERVE_CONFIG/shelley-genesis.json
  start=$(jq -er .systemStart "$genesis") || return 1
  start=$(date -u -d "$start" +%s) || return 1
  length=$(jq -er .slotLength "$genesis") || return 1
  awk -v now="$(date +%s)" -v start="$start" -v len="$length" 'BEGIN { printf "%d\n", (now - start) / len }'
}

# The limits of the IO guard on the node's lag behind the wall clock, the serving Dolos's lag behind the node and its ReadTip time, and the run of samples over a limit that breaches it.
IO_NODE_LAG_S=${IO_NODE_LAG_S:-120}
IO_SERVE_LAG_SLOTS=${IO_SERVE_LAG_SLOTS:-60}
IO_SERVE_MS=${IO_SERVE_MS:-2000}
IO_BREACH_SAMPLES=3

# One LIVE-IO sample of the node tip's lag behind the wall clock in seconds, the serving Dolos tip's lag behind the node tip in slots and the serving Dolos's ReadTip time in milliseconds, each failed read printed as failed.
io_line() {
  wall=$(wall_slot) || wall=
  node=$(docker exec "$NODE" cardano-cli latest query tip 2>/dev/null | jq -r '.slot // empty') || node=
  before=$(date +%s%N)
  serve=$("$GRPCURL" -max-time 10 -d '{}' "$ENDPOINT" utxorpc.v1.sync.SyncService/ReadTip 2>/dev/null |
    jq -r '.tip.slot // empty') || serve=
  ms=$((($(date +%s%N) - before) / 1000000))
  node_lag=failed
  serve_lag=failed
  if [ -n "$wall" ] && [ -n "$node" ]; then
    node_lag=$((wall - node))
  fi
  if [ -z "$serve" ]; then
    ms=failed
  elif [ -n "$node" ]; then
    serve_lag=$((node - serve))
  fi
  echo "LIVE-IO node_lag_s=$node_lag serve_lag_slots=$serve_lag serve_ms=$ms"
}

# The run of consecutive LIVE-IO samples over a limit, from the run $1 before the sample line $2, a value that is missing or not a whole number counting as over.
io_streak() {
  over=0
  for limit in "node_lag_s $IO_NODE_LAG_S" "serve_lag_slots $IO_SERVE_LAG_SLOTS" "serve_ms $IO_SERVE_MS"; do
    value=$(echo " $2 " | sed -n "s/.* ${limit% *}=\([^ ]*\) .*/\1/p")
    case ${value#-} in
      '' | *[!0-9]*) over=1 ;;
      *)
        if [ "$value" -gt "${limit#* }" ]; then
          over=1
        fi
        ;;
    esac
  done
  if [ "$over" -eq 1 ]; then
    echo $(($1 + 1))
  else
    echo 0
  fi
}

# Whether a run of $1 samples over a limit breaches the IO guard.
io_breached() {
  [ "$1" -ge "$IO_BREACH_SAMPLES" ]
}

# Puts the scratch tree at REV of the bare repository $ROOT/dolos.git with no planted change.
reset_scratch() {
  if [ ! -d "$ROOT/$SCRATCH/.git" ]; then
    git clone -q --no-checkout "$ROOT/dolos.git" "$ROOT/$SCRATCH"
  fi
  git -C "$ROOT/$SCRATCH" fetch -q "$ROOT/dolos.git" "$REV"
  git -C "$ROOT/$SCRATCH" checkout -q -f --detach FETCH_HEAD
  git -C "$ROOT/$SCRATCH" clean -q -fdx
}

# The tree id of a work tree's files, staged in an index of its own, or none when the tree is absent.
tree_of() {
  if [ ! -d "$1/.git" ]; then
    echo none
    return
  fi
  index="$1/.git/built-index"
  rm -f "$index"
  GIT_INDEX_FILE=$index git -C "$1" add -A
  GIT_INDEX_FILE=$index git -C "$1" write-tree
  rm -f "$index"
}

# The files that differ between an old and a new tree id of a work tree, each with a prefix, every file of the new tree when the old one is not in the repository.
changed_files() {
  if [ "$3" = none ]; then
    return
  fi
  if [ "$2" != none ] && git -C "$1" cat-file -e "$2^{tree}" 2>/dev/null; then
    git -C "$1" diff --name-only "$2" "$3"
  else
    git -C "$1" ls-tree -r --name-only "$3"
  fi | sed "s#^#$4#"
}

# The first dep-info file rustc wrote beside one of the output files of a unit, its outputs given comma separated.
dep_info() {
  echo "$1" | tr , '\n' | while read -r out; do
    out=${out#/w/}
    stem=$(basename "$out" | sed -E 's/^lib(.*)\.(rlib|rmeta|so|a)$/\1/')
    # A build script's output is a link named build-script-<name> in a dir named after the package and the unit hash.
    case $stem in
      build-script-*)
        dir=$(dirname "$out")
        stem="$(echo "$stem" | tr - _)-${dir##*-}"
        ;;
    esac
    if [ -f "$ROOT/$(dirname "$out")/$stem.d" ]; then
      echo "$ROOT/$(dirname "$out")/$stem.d"
      break
    fi
  done
}

# The output files of each unit a message file names, comma separated, one line per unit, only the fresh ones when $2 is fresh.
units() {
  only=
  if [ "${2:-}" = fresh ]; then
    only='"fresh":true'
  fi
  grep '"reason":"compiler-artifact"' "$1" | grep -F -e "$only" |
    sed -n 's/.*"filenames":\[\([^]]*\)\].*/\1/p' | tr -d '"'
}

# One line for each unit cargo reports fresh whose dep-info names a file changed between the trees of its last build and the trees $2 and $3, or that has no dep-info.
fresh_reading() {
  units "$1" fresh | while read -r outs; do
    built=$(awk -v outs="$outs" '$3 == outs { print $1, $2 }' "$BUILT" 2>/dev/null || true)
    built=${built:-none none}
    changed="$1.changed-$(echo "$built" | tr ' ' -)"
    if [ ! -f "$changed" ]; then
      {
        changed_files "$ROOT/$SCRATCH" "${built% *}" "$2" ""
        changed_files "$ROOT/$PALLAS" "${built#* }" "$3" "/w/$PALLAS/"
      } >"$changed"
    fi
    d=$(dep_info "$outs")
    if [ -z "$d" ]; then
      echo "STALE unit=$outs dep_info=missing"
      continue
    fi
    tr ' ' '\n' <"$d" | sed "s/:\$//; s#^/w/$SCRATCH/##" | grep -x -F -f "$changed" | sort -u |
      sed "s#^#STALE unit=$outs file=#"
  done
}

# Records the trees $2 and $3 for each unit a message file names, keeping the record of every other unit.
record_units() {
  units "$1" >"$1.units"
  {
    if [ -f "$BUILT" ]; then
      awk 'NR == FNR { named[$0] = 1; next } !($3 in named)' "$1.units" "$BUILT"
    fi
    sed "s#^#$2 $3 #" "$1.units"
  } >"$BUILT.new"
  mv "$BUILT.new" "$BUILT"
}

# Runs cargo with the arguments in the scratch tree, cargo's messages in $1, and refuses when cargo reused a unit that reads a file changed since that unit's last build.
build_checked() {
  json=$1
  shift
  dolos=$(tree_of "$ROOT/$SCRATCH")
  pallas=$(tree_of "$ROOT/$PALLAS")
  if [ ! -f "$BUILT" ]; then
    # With no record of any build, every file gets a new time, so cargo rebuilds each unit that reads one.
    for tree in "$SCRATCH" "$PALLAS"; do
      if [ -d "$ROOT/$tree/.git" ]; then
        git -C "$ROOT/$tree" ls-files -z --cached --others --exclude-standard | (cd "$ROOT/$tree" && xargs -0 touch)
        # The index takes the new times too, so a later checkout of the same commit keeps each file and its time.
        git -C "$ROOT/$tree" update-index -q --refresh || true
      fi
    done
  fi
  # A build that fails or reuses a stale unit leaves units built from more than one tree, so it removes the record and the next build starts cold.
  if ! sh "$IN_IMAGE" "$SCRATCH" cargo "$@" --message-format=json >"$json" 2>"$json.err"; then
    rm -f "$BUILT"
    echo "BUILD_FAILED log=$json.err"
    return 1
  fi
  rm -f "$json".changed-*
  fresh_reading "$json" "$dolos" "$pallas" >"$json.stale"
  if [ -s "$json.stale" ]; then
    rm -f "$BUILT"
    head -4 "$json.stale"
    return 1
  fi
  # A fresh unit that reads no changed file has the inputs it would have at the current trees.
  record_units "$json" "$dolos" "$pallas"
}

# Builds the test binary of the scratch tree's dolos library, cargo's messages in $1 and the rest of the arguments passed to cargo.
build_judge() {
  json=$1
  shift
  build_checked "$json" test --release -p dolos --lib --no-run "$@"
}

# The executable cargo built that a message file names, the test binary of a no run test build or the binary of a build.
executable() {
  sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' "$1" | tail -1
}
