#!/usr/bin/env python3
"""Pair the external pipeline's per-frame LN scales with Athenaeum's `lnScale`.

    scale_pairs.py <external log> <first line> <last line> <run-N.json> <filter letter>

The external log block is the LN pass whose reference is the integrated
reference image; each target prints `scale = S, sigma = s, N valid PSF fits,
M PSF signal estimates`. Frames are joined by file stem. Prints the pairs
sorted by Athenaeum's FWHM and the regression of ln(ath/ext) on the PSF
variance sigma^2 = (FWHM/2.3548)^2.
"""
import json
import re
import sys

import numpy as np


def main():
    log, a, b, run_json, filt = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4], sys.argv[5]
    ext, cur = {}, None
    with open(log, encoding="utf-8", errors="replace") as f:
        for i, line in enumerate(f, 1):
            if i < a or i > b:
                continue
            m = re.search(r"/([^/]+)_c_r\.xisf", line)
            if m and "Loading target file" in line:
                cur = m.group(1)
                continue
            m = re.search(r"scale = ([0-9.]+), sigma = ([0-9.e+-]+), (\d+) valid PSF fits, (\d+) PSF signal", line)
            if m and cur:
                ext[cur] = (float(m.group(1)), float(m.group(2)), int(m.group(3)), int(m.group(4)))
    d = json.load(open(run_json))
    g = [g for g in d["groups"] if f"__{filt}__" in g["key"]][0]
    rows = []
    print("stem                              fwhm  ext_scale sigma  nfit nsig  ath_lnScale  ath/ext")
    for fr in sorted(g["frames"], key=lambda f: f["fwhmPx"]):
        stem = fr["filename"].rsplit(".", 1)[0]
        e = ext.get(stem)
        if e and fr.get("lnScale"):
            rows.append((fr["fwhmPx"], e[0], fr["lnScale"], fr["weight"]))
            print(f"{stem[:32]:32s} {fr['fwhmPx']:5.2f} {e[0]:.4f} {e[1]:.4f} {e[2]:4d} {e[3]:4d}  {fr['lnScale']:.4f}  {fr['lnScale'] / e[0]:.4f}")
    r = np.array(rows)
    x = (r[:, 0] / 2.3548) ** 2
    y = np.log(r[:, 2] / r[:, 1]) * 100
    p = np.polyfit(x, y, 1)
    print(f"ln(ath/ext) % vs sigma^2: slope {p[0]:.3f} %/px^2, r = {np.corrcoef(x, y)[0, 1]:.3f}, n = {len(x)}")
    p2 = np.polyfit(r[:, 0], y, 1)
    print(f"vs FWHM: slope {p2[0]:.3f} %/px; mean {y.mean():.2f} %, weighted mean {np.average(y, weights=r[:, 3]):.2f} %, range {np.exp(y.min() / 100):.4f}..{np.exp(y.max() / 100):.4f}")


if __name__ == "__main__":
    main()
