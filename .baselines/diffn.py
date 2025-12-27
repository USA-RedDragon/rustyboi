import json, sys, os
def load(p):
    d=json.load(open(p)); s=set()
    for r in d['failures']: s.add((os.path.basename(r['rom']), r['mode']))
    return d, s
od, old = load(sys.argv[1]); nd, new = load(sys.argv[2])
fixed = old - new; broke = new - old
print(f"OLD failed={od['failed']}  NEW failed={nd['failed']}  net={nd['failed']-od['failed']:+d}")
print(f"fixed={len(fixed)}  broke={len(broke)}")
print("=== BROKE (regressions) ===")
for n,m in sorted(broke): print(f"  {m:4} {n}")
