# Names the builtins a V4 cost model of N entries prices, from the Haskell V4 parameter names in order.
# Usage: python3 -I v4priced.py <V4.names> <N>
import sys

names = open(sys.argv[1]).read().split()
n = int(sys.argv[2])
first = {}
last = {}
for i, name in enumerate(names):
    builtin = name.split("'")[0]
    first.setdefault(builtin, i)
    last[builtin] = i
builtins = [b for b in first if b[0].isupper() and b not in ("CekStartupCost",) and not b.startswith("Cek")]
unpriced = [b for b in builtins if last[b] >= n]
split = [b for b in unpriced if first[b] < n]
print("PARAMS", len(names), "ENTRIES", n)
print("BUILTINS-WITH-PARAMS", len(builtins))
print("UNPRICED", len(unpriced), " ".join(unpriced))
print("SPLIT", split)
