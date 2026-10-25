# Served block and parameter comparison

`served.py` reads every block a utxorpc v1 server holds (DumpHistory), cuts the node's block, transaction and parameter update bytes out of it, runs the ledger oracle of /opt/build/conformance-gen-1024 over them, and compares each oracle fact with the served value. `params` compares ReadParams with the node's protocol parameter query.

## Runners

- `sh run-plants.sh`, local, needs only python3. Expected lines are in its header.
- `sh run-e2e.sh 127.0.0.1:50061 OUT node-pparams.json 488445 205422 605798` on the box, against a Dolos built from 6cd677ea and synced from the node. The slots are walked in full. `expected-full.txt` is the output of the 2026-10-25 run (27581 blocks, disagreements 0, ReadParams disagree 1 at costModels).
- `FROM=OUT PLANT=tx:body.fee:1000000 sh run-e2e.sh ...` must print `disagreements 1` and `1  served fact  tx.body.fee`, with every other line of the summary unchanged.
- `FROM=OUT PLANT=params:perasCertBoost sh run-e2e.sh ...` must leave the summary unchanged and add `disagree perasCertBoost: served fact, node 0, served 1`.
