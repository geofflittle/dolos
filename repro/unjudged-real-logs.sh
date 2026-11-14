#!/bin/sh
# Prints the verdict arm_ran sets over real run logs for a plant none of them reached, with the lib.sh given.
# Usage: unjudged-real-logs.sh <lib.sh> <runs dir>
# Expected at a lib.sh that names UNJUDGED:
#   judge-passed=UNREACHED crate-passed=UNREACHED judge-failed=UNJUDGED
set -eu
LIB=$1
RUNS=$2
HERE=$(dirname "$LIB")
OUT=$(mktemp -d)
ROOT=$OUT
. "$LIB"
line=
for pair in judge-passed:coin:tiers-d1/head.log crate-passed:cost-models:tiers-d1/control-cost-models.log \
  judge-failed:coin:os-d1/archive-outputs-only.log; do
  name=${pair%%:*}
  rest=${pair#*:}
  arm=${rest%%:*}
  log=$RUNS/${rest#*:}
  arm_ran "$log" '^test result: ' never-planted "$(judged_pattern "$arm")" || true
  line="$line $name=$verdict"
done
rm -rf "$OUT"
echo "${line# }"
