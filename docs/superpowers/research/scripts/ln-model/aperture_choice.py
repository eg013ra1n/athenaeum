#!/usr/bin/env python3
"""Choose the F1 aperture (2F, 3F or growth curve) by spec 2026-10-04 section 4.3.

    aperture_choice.py <dataset>=<probe.jsonl>:<truth.csv> [<dataset>=<probe.jsonl>:<truth.csv> ...]
                       [--sim sim-stars.csv]

<probe.jsonl> is `ln_scale_probe`'s main-mode output (one line per member, an "arms" object per
line: psfOff, psfSel, f1_2, f1_3, f1_gc). <truth.csv> is a table with `handle`, `phot` and
optionally `plane` (the audit's decomp / truth csv); a probe label `f<id>` / `h<id>` joins on
`handle == id` and the line's `plane`. A member with no truth row is skipped (its count is printed).

A dataset name is `<group>` or `<group>/mono` / `<group>/osc`; repeating a name concatenates the
files (an OSC group is three planes, one probe file each). The mono-vs-OSC offset is taken between
`<group>/mono` and `<group>/osc`.

Step 1 (per variant = every arm named `f1_*`, on EVERY dataset): median |gap| <= 0.5 %,
|slope| <= 0.5 %/px^2, and where a group has both, mono and OSC median gaps within 0.5 % of each
other. gap = s_arm / s_truth - 1. The GATED slope is against the audit's dS2 = sigma_member^2 -
sigma_reference^2 with sigma = FWHM / 2.3548 from the truth rows' half-maximum FWHM (`fwhm_mem` /
`fwhm_ref`), the basis the 0.5 %/px^2 bar was set on; a dataset whose truth rows lack them falls back
to the fit basis, loudly. The slope against the Moffat-fit FWHMs (`refFwhmFit` / `tgtFwhmFit`,
dS2 = (F_T^2 - F_R^2) / (8 ln 2)) is printed beside it as "reported". The statistics are
`ln-scale-audit/evaluate.py:gap_stats`, imported, not re-implemented. The brightness-tercile trend (median over members of
(t1 - t3) / s) is printed and flagged above 1 %; it is not a step-1 bar (spec section 3 raises
`maxStars` on it, Task 16).

Step 2 (`--sim`, the `ln_scale_probe --stack-sim` stars csv of set 204 mono, arms in its columns):
each survivor's no-rejection master star flux against the truth arm (`sim_<arm>_norej` /
`sim_cor_norej`, median over stars); the survivor closest to the truth arm wins. Step 3: survivors
whose step-2 flux ratios are within 0.3 % of the best's (percentage points) are tied; a tie goes to
the smaller median per-member relative dispersion sigma/s (pooled over every dataset; each dataset's
own ordering is printed beside it), then to the smaller aperture (growth curve counts as its 2F
aperture). Variants whose sigma/s agree to 1e-9 relative share one sample (2F / growth curve differ
only by the factor c): that tie is decided by step 2. Ends with `aperture choice: <variant>` or `none passes`.
"""
import csv
import importlib.util
import json
import math
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("ln_scale_audit_evaluate", HERE.parent / "ln-scale-audit" / "evaluate.py")
_evaluate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_evaluate)
gap_stats = _evaluate.gap_stats

MEDIAN_GAP_BAR = 0.5  # %, step 1
SLOPE_BAR = 0.5  # %/px^2, step 1
OFFSET_BAR = 0.5  # %, mono vs OSC median gap, step 1
TERCILE_FLAG = 1.0  # %, flagged only
TIE_WINDOW = 0.3  # % of master star flux, step 3
FWHM_TO_SIGMA2 = 8.0 * math.log(2.0)


def die(msg):
    sys.exit(f"aperture_choice: {msg}")


def fnum(v):
    """float or nan for a csv cell / json value that may be empty or null."""
    try:
        return float(v)
    except (TypeError, ValueError):
        return float("nan")


def sigma2(fwhm):
    return (fwhm / 2.3548) ** 2


