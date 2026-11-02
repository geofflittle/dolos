#!/bin/sh
# Checks that build_checked refuses a unit cargo reports fresh after a file it reads changed, and passes every other unit, with a runner that prints canned cargo messages.
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

if [ "$failed" -eq 0 ]; then
  echo "LIB-TEST RESULT PASS"
else
  echo "LIB-TEST RESULT FAIL"
  exit 1
fi
