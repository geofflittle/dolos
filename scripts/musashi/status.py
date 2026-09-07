"""Progress of the background Leios aware sync.

Reads the container's own log, which the json file log driver writes
continuously, so this works while the sync is running rather than only after it
stops.

Every number below is either read from the log or derived from a number that
was. Where a number cannot be had, the line says so instead of printing a zero,
because a zero here reads as "the sync applied nothing" and that is a different
statement from "this script could not tell".

The apply stage logs one line per certified endorser block it applies, carrying
the certifying block's slot and its transaction count, so in the endorsement era
those lines are an exact progress marker. Before the endorsement era there are
none, and the write ahead log's pruning line is the only marker there; it trails
the applied tip by the retention window, so it is reported as what it is.

Three different slots are printed and each is labelled with where it came from,
because reading one of them as another is the mistake that misled a diagnosis
already. The endorser block line names the slot the PULL stage has reached, and
the pull stage runs ahead of apply by the depth of the queue, which was 4501
slots at one stop. The write ahead log pruning line is written by the APPLY
stage and names the cutoff it pruned to, which trails the applied tip by the
retention window, so it gives a lower bound on the applied tip. The store's own
tip is exact, and it can only be read with the store open, which the running
container will not allow, so it is read from the newest checkpoint while the
sync runs and from the store itself once the container is down.
"""
import os
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone

CONTAINER = "dolos-sync"
RUN = "/opt/build/run"
CONFIG = "dolos-origin.toml"
IMAGE = "dolos-build:1"
CHECKPOINTS = os.path.join(RUN, "checkpoints")
CHECKPOINT_LOG = os.path.join(RUN, "checkpoint.log")

# Tip of the node's own immutable database when this sync was started, from
#   python3 /opt/build/tipslot.py
# which reads the node's chunk files rather than any external source, so the
# projection is against the chain this sync is following.
TIP_SLOT = 2595923

TS = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+)Z")
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def load_density():
    """Blocks per slot on this chain, measured once and written to a file.

    Read from the file rather than guessed, and absent rather than defaulted, so
    a blocks per minute figure is never invented from a number nobody measured.
    """
    try:
        with open("/opt/build/run/density.txt") as f:
            for line in f:
                if line.startswith("blocks per slot"):
                    return float(line.split()[-1])
    except OSError:
        return None
    return None


def newest_checkpoint():
    """Slot and time of the newest checkpoint on disk, or None with a reason.

    A checkpoint directory is named for the store tip that was read out of it,
    so its name is an exact store tip that was true when it was taken. The time
    comes from checkpoint.log rather than from the directory, because cp -a
    keeps the mtime of the store it copied and that is a different moment.
    """
    try:
        names = [n for n in os.listdir(CHECKPOINTS) if n.isdigit()]
    except OSError as exc:
        return None, f"no checkpoints directory ({exc.strerror})"
    if not names:
        return None, "checkpoints directory holds no checkpoint"
    slot = max(int(n) for n in names)
    when = "at a time checkpoint.log does not record"
    try:
        with open(CHECKPOINT_LOG) as f:
            for line in f:
                if f" checkpoint {slot} " in line:
                    when = "at " + line.split()[0]
    except OSError:
        pass
    return slot, when


def read_store_tip():
    """Exact tip slot of the store, by opening it with dolos data summary.

    Only callable with the container down, because the store lock is held by
    whoever has it open. Opening a Dolos store also writes to it, which is why
    this is behind a flag rather than run on every status call.
    """
    out = subprocess.run(
        ["docker", "run", "--rm", "--network", "none",
         "-v", "/opt/build:/bins:ro", "-v", f"{RUN}:/run-dolos",
         "-w", "/run-dolos", "--entrypoint", "/bins/dolos.final", IMAGE,
         "-c", CONFIG, "data", "summary"],
        capture_output=True, text=True)
    text = out.stdout + out.stderr
    found = re.search(r'"state"\s*:\s*\{\s*"tip_slot"\s*:\s*(\d+)', text)
    if found is None:
        return None, f"dolos data summary carried no state tip_slot: {text.strip()[:200]}"
    return int(found.group(1)), None


def read_log():
    out = subprocess.run(["docker", "logs", CONTAINER],
                         capture_output=True, text=True, errors="replace")
    return (out.stdout + out.stderr).replace("\r", "\n")


