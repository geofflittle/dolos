#!/usr/bin/env bash
# Writes the sorted failure lines of a comparison run to a file, and fails
# unless every failing test of the run failed at a comparison's verdict.
# Usage, from reference-comparison: judged-failure-lines.sh <nextest output> <exit code> <lines file>
set -euo pipefail

output=$1
code=$2
lines=$3

# The verdict of every comparison is this assertion over its failure list.
verdict='assert!(failures.is_empty(), "{}", failures.join("\n"));'

refuse() {
  echo "::error::$1"
  exit 1
}

case "$code" in
  0 | 100) ;;
  *) refuse "the run exited $code, which is neither a pass nor a failing test" ;;
esac

plain=$(sed 's/\x1b\[[0-9;]*m//g' "$output")

summary=$(grep -E '^ *Summary \[' <<< "$plain" | tail -n 1 || true)
run=$(sed -nE 's/.* ([0-9]+) tests? run: ([0-9]+) passed.*/\1/p' <<< "$summary")
passed=$(sed -nE 's/.* ([0-9]+) tests? run: ([0-9]+) passed.*/\2/p' <<< "$summary")
if [ -z "$run" ] || [ -z "$passed" ]; then
  refuse "the run printed no test summary"
fi
failing=$((run - passed))

judged=0
while read -r site; do
  file=${site%:*:*}
  line=${site#"$file":}
  line=${line%%:*}
  if ! [ -f "$file" ] || [ "$(sed -n "${line}p" "$file" | sed 's/^ *//')" != "$verdict" ]; then
    refuse "a test panicked at $site, which is not a comparison's verdict"
  fi
  judged=$((judged + 1))
done < <(sed -nE 's/.* panicked at (tests\/[^ ]+:[0-9]+:[0-9]+):$/\1/p' <<< "$plain")

if [ "$code" -eq 0 ] && [ "$failing" -ne 0 ]; then
  refuse "the run exited 0 with $failing failing tests"
fi
if [ "$code" -eq 100 ] && { [ "$failing" -eq 0 ] || [ "$judged" -ne "$failing" ]; }; then
  refuse "$failing tests failed and $judged of them at a comparison's verdict"
fi

awk '/panicked at tests\//{f=1;next} f&&(/^note:/||/^$/||/^test /){f=0} f' "$output" | sort > "$lines"
