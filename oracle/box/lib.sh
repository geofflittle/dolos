# Functions and defaults the oracle box scripts share, reading ROOT, SCRATCH, REV, MIN_FREE_G, OUT, HERE, TARGET, IN_IMAGE, LOCK_POLL_SECONDS, RUN_LOCK, LAST_DONE, LAST_SHIP, SERVE_CONFIG, CANDIDATE_GRPC, CANDIDATE_FSYNC, NODE, ENDPOINT, GRPCURL, the ORACLE_ slice settings, CGROUP_FILE, CGROUP_ROOT and the IO_ limits.

PALLAS=${PALLAS:-scratch/pallas}
SCRATCH=${SCRATCH:-scratch/dolos}
# The clone each planted state is made in, whose files every plant writes again.
PLANT=${PLANT:-scratch/plant}
# The Dolos and pallas trees the overlay build reads, and the target dir it writes.
OVERLAY_DOLOS=${OVERLAY_DOLOS:-scratch/overlay-dolos}
OVERLAY_PALLAS=${OVERLAY_PALLAS:-scratch/overlay-pallas}
OVERLAY_TARGET=${OVERLAY_TARGET:-target/overlay}
REV=${REV:-feat/ledger-oracle}
MIN_FREE_G=${MIN_FREE_G:-6}
OUT=${OUT:-"$ROOT/runs/$(date -u +%Y%m%dT%H%M%SZ)"}
IN_IMAGE=${IN_IMAGE:-$HERE/in-image.sh}
# The image in-image.sh runs builds and tests in.
BUILD_IMAGE='dolos-build:1'
export BUILD_IMAGE
GRPCURL=${GRPCURL:-grpcurl}
# The Dolos path of the box scripts, which holds the arms manifest and each arm's plant and records.
BOX_PATH=oracle/box
# The Dolos test module that judges the corpora and the live node.
ORACLE_TESTS=serve::grpc::v1::ledger_oracle_tests
# The arms whose switch an overlay diff adds, one per line.
switches() {
  sed -n 's/^+.*oracle_plant("\([a-z0-9-]*\)").*/\1/p' "$@" | sort -u
}

