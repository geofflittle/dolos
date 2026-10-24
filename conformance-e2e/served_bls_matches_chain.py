"""Compare each pool's served BLS key with the key its registration wrote on chain.

Usage: python3 -I served_bls_matches_chain.py <ReadState distribution json> <minibf base url>
Expected verdict line on the w42 chain: "VERDICT every served key equals its chain bytes".
"""

import base64
import json
import sys
import urllib.request

import cbor2


def get(url):
    with urllib.request.urlopen(url) as response:
        return json.load(response)


def registration_key(base, operator):
    updates = get(f"{base}/pools/{operator.hex()}/updates")
    latest = [u for u in updates if u["action"] == "registered"][-1]
    raw = bytes.fromhex(get(f"{base}/txs/{latest['tx_hash']}/cbor")["cbor"])
    certs = cbor2.loads(raw)[0][4]
    certs = certs.value if isinstance(certs, cbor2.CBORTag) else certs
    cert = next(c for c in certs if c[0] == 3 and c[1] == operator)
    return cert[3]


def main():
    served = json.load(open(sys.argv[1]))["result"]["cardano"]["stakePoolDistribution"]["pools"]
    base = sys.argv[2]
    equal, differ, unkeyed = 0, [], []
    for pool in served:
        operator = base64.b64decode(pool["poolKeyhash"])
        key = pool.get("blsKey")
        chain = registration_key(base, operator)
        if key is None:
            unkeyed.append((operator.hex(), chain is not None))
            continue
        pair = (base64.b64decode(key["blsPubkey"]), base64.b64decode(key["blsPossessionProof"]))
        if chain is not None and tuple(chain) == pair:
            equal += 1
        else:
            differ.append(operator.hex())
    print(f"pools {len(served)} keyed_equal {equal} keyed_differ {len(differ)} unkeyed {len(unkeyed)}")
    for operator, chain_has_key in unkeyed:
        print(f"unkeyed {operator} chain_wrote_key={chain_has_key}")
    for operator in differ:
        print(f"differ {operator}")
    if not differ and all(not chain_has_key for _, chain_has_key in unkeyed):
        print("VERDICT every served key equals its chain bytes")
    else:
        print("VERDICT a served key disagrees with the chain")


main()
