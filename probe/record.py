# Prints the outcome, phase2 flag, ex_units and redeemer evaluations of record n of a jsonl corpus read from stdin.
import sys, json
n = int(sys.argv[1])
for line in sys.stdin:
    r = json.loads(line)
    if r["n"] == n:
        o = r["outcome"]
        print("outcome", list(o)[0], json.dumps(o.get("refused", {}))[:600])
        print("phase2", r.get("phase2"), "ex_units", r.get("ex_units"), "min_fee", r.get("min_fee"))
        print("redeemers", json.dumps(r.get("redeemers"))[:800])
        break
