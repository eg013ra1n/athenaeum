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
other. gap = s_arm / s_truth - 1 and the slope against dS2 = (F_T^2 - F_R^2) / (8 ln 2) from the
probe line's refFwhmFit / tgtFwhmFit; the statistics are `ln-scale-audit/evaluate.py:gap_stats`,
imported, not re-implemented. The brightness-tercile trend (median over members of
(t1 - t3) / s) is printed and flagged above 1 %; it is not a step-1 bar (spec section 3 raises
`maxStars` on it, Task 16).

Step 2 (`--sim`, the `ln_scale_probe --stack-sim` stars csv of set 204 mono, arms in its columns):
each survivor's no-rejection master star flux against the truth arm (`sim_<arm>_norej` /
`sim_cor_norej`, median over stars); the survivor closest to the truth arm wins. Step 3: survivors
whose step-2 flux ratios are within 0.3 % of the best's (percentage points) are tied; a tie goes to
the smaller median per-member sigma (pooled over every dataset), then to the smaller aperture
(median aperturePx, then the name). Ends with `aperture choice: <variant>` or `none passes`.
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


def load_dataset(probe_path, truth_path):
    """-> list of per-member dicts joined to the truth, and the count of members without truth."""
    truth = {}
    for r in csv.DictReader(open(truth_path)):
        if not r.get("phot"):
            continue
        truth[(int(r["handle"]), int(r.get("plane") or 0))] = float(r["phot"])
    members, skipped = [], 0
    for line in open(probe_path):
        line = line.strip()
        if not line:
            continue
        j = json.loads(line)
        if "arms" not in j or not isinstance(j["arms"], dict) or "error" in j["arms"]:
            skipped += 1
            continue
        key = (int(j["label"][1:]), int(j["plane"]))
        if key not in truth:
            skipped += 1
            continue
        ds2 = (j["tgtFwhmFit"] ** 2 - j["refFwhmFit"] ** 2) / FWHM_TO_SIGMA2
        members.append(dict(key=key, phot=truth[key], ds2=ds2, arms=j["arms"]))
    return members, skipped


def arm_arrays(members, arm):
    """-> s, phot, ds2, sigma, terciles-trend, aperture arrays over the members that carry the arm."""
    rows = [m for m in members if isinstance(m["arms"].get(arm), dict) and "s" in m["arms"][arm]]
    f = lambda k: np.array([np.nan if m["arms"][arm].get(k) is None else m["arms"][arm][k] for m in rows], float)
    s, sigma, ap = f("s"), f("sigma"), f("aperturePx")
    t = np.array([[np.nan if v is None else v for v in m["arms"][arm]["terciles"]] for m in rows], float).reshape(-1, 3)
    trend = (t[:, 0] - t[:, 2]) / s * 100
    phot = np.array([m["phot"] for m in rows])
    ds2 = np.array([m["ds2"] for m in rows])
    return s, phot, ds2, sigma, trend, ap


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
        members, skipped = load_dataset(probe, truth)
        d = datasets.setdefault(name, dict(members=[], skipped=0))
        d["members"] += members
        d["skipped"] += skipped
    arm_names = sorted({a for d in datasets.values() for m in d["members"] for a in m["arms"]})
    variants = [a for a in arm_names if a.startswith("f1_")]
    if not variants:
        die("no f1_* arm in the probe output")

    print("datasets:")
    for name, d in datasets.items():
        print(f"   {name:24s} {len(d['members'])} member-planes with truth, {d['skipped']} skipped")

    stats = {}  # (dataset, arm) -> dict
    for name, d in datasets.items():
        for arm in arm_names:
            s, phot, ds2, sigma, trend, ap = arm_arrays(d["members"], arm)
            if len(s) < 3:
                continue
            st = gap_stats(s, phot, ds2)
            st["sigma"] = float(np.nanmedian(sigma))
            st["trend"] = float(np.nanmedian(trend))
            st["aperture"] = float(np.nanmedian(ap)) if np.isfinite(ap).any() else float("nan")
            st["sigma_all"] = sigma
            stats[(name, arm)] = st

    print("\nper arm and dataset (gap = s_arm / s_truth - 1, %; slope in %/px^2 against dS2; "
          "sigma = median per-member RCR dispersion; tercile = median (t1 - t3)/s in %, ! above "
          f"{TERCILE_FLAG:g}):")
    print(f"   {'arm':8s} {'dataset':22s} {'n':>4s} {'median':>7s} {'med|gap|':>8s} {'slope':>7s} {'sigma':>7s} {'tercile':>8s}")
    for arm in arm_names:
        for name in datasets:
            st = stats.get((name, arm))
            if not st:
                continue
            flag = "!" if abs(st["trend"]) > TERCILE_FLAG else " "
            print(f"   {arm:8s} {name:22s} {st['n']:4d} {st['median']:7.2f} {st['med_abs']:8.2f} {st['slope']:7.2f} "
                  f"{st['sigma']:7.4f} {st['trend']:7.2f}{flag}")

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
            if abs(st["slope"]) > SLOPE_BAR:
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
        def key(a):
            sig = np.concatenate([stats[(n, a)]["sigma_all"] for n in datasets if (n, a) in stats])
            aps = [stats[(n, a)]["aperture"] for n in datasets if (n, a) in stats]
            ap = float(np.nanmedian(aps)) if np.isfinite(aps).any() else float("inf")
            return (float(np.nanmedian(sig)), ap, a)
        print("step 3 tie-break (median sigma pooled over datasets, then median aperturePx, then name):")
        for a in sorted(tied, key=key):
            k = key(a)
            print(f"   {a:8s} sigma {k[0]:.5f}  aperture {k[1]:.3f} px")
        pick = min(tied, key=key)
    print(f"aperture choice: {pick}")


if __name__ == "__main__":
    main()
