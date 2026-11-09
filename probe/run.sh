#!/bin/sh
# Prints the builtins each Plutus cost model of the registration parameters leaves without a parameter, from the ledger API parameter names in order.
# Usage: sh probe/run.sh, from the root of a Dolos tree.
# Expected: V1 332, V2 332 and V3 350 leave only the five builtins past protocol 12 unpriced, and V4 251 leaves 31, which are those five and 26 of the 101 builtins valid at protocol 12.
# V4 is read with the V3 names, since plutus-ledger-api 1.71 V4/ParamName.hs lists the V3 constructors in the same order.
set -eu
PARAMS=test_data/musashi-native-submission/registration-epoch64-parameters.json
HERE=$(dirname "$0")
for language in V1 V2 V3 V4; do
  entries=$(jq ".cost_models_raw.Plutus$language|length" "$PARAMS")
  names=$language
  if [ "$language" = V4 ]; then names=V3; fi
  echo "LANGUAGE $language"
  python3 -I "$HERE/v4priced.py" "$HERE/names/$names.names" "$entries"
done
