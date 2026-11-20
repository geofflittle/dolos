# The Dolos oracle

The ledger judge, `serve::grpc::v1::ledger_oracle_tests::ledger_oracle`, compares the answers of Dolos with those of the Haskell ledger on the corpora in `test_data/ledger-oracle`. It is a medium test that the `judge` job of `.github/workflows/docker.yml` runs in release on every push, its self tests run with the other tests of their size, and the image is built only when that job and the test jobs pass. `cargo nextest run --release -p dolos --lib --no-capture -E 'test(=serve::grpc::v1::ledger_oracle_tests::ledger_oracle)'` runs the judge alone, and each manifest line it proposes is printed after `CANDIDATE` and the name of its file.

`hs` holds the Haskell programs that write the corpora, and `probe` holds Python readers of a corpus that share no code with the judge.

`writer` is the reference writer, which writes the records the pallas reference comparison reads: generated Dijkstra values with the ledger's reading of each, at one ledger commit. `build-writer.sh REV WORK OUTDIR` builds it at the commit REV, adding `writer/pins/REV.project` to the build's project settings when that file exists, and `image.sh` names the image every program here compiles and runs in.