# The arms that plant a defect, each judged on the records plants/<arm>.records names.
PLANTED_ARMS=$({
  switches "$HERE"/overlay/*.diff
  for plant in "$HERE"/plants/*.diff "$HERE"/plants/*.deselect; do
    if [ -f "$plant" ]; then basename "$plant" | sed 's/\.[a-z]*$//'; fi
  done
} | sort -u | paste -sd ' ' -)

# Whether an arm leaves the records plants/<arm>.deselect names out of the head's coverage counts.
deselect_arm() {
  [ -f "$HERE/plants/$1.deselect" ]
}

# The arms it is given in judging order, one per line, head first and each deselect arm next, so a deselect arm runs on the head's build before another arm's build changes the target.
arm_order() {
  for arm in "$@"; do
    if [ "$arm" = head ]; then echo "$arm"; fi
  done
  for arm in "$@"; do
    if [ "$arm" != head ] && deselect_arm "$arm"; then echo "$arm"; fi
  done
  for arm in "$@"; do
    if [ "$arm" != head ] && ! deselect_arm "$arm"; then echo "$arm"; fi
  done
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
    endorser-tx-dropped) echo "-p dolos --lib" ;;
    unsupported-untyped) echo "-p dolos-cardano --lib" ;;
    watch-block-copied) echo "-p dolos --lib" ;;
    *) return 1 ;;
  esac
}

# The Dolos source file of the crate test an arm runs, none for an arm the judge runs.
crate_test_source() {
  case $1 in
    cost-models) echo crates/cardano/tests/musashi_cost_models.rs ;;
    endorser-tx-dropped) echo src/serve/grpc/v1/ledger_oracle_tests/endorser.rs ;;
    unsupported-untyped) echo crates/cardano/src/validate.rs ;;
    watch-block-copied) printf '%s\n' src/serve/grpc/v1beta/watch.rs src/serve/grpc/v1alpha/watch.rs ;;
  esac
}

# The arguments that pick an arm's test out of its crate test binary, none when the binary holds that test alone.
crate_test_filter() {
  case $1 in
    cost-models) echo "musashi_era_translation_keeps_the_published_v3_cost_model --exact" ;;
    endorser-tx-dropped) echo "$ORACLE_TESTS::endorser::served_endorser_transactions_are_the_nodes --exact" ;;
    unsupported-untyped) echo "validate::evaluation_tests::an_unsupported_evaluation_keeps_its_label --exact" ;;
    watch-block-copied) echo "serve::grpc::v1beta::watch::tests::the_transactions_of_one_block_share_its_bytes serve::grpc::v1alpha::watch::tests::the_transactions_of_one_block_share_its_bytes --exact" ;;
  esac
}

# Whether the crate test log $2 of arm $1 shows a passing result and each test its filter names passing.
crate_test_passed() {
  grep -q '^test result: ok\. [1-9][0-9]* passed' "$2" || return 1
  for name in $(crate_test_filter "$1"); do
    case $name in
      --*) ;;
      *) grep -q -x -F "test $name ... ok" "$2" || return 1 ;;
    esac
  done
}

# The arms among those given that run a crate test, on one line.
crate_arms() {
  for arm in "$@"; do
    if crate_test "$arm" >/dev/null; then echo "$arm"; fi
  done | paste -sd ' ' -
}

# Whether an arm's crate test is the lib test of a package with no feature of its own, which builds in the judge's cargo invocation.
joint_crate_test() {
  case $(crate_test "$1" 2>/dev/null) in
    "-p "*" --lib") ;;
    *) return 1 ;;
  esac
}

# The cargo arguments that add to the judge's build the joint crate tests of the arms given, beside the judge's own package.
joint_packages() {
  for arm in "$@"; do
    if joint_crate_test "$arm"; then crate_test "$arm" | cut -d' ' -f2; fi
  done | sort -u | grep -v -x dolos | sed 's/^/-p /' | paste -sd ' ' -
}

# The cargo target name of the binary an arm's crate test runs.
crate_test_target() {
  args=$(crate_test "$1") || return
  set -- $args
  target=
  while [ $# -gt 0 ]; do
    case $1 in
      -p) target=$(echo "$2" | tr - _) ;;
      --test) target=$2 ;;
    esac
    shift
  done
  echo "$target"
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
  if deselect_arm "$1"; then
    return
  fi
  sed -n 's#^+++ b/##p' "$HERE/plants/$1.diff"
}

# The Dolos files whose change puts an arm in the quick tier, its plant files, its plant or deselection, its records file and the source of its crate test.
arm_files() {
  plant_files "$1"
  if ! overlay_arm "$1"; then
    echo "$BOX_PATH/plants/$1.diff"
    echo "$BOX_PATH/plants/$1.deselect"
  fi
  echo "$BOX_PATH/plants/$1.records"
  crate_test_source "$1"
}

# The arms whose files differ or whose catches differ between the commits $1 and $2 of the scratch tree, one per line, failing when the files cannot be listed.
touched_arms() {
  differing_files "$ROOT/$SCRATCH" "$1" "$2" >"$OUT/changed" || return
  git -C "$ROOT/$SCRATCH" show "$1:$BOX_PATH/arms.toml" >"$OUT/arms.from" 2>/dev/null || : >"$OUT/arms.from"
  git -C "$ROOT/$SCRATCH" show "$2:$BOX_PATH/arms.toml" >"$OUT/arms.to" 2>/dev/null || : >"$OUT/arms.to"
  for arm in $PLANTED_ARMS; do
    if arm_files "$arm" | grep -q -x -F -f "$OUT/changed"; then
      echo "$arm"
    elif [ "$(catches "$arm" "$OUT/arms.from")" != "$(catches "$arm" "$OUT/arms.to")" ]; then
      echo "$arm"
    fi
  done
}

# Whether two test logs are the same but for the test harness's run time.
same_log() {
  sed 's/; finished in [0-9.]*s$//' "$1" >"$1.untimed"
  sed 's/; finished in [0-9.]*s$//' "$2" >"$2.untimed"
  cmp -s "$1.untimed" "$2.untimed"
}

# Whether two crate test logs hold the same lines but for the run time, in any order, as libtest prints each test's line when the test ends.
same_crate_log() {
  LC_ALL=C sort "$1" >"$1.sorted"
  LC_ALL=C sort "$2" >"$2.sorted"
  same_log "$1.sorted" "$2.sorted"
}

# Prints HELD when an encoding arm's log $1 holds the lines of the unplanted log $2 in any order but for its ORACLE_PLANT line and a moved bytes_equal count, FIRED when any other line differs, and MISSED when the count did not move.
encoding_verdict() {
  grep -v '^ORACLE_PLANT reached ' "$1" | sed 's/ bytes_equal=[0-9]*//' | LC_ALL=C sort >"$1.blind" || true
  sed 's/ bytes_equal=[0-9]*//' "$2" | LC_ALL=C sort >"$2.blind"
  planted=$(grep -o 'bytes_equal=[0-9]*' "$1" || true)
  unplanted=$(grep -o 'bytes_equal=[0-9]*' "$2" || true)
  if ! same_log "$1.blind" "$2.blind"; then
    echo FIRED
  elif [ -z "$planted" ] || [ "$planted" = "$unplanted" ]; then
    echo MISSED
  else
    echo HELD
  fi
}

# Sets verdict to UNRUN and fails when no line of the output $1 holds the pattern $2 that a finished run prints, and to UNREACHED when $3 names a plant and no line of the output is its reached line.
arm_ran() {
  if ! grep -q -e "$2" "$1" 2>/dev/null; then
    verdict=UNRUN
    return 1
  fi
  if [ -n "$3" ] && ! grep -q -x -F "ORACLE_PLANT reached $3" "$1"; then
    verdict=UNREACHED
    return 1
  fi
}

# Sets verdict for the arm $1 whose run wrote the output $2 to what arm_ran sets when the output lacks the pattern $3 or the plant $4, else to CAUGHT when the command after $4 succeeds and MISSED when it fails, recording the lines the command prints as what the arm served.
arm_verdict() {
  name=$1
  shift
  arm_ran "$1" "$2" "$3" || return 0
  shift 3
  mkdir -p "$OUT/served"
  if "$@" >"$OUT/served/$name.printed"; then
    verdict=CAUGHT
  else
    verdict=MISSED
  fi
  record_served "$name" <"$OUT/served/$name.printed"
  rm -f "$OUT/served/$name.printed"
}

# Writes the lines on stdin to $OUT/served/$1 as what the arm $1 served, in byte order, without the number libtest gives a panicking test's thread, and with each panic's line in a file of the scratch tree given as the name of the fn whose line is the nearest above it and the count of lines between them.
record_served() {
  mkdir -p "$OUT/served"
  sed "s/^\(thread '[^']*'\) ([0-9]*) panicked at /\1 panicked at /" |
    awk -v tree="$ROOT/$SCRATCH" '
      match($0, / panicked at [^:]+:[0-9]+:[0-9]+:$/) {
        head = substr($0, 1, RSTART + 12)
        split(substr($0, RSTART + 13, RLENGTH - 14), site, ":")
        path = tree "/" site[1]
        name = ""
        read = 0
        while (read < site[2] + 0 && (getline text <path) > 0) {
          read++
          if (text ~ /^[ \t]*(pub(\([^)]*\))?[ \t]+)?((const|async|unsafe)[ \t]+)*fn[ \t]+[A-Za-z_]/) {
            match(text, /fn[ \t]+[A-Za-z_][A-Za-z0-9_]*/)
            name = substr(text, RSTART, RLENGTH)
            sub(/^fn[ \t]+/, "", name)
            at = read
          }
        }
        close(path)
        if (read == site[2] + 0 && name != "") {
          print head site[1] ":" name "+" (read - at) ":" site[3] ":"
          next
        }
      }
      { print }' | LC_ALL=C sort >"$OUT/served/$1"
}

