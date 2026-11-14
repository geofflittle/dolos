#!/bin/sh
# Checks that build_checked refuses a unit cargo reports fresh after a file it reads changed, and passes every other unit, with a runner that prints canned cargo messages, the overlay build's target dir, which arms an overlay switches, when two test logs differ, an encoding arm's verdict, an arm's verdict from its run, what an arm served against its row of served.txt, the switches an overlay plant leaves out, the files a planted tree writes and whether git writes each diff as committed, the free space floor, the candidate's fsync state, the IO guard's breach rule, the records a passing tier writes, the reads the ship tier takes, the image read-live.sh syncs and that it builds nothing, the box lock, how many switched arms are judged at once, the pallas pin, the scratch pallas checkout, when the value leg is due, a value arm's verdict, what the value leg hands the harness tier and which of its steps it reruns, the value control's verdict, the amaru pin, the amaru leg's verdicts, the build image's name by the harness's image scripts, the image in-image.sh runs and the endorser block endorser_fixture.py reads from each LeiosDb layout.
# Usage: lib_test.sh <empty work dir>
set -eu
W=$(cd "$1" && pwd)
# Each case names its own REV, BASE, slice limits and build image, so those of the run that calls the test are dropped.
unset REV BASE ORACLE_SLICE ORACLE_MEMORY ORACLE_SWAP ARM_MEMORY BUILD_IMAGE
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

# Must build cold when the pallas checkout is a repository with no files yet.
setup empty_pallas
git -C "$ROOT" init -q scratch/pallas
expect cold_empty_tree "$(build x=false y=false)" "exit=0 "

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

# Must hand an overlay build the overlay's target dir and keep its record there, and must leave the head's record and trees as they were.
setup overlay_target
build x=false y=false >/dev/null
head_record=$(cat "$ROOT/target/built-units")
git clone -q "$ROOT/scratch/dolos" "$ROOT/scratch/overlay-dolos"
mkdir -p "$ROOT/target/overlay"
printf 'echo "$TARGET" >"%s/target-seen"\ncat "%s/messages"\n' "$ROOT" "$ROOT" >"$ROOT/runner.sh"
overlay_built=$(
  SCRATCH=scratch/dolos IN_IMAGE=$ROOT/runner.sh
  . "$LIB"
  in_overlay build_checked "$ROOT/overlay.json" build >/dev/null
  head=changed
  if [ "$(cat "$ROOT/target/built-units")" = "$head_record" ]; then head=same; fi
  echo "target=$(cat "$ROOT/target-seen") scratch=$SCRATCH head=$head overlay_units=$(grep -c . "$ROOT/target/overlay/built-units")"
)
expect overlay_target "$overlay_built" "target=target/overlay scratch=scratch/dolos head=same overlay_units=2"

# An overlay whose added lines switch a in pallas and b in Dolos, with c only on a context line and f only on a removed one, and diffs under plants/ named for d and e.
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
expect arms "$(arms)" "a:yy:Cargo.toml,Cargo.lock b:yn:src/one.rs c:nn: d:nn: e:nn: f:nn:"
# Must name the overlay's switches, and must not name c, f, or d and e, whose diffs under plants/ arm nothing.
expect planted "$(
  HERE=$H
  . "$LIB"
  echo "$PLANTED_ARMS"
)" "a b"

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
printf '%s\n' 'a lines=1 sha256=01' 'b lines=1 sha256=02' >"$S/served.txt"
mismatches() {
  (
    HERE=$S ARMS_MANIFEST=$S/arms.toml
    . "$LIB"
    arm_mismatches | paste -sd '|' -
  )
}
# Must not fire when every arm has its switch, records and catch, and serves lines no other arm serves.
expect arm_set_agrees "$(mismatches)" ""
printf '%s\n' 'a lines=1 sha256=01' 'b lines=1 sha256=01' >"$S/served.txt"
# Must fire on two arms that serve the same lines.
expect arm_served_twice "$(mismatches)" "REFUSED arms a b serve the same lines in served.txt"
printf '%s\n' 'a lines=1 sha256=01' 'b lines=1 sha256=02' >"$S/served.txt"
# Must not fire on the arms this tree plants.
expect arm_set_committed "$( (. "$LIB" && arm_mismatches) | paste -sd '|' -)" ""
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

# Arms d, e, g and h switched in Dolos, cost-models switched in Dolos with its crate test and k with a deselection, and a scratch tree whose second commit changes d's catch, e's records, the cost-models test source, k's deselection and the place of h's catch.
A=$W/armfiles
rm -rf "$A"
mkdir -p "$A/box/overlay" "$A/box/plants" "$A/scratch/dolos/oracle/box/plants" "$A/scratch/dolos/crates/cardano/tests"
: >"$A/box/overlay/pallas.diff"
for arm in d e g h cost-models; do
  printf '%s\n' '+++ b/crates/core/src/n.rs' '@@ -1 +1,2 @@' ' fn n() {}' "+oracle_plant(\"$arm\")"
done >"$A/box/overlay/dolos.diff"
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
# Must not move an arm of a list with no deselect arm, and must move k after head and before control.
expect arm_order "$(
  HERE=$A/box
  . "$LIB"
  echo "$(arm_order head control d e | paste -sd ' ' -)|$(arm_order head control d k e | paste -sd ' ' -)"
)" "head control d e|head k control d e"

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

# The verdict arm_verdict sets for a log of the lines given with the plant e, whose catch selects the line fires.
verdict_of() {
  (
    HERE=$H OUT=$L
    . "$LIB"
    printf '%s\n' "$@" >"$L/verdict.log"
    arm_verdict e "$L/verdict.log" '^test result: ' e grep -q -x fires "$L/verdict.log"
    echo "$verdict"
  )
}
finished='test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s'
# Must not fire on a finished run whose catch selects nothing, must name a run that did not finish, wrote no log or did not reach its plant, and must fire on a finished run that reached its plant and holds the line.
expect verdict_missed "$(verdict_of 'ORACLE_PLANT reached e' "$finished")" MISSED
expect verdict_unrun "$(verdict_of 'ORACLE_PLANT reached e' fires)" UNRUN
expect verdict_no_log "$(
  HERE=$H OUT=$L
  . "$LIB"
  arm_verdict e "$L/absent.log" '^test result: ' "" true
  echo "$verdict"
)" UNRUN
expect verdict_unreached "$(verdict_of 'ORACLE_PLANT reached ee' fires "$finished")" UNREACHED
expect verdict_caught "$(verdict_of 'ORACLE_PLANT reached e' fires "$finished")" CAUGHT
# Must find no script but lib.sh setting CAUGHT or MISSED, which arm_verdict sets only for a finished run.
expect verdict_words "$(cat "$HERE/breaks.sh" "$HERE/value.sh" "$HERE/amaru.sh" "$HERE/run.sh" "$HERE/regen.sh" | grep -c -E 'CAUGHT|MISSED' || true)" 0

# A box dir whose served.txt holds the row $1 or none, and the verdicts served_check sets for a passing head, the arm a whose catch printed the lines after $1, the arm h held with nothing recorded and a missed arm m, with the SERVED lines it printed and the rows it added.
S=$W/served
served_of() {
  rm -rf "$S"
  mkdir -p "$S/box/overlay" "$S/run"
  : >"$S/box/overlay/dolos.diff"
  if [ -n "$1" ]; then
    echo "$1" >"$S/box/served.txt"
  fi
  shift
  (
    HERE=$S/box OUT=$S/run ROOT=$W/served-tree
    . "$LIB"
    printf '%s\n' "$finished" >"$OUT/a.log"
    printf '%s\n' "$@" >"$OUT/a.caught"
    arm_verdict a "$OUT/a.log" '^test result: ' "" grep . "$OUT/a.caught"
    served_check "head=PASS a=$verdict h=HELD m=MISSED" >"$OUT/served.out"
    echo "$checked"
  )
}
first="thread 'one' (7) panicked at src/a.rs:1:5:"
second="thread 'two' (8) panicked at src/b.rs:2:5:"
row="a lines=2 sha256=$(printf '%s\n' "thread 'one' panicked at src/a.rs:1:5:" "thread 'two' panicked at src/b.rs:2:5:" | sha256sum | cut -c1-64)"
# Must not fire on the same lines in another order with other thread numbers, and must write the row of the lines the catch printed.
expect served_same "$(served_of "$row" "thread 'two' (7) panicked at src/b.rs:2:5:" "thread 'one' (9) panicked at src/a.rs:1:5:")" \
  "head=PASS a=CAUGHT h=UNRECORDED m=MISSED"
expect served_row "$(cat "$S/run/served.txt")" "$row"
# Must fire on another line, naming both rows and the lines, on an arm served.txt has no row for, and on a held arm with nothing recorded.
expect served_changed "$(served_of "$row" "$first" "thread 'two' (8) panicked at src/b.rs:3:5:")" \
  "head=PASS a=CHANGED h=UNRECORDED m=MISSED"
expect served_changed_line "$(head -1 "$S/run/served.out")" \
  "SERVED a=CHANGED reference=\"$row\" run=\"$(cat "$S/run/served.txt")\" lines=$S/run/served/a"
expect served_unreferenced "$(served_of "" "$first" "$second")" "head=PASS a=UNREFERENCED h=UNRECORDED m=MISSED"
expect served_unrecorded_line "$(tail -1 "$S/run/served.out")" "SERVED h=UNRECORDED reference=\"\" run=\"\" lines=$S/run/served/h"

