#!/bin/sh
# Checks that build_checked refuses a unit cargo reports fresh after a file it reads changed, and passes every other unit, with a runner that prints canned cargo messages, which arms an overlay switches, when two test logs differ, an encoding arm's verdict, the free space floor, the candidate's fsync state, the IO guard's breach rule, the records a passing tier writes, the box lock, the pallas pin, the scratch pallas checkout, when the value leg is due, when a value arm is caught, what the value leg hands the harness tier, the value control's verdict, the amaru pin and the amaru leg's verdicts.
# Usage: lib_test.sh <empty work dir>
set -eu
W=$(cd "$1" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
LIB=$HERE/lib.sh

# A fresh build root whose scratch tree holds a.rs, read by unit x, and b.rs, read by unit y.
setup() {
  ROOT=$W/$1
  rm -rf "$ROOT"
  mkdir -p "$ROOT/scratch/dolos" "$ROOT/target/release/deps"
  git -C "$ROOT/scratch/dolos" init -q
  echo 1 >"$ROOT/scratch/dolos/a.rs"
  echo 1 >"$ROOT/scratch/dolos/b.rs"
  commit
  for unit in x y; do
    file=$(echo "$unit" | tr xy ab).rs
    echo "/w/target/release/deps/lib$unit-1.rlib: /w/scratch/dolos/$file" >"$ROOT/target/release/deps/$unit-1.d"
  done
  printf 'cat "%s/messages"\n' "$ROOT" >"$ROOT/runner.sh"
}

commit() {
  git -C "$ROOT/scratch/dolos" add -A
  git -C "$ROOT/scratch/dolos" -c user.name=t -c user.email=t@t commit -q -m t
}

# Changes a.rs and commits it.
change_a() {
  echo 2 >"$ROOT/scratch/dolos/a.rs"
  commit
}

# Runs build_checked with messages naming each unit given as name=fresh, printing its exit and output.
build() {
  : >"$ROOT/messages"
  for arg in "$@"; do
    printf '{"reason":"compiler-artifact","filenames":["/w/target/release/deps/lib%s-1.rlib"],"fresh":%s}\n' \
      "${arg%=*}" "${arg#*=}" >>"$ROOT/messages"
  done
  (
    SCRATCH=scratch/dolos IN_IMAGE=$ROOT/runner.sh
    . "$LIB"
    status=0
    out=$(build_checked "$ROOT/build.json" build) || status=$?
    echo "exit=$status $out"
  )
}

failed=0
expect() {
  case $2 in
    "$3") echo "LIB-TEST $1 PASS" ;;
    *)
      echo "LIB-TEST $1 FAIL got=\"$2\" want=\"$3\""
      failed=1
      ;;
  esac
}

# Must not fire: a unit rebuilt after its file changed, and a fresh unit whose file did not change.
setup unchanged
expect cold "$(build x=false y=false)" "exit=0 "
change_a
expect rebuilt "$(build x=false y=true)" "exit=0 "
expect warm "$(build x=true y=true)" "exit=0 "

# Must fire: a unit reported fresh after its file changed.
setup changed
build x=false y=false >/dev/null
change_a
expect changed "$(build x=true y=true)" "exit=1 STALE unit=/w/target/release/deps/libx-1.rlib file=a.rs"

# Must fire: the same unit, when a build between the change and it did not build it.
setup skipped
build x=false y=false >/dev/null
change_a
expect between "$(build y=true)" "exit=0 "
expect skipped "$(build x=true y=true)" "exit=1 STALE unit=/w/target/release/deps/libx-1.rlib file=a.rs"

# An overlay whose added lines switch a in pallas and b in Dolos, with c only on a context line and f only on a removed one, and plants of their own for d in pallas and e in Dolos.
H=$W/arms
rm -rf "$H"
mkdir -p "$H/overlay" "$H/plants"
printf '%s\n' '+++ b/pallas-validate/src/x.rs' '@@ -1,2 +1,2 @@' ' oracle_plant("c")' '+oracle_plant("a")' '-oracle_plant("f")' \
  >"$H/overlay/pallas.diff"
printf '%s\n' '+++ b/src/one.rs' '@@ -1 +1,2 @@' ' fn one() {}' '+pallas::crypto::oracle_plant("b")' \
  '+++ b/src/two.rs' '@@ -1 +1,2 @@' ' fn two() {}' '+let other = 1;' >"$H/overlay/dolos.diff"
printf '%s\n' '+++ b/pallas-primitives/src/m.rs' >"$H/plants/d.diff"
printf '%s\n' '+++ b/crates/core/src/n.rs' >"$H/plants/e.diff"
arms() {
  (
    HERE=$H
    . "$LIB"
    for arm in a b c d e f; do
      overlay=n
      pallas=n
      if overlay_arm "$arm"; then overlay=y; fi
      if pallas_arm "$arm"; then pallas=y; fi
      echo "$arm:$overlay$pallas:$(plant_files "$arm" 2>/dev/null | paste -sd, -)"
    done | paste -sd ' ' -
  )
}
# Must fire for a and b, and must not for c, d, e and f, and b's quick tier files are the one file its switch is in.
expect arms "$(arms)" "a:yy:Cargo.toml,Cargo.lock b:yn:src/one.rs c:nn: d:ny:Cargo.toml,Cargo.lock e:nn:crates/core/src/n.rs f:nn:"
# Must name the overlay's switches and the arms with a plant of their own, and must not name c or f.
expect planted "$(
  HERE=$H
  . "$LIB"
  echo "$PLANTED_ARMS"
)" "a b d e"

# A manifest of catches, a records file for a, and a head log holding one line a's catch selects.
printf '%s\n' '[dolos]' 'base = "0"' '' '[catches]' \
  'a = ["judge=validation failure:disagree dolos:Cbor*", "judge=eval-oracle entry=estimate_tx_calls units:mem !units:steps"]' \
  'b = ["judge=endorser"]' 'c = ["test=c_bootstrap"]' >"$H/catches.toml"