# The row of served.txt for what the arm $1 served in this run: its name, its line count and the sha256 of its lines.
served_row() {
  echo "$1 lines=$(wc -l <"$OUT/served/$1" | tr -dc 0-9) sha256=$(sha256sum <"$OUT/served/$1" | cut -c1-64)"
}

# Sets checked to the verdict list $1 with each CAUGHT or HELD verdict turned to UNRECORDED when this run recorded nothing its arm served, to UNREFERENCED when served.txt holds no row for the arm and to CHANGED when its row differs from this run's, adding each row of this run to $OUT/served.txt and printing a SERVED line for each verdict turned.
served_check() {
  checked=
  for checked_verdict in $1; do
    checked_arm=${checked_verdict%%=*}
    case $checked_verdict in
      *=CAUGHT | *=HELD)
        reference=$(awk -v arm="$checked_arm" '$1 == arm' "$HERE/served.txt" 2>/dev/null || true)
        row=
        if [ ! -f "$OUT/served/$checked_arm" ]; then
          checked_verdict=$checked_arm=UNRECORDED
        else
          row=$(served_row "$checked_arm")
          echo "$row" >>"$OUT/served.txt"
          if [ -z "$reference" ]; then
            checked_verdict=$checked_arm=UNREFERENCED
          elif [ "$reference" != "$row" ]; then
            checked_verdict=$checked_arm=CHANGED
          fi
        fi
        case $checked_verdict in
          *=UNRECORDED | *=UNREFERENCED | *=CHANGED)
            echo "SERVED $checked_verdict reference=\"$reference\" run=\"$row\" lines=$OUT/served/$checked_arm"
            ;;
        esac
        ;;
    esac
    checked="${checked:+$checked }$checked_verdict"
  done
}

