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

Run it once per dolos-serve rollout, on the commit whose image is being pinned, on builder-1, the builder box that runs a Musashi node, and never on musashi-aws:

```
ssh builder-1 oracle-run ship <name> <commit> https://github.com/SundaeSwap-finance/dolos.git
```

`oracle-run` starts the node, runs the tier in a 20 GiB slice and stops the node when the tier ends, as the musashi-node README describes. The tier pulls the image the image workflow tagged `sha-` and the commit's first seven hex digits, takes its digest, and refuses it unless its `org.opencontainers.image.revision` label names the commit. It then waits up to `NODE_WAIT_MINUTES` (60) for the node's tip to come within `IO_NODE_LAG_S` of the wall clock, syncs the image from genesis against the node as the run's own user, and judges the candidate's reads against the serving Dolos beside the done tier's arms. It reads `NODE`, `NODE_DB`, `ENDPOINT`, `CANDIDATE_GRPC` and `CANDIDATE_NEED_G` from `$ROOT/live.env`, and `SERVE_CONFIG`, the Dolos config rendered for the node, from `oracle-run`. The candidate refuses to start unless `$ROOT` has `CANDIDATE_NEED_G` and 3 GiB more free. The sync stops at half again the time the node's tip takes at `SYNC_SLOTS_PER_S`.

The sync from genesis reached slot 2346838 in about 85 minutes (5124 s, measured on builder-1), and the whole tier has no measured time yet.

The tier records the commit in `runs/last-done` and `runs/last-ship`, and the image in `runs/last-ship`.

## The value and amaru legs

A tier runs a leg only when its inputs (the pinned pallas, its box files and its rows of `box/served.txt`) differ from those of its last passing run. When it does, the value leg adds about 1.5 minutes when the box has judged the pinned pallas before (91 s, measured on builder-1), and about 53 minutes on the box's first run, which builds its images (3149 s, measured on builder-1). The amaru leg adds about half a minute (35 s, and 80 s on the first run, measured on builder-1).

## Manifests

`oracle/box/regen.sh` writes the manifest lines the corpus judge and the live judge propose into `$ROOT/regen` and prints how each differs from the committed file. It builds the commit and syncs it from genesis against the node, so it runs on builder-1 with the node started and `SERVE_CONFIG` set as `oracle-run ship` sets them, and takes about as long as the ship tier's sync (estimated, not run on builder-1).
