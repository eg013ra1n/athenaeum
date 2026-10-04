"""Join probe JSON + per-star CSVs + truth; decompose the LN scale gap (ln-scale-audit)."""
import sys, json, csv, numpy as np
sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parent))
from stars import load, per_star, flux

def iso_mask(a, ref, r_iso, frac=0.02):
    # no other reference fit brighter than frac*this star within r_iso px
    order = np.argsort(ref["y"]); ry, rx, rs = ref["y"][order], ref["x"][order], ref["sig"][order]
    out = np.ones(len(a["rx"]), bool)
    for i, (x, y) in enumerate(zip(a["rx"], a["ry"])):
        lo, hi = np.searchsorted(ry, y - r_iso), np.searchsorted(ry, y + r_iso)
        d2 = (rx[lo:hi] - x) ** 2 + (ry[lo:hi] - y) ** 2
        me = a["r_sig"][i]
        near = (d2 > 0.25) & (d2 <= r_iso * r_iso) & (rs[lo:hi] > frac * me)
        out[i] = not near.any()
    return out

def analyse(jsonl, truth_csv, stars_dir, plane_filter=None, nbright=150, r_iso_f=8.0):
    truth = {}
    for r in csv.DictReader(open(truth_csv)):
        truth[(int(r["handle"]), int(r["plane"]))] = r
    rows = []
    for line in open(jsonl):
        j = json.loads(line)
        h = int(j["label"][1:]); p = j["plane"]
        t = truth.get((h, p))
        a = load(f"{stars_dir}/{j['label']}.p{p}.csv")
        ref = load(f"{stars_dir}/reference.p{p}.csv")
        d = per_star(a)
        ok = (a["r_amp"] + a["r_b"] < 0.5) & (a["t_amp"] + a["t_b"] < 0.5)
        for k in ("prod", "e1sky", "c3", "c4", "c5", "EFr", "EFt", "BGr", "BGt", "e2sky", "e3sky", "mtot", "c2"):
            ok &= np.isfinite(d[k])
        F = j["F"]
        iso = iso_mask(a, ref, r_iso_f * F) & ok
        br = np.where(iso, d["bright"], -np.inf)
        sel = np.argsort(-br)[:min(nbright, int(iso.sum()))]
        brall = np.where(ok, d["bright"], -np.inf)
        selall = np.argsort(-brall)[:min(nbright, int(ok.sum()))]
        med = lambda k, s=sel: float(np.median(d[k][s])) if len(s) else float("nan")
        row = dict(handle=h, plane=p, beta=j["beta"], F=F, fwhm_fit_ref=j["refFwhmFit"], fwhm_fit_mem=j["tgtFwhmFit"],
                   s_lib=j["sLib"], s_rep=j["sRep"], s_ellsky_all=j["sEllipseAnnulusSky"], n_iso=len(sel),
                   prod_iso=med("prod"), e1sky_iso=med("e1sky"), e2sky_iso=med("e2sky"), e3sky_iso=med("e3sky"),
                   c2_iso=med("c2"), c3_iso=med("c3"), c4_iso=med("c4"), c5_iso=med("c5"), mtot_iso=med("mtot"),
                   EFr_iso=med("EFr"), EFt_iso=med("EFt"), BGr_all=float(np.median(d["BGr"][ok])), BGt_all=float(np.median(d["BGt"][ok])),
                   prod_bright=med("prod", selall), c5_bright=med("c5", selall),
                   cog_r=float(np.median((a["r_sig"] / flux(a, "r", "c4", a["r_sky"]))[sel])) if len(sel) else float("nan"),
                   cog_t=float(np.median((a["t_sig"] / flux(a, "t", "c4", a["t_sky"]))[sel])) if len(sel) else float("nan"))
        if t:
            row.update(applied=float(t["applied"]), phot=float(t["phot"]), se=float(t["se"]), mode=t["mode"],
                       fwhm_ref=float(t["fwhm_ref"]), fwhm_mem=float(t["fwhm_mem"]), diff_pct=float(t["diff_pct"]))
        rows.append(row)
    return rows

if __name__ == "__main__":
    rows = analyse(sys.argv[1], sys.argv[2], sys.argv[3])
    out = sys.argv[4]
    keys = sorted({k for r in rows for k in r})
    with open(out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=keys); w.writeheader(); w.writerows(rows)
    print(len(rows), "rows ->", out)