# Writes src/c.rs of the scratch tree served_of reads with the lines given, one per argument.
site_file() {
  mkdir -p "$W/served-tree/scratch/dolos/src"
  printf '%s\n' "$@" >"$W/served-tree/scratch/dolos/src/c.rs"
}
site_file 'fn helper() {}' '#[test]' 'fn two() {' '    assert!(' '        false' '    );' '    other();' '}'
served_of "" "thread 'two' (3) panicked at src/c.rs:4:5:" >/dev/null
sited="$(cat "$S/run/served.txt")"
# Must name a panic's site by the fn above it and the lines between them, and keep a line outside the scratch tree or above every fn.
expect served_site "$(cat "$S/run/served/a")" "thread 'two' panicked at src/c.rs:two+1:5:"
served_of "" "thread 'one' (3) panicked at src/none.rs:4:5:" "thread 'two' (4) panicked at src/c.rs:1:5:" >/dev/null
expect served_site_kept "$(paste -sd ' ' - <"$S/run/served/a")" \
  "thread 'one' panicked at src/none.rs:4:5: thread 'two' panicked at src/c.rs:helper+0:5:"
# Must not fire when lines elsewhere in the file move the panic's line, as an overlay that grows above a test does.
site_file '// a plant' 'fn added() {' '}' 'fn helper() {}' '#[test]' 'pub(crate) async fn two() {' '    assert!(' '        false' '    );' '    other();' '}'
expect served_site_moved "$(served_of "$sited" "thread 'two' (5) panicked at src/c.rs:7:5:")" "head=PASS a=CAUGHT h=UNRECORDED m=MISSED"
# Must fire when the panic moves to another line of its fn, or to another fn.
expect served_site_other_line "$(served_of "$sited" "thread 'two' (5) panicked at src/c.rs:10:5:")" "head=PASS a=CHANGED h=UNRECORDED m=MISSED"
expect served_site_other_fn "$(served_of "$sited" "thread 'two' (5) panicked at src/c.rs:3:1:")" "head=PASS a=CHANGED h=UNRECORDED m=MISSED"
site_file '#[test]' 'fn two() {' '    fn helper() -> bool {' '        true' '    }' '    fn short() -> bool { true }' '    assert!(helper() && !short());' '}'
# Must not name a helper declared inside a test for a panic in the test's own body after it.
served_of "" "thread 'two' (3) panicked at src/c.rs:7:5:" >/dev/null
expect served_site_nested "$(cat "$S/run/served/a")" "thread 'two' panicked at src/c.rs:two+5:5:"
# Must name the helper for a panic inside its body.
served_of "" "thread 'two' (3) panicked at src/c.rs:4:9:" >/dev/null
expect served_site_in_helper "$(cat "$S/run/served/a")" "thread 'two' panicked at src/c.rs:helper+1:9:"
rm -rf "$W/served-tree"

# A work tree of a.rs, b.rs and c.rs, an overlay switching a in a.rs and b in b.rs and adding a call to c.rs, and plant_overlay's exit, printed switches and switched files after the files $1 change and the files $2 go.
plant_of() {
  P=$W/plant
  rm -rf "$P"
  git init -q "$P"
  for f in a b c; do printf '%s\n' "fn $f() {" '}' >"$P/$f.rs"; done
  git -C "$P" add -A
  git -C "$P" -c user.name=t -c user.email=t@t commit -q -m t
  printf '%s\n' 'fn a() {' '    oracle_plant("a");' '}' >"$P/a.rs"
  printf '%s\n' 'fn b() {' '    pallas::crypto::oracle_plant("b");' '}' >"$P/b.rs"
  printf '%s\n' 'fn c() {' '    shared();' '}' >"$P/c.rs"
  git -C "$P" diff >"$W/plant.diff"
  git -C "$P" checkout -q -- .
  for f in $1; do printf '%s\n' 'fn moved() {' '}' >"$P/$f"; done
  for f in ${2:-}; do rm "$P/$f"; done
  (
    HERE=$H
    . "$LIB"
    status=0
    out=$(plant_overlay "$W/plant.diff" "$P" 2>/dev/null) || status=$?
    echo "exit=$status $(echo $out) switched=$(cd "$P" && grep -l oracle_plant a.rs b.rs 2>/dev/null | paste -sd, -)"
  )
}
# Must not name a switch when every hunk applies, must name each switch whose hunk does not apply while planting the others, and must fail when a hunk with no switch does not apply or a file it changes is gone.
expect plant_clean "$(plant_of '')" "exit=0  switched=a.rs,b.rs"
expect plant_switch "$(plant_of a.rs)" "exit=0 a switched=b.rs"
expect plant_switches "$(plant_of 'a.rs b.rs')" "exit=0 a b switched="
expect plant_shared "$(plant_of c.rs)" "exit=1  switched=a.rs,b.rs"
expect plant_gone "$(plant_of '' a.rs)" "exit=1  switched=b.rs"
expect plant_gone_beside_switch "$(plant_of b.rs c.rs)" "exit=1 b switched=a.rs"

# A repository of a.rs, b.rs, a Cargo.toml pinning pallas by git and a submodule no clone checks out, a diff switching a in a.rs as git writes it, the same diff with longer blob names and a diff switching b in b.rs.
T=$W/plant-tree
rm -rf "$T" "$H/tree"
mkdir -p "$H/tree"
git init -q "$T/src"
for f in a b; do printf '%s\n' "fn $f() {" '}' >"$T/src/$f.rs"; done
echo 'pallas-validate = { git = "https://github.com/a/pallas", rev = "111" }' >"$T/src/Cargo.toml"
git -C "$T/src" add -A
mkdir "$T/src/sub"
git -C "$T/src" update-index --add --cacheinfo 160000,0460c5a6152ae094bcfdad64a54b1a6561d7e38a,sub
git -C "$T/src" -c user.name=t -c user.email=t@t commit -q -m t
tree_base=$(git -C "$T/src" rev-parse HEAD)
for f in a b; do
  printf '%s\n' "fn $f() {" "    oracle_plant(\"$f\");" '}' >"$T/src/$f.rs"
  git -C "$T/src" diff --no-color --abbrev=8 >"$H/tree/$f.diff"
  git -C "$T/src" diff --no-color --abbrev=12 >"$H/tree/$f-long.diff"
  git -C "$T/src" checkout -q -- .
