#!/usr/bin/env python3
"""Re-encode the Musashi Dijkstra block fixtures from the w35 block shape to w36.

Every `.block` fixture named below is generated from the `.w35hex` file beside
it, which holds the bytes exactly as the prototype-2026w35 chain served them.
The w35 bytes are kept because the three chain defects these fixtures exist to
pin (a transaction spending an output a later transaction of the same block
produces, a transaction carried twice, a transaction carried twice whose output
was spent in between) happened on that chain and on no other. The chain the
w36 build follows carries no endorser block at all, so none of these can be
cut again from a live chain and the w35 bytes are the only record there is.

What the ledger changed between the two, and therefore what this rewrites:

  w35   block = [header, [invalid_transactions / nil, transactions, leios, peras]]
        transaction = [body, witness_set, auxiliary_data / nil]
  w36   block = [header, [transactions, leios, peras]]
        block_transaction = [body, witness_set, auxiliary_data / nil, bool]

The leading element is deleted and the producer's verdict rides on each
transaction instead. Every `.w35hex` here carries `nil` in that slot, which
means no transaction in the block was invalid, so every transaction's verdict
is `true` and the rewrite is determined rather than chosen. A source that
carries a list there would need each named index to become `false`, and this
script refuses such a source rather than guessing, because a wrong verdict
changes which outputs a block creates and would do it silently.

Nothing else is touched. Indefinite length encodings stay indefinite, map key
order stays as it was, and the header is copied byte for byte, so the block
number, slot, issuer, vrf proof, body hash and both Leios header fields are the
chain's own.

Usage, from the repository root:

    python3 test_data/regen-dijkstra-w36.py            rewrite every fixture
    python3 test_data/regen-dijkstra-w36.py --check    fail unless each output
                                                       is already byte for byte
                                                       what a rewrite produces
"""

import sys

# (source, generated). Both paths are relative to the repository root.
FIXTURES = [
    ("test_data/dijkstra-quiet.w35hex", "test_data/dijkstra-quiet.block"),
    ("test_data/dijkstra-plain.w35hex", "test_data/dijkstra-plain.block"),
    ("test_data/dijkstra-certifying.w35hex", "test_data/dijkstra-certifying.block"),
    ("test_data/dijkstra-certify-only.w35hex", "test_data/dijkstra-certify-only.block"),
    (
        "crates/cardano/test_data/dijkstra-forward-ref.w35hex",
        "crates/cardano/test_data/dijkstra-forward-ref.block",
    ),
    (
        "crates/cardano/test_data/dijkstra-repeat-first.w35hex",
        "crates/cardano/test_data/dijkstra-repeat-first.block",
    ),
    (
        "crates/cardano/test_data/dijkstra-repeat-second.w35hex",
        "crates/cardano/test_data/dijkstra-repeat-second.block",
    ),
    (
        "crates/cardano/test_data/dijkstra-repeat-ranking-first.w35hex",
        "crates/cardano/test_data/dijkstra-repeat-ranking-first.block",
    ),
    (
        "crates/cardano/test_data/dijkstra-repeat-ranking-second.w35hex",
        "crates/cardano/test_data/dijkstra-repeat-ranking-second.block",
    ),
]

DIJKSTRA_ERA_TAG = 8
CBOR_NULL = 0xF6
CBOR_TRUE = 0xF5
CBOR_BREAK = 0xFF


class Refused(Exception):
    """The source is not the shape this rewrite is defined for."""


def read_head(buf, i):
    """One CBOR head. Returns (major, argument, index after the head).

    An argument of None means the indefinite length form, which is a shape this
    script preserves rather than one it normalises.
    """
    initial = buf[i]
    major = initial >> 5
    extra = initial & 0x1F
    i += 1
    if extra < 24:
        return major, extra, i
    if extra == 24:
        return major, buf[i], i + 1
    if extra == 25:
        return major, int.from_bytes(buf[i : i + 2], "big"), i + 2
    if extra == 26:
        return major, int.from_bytes(buf[i : i + 4], "big"), i + 4
    if extra == 27:
        return major, int.from_bytes(buf[i : i + 8], "big"), i + 8
    if extra == 31:
        return major, None, i
    raise Refused(f"additional information {extra} at offset {i - 1} is not valid CBOR")


def skip(buf, i):
    """Index just past the complete item that starts at i."""
    major, arg, i = read_head(buf, i)
    if major in (0, 1, 7):
        return i
    if major in (2, 3):
        if arg is None:
            while buf[i] != CBOR_BREAK:
                i = skip(buf, i)
            return i + 1
        return i + arg
    if major == 4:
        if arg is None:
            while buf[i] != CBOR_BREAK:
                i = skip(buf, i)
            return i + 1
        for _ in range(arg):
            i = skip(buf, i)
        return i
    if major == 5:
        if arg is None:
            while buf[i] != CBOR_BREAK:
                i = skip(buf, i)
                i = skip(buf, i)
            return i + 1
        for _ in range(arg):
            i = skip(buf, i)
            i = skip(buf, i)
        return i
    if major == 6:
        return skip(buf, i)
    raise Refused(f"major type {major} at offset {i} is not an item this walk knows")


