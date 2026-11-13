# Musashi Leios devnet comparisons

These compare a Dolos that follows the Musashi Leios devnet with the node it follows, and check the log of a workspace test run.

`compare-utxo-set.py` takes no arguments, reads the node's whole utxo set through `guarded-utxo.sh`, which kills a dump that would press the box, and asks the follower for every one of those keys over UtxoRPC in batches, and prints one `UTXO` count line, a line per missing or differing key, and exits non-zero if anything differs or if the follower never came level. Set `EXTRA_KEYS` to a file of `txid#index` lines to also check the other direction, which no enumeration of the follower can reach.

`compare-params.py` takes no arguments, reads the node's protocol parameters with `cardano-cli` and the follower's over UtxoRPC, and prints one line per parameter ending IDENTICAL, DIFFERS or REPORTED, a `PARAMS` count line, and exits non-zero if anything differs.

`probe/run-probe.sh` takes no arguments and talks to no node, and runs both comparisons six times over fixtures, once over a ledger that agrees and once each over a ledger with a known parameter fault, a known utxo fault, a reply that names a key without the utxo under it and a follower holding a utxo the node does not, checking both the exit and the line each one prints, and its expected last line is `PROBE 6 arms, 6 as expected`.

`ignored-inventory.py <cargo test log>` names every test binary in that log which passes no test, either because it ignores all of them or because it holds none, and refuses one that `test_data/all-ignored-binaries.toml` does not record with a reason, a recorded entry that now passes a test, a log naming no binary, a log holding a failure, and a log naming one target twice. Its count line reads `INVENTORY <n> binaries, <a> all ignored, <e> empty, <u> unrecorded, <s> stale`. Run it on the log of a whole workspace test run, because a summed count line over 65 binaries hides the ones whose green asserted nothing.

`probe/run-inventory-probe.sh` takes an optional tree root and runs that check over six logs under `test_data/inventory-arms`, one it must accept and five it must refuse, and its expected last line is `INVENTORY PROBE 6 arms, 6 as expected`. `tests/musashi_harness_arms.rs` runs it and `probe/run-probe.sh` under `cargo test`, so a harness that stopped refusing fails the suite.
