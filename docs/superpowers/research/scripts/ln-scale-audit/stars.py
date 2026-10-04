"""Per-star analysis of ln_scale_probe CSVs (ln-scale-audit 2026-10-04)."""
import sys, numpy as np

def load(path):
    with open(path) as f:
        head = f.readline().strip().split(",")
    m = np.genfromtxt(path, delimiter=",", skip_header=1)
    if m.ndim == 1:
        m = m[None, :]
    return {h: m[:, i] for i, h in enumerate(head)}

def flux(a, side, key, sky):
    return a[f"{side}_{key}_s"] - a[f"{side}_{key}_n"] * sky

def per_star(a):
    d = {}
    rs, ts = a["r_sky"], a["t_sky"]
    d["prod"] = a["r_sig"] / a["t_sig"]
    for g in ("e1", "e2", "e3"):
        d[f"{g}sky"] = flux(a, "r", g, rs) / flux(a, "t", g, ts)
        d[f"{g}fit"] = flux(a, "r", g, a["r_b"]) / flux(a, "t", g, a["t_b"])
    for c in ("c1", "c1.5", "c2", "c3", "c4", "c5"):
        d[c] = flux(a, "r", c, rs) / flux(a, "t", c, ts)
    d["mtot"] = a["r_mtot"] / a["t_mtot"]
    # enclosed fraction of the production ellipse (annulus sky) against the 4F circle
    d["EFr"] = flux(a, "r", "e1", rs) / flux(a, "r", "c4", rs)
    d["EFt"] = flux(a, "t", "e1", ts) / flux(a, "t", "c4", ts)
    # background term: (fit b - annulus sky) * N / flux(4F)
    d["BGr"] = (a["r_b"] - rs) * a["r_e1_n"] / flux(a, "r", "c4", rs)
    d["BGt"] = (a["t_b"] - ts) * a["t_e1_n"] / flux(a, "t", "c4", ts)
    d["bright"] = flux(a, "r", "c3", rs)
    return d

def summary(path, bins=(0.0, 0.02, 0.10, 0.30, 1.0), keys=("prod","e1sky","e2sky","e3sky","e2fit","e3fit","c2","c3","c4","c5","mtot","EFr","EFt","BGr","BGt")):
    a = load(path)
    # unsaturated: peak (amp + b) below 0.5 in both frames, finite everything
    ok = (a["r_amp"] + a["r_b"] < 0.5) & (a["t_amp"] + a["t_b"] < 0.5) & np.isfinite(a["r_sky"]) & np.isfinite(a["t_sky"])
    d = per_star(a)
    for k in keys:
        ok &= np.isfinite(d[k])
    br = d["bright"]
    order = np.argsort(-np.where(ok, br, -np.inf))
    n = int(ok.sum())
    out = {}
    print(f"{path}: {len(a['rx'])} pairs, {n} usable; ref FWHM fit med {np.median(a['r_fwhm']):.3f} tgt {np.median(a['t_fwhm']):.3f}")
    print("  bin(rank frac)      n  " + " ".join(f"{k:>7s}" for k in keys))
    for lo, hi in zip(bins[:-1], bins[1:]):
        idx = order[int(lo * n):max(int(hi * n), int(lo * n) + 1)]
        row = [np.median(d[k][idx]) for k in keys]
        out[(lo, hi)] = dict(zip(keys, row))
        print(f"  {lo:4.2f}-{hi:4.2f} {len(idx):7d}  " + " ".join(f"{v:7.4f}" for v in row))
    return out

if __name__ == "__main__":
    for p in sys.argv[1:]:
        summary(p)
