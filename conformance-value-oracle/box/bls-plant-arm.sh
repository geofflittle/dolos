#!/bin/sh
# Runs the two pool BLS key serving tests of a Dolos tree with every pallas crate taken from a local pallas tree.
# usage: [W=dir] bls-plant-arm.sh DOLOS_TREE PALLAS_TREE TARGET
# The pallas tree at repro/plant-bls-key-decode-flip flips the last byte of every decoded BLS public key.
# Expected: the test whose key comes from the same decode passes, the test whose key comes from the node fails at the key equality.
W=${W:-/opt/build/value-oracle-1025}
DOLOS=$1
PALLAS=$2
TARGET=$3
SRC='https://github.com/geofflittle/pallas'
set --
for c in pallas pallas-addresses pallas-codec pallas-configs pallas-crypto pallas-hardano pallas-network pallas-network2 pallas-primitives pallas-traverse pallas-txbuilder pallas-utxorpc pallas-validate; do
  set -- "$@" --config "patch.\"$SRC\".$c.path=\"/w/$PALLAS/$c\"" --config "patch.crates-io.$c.path=\"/w/$PALLAS/$c\""
done
TARGET=$TARGET exec sh "$W/dolos-run.sh" "$DOLOS" cargo test -p dolos --lib "$@" a_pool_registered