done
# Gives each file of the planted clone an old time the index records.
tree_stamp() {
  touch -d @1000000000 "$T/built/a.rs" "$T/built/b.rs" "$T/built/Cargo.toml"
  git -C "$T/built" update-index -q --refresh
}
# Plants the diffs given over the repository's commit into the clone $T/built, printing plant_tree's exit, its regen lines, the files it wrote and kept, the switched files and where the clone's Cargo.toml points pallas.
tree_plant() {
  (
    ROOT=$T HERE=$H
    . "$LIB"
    status=0
    out=$(plant_tree built "$T/src" "$tree_base" scratch/pallas-x "$@" 2>"$T/plant.err") || status=$?
    kept=
    written=
    for f in a.rs b.rs Cargo.toml; do
      if [ "$(stat -c %Y "$T/built/$f")" = 1000000000 ]; then kept="$kept$f,"; else written="$written$f,"; fi
    done
    echo "exit=$status $(echo $out) $(sed -n 's/^REGEN //p' "$T/plant.err" | paste -sd ' ' -) kept=$kept written=$written" \
      "switched=$(cd "$T/built" && grep -l oracle_plant a.rs b.rs | paste -sd, -) $(sed -n 's/.*path = "\([^"]*\)".*/\1/p' "$T/built/Cargo.toml")"
  )
}
tree_plant "$H/tree/a.diff" >/dev/null
tree_stamp
# Must not write a file whose content the last plant gave it, and must say git writes a diff it wrote as committed.
expect tree_kept "$(tree_plant "$H/tree/a.diff")" \
  "exit=0  diff=tree/a.diff same kept=a.rs,b.rs,Cargo.toml, written= switched=a.rs /w/scratch/pallas-x/pallas-validate"
tree_stamp
# Must write each file whose content changes, and only those.
expect tree_moved "$(tree_plant "$H/tree/b.diff")" \
  "exit=0  diff=tree/b.diff same kept=Cargo.toml, written=a.rs,b.rs, switched=b.rs /w/scratch/pallas-x/pallas-validate"
tree_stamp
# Must say git does not write a diff as committed when its bytes differ from what git writes for the same change.
expect tree_regen_differs "$(tree_plant "$H/tree/a-long.diff")" \
  "exit=0  diff=tree/a-long.diff differs kept=Cargo.toml, written=a.rs,b.rs, switched=a.rs /w/scratch/pallas-x/pallas-validate"

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

# The exit of record_pass for a run of the tier $1 of commit c and image i with the verdicts $2, and what it leaves in last-done and in last-ship but for the time.
records_of() {
  (
    HERE=$H
    . "$LIB"
    R=$W/records
    rm -rf "$R"
    mkdir -p "$R"
    LAST_DONE=$R/last-done LAST_SHIP=$R/last-ship
    status=0
    record_pass "$1" c "$2" i || status=$?
    last_done=absent
    last_ship=absent
    if [ -f "$LAST_DONE" ]; then last_done=$(cat "$LAST_DONE"); fi
    if [ -f "$LAST_SHIP" ]; then last_ship=$(sed 's/ time=[0-9TZ:-]*$//' "$LAST_SHIP"); fi
    echo "exit=$status last-done=$last_done last-ship=$last_ship"
  )
}
arms_pass='head=PASS control=CLEAN colret=CAUGHT'
# Must not fire: a done run that passes writes last-done alone, and a ship run whose live leg fails writes neither.
expect ship_by_done "$(records_of done "$arms_pass")" "exit=0 last-done=c last-ship=absent"
# Must not fire: every passing word a done run prints. Must fire: a verdict word the pass rule does not name.
expect done_every_passing_word "$(records_of done "$arms_pass ctx=HELD fix-arms=PASS value=passed-on-these-inputs")" \
  "exit=0 last-done=c last-ship=absent"
expect done_unnamed_word "$(records_of done "$arms_pass coin=UNREACHED")" "exit=1 last-done=absent last-ship=absent"
expect ship_live_failed "$(records_of ship "$arms_pass live-shift=CAUGHT live=FAIL")" "exit=1 last-done=absent last-ship=absent"
expect ship_shift_missed "$(records_of ship "$arms_pass live-shift=MISSED live=PASS")" "exit=1 last-done=absent last-ship=absent"
# Must fire: a ship run that passes writes both.
expect ship_passed "$(records_of ship "$arms_pass live-shift=CAUGHT live=PASS")" \
  "exit=0 last-done=c last-ship=commit=c image=i tier=ship verdicts=\"$arms_pass live-shift=CAUGHT live=PASS\""

# Prints lock=free, lock=held when flock reports the lock taken, or the exit of any other flock failure, for the lock RUN_LOCK.
LOCK_PROBE='status=0; flock -n -E 75 "$RUN_LOCK" true || status=$?; case $status in 0) state=free ;; 75) state=held ;; *) state=flock_exit_$status ;; esac'

# Writes for the build root $1 a cgroup file placing the process in a scope of the slice $2, and that slice's memory limit $3 and swap limit $4.
slice_of() {
  mkdir -p "$1/cgroup-fs$2"
  echo "0::$2/run-t.scope" >"$1/cgroup"
  echo "$3" >"$1/cgroup-fs$2/memory.max"
  echo "$4" >"$1/cgroup-fs$2/memory.swap.max"
}

# Runs the script run.sh, or $3, with the argument done, or $4, on the build root $1 and the lock $1/run.lock, JUDGED set unless $2 is entry, a floor no disk meets so a run past the lock refuses at once, its output and exit in $1/run.out, a df that writes to $1/df.seen whether a process held the lock when the run read free space, a systemctl and a systemd-run that print their arguments, and the oracle slice with limits unless $1/cgroup places it elsewhere.
tier() {
  mkdir -p "$1/bin"
  printf '%s\n' '#!/bin/sh' "$LOCK_PROBE" 'echo "lock=$state" >>"$ROOT/df.seen"' \
    "exec $(command -v df) \"\$@\"" >"$1/bin/df"
  chmod +x "$1/bin/df"
  printf '%s\n' '#!/bin/sh' 'echo "systemctl $*"' >"$1/bin/systemctl"
  printf '%s\n' '#!/bin/sh' 'echo "systemd-run $*"' >"$1/bin/systemd-run"
  chmod +x "$1/bin/systemctl" "$1/bin/systemd-run"
  if [ ! -f "$1/cgroup" ]; then
    slice_of "$1" /dolos.slice/dolos-oracle.slice 2684354560 1073741824
  fi
  (
    ROOT=$1 OUT=$1/runs/t MIN_FREE_G=999999 LOCK_POLL_SECONDS=1 RUN_LOCK=$1/run.lock PATH=$1/bin:$PATH
    CGROUP_FILE=$1/cgroup CGROUP_ROOT=$1/cgroup-fs
    export ROOT OUT MIN_FREE_G LOCK_POLL_SECONDS RUN_LOCK PATH CGROUP_FILE CGROUP_ROOT
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

# Holds the lock $1 in the background, recording its pid there, until $1.go exists, then empties the record and exits after $2 more seconds, or at once.
hold_lock() {
  sh -c 'exec 9>>"$1"; flock -n 9 || exit 1; echo $$ >"$1"; until [ -e "$1.go" ]; do sleep 0.2; done; : >"$1"; sleep "$2"' sh "$1" "${2:-0}" &
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

# Must not fire: a holder that empties its record and keeps the lock over the run's next polls adds no WAITING line.
lock_root lock-emptied
hold_lock "$K/run.lock" 3
tier "$K" &
runner=$!
await_waiting "$K"
touch "$K/run.lock.go"
wait "$holder" || true
wait "$runner" || true
expect lock_emptied "$(run_out "$K")" "WAITING lock_pid=$holder REFUSED free run-exit=2 exit=2"

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

# Runs the entry run.sh on the build root $1, whose judged commit's run.sh records the REV and BASE it receives in $1/old.seen, with REV $2 and BASE $3.
named_run() {
  (
    REV=$2 BASE=$3
    export REV BASE
    tier "$1" entry
  )
}
lock_root named
mkdir -p "$K/old/oracle/box"
printf '%s\n' '#!/bin/sh' 'echo "rev=$REV base=$BASE" >"$ROOT/old.seen"' >"$K/old/oracle/box/run.sh"
git -C "$K/old" init -q
git -C "$K/old" add -A
git -C "$K/old" -c user.name=t -c user.email=t@t commit -q -m old
git clone -q --bare "$K/old" "$K/dolos.git"
full=$(git -C "$K/dolos.git" rev-parse HEAD)
short=$(echo "$full" | cut -c1-8)
# Must not fire: a REV and a BASE given as full shas reach the judged commit's run.sh as given.
named_run "$K" "$full" "$full"
expect named_full "$(cat "$K/old.seen")" "rev=$full base=$full"
# Must not fire: an empty BASE stays empty, so the judged run.sh reads the last passing run's commit.
named_run "$K" "$full" ""
expect named_empty "$(cat "$K/old.seen")" "rev=$full base="
# Must fire: an abbreviated REV and BASE reach the judged commit's run.sh as full shas.
named_run "$K" "$short" "$short"
expect named_short "$(cat "$K/old.seen")" "rev=$full base=$full"
# Must fire: a BASE or REV naming no commit is refused before the run waits for a lock another process holds.
rm -f "$K/run.lock"
hold_lock "$K/run.lock"
named_run "$K" "$full" nothing
expect named_base_refused "$(run_out "$K")" "REFUSED BASE=nothing names no commit in $K/dolos.git run-exit=2 exit=2"
named_run "$K" nothing "$full"
expect named_rev_refused "$(run_out "$K")" "REFUSED REV=nothing names no commit in $K/dolos.git run-exit=2 exit=2"
touch "$K/run.lock.go"
wait "$holder" || true

# Must fire: the entry run.sh and the entry regen.sh each hand the run to the script of the same name in the judged commit, with its arguments.
lock_root entry-script
mkdir -p "$K/old/oracle/box"
for script in run.sh regen.sh; do
  printf '%s\n' '#!/bin/sh' "echo \"script=$script args=\$*\" >\"\$ROOT/old.seen\"" >"$K/old/oracle/box/$script"
done
git -C "$K/old" init -q
git -C "$K/old" add -A
git -C "$K/old" -c user.name=t -c user.email=t@t commit -q -m old
git clone -q --bare "$K/old" "$K/dolos.git"
for script in run.sh regen.sh; do
  (
    REV=$(git -C "$K/dolos.git" rev-parse HEAD)
    export REV
    tier "$K" entry "$script" done
  )
  expect "entry_$script" "$(cat "$K/old.seen")" "script=$script args=done"
done

# A digest reference to an image, and the dir $W/reads-$1 of reads made from it at the commit $2 whose candidate verdict is $3.
IMAGE_REF=ghcr.io/o/dolos@sha256:$(printf '%064d' 0)
reads_of() {
  mkdir -p "$W/reads-$1"
  echo "image=$IMAGE_REF commit=$2 candidate=$3" >"$W/reads-$1/reads.txt"
}
reads_of rev c PASS
# Must not fire: run.sh given quick, done or ship goes on to the free space floor, ship given reads made at REV.
for word in quick done ship; do
  lock_root "tier-word-$word"
  (
    REV=c LIVE_READS=$W/reads-rev
    export REV LIVE_READS
    tier "$K" "" run.sh "$word"
  )
  expect "tier_word_$word" "$(run_out "$K")" "REFUSED free run-exit=2 exit=2"
done
# Must fire: run.sh given any other word prints the usage line naming the three tiers and exits 2.
for word in live value regen quicker; do
  lock_root "tier-word-$word"
  tier "$K" "" run.sh "$word"
  expect "tier_word_$word" "$(run_out "$K")" "usage: run.sh quick|done|ship run-exit=2 exit=2"
done

# The output of a ship run with REV c on the build root $W/ship-reads-$1, given the reads $2, or no reads when $2 is empty.
ship_reads_of() {
  lock_root "ship-reads-$1"
  (
    REV=c
    export REV
    unset LIVE_READS
    if [ -n "$2" ]; then
      LIVE_READS=$2
      export LIVE_READS
    fi
    tier "$K" "" run.sh ship
  )
  run_out "$K"
}
# Must not fire: reads whose candidate failed reach the free space floor, since the live verdict fails on them later.
reads_of failed c "FAIL exit=1"
expect ship_reads_failed "$(ship_reads_of failed "$W/reads-failed")" "REFUSED free run-exit=2 exit=2"
# Must refuse: no reads, a dir that holds no record of them, and reads of an image built from another commit than REV.
expect ship_reads_unset "$(ship_reads_of unset "")" "REFUSED LIVE_READS= holds no reads.txt run-exit=2 exit=2"
mkdir -p "$W/reads-none"
expect ship_reads_none "$(ship_reads_of none "$W/reads-none")" \
  "REFUSED LIVE_READS=$W/reads-none holds no reads.txt run-exit=2 exit=2"
reads_of other d PASS
expect ship_reads_other "$(ship_reads_of other "$W/reads-other")" \
  "REFUSED LIVE_READS=$W/reads-other commit=d is not REV=c run-exit=2 exit=2"

# A build root $W/$1 whose dolos.git holds one commit of these box scripts, without read-live.sh when $2 is old, with a candidate.sh that records what it was handed in the root's candidate.seen and exits $3, and a docker that logs each call to the root's docker.log and answers image inspect with that commit's id, or with $4 when it is given.
image_root() {
  lock_root "$1"
  mkdir -p "$K/src/oracle/box"
  cp "$HERE"/*.sh "$K/src/oracle/box/"
  cp -R "$HERE/bin" "$HERE/overlay" "$K/src/oracle/box/"
  if [ "$2" = old ]; then
    rm -f "$K/src/oracle/box/read-live.sh"
  fi
  printf '%s\n' '#!/bin/sh' "echo \"candidate=\$CANDIDATE commit=\$COMMIT dir=\$1\" >'$K/candidate.seen'" "exit $3" \
    >"$K/src/oracle/box/candidate.sh"
  git -C "$K/src" init -q
  git -C "$K/src" add -A
  git -C "$K/src" -c user.name=t -c user.email=t@t commit -q -m box
  git clone -q --bare "$K/src" "$K/dolos.git"
  revision=${4-$(git -C "$K/dolos.git" rev-parse HEAD)}
  printf '%s\n' '#!/bin/sh' "echo \"\$*\" >>'$K/docker.log'" \
    "if [ \"\$1 \$2\" = 'image inspect' ]; then echo '$revision'; fi" >"$K/docker"
  chmod +x "$K/docker"
  : >"$K/docker.log"
  : >"$K/live.env"
}

# The output of read-live.sh run as an entry on the build root of image_root with the image $1, a build image named so that any build reaches the docker, and no harness to fetch a build image's scripts from.
read_live_of() {
  (
    IMAGE=$1 ORACLE_DOCKER=$K/docker BUILD_IMAGE=b VALUE_HARNESS_URL=$K/missing
    export IMAGE ORACLE_DOCKER BUILD_IMAGE VALUE_HARNESS_URL
    tier "$K" entry read-live.sh
  )
  run_out "$K"
}

# The docker calls the image check makes for the image $1.
image_calls() {
  printf '%s\n' "pull -q --platform linux/amd64 $1" \
    "image inspect --platform linux/amd64 -f {{index .Config.Labels \"org.opencontainers.image.revision\"}} $1"
}

# Must not fire: an image named by digest whose revision label is a commit holding read-live.sh hands the run to that commit's read-live.sh, which syncs the image as the candidate and records the image, the commit and the candidate's verdict.
image_root read-live new 0
sha=$(git -C "$K/dolos.git" rev-parse HEAD)
expect read_live_runs "$(read_live_of "$IMAGE_REF")" \
  "READS image=$IMAGE_REF commit=$sha candidate=PASS dir=$K/runs/t/live run-exit=0 exit=0"
expect read_live_candidate "$(cat "$K/candidate.seen" 2>&1)" "candidate=$IMAGE_REF commit=$sha dir=$K/runs/t/live"
expect read_live_record "$(cat "$K/runs/t/live/reads.txt" 2>&1)" "image=$IMAGE_REF commit=$sha candidate=PASS"
# Must not fire: the run builds nothing, so its only docker calls are the image check's, though a build would reach this docker.
expect read_live_builds_nothing "$(cat "$K/docker.log")" "$(image_calls "$IMAGE_REF")"
# Must fire: a build on the same root reaches the docker, so read_live_builds_nothing would see one.
(
  ROOT=$K HERE=$K/src/oracle/box OUT=$K/runs/control BUILD_IMAGE=b ORACLE_DOCKER=$K/docker SCRATCH=scratch PALLAS=scratch
  export ROOT BUILD_IMAGE
  . "$K/src/oracle/box/lib.sh"
  : >"$K/docker.log"
  build_candidate >/dev/null 2>&1 || true
)
expect read_live_build_seen "$(cut -d ' ' -f 1-4 "$K/docker.log" | sed -n 1p)" "run --cgroup-parent dolos-oracle.slice --rm"
# Must fire: a candidate that fails is recorded as failed and fails the run.
image_root read-live-failed new 1
sha=$(git -C "$K/dolos.git" rev-parse HEAD)
expect read_live_failed "$(read_live_of "$IMAGE_REF")" \
  "READS image=$IMAGE_REF commit=$sha candidate=FAIL exit=1 dir=$K/runs/t/live run-exit=1 exit=1"
# Must refuse before any docker call: an image named by a tag, or by a tag and a digest.
image_root read-live-tag new 0
expect read_live_tag "$(read_live_of ghcr.io/o/dolos:sha-c)" \
  "REFUSED IMAGE=ghcr.io/o/dolos:sha-c is not <repository>@sha256:<64 hex digits> with no tag run-exit=2 exit=2"
expect read_live_tag_and_digest "$(read_live_of "ghcr.io/o/dolos:sha-c@sha256:$(printf '%064d' 0)")" \
  "REFUSED IMAGE=ghcr.io/o/dolos:sha-c@sha256:$(printf '%064d' 0) is not <repository>@sha256:<64 hex digits> with no tag run-exit=2 exit=2"
expect read_live_tag_calls "$(cat "$K/docker.log")" ""
# Must refuse: an image whose revision label is empty, missing, or not a full commit id.
for label in empty none short; do
  case $label in
    empty) value= ;;
    none) value='<no value>' ;;
    short) value=$(echo "$sha" | cut -c1-7) ;;
  esac
  image_root "read-live-label-$label" new 0 "$value"
  expect "read_live_label_$label" "$(read_live_of "$IMAGE_REF")" \
    "REFUSED IMAGE=$IMAGE_REF revision=$value is not a full commit id run-exit=2 exit=2"
done
# Must refuse: a revision label naming a commit the build root lacks, and one naming a commit older than read-live.sh.
image_root read-live-absent new 0 "$(printf '%040d' 1)"
expect read_live_absent "$(read_live_of "$IMAGE_REF")" \
  "REFUSED REV=$(printf '%040d' 1) names no commit in $K/dolos.git run-exit=2 exit=2"
image_root read-live-old old 0
sha=$(git -C "$K/dolos.git" rev-parse HEAD)
expect read_live_old "$(read_live_of "$IMAGE_REF")" \
  "REFUSED REV=$sha holds no oracle/box/read-live.sh run-exit=2 exit=2"

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

# A box dir with the value and amaru legs' files and a scratch tree with its corpora, and whether each leg is due before a pass, after one, after a changed pin, image, plant, corpus or seed.
V=$W/value
rm -rf "$V"
mkdir -p "$V/box/value" "$V/box/overlay" "$V/box/amaru" "$V/runs" "$V/scratch/dolos/test_data/ledger-oracle"
cp "$HERE/lib.sh" "$HERE/value.sh" "$HERE/amaru.sh" "$V/box/"
echo r1 >"$V/box/value/harness.rev"
echo p1 >"$V/box/value/pallas.diff"
echo o1 >"$V/box/overlay/pallas.diff"
echo a1 >"$V/box/amaru/a.diff"
echo i1 >"$V/scratch/dolos/test_data/ledger-oracle/ic.jsonl.gz"
echo s1 >"$V/scratch/dolos/test_data/ledger-oracle/supp.jsonl.gz"
: >"$V/box/served.txt"
# Prints for the leg $1, value or amaru, on the pin $2 and, for amaru, the build image $3 whether it is due, or with record set records its inputs as passed.
due() {
  (
    ROOT=$V HERE=$V/box SCRATCH=scratch/dolos
    . "$V/box/lib.sh"
    . "$V/box/value.sh"
    . "$V/box/amaru.sh"
    if [ "$1" = value ]; then
      inputs=$(value_inputs "$2")
      last=$LAST_VALUE
    else
      inputs=$(amaru_inputs "$2" "$3")
      last=$LAST_AMARU
    fi
    if [ "${record:-}" = yes ]; then
      echo "$inputs" >"$last"
    elif leg_due "$last" "$inputs"; then
      echo due
    else
      echo passed
    fi
  )
}
# Must fire with no passing leg, must not on the inputs that passed, and must fire on another pin, another build image for amaru, another plant of its own, another corpus or another seed.
expect value_due_none "$(due value aaa i1)" due
expect amaru_due_none "$(due amaru 'u r1' i1)" due
record=yes due value aaa i1
record=yes due amaru 'u r1' i1
expect value_due_same "$(due value aaa i1)" passed
expect amaru_due_same "$(due amaru 'u r1' i1)" passed
expect value_due_pin "$(due value bbb i1)" due
expect amaru_due_rev "$(due amaru 'u r2' i1)" due
expect amaru_due_image "$(due amaru 'u r1' i2)" due
echo p2 >"$V/box/value/pallas.diff"
expect value_due_plant "$(due value aaa i1)" due
# Must not fire on the plant of the other leg.
expect amaru_due_value_plant "$(due amaru 'u r1' i1)" passed
record=yes due value aaa i1
echo a2 >"$V/box/amaru/a.diff"
expect amaru_due_plant "$(due amaru 'u r1' i1)" due
expect value_due_amaru_plant "$(due value aaa i1)" passed
record=yes due amaru 'u r1' i1
echo s2 >"$V/scratch/dolos/test_data/ledger-oracle/supp.jsonl.gz"
expect value_due_corpus "$(due value aaa i1)" due
expect amaru_due_corpus "$(due amaru 'u r1' i1)" passed
record=yes due value aaa i1
expect value_due_seed "$(VALUE_SEED=2 due value aaa i1)" due
# Must not fire on another row of an arm of neither leg or of the other leg, and must fire on another row of its own arm.
printf '%s\n' 'amaru-d lines=1 sha256=a' 'coin lines=1 sha256=a' 'value-e lines=1 sha256=a' >"$V/box/served.txt"
record=yes due value aaa i1
record=yes due amaru 'u r1' i1
printf '%s\n' 'amaru-d lines=1 sha256=a' 'coin lines=2 sha256=b' 'value-e lines=1 sha256=a' >"$V/box/served.txt"
expect value_due_other_row "$(due value aaa i1)" passed
expect amaru_due_other_row "$(due amaru 'u r1' i1)" passed
printf '%s\n' 'amaru-d lines=1 sha256=a' 'coin lines=2 sha256=b' 'value-e lines=2 sha256=b' >"$V/box/served.txt"
expect value_due_value_row "$(due value aaa i1)" due
expect amaru_due_value_row "$(due amaru 'u r1' i1)" passed
printf '%s\n' 'amaru-d lines=2 sha256=b' 'coin lines=2 sha256=b' 'value-e lines=1 sha256=a' >"$V/box/served.txt"
expect value_due_amaru_row "$(due value aaa i1)" passed
expect amaru_due_amaru_row "$(due amaru 'u r1' i1)" due

# A build root whose branch pins pallas and holds the corpora, and a copy of this box dir whose arms, tests, image build and legs are stubs that count each leg run.
Q=$W/quick
rm -rf "$Q" "$W/quick-src" "$W/quick-box"
mkdir -p "$Q/runs" "$W/quick-src/test_data/ledger-oracle"
git init -q --bare "$Q/dolos.git"
git -C "$W/quick-src" init -q
echo 'pallas-validate = { git = "https://github.com/a/pallas", rev = "abc" }' >"$W/quick-src/Cargo.toml"
echo i >"$W/quick-src/test_data/ledger-oracle/ic.jsonl.gz"
echo s >"$W/quick-src/test_data/ledger-oracle/supp.jsonl.gz"
git -C "$W/quick-src" add -A
git -C "$W/quick-src" -c user.name=t -c user.email=t@t commit -q -m t
git -C "$W/quick-src" push -q "$Q/dolos.git" HEAD:refs/heads/b
qrev=$(git -C "$W/quick-src" rev-parse HEAD)
cp -R "$HERE" "$W/quick-box"
echo 'echo "ARMS head=PASS control=CLEAN logs=x"' >"$W/quick-box/breaks.sh"
echo 'exit 0' >"$W/quick-box/lib_test.sh"
echo 'exit 0' >"$W/quick-box/fields_test.sh"
echo 'build_image() { echo dolos-build:stub; }' >>"$W/quick-box/lib.sh"
echo 'value_leg() { echo ran >>"$ROOT/legs"; echo "VALUE value-head=PASS value-control=CLEAN pin=x"; }' >>"$W/quick-box/value.sh"
printf '%s\n' 'amaru_leg() { echo ran >>"$ROOT/amaru-legs"; echo "AMARU amaru-head=PASS rev=x"; }' \
  'amaru_pin() { cat "$ROOT/amaru-pin"; }' 'pallas_history() { :; }' 'image_id() { echo i1; }' >>"$W/quick-box/amaru.sh"
echo 'u r1' >"$Q/amaru-pin"
slice_of "$Q" /dolos.slice/dolos-oracle.slice 2684354560 1073741824
# The number of value and amaru leg runs after a quick run on the build root Q with BASE the judged commit.
quick_legs() {
  (
    ROOT=$Q OUT=$Q/runs/q$1 JUDGED=t REV=$qrev BASE=$qrev MIN_FREE_G=0 RUN_LOCK=$Q/run.lock
    CGROUP_FILE=$Q/cgroup CGROUP_ROOT=$Q/cgroup-fs
    export ROOT OUT JUDGED REV BASE MIN_FREE_G RUN_LOCK CGROUP_FILE CGROUP_ROOT
    timeout 30 sh "$W/quick-box/run.sh" quick >"$Q/q$1.out" 2>&1 || true
  )
  echo "value=$(cat "$Q/legs" 2>/dev/null | grep -c . || true) amaru=$(cat "$Q/amaru-legs" 2>/dev/null | grep -c . || true)"
}
# Must run the legs on an unchanged pin when no leg has passed on these inputs, and must not run them again once they have.
expect quick_value_unpassed "$(quick_legs 1)" "value=1 amaru=1"
expect quick_value_passed "$(quick_legs 2)" "value=1 amaru=1"
expect quick_value_line "$(grep '^DONE ' "$Q/q2.out" | sed 's/ base=.*//')" \
  "DONE tier=quick head=PASS control=CLEAN value=passed-on-these-inputs amaru=passed-on-these-inputs"
# Must run the amaru leg alone on another amaru commit.
echo 'u r2' >"$Q/amaru-pin"
expect quick_amaru_rev "$(quick_legs 3)" "value=1 amaru=2"

# The run of the value arm emptyguards with the tier output $2, the reached line $3 and the disagreement class $4, and value_verdict's verdict for it.
value_verdict_of() {
  V=$W/value-verdict-$1
  rm -rf "$V"
  mkdir -p "$V/value/emptyguards"
  echo "$2" >"$V/value/emptyguards.out"
  echo "$3" >"$V/value/emptyguards/tx.given.reached"
  printf '{"cls": "%s", "key": "k"}\n' "$4" >"$V/value/emptyguards/tx.given.schema.dis.jsonl"
  (
    ROOT=$W HERE=$HERE OUT=$V
    . "$LIB"
    . "$HERE/value.sh"
    value_verdict emptyguards
    echo "$verdict"
  )
}
ran='DECODE-ORACLE values=4 ledger_accepts=4'
reached='ORACLE_PLANT reached emptyguards'
class='pallas refuses ledger-valid input'
# Must not fire with another class, must name a tier that judged no value or a plant unreached or another plant reached, and must fire with both.
expect value_verdict_other_class "$(value_verdict_of other_class "$ran" "$reached" other)" MISSED
expect value_verdict_unrun "$(value_verdict_of unrun 'DECODE-ORACLE values=0 ledger_accepts=0' "$reached" "$class")" UNRUN
expect value_verdict_no_output "$(value_verdict_of no_output '' "$reached" "$class")" UNRUN
expect value_verdict_unreached "$(value_verdict_of unreached "$ran" '' "$class")" UNREACHED
expect value_verdict_other_plant "$(value_verdict_of other_plant "$ran" 'ORACLE_PLANT reached emptyguard' "$class")" UNREACHED
expect value_verdict_both "$(value_verdict_of both "$ran" "$reached" "$class")" CAUGHT
# Must record no line of another class, and must record each disagreement of the class with its file's name under the arm's verdict name.
expect value_served_other_class "$(wc -l <"$W/value-verdict-other_class/served/value-emptyguards" | tr -dc 0-9)" 0
expect value_served_both "$(cat "$W/value-verdict-both/served/value-emptyguards")" \
  "tx.given.schema.dis.jsonl:{\"cls\": \"$class\", \"key\": \"k\"}"

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

# The verdict of amaru_caught for the name part divideinteger on a test log of the lines given, and the lines it printed.
amaru_caught_of() {
  (
    ROOT=$W HERE=$HERE
    . "$HERE/amaru.sh"
    printf '%s\n' "$@" >"$W/amaru-arm.log"
    if lines=$(amaru_caught "$W/amaru-arm.log" divideinteger); then echo "caught $lines"; else echo "missed $lines"; fi
  )
}
# Must not fire on another failed test or a passing divideInteger test, and must fire on a failed one, printing it.
expect amaru_caught_other "$(amaru_caught_of 'test builtin_semantics_modinteger_01_v3_protocol_version_11 ... FAILED' 'test builtin_semantics_divideinteger_divideinteger_01_v3_protocol_version_11 ... ok')" "missed "
expect amaru_caught_divide "$(amaru_caught_of 'test builtin_semantics_divideinteger_divideinteger_neg_pos_v3_protocol_version_11 ... FAILED')" \
  "caught test builtin_semantics_divideinteger_divideinteger_neg_pos_v3_protocol_version_11 ... FAILED"

# A build root whose harness tier.sh runs tx:given and eb:arb unless handed pairs, logging each pair whose values it made and each it ran the probe $R/head on, which prints the line in probe-KIND before each value.
tier_root() {
  R=$W/tier
  rm -rf "$R"
  mkdir -p "$R/value/gen/bin" "$R/scratch/dolos/test_data/ledger-oracle" "$R/scratch/pallas/test_data"
  touch "$R/scratch/dolos/test_data/ledger-oracle/ic.jsonl.gz" "$R/scratch/dolos/test_data/ledger-oracle/supp.jsonl.gz"
  echo a >"$R/scratch/pallas/test_data/dijkstra-a.tx"
  touch "$R/scratch/pallas/test_data/dijkstra-b.block" "$R/scratch/pallas/test_data/c.ebbody"
  echo generator >"$R/value/gen/bin/conformance-gen"
  echo image-1 >"$R/image"
  printf '#!/bin/sh\ncat "%s/probe-$1" 2>/dev/null\ncat\n' "$R" >"$R/head"
  chmod +x "$R/head"
  cat >"$R/value/gen/tier.sh" <<'EOF'
mkdir -p "$1"
echo "tier pairs=[$(echo ${PAIRS:-})] probe=$(basename "$PROBE") from=${GEN_FROM:+yes} tx=$(echo $TX_FILES | wc -w) block=$(basename $BLOCK_FILES) eb=$(basename $EB_FILES) n=$2" >>"$LOG"
for p in ${PAIRS:-tx:given eb:arb}; do
  b=$1/${p%%:*}.${p#*:}
  if [ "${VRUN:-}" = /dev/null ]; then
    echo 0 >"$b.status"
    continue
  fi
  if [ -n "${GEN_FROM:-}" ]; then
    ln -f "$GEN_FROM/${p%%:*}.${p#*:}.gen.jsonl" "$b.gen.jsonl"
    ln -f "$GEN_FROM/${p%%:*}.${p#*:}.oracle.jsonl" "$b.oracle.jsonl"
  else
    echo "made $p" >>"$LOG"
    case $p in tx:given) cat $TX_FILES ;; *) printf 'v\nw\n' ;; esac >"$b.gen.jsonl"
    sed 's/^/o/' "$b.gen.jsonl" >"$b.oracle.jsonl"
  fi
  "$PROBE" "${p%%:*}" <"$b.gen.jsonl" >"$b.probe.jsonl"
  [ "$PROBE" = true ] || echo "ran $p" >>"$LOG"
  R=$(dirname "$LOG")
  cat "$R/status-${p%%:*}" >"$b.status" 2>/dev/null || echo 0 >"$b.status"
  [ -e "$R/crash-${p%%:*}" ] || echo 1 >"$b.compared"
done
[ -n "${TALLY:-}" ] || echo "DECODE-ORACLE values=$(cat "$1"/*.gen.jsonl | grep -c .)"
EOF
}

# What value_tier made and ran for the pairs $1 in a run dir of a name no earlier call used, over the results the earlier calls kept, and the tally of the dir.
tier_of() {
  : >"$R/log"
  runs=$(($(cat "$R/runs.count" 2>/dev/null || echo 0) + 1))
  echo "$runs" >"$R/runs.count"
  rm -rf "$R"/runs-*
  mkdir -p "$R/runs-$runs/value/bin"
  cp "$R/head" "$R/runs-$runs/value/bin/head"
  (
    ROOT=$R OUT=$R/runs-$runs SCRATCH=scratch/dolos PALLAS=scratch/pallas HERE=$HERE VALUE_N=2 LOG=$R/log
    export LOG
    . "$HERE/value.sh"
    image_id() { cat "$R/image"; }
    value_tier head "$R/runs-$runs/value/head" "$1" | grep '^DECODE-ORACLE '
  )
  echo "made=[$(sed -n 's/^made //p' "$R/log" | sort | paste -sd ' ' -)] ran=[$(sed -n 's/^ran //p' "$R/log" | sort | paste -sd ' ' -)]"
}

# A first run makes and runs every pair, and each later run with nothing changed must make and run none and still tally every value.
tier_root
expect tier_first "$(tier_of '')" "DECODE-ORACLE values=3
made=[eb:arb tx:given] ran=[eb:arb tx:given]"
expect tier_unchanged "$(tier_of '')" "DECODE-ORACLE values=3
made=[] ran=[]"
expect tier_named "$(tier_of 'tx:given')" "DECODE-ORACLE values=1
made=[] ran=[]"
# Must rerun only the pair whose probe output differs, and must remake the values of a given pair when its file changes, and of every pair when the ledger image does.
echo extra >"$R/probe-tx"
expect tier_probe_out "$(tier_of '')" "DECODE-ORACLE values=3
made=[] ran=[tx:given]"
echo b >"$R/scratch/pallas/test_data/dijkstra-a.tx"
expect tier_given_file "$(tier_of '')" "DECODE-ORACLE values=3
made=[tx:given] ran=[tx:given]"
echo generator-2 >"$R/value/gen/bin/conformance-gen"
expect tier_generator "$(tier_of '')" "DECODE-ORACLE values=3
made=[eb:arb tx:given] ran=[eb:arb tx:given]"
# Must hand the harness the given files, the count and the values it made.
expect tier_handed "$(grep '^tier ' "$R/log" | tail -1)" "tier pairs=[eb:arb tx:given] probe=head from=yes tx=3 block=dijkstra-b.block eb=c.ebbody n=2"
# Must not fire: an image docker holds under a tag the harness does not run changes no input.
echo image-2 >"$R/image"
expect tier_other_image "$(tier_of '')" "DECODE-ORACLE values=3
made=[] ran=[]"
# Must not fire: a pair that failed before its compare finished is run again.
echo 1 >"$R/status-tx"
touch "$R/crash-tx"
echo extra-2 >>"$R/probe-tx"
tier_of '' >/dev/null
expect tier_crash_rerun "$(tier_of '')" "DECODE-ORACLE values=3
made=[] ran=[tx:given]"
# Must fire: a pair whose compare finished with a disagreement is kept like a pass.
rm "$R/crash-tx"
tier_of '' >/dev/null
expect tier_disagree_kept "$(tier_of '')" "DECODE-ORACLE values=3
made=[] ran=[]"
rm "$R/status-tx"

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
    value_control "$R/head" "$R/control" "$R/probe" >/dev/null 2>&1
    echo "$control"
  )
}
# Must not fire on equal output, must name a control that ran no value, and must fire on other output and a reached plant.
expect control_same "$(control_of same v 'out' 'out')" CLEAN
expect control_none "$(control_of none '' '' 'out')" UNRUN
expect control_differs "$(control_of differs v 'out' 'other')" FIRED
expect control_reached "$(control_of reached v 'ORACLE_PLANT reached x' 'ORACLE_PLANT reached x')" FIRED

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

# A pool of the width $1 over eight items, each line the item, whether four items had started within five seconds of its start, and whether at most four ran half a second later.
pool_of() {
  P=$W/pool-$1
  rm -rf "$P"
  mkdir -p "$P/started" "$P/running"
  (
    ROOT=$W OUT=$P
    . "$LIB"
    seen() {
      touch "$P/started/$1" "$P/running/$1"
      t=0
      while [ "$(ls "$P/started" | wc -l)" -lt 4 ] && [ "$t" -lt 50 ]; do
        sleep 0.1
        t=$((t + 1))
      done
      barrier=met
      if [ "$(ls "$P/started" | wc -l)" -lt 4 ]; then barrier=unmet; fi
      sleep 0.5
      bound=ok
      if [ "$(ls "$P/running" | wc -l)" -gt 4 ]; then bound=over; fi
      rm "$P/running/$1"
      echo "$1:$barrier:$bound"
    }
    pool "$1" seen a b c d e f g h | paste -sd ' ' -
  )
}
# Must run four at a time on a width of four and print in argument order, and must not run more than four at once.
expect pool_four "$(pool_of 4)" "a:met:ok b:met:ok c:met:ok d:met:ok e:met:ok f:met:ok g:met:ok h:met:ok"

# The crate test arms, the judge's added packages and the test targets of the arms given, and the executables a message file of two test binaries names.
crate_of() {
  (
    ROOT=$W
    . "$LIB"
    printf '%s\n' '{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"dolos","src_path":"a"},"executable":"/w/target/release/deps/dolos-1"}' \
      '{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"dolos_cardano","src_path":"b"},"executable":"/w/target/release/deps/dolos_cardano-2"}' \
      '{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"hex","src_path":"c"},"executable":null}' >"$W/two.build"
    echo "arms=[$(crate_arms "$@")] joint=[$(joint_packages "$@")] targets=[$(for a in $(crate_arms "$@"); do crate_test_target "$a"; done | paste -sd ' ' -)] dolos=$(executable "$W/two.build" dolos) cardano=$(executable "$W/two.build" dolos_cardano) last=$(executable "$W/two.build")"
  )
}
# Must name no crate test and add no package for arms the judge runs, must add dolos-cardano alone for the non-strict lib tests, and must pick each executable by its target.
expect crate_none "$(crate_of head control coin)" "arms=[] joint=[] targets=[] dolos=/w/target/release/deps/dolos-1 cardano=/w/target/release/deps/dolos_cardano-2 last=/w/target/release/deps/dolos_cardano-2"
expect crate_all "$(crate_of head cost-models endorser-tx-dropped unsupported-untyped watch-block-copied)" "arms=[cost-models endorser-tx-dropped unsupported-untyped watch-block-copied] joint=[-p dolos-cardano] targets=[musashi_cost_models dolos dolos_cardano dolos] dolos=/w/target/release/deps/dolos-1 cardano=/w/target/release/deps/dolos_cardano-2 last=/w/target/release/deps/dolos_cardano-2"
expect crate_strict "$(crate_of cost-models endorser-tx-dropped | sed 's/ dolos=.*//')" "arms=[cost-models endorser-tx-dropped] joint=[] targets=[musashi_cost_models dolos]"

# Must keep a value switch's name and prefix an overlay switch's.
expect value_key "$(
  ROOT=$W HERE=$HERE
  . "$HERE/value.sh"
  echo "$(value_key value-spend-redeemer) $(value_key emptyguards)"
)" "value-spend-redeemer value-emptyguards"

# Must not fire: run.sh, regen.sh and breaks.sh run in the oracle slice with its limits set go on to the free space floor without setting a limit or starting a scope, as the lock cases of each entry script show.
for script in run.sh regen.sh breaks.sh; do
  lock_root "slice-in-$script"
  tier "$K" "" "$script"
  expect "slice_in_$script" "$(run_out "$K")" "REFUSED free$([ "$script" != breaks.sh ] && echo ' run-exit=2') exit=2"
done
# Must fire: each entry script run outside the slice with the box's ORACLE_MEMORY sets its limits and runs itself again in a scope of it, before taking the lock.
for script in run.sh regen.sh breaks.sh candidate.sh endorser-fixture.sh; do
  lock_root "slice-out-$script"
  slice_of "$K" /user.slice/user-0.slice 9999 max
  case $script in
    candidate.sh) arg=dir ;;
    endorser-fixture.sh) arg=out ;;
    *) arg=done ;;
  esac
  (
    CANDIDATE=c COMMIT=c NODE=n ENDPOINT=e SERVE_CONFIG=$K/config CANDIDATE_GRPC=g CANDIDATE_NEED_G=999999
    TREE=t GRPCURL=g PEER=p LEIOS_DB=l ORACLE_MEMORY=24G
    export CANDIDATE COMMIT NODE ENDPOINT SERVE_CONFIG CANDIDATE_GRPC CANDIDATE_NEED_G TREE GRPCURL PEER LEIOS_DB ORACLE_MEMORY
    tier "$K" "" "$script" "$([ "$arg" = done ] && echo done || echo "$K/$arg")"
  )
  expect "slice_out_$script" "$(run_out "$K" | sed -e "s#$HERE/##g" -e "s#$K/##g")" \
    "systemctl set-property --runtime dolos-oracle.slice MemoryMax=24G MemorySwapMax=1G systemd-run --scope --quiet --collect --slice=dolos-oracle.slice sh $script $arg exit=0"
  expect "slice_out_lock_$script" "$(record_left "$K")" removed
