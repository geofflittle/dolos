# Conformance value oracle, server side

The build wrappers and test results for this tree at 55035837, used beside the value oracle recorded on the pallas branch `repro/conformance-value-oracle`.

The runners expect /opt/build/value-oracle-1025 on the build host, with the files of `box/` at its root, this tree under `dolos/` and the image `dolos-build:1`.

## Runners and expected verdicts

`sh dolos-run.sh TREE COMMAND...` runs one command in the build image against a tree under the root.

`MINFREE=30 KILLFREE=12 sh dolos-percrate.sh dolos dolos-pc.log PACKAGE...` runs `cargo test --locked -p PACKAGE` one package at a time in one target dir, with debug info off. It does not start a package below MINFREE GB free, kills the build below KILLFREE GB, and deletes the target dir at the end. Per package it writes `PACKAGE p EXIT rc` and `PACKAGE p suites N passed N failed N ignored N`, then `DONE free NG`.

The recorded run over the thirteen workspace packages (`results/dolos/dolos-pc.log`): every package EXIT 0, 71 suites, 2013 passed, 0 failed, 42 ignored. `results/dolos/dolos-pc-suites.txt` holds each suite's result line.

`sh bls-plant-arm.sh dolos PALLAS_TREE TARGET` runs the two pool BLS key serving tests with every pallas crate taken from a local tree. With pallas at `repro/plant-bls-key-decode-flip` the test whose key comes from the same decode passes and the test whose key comes from the node fails at the key equality.

Lints at 55035837: `cargo clippy --locked --all-targets --all-features -- -D warnings` exit 0 (`results/dolos/dolos-55035837-clippy.log`), `cargo fmt --all -- --check` exit 0 (`results/dolos/dolos-55035837-fmt.log`).
