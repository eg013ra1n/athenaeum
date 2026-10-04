#!/usr/bin/env python3
"""Halo rings and LN deficit around one bright star, three masters in one geometry.

    halo_profile.py <master_LN> <master_noLN> <master_external> <x> <y>

Prints ring medians of (pixel/background - 1) for each master, the deficit
`D = LN - k*noLN` (k from the 300-420 px annulus) in units of the LN master's
background, its fit on the 128-px bicubic B-spline node basis (the LN mesh at
scale 1024) and the area integral of the ring deficit expressed as one node.
numpy only; the XISF/FITS reader is the acceptance harness's imgcmp.py.
"""
import os
import sys

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "acceptance"))
from imgcmp import read  # noqa: E402

STRIDE = 128
R = 420


def bspl(t):
    t = np.abs(t)
    out = np.zeros_like(t)
    m1 = t < 1
    out[m1] = 2 / 3 - t[m1] ** 2 + t[m1] ** 3 / 2
    m2 = (t >= 1) & (t < 2)
    out[m2] = (2 - t[m2]) ** 3 / 6
    return out


def box(img, n=15):
    c = np.cumsum(np.cumsum(np.pad(img, ((1, 0), (1, 0))), 0), 1)
    return (c[n:, n:] - c[:-n, n:] - c[n:, :-n] + c[:-n, :-n]) / (n * n)


def window(path, x0, y0):
    a = read(path)
    if a.ndim == 3:
        a = a[0]
    return a[y0 - R:y0 + R + 1, x0 - R:x0 + R + 1].astype(np.float64)


def main():
    p_ln, p_noln, p_ext = sys.argv[1:4]
    x0, y0 = int(sys.argv[4]), int(sys.argv[5])
    imgs = {"LN": window(p_ln, x0, y0), "noLN": window(p_noln, x0, y0), "external": window(p_ext, x0, y0)}
    yy, xx = np.mgrid[-R:R + 1, -R:R + 1]
    rr = np.hypot(xx, yy)
    far = (rr >= 360) & (rr < 420)
    bg = {k: np.median(v[far]) for k, v in imgs.items()}
    edges = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 120, 140, 160, 180, 200, 220, 250, 280, 320, 360, 400]
    print("ring      " + "  ".join(f"{k:>9}" for k in imgs))
    for a, b in zip(edges[:-1], edges[1:]):
        m = (rr >= a) & (rr < b)
        print(f"{a:3d}-{b:3d}  " + "  ".join(f"{(np.median(imgs[k][m]) / bg[k] - 1) * 100:8.2f}%" for k in imgs))

    ln, noln = imgs["LN"], imgs["noLN"]
    far2 = (rr >= 300) & (rr <= 420)
    k = np.median(ln[far2]) / np.median(noln[far2])
    D = (ln - k * noln) / np.median(ln[far2])
    Ds = box(D)
    rr2 = rr[7:-7, 7:-7]
    print(f"\nk (far-annulus ratio LN/noLN) = {k:.4f}")
    print("ring     D (15x15 smoothed, % of bg)")
    for lo, hi in [(45, 60), (60, 80), (80, 100), (100, 120), (120, 140), (140, 160), (160, 180), (180, 220), (220, 260)]:
        m = (rr2 >= lo) & (rr2 < hi)
        print(f"{lo:3d}-{hi:3d}  {np.median(Ds[m]) * 100:7.2f}%")

    # area integral of the ring deficit inside 220 px, expressed as one node (basis integral = stride^2)
    tot = 0.0
    for lo, hi in [(0, 45), (45, 60), (60, 80), (80, 100), (100, 120), (120, 140), (140, 160), (160, 180), (180, 220)]:
        m = (rr2 >= lo) & (rr2 < hi)
        med = np.median(Ds[m]) if lo >= 45 else np.median(Ds[(rr2 >= 45) & (rr2 < 60)])
        tot += med * np.pi * (hi ** 2 - lo ** 2)
    print(f"area integral r<220: {tot * 100:.0f} %*px^2  = one {STRIDE}-px node of {tot / STRIDE ** 2 * 100:.1f} % of bg")

    # 7x7 node-basis fit, ridge-regularized, 45 < r <= 330
    ax, ay = x0 + xx, y0 + yy
    ic, jc = int(round(x0 / STRIDE)), int(round(y0 / STRIDE))
    ii, jj = range(ic - 3, ic + 4), range(jc - 3, jc + 4)
    cols = [(bspl((ax - i * STRIDE) / STRIDE) * bspl((ay - j * STRIDE) / STRIDE)).ravel() for j in jj for i in ii]
    A = np.array(cols).T
    d = D.ravel()
    mask = ((rr > 45) & (rr <= 330)).ravel()
    w = mask.copy()
    lam = 1e-3 * mask.sum()
    coef = None
    for _ in range(4):
        Aw, dw = A[w], d[w]
        coef = np.linalg.solve(Aw.T @ Aw + lam * np.eye(A.shape[1]), Aw.T @ dw)
        res = d - A @ coef
        s = 1.4826 * np.median(np.abs(res[w] - np.median(res[w])))
        w = mask & (np.abs(res) < 3 * s)
    fit = (A @ coef).reshape(D.shape)
    fs, rs = box(fit), box(D - fit)
    print("\nnode-basis fit: ring  D  fit  residual(median)")
    for lo, hi in [(45, 60), (60, 80), (80, 100), (100, 120), (120, 140), (140, 160), (160, 180), (180, 220)]:
        m = (rr2 >= lo) & (rr2 < hi)
        print(f"{lo:3d}-{hi:3d}  {np.median(Ds[m]) * 100:7.2f}%  {np.median(fs[m]) * 100:7.2f}%  {np.median(rs[m]) * 100:6.2f}%")
    C = np.array(coef).reshape(len(jj), len(ii))
    print(f"node coefficients (% of bg); centre node ({ic},{jc}) = pixel ({ic * STRIDE},{jc * STRIDE}), star at ({x0 - ic * STRIDE:+d},{y0 - jc * STRIDE:+d})")
    print("      " + " ".join(f"{i:7d}" for i in ii))
    for r, j in enumerate(jj):
        print(f"{j:5d} " + " ".join(f"{C[r, c] * 100:7.2f}" for c in range(len(ii))))


if __name__ == "__main__":
    main()