done
# Must refuse: a run outside the slice given no ORACLE_MEMORY sets no limit and starts no scope.
lock_root slice-memory-unset
slice_of "$K" /user.slice/user-0.slice 9999 max
tier "$K"
expect slice_memory_unset "$(run_out "$K")" "REFUSED slice=dolos-oracle.slice memory=unset run-exit=2 exit=2"
# Must fire: an ORACLE_MEMORY given to a run sets the slice's memory limit, and the swap limit stays.
lock_root slice-lowered
slice_of "$K" /user.slice/user-0.slice 9999 max
(
  ORACLE_MEMORY=1536M
  export ORACLE_MEMORY
  tier "$K"
)
expect slice_lowered "$(run_out "$K" | sed -e "s#$HERE/##g")" \
  "systemctl set-property --runtime dolos-oracle.slice MemoryMax=1536M MemorySwapMax=1G systemd-run --scope --quiet --collect --slice=dolos-oracle.slice sh run.sh done exit=0"
# Must refuse a run in the oracle slice when the slice has no memory limit or no swap limit.
lock_root slice-unbounded
slice_of "$K" /dolos.slice/dolos-oracle.slice max 1073741824
tier "$K"
expect slice_unbounded "$(run_out "$K")" "REFUSED slice=dolos-oracle.slice memory=max swap=1073741824 run-exit=2 exit=2"
lock_root slice-no-swap
slice_of "$K" /dolos.slice/dolos-oracle.slice 2684354560 max
tier "$K"
expect slice_no_swap "$(run_out "$K")" "REFUSED slice=dolos-oracle.slice memory=2684354560 swap=max run-exit=2 exit=2"

