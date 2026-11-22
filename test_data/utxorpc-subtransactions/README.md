# Subtransaction inputs over UTxORPC v1

Dolos prepares one input-resolution cache per block response. That cache must
include each child's spending and reference inputs, even when no top-level
transaction mentions them. Otherwise Pallas can map the child's input reference
but cannot fill `as_output`, and sundae-sync-v2's input-side address and token
filters can miss the enclosing transaction.

## Evidence and boundaries

The unchanged `../musashi-w36/ranking-spends-sub-transaction-output.block` consumes
an output from the unchanged `ranking-sub-transaction-two-outputs.block` in that
directory. The regression maps that captured output under its own child hash and
compares cached and uncached v1 mapping. Its parent's reference input duplicates
the child's spend, so **this captured pair passes before and after the fix**.
It is compatibility evidence, not reproduction of the omission.

`src/tests/musashi.rs::child_only_inputs` explicitly constructs a **synthetic**
serving case from `ranking-sub-transaction.block`: it retains only the last parent,
removes that parent's reference inputs, and selects the parent success flag. It
preserves the original child body and builds synthetic resolved outputs whose
address and token policy distinguish child inputs from parent inputs and created
outputs. The resulting block is not a valid ledger block: header commitments and
signatures are not recomputed. These tests exercise response mapping, never
submission or consensus validation. Captured files are never rewritten.

The same child-only assertion is run against baseline and fixed production code,
using separate build directories. Additional checks cover child reference inputs,
invalid-parent verdicts, shared dependencies, unresolved inputs and cache refresh.
The existing ordinary-transaction tests retain v1alpha/v1beta and bulk-failure
coverage.

The paired test starts the real Dolos v1 sync service on an ephemeral loopback
port using isolated in-memory state/archive stores. It invokes the actual
sundae-sync-v2 `musashi` test executable as a separate process. That consumer
checks FetchBlock, DumpHistory, FollowTip Apply/Undo, child hashes and resolved
inputs, and its real address/token filters. Removing child input enrichment must
turn both filter matches off. Apply/Undo notifications are injected by the test;
this does not replay ledger rollback. Existing sundae-sync archive tests cover
child lookup rows and their rollback with mocked AWS transport.

A read-only sample of the last 100 transactions in each of 50 recent local
Musashi blocks decoded 2,462 transactions and found no subtransactions. The older
captures also repeat child dependencies at the parent. **An unmodified network
capture reproducing child-only dependency loss remains unavailable.** See
`evidence/network-sample.json`. This is not full live-network or AWS deployment
verification and does not establish full native Dijkstra validation.

## Reproduction

From Dolos, the ordinary regression is:

```sh
cargo test --locked -p dolos --lib serve::grpc::block_context::tests
python3 test_data/utxorpc-subtransactions/verify.py
```

To reproduce the baseline, apply `evidence/baseline-tests.patch` with
`git apply --unidiff-zero` in a separate
checkout of `16e112d7a9864bfa44c93b2a84227456908ccacb` and run the same test
command with its own `CARGO_TARGET_DIR`. The patch adds only the tests and their
helper, leaving production resolution unchanged. The child-only test fails with
`as_output: None`; the captured-pair test passes. The same consumer executable
also fails against that baseline server and passes against the fixed server.

The paired consumer test is no longer in Dolos, because its consumer target
`dolos_subtransactions` is on no published sundae-sync-v2 branch. The
`evidence/consumer-*.txt` files are its recorded runs.

Exact dependency revisions, decoded-byte checksums and verification results are
recorded in `provenance.json` and `verification.json`. No dependency upgrade is
required: both repositories already use the same fork's v1 wire schema.
