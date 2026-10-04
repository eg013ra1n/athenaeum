#!/usr/bin/env python3
"""Emulate Athenaeum's LN background cell statistic at a bright star.

    halo_cells.py <single_calibrated_frame> <deep_master_noLN> <xs> <ys> <xm> <ym>

`(xs, ys)` is the star's approximate position in the single frame, `(xm, ym)`
in the master's geometry. The node is placed where the LN mesh at scale 1024
puts it relative to the star in the master geometry (node = floor to the
128-px grid). The single frame plays the target (deviation 3.2); the master
plus Gaussian noise plays an n-frame LN reference (deviation 3.0). Mirrors
`ln/background.rs::robust_cell_level`: clip [4.5e-5, 0.85], 2x2 subsampling
above 4096 px, up to 5 rounds of median +- k*1.4826*MAD, rejection limit 0.3.
"""
import os
import sys

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "acceptance"))
from imgcmp import read  # noqa: E402

MAD2S = 1.4826


def robust_cell_level(samples, dev_sigma, rejection_limit=0.3, rounds=5):
    kept = samples[np.isfinite(samples)].copy()
    total = len(kept)
    for _ in range(rounds):
        if len(kept) == 0:
            break
        med = np.median(kept)
        mad = np.median(np.abs(kept - med))
        if mad <= 0:
            break
        bound = dev_sigma * MAD2S * mad
        before = len(kept)
        kept = kept[np.abs(kept - med) <= bound]
        if len(kept) == before:
            break
    frac = (total - len(kept)) / total if total else 1.0
    if len(kept) == 0 or frac > rejection_limit:
        return np.nan, False, frac
    return np.median(kept), True, frac


def cell_samples(img, cx, cy, stride=128, high=0.85, low=4.5e-5):
    half = stride // 2
    w = img[cy - half:cy + half, cx - half:cx + half].astype(np.float32).copy()
    w[(w < low) | (w > high)] = np.nan
    step = 2 if w.size > 4096 else 1
    return w[::step, ::step].ravel()


def grid3(img, nx, ny, dev, bg, stride=128):
    out = {}
    for dj in (-1, 0, 1):
        for di in (-1, 0, 1):
            lvl, ok, frac = robust_cell_level(cell_samples(img, nx + di * stride, ny + dj * stride, stride), dev)
            out[(di, dj)] = (lvl / bg - 1 if ok else np.nan, ok, frac)
    return out


def fill(out):
    res = {}
    for k, (lvl, ok, _) in out.items():
        if ok:
            res[k] = lvl
            continue
        nb = [out[(k[0] + a, k[1] + b)][0] for a in (-1, 0, 1) for b in (-1, 0, 1)
              if (a, b) != (0, 0) and (k[0] + a, k[1] + b) in out and out[(k[0] + a, k[1] + b)][1]]
        res[k] = np.mean(nb) if nb else np.nan
    return res


def show(label, out):
    print(label)
    for dj in (-1, 0, 1):
        print("   " + "  ".join(f"{out[(di, dj)][0] * 100:7.2f}%{'' if out[(di, dj)][1] else '*'} rej={out[(di, dj)][2] * 100:4.1f}%" for di in (-1, 0, 1)))


def sky(img, x, y):
    reg = img[y - 600:y - 400, x - 600:x - 400]
    bg = np.median(reg)
    return bg, np.median(np.abs(reg - bg)) * MAD2S


def main():
    p_single, p_master = sys.argv[1], sys.argv[2]
    xs, ys, xm, ym = (int(v) for v in sys.argv[3:7])
    t = read(p_single)
    if t.ndim == 3:
        t = t[0]
    w = t[ys - 60:ys + 61, xs - 60:xs + 61]
    yy, xx = np.nonzero(w > 0.9)
    sx, sy = xs - 60 + xx.mean(), ys - 60 + yy.mean()  # saturated-core centroid
    bgt, sigt = sky(t, int(sx), int(sy))
    offx, offy = xm - (xm // 128) * 128, ym - (ym // 128) * 128  # star offset from its node in the master geometry
    nx, ny = int(round(sx - offx)), int(round(sy - offy))
    print(f"single frame: star ({sx:.1f},{sy:.1f}), node ({nx},{ny}), bg {bgt:.3e}, sigma/bg {sigt / bgt:.3f}")
    tgt = grid3(t, nx, ny, 3.2, bgt)
    show("TARGET single frame, dev 3.2 (level/bg-1, * = invalid cell)", tgt)
    tgt_f = fill(tgt)

    r = read(p_master)
    if r.ndim == 3:
        r = r[0]
    bgr, sigr = sky(r, xm, ym)
    mnx, mny = (xm // 128) * 128, (ym // 128) * 128
    print(f"master proxy: bg {bgr:.3e}, sigma/bg {sigr / bgr:.3f}, node ({mnx},{mny})")
    rng = np.random.default_rng(1)
    print("A) star cell vs noise level (master + noise; 4 seeds each; dev 3.2 so only noise changes):")
    for n in (1, 2, 3, 5, 10, 28):
        rows = []
        for _ in range(4):
            add = np.sqrt(max(((sigt / bgt) / np.sqrt(n) * bgr) ** 2 - sigr ** 2, 0))
            win = r[ym - 200:ym + 201, xm - 200:xm + 201] + rng.normal(0, add, (401, 401)).astype(np.float32)
            lvl, ok, frac = robust_cell_level(cell_samples(win, 200 - (xm - mnx), 200 - (ym - mny)), 3.2)
            rows.append((frac, ok, lvl / bgr - 1 if ok else np.nan))
        print(f"  n={n:3d} sigma/bg={(sigt / bgt) / np.sqrt(n):.3f}: rej " + ", ".join(f"{f * 100:4.1f}%{'' if ok else '*'}" for f, ok, _ in rows)
              + "   level/bg-1: " + ", ".join(f"{l * 100:5.1f}%" for _, _, l in rows))
    print("B) reference proxy at n = 5, 10, 20 with dev 3.0, and B/bg = ref - tgt (s = 1) after the invalid fill:")
    for n in (5, 10, 20):
        add = np.sqrt(max(((sigt / bgt) / np.sqrt(n) * bgr) ** 2 - sigr ** 2, 0))
        win = r[ym - 900:ym + 901, xm - 900:xm + 901] + rng.normal(0, add, (1801, 1801)).astype(np.float32)
        ref = grid3(win, 900 - (xm - mnx), 900 - (ym - mny), 3.0, bgr)
        show(f"  REFERENCE proxy n={n}, dev 3.0", ref)
        ref_f = fill(ref)
        for dj in (-1, 0, 1):
            print("     B/bg: " + "  ".join(f"{(ref_f[(di, dj)] - tgt_f[(di, dj)]) * 100:7.2f}%" for di in (-1, 0, 1)))
    print("C) single frame, cell size 128/192/256 at dev 3.0/3.2/3.5:")
    for dev in (3.0, 3.2, 3.5):
        for stride in (128, 192, 256):
            lvl, ok, frac = robust_cell_level(cell_samples(t, nx, ny, stride), dev)
            print(f"  dev {dev} stride {stride}: rej {frac * 100:4.1f}% {'valid' if ok else 'INVALID'} level/bg-1 {((lvl / bgt - 1) * 100) if ok else float('nan'):5.1f}%")


if __name__ == "__main__":
    main()
