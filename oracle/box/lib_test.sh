#!/bin/sh
# Checks that build_checked refuses a unit cargo reports fresh after a file it reads changed, and passes every other unit, with a runner that prints canned cargo messages, which arms an overlay switches, when two test logs differ, the free space floor, the candidate's fsync state, the IO guard's breach rule and the lock of the tiers.
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

# Runs run.sh's done tier on the build root $1 with a floor no disk meets, so a run past the lock refuses at once, its output and exit in $1/run.out, and with a df that writes to $1/df.seen whether a running process held the lock when the run read free space.
tier() {
  mkdir -p "$1/bin"
  printf '%s\n' '#!/bin/sh' \
    'pid=$(readlink "$ROOT/run.lock" 2>/dev/null) || pid=' \
    'if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then echo lock=running; else echo lock=none; fi >>"$ROOT/df.seen"' \
    "exec $(command -v df) \"\$@\"" >"$1/bin/df"
  chmod +x "$1/bin/df"
  (
    ROOT=$1 OUT=$1/runs/t JUDGED=t MIN_FREE_G=999999 LOCK_POLL_SECONDS=1 PATH=$1/bin:$PATH
    export ROOT OUT JUDGED MIN_FREE_G LOCK_POLL_SECONDS PATH
    status=0
    sh "$HERE/run.sh" done >"$1/run.out" 2>&1 || status=$?
    echo "exit=$status" >>"$1/run.out"
  )
}

# Whether the build root $1 still holds a lock.
lock_left() {
  if [ -L "$1/run.lock" ] || [ -e "$1/run.lock" ]; then echo left; else echo released; fi
}

# Must not fire: with no lock the run goes on to the floor holding a lock of its own, and releases it.
K=$W/lock-free
rm -rf "$K"
mkdir -p "$K"
tier "$K"
expect lock_free "$(sed 's/^REFUSED free=[0-9]*G below 999999G at .*/REFUSED free/' "$K/run.out" | paste -sd ' ' -)" "REFUSED free exit=2"
expect lock_free_held "$(paste -sd ' ' - <"$K/df.seen")" lock=running
expect lock_free_released "$(lock_left "$K")" released

# Must fire: a lock a running process holds makes the run wait, and the run goes on once the lock is released.
K=$W/lock-held
rm -rf "$K"
mkdir -p "$K"
sleep 120 &
holder=$!
ln -sn "$holder" "$K/run.lock"
tier "$K" &
runner=$!
i=0
until grep -q '^WAITING' "$K/run.out" 2>/dev/null || ! kill -0 "$runner" 2>/dev/null || [ "$i" -ge 20 ]; do
  sleep 1
  i=$((i + 1))
done
expect lock_waits "$(paste -sd ' ' - <"$K/run.out")" "WAITING lock_pid=$holder"
rm -f "$K/run.lock"
kill "$holder" 2>/dev/null || true
wait "$runner" || true
expect lock_goes_on "$(sed 's/^REFUSED free=[0-9]*G below 999999G at .*/REFUSED free/' "$K/run.out" | paste -sd ' ' -)" "WAITING lock_pid=$holder REFUSED free exit=2"
expect lock_taken "$(paste -sd ' ' - <"$K/df.seen")" lock=running
expect lock_held_released "$(lock_left "$K")" released

# Must fire: a lock whose process has exited is reported and the run stops before the floor.
K=$W/lock-stale
rm -rf "$K"
mkdir -p "$K"
sh -c 'exit 0' &
dead=$!
wait "$dead"
ln -sn "$dead" "$K/run.lock"
tier "$K"
expect lock_stale "$(paste -sd ' ' - <"$K/run.out")" "REFUSED lock=$K/run.lock lock_pid=$dead stale exit=2"
expect lock_stale_kept "$(lock_left "$K")" left

if [ "$failed" -eq 0 ]; then
  echo "LIB-TEST RESULT PASS"
else
  echo "LIB-TEST RESULT FAIL"
  exit 1
fi