# The number of switched arms judged at once in the oracle slice with the memory limit $1, for ARM_MEMORY $2 or its default when $2 is empty.
arm_jobs_of() {
  J=$W/arm-jobs-$1
  slice_of "$J" /dolos.slice/dolos-oracle.slice "$1" 1073741824
  (
    ROOT=$J CGROUP_FILE=$J/cgroup CGROUP_ROOT=$J/cgroup-fs
    unset ARM_MEMORY
    [ -z "$2" ] || ARM_MEMORY=$2
    . "$LIB"
    arm_jobs
  )
}
# Must not fire: a slice that holds several judges runs that many at once.
expect arm_jobs_fit "$(arm_jobs_of 8589934592 1073741824)" 8
expect arm_jobs_floor "$(arm_jobs_of 3221225471 1073741824)" 2
# Must fire: a slice smaller than one judge still runs one, and the box's slice runs fewer judges than the four it was killed at.
expect arm_jobs_one "$(arm_jobs_of 536870912 1073741824)" 1
expect arm_jobs_box "$(arm_jobs_of 2684354560 '')" 1

# The arguments the docker of the box scripts hands the docker it fronts for the arguments given, or its refusal and exit.
D=$W/docker
mkdir -p "$D"
printf '%s\n' '#!/bin/sh' 'echo "$*"' >"$D/docker"
chmod +x "$D/docker"
docker_of() {
  status=0
  ORACLE_DOCKER=$D/docker ORACLE_SLICE=dolos-oracle.slice sh "$HERE/bin/docker" "$@" 2>&1 || status=$?
  echo "exit=$status"
}
# Must not fire on a command that starts no container.
expect docker_inspect "$(docker_of inspect -f x n | paste -sd ' ' -)" "inspect -f x n exit=0"
expect docker_exec "$(docker_of exec n cardano-cli | paste -sd ' ' -)" "exec n cardano-cli exit=0"
# Must place each container a run or a create starts in the slice.
expect docker_run "$(docker_of run --rm -i image cmd | paste -sd ' ' -)" "run --cgroup-parent dolos-oracle.slice --rm -i image cmd exit=0"
expect docker_create "$(docker_of create image | paste -sd ' ' -)" "create --cgroup-parent dolos-oracle.slice image exit=0"
expect docker_container_run "$(docker_of container run -d image | paste -sd ' ' -)" "container run --cgroup-parent dolos-oracle.slice -d image exit=0"
# Must place the container of each step a build runs in the slice.
expect docker_build "$(docker_of build -t name dir | paste -sd ' ' -)" "build --cgroup-parent dolos-oracle.slice -t name dir exit=0"
expect docker_image_build "$(docker_of image build -t name dir | paste -sd ' ' -)" "image build --cgroup-parent dolos-oracle.slice -t name dir exit=0"
expect docker_builder_build "$(docker_of builder build -t name dir | paste -sd ' ' -)" "builder build --cgroup-parent dolos-oracle.slice -t name dir exit=0"
# Must refuse an option before the command, which could name a run it does not see.
expect docker_option "$(docker_of --host x run image | paste -sd ' ' -)" "REFUSED docker option --host before the command exit=2"
# Must resolve to itself on the PATH lib.sh exports, so a bare docker in a box script or the harness it starts reaches it.
expect docker_path "$(
  ROOT=$W/docker-path
  . "$LIB"
  command -v docker
)" "$HERE/bin/docker"

