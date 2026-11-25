#!/bin/sh
# Prints the first 12 hex digits of the sha256 of the sha256 lines of the files under the given paths, sorted by path.
# Usage: hash-files.sh <path>...
set -eu
find "$@" -type f -exec sha256sum {} + | LC_ALL=C sort -k 2 | sha256sum | cut -c1-12