printf '%s\n' 'ic#1' 'ic#2' >"$H/a.records"
printf '%s\n' 'slot=7' >"$H/b.records"
printf '%s\n' "'c_bootstrap'" >"$H/c.records"
v='VERDICT judge=validation entry=receive_tx'
e='VERDICT judge=eval-oracle entry=estimate_tx_calls'
printf '%s\n' "$v record=ic#2 cells=failure:disagree,dolos:CborDecodingError outcome=disagree in the head" >"$H/head.log"
printf '%s\n' \
  "$v record=ic#1 cells=failure:disagree,ledger:accepted,dolos:CborDecodingError outcome=disagree fires" \
  "$v record=ic#3 cells=failure:disagree,dolos:CborDecodingError outcome=disagree another record" \
  "$v record=ic#1 cells=failure:disagree,dolos:accepted outcome=disagree another answer" \
  "VERDICT judge=rules entry=receive_tx record=ic#1 cells=failure:disagree,dolos:CborDecodingError outcome=disagree another judge" \
  "$v record=ic#2 cells=failure:disagree,dolos:CborDecodingError outcome=disagree in the head" \
  "$e record=ic#2 cells=units:mem outcome=disagree mem alone" \
  "$e record=ic#2 cells=units:mem,units:steps outcome=disagree both units" \
  "VERDICT judge=eval-oracle entry=eval_tx:v1 record=ic#2 cells=units:mem outcome=disagree another layer" \
  "DISAGREE ic#1 ledger=accepted dolos=refused[CborDecodingError]" \
  'VERDICT judge=endorser entry=fetch_block:v1 record=selection cells= outcome=disagree ENDORSER-DIFF entry=fetch_block:v1 slot=7 list=ids index=0' \
  'VERDICT judge=endorser entry=fetch_block:v1 record=selection cells= outcome=disagree ENDORSER-DIFF entry=fetch_block:v1 slot=70 list=ids index=0' \
  "thread 'c_bootstrap' (9) panicked at x.rs:1:1:" "thread 'c_other' (9) panicked at x.rs:1:1:" >"$H/arm.log"
caught_by() {
  (
    HERE=$H ARMS_MANIFEST=$H/catches.toml
    . "$LIB"
    verdict_catch "$1" "$H/arm.log" "$H/$1.records" "$H/head.log" 2>&1 | sed 's/.* outcome=disagree //; s/ ([0-9]*) panicked.*//' | paste -sd '|' -
  )
}
# Must fire on each catch's own line, and must not fire on another record, answer, judge, entry point or test, both units, a line the head holds or a line not of the verdict type.
expect caught_cells "$(caught_by a)" "fires|mem alone"
expect caught_selection "$(caught_by b)" "ENDORSER-DIFF entry=fetch_block:v1 slot=7 list=ids index=0"
expect caught_test "$(caught_by c)" "thread 'c_bootstrap'"
expect caught_none "$(caught_by d)" "ARM d NO_CATCH"

# An arm set whose switches, records files and catches agree.
S=$W/armset
rm -rf "$S"
mkdir -p "$S/overlay" "$S/plants"
printf '%s\n' '+++ b/src/one.rs' '+oracle_plant("a")' '+oracle_plant("b")' >"$S/overlay/dolos.diff"
printf '%s\n' 'ic#1' >"$S/plants/a.records"
printf '%s\n' 'ic#2' >"$S/plants/b.records"
printf '%s\n' '[catches]' 'a = ["judge=validation"]' 'b = ["judge=utxo"]' >"$S/arms.toml"
mismatches() {
  (
    HERE=$S ARMS_MANIFEST=$S/arms.toml
    . "$LIB"
    arm_mismatches | paste -sd '|' -
  )
}
# Must not fire when every arm has its switch, records and catch.
expect arm_set_agrees "$(mismatches)" ""
printf '%s\n' '+++ b/src/one.rs' '+oracle_plant("a")' >"$S/overlay/dolos.diff"
# Must fire on the records file and the catch of an arm whose switch is gone.
expect arm_switch_removed "$(mismatches)" "REFUSED plants/b.records names no planted arm|REFUSED the catch of b names no planted arm"

# A scratch tree whose second commit adds src/one.rs, the file b's switch is in.
T=$W/touched
rm -rf "$T"
mkdir -p "$T/scratch/dolos/src"
git -C "$T/scratch/dolos" init -q
echo 1 >"$T/scratch/dolos/a.rs"
git -C "$T/scratch/dolos" add -A
git -C "$T/scratch/dolos" -c user.name=t -c user.email=t@t commit -q -m t
first=$(git -C "$T/scratch/dolos" rev-parse HEAD)
echo 1 >"$T/scratch/dolos/src/one.rs"
git -C "$T/scratch/dolos" add -A
git -C "$T/scratch/dolos" -c user.name=t -c user.email=t@t commit -q -m t
touched() {
  (
    ROOT=$T OUT=$T HERE=$H
    . "$LIB"
    if touched_arms "$1" HEAD >"$T/arms" 2>/dev/null; then
      echo "exit=0 $(paste -sd ' ' - <"$T/arms")"
    else
      echo "exit=$?"
    fi
  )
}
# Must name b for its changed file, and must fail rather than name no arm when the files between the commits cannot be listed.
expect touched_changed "$(touched "$first")" "exit=0 b"
expect touched_unlisted "$(touched 0000000000000000000000000000000000000000)" "exit=128"

# Arms d, e, g and h with a plant of their own, cost-models with its crate test and k with a deselection, and a scratch tree whose second commit changes d's catch, e's records, the cost-models test source, k's deselection and the place of h's catch.
A=$W/armfiles
rm -rf "$A"
mkdir -p "$A/box/plants" "$A/scratch/dolos/oracle/box/plants" "$A/scratch/dolos/crates/cardano/tests"
for arm in d e g h cost-models; do
  printf '%s\n' '+++ b/crates/core/src/n.rs' >"$A/box/plants/$arm.diff"
done
echo 'ic#1' >"$A/box/plants/k.deselect"
echo 'ic#1' >"$A/scratch/dolos/oracle/box/plants/k.deselect"
manifest() {
  printf '%s\n' '[catches]' "$@" >"$A/scratch/dolos/oracle/box/arms.toml"
}
manifest 'd = ["judge=validation"]' 'e = ["judge=utxo"]' 'g = ["judge=rules"]' 'h = ["judge=cost"]' 'cost-models = ["test=t"]'
echo 'ic#1' >"$A/scratch/dolos/oracle/box/plants/e.records"
echo 'fn t() {}' >"$A/scratch/dolos/crates/cardano/tests/musashi_cost_models.rs"
git -C "$A/scratch/dolos" init -q
git -C "$A/scratch/dolos" add -A
git -C "$A/scratch/dolos" -c user.name=t -c user.email=t@t commit -q -m t
before=$(git -C "$A/scratch/dolos" rev-parse HEAD)
manifest 'h = ["judge=cost"]' 'd = ["judge=validation failure:disagree"]' 'e = ["judge=utxo"]' 'g = ["judge=rules"]' 'cost-models = ["test=t"]'
echo 'ic#2' >"$A/scratch/dolos/oracle/box/plants/e.records"
echo 'fn t() { assert!(true) }' >"$A/scratch/dolos/crates/cardano/tests/musashi_cost_models.rs"
echo 'ic#2' >"$A/scratch/dolos/oracle/box/plants/k.deselect"
git -C "$A/scratch/dolos" add -A
git -C "$A/scratch/dolos" -c user.name=t -c user.email=t@t commit -q -m t
# Must fire for the changed catch of d, the records of e, the test source of cost-models and the deselection of k, and must not for g, unchanged, or h, whose catch only moved.
expect touched_arm_files "$(
  ROOT=$A OUT=$A HERE=$A/box
  . "$LIB"
  touched_arms "$before" HEAD | paste -sd ' ' -
)" "cost-models d e k"
# Must name k a deselect arm with no plant files, and must not name d one.
expect deselect_arm "$(
  HERE=$A/box
  . "$LIB"
  echo "$PLANTED_ARMS|$(deselect_arm k && echo k)|$(deselect_arm d || echo not-d)|$(plant_files k)"
)" "cost-models d e g h k|k|not-d|"

