# The Dolos oracle

`oracle/box/run.sh` judges a Dolos commit in one of three tiers. Each tier judges the commit (the head) on the oracle's corpora and judges planted defects (the arms in `box/arms.toml`), each of which must be caught. Each tier also runs the value leg (`box/value.sh`, the pallas conformance harness) and the amaru leg (`box/amaru.sh`, the amaru-uplc conformance test) when their inputs differ from those of their last passing run.

On a builder box, `oracle-run <tier> <run name> <full commit id>` runs a tier, as the musashi-node README describes.

## quick

Checks the head, the control (the head on every arm's records) and the arms whose plants touch a file the change alters since the last passing done or ship run.

Run it on every iteration of a change, on a builder box.

Takes about 3 minutes when the change touches no arm (164 s, measured on builder-1), and about 8 when no done or ship run has passed on the box, since every arm runs then (502 s, measured on builder-1).

## done

Checks the head, the control and every arm, and fails on a fix commit that `box/arms.toml` gives neither an arm nor an exclusion.

Run it once per finished change, before the push, on a builder box.

Takes about 4 to 5 minutes (249 s and 302 s, measured on builder-1).

## ship

The done tier, and the live judge over reads of the image being pinned, which the image's sync from genesis against the node took beside the serving Dolos.

Run it once per dolos-serve rollout, on the image being pinned, in two steps.

On musashi-aws, the box that reaches the node, with the image's digest from the image workflow:

```
git -C /opt/build/ship/dolos.git fetch -q https://github.com/SundaeSwap-finance/dolos.git leios-musashi
mkdir -p /opt/build/ship/work/launch
git -C /opt/build/ship/dolos.git archive FETCH_HEAD oracle/box | tar -x -C /opt/build/ship/work/launch
ROOT=/opt/build/ship OUT=/opt/build/ship/runs/<name> IMAGE=<repository>@sha256:<digest> sh /opt/build/ship/work/launch/oracle/box/read-live.sh
```

`read-live.sh` pulls the image, takes the commit from its `org.opencontainers.image.revision` label, runs that commit's `read-live.sh`, syncs the image beside the serving Dolos and writes the candidate's reads, the serving Dolos's reads and a `reads.txt` naming the image, the commit and the candidate's verdict into `$OUT/live`. It builds nothing, and reads `NODE`, `NODE_DB`, `ENDPOINT`, `SERVE_CONFIG`, `CANDIDATE_GRPC` and `GRPCURL` from `$ROOT/live.env`. It refuses an image named by a tag, a label that is not a full commit id, a commit `$ROOT/dolos.git` lacks, and a commit whose `oracle/box` has no `read-live.sh`. It bounds the run's slice at 2400M of memory and 512M of swap unless `ORACLE_MEMORY` and `ORACLE_SWAP` say otherwise, about three quarters of the memory musashi-aws has available beside the node and the serving Dolos (3299 MB available, measured). Every container an oracle script starts is the kernel's first choice when the box runs out of memory, so a candidate that outgrows the box is killed before the node. The candidate refuses to start unless `$ROOT` has 3 GiB more free than the serving Dolos's store holds, and `reads.txt` then names its verdict `REFUSED` and the run exits 2. The sync stops at half again the time the node's tip takes at `SYNC_SLOTS_PER_S`. Nearly all of its time is the sync, which reached slot 1441438 of a tip near slot 2349000 in 67 minutes (4006 s, measured on musashi-aws), so a whole sync takes over two hours (estimated).

Then copy the reads to a builder box and run the tier there on the commit `reads.txt` names, which it refuses reads of any other commit for, as it refuses reads whose candidate refused:

```
ssh musashi-aws tar -C /opt/build/ship/runs/<name> -c live | ssh builder-1 'mkdir -p /opt/oracle/reads/<name> && tar -x -C /opt/oracle/reads/<name>'
ssh builder-1 LIVE_READS=/opt/oracle/reads/<name>/live oracle-run ship <name> <commit> https://github.com/SundaeSwap-finance/dolos.git
```

The tier records the commit in `runs/last-done` and `runs/last-ship`, and the image in `runs/last-ship`. It takes about as long as the done tier (estimated).

## The value and amaru legs

A tier runs a leg only when its inputs (the pinned pallas, its box files and its rows of `box/served.txt`) differ from those of its last passing run. When it does, the value leg adds about 1.5 minutes when the box has judged the pinned pallas before (91 s, measured on builder-1), and about 53 minutes on the box's first run, which builds its images (3149 s, measured on builder-1). The amaru leg adds about half a minute (35 s, and 80 s on the first run, measured on builder-1).

## Manifests

`oracle/box/regen.sh` writes the manifest lines the corpus judge and the live judge propose into `$ROOT/regen` and prints how each differs from the committed file. It builds the commit and syncs it from genesis against the node, so it runs on musashi-aws and takes about as long as `read-live.sh` (estimated).
