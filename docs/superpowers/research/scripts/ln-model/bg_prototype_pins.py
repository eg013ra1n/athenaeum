#!/usr/bin/env python3
"""The numbers behind plan 2026-10-05's rulings P-1, P-2, P-19 and P-20,
from the numpy oracle bg_prototype.py (spec 2026-10-04 §1.2).

    bg_prototype_pins.py [gradient|pins|strip|noise|all]

gradient  P-1: a linear gradient's node error vs λ, every node and one knot
          interval inside (2048 × 1536 at scale 512).
pins      P-2: the halo and broad-feature pins at three noise ratios, with
          and without the shared refit, on the nodes near the feature.
strip     P-19: a NaN strip (x ≥ 1400): the farthest strip node from the
          edge level, and the covered part's error, at λ = 1e-4 and 1e-2.
noise     P-20: analytic node noise (single frame, σ/sky 0.188) on the set-204
          geometry at scale 1024, full plane and with a 30 %-wide gap, vs λ.
Slow parts (noise) take a few minutes; numpy only.
"""
import sys

import numpy as np

import bg_prototype as b

SKY = 0.1


def nodes_xy(W, H, stride):
    gw = -(-(W - 1) // stride) + 1
    gh = -(-(H - 1) // stride) + 1
    return np.meshgrid(np.minimum(np.arange(gw) * stride, W - 1), np.minimum(np.arange(gh) * stride, H - 1))


def gradient():
    W, H, scale = 2048, 1536, 512
    yy, xx = np.mgrid[0:H, 0:W].astype(float)
    amp = 0.02
    f = lambda X, Y: SKY + amp * (0.6 * X / (W - 1) + 0.4 * Y / (H - 1))  # noqa: E731
    for lam in (1e-4, 3e-3, 1e-2, 0.0):
        b.LAM_REL = lam
        g = b.Geom(W, H, scale)
        c, _ = b.own(g, b.tile_table(f(xx, yy)))
        n = b.nodes(g, c, scale // 8)
        X, Y = nodes_xy(W, H, scale // 8)
        err = np.abs(n - f(X, Y)) / amp
        inner = (X >= scale) & (X <= W - 1 - scale) & (Y >= scale) & (Y <= H - 1 - scale)
        print(f"P-1 λ={lam:g}: every node {err.max():.2e}, one knot interval inside {err[inner].max():.2e} of the amplitude")


def pins():
    W, H, scale = 2048, 1536, 512
    yy, xx = np.mgrid[0:H, 0:W].astype(float)
    cx, cy = 0.55 * W, 0.45 * H
    r = np.hypot(xx - cx, yy - cy)
    fwhm = 1.3 * scale
    cases = {
        "halo": (SKY + 0.6 * (1 + (r / 15) ** 2) ** -1.5, 600.0),
        "broad feature": (SKY * (1 + 0.30 * np.exp(-0.5 * (r / (fwhm / 2.3548)) ** 2)), fwhm),
    }
    sT = 0.188 * SKY
    for lam in (1e-4, 1e-2):
        b.LAM_REL = lam
        g = b.Geom(W, H, scale)
        X, Y = nodes_xy(W, H, scale // 8)
        for name, (sky, radius) in cases.items():
            near = np.hypot(X - cx, Y - cy) <= radius
            for n in (1, 5, 28):
                own, sh = 0.0, 0.0
                for seed in range(3):
                    rng = np.random.default_rng(seed * 100 + n)
                    tgt = sky + rng.normal(0, sT, sky.shape)
                    ref = sky + rng.normal(0, sT / np.sqrt(n), sky.shape)
                    tr, tt = b.tile_table(ref), b.tile_table(tgt)
                    cr, ar = b.own(g, tr)
                    ct, at = b.own(g, tt)
                    own = max(own, np.abs(b.nodes(g, cr, scale // 8) - b.nodes(g, ct, scale // 8))[near].max() / SKY)
                    sr, st = b.shared(g, tr, tt, ar, at)
                    sh = max(sh, np.abs(b.nodes(g, sr, scale // 8) - b.nodes(g, st, scale // 8))[near].max() / SKY)
                print(f"P-2 λ={lam:g} {name} n={n}: without the refit {own * 100:.3f} %, with it {sh * 100:.3f} % of sky")


def strip():
    W, H, scale = 2048, 1536, 512
    yy, xx = np.mgrid[0:H, 0:W].astype(float)
    for lam in (1e-4, 1e-2):
        b.LAM_REL = lam
        g = b.Geom(W, H, scale)
        X, _Y = nodes_xy(W, H, scale // 8)
        for grad in (0.01, 0.0):
            for seed in (3, 4, 5):
                rng = np.random.default_rng(seed)
                sky = SKY + grad * xx / (W - 1)
                p = sky + rng.normal(0, 0.002, sky.shape)
                p[:, 1400:] = np.nan
                c, _ = b.own(g, b.tile_table(p))
                n = b.nodes(g, c, scale // 8)
                edge = SKY + grad * 1400 / (W - 1)
                dev = np.abs(n[X >= 1400] - edge).max() / SKY
                cov = np.abs(n - (SKY + grad * X / (W - 1)))[X <= 888].max() / SKY
                print(f"P-19 λ={lam:g} gradient {grad / SKY * 100:.0f} % seed {seed}: strip {dev * 100:.3f} %, covered {cov * 100:.4f} % of sky")


def noise():
    W, H, scale = 6248, 4176, 1024
    stride = scale // 8
    tw, th = -(-W // 32), -(-H // 32)
    xs = np.array([((u * 32) + min(u * 32 + 32, W) - 1) / 2 for v in range(th) for u in range(tw)])
    ys = np.array([((v * 32) + min(v * 32 + 32, H) - 1) / 2 for v in range(th) for u in range(tw)])
    sig_s = 1.2533 * 0.188 / 32
    X, Y = nodes_xy(W, H, stride)
    print(f"old cell model node noise: {0.188 * 1.2533 / 64 * 100:.3f} % of sky")
    for lam in (1e-4, 1e-3, 3e-3, 1e-2):
        g = b.Geom(W, H, scale)
        E = g.design(X.ravel().astype(float), Y.ravel().astype(float))
        P = g.penalty()

        def gain(mask):
            idx = np.where(mask)[0]
            B = g.design(xs[idx], ys[idx])
            A = B.T @ B
            d = np.diag(A)
            A = A + lam * np.median(d[d > 0]) * P
            M = E @ np.linalg.solve(A, B.T)
            return (np.sqrt((M ** 2).sum(1)) * sig_s).reshape(X.shape)

        full, gap = gain(np.ones(xs.size, bool)), gain(xs < 0.7 * W)
        inner = (X >= scale) & (X <= W - 1 - scale) & (Y >= scale) & (Y <= H - 1 - scale)
        near = (X >= 0.7 * W) & (X <= 0.7 * W + 2 * stride)
        print(
            f"P-20 λ={lam:g}: interior {full[inner].max() * 100:.3f} %, plane edge {full.max() * 100:.3f} %, "
            f"gap ≤ 2 strides {gap[near].max() * 100:.3f} %, gap anywhere {gap[X >= 0.7 * W].max() * 100:.3f} % of sky"
        )


if __name__ == "__main__":
    what = sys.argv[1] if len(sys.argv) > 1 else "all"
    for name, fn in [("gradient", gradient), ("pins", pins), ("strip", strip), ("noise", noise)]:
        if what in (name, "all"):
            fn()
