#!/bin/sh
# Syncs the image IMAGE as a candidate beside the serving Dolos, reads the candidate, the serving Dolos and the node into $OUT/live, and records there the image, the commit its revision label names and the candidate's verdict, which the ship tier judges.
# Usage: ROOT=dir IMAGE=<repository>@sha256:<64 hex digits> read-live.sh
# Runs where the node is reachable, as the commit the label names it, and builds nothing.
set -eu
: "${ROOT:?}" "${IMAGE:?}"
HERE=$(cd "$(dirname "$0")" && pwd)
# The oracle slice's memory and swap on musashi-aws, the box this script runs on.
ORACLE_MEMORY=${ORACLE_MEMORY:-2400M}
ORACLE_SWAP=${ORACLE_SWAP:-512M}
. "$HERE/lib.sh"
trap finish EXIT
if [ -z "${JUDGED:-}" ]; then
  REV=$(image_commit "$IMAGE")
fi
start_run "$0" "$@"

mkdir -p "$OUT/live"
candidate_fsync >/dev/null || exit 2
commit=$REV
status=0
sync_candidate "$IMAGE" || status=$?
candidate=$(candidate_verdict "$status" "$OUT/live/candidate.log")
echo "image=$IMAGE commit=$commit candidate=$candidate" >"$OUT/live/reads.txt"
echo "READS image=$IMAGE commit=$commit candidate=$candidate dir=$OUT/live"
case $candidate in
  PASS) ;;
  REFUSED) exit 2 ;;
  *) exit 1 ;;
esac
