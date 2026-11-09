# Mutation judge selection verdicts

Each file is a whole copy of `crates/ledger-oracle-judge/src/mutation.rs` that
`repro.sh` mounts over the tree's copy before running
`cargo test -p ledger-oracle-judge --lib mutation::` in `dolos-build:1`.

Run on the box from a copy of this tree at `/opt/build/<dir>/dolos`, with the
variants in `/opt/build/<dir>/repro` and `D` in `repro.sh` set to that dir:

    sh repro.sh selection-red selection-adds-all selection-on-mutant selection-fixed

Expected lines, measured 2026-11-09 on this branch's parent:

| variant | what it breaks | expected |
| --- | --- | --- |
| selection-red | `select` adds nothing and `judge` keeps selection verdicts | `test result: FAILED. 7 passed; 2 failed`, `repro selection-red exit=101` |
| selection-adds-all | `select` adds record verdicts too | `test result: FAILED. 8 passed; 1 failed`, panics in `the_corpus_adds_its_selection_failures_and_not_its_record_failures` |
| selection-on-mutant | `judge` keeps selection verdicts on the mutant | `test result: FAILED. 8 passed; 1 failed`, panics in `a_selection_verdict_on_a_mutant_run_is_not_a_verdict_on_the_mutant` |
| selection-fixed | nothing | `test result: ok. 9 passed; 0 failed`, `repro selection-fixed exit=0` |

The defect it answers: before the fix, each mutant run judged one record, so
the `judged=0` selection failure of a judge that saw no record of its kind
counted as a disagreement on that mutant (1387 mutants on the first base run).