# Applies the overlay diff $1 over the work tree $2, keeping each hunk that applies, prints the switch each rejected hunk adds, and fails when a rejected hunk adds no switch or a file of the diff did not apply hunk by hunk.
plant_overlay() {
  status=0
  applied=$(git -C "$2" apply -v --reject "$1" 2>&1) || status=1
  echo "$applied" >&2
  if [ "$status" -eq 0 ]; then
    return 0
  fi
  rejected=0
  for file in $(sed -n 's#^diff --git a/.* b/##p' "$1"); do
    if echo "$applied" | grep -q -x -F "Applied patch $file cleanly."; then
      continue
    elif [ -f "$2/$file.rej" ]; then
      rejected=$((rejected + 1))
      awk '
        /^@@/ { if (hunks && !named) shared = 1; hunks++; named = 0; next }
        /^\+/ && match($0, /oracle_plant\("[a-z0-9-]*"\)/) { print substr($0, RSTART + 14, RLENGTH - 16); named = 1 }
        END { if (hunks && !named) shared = 1; exit shared }' "$2/$file.rej" || status=2
    else
      status=2
    fi
  done
  [ "$rejected" -gt 0 ] && [ "$status" -eq 1 ]
}

# Commits the plant clone's files over the parent $1 without the rejected hunks plant_overlay leaves, and prints the commit, the same one for the same files and parent.
planted_commit() {
  git -C "$ROOT/$PLANT" ls-files -z --others -- '*.rej' | (cd "$ROOT/$PLANT" && xargs -0 -r rm -f) &&
    planted_tree=$(tree_of "$ROOT/$PLANT") &&
    GIT_AUTHOR_NAME=oracle GIT_AUTHOR_EMAIL=oracle GIT_AUTHOR_DATE='@1 +0000' \
      GIT_COMMITTER_NAME=oracle GIT_COMMITTER_EMAIL=oracle GIT_COMMITTER_DATE='@1 +0000' \
      git -C "$ROOT/$PLANT" commit-tree "$planted_tree" -p "$1" -m planted
}

# Puts the clone $1 at the commit $3 of the repository $2 with each diff after $4 planted over it hunk by hunk, and its pallas crates pointed at the tree $4 when $4 is not empty, making each state in the plant clone so the clone $1 writes only the files whose content differs from its last state, and prints the switch each rejected hunk adds and, on stderr, whether git writes each diff as committed.
plant_tree() {
  plant_at=$1
  plant_paths=$4
  clone_at "$PLANT" "$2" "$3" >&2 || return 2
  plant_state=$(git -C "$ROOT/$PLANT" rev-parse HEAD) || return 2
  shift 4
  for plant_diff; do
    plant_overlay "$plant_diff" "$ROOT/$PLANT" || return
    plant_parent=$plant_state
    plant_state=$(planted_commit "$plant_parent") || return 2
    plant_regen=differs
    if git -C "$ROOT/$PLANT" diff --no-color --abbrev=8 "$plant_parent" "$plant_state" | cmp -s - "$plant_diff"; then
      plant_regen=same
    fi
    echo "REGEN diff=${plant_diff#"$HERE"/} $plant_regen" >&2
  done
  if [ -n "$plant_paths" ]; then
    pallas_paths "$ROOT/$PLANT" "$plant_paths" &&
      plant_state=$(planted_commit "$plant_state") || return 2
  fi
  clone_at "$plant_at" "$ROOT/$PLANT" "$plant_state" >&2 || return 2
}

# Whether a verdict list holds a passing head and every verdict in it names a passing word.
passed() {
  case " $1 " in
    *" head=PASS "*) ;;
    *) return 1 ;;
  esac
  for verdict in $1; do
    case $verdict in
      *=PASS | *=CLEAN | *=CAUGHT | *=HELD | value=passed-on-these-inputs | amaru=passed-on-these-inputs) ;;
      *) return 1 ;;
    esac
  done
}

# Records the commit $2 of a run of the tier $1 whose verdicts $3 pass, in LAST_DONE for the done and ship tiers and in LAST_SHIP with the verdicts and the UTC time for the ship tier and the live leg.
record_pass() {
  case $1 in
    done | ship) passed "$3" ;;
    live) [ "$3" = "live=PASS live-shift=CAUGHT" ] ;;
    *) false ;;
  esac || return 1
  case $1 in
    done | ship) echo "$2" >"$LAST_DONE" ;;
  esac
  case $1 in
    ship | live) echo "commit=$2 tier=$1 verdicts=\"$3\" time=$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$LAST_SHIP" ;;
  esac
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

