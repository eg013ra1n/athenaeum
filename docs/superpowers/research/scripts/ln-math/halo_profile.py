#!/usr/bin/env python3
"""Halo rings and LN deficit around one bright star, three masters in one geometry.

    halo_profile.py <master_LN> <master_noLN> <master_external> <x> <y>
                    [--search R] [--k K_LN,K_NOLN,K_EXT] [--bg0 BG] [--labels A,B,C]

--search R   refine (x, y) in each master by the centroid of the 7 x 7 box around the brightest
             pixel of a 5 x 5 box-smoothed copy within R px (default 0: no refinement).
--k          per-master flux scale k (default 1,1,1); --bg0 the background that normalizes the
             scale-normalized excess (default: the no-LN master's own background annulus median).
--labels     names of the three slots in the output (default LN,noLN,external).
After today's tables the script prints every ring excess both raw and scale-normalized,
`(median(X/k) - background(X/k)) / BG` (L-21).

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


def plane(path):
    a = read(path)
    if a.ndim == 3:
        a = a[0]
    return a


def refine(a, x0, y0, search):
    """Centroid of the 7 x 7 box around the brightest pixel of a 5 x 5 box-smoothed copy within
    `search` px of (x0, y0). Returns the refined integer (x, y)."""
    if search <= 0:
        return x0, y0
    ys, xs = max(y0 - search, 0), max(x0 - search, 0)
    sub = a[ys:y0 + search + 1, xs:x0 + search + 1].astype(np.float64)
    sub = np.nan_to_num(sub, nan=0.0)
    sm = box(sub, 5)  # valid-mode: sm[i, j] is centred on sub[i + 2, j + 2]
    iy, ix = np.unravel_index(np.argmax(sm), sm.shape)
    cy, cx = iy + 2, ix + 2
    y1, x1 = max(cy - 3, 0), max(cx - 3, 0)
    patch = sub[y1:cy + 4, x1:cx + 4]
    patch = patch - patch.min()
    gy, gx = np.mgrid[y1:y1 + patch.shape[0], x1:x1 + patch.shape[1]]
    tot = patch.sum()
    if tot <= 0:
        return xs + cx, ys + cy
    return int(round(xs + (patch * gx).sum() / tot)), int(round(ys + (patch * gy).sum() / tot))


def window(a, x0, y0):
    return a[y0 - R:y0 + R + 1, x0 - R:x0 + R + 1].astype(np.float64)


def opt(args, name, default):
    if name in args:
        i = args.index(name)
        val = args[i + 1]
        del args[i:i + 2]
        return val
    return default


def main():
    args = sys.argv[1:]
    search = int(opt(args, "--search", "0"))
    ks = [float(v) for v in opt(args, "--k", "1,1,1").split(",")]
    bg0_arg = opt(args, "--bg0", None)
    labels = opt(args, "--labels", "LN,noLN,external").split(",")
    if len(args) != 5 or len(ks) != 3 or len(labels) != 3:
        sys.exit(__doc__)
    p_ln, p_noln, p_ext = args[0:3]
    x0, y0 = int(args[3]), int(args[4])
    full = {"LN": plane(p_ln), "noLN": plane(p_noln), "external": plane(p_ext)}
    pos = {}
    for key, a in full.items():
        pos[key] = refine(a, x0, y0, search)
        if search > 0:
            print(f"refined position {key}: ({pos[key][0]}, {pos[key][1]}) from ({x0}, {y0}), search {search} px")
    imgs = {key: window(a, *pos[key]) for key, a in full.items()}
    x0, y0 = pos["LN"]
    name = dict(zip(imgs, labels))
    yy, xx = np.mgrid[-R:R + 1, -R:R + 1]
    rr = np.hypot(xx, yy)
    far = (rr >= 360) & (rr < 420)
    bg = {k: np.median(v[far]) for k, v in imgs.items()}
    edges = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 120, 140, 160, 180, 200, 220, 250, 280, 320, 360, 400]
    print("ring      " + "  ".join(f"{name[k]:>9}" for k in imgs))
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

    # Scale-normalized read (L-21): (median(X/k) - background(X/k)) / BG, one fixed BG.
    bg0 = float(bg0_arg) if bg0_arg is not None else bg["noLN"]
    kk = dict(zip(imgs, ks))
    print(f"\nscale-normalized ring excess, (median(X/k) - bg(X/k)) / BG, BG = {bg0:.6g}; k = "
          + ", ".join(f"{name[key]} {kk[key]:g}" for key in imgs))
    print("ring      " + "  ".join(f"{name[k] + ' raw':>13} {name[k] + ' norm':>13}" for k in imgs))
    for a, b in zip(edges[:-1], edges[1:]):
        m = (rr >= a) & (rr < b)
        cells = []
        for key, v in imgs.items():
            raw = np.median(v[m]) / bg[key] - 1
            norm = (np.median(v[m] / kk[key]) - bg[key] / kk[key]) / bg0
            cells.append(f"{raw * 100:12.2f}% {norm * 100:12.2f}%")
        print(f"{a:3d}-{b:3d}  " + "  ".join(cells))


if __name__ == "__main__":
    main()
