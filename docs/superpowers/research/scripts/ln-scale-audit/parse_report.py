"""grid-report.txt --ln-truth rows -> CSV (ln-scale-audit)."""
import re, sys, csv, sqlite3
rep, db, out = sys.argv[1], sys.argv[2], sys.argv[3]
pat = re.compile(r"handle (\d+) plane (\d+) \[(.+?)\]: ln_scale ([\d.]+) x k ([\d.]+) = ([\d.]+) \((\w+)\)\s+photometric ([\d.]+) \+- ([\d.]+) \(bootstrap SE, n (\d+), (\w+), FWHM ref ([\d.]+) mem ([\d.]+) px.*rel diff ([+-][\d.]+) %")
con = sqlite3.connect(db)
paths = {}
for h, g, p in con.execute("SELECT project_frame, group_key, path FROM stacking_artifacts WHERE kind='registered' AND project_frame IS NOT NULL"):
    paths[h] = (g, p)
rows = []
for line in open(rep):
    m = pat.search(line)
    if not m:
        continue
    h = int(m.group(1))
    g, p = paths.get(h, ("", ""))
    rows.append(dict(handle=h, plane=int(m.group(2)), instr=m.group(3), ln_scale=float(m.group(4)), k=float(m.group(5)),
                     applied=float(m.group(6)), source=m.group(7), phot=float(m.group(8)), se=float(m.group(9)), n=int(m.group(10)),
                     mode=m.group(11), fwhm_ref=float(m.group(12)), fwhm_mem=float(m.group(13)), diff_pct=float(m.group(14)), group=g, path=p))
with open(out, "w", newline="") as f:
    w = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
    w.writeheader(); w.writerows(rows)
print(len(rows), "rows")