# Prints the full sha of the commit the ref $2 names in $ROOT/dolos.git, or exits 2 with a REFUSED line naming the variable $1 when it names none.
full_commit() {
  if ! git -C "$ROOT/dolos.git" rev-parse --verify --quiet "$2^{commit}"; then
    echo "REFUSED $1=$2 names no commit in $ROOT/dolos.git" >&2
    exit 2
  fi
}

LOCK_POLL_SECONDS=${LOCK_POLL_SECONDS:-30}
# The file of the box's one oracle run lock, since the candidate's name, its port and the build disk are the box's.
RUN_LOCK=${RUN_LOCK:-/run/lock/dolos-oracle.lock}

# Takes the box lock, a flock on RUN_LOCK through fd 9 that the kernel drops once no process holds fd 9 open, kept across exec, waiting while another process holds it, and recording this process's pid in the file after reporting the pid a holder that exited left there.
take_lock() {
  if ! [ "/proc/$$/fd/9" -ef "$RUN_LOCK" ]; then
    exec 9>>"$RUN_LOCK"
    waited=
    while :; do
      status=0
      flock -n -E 75 9 || status=$?
      case $status in
        0) break ;;
        75) ;;
        *)
          echo "REFUSED lock=$RUN_LOCK flock_exit=$status"
          exit 2
          ;;
      esac
      holder=$(cat "$RUN_LOCK")
      if [ -n "$holder" ] && [ "$holder" != "$waited" ]; then
        echo "WAITING lock_pid=$holder"
        waited=$holder
      fi
      sleep "$LOCK_POLL_SECONDS"
    done
    holder=$(cat "$RUN_LOCK")
    if [ -n "$holder" ]; then
      echo "LOCK stale pid=$holder removed"
    fi
    echo "$$" >"$RUN_LOCK"
  fi
  trap release_lock EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
}

# Empties the box lock's record when it names this process, keeping the file that waiting runs hold open.
release_lock() {
  if [ "$(cat "$RUN_LOCK" 2>/dev/null)" = "$$" ]; then
    : >"$RUN_LOCK"
  fi
}

# The id of the local docker image $1, or none when docker has no such image.
image_id() {
  docker image inspect -f '{{.Id}}' "$1" 2>/dev/null || echo none
}

# Whether a leg whose inputs are $2 is due: the record $1 of the inputs of its last passing run is missing or differs.
leg_due() {
  [ ! -f "$1" ] || [ "$(cat "$1")" != "$2" ]
}

# The systemd slice every process and container of an oracle run is placed in, and the memory and swap it may use, sized so the node and the serving Dolos keep their peak working sets.
ORACLE_SLICE=${ORACLE_SLICE:-dolos-oracle.slice}
ORACLE_MEMORY=${ORACLE_MEMORY:-2560M}
ORACLE_SWAP=${ORACLE_SWAP:-1G}
CGROUP_FILE=${CGROUP_FILE:-/proc/self/cgroup}
CGROUP_ROOT=${CGROUP_ROOT:-/sys/fs/cgroup}
# The docker every oracle script calls, a command that places each container it starts in ORACLE_SLICE in front of the docker it was found as.
if [ -z "${ORACLE_DOCKER:-}" ]; then
  ORACLE_DOCKER=$(command -v docker || echo docker)
fi
case :$PATH: in
  *":$HERE/bin:"*) ;;
  *) PATH=$HERE/bin:$PATH ;;
esac
export ORACLE_SLICE ORACLE_DOCKER PATH

# The memory and swap limits of the slice this process runs in, or none when that slice is not ORACLE_SLICE.
slice_limits() {
  cgroup=$(sed -n 's/^0:://p' "$CGROUP_FILE")
  slice=${cgroup%/*}
  case $slice in
    */"$ORACLE_SLICE")
      echo "memory=$(cat "$CGROUP_ROOT$slice/memory.max") swap=$(cat "$CGROUP_ROOT$slice/memory.swap.max")"
      ;;
    *) echo none ;;
  esac
}

# Runs the script $1 again with the arguments after it in a scope of ORACLE_SLICE, after setting the slice's limits, when this process runs outside it, and exits 2 when the slice it runs in has no memory or swap limit.
enter_slice() {
  if [ "$(slice_limits)" = none ]; then
    systemctl set-property --runtime "$ORACLE_SLICE" MemoryMax="$ORACLE_MEMORY" MemorySwapMax="$ORACLE_SWAP" || exit 2
    exec systemd-run --scope --quiet --collect --slice="$ORACLE_SLICE" sh "$@"
  fi
  case $(slice_limits) in
    memory=[0-9]*" "swap=[0-9]*) ;;
    *)
      echo "REFUSED slice=$ORACLE_SLICE $(slice_limits)"
      exit 2
      ;;
  esac
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