# Commits to the git repo $W/harness-src image scripts whose build-image.sh prints NAME:$1@DOCKERFILE for the image dir NAME of the images dir it is handed, and prints the commit.
harness_rule() {
  H=$W/harness-src
  [ -d "$H/.git" ] || git init -q "$H"
  mkdir -p "$H/conformance-value-oracle/gen"
  printf '#!/bin/sh\necho "$1:%s@$(cat "$2/$1/Dockerfile")"\n' "$1" >"$H/conformance-value-oracle/gen/build-image.sh"
  printf '#!/bin/sh\necho "$1:%s"\n' "$1" >"$H/conformance-value-oracle/gen/image-name.sh"
  git -C "$H" add -A
  git -C "$H" -c user.name=test -c user.email=test@example.invalid commit -q -m "$1"
  git -C "$H" rev-parse HEAD
}

# A copy at $W/$1 of the box scripts and overlay diffs whose build image dir holds the Dockerfile line $2 and whose harness.rev names the commit $3, with a docker in its bin that logs each call to docker.log.
image_box() {
  B=$W/$1
  rm -rf "$B"
  mkdir -p "$B/bin" "$B/box/images/dolos-build" "$B/box/value"
  cp "$HERE"/*.sh "$B/box/"
  cp -R "$HERE/overlay" "$B/box/"
  echo "$2" >"$B/box/images/dolos-build/Dockerfile"
  echo "$3" >"$B/box/value/harness.rev"
  printf '%s\n' '#!/bin/sh' "echo \"\$*\" >>'$B/docker.log'" >"$B/bin/docker"
  chmod +x "$B/bin/docker"
}

# The image in-image.sh of the copy $1 runs a command in.
run_image() {
  : >"$1/docker.log"
  PATH=$1/bin:$PATH ROOT=$W sh "$1/box/in-image.sh" tree true 2>/dev/null || echo "in-image exit=$?"
  sed -n 's|.* -w /w/tree \([^ ]*\) true$|\1|p' "$1/docker.log"
}

# The name build_image of the copy $1 prints for the build image, its build root under the copy and its harness fetched from $2.
build_name() {
  (
    ROOT=$1/root HERE=$1/box PATH=$1/bin:$PATH VALUE_HARNESS_URL=$2
    . "$1/box/lib.sh"
    build_image dolos-build 2>/dev/null || echo "build_image exit=$?"
  )
}

rule1=$(harness_rule rule1)
image_box image-a a "$rule1"
# Must name and build the build image by the image scripts of the harness at value/harness.rev, over this box's images dir.
expect image_rule "$(build_name "$W/image-a" "$W/harness-src")" "dolos-build:rule1@a"
# Must fire: a harness commit whose rule differs names another image.
echo "$(harness_rule rule2)" >"$W/image-a/box/value/harness.rev"
expect image_rule_changed "$(build_name "$W/image-a" "$W/harness-src")" "dolos-build:rule2@a"
# Must not fire: a harness commit the build root holds is not fetched again.
expect image_rule_held "$(build_name "$W/image-a" "$W/missing")" "dolos-build:rule2@a"
# Must run the image BUILD_IMAGE names, and must start no container when none is named.
expect image_run_named "$(BUILD_IMAGE=named:1 run_image "$W/image-a")" named:1
expect image_run_unnamed "$(run_image "$W/image-a" | sed 's/exit=[0-9]*/exit=N/') calls=$(grep -c . "$W/image-a/docker.log")" "in-image exit=N calls=0"

# Commits to the git repo $W/gen-src a harness whose generator is named gen-$1 and whose build-generator.sh logs each call to $W/gen.log and makes that generator unless $2 is fail, and prints the commit.
harness_gen() {
  H=$W/gen-src
  G=$H/conformance-value-oracle/gen
  [ -d "$H/.git" ] || git init -q "$H"
  mkdir -p "$G"
  printf '#!/bin/sh\necho "$1:img"\n' >"$G/build-image.sh"
  printf '#!/bin/sh\necho gen-%s\n' "$1" >"$G/generator-name.sh"
  cat >"$G/build-generator.sh" <<EOF
#!/bin/sh
echo "build work=\$1 out=\$2" >>"$W/gen.log"
[ "$2" != fail ] || exit 1
mkdir -p "\$2"
printf '#!/bin/sh\n' >"\$2/gen-$1"
chmod +x "\$2/gen-$1"
echo "\$2/gen-$1"
EOF
  git -C "$H" add -A
  git -C "$H" -c user.name=test -c user.email=test@example.invalid commit -q -m "$1"
  git -C "$H" rev-parse HEAD
}

# What value_harness of the build root $W/gen-root prints for the harness commit $1 and the generator dir $W/gen-dir, and each build call it made.
gen_of() {
  B=$W/gen-box
  rm -rf "$B" "$W/gen-root/value/gen"
  mkdir -p "$B/value" "$W/gen-root/value"
  cp "$HERE"/*.sh "$B/"
  echo "$1" >"$B/value/harness.rev"
  : >"$W/gen.log"
  (
    ROOT=$W/gen-root OUT=$W/gen-root HERE=$B VALUE_HARNESS_URL=$W/gen-src VALUE_GEN_DIR=$W/gen-dir
    . "$B/lib.sh"
    . "$B/value.sh"
    if rev=$(value_harness 2>"$W/gen.err"); then
      echo "rev=$([ "$rev" = "$1" ] && echo harness) bin=$(cat "$ROOT/value/gen/bin/conformance-gen" 2>/dev/null | wc -c | tr -dc 0-9)"
    else
      echo "value_harness exit=$? $(grep -o 'VALUE REFUSED [a-z_]*=[a-z]*' "$W/gen.err")"
    fi
  )
  sed "s|$W/||g" "$W/gen.log"
}

# Must not fire: a generator the generator dir holds under the harness's name is used and not built.
gen1=$(harness_gen g1 ok)
mkdir -p "$W/gen-dir"
printf '#!/bin/sh\n# held\n' >"$W/gen-dir/gen-g1"
chmod +x "$W/gen-dir/gen-g1"
expect gen_held "$(gen_of "$gen1")" "rev=harness bin=17"
# Must fire: a generator the dir lacks is built into the dir, with a work dir the build root keeps across builds.
gen2=$(harness_gen g2 ok)
expect gen_built "$(gen_of "$gen2")" "rev=harness bin=10
build work=gen-root/value/generator-work out=gen-dir"
# Must refuse when the build fails, naming the generator unbuilt.
gen3=$(harness_gen g3 fail)
expect gen_unbuilt "$(gen_of "$gen3")" "value_harness exit=1 VALUE REFUSED generator=unbuilt
build work=gen-root/value/generator-work out=gen-dir"

# Writes a docker at $W/node-cli/docker that answers inspect of a container with the image node:1, the user $1 and the environment A=1 and B=two words, and logs every other call and each line of the env file it is handed.
node_docker() {
  D=$W/node-cli
  rm -rf "$D"
  mkdir -p "$D"
  cat >"$D/docker" <<EOF
#!/bin/sh
case "\$1 \$3" in
  "inspect {{.Config.Image}}") echo node:1 ;;
  "inspect {{.Config.User}}") echo "$1" ;;
  inspect*) printf 'A=1\nB=two words\n' ;;
  *)
    echo "\$*" | sed 's|--env-file [^ ]*|--env-file F|' >>"$D/docker.log"
    prev=
    for a; do
      if [ "\$prev" = --env-file ]; then sed 's/^/env /' "\$a" >>"$D/docker.log"; fi
      prev=\$a
    done
    ;;
esac
EOF
  chmod +x "$D/docker"
}

# The docker calls node-cli.sh makes for a tip query through the docker shim in front of the docker of node_docker.
node_cli_of() {
  : >"$W/node-cli/docker.log"
  PATH=$HERE/bin:$PATH ORACLE_DOCKER=$W/node-cli/docker ORACLE_SLICE=dolos-oracle.slice NODE=n \
    sh "$HERE/node-cli.sh" latest query tip || echo "node-cli exit=$?"
  cat "$W/node-cli/docker.log"
}

# Must not fire: no box script runs a command in another container, whose memory the oracle slice does not bound.
expect node_cli_no_exec "$(cd "$HERE" && grep -l 'docker exec' -- *.sh | grep -v '^lib_test.sh$')" ""
# Must run cardano-cli in a container of the node's image, user, environment and volumes that the shim places in the slice.
node_docker ""
expect node_cli_root "$(node_cli_of)" "run --cgroup-parent dolos-oracle.slice --rm --network none --volumes-from n --env-file F --user 0 --entrypoint cardano-cli node:1 latest query tip
env A=1
env B=two words"
node_docker 1000:1000
expect node_cli_user "$(node_cli_of | head -1)" "run --cgroup-parent dolos-oracle.slice --rm --network none --volumes-from n --env-file F --user 1000:1000 --entrypoint cardano-cli node:1 latest query tip"

# Writes the captured endorser block into a LeiosDb of a layout, a change and the schemas named, then prints node-eb's exit and either same, when it prints the line write_leiosdb.py says it must, or its refusal.
node_eb_of() {
  db=$W/leiosdb/$1.db
  layout=$2
  change=$3
  shift 3
  mkdir -p "$W/leiosdb"
  python3 -I "$HERE/leiosdb/write_leiosdb.py" "$db" "$layout" "$change" "$@" >"$db.want"
  status=0
  python3 -I "$HERE/endorser_fixture.py" node-eb "$db" 100000 600 \
    17c497727d273e8dbd0bc188752ef8a7334f5fac6dc077324424a5410cc77fc0 >"$db.got" 2>"$db.err" || status=$?
  if cmp -s "$db.want" "$db.got"; then echo "exit=$status same"; else echo "exit=$status $(cat "$db.err")"; fi
}

# Must not fire: the same block read from either partition of the rc-2026w42 layout and of the rc-2026w45 layout.
expect leiosdb_rows "$(node_eb_of rows rows none rc-2026w42-imm)" "exit=0 same"
expect leiosdb_rows_volatile "$(node_eb_of rows-vol rows none rc-2026w42-vol)" "exit=0 same"
expect leiosdb_closure "$(node_eb_of closure closure none rc-2026w45-imm)" "exit=0 same"
expect leiosdb_closure_volatile "$(node_eb_of closure-vol closure none rc-2026w45-vol)" "exit=0 same"
# Must fire: a layout neither release writes, both layouts in one file, a transaction not written, a transaction whose bytes changed and a stored body whose bytes changed.
expect leiosdb_neither "$(node_eb_of neither none none consensus-60f44c41)" \
  "exit=1 refused: the leios store holds no known layout, it holds ebs(ebSlot, ebHashBytes, txWritten, txBounds, body, txs)"
expect leiosdb_both "$(node_eb_of both none none rc-2026w42-imm rc-2026w45-imm)" \
  "exit=1 refused: the leios store holds the layouts rows and closure"
expect leiosdb_rows_unfilled "$(node_eb_of rows-unfilled rows unfilled rc-2026w42-imm)" "exit=1 refused: offset 3 unfilled"
expect leiosdb_rows_changed "$(node_eb_of rows-changed rows tx-changed rc-2026w42-imm)" \
  "exit=1 refused: offset 3 bytes do not hash to the body's entry"
expect leiosdb_closure_unfilled "$(node_eb_of closure-unfilled closure unfilled rc-2026w45-imm)" "exit=1 refused: offset 3 unfilled"
expect leiosdb_closure_absent "$(node_eb_of closure-absent closure absent rc-2026w45-imm)" "exit=1 refused: offset 0 unfilled"
expect leiosdb_closure_changed "$(node_eb_of closure-changed closure tx-changed rc-2026w45-imm)" \
  "exit=1 refused: offset 3 bytes do not hash to the body's entry"
expect leiosdb_closure_body_changed "$(node_eb_of closure-body-changed closure body-changed rc-2026w45-imm)" \
  "exit=1 refused: the body of 17c497727d273e8dbd0bc188752ef8a7334f5fac6dc077324424a5410cc77fc0 does not hash to it"

if [ "$failed" -eq 0 ]; then
  echo "LIB-TEST RESULT PASS"
else
  echo "LIB-TEST RESULT FAIL"
  exit 1
fi