# A head control log, the same log with another run time, and the same log with one record judged differently.
L=$W/logs
rm -rf "$L"
mkdir -p "$L"
printf '%s\n' 'AGREE tx=aa' 'AGREE tx=bb' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.88s' >"$L/head"
printf '%s\n' 'AGREE tx=aa' 'AGREE tx=bb' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.29s' >"$L/retimed"
printf '%s\n' 'AGREE tx=aa' 'DISAGREE tx=bb' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.88s' >"$L/differs"
same() {
  (
    HERE=$H
    . "$LIB"
    if same_log "$1" "$2"; then echo same; else echo differs; fi
  )
}
# Must not fire on a run time alone, and must fire on one record.
expect retimed "$(same "$L/head" "$L/retimed")" same
expect differs "$(same "$L/head" "$L/differs")" differs

# Crate test logs of the watch arm: every version passing, its v1beta test failing, and only the v1 test run.
watch=watch::tests::the_transactions_of_one_block_share_its_bytes
printf '%s\n' "test serve::grpc::v1beta::$watch ... ok" "test serve::grpc::v1alpha::$watch ... ok" "test serve::grpc::v1::$watch ... ok" \
  'test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 386 filtered out; finished in 0.02s' >"$L/watch-all"
printf '%s\n' "test serve::grpc::v1beta::$watch ... FAILED" "test serve::grpc::v1alpha::$watch ... ok" \
  'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 387 filtered out; finished in 0.02s' >"$L/watch-failed"
printf '%s\n' "test serve::grpc::v1::$watch ... ok" \
  'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 388 filtered out; finished in 0.02s' >"$L/watch-v1"
printf '%s\n' 'test serve::grpc::v1::ledger_oracle_tests::endorser::served_endorser_transactions_are_the_nodes ... ok' \
  'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 388 filtered out; finished in 0.31s' >"$L/endorser"
printf '%s\n' 'test musashi_era_translation_keeps_the_published_v3_cost_model ... ok' \
  'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s' >"$L/cost-models"
passed() {
  (
    HERE=$H
    . "$LIB"
    if crate_test_passed "$1" "$2"; then echo passed; else echo failed; fi
  )
}
# Must not fire when every test passes, and must fire on a failing test and on a planted test that did not run.
expect crate_passed "$(passed watch-block-copied "$L/watch-all")" passed
expect crate_endorser "$(passed endorser-tx-dropped "$L/endorser")" passed
expect crate_cost_models "$(passed cost-models "$L/cost-models")" passed
expect crate_failed "$(passed watch-block-copied "$L/watch-failed")" failed
expect crate_unrun "$(passed watch-block-copied "$L/watch-v1")" failed

# Two runs of the watch arm's crate test, the second printing its tests in the other order at another run time.
printf '%s\n' "test serve::grpc::v1beta::$watch ... ok" "test serve::grpc::v1alpha::$watch ... ok" \
  'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 388 filtered out; finished in 0.04s' >"$L/watch-two"
printf '%s\n' "test serve::grpc::v1alpha::$watch ... ok" "test serve::grpc::v1beta::$watch ... ok" \
  'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 388 filtered out; finished in 0.00s' >"$L/watch-swapped"
same_crate() {
  (
    HERE=$H
    . "$LIB"
    if same_crate_log "$1" "$2"; then echo same; else echo differs; fi
  )
}
# Must not fire on the order the tests ended in, and must fire on a failing test.
expect crate_log_swapped "$(same_crate "$L/watch-two" "$L/watch-swapped")" same
expect crate_log_failed "$(same_crate "$L/watch-two" "$L/watch-failed")" differs

# An unplanted log and an encoding arm's logs: one with only bytes_equal moved, one with a context also judged differently, and one with nothing moved.
context='CONTEXT compared=2 agree=2 differ=0 known=0 unbuilt=0 no_ledger_context=0 pallas_only=0'
printf '%s\n' "$context bytes_equal=0 by_language=V3=2/0/0 by_level=top=2/0/0" 'RESULT PASS' >"$L/unplanted"
printf '%s\n' 'ORACLE_PLANT reached e' "$context bytes_equal=2 by_language=V3=2/0/0 by_level=top=2/0/0" 'RESULT PASS' >"$L/rewritten"
printf '%s\n' 'ORACLE_PLANT reached e' 'CONTEXT compared=2 agree=1 differ=1 known=0 unbuilt=0 no_ledger_context=0 pallas_only=0 bytes_equal=1 by_language=V3=1/0/1 by_level=top=1/0/1' \
  'CONTEXT-DIFF ic#1 top Spend#0 V3 arg=0 path=0.3 field=context.scriptContextTxInfo.txInfoFee ledger=7 pallas=8' 'RESULT FAIL' >"$L/revalued"
printf '%s\n' 'ORACLE_PLANT reached e' "$context bytes_equal=0 by_language=V3=2/0/0 by_level=top=2/0/0" 'RESULT PASS' >"$L/unmoved"
# A failing unplanted log and a planted one whose panic message the test harness wrote among the oracle's lines at another place.
panic="thread 'ledger_oracle_records' panicked at mod.rs:1:5:"
printf '%s\n' "$context bytes_equal=0 by_language=V3=2/0/0 by_level=top=2/0/0" 'RESULT FAIL' "$panic" >"$L/unplanted-failing"
printf '%s\n' 'ORACLE_PLANT reached e' "$panic" "$context bytes_equal=2 by_language=V3=2/0/0 by_level=top=2/0/0" 'RESULT FAIL' >"$L/interleaved"
encoding() {
  (
    HERE=$H
    . "$LIB"
    encoding_verdict "$1" "${2:-$L/unplanted}"
  )
}
# Must hold when only the bytes count moved, also when the harness interleaved the panic elsewhere, fire on a changed value, and miss when the plant changed no bytes.
expect encoding_held "$(encoding "$L/rewritten")" HELD
expect encoding_interleaved "$(encoding "$L/interleaved" "$L/unplanted-failing")" HELD
expect encoding_fired "$(encoding "$L/revalued")" FIRED
expect encoding_missed "$(encoding "$L/unmoved")" MISSED

