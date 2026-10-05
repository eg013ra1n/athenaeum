#!/usr/bin/env python3
"""Whole-frame structure residual of masters against a no-LN control, by block class (checkpoint A review).

    star_blocks.py --control <master.fits> --ref <LN reference.fits> [--block 96] [--plane 0] \
        <label>=<master> [<label>=<master> ...]

Definition (reproduced from the review's one-off; every read is `acceptance/imgcmp.py`'s `read`):
  - every image is cut into non-overlapping
    block x block tiles (the remainder at the right/bottom edge is dropped) and each tile reduced
    to its `nanmedian`;
  - structure residual of an arm, per block, in %:
        100 * (arm_block / median(arm) - control_block / median(control))
    (each image divided by its own median, so a level difference between masters cancels);
  - block class from the control's per-block MAX pixel divided by the CONTROL's own image median
    (the review's helper did this; the reference's median would move the 50 / 200 / 10 thresholds
    by the master-to-reference flux ratio, 4 % on Horse C11 G):
        "star > 50 sky", "star > 200 sky" (a subset of the first), "no star > 10 sky" (max <= 10 sky);
  - per arm and class: n, rms and median of the residual;
  - with two or more arms: the 12 blocks with the largest |arm0 - arm1| (block medians, in % of
    arm 0's own image median: the unit that reproduces the review's printed column; the LN
    reference's median, 0.06 % higher on Horse C11 G, moves a few rows by 0.01) with their
    centres, each arm's residual and the control's max pixel over the control's median.

--ref is the LN reference plane; its median is printed for the record (the reference sky of the
LN stage) and nothing else is read from it.
"""
import os
import sys

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "acceptance"))
from imgcmp import read  # noqa: E402

CLASSES = (("star > 50 sky", lambda mx: mx > 50.0),
           ("star > 200 sky", lambda mx: mx > 200.0),
           ("no star > 10 sky", lambda mx: mx <= 10.0))


def plane(path, index):
    a = read(path)
    return a[index] if a.ndim == 3 else a


def block_reduce(a, size, fn):
    h, w = a.shape
    nh, nw = h // size, w // size
    t = a[:nh * size, :nw * size].reshape(nh, size, nw, size).transpose(0, 2, 1, 3).reshape(nh, nw, size * size)
    return fn(t, axis=2)


def main():
    args = sys.argv[1:]
    opts = {"--control": None, "--ref": None, "--block": "96", "--plane": "0"}
    for key in opts:
        if key in args:
            i = args.index(key)
            opts[key] = args[i + 1]
            del args[i:i + 2]
    arms = []
    for a in args:
        if "=" not in a:
            sys.exit(f"cannot read argument {a!r} (want <label>=<master>)")
        label, path = a.split("=", 1)
        arms.append((label, path))
    if not arms or opts["--control"] is None or opts["--ref"] is None:
        sys.exit(__doc__)
    size, pl = int(opts["--block"]), int(opts["--plane"])
    ref = plane(opts["--ref"], pl)
    sky = float(np.nanmedian(ref))
    ctl = plane(opts["--control"], pl)
    ctl_med = float(np.nanmedian(ctl))
    ctl_blocks = block_reduce(ctl, size, np.nanmedian)
    ctl_max = block_reduce(ctl, size, np.nanmax) / ctl_med
    nh, nw = ctl_blocks.shape
    print(f"control {os.path.basename(opts['--control'])}: {nh} x {nw} blocks of {size} px, "
          f"sky (reference median) {sky:.6g}, control median {ctl_med:.6g}")

    blocks, resid, med0 = {}, {}, None
    for label, path in arms:
        img = plane(path, pl)
        if med0 is None:
            med0 = float(np.nanmedian(img))
        if img.shape != ctl.shape:
            sys.exit(f"{label}: shape {img.shape} is not the control's {ctl.shape}")
        blocks[label] = block_reduce(img, size, np.nanmedian)
        resid[label] = 100.0 * (blocks[label] / float(np.nanmedian(img)) - ctl_blocks / ctl_med)

    for name, sel in CLASSES:
        mask = sel(ctl_max) & np.isfinite(ctl_max)
        parts = []
        for label, _ in arms:
            r = resid[label][mask & np.isfinite(resid[label])]
            parts.append(f"rms({label}-control)={np.sqrt(np.mean(r ** 2)):.3f} %" if r.size else f"rms({label})=n/a")
        meds = []
        for label, _ in arms:
            r = resid[label][mask & np.isfinite(resid[label])]
            meds.append(f"median({label}-control)={np.median(r):+.3f}" if r.size else f"median({label})=n/a")
        print(f"   blocks with {name:17s} n={int(mask.sum()):5d}: " + " ".join(parts) + "   " + " ".join(meds))

    if len(arms) >= 2:
        (l0, _), (l1, _) = arms[0], arms[1]
        diff = 100.0 * (blocks[l0] - blocks[l1]) / med0
        order = np.argsort(-np.abs(np.where(np.isfinite(diff), diff, 0.0)), axis=None)[:12]
        print(f"\ntop 12 |{l0}-{l1}| {size}-px blocks (% of {l0}'s median); block centre; each arm's structure residual vs the control (% ); "
              "max pixel of the block in the control / the control's median")
        for flat in order:
            j, i = divmod(int(flat), nw)
            cols = "  ".join(f"{label}-control {resid[label][j, i]:+7.2f}" for label, _ in arms)
            print(f"   ({int((i + 0.5) * size):6d}, {int((j + 0.5) * size):6d}) {l0}-{l1} {diff[j, i]:+7.2f}  {cols}   "
                  f"control max {ctl_max[j, i]:7.1f}x median")


if __name__ == "__main__":
    main()
