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

The done tier, while the live leg builds the commit, syncs it from genesis against the node and compares what it serves with the serving Dolos and the node.

Run it once per dolos-serve rollout, on the commit being pinned, on a box that reaches the node (musashi-aws). It is the only tier that reads the live node.

Takes 82 to 101 minutes, nearly all of it the sync (measured on musashi-aws).

## The value and amaru legs

A tier runs a leg only when its inputs (the pinned pallas, its box files and its rows of `box/served.txt`) differ from those of its last passing run. When it does, the value leg adds about 1.5 minutes when the box has judged the pinned pallas before (91 s, measured on builder-1), and about 53 minutes on the box's first run, which builds its images (3149 s, measured on builder-1). The amaru leg adds about half a minute (35 s, and 80 s on the first run, measured on builder-1).

## Manifests

`oracle/box/regen.sh` writes the manifest lines the corpus judge and the live judge propose into `$ROOT/regen` and prints how each differs from the committed file. It syncs a candidate from genesis as ship does, so it runs on musashi-aws and takes about as long (estimated).