# The free gibibytes of a filesystem holding $1 bytes free and require_free's verdict with a floor of $2, with a df that answers -B1 in bytes and -BG in gibibytes rounded up.
free_of() {
  (
    ROOT=$W/free MIN_FREE_G=$2
    HERE=$H
    . "$LIB"
    df() {
      echo Avail
      case $1 in
        -B1) echo "$DF_BYTES" ;;
        -BG) echo "$(((DF_BYTES + 1073741823) / 1073741824))G" ;;
      esac
    }
    DF_BYTES=$1
    status=0
    out=$(require_free) || status=$?
    echo "free=$(free_g /) exit=$status $out"
  )
}
# Must not fire: exactly 10 gibibytes free passes a 10 gibibyte floor.
expect free_whole "$(free_of 10737418240 10)" "free=10 exit=0 "
# Must fire: one byte short of 10 gibibytes is 9, which a 10 gibibyte floor refuses.
expect free_short "$(free_of 10737418239 10)" "free=9 exit=2 REFUSED free=9G below 10G at $W/free"

# Maps of a process with libeatmydata preloaded, of one without it, of one whose loader has mapped only itself, of one that has exited, and none at all.
M=$W/maps
rm -rf "$M"
mkdir -p "$M"
printf '%s\n' '7f00 r-xp 00000000 08:01 1 /usr/lib/x86_64-linux-gnu/libc.so.6' '7f01 r-xp 00000000 08:01 2 /opt/eatmydata.so' >"$M/preloaded"
printf '%s\n' '7f00 r-xp 00000000 08:01 1 /usr/lib/x86_64-linux-gnu/libc.so.6' >"$M/plain"
printf '%s\n' '7f02 r-xp 00000000 08:01 3 /usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2' >"$M/loading"
: >"$M/exited"
fsync_of() {
  (
    HERE=$H
    . "$LIB"
    fsync_state "$1"
  )
}
# Must not fire: a process whose maps name libc but not libeatmydata.
expect fsync_on "$(fsync_of "$M/plain")" fsync=on
# Must fire: a process whose maps name libeatmydata.
expect fsync_off "$(fsync_of "$M/preloaded")" fsync=off
# Neither: maps that name only the loader, none, or cannot be read.
expect fsync_loading "$(fsync_of "$M/loading")" fsync=unknown
expect fsync_exited "$(fsync_of "$M/exited")" fsync=unknown
expect fsync_missing "$(fsync_of "$M/missing")" fsync=unknown

# The fsync state of a running process, with the library the candidate preloads in $1 or none.
process_fsync() {
  if [ -n "$1" ]; then
    LD_PRELOAD=$1 sleep 30 &
  else
    sleep 30 &
  fi
  pid=$!
  sleep 1
  fsync_of "/proc/$pid/maps"
  kill "$pid"
}
expect fsync_process_on "$(process_fsync '')" fsync=on
expect fsync_process_off "$(process_fsync /usr/lib/x86_64-linux-gnu/libeatmydata.so.1)" fsync=off

# The fsync mode CANDIDATE_FSYNC=$1 asks for, with the exit of candidate_fsync.
mode_of() {
  (
    HERE=$H
    . "$LIB"
    status=0
    out=$(CANDIDATE_FSYNC=$1 candidate_fsync 2>&1) || status=$?
    echo "exit=$status $out"
  )
}
# Must not fire: unset and the two named modes.
expect mode_unset "$(mode_of '')" "exit=0 off"
expect mode_on "$(mode_of on)" "exit=0 on"
expect mode_off "$(mode_of off)" "exit=0 off"
# Must fire: any other value.
expect mode_other "$(mode_of none)" "exit=2 LIVE-CANDIDATE REFUSED candidate_fsync=none want=off,on"

# The streak after one LIVE-IO sample line $2 following a streak of $1, with limits of 60 s of node lag, 40 slots of serve lag and 2000 ms.
streak_of() {
  (
    HERE=$H
    . "$LIB"
    IO_NODE_LAG_S=60 IO_SERVE_LAG_SLOTS=40 IO_SERVE_MS=2000
    io_streak "$1" "$2"
  )
}
# Must not fire: every value at its limit, and a serve tip ahead of the node tip read before it.
expect io_at_limit "$(streak_of 2 'LIVE-IO node_lag_s=60 serve_lag_slots=40 serve_ms=2000')" 0
expect io_serve_ahead "$(streak_of 0 'LIVE-IO node_lag_s=5 serve_lag_slots=-20 serve_ms=80')" 0
# Must fire: each value one over its limit, a failed read and a missing value.
expect io_node_over "$(streak_of 0 'LIVE-IO node_lag_s=61 serve_lag_slots=0 serve_ms=80')" 1
expect io_serve_over "$(streak_of 1 'LIVE-IO node_lag_s=5 serve_lag_slots=41 serve_ms=80')" 2
expect io_ms_over "$(streak_of 0 'LIVE-IO node_lag_s=5 serve_lag_slots=0 serve_ms=2001')" 1
expect io_failed "$(streak_of 0 'LIVE-IO node_lag_s=5 serve_lag_slots=failed serve_ms=failed')" 1
expect io_missing "$(streak_of 0 'LIVE-IO node_lag_s=5 serve_lag_slots=0')" 1

# Folds LIVE-IO sample lines through the guard with the limits of streak_of, printing the sample that breaches or the streak after the last.
guard_of() {
  (
    HERE=$H
    . "$LIB"
    IO_NODE_LAG_S=60 IO_SERVE_LAG_SLOTS=40 IO_SERVE_MS=2000
    streak=0
    n=0
    for line in "$@"; do
      n=$((n + 1))
      streak=$(io_streak "$streak" "$line")
      if io_breached "$streak"; then
        echo "breach sample=$n"
        return
      fi
    done
    echo "clear streak=$streak"
  )
}
under='LIVE-IO node_lag_s=20 serve_lag_slots=0 serve_ms=80'
over='LIVE-IO node_lag_s=20 serve_lag_slots=0 serve_ms=2500'
# Must not fire: two samples over then one under.
expect io_recovers "$(guard_of "$over" "$over" "$under")" "clear streak=0"
# Must fire: three samples over in a row.
expect io_breach "$(guard_of "$under" "$over" "$over" "$over")" "breach sample=4"

