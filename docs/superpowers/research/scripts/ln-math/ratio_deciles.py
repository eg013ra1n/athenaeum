#!/usr/bin/env python3
"""Per-brightness-decile LN ratio bias and scatter from the ln-scale-audit per-star CSVs.

    ratio_deciles.py <audit scratch dir> <handle> [<handle> ...]

Reads `<dir>/runB-stars/h<handle>.p0.csv` and `<dir>/runB-truth.csv` (the
previous audit's dumps). `prod` = the production FWTM-ellipse ratio with the
fitted background; `c3` = the common 3F circle with annulus sky; peak SNR =
target amplitude / the registered frame's pixel noise. Prints decile medians
and MADs and the robust location for four selections: all pairs, the bright
quarter, peak SNR >= 40, and peak SNR >= 40 restricted to the 500 brightest
(the external pipeline's rule).
"""
import csv
import os
import sys

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "acceptance"))
from imgcmp import read  # noqa: E402


def loc(z):
    z = np.asarray(z, float)
    k = np.ones(len(z), bool)
    s = 0.0
    for _ in range(10):
        m = np.median(z[k])
        s = 1.4826 * np.median(np.abs(z[k] - m))
        nk = np.abs(z - m) <= 3 * s
        if nk.sum() == k.sum():
            break
        k = nk
    return np.median(z[k]), s, k.sum()


def main():
    base = sys.argv[1]
    truth = {r["handle"]: r for r in csv.DictReader(open(os.path.join(base, "runB-truth.csv")))}
    for h in sys.argv[2:]:
        a = np.genfromtxt(os.path.join(base, "runB-stars", f"h{h}.p0.csv"), delimiter=",", names=True)
        tr = truth[h]
        ok = (np.isfinite(a["r_sky"]) & np.isfinite(a["t_sky"]) & (a["r_amp"] + a["r_b"] < 0.5)
              & (a["t_amp"] + a["t_b"] < 0.5) & (a["r_sig"] > 0) & (a["t_sig"] > 0))
        a = a[ok]
        prod = a["r_sig"] / a["t_sig"]
        bright = a["r_c3_s"] - a["r_c3_n"] * a["r_sky"]
        c3 = bright / (a["t_c3_s"] - a["t_c3_n"] * a["t_sky"])
        img = read(tr["path"])
        patch = img[1000:1400, 1000:1400]
        sig = 1.4826 * np.median(np.abs(patch - np.median(patch)))
        snr = a["t_amp"] / sig
        o = np.argsort(-bright)
        n = len(o)
        phot = float(tr["phot"])
        print(f"\nmember h{h}: FWHM ref {tr['fwhm_ref']} mem {tr['fwhm_mem']}, truth {phot:.4f}, applied {float(tr['applied']):.4f}, stars {n}, sigma_px {sig:.2e}")
        print(" decile    n  medPeakSNR  prod(med)  prod MAD   c3(med)  c3 MAD")
        for q in range(10):
            s = o[q * n // 10:(q + 1) * n // 10]
            print(f"  {q:2d}  {len(s):5d}  {np.median(snr[s]):8.1f}   {np.median(prod[s]):.4f}   {1.4826 * np.median(np.abs(prod[s] - np.median(prod[s]))):.4f}   {np.median(c3[s]):.4f}  {1.4826 * np.median(np.abs(c3[s] - np.median(c3[s]))):.4f}")
        sels = [("all", np.ones(n, bool)), ("bright quarter", np.isin(np.arange(n), o[:n // 4])),
                ("peakSNR>=40", snr >= 40), ("peakSNR>=40 top500", np.isin(np.arange(n), o[:500]) & (snr >= 40))]
        for name, sel in sels:
            lp, sp, kp = loc(prod[sel])
            lc, sc, kc = loc(c3[sel])
            print(f"  {name:20s} n={sel.sum():5d}  prod {lp:.4f} (sigma {sp:.4f}, se {sp / np.sqrt(kp):.5f}) gap {100 * (lp / phot - 1):+.2f}%"
                  f"   c3 {lc:.4f} (sigma {sc:.4f}, se {sc / np.sqrt(kc):.5f}) gap {100 * (lc / phot - 1):+.2f}%")


if __name__ == "__main__":
    main()
