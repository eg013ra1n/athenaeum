#!/usr/bin/env python3
"""Stage table + per-frame sub-stage medians for one acceptance run.
usage: tier1-extract.py <work-dir>  (reads <work-dir>/logs/*.jsonl and <work-dir>/run.txt)"""
import glob, json, statistics, sys, re, os
W = sys.argv[1]
rows = []
for p in sorted(glob.glob(os.path.join(W, "logs", "*.jsonl"))):
    for line in open(p, errors="replace"):
        try: rows.append(json.loads(line))
        except Exception: pass
def F(r): return r.get("fields", {})
# the run id: the last "stacking run started" in these logs
starts = [r for r in rows if F(r).get("message") == "stacking run started"]
run_id = F(starts[-1]).get("run_id") if starts else None
print(f"run_id={run_id}")
print(open(os.path.join(W, "run.txt")).read().strip())
print("\n== stages (ms) ==")
for r in rows:
    f = F(r)
    if f.get("message") == "stacking stage finished" and f.get("run_id") == run_id:
        print(f"{f.get('stage'):>10}  {f.get('duration_ms')/60000:7.2f} min")
for r in rows:
    f = F(r)
    if f.get("message") == "stacking run finished" and f.get("run_id") == run_id:
        print(f"{'TOTAL':>10}  {f.get('duration_ms')/60000:7.2f} min  outcome={f.get('outcome')}")
print("\n== fan-out admitted ==")
for r in rows:
    f = F(r)
    if f.get("message") == "fan-out admitted" and f.get("run_id") == run_id:
        print(f"  {f.get('stage'):>10} {f.get('group_key'):<32} admission={f.get('admission')} ws={f.get('working_set_bytes')} shared={f.get('shared_bytes')} pool={f.get('pool_threads')}")
print("\n== per-frame sub-stage medians (ms) ==")
EVENTS = {
  "light calibrated": ["compute_ms","cosmetic_ms","debayer_ms","write_ms"],
  "frame plane measured": ["read_ms","background_ms","noise_ms","detect_ms","fit_ms","duration_ms"],
  "frame stars detected": ["read_ms","detect_ms"],
  "frame registered": ["read_ms","detect_ms","align_ms","duration_ms"],
  "ln frame normalized": ["warp_ms","background_ms","scale_ms","write_ms"],
  "plane integrated": ["read_ms","combine_ms","duration_ms"],
  "drizzle plane deposited": ["read_ms","deposit_ms","duration_ms"],
}
# only rows after the run start timestamp
t_start = starts[-1]["timestamp"] if starts else ""
for ev, keys in EVENTS.items():
    sel = [F(r) for r in rows if F(r).get("message") == ev and r.get("timestamp","") >= t_start]
    if not sel: print(f"  {ev}: none"); continue
    out = []
    for k in keys:
        vals = [f[k] for f in sel if isinstance(f.get(k), (int, float))]
        if vals: out.append(f"{k}={statistics.median(vals):.0f}")
    print(f"  {ev} (n={len(sel)}): " + " ".join(out))
