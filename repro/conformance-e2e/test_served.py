"""Checks each verdict of served.py on values whose answer is known.

usage: python3 -I test_served.py
"""

import contextlib
import io
import os
import sys
from decimal import Decimal
from fractions import Fraction

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import served as S  # noqa: E402

CASES = []


def case(fn):
    CASES.append(fn)
    return fn


@case
def equal_values_agree():
    assert S.ppu_verdict(45, 308000, "308000") is None
    assert S.ppu_verdict(9, ("ratio", 3, 10), {"numerator": 3, "denominator": 10}) is None
    assert S.ppu_verdict(9, ("ratio", 1, 2), {"numerator": 2, "denominator": 4}) is None
    assert S.ppu_verdict(20, [16500000, 10000000000], {"memory": "16500000", "steps": "10000000000"}) is None
    assert S.ppu_verdict(30, 100000000000, {"int": "100000000000"}) is None
    assert S.param_equal("ratio", Decimal("0.0577"), {"numerator": 577, "denominator": 10000})
    assert S.param_equal("int", None, None)
    assert S.param_equal("int", 0, "0")


@case
def a_different_value_is_a_served_fact():
    assert S.ppu_verdict(45, 308000, "308001") == "served fact"
    assert S.ppu_verdict(45, 308000, None) == "served fact"
    assert S.ppu_verdict(9, ("ratio", 3, 10), {"numerator": 3, "denominator": 11}) == "served fact"
    assert not S.param_equal("int", None, "0")
    assert not S.param_equal("int", 0, None)
    assert not S.param_equal("ratio", Decimal("0.0577"), {"numerator": 577, "denominator": 10001})


@case
def a_rational_cut_to_32_bits_is_its_own_class():
    n, d = (1 << 32) + 3, (1 << 32) + (1 << 31) + 7
    sd = (1 << 31) + 7
    assert Fraction(n, d) != Fraction(3, sd)
    assert S.ppu_verdict(9, ("ratio", n, d), {"numerator": 3, "denominator": sd}) == "32 bit rational width"
    assert S.ppu_verdict(9, ("ratio", n, d), {"numerator": 3, "denominator": 7}) == "served fact"


@case
def block_facts_read_absent_proto_fields_as_their_defaults():
    f = S.block_facts({"header": {"slot": "16", "hash": "AA=="}, "body": {}})
    assert f["block_no"] == 0 and f["txs"] == 0 and not f["peras_certificate"] and not f["eb_announcement"]


@case
def cbor_slices_cover_indefinite_forms():
    b = bytes.fromhex("9f01a2010203045f4101ff40ff")
    assert S.end(b, 0) == len(b)
    assert [b[s:e].hex() for s, e in S.elems(b, 0)] == ["01", "a201020304", "5f4101ff", "40"]


@case
def an_unset_optional_param_agrees_only_with_a_node_null():
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        S.params_verdicts({"perasBootstrapRound": None, "perasCertBoost": 0}, {"perasCertBoost": "0"})
        S.params_verdicts({"perasBootstrapRound": 7, "perasCertBoost": 0}, {"perasCertBoost": "0"})
    first, second = [ln for ln in out.getvalue().splitlines() if ln.startswith("params:")]
    assert "agree 2, disagree 0" in first, first
    assert "agree 1, disagree 1" in second, second


HEADER = bytes.fromhex("82" "88" "000000000000" "08" "00" "40")
NODE_BODY = bytes.fromhex("83" "80" "8241014102" "40")
TX = bytes.fromhex("84a10001a0f6f5")


@case
def a_body_as_long_as_the_header_says_is_the_node_block():
    nb = b"\x82\x08\x82" + HEADER + NODE_BODY
    era, _, _, count, _, inlined, block, peras = S.node_block(nb)
    assert (era, count, inlined, block, peras) == (8, 0, False, nb[2:], b"\x40")


@case
def an_indefinite_transaction_list_is_a_node_block():
    body = b"\x83\x9f" + TX + b"\xff\xf6\x40"
    nb = b"\x82\x08\x82" + HEADER[:8] + bytes([len(body)]) + HEADER[9:] + body
    _, _, _, count, _, inlined, block, _ = S.node_block(nb)
    assert (count, inlined, block) == (None, False, nb[2:])


@case
def a_body_longer_than_the_header_says_is_rebuilt_from_its_tail():
    served = b"\x83\x82" + TX + TX + NODE_BODY[2:]
    nb = b"\x82\x08\x82" + HEADER + served
    era, _, _, count, first, inlined, block, peras = S.node_block(nb)
    assert (era, count, inlined, peras) == (8, 2, True, b"\x40")
    assert block == b"\x82" + HEADER + NODE_BODY
    assert S.mempool(nb, (first, S.end(nb, first)))[0] == bytes.fromhex("83a10001a0f6")


def main():
    failed = 0
    for fn in CASES:
        try:
            fn()
            print(f"ok   {fn.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"FAIL {fn.__name__} {e}")
    print(f"cases {len(CASES)}, failed {failed}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