def transaction_positions(buf, start):
    """Where each transaction of the list starting at `start` begins and ends.

    Returns the list of (begin, end) and the index just past the whole list.
    """
    major, count, i = read_head(buf, start)
    if major != 4:
        raise Refused(f"the transaction list at offset {start} is not an array")
    spans = []
    if count is None:
        while buf[i] != CBOR_BREAK:
            begin = i
            i = skip(buf, i)
            spans.append((begin, i))
        return spans, i + 1
    for _ in range(count):
        begin = i
        i = skip(buf, i)
        spans.append((begin, i))
    return spans, i


def rewrite(raw):
    """The w36 encoding of a w35 block. Refuses anything that is not one."""
    major, outer, i = read_head(raw, 0)
    if major != 4 or outer != 2:
        raise Refused("the file does not start with a two element era wrapper")
    major, era, i = read_head(raw, i)
    if major != 0 or era != DIJKSTRA_ERA_TAG:
        raise Refused(f"era tag is {era}, not the Dijkstra tag {DIJKSTRA_ERA_TAG}")
    block_head = i
    major, block_len, i = read_head(raw, i)
    if major != 4 or block_len != 2:
        raise Refused(f"the block is an array of {block_len}, not of two")

    header_begin = i
    header_end = skip(raw, i)

    body_head = header_end
    major, body_len, after_body_head = read_head(raw, body_head)
    if major != 4:
        raise Refused("the block body is not an array")
    if body_len == 3:
        raise Refused(
            "the block body already has three elements, so this source is w36 "
            "and rewriting it again would append a second verdict to every "
            "transaction"
        )
    if body_len != 4:
        raise Refused(f"the block body is an array of {body_len}, not of four")

    invalid_begin = after_body_head
    if raw[invalid_begin] != CBOR_NULL:
        raise Refused(
            "the deleted leading element is not nil, so this block names "
            "transactions the producer rejected and each of them would need "
            "the verdict false, which this rewrite does not decide"
        )
    invalid_end = invalid_begin + 1

    tx_list_begin = invalid_end
    spans, tx_list_end = transaction_positions(raw, tx_list_begin)
    tail = raw[tx_list_end : skip(raw, skip(raw, tx_list_end))]

    out = bytearray()
    out += raw[:block_head]
    out += raw[block_head:header_end]
    out.append(0x80 | 3)
    out += raw[tx_list_begin : spans[0][0]] if spans else raw[tx_list_begin:tx_list_end]

    if spans:
        for begin, end in spans:
            major, arity, after = read_head(raw, begin)
            if major != 4:
                raise Refused(f"the transaction at offset {begin} is not an array")
            if arity == 4:
                raise Refused(
                    f"the transaction at offset {begin} already has four "
                    "elements, so this source is w36"
                )
            if arity != 3:
                raise Refused(
                    f"the transaction at offset {begin} is an array of {arity}, "
                    "not of three"
                )
            out.append(0x80 | 4)
            out += raw[after:end]
            out.append(CBOR_TRUE)
        if raw[tx_list_end - 1] == CBOR_BREAK and tx_list_end - 1 >= spans[-1][1]:
            out.append(CBOR_BREAK)
    out += tail
    return bytes(out)


def load(path):
    with open(path) as handle:
        return bytes.fromhex(handle.read().strip())


def main(argv):
    check = "--check" in argv[1:]
    unknown = [a for a in argv[1:] if a != "--check"]
    if unknown:
        print(f"regen-dijkstra-w36.py: unknown argument {unknown[0]}", file=sys.stderr)
        return 2

    bad = 0
    for source, generated in FIXTURES:
        try:
            produced = rewrite(load(source))
        except Refused as why:
            print(f"REFUSED {source}: {why}", file=sys.stderr)
            bad += 1
            continue
        text = produced.hex() + "\n"
        if check:
            with open(generated) as handle:
                on_disk = handle.read()
            if on_disk == text:
                print(f"IDENTICAL {generated}  {len(produced)} bytes")
            else:
                print(
                    f"DIFFERS {generated}  a rewrite of {source} is "
                    f"{len(produced)} bytes and does not match what is on disk",
                    file=sys.stderr,
                )
                bad += 1
        else:
            with open(generated, "w") as handle:
                handle.write(text)
            print(f"WROTE {generated}  {len(produced)} bytes from {source}")

    if bad:
        print(f"{bad} of {len(FIXTURES)} fixtures did not come out", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
