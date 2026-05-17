#!/usr/bin/env python3
"""Label plate-solve gate-audit rows good/suspect by cross-frame clustering
and print per-stage separation of rms_px / inlier_ratio / inliers.

No external truth needed: frames of one object cluster tightly on the sky,
so an *accepted* solve far from the cohort median (or far from the FITS
header pointing) is a likely false positive. Use the printed distributions
to choose the Phase-2 blind-gate thresholds.

Usage:  python3 scripts/analyze_gate_csv.py /tmp/gate_calib.csv
"""
import csv
import math
import statistics as st
import sys


def angsep(r1, d1, r2, d2):
    r1, d1, r2, d2 = map(math.radians, (r1, d1, r2, d2))
    c = math.sin(d1) * math.sin(d2) + math.cos(d1) * math.cos(d2) * math.cos(r1 - r2)
    return math.degrees(math.acos(max(-1.0, min(1.0, c))))


def dist(vals):
    vals = sorted(vals)
    if not vals:
        return "n=0"
    q = lambda p: vals[min(len(vals) - 1, int(p * len(vals)))]
    return (
        f"n={len(vals)} med={st.median(vals):.3f} "
        f"p90={q(0.90):.3f} p95={q(0.95):.3f} max={vals[-1]:.3f}"
    )


def main(path):
    rows = [r for r in csv.DictReader(open(path)) if r["accepted"] == "true"]
    if not rows:
        print("No accepted rows in", path)
        return
    # Cohort = median of all accepted solved positions (robust to outliers).
    mra = st.median(float(r["solved_ra"]) for r in rows)
    mdec = st.median(float(r["solved_dec"]) for r in rows)
    for r in rows:
        d = angsep(float(r["solved_ra"]), float(r["solved_dec"]), mra, mdec)
        dh = r["dist_from_header_deg"]
        r["_label"] = (
            "suspect"
            if d > 5.0 or (dh not in ("", None) and float(dh) > 5.0)
            else "good"
        )

    for stage in ("hinted", "scale_cleared", "full_blind"):
        for lab in ("good", "suspect"):
            s = [r for r in rows if r["stage"] == stage and r["_label"] == lab]
            print(f"[{stage}/{lab}] rms_px      {dist([float(x['rms_px']) for x in s])}")
            print(f"[{stage}/{lab}] inlier_ratio {dist([float(x['inlier_ratio']) for x in s])}")
            print(f"[{stage}/{lab}] inliers     {dist([float(x['inliers']) for x in s])}")
            print(f"[{stage}/{lab}] recov/hdr   "
                  + dist([
                      float(x["recovered_scale_arcsec"]) / float(x["header_scale_arcsec"])
                      for x in s
                      if x["header_scale_arcsec"] not in ("", None)
                      and float(x["header_scale_arcsec"]) > 0
                  ]))
    n_good = sum(1 for r in rows if r["_label"] == "good")
    n_susp = len(rows) - n_good
    print(f"\naccepted rows: {len(rows)}  good={n_good}  suspect={n_susp}")
    print(
        "Threshold rule of thumb: pick blind_rms_max_px between p95(good) and "
        "p10(suspect); blind_min_inlier_ratio symmetrically (dense fields); "
        "blind_inlier_floor >= max(current floor, p10 of good 'inliers' on "
        "full_blind). Express the RMS ceiling as a multiple of the per-frame "
        "adaptive_tol_px so it is FOV-robust."
    )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: analyze_gate_csv.py <gate_csv_path>")
    main(sys.argv[1])