# The pallas commit the scratch tree's Cargo.toml pins pallas-validate to.
pallas_pin() {
  sed -n 's/^pallas-validate = { git = "[^"]*", rev = "\([0-9a-f]*\)".*/\1/p' "$ROOT/$SCRATCH/Cargo.toml" | head -1
}

# Fetches a pallas commit and its history, from the cargo git database or else the repository pallas-validate is pinned to, into the scratch pallas repository.
pallas_history() {
  if [ ! -d "$ROOT/$PALLAS/.git" ]; then
    git init -q "$ROOT/$PALLAS" || return
  fi
  for db in "$ROOT"/cargo-home/git/db/pallas-*; do
    if git -C "$db" cat-file -e "$1^{commit}" 2>/dev/null; then
      git -C "$ROOT/$PALLAS" fetch -q "$db" '+refs/*:refs/db/*' || return
      break
    fi
  done
  if ! git -C "$ROOT/$PALLAS" cat-file -e "$1^{commit}" 2>/dev/null; then
    url=$(sed -n 's/^pallas-validate = { git = "\([^"]*\)".*/\1/p' "$ROOT/$SCRATCH/Cargo.toml" | head -1)
    git -C "$ROOT/$PALLAS" fetch -q "$url" "$1" || return
  fi
}

# Checks pallas out at a commit into the scratch pallas tree.
pallas_tree() {
  pallas_history "$1" || return
  git -C "$ROOT/$PALLAS" checkout -q -f --detach "$1" &&
    git -C "$ROOT/$PALLAS" clean -q -fdx
}

# Points the pallas crates of the Dolos tree $1 at the pallas tree $2 under the build root.
pallas_paths() {
  sed -E -i 's#^(pallas[a-z-]*) = \{ git = "https://github.com/[^/]+/pallas", rev = "[0-9a-f]+"#\1 = { path = "/w/'"$2"'/\1"#' \
    "$1/Cargo.toml"
}

# The manifest of fix commits, each with its arm or the reason it has none.
ARMS_MANIFEST=${ARMS_MANIFEST:-$HERE/arms.toml}

# The reasons arms.toml may give a fix commit no arm, a line each with the reason, why when its row must carry a why, and what it covers.
FIX_EXCLUSION_REASONS="
oracle - the oracle's own scripts, checked by lib_test.sh
pin - adopts pallas commits listed under pallas
lint - no behaviour change
tooling - scripts outside Dolos
relay - the relay to the node, which no judge drives
follower - chain following, epoch state and the mempool across blocks, judged by the live leg and not by an arm
network - node to node mini protocols, judged by the live leg
conway - Conway and earlier era paths, which Musashi transactions no longer reach
uncalled why a function no Dolos path calls, so no oracle input reaches it, its why naming the search that finds no caller
gap why an arm that plants the defect back and no judge catches, its why naming the miss
unrevertable why a revert that does not apply or build at the pin, its why naming the conflict"

# The reasons of FIX_EXCLUSION_REASONS.
FIX_EXCLUSIONS=$(echo "$FIX_EXCLUSION_REASONS" | awk 'NF { printf "%s ", $1 }')

# The reasons of FIX_EXCLUSION_REASONS whose row must also carry a why.
FIX_EXCLUSIONS_WITH_WHY=$(echo "$FIX_EXCLUSION_REASONS" | awk '$2 == "why" { printf "%s ", $1 }')