def load_dataset(probe_path, truth_path):
    """-> (members, counts). counts: no_truth, arms_error, null_fwhm (members kept for the gap, skipped for the slope)."""
    truth = {}
    for r in csv.DictReader(open(truth_path)):
        if not r.get("phot"):
            continue
        truth[(int(r["handle"]), int(r.get("plane") or 0))] = r
    members = []
    counts = dict(no_truth=0, arms_error=0, null_fwhm=0)
    for line in open(probe_path):
        line = line.strip()
        if not line:
            continue
        j = json.loads(line)
        if "arms" not in j or not isinstance(j["arms"], dict) or "error" in j["arms"]:
            counts["arms_error"] += 1
            continue
        key = (int(j["label"][1:]), int(j["plane"]))
        if key not in truth:
            counts["no_truth"] += 1
            continue
        t = truth[key]
        tf, rf = fnum(j.get("tgtFwhmFit")), fnum(j.get("refFwhmFit"))
        ds2_fit = (tf ** 2 - rf ** 2) / FWHM_TO_SIGMA2
        if not math.isfinite(ds2_fit):
            counts["null_fwhm"] += 1
        ds2_hm = sigma2(fnum(t.get("fwhm_mem"))) - sigma2(fnum(t.get("fwhm_ref")))
        members.append(dict(key=key, phot=float(t["phot"]), ds2=ds2_fit, ds2_hm=ds2_hm, arms=j["arms"]))
    return members, counts


def arm_arrays(members, arm):
    """-> dict of arrays over the members that carry the arm."""
    rows = [m for m in members if isinstance(m["arms"].get(arm), dict) and "s" in m["arms"][arm]]
    f = lambda k: np.array([np.nan if m["arms"][arm].get(k) is None else m["arms"][arm][k] for m in rows], float)
    s, sigma, ap = f("s"), f("sigma"), f("aperturePx")
    t = np.array([[np.nan if v is None else v for v in m["arms"][arm]["terciles"]] for m in rows], float).reshape(-1, 3)
    return dict(s=s, sigma=sigma, rel=sigma / s, ap=ap, trend=(t[:, 0] - t[:, 2]) / s * 100,
                phot=np.array([m["phot"] for m in rows]), ds2=np.array([m["ds2"] for m in rows]),
                ds2_hm=np.array([m["ds2_hm"] for m in rows]))


def slope_of(a, which):
    """gap slope (%/px^2) against dS2 `which` over the members where it is finite; the other gap
    statistics always use every member (gap_stats on the finite subset would drop them)."""
    ok = np.isfinite(a[which]) & np.isfinite(a["s"] / a["phot"])
    if ok.sum() < 3:
        return float("nan")
    return gap_stats(a["s"][ok], a["phot"][ok], a[which][ok])["slope"]


def parse_args(argv):
    sim = None
    specs = []
    it = iter(argv)
    for a in it:
        if a == "--sim":
            sim = next(it, None)
            if sim is None:
                die("--sim needs a stars csv")
        elif "=" in a and ":" in a.split("=", 1)[1]:
            name, rest = a.split("=", 1)
            probe, truth = rest.rsplit(":", 1)
            specs.append((name, probe, truth))
        else:
            die(f"cannot read argument {a!r} (want <dataset>=<probe.jsonl>:<truth.csv> or --sim <csv>)")
    if not specs:
        die(__doc__.strip().splitlines()[0] + "\nusage: aperture_choice.py <dataset>=<probe.jsonl>:<truth.csv> ... [--sim sim-stars.csv]")
    return specs, sim