# The exit of record_pass for a run of the tier $1 of commit c with the verdicts $2, and what it leaves in last-done and in last-ship but for the time.
records_of() {
  (
    HERE=$H
    . "$LIB"
    R=$W/records
    rm -rf "$R"
    mkdir -p "$R"
    LAST_DONE=$R/last-done LAST_SHIP=$R/last-ship
    status=0
    record_pass "$1" c "$2" || status=$?
    last_done=absent
    last_ship=absent
    if [ -f "$LAST_DONE" ]; then last_done=$(cat "$LAST_DONE"); fi
    if [ -f "$LAST_SHIP" ]; then last_ship=$(sed 's/ time=[0-9TZ:-]*$//' "$LAST_SHIP"); fi
    echo "exit=$status last-done=$last_done last-ship=$last_ship"
  )
}
arms_pass='head=PASS control=CLEAN colret=CAUGHT'
# Must not fire: a done run that passes writes last-done alone, and a ship or live run whose live leg fails writes neither.
expect ship_by_done "$(records_of done "$arms_pass")" "exit=0 last-done=c last-ship=absent"
# Must not fire: every passing word a done run prints. Must fire: a verdict word the pass rule does not name.
expect done_every_passing_word "$(records_of done "$arms_pass ctx=HELD fix-arms=PASS value=passed-on-these-inputs")" \
  "exit=0 last-done=c last-ship=absent"
expect done_unnamed_word "$(records_of done "$arms_pass coin=UNREACHED")" "exit=1 last-done=absent last-ship=absent"
expect ship_live_failed "$(records_of ship "$arms_pass live-shift=CAUGHT live=FAIL")" "exit=1 last-done=absent last-ship=absent"
expect ship_shift_missed "$(records_of ship "$arms_pass live-shift=MISSED live=PASS")" "exit=1 last-done=absent last-ship=absent"
expect live_failed "$(records_of live 'live=FAIL live-shift=CAUGHT')" "exit=1 last-done=absent last-ship=absent"
expect live_shift_missed "$(records_of live 'live=PASS live-shift=MISSED')" "exit=1 last-done=absent last-ship=absent"
# Must fire: a ship run that passes writes both, and a live run that passes writes last-ship alone.
expect ship_passed "$(records_of ship "$arms_pass live-shift=CAUGHT live=PASS")" \
  "exit=0 last-done=c last-ship=commit=c tier=ship verdicts=\"$arms_pass live-shift=CAUGHT live=PASS\""
expect live_passed "$(records_of live 'live=PASS live-shift=CAUGHT')" \
  "exit=0 last-done=absent last-ship=commit=c tier=live verdicts=\"live=PASS live-shift=CAUGHT\""

# Prints lock=free, lock=held when flock reports the lock taken, or the exit of any other flock failure, for the lock RUN_LOCK.
LOCK_PROBE='status=0; flock -n -E 75 "$RUN_LOCK" true || status=$?; case $status in 0) state=free ;; 75) state=held ;; *) state=flock_exit_$status ;; esac'

# Runs the script run.sh, or $3, with the argument done, or $4, on the build root $1 and the lock $1/run.lock, JUDGED set unless $2 is entry, a floor no disk meets so a run past the lock refuses at once, its output and exit in $1/run.out, and a df that writes to $1/df.seen whether a process held the lock when the run read free space.
tier() {
  mkdir -p "$1/bin"
  printf '%s\n' '#!/bin/sh' "$LOCK_PROBE" 'echo "lock=$state" >>"$ROOT/df.seen"' \
    "exec $(command -v df) \"\$@\"" >"$1/bin/df"
  chmod +x "$1/bin/df"
  (
    ROOT=$1 OUT=$1/runs/t MIN_FREE_G=999999 LOCK_POLL_SECONDS=1 RUN_LOCK=$1/run.lock PATH=$1/bin:$PATH
    export ROOT OUT MIN_FREE_G LOCK_POLL_SECONDS RUN_LOCK PATH
    unset JUDGED
    if [ "${2:-}" != entry ]; then
      JUDGED=t
      export JUDGED
    fi
    status=0
    timeout 15 sh "$HERE/${3:-run.sh}" "${4:-done}" >"$1/run.out" 2>&1 || status=$?
    echo "exit=$status" >>"$1/run.out"
  )
}

# The output of a tier run on the build root $1 on one line, its free space refusal shortened.
run_out() {
  sed 's/^REFUSED free=[0-9]*G below 999999G at .*/REFUSED free/' "$1/run.out" | paste -sd ' ' -
}

# Whether the lock file of the build root $1 still records a pid, is empty, or is gone.
record_left() {
  if [ -s "$1/run.lock" ]; then
    echo left
  elif [ -e "$1/run.lock" ]; then
    echo released
  else
    echo removed
  fi
}

# A fresh build root for a lock case.
lock_root() {
  K=$W/$1
  rm -rf "$K"
  mkdir -p "$K"
}

# Holds the lock $1 in the background, recording its pid there, until $1.go exists, then empties the record and exits.
hold_lock() {
  sh -c 'exec 9>>"$1"; flock -n 9 || exit 1; echo $$ >"$1"; until [ -e "$1.go" ]; do sleep 0.2; done; : >"$1"' sh "$1" &
  holder=$!
  until [ -s "$1" ] || ! kill -0 "$holder" 2>/dev/null; do sleep 0.1; done
}

# Waits up to 10 s for the tier run on the build root $1 to print WAITING or exit.
await_waiting() {
  i=0
  until grep -q '^WAITING' "$1/run.out" 2>/dev/null || ! kill -0 "$runner" 2>/dev/null || [ "$i" -ge 10 ]; do
    sleep 1
    i=$((i + 1))
  done
}

# Must not fire: with no lock the run goes on to the floor holding the lock, and empties its record.
lock_root lock-free
tier "$K"
expect lock_free "$(run_out "$K")" "REFUSED free run-exit=2 exit=2"
expect lock_free_held "$(paste -sd ' ' - <"$K/df.seen")" lock=held
expect lock_free_released "$(record_left "$K")" released

# Must fire: a lock a running process holds makes the run wait, and the run goes on once the holder releases it.
lock_root lock-held
hold_lock "$K/run.lock"
tier "$K" &
runner=$!
await_waiting "$K"
expect lock_waits "$(paste -sd ' ' - <"$K/run.out")" "WAITING lock_pid=$holder"
touch "$K/run.lock.go"
wait "$holder" || true
wait "$runner" || true
expect lock_goes_on "$(run_out "$K")" "WAITING lock_pid=$holder REFUSED free run-exit=2 exit=2"
expect lock_taken "$(paste -sd ' ' - <"$K/df.seen")" lock=held
expect lock_held_released "$(record_left "$K")" released

# Must fire: a holder killed while the run waits leaves its record, and the run reports it and goes on.
lock_root lock-killed
hold_lock "$K/run.lock"
tier "$K" &
runner=$!
await_waiting "$K"
kill -9 "$holder"
wait "$holder" 2>/dev/null || true
wait "$runner" || true
expect lock_holder_killed "$(run_out "$K")" "WAITING lock_pid=$holder LOCK stale pid=$holder removed REFUSED free run-exit=2 exit=2"
expect lock_killed_held "$(paste -sd ' ' - <"$K/df.seen")" lock=held

