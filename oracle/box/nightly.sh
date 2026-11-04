#!/bin/sh
# Fetches the Sundae Dolos leios-musashi branch into the build root and runs the ship tier on it into runs/nightly-<UTC date>.
# Usage: ROOT=dir nightly.sh, from the box crontab line
# 0 2 * * * ROOT=/opt/build/ledger-agreement-1028 sh /opt/build/ledger-agreement-1028/nightly/oracle/box/nightly.sh >>/opt/build/ledger-agreement-1028/runs/nightly.log 2>&1
set -eu
: "${ROOT:?}"
HERE=$(cd "$(dirname "$0")" && pwd)
URL=${NIGHTLY_URL:-https://github.com/SundaeSwap-finance/dolos.git}
git -C "$ROOT/dolos.git" fetch -q "$URL" +refs/heads/leios-musashi:refs/heads/leios-musashi
REV=leios-musashi
OUT="$ROOT/runs/nightly-$(date -u +%Y%m%d)"
export ROOT REV OUT
exec sh "$HERE/run.sh" ship