def main():
    density = load_density()
    text = read_log()
    if not text.strip():
        print("no log yet: the container has produced no output")
        return

    applied = []     # (time, slot, spliced)
    repeated = 0     # transactions an earlier endorser block had already carried
    unreadable = 0   # apply lines whose transaction count could not be read
    drifts = {}      # epoch -> (signed lovelace, which pots moved)
    lenient = {"skipped_inputs": 0, "recreated_outputs": 0, "blocks": 0}
    retention = None # wal retention window in slots, read from the pruning line
    prunes = []      # (time, cutoff_slot)
    errors = []
    started = None
    last_time = None

    for raw in text.splitlines():
        line = ANSI.sub("", raw)
        m = TS.match(line)
        if not m:
            continue
        t = datetime.strptime(m.group(1)[:26], "%Y-%m-%dT%H:%M:%S.%f")
        started = started or t
        last_time = t
        if "applied the transactions of a certified endorser block" in line:
            # The line said txs= before the follower learned to leave out a
            # transaction an earlier endorser block already carried, and says
            # named=, repeated= and spliced= after. One log can hold both, so
            # both are read, and a line that carries neither is counted as
            # unreadable rather than as zero transactions applied.
            spliced = re.search(r"spliced=(\d+)", line) or re.search(r"txs=(\d+)", line)
            again = re.search(r"repeated=(\d+)", line)
            if spliced is None:
                unreadable += 1
                continue
            applied.append((t, int(re.search(r"slot=(\d+)", line).group(1)),
                            int(spliced.group(1))))
            repeated += int(again.group(1)) if again else 0
        elif "pots drifted from max supply under lenient apply" in line:
            # The epoch boundary cannot conserve supply under the lenient rule,
            # so the drift is reported rather than asserted. Every field is read
            # from the line; a line missing one is shown as unreadable rather
            # than as a drift of zero, which would read as "supply conserved".
            ep = re.search(r"epoch=(\d+)", line)
            lv = re.search(r"drift_lovelace=(-?\d+)", line)
            # tracing renders the field unquoted and it runs to end of line,
            # so it is taken as the rest of the line rather than as a token.
            mv = re.search(r"moved=(.+)$", line)
            if ep and lv:
                # Keyed by epoch, last one wins. The container's log spans
                # every run including the ones before a rewind, so the same
                # boundary appears once per time it was applied. Summing the
                # lines would report a rewind as extra supply, which is the one
                # number here that must not be inflated.
                drifts[int(ep.group(1))] = (int(lv.group(1)),
                                            mv.group(1) if mv else "unreported")
            else:
                unreadable += 1
        elif "applied a block the way the node does" in line:
            si = re.search(r"skipped_inputs=(\d+)", line)
            ro = re.search(r"recreated_outputs=(\d+)", line)
            lenient["blocks"] += 1
            lenient["skipped_inputs"] += int(si.group(1)) if si else 0
            lenient["recreated_outputs"] += int(ro.group(1)) if ro else 0
        elif "cutoff_slot=" in line:
            prunes.append((t, int(re.search(r"cutoff_slot=(\d+)", line).group(1))))
        elif "max_slots=" in line:
            retention = int(re.search(r"max_slots=(\d+)", line).group(1))
        elif " ERROR " in line or "cannot resolve" in line or "panicked" in line:
            errors.append(line.strip()[:230])

    running = subprocess.run(["docker", "ps", "--filter", f"name={CONTAINER}",
                              "--format", "{{.Status}}"],
                             capture_output=True, text=True).stdout.strip()

    print(f"container       {running or 'NOT RUNNING'}")
    print(f"log spans       {started} .. {last_time}"
          f"  ({(last_time - started).total_seconds()/60:.1f} min)")

    # Three slots, each labelled with the stage that produced it. See the module
    # docstring for why they are not the same number.
    pull_slot = applied[-1][1] if applied else None
    cutoff = prunes[-1][1] if prunes else None
    apply_floor = None
    if cutoff is not None and retention is not None:
        apply_floor = cutoff + retention

    if pull_slot is None:
        print("pull slot       no endorser block line yet, so the pull stage has named no slot")
    else:
        print(f"pull slot       {pull_slot}  (pull stage, runs ahead of apply by the queue depth)")

    if apply_floor is not None:
        print(f"apply tip       at least {apply_floor}  (apply stage, wal prune cutoff "
              f"{cutoff} plus retention {retention})")
        print(f"                at least {100.0*apply_floor/TIP_SLOT:.2f} percent of slot {TIP_SLOT}")
    elif cutoff is not None:
        print(f"apply tip       wal prune cutoff {cutoff} seen but no max_slots line, "
              "so the retention window is unknown and no bound can be given")
    else:
        print("apply tip       no wal pruning line yet, so the apply stage has named no slot")

    if running:
        ckpt, ckpt_note = newest_checkpoint()
        if ckpt is None:
            print(f"store tip       not readable while the container holds the store lock, "
                  f"and {ckpt_note}")
        else:
            print(f"store tip       {ckpt} exactly, as of the checkpoint taken {ckpt_note}")
            print("                the live store cannot be opened while the container runs")
    elif "--read-store" in sys.argv:
        store, store_note = read_store_tip()
        if store is None:
            print(f"store tip       could not be read: {store_note}")
        else:
            print(f"store tip       {store} exactly  (dolos data summary on {RUN}/data)")
    else:
        print("store tip       the container is down, so pass --read-store to open the store "
              "and read it exactly")
        print("                opening a Dolos store writes to it, measured, so this is not "
              "done unasked")

    slot = apply_floor if apply_floor is not None else pull_slot

    print(f"endorser blocks applied {len(applied)}")
    print(f"endorser txs    applied {sum(x[2] for x in applied)}")
    print(f"                repeated by a later endorser block, left out {repeated}")

    if lenient["blocks"]:
        print(f"lenient apply   {lenient['blocks']} blocks needed it, "
              f"{lenient['skipped_inputs']} inputs skipped, "
              f"{lenient['recreated_outputs']} outputs re-created")

    if drifts:
        total = sum(lovelace for lovelace, _ in drifts.values())
        print(f"pots drift      {len(drifts)} epoch boundaries, "
              f"{total} lovelace total ({total / 1_000_000:.0f} ada)")
        for epoch in sorted(drifts)[-6:]:
            lovelace, moved = drifts[epoch]
            print(f"                epoch {epoch}: {lovelace:+d} lovelace "
                  f"({lovelace / 1_000_000:+.0f} ada)  {moved}")
    else:
        print("pots drift      none reported yet")

    if unreadable:
        print(f"                {unreadable} apply lines carried no transaction count")

    # Rate over the last ten minutes.
    #
    # The apply stage emits its endorser block lines in bursts, one per flushed
    # batch, so a window that happens to hold one burst spans almost no time and
    # a rate divided by it is nonsense. Any window shorter than a minute is
    # reported as too short rather than turned into a number.
    window = last_time - timedelta(minutes=10)
    applied_marks = [x for x in applied if x[0] >= window]
    prune_marks = [(t, s, 0) for t, s in prunes if t >= window]

    def span_seconds(marks):
        if len(marks) < 2:
            return 0.0
        return (marks[-1][0] - marks[0][0]).total_seconds()

    if span_seconds(applied_marks) >= 60:
        marks, source = applied_marks, "applied endorser blocks"
    elif span_seconds(prune_marks) >= 60:
        marks, source = prune_marks, "write ahead log pruning, which trails"
    else:
        marks, source = [], None

    rate = None
    if not marks:
        print("last ten min    no progress window of at least a minute yet")
    else:
        mins = (marks[-1][0] - marks[0][0]).total_seconds() / 60.0
        slots = marks[-1][1] - marks[0][1]
        txs = sum(x[2] for x in marks)
        rate = slots / mins
        print(f"last ten min    {slots} slots over {mins:.1f} min = "
              f"{rate:.0f} slots per minute  ({source})")
        if density:
            print(f"                {rate*density:.0f} blocks per minute "
                  f"(at {density:.5f} blocks per slot, measured)")
        else:
            print("                blocks per minute unavailable: "
                  "/opt/build/run/density.txt has no measured blocks per slot")
        print(f"                {txs/mins:.0f} endorser transactions per minute")

    print(f"errors          {len(errors)}")
    for e in errors[:5]:
        print("   ", e)

    if rate and slot:
        left = TIP_SLOT - slot
        hours = left / rate / 60.0
        print(f"projected       {left} slots left, {hours:.1f} hours at the last ten minute rate")
        print("                the tip moves too, so this is a floor rather than a finish time")
    else:
        print("projected       no rate, so no projection")


if __name__ == "__main__":
    main()
