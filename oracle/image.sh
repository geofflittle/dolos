#!/bin/sh
# Builds the image of image/, the Haskell toolchain the oracle programs compile and run in, when docker has none of its name, and prints the name.
# The name is tagged with the hash of the files of image/.
# Usage: image.sh
set -eu
dir=$(cd "$(dirname "$0")/image" && pwd)
hash=$(cd "$dir" && find . -type f -exec sha256sum {} + | LC_ALL=C sort -k 2 | sha256sum | cut -c1-12)
name=ledger-build:$hash
if ! docker image inspect "$name" >/dev/null 2>&1; then
  docker build --memory 4g --memory-swap 4g -t "$name" "$dir" >&2
fi
echo "$name"
