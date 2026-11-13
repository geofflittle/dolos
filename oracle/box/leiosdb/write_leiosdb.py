"""Writes the captured rc-2026w42 endorser block into a LeiosDb of the given schemas, and prints the node-eb line it must give.

Usage: write_leiosdb.py <out db> <rows|closure|none> <none|unfilled|absent|tx-changed|body-changed> <schema name>...
"""
import gzip
import hashlib
import json
import os
import sqlite3
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
# The slot of the announcement the captured fetch-block request names.
SLOT = 529
# The offset of the transaction the changes act on.
CHANGED = 3


def read_head(data, at):
    """The major type, argument and end of the definite length CBOR head at an offset."""
    major, info = data[at] >> 5, data[at] & 31
    if info < 24:
        return major, info, at + 1
    width = 1 << (info - 24)
    return major, int.from_bytes(data[at + 1 : at + 1 + width], "big"), at + 1 + width


def captured():
    """The endorser block bytes and its transactions of the captured fetch-block and fetch-block-txs replies."""
    block = open(os.path.join(HERE, "fetch-block.cbor"), "rb").read()
    _, _, at = read_head(block, 0)
    _, _, at = read_head(block, at)
    eb = block[at:]
    replies = gzip.open(os.path.join(HERE, "fetch-block-txs.cbor.gz")).read()
    _, _, at = read_head(replies, 0)
    _, _, at = read_head(replies, at)
    _, count, at = read_head(replies, at)
    txs = []
    for _ in range(count):
        _, _, at = read_head(replies, at)
        _, size, at = read_head(replies, at)
        txs.append(replies[at : at + size])
        at += size
    return eb, txs


def main(out, layout, change, *schemas):
    """Writes the store the arguments name and prints the node-eb line it must give."""
    eb, txs = captured()
    eb_hash = hashlib.blake2b(eb, digest_size=32).digest()
    stored = list(txs)
    if change == "unfilled":
        stored[CHANGED] = bytes(len(txs[CHANGED]))
    if change == "tx-changed":
        stored[CHANGED] = bytes([txs[CHANGED][0] ^ 1]) + txs[CHANGED][1:]
    if os.path.exists(out):
        os.remove(out)
    con = sqlite3.connect(out)
    for schema in schemas:
        con.executescript(open(os.path.join(HERE, f"{schema}.sql")).read())
    if layout != "none":
        con.execute("insert into ebs (ebSlot, ebHashBytes, ebBytesSize) values (?, ?, ?)", (SLOT, eb_hash, len(eb)))
    if layout == "rows":
        for offset, (tx, tx_bytes) in enumerate(zip(txs, stored)):
            filled = 0 if change == "unfilled" and offset == CHANGED else 1
            tx_hash = hashlib.blake2b(tx, digest_size=32).digest()
            con.execute("insert into ebTxs values (?, ?, ?, ?)", (eb_hash, offset, tx_hash, len(tx)))
            con.execute("insert into ebTxBytes values (?, ?, ?, ?)", (eb_hash, offset, filled, tx_bytes))
    if layout == "closure":
        bounds = [0]
        for tx in txs:
            bounds.append(bounds[-1] + len(tx))
        tx_bounds = b"".join(b.to_bytes(4, "big") for b in bounds)
        body = eb[:-1] + bytes([eb[-1] ^ 1]) if change == "body-changed" else eb
        con.execute("insert into ebBodies values (?, ?, ?)", (eb_hash, tx_bounds, body))
        if change != "absent":
            con.execute("insert into ebClosures values (?, ?)", (eb_hash, b"".join(stored)))
    con.commit()
    con.close()
    print(json.dumps({"slot": SLOT, "body": eb.hex(), "txs": [tx.hex() for tx in txs]}))


if __name__ == "__main__":
    main(*sys.argv[1:])
