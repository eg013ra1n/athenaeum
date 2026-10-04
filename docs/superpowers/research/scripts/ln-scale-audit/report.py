import sys, csv, numpy as np
rows = [r for r in csv.DictReader(open(sys.argv[1]))]
def f(r, k):
    try: return float(r[k])
    except: return float("nan")
rows = [r for r in rows if r.get("phot")]
rows.sort(key=lambda r: f(r, "fwhm_mem"))
s2 = lambda fw: (fw / 2.3548) ** 2
print(f"{'h':>4} {'p':>1} {'mode':>7} {'FWm':>5} {'fitFW':>5} {'dS2':>6} | {'applied':>7} {'s_lib':>7} {'phot':>7} | {'gap%':>6} {'libgap%':>7} | {'bg%':>6} {'ap%':>6} {'EFr':>6} {'EFt':>6} {'EFr/t%':>7} | {'c4iso':>6} {'c5iso':>6} {'c5/ph%':>6} {'n':>4}")
X, Y = {}, {}
for r in rows:
    ds2 = s2(f(r, "fwhm_mem")) - s2(f(r, "fwhm_ref"))
    gap = f(r, "applied") / f(r, "phot") - 1
    libgap = f(r, "s_lib") / f(r, "phot") - 1
    bg = f(r, "s_lib") / f(r, "s_ellsky_all") - 1          # fit background vs annulus sky (all matched stars)
    ap = f(r, "e1sky_iso") / f(r, "c4_iso") - 1              # FWTM ellipse vs 4F circle (bright isolated)
    efr = f(r, "EFr_iso") / f(r, "EFt_iso") - 1
    c5ph = f(r, "c5_iso") / f(r, "phot") - 1
    for k, v in (("gap", gap), ("libgap", libgap), ("bg", bg), ("ap", ap), ("efr", efr), ("c5ph", c5ph)):
        X.setdefault((k, r["mode"]), []).append(ds2); Y.setdefault((k, r["mode"]), []).append(v)
        X.setdefault((k, "all"), []).append(ds2); Y.setdefault((k, "all"), []).append(v)
    print(f"{r['handle']:>4} {r['plane']:>1} {r['mode']:>7} {f(r,'fwhm_mem'):5.2f} {f(r,'fwhm_fit_mem'):5.2f} {ds2:6.2f} | {f(r,'applied'):7.4f} {f(r,'s_lib'):7.4f} {f(r,'phot'):7.4f} | {gap*100:6.2f} {libgap*100:7.2f} | {bg*100:6.2f} {ap*100:6.2f} {f(r,'EFr_iso'):6.3f} {f(r,'EFt_iso'):6.3f} {efr*100:7.2f} | {f(r,'c4_iso'):6.4f} {f(r,'c5_iso'):6.4f} {c5ph*100:6.2f} {int(f(r,'n_iso')):4d}")
print("\nlinear fits y = a + b*dS2 (b in %/px^2), Pearson r:")
for (k, m) in sorted(X):
    x, y = np.array(X[(k, m)]), np.array(Y[(k, m)]) * 100
    ok = np.isfinite(x) & np.isfinite(y)
    x, y = x[ok], y[ok]
    if len(x) < 3: continue
    b, a = np.polyfit(x, y, 1)
    rr = np.corrcoef(x, y)[0, 1]
    print(f"  {k:7s} {m:8s} n {len(x):3d}  slope {b:6.2f}  intercept {a:6.2f}  r {rr:6.3f}  median {np.median(y):6.2f}")
