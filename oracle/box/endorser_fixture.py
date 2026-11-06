"""Prints the endorser block the node's leios store holds, and the fixture cases of the endorser transaction judge.

Usage:
  endorser_fixture.py node-eb <leios db> <tip slot> <certifying slot> <endorser block hash hex>
  endorser_fixture.py case <parent cbor> <block cbor> [<node-eb json>]
"""
import hashlib
import json
import sqlite3
import sys

# A day of one second slots, the age a row must reach before an immutable read of a file the node still writes is safe.
SETTLED_SLOTS = 86400


def head(major, n):
    """The CBOR head of a major type and argument, in its shortest form."""
    if n < 24:
        return bytes([major << 5 | n])
    for code, width in ((24, 1), (25, 2), (26, 4), (27, 8)):
        if n < 1 << (8 * width):
            return bytes([major << 5 | code]) + n.to_bytes(width, "big")
    raise ValueError(n)


def blake2b256(data):
    """The 32 byte BLAKE2b hash of the bytes."""
    return hashlib.blake2b(data, digest_size=32).digest()


def node_eb(db, tip, certifying, eb_hex):
    """The endorser block of the hash as the slot of its announcing block, its body and its transactions in body order."""
    tip, certifying, eb = int(tip), int(certifying), bytes.fromhex(eb_hex)
    if certifying > tip - SETTLED_SLOTS:
        sys.exit(f"refused: certified at slot {certifying}, less than {SETTLED_SLOTS} slots before tip {tip}")
    con = sqlite3.connect(f"file:{db}?mode=ro&immutable=1", uri=True)
    ebs = con.execute("select ebSlot, ebBytesSize from ebs where ebHashBytes = ?", (eb,)).fetchall()
    if len(ebs) != 1:
        sys.exit(f"refused: {len(ebs)} ebs rows for {eb_hex}")
    slot, size = ebs[0]
    rows = con.execute(
        "select t.txOffset, t.txHashBytes, t.txBytesSize, b.filled, b.txBytes from ebTxs t "
        "left join ebTxBytes b on b.ebHashBytes = t.ebHashBytes and b.txOffset = t.txOffset "
        "where t.ebHashBytes = ? order by t.txOffset",
        (eb,),
    ).fetchall()
    body = head(5, len(rows))
    txs = []
    for expected, (offset, tx_hash, tx_size, filled, tx_bytes) in enumerate(rows):
        if offset != expected:
            sys.exit(f"refused: offset {offset} where {expected} belongs")
        if filled != 1 or tx_bytes is None:
            sys.exit(f"refused: offset {offset} unfilled")
        if blake2b256(tx_bytes) != tx_hash or len(tx_bytes) != tx_size:
            sys.exit(f"refused: offset {offset} bytes do not hash to their row")
        body += head(2, 32) + tx_hash + head(0, tx_size)
        txs.append(tx_bytes.hex())
    if blake2b256(body) != eb or len(body) != size:
        sys.exit(f"refused: the rebuilt body of {eb_hex} does not hash to it")
    print(json.dumps({"slot": slot, "body": body.hex(), "txs": txs}))


def case(parent, block, eb=None):
    """One fixture line from the node's bytes of a ranking block, of its parent and of the endorser block it certifies."""
    endorser = json.load(open(eb)) if eb else None
    line = {"parent": open(parent, "rb").read().hex(), "block": open(block, "rb").read().hex(), "endorser": endorser}
    print(json.dumps(line))


if __name__ == "__main__":
    command, *args = sys.argv[1:]
    {"node-eb": node_eb, "case": case}[command](*args)
