# Musashi Leios devnet sync operations

These run on the box that syncs Dolos from origin against the Musashi Leios devnet, from `/opt/build/run`, where they must stay byte identical to these copies.

`checkpoint.sh` stops the `dolos-sync` container, copies its data directory to `checkpoints/<store tip slot>`, starts it again and logs the tip, the size and how long the sync was down, and `dolos-checkpoint.timer` runs it with `--if-crossed` every five minutes so it acts only once the applied tip has crossed another ten percent of the chain, keeping the newest four and skipping when under 15 GB would remain free.

`rewind.sh <tip-slot>` puts one of those checkpoints back, keeping the store it replaced as `data.broken.<timestamp>`, and it copies rather than moves so the same checkpoint can be used twice.

`status.py` reports progress as three separately labelled slots, because the pull stage's slot, the apply stage's lower bound and the store's own tip are different numbers and reading one as another has misled a diagnosis.
