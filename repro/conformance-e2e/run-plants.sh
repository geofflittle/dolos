#!/bin/sh
# Runs the unit cases on the clean comparison and on each planted break, and prints one verdict line each.
# usage: run-plants.sh
# expected:
#   clean: cases 9, failed 0
#   rational: FAIL a_rational_cut_to_32_bits_is_its_own_class
#   rebuild: FAIL a_body_longer_than_the_header_says_is_rebuilt_from_its_tail
#   optional: FAIL an_unset_optional_param_agrees_only_with_a_node_null
set -eu
D=$(cd "$(dirname "$0")" && pwd)
echo "clean: $(python3 -I "$D/test_served.py" | tail -1)"
for p in rational rebuild optional; do
  T=$(mktemp -d)
  cp "$D/served.py" "$D/test_served.py" "$T/"
  (cd "$T" && patch -s -p1 < "$D/plants/$p.patch")
  echo "$p: $(python3 -I "$T/test_served.py" | /usr/bin/grep '^FAIL' | cut -d' ' -f1-2)"
  rm -rf "$T"
done