# The rows of the section $1 of the arms manifest, a line base=<commit> and then one line per fix commit, its commit, arm=<arm> or exclude=<reason>, and why when the row has one.
manifest_rows() {
  awk -v section="[$1]" '
    /^\[/ { inside = ($0 == section); next }
    !inside { next }
    /^base = / { gsub(/"/, "", $3); print "base=" $3 }
    /^  \{ commit = / {
      line = $0
      gsub(/[{},"]/, "", line)
      split(line, w, " ")
      print w[3], w[4] "=" w[6], (w[7] == "why" && w[9] != "" ? "why" : "")
    }' "$ARMS_MANIFEST"
}

# The catches the arms manifest, or the manifest file $2, gives the arm $1, separated by tabs.
catches() {
  awk -v arm="$1" '
    /^\[/ { inside = ($0 == "[catches]"); next }
    !inside || $1 != arm || $2 != "=" { next }
    {
      line = $0
      sub(/^[^[]*\[ *"/, "", line)
      sub(/" *\] *$/, "", line)
      gsub(/" *, *"/, "\t", line)
      print line
      exit
    }' "${2:-$ARMS_MANIFEST}"
}

# Prints a REFUSED line for each planted arm with no records file or no catch, and for each records file or catch that names no planted arm.
arm_mismatches() {
  for arm in $PLANTED_ARMS; do
    if [ ! -s "$HERE/plants/$arm.records" ]; then
      echo "REFUSED arm $arm has no plants/$arm.records"
    fi
    if [ -z "$(catches "$arm")" ]; then
      echo "REFUSED arm $arm has no catch in $ARMS_MANIFEST"
    fi
  done
  {
    for records in "$HERE"/plants/*.records; do
      if [ -f "$records" ]; then
        echo "plants/$(basename "$records")	$(basename "$records" .records)"
      fi
    done
    awk '/^\[/ { inside = ($0 == "[catches]"); next } inside && $2 == "=" { print "the catch of " $1 "\t" $1 }' "$ARMS_MANIFEST"
  } | while IFS='	' read -r source arm; do
    case " $PLANTED_ARMS " in
      *" $arm "*) ;;
      *) echo "REFUSED $source names no planted arm" ;;
    esac
  done
}

# Prints each line of the log $2 that a catch of the arm $1 selects, that names a record of the file $3 by its record field or, for a line about no one record, anywhere in it, and that the log $4 lacks.
verdict_catch() {
  specs=$(catches "$1")
  if [ -z "$specs" ]; then
    echo "ARM $1 NO_CATCH" >&2
    return 1
  fi
  awk -v specs="$specs" -v records="$3" -v head="$4" '
    function holds(want, cells, n, c) {
      n = length(want)
      for (c in cells) {
        if (substr(want, n) == "*" ? index(c, substr(want, 1, n - 1)) == 1 : c == want) return 1
      }
      return 0
    }
    function any(alternatives, cells, a, n, i) {
      n = split(alternatives, a, "|")
      for (i = 1; i <= n; i++) if (holds(a[i], cells)) return 1
      return 0
    }
    function one_of(alternatives, value, a, n, i) {
      n = split(alternatives, a, "|")
      for (i = 1; i <= n; i++) if (a[i] == value) return 1
      return 0
    }
    function selects(spec, line, w, terms, n, i, t, verdict, cells, list, k) {
      split(line, w, " ")
      verdict = w[1] == "VERDICT" && w[6] == "outcome=disagree"
      split("", cells)
      k = split(substr(w[5], 7), list, ",")
      for (i = 1; i <= k; i++) cells[list[i]] = 1
      n = split(spec, terms, " ")
      for (i = 1; i <= n; i++) {
        t = terms[i]
        if (t ~ /^test=/) {
          if (index(line, "thread \047" substr(t, 6) "\047 ") != 1 || !index(line, " panicked at ")) return 0
        } else if (!verdict) {
          return 0
        } else if (t ~ /^judge=/) {
          if (!one_of(substr(t, 7), substr(w[2], 7))) return 0
        } else if (t ~ /^entry=/) {
          if (!one_of(substr(t, 7), substr(w[3], 7))) return 0
        } else if (t ~ /^!/) {
          if (holds(substr(t, 2), cells)) return 0
        } else if (!any(t, cells)) {
          return 0
        }
      }
      return 1
    }
    function names_record(line, w, r) {
      split(line, w, " ")
      if (w[1] == "VERDICT" && w[4] != "record=selection") return (substr(w[4], 8) in ids)
      for (r in ids) if (index(" " line " ", " " r " ")) return 1
      return 0
    }
    BEGIN { nspecs = split(specs, spec, "\t") }
    FILENAME == records { if (NF) ids[$1] = 1; next }
    FILENAME == head { seen[$0] = 1; next }
    ($0 in seen) || !names_record($0) { next }
    {
      for (i = 1; i <= nspecs; i++) {
        if (selects(spec[i], $0)) {
          print
          next
        }
      }
    }' "$3" "$4" "$2"
}

# Prints a FIX-ARM line for each fix subject commit of the repository $1 from the base of the manifest section $2 to the commit $3 that the section gives no arm of PLANTED_ARMS and no known exclusion, then a FIX-ARMS line of counts, and fails when it printed a FIX-ARM line.
fix_arms() {
  rows=$(manifest_rows "$2")
  base=$(echo "$rows" | sed -n 's/^base=//p')
  if [ -z "$base" ] || ! git -C "$1" rev-parse -q --verify "$base^{commit}" >/dev/null; then
    echo "FIX-ARM repo=$2 base=${base:-none} not-found"
    echo "FIX-ARMS repo=$2 fixes=0 armed=0 excluded=0 unarmed=0 failing=1"
    return 1
  fi
  fixes=0 armed=0 excluded=0 unarmed=0 failing=0
  while read -r sha subject; do
    case $subject in fix*) ;; *) continue ;; esac
    fixes=$((fixes + 1))
    row=$(echo "$rows" | awk -v sha="$sha" 'index(sha, $1) == 1 && length($1) >= 7 { print $2; exit }')
    why=$(echo "$rows" | awk -v sha="$sha" 'index(sha, $1) == 1 && length($1) >= 7 { print $3; exit }')
    case $row in
      arm=*)
        if echo " $PLANTED_ARMS " | grep -q -F " ${row#arm=} "; then
          armed=$((armed + 1))
          continue
        fi
        ;;
      exclude=*)
        if echo " $FIX_EXCLUSIONS " | grep -q -F " ${row#exclude=} " &&
          { [ "$why" = why ] || ! echo " $FIX_EXCLUSIONS_WITH_WHY " | grep -q -F " ${row#exclude=} "; }; then
          excluded=$((excluded + 1))
          continue
        fi
        [ "$row" = exclude=unarmed ] && unarmed=$((unarmed + 1))
        ;;
    esac
    failing=$((failing + 1))
    echo "FIX-ARM repo=$2 commit=$(echo "$sha" | cut -c1-10) row=${row:-none} subject=\"$subject\""
  done <<EOF
$(git -C "$1" log --no-merges --format='%H %s' "$base..$3")
EOF
  echo "FIX-ARMS repo=$2 fixes=$fixes armed=$armed excluded=$excluded unarmed=$unarmed failing=$failing"
  [ "$failing" -eq 0 ]
}

# Prints the fix arm lines of the scratch tree and of the pallas commit it pins, and fails when either names a commit.
fix_arms_both() {
  pin=$(pallas_pin)
  dolos=0
  fix_arms "$ROOT/$SCRATCH" dolos HEAD || dolos=1
  if ! pallas_history "$pin"; then
    echo "FIX-ARM repo=pallas pin=${pin:-none} not-fetched"
    return 1
  fi
  fix_arms "$ROOT/$PALLAS" pallas "$pin" && [ "$dolos" -eq 0 ]
}

# Puts the clone $1 under the build root at the commit $3 of the repository $2 with no other change, cloning it first when it is absent, and writes only the files whose content differs from its last state.
clone_at() {
  if [ ! -d "$ROOT/$1/.git" ]; then
    git clone -q --no-checkout "$2" "$ROOT/$1" || return
  fi
  git -C "$ROOT/$1" fetch -q "$2" "$3" &&
    git -C "$ROOT/$1" checkout -q -f --detach FETCH_HEAD &&
    git -C "$ROOT/$1" clean -q -fdx
}

# Puts the scratch tree at REV of the bare repository $ROOT/dolos.git with no planted change.
reset_scratch() {
  clone_at "$SCRATCH" "$ROOT/dolos.git" "$REV"
}

# Runs the function $2 on each later argument, $1 at a time with the arguments dealt to $1 workers in turn, then prints each run's output in argument order.
pool() {
  pool_width=$1
  pool_fn=$2
  shift 2
  mkdir -p "$OUT/pool"
  pool_worker=0
  while [ "$pool_worker" -lt "$pool_width" ]; do
    (
      pool_i=0
      for pool_item; do
        if [ $((pool_i % pool_width)) -eq "$pool_worker" ]; then
          "$pool_fn" "$pool_item" >"$OUT/pool/$pool_item" 2>&1 || true
        fi
        pool_i=$((pool_i + 1))
      done
    ) &
    pool_worker=$((pool_worker + 1))
  done
  wait
  for pool_item; do
    cat "$OUT/pool/$pool_item"
  done
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
  # One line for each unit of the target dir, the tree ids of the scratch trees its last build read and its output files.
  BUILT="$ROOT/${TARGET:-target}/built-units"
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

# Runs a command with the trees and the target dir of the overlay build in place of the head's.
in_overlay() {
  SCRATCH=$OVERLAY_DOLOS PALLAS=$OVERLAY_PALLAS TARGET=$OVERLAY_TARGET "$@"
}

# Builds the test binary of the scratch tree's dolos library, cargo's messages in $1 and the rest of the arguments passed to cargo.
build_judge() {
  json=$1
  shift
  build_checked "$json" test --release -p dolos --lib --no-run "$@"
}

# The executable cargo built that a message file names, the test binary of a no run test build or the binary of a build, of the target $2 when it is given.
executable() {
  if [ -n "${2:-}" ]; then
    grep -F "\"name\":\"$2\"" "$1"
  else
    cat "$1"
  fi | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -1
}