# Must fire: a record of a process that has exited is reported, and the run takes the lock without waiting.
lock_root lock-stale
sh -c 'exit 0' &
dead=$!
wait "$dead"
echo "$dead" >"$K/run.lock"
tier "$K"
expect lock_stale "$(run_out "$K")" "LOCK stale pid=$dead removed REFUSED free run-exit=2 exit=2"
expect lock_stale_held "$(paste -sd ' ' - <"$K/df.seen")" lock=held
expect lock_stale_released "$(record_left "$K")" released

# Must fire: a record naming a running process that holds no lock, as a pid reused after a reboot does, is reported, and the run takes the lock without waiting.
lock_root lock-reused
sleep 60 &
reused=$!
echo "$reused" >"$K/run.lock"
tier "$K"
kill "$reused"
expect lock_reused "$(run_out "$K")" "LOCK stale pid=$reused removed REFUSED free run-exit=2 exit=2"
expect lock_reused_held "$(paste -sd ' ' - <"$K/df.seen")" lock=held

# Must fire: the entry run.sh holds the lock while the run.sh of a judged commit older than the lock runs, which here records whether the lock is held and its own pid.
lock_root lock-old
mkdir -p "$K/old/oracle/box"
printf '%s\n' '#!/bin/sh' "$LOCK_PROBE" 'echo "lock=$state pid=$$" >"$ROOT/old.seen"' >"$K/old/oracle/box/run.sh"
git -C "$K/old" init -q
git -C "$K/old" add -A
git -C "$K/old" -c user.name=t -c user.email=t@t commit -q -m old
git clone -q --bare "$K/old" "$K/dolos.git"
(
  REV=$(git -C "$K/dolos.git" rev-parse HEAD)
  export REV
  tier "$K" entry
)
expect lock_old "$(cat "$K/old.seen")" "lock=held pid=$(cat "$K/run.lock")"
# Must not fire: the entry run.sh prints no exit line of its own when it hands the run to the judged commit's run.sh.
expect lock_old_exit "$(run_out "$K")" "exit=0"

# Must fire: breaks.sh and candidate.sh, each run alone, hold the lock when they read free space, candidate.sh with a config to copy.
lock_root lock-breaks
tier "$K" judged breaks.sh
expect lock_breaks "$(run_out "$K")" "REFUSED free exit=2"
expect lock_breaks_held "$(paste -sd ' ' - <"$K/df.seen")" lock=held
lock_root lock-candidate
mkdir -p "$K/config" "$K/dir"
for file in dolos.toml byron-genesis.json shelley-genesis.json alonzo-genesis.json conway-genesis.json dijkstra-genesis.json; do
  : >"$K/config/$file"
done
(
  CANDIDATE=c COMMIT=c NODE=n ENDPOINT=e SERVE_CONFIG=$K/config CANDIDATE_GRPC=g CANDIDATE_NEED_G=999999
  export CANDIDATE COMMIT NODE ENDPOINT SERVE_CONFIG CANDIDATE_GRPC CANDIDATE_NEED_G
  tier "$K" judged candidate.sh "$K/dir"
)
expect lock_candidate_held "$(sed -n 1p "$K/df.seen")" lock=held
expect lock_candidate_released "$(record_left "$K")" released

# A build root whose scratch Cargo.toml holds the lines after $1, and the pin pallas_pin reads from it.
pin_of() {
  V=$W/pin-$1
  rm -rf "$V"
  mkdir -p "$V/scratch/dolos"
  shift
  printf '%s\n' "$@" >"$V/scratch/dolos/Cargo.toml"
  (
    ROOT=$V SCRATCH=scratch/dolos
    . "$LIB"
    echo "pin=$(pallas_pin)"
  )
}
# Must not read another pallas crate's rev or a path entry, and must read the pallas-validate rev.
expect pin_other "$(pin_of other 'pallas-traverse = { git = "https://github.com/a/pallas", rev = "111" }' 'pallas-validate = { path = "../pallas/pallas-validate" }')" "pin="
expect pin_validate "$(pin_of validate 'pallas-traverse = { git = "https://github.com/a/pallas", rev = "111" }' 'pallas-validate = { git = "https://github.com/a/pallas", rev = "222", features = ["x"] }')" "pin=222"

# A build root with no cargo git database whose scratch Cargo.toml pins pallas-validate to the commit $2 of the repository $1, and the scratch pallas head after pallas_tree.
tree_of() {
  T=$W/tree
  rm -rf "$T"
  mkdir -p "$T/scratch/dolos"
  echo "pallas-validate = { git = \"$1\", rev = \"$2\" }" >"$T/scratch/dolos/Cargo.toml"
  (
    ROOT=$T SCRATCH=scratch/dolos PALLAS=scratch/pallas
    . "$LIB"
    if pallas_tree "$2" >/dev/null 2>&1; then git -C "$T/scratch/pallas" rev-parse HEAD; else echo refused; fi
  )
}
P=$W/tree-src
rm -rf "$P"
git init -q "$P"
git -C "$P" -c user.name=t -c user.email=t@t commit -q --allow-empty -m t
src=$(git -C "$P" rev-parse HEAD)
# Must not check out a commit the pinned repository lacks, and must fetch the pinned commit from that repository.
expect tree_absent "$(tree_of "$P" 0000000000000000000000000000000000000001)" refused
expect tree_fetch "$(tree_of "$P" "$src")" "$src"

# A box dir with the value leg's files, and the verdicts of value_due before a pass, after one, after a changed pin and after a changed plant.
V=$W/value
rm -rf "$V"
mkdir -p "$V/box/value" "$V/box/overlay" "$V/box/amaru" "$V/runs"
cp "$HERE/value.sh" "$HERE/amaru.sh" "$V/box/"
echo r1 >"$V/box/value/harness.rev"
echo p1 >"$V/box/value/pallas.diff"
echo o1 >"$V/box/overlay/pallas.diff"
echo a1 >"$V/box/amaru/a.diff"
due() {
  (
    ROOT=$V HERE=$V/box
    . "$V/box/value.sh"
    if value_due "$1"; then echo due; else echo passed; fi
  )
}
record_value() {
  (
    ROOT=$V HERE=$V/box
    . "$V/box/value.sh"
    value_inputs "$1" >"$LAST_VALUE"
  )
}
# Must fire with no passing leg, must not on the inputs that passed, and must fire on another pin, another value plant or another amaru plant.
expect value_due_none "$(due aaa)" due
record_value aaa
expect value_due_same "$(due aaa)" passed
expect value_due_pin "$(due bbb)" due
echo p2 >"$V/box/value/pallas.diff"
expect value_due_plant "$(due aaa)" due
record_value aaa
echo a2 >"$V/box/amaru/a.diff"
expect value_due_amaru_plant "$(due aaa)" due

