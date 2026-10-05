#!/usr/bin/env python3
"""Nebula-box excess of masters against a no-LN control, raw and scale-normalized (spec §11.3).

    nebula_boxes.py <boxes.json> <control.fits> <master>... [--k K ...] [--control-k K] [--size 100]

boxes.json is {"nebula": [[x, y], ...], "background": [[x, y], ...]} (box top-left corners,
`--size` px square). It is written and committed BEFORE the LN master is read.

For each master X (flux scale k, default 1; pass one --k per master, in order):
  raw        (median(box of X) - bg(X)) / bg(X)
  normalized (median(box of X/k) - bg(X/k)) / bg0,   bg0 = bg(control), one fixed denominator
bg(X) is the median over the background boxes' pixels. Each line also prints the difference
from the control's excess, in points of background: d_raw = raw - control raw, and
d_norm = norm - control norm, where the control's normalized read uses the control's OWN flux
scale (`--control-k`, default 1: a control read at its LN-reference scale k is normalized by that k,
exactly as a master is; the checkpoint-A tables are norm - norm). numpy only; reader is imgcmp.read.
"""
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "acceptance"))
from imgcmp import read  # noqa: E402


def plane(path):
    a = read(path)
    return a[0] if a.ndim == 3 else a


def boxes_of(a, corners, size):
    return [a[y:y + size, x:x + size] for x, y in corners]


def background(a, corners, size):
    vals = np.concatenate([b.ravel() for b in boxes_of(a, corners, size)])
    return float(np.median(vals[np.isfinite(vals)]))


def reads(a, k, bg_box, nebula, size, bg0):
    """-> [(raw, normalized)] per nebula box."""
    bg = background(a, bg_box, size)
    out = []
    for b in boxes_of(a, nebula, size):
        v = b[np.isfinite(b)]
        med = float(np.median(v))
        raw = (med - bg) / bg
        norm = (med / k - bg / k) / bg0
        out.append((raw, norm))
    return out


def main():
    args = sys.argv[1:]
    size = 100
    ks = []
    control_k = 1.0
    while "--control-k" in args:
        i = args.index("--control-k")
        control_k = float(args[i + 1])
        del args[i:i + 2]
    while "--size" in args:
        i = args.index("--size")
        size = int(args[i + 1])
        del args[i:i + 2]
    while "--k" in args:
        i = args.index("--k")
        ks.append(float(args[i + 1]))
        del args[i:i + 2]
    if len(args) < 3:
        sys.exit(__doc__)
    boxes = json.load(open(args[0]))
    nebula, bg_box = boxes["nebula"], boxes["background"]
    control = args[1]
    masters = args[2:]
    if ks and len(ks) != len(masters):
        sys.exit(f"--k given {len(ks)} times for {len(masters)} masters")
    if ks and "--control-k" not in sys.argv:
        sys.exit("--k given without --control-k: the control would be read at k = 1 (its raw read) "
                 "against normalized masters; pass the control's own --control-k")
    ks = ks or [1.0] * len(masters)
    ctl = plane(control)
    bg0 = background(ctl, bg_box, size)
    ctl_reads = reads(ctl, control_k, bg_box, nebula, size, bg0)
    print(f"control {os.path.basename(control)}: bg0 = {bg0:.6g}, k = {control_k:g}, {len(nebula)} nebula boxes, "
          f"{len(bg_box)} background boxes, size {size}")
    print("control reads  raw%: " + " / ".join(f"{r * 100:.3f}" for r, _ in ctl_reads)
          + "   norm%: " + " / ".join(f"{n * 100:.3f}" for _, n in ctl_reads))
    print("master                          box  (x, y)         k        raw%      norm%   d_raw(pts)  d_norm(pts)")
    for path, k in zip(masters, ks):
        a = plane(path)
        for i, ((raw, norm), (cr, cn)) in enumerate(zip(reads(a, k, bg_box, nebula, size, bg0), ctl_reads)):
            x, y = nebula[i]
            print(f"{os.path.basename(path)[:30]:30s} {i:4d}  ({x:5d},{y:5d}) {k:8.4f} {raw * 100:9.3f} "
                  f"{norm * 100:9.3f} {(raw - cr) * 100:11.3f} {(norm - cn) * 100:12.3f}")


if __name__ == "__main__":
    main()
