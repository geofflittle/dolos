#!/bin/sh
# Builds the reference writer of writer/ at the cardano-ledger commit REV into OUTDIR, named for the hash of its sources, the commit and the image, and prints its path.
# Usage: [JOBS=n] [MEMORY=4g] build-writer.sh <rev> <work dir> <out dir>
# The work dir holds the cardano-ledger checkout and the cabal store, the package store hs/build.sh compiles against.
# writer/pins/REV.project, when present, adds to the build's project settings for that commit.
set -eu
O=$(cd "$(dirname "$0")" && pwd)
REV=$1
WORK=$(mkdir -p "$2" && cd "$2" && pwd)
OUTDIR=$(mkdir -p "$3" && cd "$3" && pwd)
IMAGE=${IMAGE:-$(sh "$O/image.sh")}
sources=$(cd "$O" && find writer build-writer.sh -type f -exec sha256sum {} + | LC_ALL=C sort -k 2 | sha256sum | cut -c1-12)
name=reference-writer-$sources-$(echo "$REV" | cut -c1-12)-${IMAGE#*:}
if [ -x "$OUTDIR/$name" ]; then
  echo "$OUTDIR/$name"
  exit 0
fi
ledger=$WORK/cardano-ledger
if [ ! -d "$ledger/.git" ]; then
  git init -q "$ledger"
fi
git -C "$ledger" fetch -q --depth 1 https://github.com/IntersectMBO/cardano-ledger "$REV"
git -C "$ledger" checkout -q -f "$REV"
rm -rf "$WORK/writer"
cp -R "$O/writer" "$WORK/writer"
cat >"$WORK/writer/Build.hs" <<EOF
-- | The identity of this build of the writer.
module Build (pin, name) where

-- | The cardano-ledger commit the writer is built at.
pin :: String
pin = "$REV"

-- | The writer's name, which the records it writes carry.
name :: String
name = "$name"
EOF
echo 'packages: /work/writer' >"$ledger/cabal.project.local"
if [ -f "$O/writer/pins/$REV.project" ]; then
  cat "$O/writer/pins/$REV.project" >>"$ledger/cabal.project.local"
fi
mkdir -p "$WORK/cabal-dir"
rm -f "$WORK/reference-writer.built"
docker run --rm --log-driver none --memory "${MEMORY:-4g}" --memory-swap "${MEMORY:-4g}" --user "$(id -u):$(id -g)" -v "$WORK:/work" -e CABAL_DIR=/work/cabal-dir -e HOME=/work -w /work/cardano-ledger "$IMAGE" sh -c "cabal update && cabal build -j${JOBS:-2} reference-writer && cp \"\$(cabal list-bin reference-writer)\" /work/reference-writer.built" >&2
mv "$WORK/reference-writer.built" "$OUTDIR/$name"
echo "$OUTDIR/$name"