# An arm's run dir with the reached line $2 and the disagreement class $3, and value_caught's verdict for the arm a with the class c.
caught_of() {
  C=$W/caught-$1
  rm -rf "$C"
  mkdir -p "$C"
  echo "$2" >"$C/tx.given.reached"
  printf '{"cls": "%s", "key": "k"}\n' "$3" >"$C/tx.given.schema.dis.jsonl"
  (
    ROOT=$W HERE=$HERE
    . "$HERE/value.sh"
    if value_caught "$C" c a; then echo caught; else echo missed; fi
  )
}
# Must not fire with the plant unreached, another plant reached or another class, and must fire with both.
expect value_caught_unreached "$(caught_of unreached '' c)" missed
expect value_caught_other_plant "$(caught_of other_plant 'ORACLE_PLANT reached ab' c)" missed
expect value_caught_other_class "$(caught_of other_class 'ORACLE_PLANT reached a' cc)" missed
expect value_caught_both "$(caught_of both 'ORACLE_PLANT reached a' c)" caught

# A pallas tree whose pallas-validate Cargo.toml names an amaru-uplc entry, and the url and commit amaru_pin reads at its head.
A=$W/amaru-pin
rm -rf "$A"
mkdir -p "$A/scratch/pallas/pallas-validate"
git -C "$A/scratch/pallas" init -q
printf '%s\n' 'amaru-uplc = { version = "=0.1.0", optional = true }' \
  'amaru-uplc-native = { package = "amaru-uplc", git = "https://example.org/amaru.git", rev = "abc1", optional = true }' \
  'amaru-kernel = { git = "https://example.org/other.git", rev = "def2", optional = true }' >"$A/scratch/pallas/pallas-validate/Cargo.toml"
git -C "$A/scratch/pallas" add -A
git -C "$A/scratch/pallas" -c user.name=t -c user.email=t@t commit -q -m t
# Must not read the crates.io entry or another crate's commit, and must read the amaru-uplc commit.
expect amaru_pin "$(
  ROOT=$A PALLAS=scratch/pallas HERE=$HERE
  . "$HERE/amaru.sh"
  amaru_pin HEAD
)" "https://example.org/amaru.git abc1"

# The verdict of amaru_passed on a test log whose result line is $2, with $1 generated tests.
amaru_passed_of() {
  (
    ROOT=$W HERE=$HERE
    . "$HERE/amaru.sh"
    printf '%s\n' "$2" | sed -n 's/^\(test result: .* filtered out; finished in\) [0-9.]*s$/\1/p' >"$W/amaru.log.result"
    if amaru_passed "$W/amaru.log" "$1"; then echo pass; else echo fail; fi
  )
}
# Must not pass with a test short, a failed test, a filtered test or no generated test, and must pass with every test passed.
expect amaru_short "$(amaru_passed_of 1065 'test result: ok. 1064 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.20s')" fail
expect amaru_failed "$(amaru_passed_of 1065 'test result: FAILED. 1064 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.20s')" fail
expect amaru_filtered "$(amaru_passed_of 1065 'test result: ok. 1065 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 1.20s')" fail
expect amaru_none "$(amaru_passed_of 0 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s')" fail
expect amaru_all "$(amaru_passed_of 1065 'test result: ok. 1065 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.20s')" pass

# The count amaru_generated gives for a target holding a generated source of $2 tests at the build path $1 under debug/build, or none when $1 is empty.
amaru_generated_of() {
  (
    ROOT=$W/gen-count HERE=$HERE
    rm -rf "$ROOT"
    mkdir -p "$ROOT/t/debug/build/amaru-kernel-1/out"
    printf '#[test]\nfn k() {}\n' >"$ROOT/t/debug/build/amaru-kernel-1/out/generated_tests.rs"
    if [ -n "$1" ]; then
      mkdir -p "$ROOT/t/debug/build/$1/out"
      i=0
      while [ "$i" -lt "$2" ]; do
        printf '#[test]\nfn t%s() {}\n' "$i"
        i=$((i + 1))
      done >"$ROOT/t/debug/build/$1/out/generated_tests.rs"
    fi
    . "$HERE/amaru.sh"
    amaru_generated t
  )
}
# Must not count another crate's generated tests, and must count amaru-uplc's in the hashed directory or under the crate directory.
expect amaru_generated_none "$(amaru_generated_of '' 0)" 0
expect amaru_generated_flat "$(amaru_generated_of amaru-uplc-1a2b 3)" 3
expect amaru_generated_nested "$(amaru_generated_of amaru-uplc/1a2b 3)" 3

# The verdict of amaru_caught for the name part divideinteger on a test log of the lines given.
amaru_caught_of() {
  (
    ROOT=$W HERE=$HERE
    . "$HERE/amaru.sh"
    printf '%s\n' "$@" >"$W/amaru-arm.log"
    if amaru_caught "$W/amaru-arm.log" divideinteger; then echo caught; else echo missed; fi
  )
}
# Must not fire on another failed test or a passing divideInteger test, and must fire on a failed one.
expect amaru_caught_other "$(amaru_caught_of 'test builtin_semantics_modinteger_01_v3_protocol_version_11 ... FAILED' 'test builtin_semantics_divideinteger_divideinteger_01_v3_protocol_version_11 ... ok')" missed
expect amaru_caught_divide "$(amaru_caught_of 'test builtin_semantics_divideinteger_divideinteger_neg_pos_v3_protocol_version_11 ... FAILED')" caught

# A build root whose harness tier.sh prints the pairs, probe and given files it was handed, and what value_tier hands it for the pairs $1.
tier_of() {
  R=$W/tier
  rm -rf "$R"
  mkdir -p "$R/value/gen" "$R/scratch/dolos/test_data/ledger-oracle" "$R/scratch/pallas/test_data" "$R/runs"
  touch "$R/scratch/pallas/test_data/dijkstra-a.tx" "$R/scratch/pallas/test_data/dijkstra-b.block" "$R/scratch/pallas/test_data/c.ebbody"
  echo 'echo "pairs=[${PAIRS:-}] probe=$(basename "$PROBE") tx=$(echo $TX_FILES | wc -w) block=$(basename $BLOCK_FILES) eb=$(basename $EB_FILES) n=$2"' >"$R/value/gen/tier.sh"
  (
    ROOT=$R OUT=$R/runs SCRATCH=scratch/dolos PALLAS=scratch/pallas HERE=$HERE VALUE_N=7
    . "$HERE/value.sh"
    value_tier head "$R/runs/t" "$1"
  )
}
# Must not hand every pair when pairs are named, and must hand the given files and no pairs when none are.
expect tier_pairs "$(tier_of 'tx:given eb:arb')" "pairs=[tx:given eb:arb] probe=head tx=3 block=dijkstra-b.block eb=c.ebbody n=7"
expect tier_all "$(tier_of '')" "pairs=[] probe=head tx=3 block=dijkstra-b.block eb=c.ebbody n=7"

