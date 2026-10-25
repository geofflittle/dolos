"""Compares what a utxorpc v1 server serves against the ledger reading of the same CBOR.

usage:
  served.py dump GRPC OUT            every block by DumpHistory into OUT/blocks.jsonl
  served.py fetch GRPC OUT SLOT...   FetchBlock of the named slots into OUT/fetched.jsonl
  served.py cut GRPC OUT SLOT...     oracle inputs OUT/block.hex, OUT/tx.hex and OUT/ppu.hex with their index,
                                     every transaction of the named slots and of blocks without a certificate,
                                     the first and the last of any other certifying block
  served.py compare OUT              per field verdicts of the served values against the oracle lines
  served.py params GRPC NODE OUT     ReadParams against the node protocol parameter query
"""

import base64
import hashlib
import json
import os
import subprocess
import sys
from collections import Counter
from decimal import Decimal
from fractions import Fraction

sys.setrecursionlimit(100000)

GRPCURL = os.environ.get("GRPCURL", os.path.join(os.path.dirname(os.path.abspath(__file__)), "grpcurl"))
# Served values each key of the comparison reads, so a planted break can replace one of them.
PLANT = os.environ.get("PLANT", "")


def call(addr, method, req, defaults=False):
    args = [GRPCURL, "-plaintext", "-max-msg-sz", "1000000000"]
    if defaults:
        args.append("-emit-defaults")
    args += ["-d", json.dumps(req), addr, method]
    out = subprocess.run(args, check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def b64(s):
    return base64.b64decode(s or "")


def blake(b):
    return hashlib.blake2b(b, digest_size=32).digest()


# CBOR slices ------------------------------------------------------------------


def head(b, p):
    ib = b[p]
    major, info = ib >> 5, ib & 31
    p += 1
    if info < 24:
        return major, info, p
    if info == 31:
        return major, None, p
    n = {24: 1, 25: 2, 26: 4, 27: 8}[info]
    return major, int.from_bytes(b[p:p + n], "big"), p + n


def end(b, p):
    major, arg, p = head(b, p)
    if major in (0, 1, 7):
        return p
    if major in (2, 3):
        if arg is None:
            while b[p] != 0xFF:
                p = end(b, p)
            return p + 1
        return p + arg
    if major == 6:
        return end(b, p)
    if arg is None:
        while b[p] != 0xFF:
            p = end(b, p)
            if major == 5:
                p = end(b, p)
        return p + 1
    for _ in range(arg):
        p = end(b, p)
        if major == 5:
            p = end(b, p)
    return p


def untag(b, p):
    while b[p] >> 5 == 6:
        _, _, p = head(b, p)
    return p


def elems(b, p):
    """The (start, end) of each element of the array at p, or of each key and value of the map at p."""
    major, arg, q = head(b, untag(b, p))
    assert major in (4, 5), f"not an array or map at {p}"
    out = []
    while (arg is None and b[q] != 0xFF) or (arg is not None and len(out) < arg * (2 if major == 5 else 1)):
        e = end(b, q)
        out.append((q, e))
        q = e
    return out


def uint(b, p):
    major, arg, _ = head(b, p)
    assert major == 0
    return arg


def mempool(b, tx):
    """The three element `[body, witnesses, auxiliary data]` the oracle reads, from a block or mempool form."""
    parts = elems(b, tx[0])
    flags = [i for i, (s, _) in enumerate(parts) if b[s] in (0xF4, 0xF5)]
    keep = [s for i, s in enumerate(parts) if i not in flags]
    assert len(keep) == 3, f"transaction of {len(parts)} elements"
    return b"\x83" + b"".join(b[s:e] for s, e in keep), b[keep[0][0]:keep[0][1]]


# Served facts -----------------------------------------------------------------


def bigint(x):
    if x is None:
        return None
    if "int" in x:
        return int(x["int"])
    if "bigUInt" in x:
        return int.from_bytes(b64(x["bigUInt"]), "big")
    if "bigNInt" in x:
        return -1 - int.from_bytes(b64(x["bigNInt"]), "big")
    return 0


def opt(x):
    return None if x is None else int(x)


def tx_facts(t):
    c = t.get("common", {})
    w = c.get("witnesses", {})
    col = t.get("collateral", {})
    scripts = [next(iter(s), None) for s in w.get("script", [])]
    validity = c.get("validity", {})
    donation = bigint(c.get("treasuryDonation"))
    return {
        "body.fee": bigint(t.get("fee")) or 0,
        "body.ttl": opt(validity.get("ttl")),
        "body.validity_start": opt(validity.get("start")),
        "body.inputs": len(c.get("inputs", [])),
        "body.outputs": len(c.get("outputs", [])),
        "body.outputs_coin": sum(bigint(o.get("coin")) or 0 for o in c.get("outputs", [])),
        "body.certs": len(c.get("certificates", [])),
        "body.withdrawals": len(c.get("withdrawals", [])),
        "body.mint_policies": len(c.get("mint", [])),
        "body.aux_data_hash": bool(b64(c.get("auxiliaryDataHash"))),
        "body.script_data_hash": bool(b64(c.get("scriptDataHash"))),
        "body.collateral": len(col.get("collateral", [])),
        "body.guards": len(c.get("guards", [])),
        "body.network_id": opt(c.get("networkId")),
        "body.collateral_return": "collateralReturn" in col,
        "body.total_collateral": bigint(col.get("totalCollateral")),
        "body.reference_inputs": len(c.get("referenceInputs", [])),
        "body.voters": len(c.get("votes", [])),
        "body.proposals": len(c.get("proposals", [])),
        "body.treasury": bigint(c.get("treasuryValue")),
        "body.donation": 0 if donation is None else donation,
        "body.sub_transactions": len(t.get("subTransactions", [])),
        "body.required_top_level_guards": len(c.get("requiredTopLevelGuards", [])),
        "body.direct_deposits": len(c.get("directDeposits", [])),
        "body.account_balance_intervals": len(c.get("accountBalanceIntervals", [])),
        "body.starting_account_balance_intervals": len(t.get("startingAccountBalanceIntervals", [])),
        "wits.vkey": len(w.get("vkeywitness", [])),
        "wits.bootstrap": len(w.get("bootstrapWitnesses", [])),
        "wits.native": scripts.count("native"),
        "wits.plutus_v1": scripts.count("plutusV1"),
        "wits.plutus_v2": scripts.count("plutusV2"),
        "wits.plutus_v3": scripts.count("plutusV3"),
        "wits.plutus_v4": scripts.count("plutusV4"),
        "wits.datums": len(w.get("plutusDatums", [])),
        "wits.redeemers": len(w.get("redeemers", [])),
        "aux_data": "auxiliary" in c,
    }


def block_facts(blk):
    h = blk.get("header", {})
    body = blk.get("body", {})
    return {
        "hash": b64(h.get("hash")).hex(),
        "block_no": int(h.get("height", 0)),
        "slot": int(h.get("slot", 0)),
        "contains_leios_cert": bool(h.get("bodyContainsLeiosCertificate", False)),
        "eb_announcement": "endorserBlockAnnouncement" in h,
        "txs": len(body.get("tx", [])),
        "leios_certificate": "leiosCertificate" in body,
        "peras_certificate": bool(b64(body.get("perasCertificate"))),
    }


# The block facts the oracle prints that the v1 block message has no field for.
NO_V1_BLOCK_FIELD = {"prev_genesis", "body_size", "body_hash", "protocol_major"}

# Protocol parameter update keys and the v1 field each maps to.
PPU_FIELDS = {
    0: "minFeeCoefficient", 1: "minFeeConstant", 2: "maxBlockBodySize", 3: "maxTxSize",
    4: "maxBlockHeaderSize", 5: "stakeKeyDeposit", 6: "poolDeposit", 7: "poolRetirementEpochBound",
    8: "desiredNumberOfPools", 9: "poolInfluence", 10: "monetaryExpansion", 11: "treasuryExpansion",
    16: "minPoolCost", 17: "coinsPerUtxoByte", 18: "costModels", 19: "prices",
    20: "maxExecutionUnitsPerTransaction", 21: "maxExecutionUnitsPerBlock", 22: "maxValueSize",
    23: "collateralPercentage", 24: "maxCollateralInputs", 25: "poolVotingThresholds",
    26: "drepVotingThresholds", 27: "minCommitteeSize", 28: "committeeTermLimit",
    29: "governanceActionValidityPeriod", 30: "governanceActionDeposit", 31: "drepDeposit",
    32: "drepInactivityPeriod", 33: "minFeeScriptRefCostPerByte", 34: "maxRefScriptSizePerBlock",
    35: "maxRefScriptSizePerTx", 36: "refScriptCostStride", 37: "refScriptCostMultiplier",
    38: "maxPledgeLeverage", 39: "minPoolMargin", 40: "leiosAnnouncementPeriodLength",
    41: "leiosVotePeriodLength", 42: "leiosDiffusionPeriodLength", 43: "leiosCommitteeSize",
    44: "leiosQuorumStakeThreshold", 45: "maxEndorserBlockReferencesSize",
    46: "maxEndorserBlockTxsSize", 47: "maxEndorserBlockExecutionUnits",
    48: "maxRefScriptSizePerEndorserBlock", 49: "perasMinCandidateBlockAge",
    50: "perasHealingFactor", 51: "perasCertBoost", 52: "perasTargetCommitteeSize",
    53: "perasBootstrapRound", 54: "perasQuorumThresholdSafetyMargin",
    55: "refInputsCostPerMultiAssetPolicy", 56: "refInputsCostPerDatumByte",
}


# Ledger values ----------------------------------------------------------------


def value(b, p=0):
    """A decoded CBOR value, with tag 30 as a Fraction and its written pair kept."""
    major, arg, q = head(b, p)
    if major == 0:
        return arg
    if major == 1:
        return -1 - arg
    if major == 6 and arg == 30:
        n, d = (value(b, s) for s, _ in elems(b, q))
        return ("ratio", n, d)
    if major == 6:
        return value(b, q)
    if major == 4:
        return [value(b, s) for s, _ in elems(b, p)]
    if major == 5:
        e = elems(b, p)
        return {value(b, e[i][0]): value(b, e[i + 1][0]) for i in range(0, len(e), 2)}
    if major == 2:
        return b[q:end(b, p)]
    if b[p] in (0xF4, 0xF5):
        return b[p] == 0xF5
    if b[p] == 0xF6:
        return None
    raise ValueError(f"unexpected CBOR at {p}: {b[p]:02x}")


def ratio_verdict(ledger, served):
    if ledger is None or ledger[0] != "ratio":
        return "served fact"
    if served is None:
        return "served fact"
    _, n, d = ledger
    sn, sd = int(served.get("numerator", 0)), int(served.get("denominator", 0))
    if sd and d and Fraction(sn, sd) == Fraction(n, d):
        return None
    if sn == ((n + (1 << 31)) % (1 << 32)) - (1 << 31) and sd == d % (1 << 32):
        return "32 bit rational width"
    return "served fact"


def exunits(served):
    return [int(served.get("memory", 0)), int(served.get("steps", 0))] if served is not None else None


def ppu_verdict(key, ledger, served):
    """None when the served value equals the ledger value of key, otherwise the class."""
    if key in (9, 10, 11, 33, 37, 38, 39, 44, 50, 54):
        return ratio_verdict(ledger, served)
    if key in (20, 21, 47):
        return None if exunits(served) == ledger else "served fact"
    if key == 19:
        if served is None:
            return "served fact"
        verdicts = [ratio_verdict(ledger[0], served.get("memory")), ratio_verdict(ledger[1], served.get("steps"))]
        return next((v for v in verdicts if v), None)
    if key in (25, 26):
        got = (served or {}).get("thresholds", [])
        if len(got) != len(ledger):
            return "served fact"
        return next((v for v in (ratio_verdict(a, b) for a, b in zip(ledger, got)) if v), None)
    if key == 18:
        names = {0: "plutusV1", 1: "plutusV2", 2: "plutusV3", 3: "plutusV4"}
        got = {k: [int(x) for x in v.get("values", [])] for k, v in (served or {}).items()}
        want = {names.get(k, f"raw {k}"): v for k, v in ledger.items()}
        return None if got == want else "served fact"
    if isinstance(served, dict):
        return None if bigint(served) == ledger else "served fact"
    return None if served is not None and int(served) == ledger else "served fact"


# Commands ---------------------------------------------------------------------


def dump(addr, out):
    os.makedirs(out, exist_ok=True)
    token = None
    n = 0
    with open(f"{out}/blocks.jsonl", "w") as fh:
        while True:
            req = {"max_items": 100}
            if token:
                req["start_token"] = token
            res = call(addr, "utxorpc.v1.sync.SyncService/DumpHistory", req)
            for blk in res.get("block", []):
                fh.write(json.dumps(blk) + "\n")
                n += 1
            token = res.get("nextToken")
            if not token or not res.get("block"):
                break
    print(f"dumped {n} blocks")


def lines(path):
    with open(path) as fh:
        for x in fh:
            if x.strip():
                yield json.loads(x)


def load_blocks(out, name="blocks.jsonl"):
    return lines(f"{out}/{name}")


def fetch(addr, out, slots):
    by_slot = {}
    for b in load_blocks(out):
        s = int(b["cardano"]["header"].get("slot", 0))
        if s in slots:
            by_slot[s] = b
    same = 0
    with open(f"{out}/fetched.jsonl", "w") as fh:
        for s in slots:
            h = by_slot[s]["cardano"]["header"]
            res = call(addr, "utxorpc.v1.sync.SyncService/FetchBlock", {"ref": {"slot": s, "hash": h["hash"]}})
            blk = res["block"]
            fh.write(json.dumps(blk) + "\n")
            same += blk == by_slot[s]
            print(f"fetch slot {s}: FetchBlock equals DumpHistory {blk == by_slot[s]}")
    print(f"fetched {len(slots)}, equal to the dumped block {same}")


def node_block(nb):
    """The parts of a served `[era, block]` and the block the node sent."""
    # `[era, [header, [transactions, leios certificate, peras certificate]]]`, each array head one byte.
    header = (3, end(nb, 3))
    assert nb[0] == 0x82 and nb[2] == 0x82 and nb[header[1]] == 0x83, "not a two element block"
    era = uint(nb, 1)
    body_size = uint(nb, elems(nb, elems(nb, 3)[0][0])[6][0])
    body = (header[1], len(nb))
    # The count is None for an indefinite transaction list.
    _, count, first = head(nb, body[0] + 1)
    inlined = body[1] - body[0] != body_size
    if inlined:
        # The node's body is an empty transaction list and the two certificates, the tail of the served body.
        tail = nb[len(nb) - (body_size - 2):]
        leios_end = end(tail, 0)
        assert end(tail, leios_end) == len(tail), "the tail is not the two certificates"
        block_bytes = b"\x82" + nb[header[0]:header[1]] + b"\x83\x80" + tail
        peras_raw = tail[leios_end:]
    else:
        parts = elems(nb, body[0])
        block_bytes = nb[2:]
        peras_raw = nb[parts[2][0]:parts[2][1]]
    return era, header, body, count, first, inlined, block_bytes, peras_raw


def cut(addr, out, full):
    index = []
    with open(f"{out}/block.hex", "w") as bh, open(f"{out}/tx.hex", "w") as th, open(f"{out}/ppu.hex", "w") as ph, \
            open(f"{out}/slim.jsonl", "w") as slim:
        nt = 0
        npu = 0
        for bi, blk in enumerate(load_blocks(out)):
            nb = b64(blk["nativeBytes"])
            era, header, body, count, first, inlined, block_bytes, peras_raw = node_block(nb)
            served = blk["cardano"].get("body", {}).get("tx", [])
            slot = int(blk["cardano"]["header"].get("slot", 0))
            walk = not inlined or slot in full
            bh.write(block_bytes.hex() + "\n")
            # Every transaction of a block whose body is the node's, or of a FULL slot, the first of any other.
            if walk or count is None:
                txs = elems(nb, body[0] + 1)
                count = len(txs)
                txs = txs if walk else txs[:1]
            else:
                txs = [(first, end(nb, first))] if count else []
            rec = dict(
                block=bi, era=era, header_hash=blake(nb[header[0]:header[1]]).hex(), peras_raw=peras_raw.hex(),
                served_bytes_txs=count, inlined=inlined, body_len=body[1] - body[0], txs=[],
            )
            # Tag 24 over indefinite bytes needs these three bytes in a row, so zero hits rules it out.
            rec["tag24_indef_bytes"] = nb.count(b"\xd8\x18\x5f")
            source = "endorser" if inlined else "block"
            sources = [(i, source, mempool(nb, t)) for i, t in enumerate(txs)]
            picked = {i for i, _, _ in sources}
            blk["cardano"].setdefault("body", {})["tx"] = [t if i in picked else {} for i, t in enumerate(served)]
            slim.write(json.dumps(blk) + "\n")
            for pos, src, (tx3, body_bytes) in sources:
                th.write(tx3.hex() + "\n")
                trec = dict(line=nt, pos=pos, source=src, body_hash=blake(body_bytes).hex(), ppus=[])
                # Each parameter change proposal of key 20 is an oracle `ppu` input.
                bparts = elems(body_bytes, 0)
                for k in range(0, len(bparts), 2):
                    if uint(body_bytes, bparts[k][0]) != 20:
                        continue
                    for ps, _ in elems(body_bytes, bparts[k + 1][0]):
                        proc = elems(body_bytes, ps)
                        action = elems(body_bytes, proc[2][0])
                        if uint(body_bytes, action[0][0]) == 0:
                            us, ue = action[2]
                            ph.write(body_bytes[us:ue].hex() + "\n")
                            trec["ppus"].append(npu)
                            npu += 1
                rec["txs"].append(trec)
                nt += 1
            index.append(rec)
    with open(f"{out}/index.jsonl", "w") as fh:
        for r in index:
            fh.write(json.dumps(r) + "\n")
    print(f"cut {len(index)} blocks, {nt} transactions, {npu} parameter updates")


def plant(kind, facts):
    """Replaces the one served value PLANT names, as `KIND:KEY:INDEX`, to show the comparison refuses it."""
    if PLANT and not PLANT.startswith("params:"):
        k, key, i = PLANT.split(":")
        if k == kind and plant.seen == int(i) and key in facts:
            facts[key] = "planted"
        if k == kind:
            plant.seen += 1
    return facts


plant.seen = 0


def compare(out):
    blocks = load_blocks(out, "slim.jsonl")
    index = list(lines(f"{out}/index.jsonl"))
    ob = lines(f"{out}/block.oracle.jsonl")
    ot = lines(f"{out}/tx.oracle.jsonl")
    op = list(lines(f"{out}/ppu.oracle.jsonl"))
    seen_tx_lines = 0
    agree, dis, no_field, by_def = Counter(), [], Counter(), Counter()
    refused = Counter()
    linked = Counter()

    def verdict(kind, key, ledger, served, where, cls=None):
        if cls is None and ledger == served:
            agree[f"{kind}.{key}"] += 1
            return
        dis.append(dict(where, kind=kind, key=key, ledger=ledger, served=served, cls=cls or "served fact"))

    nblocks = 0
    eb_served = 0
    for blk, rec, o in zip(blocks, index, ob):
        nblocks += 1
        card = blk["cardano"]
        where = dict(slot=int(card["header"].get("slot", 0)))
        tos = []
        for trec in rec["txs"]:
            assert trec["line"] == seen_tx_lines, "transaction oracle lines out of step"
            tos.append(next(ot))
            seen_tx_lines += 1
        if not o["ok"]:
            refused["block"] += 1
            dis.append(dict(where, kind="block", cls="ledger refuses the served block bytes", ledger=o["err"]))
            continue
        lf = o["facts"]
        sf = plant("block", block_facts(card))
        linked["header hash of the served bytes equals the served hash"] += rec["header_hash"] == sf["hash"]
        certifying = bool(lf["contains_leios_cert"] and lf["leios_certificate"])
        if rec["inlined"]:
            # The served body is longer than the header says, and the node's body was rebuilt from its tail.
            by_def[f"served body longer than the header body size, certifying {certifying}"] += 1
        else:
            linked["header body size equals the served body length"] += lf["body_size"] == rec["body_len"]
        for k, a in lf.items():
            if k in NO_V1_BLOCK_FIELD:
                no_field[f"block.{k}"] += 1
            elif k == "txs" and rec["inlined"] and certifying and a == 0 and sf[k] == rec["served_bytes_txs"]:
                # v1 defines the transactions of a certifying block as those of the certified endorser block.
                by_def["block.txs of a certifying block counts the endorser block transactions"] += 1
            elif k == "peras_certificate" and a and not sf[k] and rec["peras_raw"] == "40":
                verdict("block", k, a, sf[k], where, "an empty Peras certificate read as absent")
            else:
                verdict("block", k, a, sf[k], where)
        served_txs = card.get("body", {}).get("tx", [])
        if lf["contains_leios_cert"]:
            eb_served += len(served_txs)
        if len(served_txs) != rec["served_bytes_txs"]:
            dis.append(dict(where, kind="block", cls="served transaction count differs from the block body", key="txs"))
            continue
        for trec, to in zip(rec["txs"], tos):
            st = served_txs[trec["pos"]]
            twhere = dict(where, tx=b64(st["common"].get("hash")).hex(), source=trec["source"])
            linked[f"{trec['source']} body hash equals the served hash"] += trec["body_hash"] == twhere["tx"]
            if trec["body_hash"] != twhere["tx"]:
                dis.append(dict(twhere, kind="tx", cls="served hash differs from the body hash", key="hash"))
            if not to["ok"]:
                refused["tx"] += 1
                dis.append(dict(twhere, kind="tx", cls="ledger refuses the served transaction bytes", ledger=to["err"]))
                continue
            sf = plant("tx", tx_facts(st))
            for k, a in to["facts"].items():
                verdict("tx", k, a, sf.get(k, "<no field>"), twhere)
            served_ppus = [
                p["govAction"]["parameterChangeAction"].get("protocolParamUpdate", {})
                for p in st.get("common", {}).get("proposals", [])
                if "parameterChangeAction" in p.get("govAction", {})
            ]
            if len(served_ppus) != len(trec["ppus"]):
                dis.append(dict(twhere, kind="ppu", cls="served parameter change count differs", key="proposals"))
                continue
            for su, line in zip(served_ppus, trec["ppus"]):
                po = op[line]
                if not po["ok"]:
                    refused["ppu"] += 1
                    dis.append(dict(twhere, kind="ppu", cls="ledger refuses the served update bytes", ledger=po["err"]))
                    continue
                su = plant("ppu", dict(su))
                for k, hexv in po["facts"].items():
                    n = int(k[1:])
                    field = PPU_FIELDS.get(n)
                    served = su.get(field) if field else None
                    led = value(bytes.fromhex(hexv))
                    cls = ppu_verdict(n, led, served) if served != "planted" else "served fact"
                    if cls is None:
                        agree[f"ppu.{k}"] += 1
                    else:
                        dis.append(dict(twhere, kind="ppu", key=k, ledger=hexv, served=served, cls=cls))
                extra = sorted(set(su) - {PPU_FIELDS.get(int(k[1:])) for k in po["facts"]})
                for f in extra:
                    dis.append(dict(twhere, kind="ppu", key=f, ledger=None, served=su[f], cls="served a key the ledger does not read"))
    with open(f"{out}/dis.jsonl", "w") as fh:
        for d in dis:
            fh.write(json.dumps(d, default=str) + "\n")
    by_source = Counter(t["source"] for r in index for t in r["txs"])
    npu = sum(len(t["ppus"]) for r in index for t in r["txs"])
    assert next(ot, None) is None and next(ob, None) is None and nblocks == len(index), "oracle lines left over"
    print(
        f"sample: blocks {nblocks}, block body transactions {by_source['block']}, "
        f"endorser transactions {by_source['endorser']} of {eb_served} served, parameter updates {npu}"
    )
    print(f"ledger refuses: {dict(refused) or 0}")
    for k, v in sorted(linked.items()):
        print(f"linked {k}: {v}")
    peras = Counter("null" if r["peras_raw"] == "f6" else "empty bytes" if r["peras_raw"] == "40" else "other" for r in index)
    print(f"peras certificate field over every block: {dict(peras)}")
    hits = sum(r["tag24_indef_bytes"] for r in index)
    print(f"tag 24 over indefinite bytes, d8185f in every served block: {hits} in {sum(1 for r in index if r['tag24_indef_bytes'])} blocks")
    print(f"agree {sum(agree.values())} field values over {len(agree)} fields")
    for k in sorted(agree):
        print(f"  agree {agree[k]:6d}  {k}")
    for k, v in sorted(by_def.items()):
        print(f"  by the v1 definition {v:6d}  {k}")
    for k, v in sorted(no_field.items()):
        print(f"  no v1 field {v:6d}  {k}")
    by = Counter((d["cls"], d.get("kind"), d.get("key")) for d in dis)
    print(f"disagreements {len(dis)}")
    for (cls, kind, key), c in sorted(by.items(), key=lambda x: (-x[1], str(x[0]))):
        print(f"  {c:6d}  {cls}  {kind}.{key}")


# Node parameter names and the v1 field each maps to, with how the node writes the value.
NODE_PARAMS = {
    "utxoCostPerByte": ("coinsPerUtxoByte", "int"), "maxTxSize": ("maxTxSize", "int"),
    "txFeePerByte": ("minFeeCoefficient", "int"), "txFeeFixed": ("minFeeConstant", "int"),
    "maxBlockBodySize": ("maxBlockBodySize", "int"), "maxBlockHeaderSize": ("maxBlockHeaderSize", "int"),
    "stakeAddressDeposit": ("stakeKeyDeposit", "int"), "stakePoolDeposit": ("poolDeposit", "int"),
    "poolRetireMaxEpoch": ("poolRetirementEpochBound", "int"), "stakePoolTargetNum": ("desiredNumberOfPools", "int"),
    "poolPledgeInfluence": ("poolInfluence", "ratio"), "monetaryExpansion": ("monetaryExpansion", "ratio"),
    "treasuryCut": ("treasuryExpansion", "ratio"), "minPoolCost": ("minPoolCost", "int"),
    "protocolVersion": ("protocolVersion", "version"), "maxValueSize": ("maxValueSize", "int"),
    "collateralPercentage": ("collateralPercentage", "int"), "maxCollateralInputs": ("maxCollateralInputs", "int"),
    "costModels": ("costModels", "costs"), "executionUnitPrices": ("prices", "prices"),
    "maxTxExecutionUnits": ("maxExecutionUnitsPerTransaction", "exunits"),
    "maxBlockExecutionUnits": ("maxExecutionUnitsPerBlock", "exunits"),
    "minFeeRefScriptCostPerByte": ("minFeeScriptRefCostPerByte", "ratio"),
    "poolVotingThresholds": ("poolVotingThresholds", "pool"), "dRepVotingThresholds": ("drepVotingThresholds", "drep"),
    "committeeMinSize": ("minCommitteeSize", "int"), "committeeMaxTermLength": ("committeeTermLimit", "int"),
    "govActionLifetime": ("governanceActionValidityPeriod", "int"), "govActionDeposit": ("governanceActionDeposit", "int"),
    "dRepDeposit": ("drepDeposit", "int"), "dRepActivity": ("drepInactivityPeriod", "int"),
    "maxRefScriptSizePerBlock": ("maxRefScriptSizePerBlock", "int"), "maxRefScriptSizePerTx": ("maxRefScriptSizePerTx", "int"),
    "refScriptCostStride": ("refScriptCostStride", "int"), "refScriptCostMultiplier": ("refScriptCostMultiplier", "ratio"),
    "maxPledgeLeverage": ("maxPledgeLeverage", "ratio"), "minPoolMargin": ("minPoolMargin", "ratio"),
    "leiosAnnouncementPeriodLength": ("leiosAnnouncementPeriodLength", "int"),
    "leiosVotePeriodLength": ("leiosVotePeriodLength", "int"),
    "leiosDiffusionPeriodLength": ("leiosDiffusionPeriodLength", "int"),
    "leiosCommitteeSize": ("leiosCommitteeSize", "int"), "leiosQuorumStakeThreshold": ("leiosQuorumStakeThreshold", "ratio"),
    "maxEndorserBlockReferencesSize": ("maxEndorserBlockReferencesSize", "int"),
    "maxEndorserBlockTxsSize": ("maxEndorserBlockTxsSize", "int"),
    "maxEndorserBlockExecutionUnits": ("maxEndorserBlockExecutionUnits", "exunits"),
    "maxRefScriptSizePerEndorserBlock": ("maxRefScriptSizePerEndorserBlock", "int"),
    "perasMinCandidateBlockAge": ("perasMinCandidateBlockAge", "int"), "perasHealingFactor": ("perasHealingFactor", "ratio"),
    "perasCertBoost": ("perasCertBoost", "int"), "perasTargetCommitteeSize": ("perasTargetCommitteeSize", "int"),
    "perasBootstrapRound": ("perasBootstrapRound", "int"),
    "perasQuorumThresholdSafetyMargin": ("perasQuorumThresholdSafetyMargin", "ratio"),
    "refInputsCostPerMultiAssetPolicy": ("refInputsCostPerMultiAssetPolicy", "int"),
    "refInputsCostPerDatumByte": ("refInputsCostPerDatumByte", "int"),
}
POOL_ORDER = ["motionNoConfidence", "committeeNormal", "committeeNoConfidence", "hardForkInitiation", "ppSecurityGroup"]
DREP_ORDER = [
    "motionNoConfidence", "committeeNormal", "committeeNoConfidence", "updateToConstitution", "hardForkInitiation",
    "ppNetworkGroup", "ppEconomicGroup", "ppTechnicalGroup", "ppGovGroup", "treasuryWithdrawal",
]
PERAS_AND_REF_INPUTS = {k for k in NODE_PARAMS if k.startswith("peras") or k.startswith("refInputs")}
# The `optional` scalars of v1 `PParams`, which the JSON leaves out when unset.
OPTIONAL_SCALARS = {
    "maxRefScriptSizePerBlock", "maxRefScriptSizePerTx", "refScriptCostStride", "leiosAnnouncementPeriodLength",
    "leiosVotePeriodLength", "leiosDiffusionPeriodLength", "leiosCommitteeSize", "maxEndorserBlockReferencesSize",
    "maxEndorserBlockTxsSize", "maxRefScriptSizePerEndorserBlock", "perasMinCandidateBlockAge", "perasCertBoost",
    "perasTargetCommitteeSize", "perasBootstrapRound",
}


def ratio(x):
    if x is None:
        return None
    return Fraction(int(x.get("numerator", 0)), int(x.get("denominator", 1)))


def node_ratio(x):
    return None if x is None else Fraction(x)


def param_equal(how, node, served):
    if node is None:
        return served is None
    if how == "int":
        served = bigint(served) if isinstance(served, dict) else opt(served)
        return served == node
    if how == "ratio":
        return ratio(served) == node_ratio(node)
    if how == "version":
        return served is not None and [int(served.get("major", 0)), int(served.get("minor", 0))] == [node["major"], node["minor"]]
    if how == "exunits":
        return exunits(served) == [node["memory"], node["steps"]]
    if how == "prices":
        return served is not None and [ratio(served.get("memory")), ratio(served.get("steps"))] == [
            node_ratio(node["priceMemory"]), node_ratio(node["priceSteps"])]
    if how in ("pool", "drep"):
        order = POOL_ORDER if how == "pool" else DREP_ORDER
        got = [ratio(t) for t in (served or {}).get("thresholds", [])]
        return got == [node_ratio(node[k]) for k in order]
    if how == "costs":
        names = {"PlutusV1": "plutusV1", "PlutusV2": "plutusV2", "PlutusV3": "plutusV3", "PlutusV4": "plutusV4"}
        got = {k: [int(x) for x in (v or {}).get("values", [])] for k, v in (served or {}).items() if v}
        return got == {names[k]: v for k, v in node.items()}
    raise ValueError(how)


def params(addr, node_path, out):
    os.makedirs(out, exist_ok=True)
    res = call(addr, "utxorpc.v1.query.QueryService/ReadParams", {}, defaults=True)
    with open(f"{out}/readparams.json", "w") as fh:
        json.dump(res, fh, indent=1)
    served = res["values"]["cardano"]
    if PLANT.startswith("params:"):
        served[PLANT.split(":")[1]] = {"int": "1"} if isinstance(served.get(PLANT.split(":")[1]), dict) else "1"
    with open(node_path) as fh:
        node = json.load(fh, parse_float=Decimal)
    print(f"ledger tip {res.get('ledgerTip')}")
    params_verdicts(node, served)


def params_verdicts(node, served):
    agree, dis = [], []
    for k in sorted(node):
        if k not in NODE_PARAMS:
            dis.append((k, "node parameter with no v1 field", node[k], None))
            continue
        field, how = NODE_PARAMS[k]
        if field not in served and field in OPTIONAL_SCALARS:
            # An unset optional scalar is left out of the JSON, and it agrees only with a node null.
            if node[k] is None:
                agree.append(k)
            else:
                dis.append((k, "served fact", node[k], None))
        elif field not in served:
            dis.append((k, "served message lacks the field", node[k], None))
        elif param_equal(how, node[k], served[field]):
            agree.append(k)
        else:
            dis.append((k, "served fact", node[k], served[field]))
    unmapped = sorted(set(served) - {f for f, _ in NODE_PARAMS.values()})
    print(f"params: node {len(node)}, served fields {len(served)}, agree {len(agree)}, disagree {len(dis)}, served fields no node key {unmapped}")
    pr = [k for k in agree if k in PERAS_AND_REF_INPUTS]
    print(f"peras and reference input params agree {len(pr)} of {len(PERAS_AND_REF_INPUTS)}: {sorted(pr)}")
    for k, cls, a, b in dis:
        print(f"  disagree {k}: {cls}, node {a}, served {b}")


def main():
    cmd = sys.argv[1]
    if cmd == "dump":
        dump(sys.argv[2], sys.argv[3])
    elif cmd == "fetch":
        fetch(sys.argv[2], sys.argv[3], [int(x) for x in sys.argv[4:]])
    elif cmd == "cut":
        cut(sys.argv[2], sys.argv[3], {int(x) for x in sys.argv[4:]})
    elif cmd == "compare":
        compare(sys.argv[2])
    elif cmd == "params":
        params(sys.argv[2], sys.argv[3], sys.argv[4])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
