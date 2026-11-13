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


def read_head(data, at):
    """The major type, argument and end of the definite length CBOR head at an offset."""
    major, info = data[at] >> 5, data[at] & 31
    if info < 24:
        return major, info, at + 1
    if info > 27:
        sys.exit(f"refused: an indefinite or reserved CBOR head at offset {at} of the body")
    width = 1 << (info - 24)
    return major, int.from_bytes(data[at + 1 : at + 1 + width], "big"), at + 1 + width


def blake2b256(data):
    """The 32 byte BLAKE2b hash of the bytes."""
    return hashlib.blake2b(data, digest_size=32).digest()


def body_entries(body):
    """The transaction hashes and sizes an endorser block body lists, in body order."""
    major, count, at = read_head(body, 0)
    if major != 5:
        sys.exit("refused: the body is not a map")
    entries = []
    for _ in range(count):
        major, length, at = read_head(body, at)
        if major != 2 or length != 32:
            sys.exit(f"refused: a body key at offset {at} is not 32 bytes")
        tx_hash, at = body[at : at + 32], at + 32
        major, tx_size, at = read_head(body, at)
        if major != 0:
            sys.exit(f"refused: a body value at offset {at} is not an unsigned integer")
        entries.append((tx_hash, tx_size))
    return entries


def select(con, tables, table, eb):
    """The rows of a table for an endorser block hash, holding the columns the layout reads, in order of the first."""
    columns = tables[table]
    query = f"select {', '.join(columns)} from {table} where ebHashBytes = ? order by {columns[0]}"
    return con.execute(query, (eb,)).fetchall()


def read_rows(con, tables, eb):
    """The body rebuilt from a row per transaction, and each transaction's bytes in body order, None where unfilled."""
    filled = {offset: tx_bytes for offset, is_filled, tx_bytes in select(con, tables, "ebTxBytes", eb) if is_filled == 1}
    rows = select(con, tables, "ebTxs", eb)
    body = head(5, len(rows))
    txs = []
    for expected, (offset, tx_hash, tx_size) in enumerate(rows):
        if offset != expected:
            sys.exit(f"refused: offset {offset} where {expected} belongs")
        body += head(2, 32) + tx_hash + head(0, tx_size)
        txs.append(filled.get(offset))
    return body, txs


def read_closure(con, tables, eb):
    """The stored body, and each transaction's bytes in body order cut from the closure at its bounds, None where unfilled."""
    bodies = select(con, tables, "ebBodies", eb)
    if len(bodies) != 1:
        sys.exit(f"refused: {len(bodies)} ebBodies rows")
    body, bounds = bodies[0]
    positions = [int.from_bytes(bounds[at : at + 4], "big") for at in range(0, len(bounds), 4)]
    closures = select(con, tables, "ebClosures", eb)
    closure = closures[0][0] if closures else bytes(positions[-1])
    # The node allocates the closure as zeroes and writes each transaction into it as it arrives, so a transaction of only zero bytes is unfilled.
    txs = [closure[start:end] for start, end in zip(positions, positions[1:])]
    return body, [tx if any(tx) else None for tx in txs]


# The columns each LeiosDb layout's reader reads from each of its tables beside ebHashBytes, rows as node release rc-2026w42 writes it and closure as rc-2026w43 to rc-2026w45 write it.
LAYOUTS = {
    "rows": (
        {
            "ebs": ("ebSlot", "ebBytesSize"),
            "ebTxs": ("txOffset", "txHashBytes", "txBytesSize"),
            "ebTxBytes": ("txOffset", "filled", "txBytes"),
        },
        read_rows,
    ),
    "closure": (
        {
            "ebs": ("ebSlot", "ebBytesSize"),
            "ebBodies": ("body", "txBounds"),
            "ebClosures": ("txs",),
        },
        read_closure,
    ),
}


def layout_of(con):
    """The name of the one known layout whose tables and columns the store holds."""
    held = {}
    for (table,) in con.execute("select name from sqlite_master where type = 'table' order by name"):
        held[table] = [row[1] for row in con.execute(f'pragma table_info("{table}")')]
    names = [
        name
        for name, (tables, _) in LAYOUTS.items()
        if all({"ebHashBytes", *columns} <= set(held.get(table, ())) for table, columns in tables.items())
    ]
    if not names:
        found = ", ".join(f"{table}({', '.join(columns)})" for table, columns in held.items())
        sys.exit(f"refused: the leios store holds no known layout, it holds {found}")
    if len(names) > 1:
        sys.exit(f"refused: the leios store holds the layouts {' and '.join(names)}")
    return names[0]


def node_eb(db, tip, certifying, eb_hex):
    """The endorser block of the hash as the slot of its announcing block, its body and its transactions in body order."""
    tip, certifying, eb = int(tip), int(certifying), bytes.fromhex(eb_hex)
    if certifying > tip - SETTLED_SLOTS:
        sys.exit(f"refused: certified at slot {certifying}, less than {SETTLED_SLOTS} slots before tip {tip}")
    con = sqlite3.connect(f"file:{db}?mode=ro&immutable=1", uri=True)
    tables, read = LAYOUTS[layout_of(con)]
    ebs = select(con, tables, "ebs", eb)
    if len(ebs) != 1:
        sys.exit(f"refused: {len(ebs)} ebs rows for {eb_hex}")
    slot, size = ebs[0]
    body, txs = read(con, tables, eb)
    if blake2b256(body) != eb or len(body) != size:
        sys.exit(f"refused: the body of {eb_hex} does not hash to it")
    entries = body_entries(body)
    if len(entries) != len(txs):
        sys.exit(f"refused: the body lists {len(entries)} transactions where the store holds {len(txs)}")
    for offset, ((tx_hash, tx_size), tx_bytes) in enumerate(zip(entries, txs)):
        if tx_bytes is None:
            sys.exit(f"refused: offset {offset} unfilled")
        if blake2b256(tx_bytes) != tx_hash or len(tx_bytes) != tx_size:
            sys.exit(f"refused: offset {offset} bytes do not hash to the body's entry")
    print(json.dumps({"slot": slot, "body": body.hex(), "txs": [tx.hex() for tx in txs]}))


def case(parent, block, eb=None):
    """One fixture line from the node's bytes of a ranking block, of its parent and of the endorser block it certifies."""
    endorser = json.load(open(eb)) if eb else None
    line = {"parent": open(parent, "rb").read().hex(), "block": open(block, "rb").read().hex(), "endorser": endorser}
    print(json.dumps(line))


if __name__ == "__main__":
    command, *args = sys.argv[1:]
    {"node-eb": node_eb, "case": case}[command](*args)
