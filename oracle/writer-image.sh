#!/bin/sh
# Prints, as name=value lines, the published images of the reference writer at the cardano-ledger commit REV and of the toolchain it builds on, and the writer's name.
# The writer is tagged with the hash of its sources and of writer/pins/REV.project when present, the commit, and the toolchain's hash.
# Usage: writer-image.sh <rev>
set -eu
O=$(cd "$(dirname "$0")" && pwd)
REV=$1
pin=writer/pins/$REV.project
if [ ! -f "$O/$pin" ]; then
  pin=
fi
sources=$(cd "$O" && sh hash-files.sh $(find writer -path writer/pins -prune -o -type f -print) $pin)
toolchain=$(cd "$O/image" && sh "$O/hash-files.sh" .)
tag=$sources-$(echo "$REV" | cut -c1-12)-$toolchain
echo "toolchain=ghcr.io/geofflittle/ledger-build:$toolchain"
echo "image=ghcr.io/geofflittle/reference-writer:$tag"
echo "name=reference-writer-$tag"