def main():
    specs, sim_path = parse_args(sys.argv[1:])
    datasets = {}
    for name, probe, truth in specs:
        members, counts = load_dataset(probe, truth)
        d = datasets.setdefault(name, dict(members=[], counts=dict(no_truth=0, arms_error=0, null_fwhm=0)))
        d["members"] += members
        for k, v in counts.items():
            d["counts"][k] += v
    arm_names = sorted({a for d in datasets.values() for m in d["members"] for a in m["arms"]})
    variants = [a for a in arm_names if a.startswith("f1_")]
    if not variants:
        die("no f1_* arm in the probe output")

    print("datasets:")
    basis = {}
    for name, d in datasets.items():
        c = d["counts"]
        hm_ok = all(math.isfinite(m["ds2_hm"]) for m in d["members"])
        basis[name] = "ds2_hm" if hm_ok else "ds2"
        print(f"   {name:24s} {len(d['members'])} member-planes with truth; skipped: no truth {c['no_truth']}, "
              f"arms error {c['arms_error']}, null FWHM {c['null_fwhm']} (slope only)")
        if not hm_ok:
            print(f"   !! {name}: the truth rows carry no half-maximum FWHM (fwhm_mem / fwhm_ref) for every member; "
                  "the GATED slope falls back to the Moffat-fit FWHM basis, which is NOT the basis the bar was set on")

    stats = {}  # (dataset, arm) -> dict
    for name, d in datasets.items():
        for arm in arm_names:
            a = arm_arrays(d["members"], arm)
            if len(a["s"]) < 3:
                continue
            st = gap_stats(a["s"], a["phot"], np.where(np.isfinite(a["ds2"]), a["ds2"], 0.0))
            st["slope_fit"] = slope_of(a, "ds2")
            st["slope"] = slope_of(a, basis[name])
            st["sigma"] = float(np.nanmedian(a["sigma"]))
            st["rel"] = float(np.nanmedian(a["rel"]))
            st["trend"] = float(np.nanmedian(a["trend"]))
            st["aperture"] = float(np.nanmedian(a["ap"])) if np.isfinite(a["ap"]).any() else float("nan")
            st["rel_all"] = a["rel"]
            stats[(name, arm)] = st

    print("\nper arm and dataset (gap = s_arm / s_truth - 1, %; slope = GATED slope in %/px^2 on the half-max basis "
          "(fit-FWHM slope reported beside it); sigma = median per-member RCR dispersion, sig/s its relative "
          f"value; tercile = median (t1 - t3)/s in %, ! above {TERCILE_FLAG:g}):")
    print(f"   {'arm':8s} {'dataset':22s} {'n':>4s} {'median':>7s} {'med|gap|':>8s} {'slope':>7s} {'fit-rep':>8s} "
          f"{'sigma':>7s} {'sig/s':>8s} {'tercile':>8s}")
    for arm in arm_names:
        for name in datasets:
            st = stats.get((name, arm))
            if not st:
                continue
            flag = "!" if abs(st["trend"]) > TERCILE_FLAG else " "
            print(f"   {arm:8s} {name:22s} {st['n']:4d} {st['median']:7.2f} {st['med_abs']:8.2f} {st['slope']:7.2f} "
                  f"{st['slope_fit']:8.2f} {st['sigma']:7.4f} {st['rel']:8.5f} {st['trend']:7.2f}{flag}")

    groups = {}
    for name in datasets:
        if "/" in name:
            g, kind = name.rsplit("/", 1)
            if kind in ("mono", "osc"):
                groups.setdefault(g, {})[kind] = name
    offsets = {}  # (arm, dataset) -> offset
    pairs = {g: k for g, k in groups.items() if "mono" in k and "osc" in k}
    if pairs:
        print("\nmono-vs-OSC median gap offset (%, mono - OSC):")
        for g, k in pairs.items():
            for arm in arm_names:
                a, b = stats.get((k["mono"], arm)), stats.get((k["osc"], arm))
                if a and b:
                    off = a["median"] - b["median"]
                    offsets[(arm, k["mono"])] = offsets[(arm, k["osc"])] = off
                    print(f"   {arm:8s} {g:22s} {off:7.2f}")

    def passes(arm):
        """-> (ok, [reasons]) over every dataset."""
        why = []
        for name in datasets:
            st = stats.get((name, arm))
            if st is None:
                why.append(f"{name}: no data")
                continue
            if st["med_abs"] > MEDIAN_GAP_BAR:
                why.append(f"{name}: median |gap| {st['med_abs']:.2f} > {MEDIAN_GAP_BAR}")
            if not abs(st["slope"]) <= SLOPE_BAR:
                why.append(f"{name}: |slope| {abs(st['slope']):.2f} > {SLOPE_BAR}")
            off = offsets.get((arm, name))
            if off is not None and abs(off) > OFFSET_BAR:
                why.append(f"{name}: mono-vs-OSC offset {off:+.2f} beyond {OFFSET_BAR}")
        return not why, why

    print("\nstep 1 (every dataset: median |gap| <= %g %%, |slope| <= %g %%/px^2, mono-vs-OSC offset <= %g %%):"
          % (MEDIAN_GAP_BAR, SLOPE_BAR, OFFSET_BAR))
    survivors = []
    for arm in arm_names:
        ok, why = passes(arm)
        kind = "variant" if arm in variants else "baseline"
        print(f"   {arm:8s} ({kind}) {'PASS' if ok else 'fail'}" + ("" if ok else "  - " + "; ".join(why)))
        if ok and arm in variants:
            survivors.append(arm)
    print("   F1 variants passing step 1 on every dataset: " + (", ".join(survivors) if survivors else "none"))

    if not survivors:
        print("aperture choice: none passes")
        return

    if sim_path is None:
        if len(survivors) == 1:
            print("(no --sim: a single survivor needs no step 2)")
            print(f"aperture choice: {survivors[0]}")
        else:
            print("(no --sim: step 2 not run)")
            print("aperture choice: undecided between " + ", ".join(survivors) + " (rerun with --sim)")
        return

    with open(sim_path) as fh:
        head = next(csv.reader(fh))
    for need in ["ref", "sim_cor_norej"] + [f"sim_{a}_norej" for a in survivors]:
        if need not in head:
            die(f"{sim_path}: no column {need} (was the stack-sim run with these arms?)")
    cols = ["ref", "sim_cor_norej"] + [f"sim_{a}_norej" for a in survivors]
    rows = [[float(r[c]) for c in cols] for r in csv.DictReader(open(sim_path))]
    m = np.array(rows)
    ok = np.all(np.isfinite(m) & (m > 0), axis=1)
    m = m[ok]
    print(f"\nstep 2 ({sim_path}, {int(ok.sum())} stars): no-rejection master star flux, arm / truth-corrected arm - 1 (%), median;"
          " ratio to the LN reference reported beside it:")
    dev = {}
    for i, arm in enumerate(survivors):
        col = m[:, 2 + i]
        dev[arm] = (float(np.median(col / m[:, 1])) - 1) * 100
        print(f"   {arm:8s} {dev[arm]:+7.3f} %   (arm / LN reference {float(np.median(col / m[:, 0])):.4f})")
    print(f"   truth-corrected arm / LN reference {float(np.median(m[:, 1] / m[:, 0])):.4f}")
    best = min(survivors, key=lambda a: abs(dev[a]))
    tied = [a for a in survivors if abs(dev[a] - dev[best]) <= TIE_WINDOW]
    print(f"step 2: closest to the truth arm: {best}; within {TIE_WINDOW} % of it (tied): {', '.join(tied)}")
    pick = best
    if len(tied) > 1:
        def pooled(a):
            return float(np.nanmedian(np.concatenate([stats[(n, a)]["rel_all"] for n in datasets if (n, a) in stats])))

        def aper(a):
            aps = [stats[(n, a)]["aperture"] for n in datasets if (n, a) in stats]
            return float(np.nanmedian(aps)) if np.isfinite(aps).any() else float("inf")
        rel = {a: pooled(a) for a in tied}
        print("step 3 tie-break (median per-member sigma/s pooled over datasets, then aperture, growth curve = its 2F):")
        for a in sorted(tied, key=lambda a: rel[a]):
            per = ", ".join(f"{n}: {stats[(n, a)]['rel']:.5f}" for n in datasets if (n, a) in stats)
            print(f"   {a:8s} pooled sigma/s {rel[a]:.6f}  aperture {aper(a):.3f} px  per dataset [{per}]")
        for n in datasets:
            order = sorted((a for a in tied if (n, a) in stats), key=lambda a: stats[(n, a)]["rel"])
            print(f"   sigma/s ordering on {n}: " + " < ".join(order))
        best_rel = min(rel.values())
        left = [a for a in tied if rel[a] <= best_rel * (1 + 1e-9)]
        if len(left) > 1:
            best_ap = min(aper(a) for a in left)
            left = [a for a in left if aper(a) <= best_ap * (1 + 1e-9)]
        if len(left) > 1:
            print("same-sample tie (2F / growth curve): decided by step 2")
            left = [min(left, key=lambda a: abs(dev[a]))]
        pick = left[0]
    print(f"aperture choice: {pick}")


if __name__ == "__main__":
    main()