# A head dir holding the values $2 and the head probe output $3 for the pair tx.given, and the verdict of value_control with a probe printing $4.
control_of() {
  R=$W/control-$1
  rm -rf "$R"
  mkdir -p "$R/head"
  if [ -n "$2" ]; then
    echo "$2" >"$R/head/tx.given.gen.jsonl"
    printf '%s\n' "$3" >"$R/head/tx.given.probe.jsonl"
  fi
  printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$4" >"$R/probe"
  chmod +x "$R/probe"
  (
    ROOT=$W HERE=$HERE
    . "$HERE/value.sh"
    if value_control "$R/head" "$R/control" "$R/probe" >/dev/null 2>&1; then echo clean; else echo fired; fi
  )
}
# Must not fire on equal output, and must fire on no values, other output and a reached plant.
expect control_same "$(control_of same v 'out' 'out')" clean
expect control_none "$(control_of none '' '' 'out')" fired
expect control_differs "$(control_of differs v 'out' 'other')" fired
expect control_reached "$(control_of reached v 'ORACLE_PLANT reached x' 'ORACLE_PLANT reached x')" fired

# A repository with a base commit, a feature commit and the fix commits named in $1, a manifest arms.toml whose dolos rows are the remaining arguments, and the exit and lines of fix_arms over it.
fixes_of() {
  F=$W/fixes
  rm -rf "$F"
  git init -q "$F"
  git -C "$F" -c user.name=t -c user.email=t@t commit -q --allow-empty -m base
  base=$(git -C "$F" rev-parse HEAD)
  git -C "$F" -c user.name=t -c user.email=t@t commit -q --allow-empty -m "feat: a feature"
  for subject in $1; do
    git -C "$F" -c user.name=t -c user.email=t@t commit -q --allow-empty -m "fix: $subject"
  done
  shift
  {
    echo '[dolos]'
    echo "base = \"$base\""
    echo 'fixes = ['
    for row in "$@"; do
      sha=$(git -C "$F" log -1 --format=%h --abbrev=9 --grep="^fix: ${row%% *}\$")
      echo "  { commit = \"$sha\", ${row#* } },"
    done
    echo ']'
  } >"$F/arms.toml"
  (
    ROOT=$W HERE=$HERE ARMS_MANIFEST=$F/arms.toml
    . "$LIB"
    PLANTED_ARMS="colret coin"
    out=$(fix_arms "$F" dolos HEAD) && code=0 || code=$?
    echo "exit=$code $(echo "$out" | sed 's/commit=[0-9a-f]* //' | paste -sd '|' -)"
  )
}
# Must not fire on a feature commit with no row, an armed fix and an excluded one, and must fire on a fix with no row, an arm outside PLANTED_ARMS, an unknown reason, the retired reason unarmed, a gap or unrevertable row with no why and a base the repository lacks.
expect fixes_none "$(fixes_of '')" "exit=0 FIX-ARMS repo=dolos fixes=0 armed=0 excluded=0 unarmed=0 failing=0"
expect fixes_rowed "$(fixes_of 'a b c d' 'a arm = "colret"' 'b exclude = "conway"' 'c exclude = "gap", why = "no record"' 'd exclude = "unrevertable", why = "a conflict"')" \
  "exit=0 FIX-ARMS repo=dolos fixes=4 armed=1 excluded=3 unarmed=0 failing=0"
expect fixes_gap_unexplained "$(fixes_of 'a b c' 'a exclude = "gap"' 'b exclude = "unrevertable"' 'c exclude = "gap", why = ""')" \
  'exit=1 FIX-ARM repo=dolos row=exclude=gap subject="fix: c"|FIX-ARM repo=dolos row=exclude=unrevertable subject="fix: b"|FIX-ARM repo=dolos row=exclude=gap subject="fix: a"|FIX-ARMS repo=dolos fixes=3 armed=0 excluded=0 unarmed=0 failing=3'
expect fixes_unarmed "$(fixes_of 'a' 'a exclude = "unarmed"')" \
  'exit=1 FIX-ARM repo=dolos row=exclude=unarmed subject="fix: a"|FIX-ARMS repo=dolos fixes=1 armed=0 excluded=0 unarmed=1 failing=1'
expect fixes_missing "$(fixes_of 'a b' 'a arm = "colret"')" \
  'exit=1 FIX-ARM repo=dolos row=none subject="fix: b"|FIX-ARMS repo=dolos fixes=2 armed=1 excluded=0 unarmed=0 failing=1'
expect fixes_unknown_arm "$(fixes_of 'a' 'a arm = "nothing"')" \
  'exit=1 FIX-ARM repo=dolos row=arm=nothing subject="fix: a"|FIX-ARMS repo=dolos fixes=1 armed=0 excluded=0 unarmed=0 failing=1'
expect fixes_uncalled "$(fixes_of 'a' 'a exclude = "uncalled", why = "no caller"')" \
  "exit=0 FIX-ARMS repo=dolos fixes=1 armed=0 excluded=1 unarmed=0 failing=0"
expect fixes_uncalled_unexplained "$(fixes_of 'a' 'a exclude = "uncalled"')" \
  'exit=1 FIX-ARM repo=dolos row=exclude=uncalled subject="fix: a"|FIX-ARMS repo=dolos fixes=1 armed=0 excluded=0 unarmed=0 failing=1'
expect fixes_unknown_reason "$(fixes_of 'a' 'a exclude = "later"')" \
  'exit=1 FIX-ARM repo=dolos row=exclude=later subject="fix: a"|FIX-ARMS repo=dolos fixes=1 armed=0 excluded=0 unarmed=0 failing=1'
expect fixes_base_absent "$(
  F=$W/fixes
  sed -i.bak 's/^base = .*/base = "0000000000000000000000000000000000000001"/' "$F/arms.toml"
  (
    ROOT=$W HERE=$HERE ARMS_MANIFEST=$F/arms.toml
    . "$LIB"
    fix_arms "$F" dolos HEAD | paste -sd '|' -
  )
)" "FIX-ARM repo=dolos base=0000000000000000000000000000000000000001 not-found|FIX-ARMS repo=dolos fixes=0 armed=0 excluded=0 unarmed=0 failing=1"

# Must keep a value switch's name and prefix an overlay switch's.
expect value_key "$(
  ROOT=$W HERE=$HERE
  . "$HERE/value.sh"
  echo "$(value_key value-spend-redeemer) $(value_key emptyguards)"
)" "value-spend-redeemer value-emptyguards"

if [ "$failed" -eq 0 ]; then
  echo "LIB-TEST RESULT PASS"
else
  echo "LIB-TEST RESULT FAIL"
  exit 1
fi
