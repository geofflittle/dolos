#!/bin/sh
# Compiles the key list probe in the base shape and the branch shape, from /pallas/keys-probe in the pallas-ci image.
# Expected: "base-exit=0" and "KEYS ["peras_cert_boost"]" (the base compiles with new_key unjudged),
# then "complete-exit=0" and the same KEYS line (the branch shape with every field named),
# then "error: pattern requires `..` due to inaccessible fields" and "branch-exit=1" (the branch shape with new_key unnamed).
cd "$(dirname "$0")"
rustc --edition 2021 --cfg base -o /tmp/probe-base probe.rs
echo base-exit=$?
/tmp/probe-base
rustc --edition 2021 --cfg complete -o /tmp/probe-complete probe.rs
echo complete-exit=$?
/tmp/probe-complete
rustc --edition 2021 -o /tmp/probe-branch probe.rs 2>&1 | grep -E "^error"
rustc --edition 2021 -o /tmp/probe-branch probe.rs >/dev/null 2>&1
echo branch-exit=$?
