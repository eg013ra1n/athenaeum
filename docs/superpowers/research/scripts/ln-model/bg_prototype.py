"""numpy oracle of the LN background model at the scale (spec 2026-10-04
§1.2: 32-px tiles, a penalized tensor cubic B-spline with knot spacing =
scale, two rejection rounds, the 8-sample floor, the shared refit). The plan
2026-10-05 sized the §1.6 pins with it; bg_prototype_pins.py prints those
numbers. The hot-pixel pass is left out (no fixture pixel exceeds 0.85).
LAM_REL is the membrane penalty relative to the median diagonal of BᵀB
(plan ruling P-20: 1e-2)."""
import numpy as np

TILE = 32
MIN_PRESENT = 256
KT = 3.0
KS = 3.0
FLOOR = 8
LAM_REL = 1e-2
MADS = 1.4826


def tile_table(plane):
    h, w = plane.shape
    tw, th = -(-w // TILE), -(-h // TILE)
    xs, ys, vals, sig = [], [], [], []
    for v in range(th):
        for u in range(tw):
            x0, y0 = u * TILE, v * TILE
            x1, y1 = min(x0 + TILE, w), min(y0 + TILE, h)
            px = plane[y0:y1, x0:x1].ravel()
            px = px[np.isfinite(px)]
            px = px[(px >= 4.5e-5) & (px <= 0.85)]
            if px.size < MIN_PRESENT:
                xs.append((x0 + x1 - 1) / 2); ys.append((y0 + y1 - 1) / 2)
                vals.append(np.nan); sig.append(np.nan)
                continue
            kept = px
            for _ in range(5):
                med = np.median(kept)
                mad = np.median(np.abs(kept - med))
                if mad <= 0:
                    break
                nk = kept[np.abs(kept - med) <= KT * MADS * mad]
                if nk.size == kept.size:
                    break
                kept = nk
            val = np.median(kept)
            xs.append((x0 + x1 - 1) / 2); ys.append((y0 + y1 - 1) / 2)
            vals.append(val); sig.append(MADS * np.median(np.abs(kept - val)))
    return np.array(xs), np.array(ys), np.array(vals), np.array(sig)


def basis(x, hk, n):
    u = x / hk
    m = np.clip(np.floor(u).astype(int), 0, n - 4)
    t = u - m
    w = np.stack([(1 - t) ** 3 / 6, (3 * t ** 3 - 6 * t ** 2 + 4) / 6,
                  (-3 * t ** 3 + 3 * t ** 2 + 3 * t + 1) / 6, t ** 3 / 6], axis=1)
    return m, w


class Geom:
    def __init__(self, W, H, scale):
        self.W, self.H, self.h = W, H, float(scale)
        self.nx = int(np.ceil((W - 1) / scale)) + 3
        self.ny = int(np.ceil((H - 1) / scale)) + 3

    def design(self, x, y):
        mx, wx = basis(x, self.h, self.nx)
        my, wy = basis(y, self.h, self.ny)
        B = np.zeros((x.size, self.nx * self.ny))
        for b in range(4):
            for a in range(4):
                B[np.arange(x.size), (my + b) * self.nx + (mx + a)] += wy[:, b] * wx[:, a]
        return B

    def penalty(self):
        n = self.nx * self.ny
        P = np.zeros((n, n))
        for j in range(self.ny):
            for i in range(self.nx):
                a = j * self.nx + i
                for b in ([a + 1] if i + 1 < self.nx else []) + ([a + self.nx] if j + 1 < self.ny else []):
                    P[a, a] += 1; P[b, b] += 1; P[a, b] -= 1; P[b, a] -= 1
        return P

    def cell(self, x, y):
        mx, _ = basis(x, self.h, self.nx)
        my, _ = basis(y, self.h, self.ny)
        return my * (self.nx - 3) + mx


def fit(g, x, y, rhs_list, idx):
    B = g.design(x[idx], y[idx])
    A = B.T @ B
    d = np.diag(A)
    lam = LAM_REL * np.median(d[d > 0])
    A = A + lam * g.penalty()
    L = np.linalg.cholesky(A)
    out = []
    for r in rhs_list:
        z = np.linalg.solve(L, B.T @ r[idx])
        out.append(np.linalg.solve(L.T, z))
    return out


def evalc(g, c, x, y):
    return g.design(np.atleast_1d(x).astype(float), np.atleast_1d(y).astype(float)) @ c


def own(g, tt):
    x, y, v, _sigma = tt
    allid = np.where(np.isfinite(v))[0]
    acc = allid
    (c,) = fit(g, x, y, [v], acc)
    for _ in range(2):
        e = v[allid] - evalc(g, c, x[allid], y[allid])
        ea = v[acc] - evalc(g, c, x[acc], y[acc])
        se = MADS * np.median(np.abs(ea - np.median(ea)))
        # plan ruling P-21: a residual MAD at the f32 rounding level of the
        # tile values is no noise at all — treated as σ_e ≤ 0.
        floor = 1e-6 * abs(np.median(v[acc]))
        acc = allid[np.abs(e) <= KS * se] if se > floor else allid
        (c,) = fit(g, x, y, [v], acc)
    cells = np.bincount(g.cell(x[acc], y[acc]), minlength=(g.nx - 3) * (g.ny - 3))
    assert (cells >= FLOOR).any()
    return c, set(acc.tolist())


def shared(g, tr, tt, acc_r, acc_t):
    ids = np.array(sorted(acc_r & acc_t))
    cr, ct = fit(g, tr[0], tr[1], [tr[2], tt[2]], ids)
    return cr, ct


def nodes(g, c, stride):
    gw = -(-(g.W - 1) // stride) + 1
    gh = -(-(g.H - 1) // stride) + 1
    xs = np.minimum(np.arange(gw) * stride, g.W - 1)
    ys = np.minimum(np.arange(gh) * stride, g.H - 1)
    X, Y = np.meshgrid(xs, ys)
    return evalc(g, c, X.ravel(), Y.ravel()).reshape(gh, gw)
