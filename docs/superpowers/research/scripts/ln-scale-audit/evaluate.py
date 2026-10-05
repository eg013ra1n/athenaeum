"""Every candidate estimator against the photometric truth (ln-scale-audit)."""
import sys, csv, json, numpy as np


def gap_stats(s, phot, ds2):
    """Gap statistics of per-member scales `s` against the photometric truth `phot` (arrays over the
    same members): gap = s/phot - 1 in %, its median, median |gap|, p90 |gap|, sd, and the linear
    slope (%/px^2) and Pearson r against `ds2` over the members whose gap is finite. Shared with
    ln-model/aperture_choice.py, which must not re-implement it."""
    g = (s / phot - 1) * 100
    ok = np.isfinite(g)
    g2, x2 = g[ok], ds2[ok]
    sl = np.polyfit(x2, g2, 1)[0]; rr = np.corrcoef(x2, g2)[0, 1]
    return dict(median=np.median(g2), med_abs=np.median(np.abs(g2)), p90_abs=np.percentile(np.abs(g2), 90),
                sd=g2.std(), slope=sl, r=rr, n=int(ok.sum()))


def run(decomp, est, label):
    D = {(int(r["handle"]), int(r["plane"])): r for r in csv.DictReader(open(decomp)) if r.get("phot")}
    E = {}
    for line in open(est):
        j = json.loads(line)
        stem = j["file"].split(".p")[0]; plane = int(j["file"].split(".p")[1].split(".")[0])
        E[(int(stem[1:]), plane)] = j["est"]
    keys = sorted(D)
    ds2 = np.array([(float(D[k]["fwhm_mem"]) / 2.3548) ** 2 - (float(D[k]["fwhm_ref"]) / 2.3548) ** 2 for k in keys])
    phot = np.array([float(D[k]["phot"]) for k in keys])
    cands = {"production (applied)": np.array([float(D[k]["applied"]) for k in keys]),
             "production (detected, s_lib)": np.array([float(D[k]["s_lib"]) for k in keys]),
             "E: s_lib x CoG(c_t/c_r)": np.array([float(D[k]["s_lib"]) * float(D[k]["cog_t"]) / float(D[k]["cog_r"]) for k in keys])}
    for name in ("e1sky", "e2fit", "e3fit", "e2sky", "e3sky", "c1.5", "c2", "c3", "c4", "c5", "mtot"):
        for sub in ("all", "bright"):
            cands[f"{name} [{sub}]"] = np.array([E[k][name][sub] for k in keys])
    print(f"== {label}: n {len(keys)} member-planes; gap = s/phot - 1 in %; slope vs dS2 in %/px^2")
    print(f"   {'estimator':32s} {'median':>7s} {'med|g|':>7s} {'p90|g|':>7s} {'sd':>6s} {'slope':>7s} {'r':>6s}")
    for name, s in cands.items():
        st = gap_stats(s, phot, ds2)
        print(f"   {name:32s} {st['median']:7.2f} {st['med_abs']:7.2f} {st['p90_abs']:7.2f} {st['sd']:6.2f} {st['slope']:7.2f} {st['r']:6.2f}")
if __name__ == "__main__":
    for a, b, l in zip(sys.argv[1::3], sys.argv[2::3], sys.argv[3::3]):
        run(a, b, l)
