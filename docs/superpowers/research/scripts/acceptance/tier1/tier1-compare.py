#!/usr/bin/env python3
"""Byte-compare two acceptance runs' outputs: masters (raw, then header card SET + data bytes),
registration_results rows, .athln sidecars. usage: tier1-compare.py <W_base> <W_head>"""
import glob, os, sqlite3, sys, hashlib
A, B = sys.argv[1], sys.argv[2]
def fits_split(path):
    data = open(path, "rb").read()
    cards = set(); off = 0
    while True:
        block = data[off:off+2880]; off += 2880
        done = False
        for i in range(0, 2880, 80):
            c = block[i:i+80].decode("latin1")
            if c.startswith("END") and c.strip() == "END": done = True; break
            if c.strip(): cards.add(c)
        if done or off >= len(data): break
    return cards, hashlib.blake2b(data[off:]).hexdigest(), len(data) - off
ok = True
print("== masters ==")
for pa in sorted(glob.glob(os.path.join(A, "stackout", "*.fits"))):
    pb = os.path.join(B, "stackout", os.path.basename(pa))
    if not os.path.exists(pb): print(f"  MISSING in head: {os.path.basename(pa)}"); ok = False; continue
    raw = open(pa,"rb").read() == open(pb,"rb").read()
    ca, da, na = fits_split(pa); cb, db_, nb = fits_split(pb)
    print(f"  {os.path.basename(pa)}: raw_identical={raw} data_identical={da==db_} ({na} B) cards_equal={ca==cb} (+{len(cb-ca)} -{len(ca-cb)})")
    if da != db_: ok = False
    if ca != cb:
        for c in sorted(ca - cb)[:5]: print("     base-only:", c.strip())
        for c in sorted(cb - ca)[:5]: print("     head-only:", c.strip())
print("== registration_results ==")
def reg(w):
    db = sqlite3.connect(os.path.join(w, "athenaeum.db"))
    return db.execute("select frame_id, transform_json from registration_results order by frame_id").fetchall()
ra, rb = reg(A), reg(B)
diff = [ (x[0]) for x, y in zip(ra, rb) if x != y ]
print(f"  rows base={len(ra)} head={len(rb)} differing={len(diff)} {diff[:10]}")
if len(ra) != len(rb) or diff: ok = False
print("== .athln sidecars ==")
sa = sorted(glob.glob(os.path.join(A, "stackwork", "**", "*.athln"), recursive=True))
nd = 0; missing = 0
for pa in sa:
    pb = pa.replace(A, B, 1)
    if not os.path.exists(pb): missing += 1; continue
    if open(pa,"rb").read() != open(pb,"rb").read(): nd += 1
print(f"  sidecars base={len(sa)} missing_in_head={missing} differing={nd}")
if missing or nd: ok = False
print("== calibrated frames (pixel data + card set) ==")
ca_ = sorted(glob.glob(os.path.join(A, "stackwork", "**", "calibrated", "**", "*.fits"), recursive=True))
ndat = 0; ncard = 0; miss = 0
for pa in ca_:
    pb = pa.replace(A, B, 1)
    if not os.path.exists(pb): miss += 1; continue
    ca, da, _ = fits_split(pa); cb, db_, _ = fits_split(pb)
    if da != db_: ndat += 1
    if ca != cb: ncard += 1
print(f"  calibrated base={len(ca_)} missing_in_head={miss} data_differing={ndat} cardset_differing={ncard}")
if miss or ndat: ok = False
print("RESULT:", "IDENTICAL" if ok else "DIFFERENCES FOUND")
