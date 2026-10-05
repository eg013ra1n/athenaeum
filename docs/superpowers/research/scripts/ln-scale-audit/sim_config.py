#!/usr/bin/env python3
"""Config for `ln_scale_probe --stack-sim` on a frame-set group (ln-scale-audit 2026-10-04).

    sim_config.py [--arms <probe.jsonl>] <frameset-frames.json> <group-key> <reference-stars.csv> \
        <member-truth.csv> <warped-dir> <ln-reference.fits> <master.fits> <f_ref> <f_phot> <out.json> [n_stars]

<reference-stars.csv> is the probe's `--stars-dir` reference.p0.csv (every accepted LN
reference fit). Stars: unsaturated (amp + b < 0.5), no other fit brighter than 2 % of the
star within 40 px, 64 px from the edges, the brightest n_stars (default 400).
<member-truth.csv> has handle, applied, phot, fwhm_mem, fwhm_ref per member (frameset_decomp
output or equivalent); a member without a truth gets the correction its PSF variance
predicts from the group's own linear fit of gap on delta sigma^2. The correction is
c = phot / applied (the truth over the applied scale). Members read their warped plane
from <warped-dir>/f<frame>.p0.fits (the probe's --dump-warped output).

--arms <probe.jsonl>: the probe's main-mode output (one line per member label f<frame>, an
"arms" object per line). Every member then carries `arms[name] = s_arm / applied` for every
arm of its line, which `ln_scale_probe --stack-sim` simulates beside the truth arm (the
`sim_<name>` / `sim_<name>_norej` columns). `applied` is the value the truth correction uses
(the truth csv's, else the frame's own `lnScale`). A member with no probe line, or a line whose
arm is not finite, is an error: an arm over a subset of the group would not be comparable.
"""
import csv
import json
import sys

import numpy as np

sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parent))
from stars import load  # noqa: E402


def load_arms(path):
    """-> {label: {arm: s}} from the probe's jsonl (a line without "arms" is skipped)."""
    out = {}
    for line in open(path):
        line = line.strip()
        if not line:
            continue
        j = json.loads(line)
        if "arms" in j:
            out[j["label"]] = {k: v["s"] for k, v in j["arms"].items() if isinstance(v, dict) and "s" in v}
    return out


def main():
    argv = sys.argv[1:]
    arms_path = None
    if "--arms" in argv:
        i = argv.index("--arms")
        arms_path = argv[i + 1]
        del argv[i:i + 2]
    (frames_json, key, ref_csv, truth_csv, warped, ref_fits, master, f_ref, f_phot, out) = argv[:10]
    n_stars = int(argv[10]) if len(argv) > 10 else 400
    arm_scales = load_arms(arms_path) if arms_path else None
    ref = load(ref_csv)
    x, y, sig, amp, b = ref["x"], ref["y"], ref["sig"], ref["amp"], ref["b"]
    ok = (amp + b < 0.5) & (x > 64) & (y > 64) & (sig > 0)
    order = np.argsort(y)
    ys, xs, ss = y[order], x[order], sig[order]
    iso = np.ones(len(x), bool)
    for i in np.nonzero(ok)[0]:
        lo, hi = np.searchsorted(ys, y[i] - 40), np.searchsorted(ys, y[i] + 40)
        d2 = (xs[lo:hi] - x[i]) ** 2 + (ys[lo:hi] - y[i]) ** 2
        iso[i] = not ((d2 > 0.25) & (d2 <= 1600) & (ss[lo:hi] > 0.02 * sig[i])).any()
    cand = np.nonzero(ok & iso)[0]
    cand = cand[np.argsort(-sig[cand])][:n_stars]
    stars = [[float(x[i]), float(y[i])] for i in cand]
    truth = {int(r["handle"]): r for r in csv.DictReader(open(truth_csv))}
    s2 = lambda fw: (fw / 2.3548) ** 2
    pts = np.array([(s2(float(r["fwhm_mem"])) - s2(float(r["fwhm_ref"])),
                     float(r["applied"]) / float(r["phot"]) - 1) for r in truth.values()])
    slope, icpt = np.polyfit(pts[:, 0], pts[:, 1], 1)
    fref_hm = float(next(iter(truth.values()))["fwhm_ref"])
    members = []
    for f in json.load(open(frames_json)):
        if f["group"] != key or not f["included"]:
            continue
        t = truth.get(f["frame"])
        if t:
            c = float(t["phot"]) / float(t["applied"])
        else:
            # The measured FWHM is the fit's, not the half-maximum one the regression is on;
            # good enough for the one or two members whose truth photometry found too few stars.
            c = 1.0 / (1.0 + icpt + slope * (s2(f["fwhm"] * 1.1) - s2(fref_hm)))
            print(f"frame {f['frame']}: no truth, regression correction {c:.4f}", file=sys.stderr)
        m = dict(label=f"f{f['frame']}", registered=f"{warped}/f{f['frame']}.p0.fits",
                 athln=f["athlnPath"], weight=f["weight"], correction=c)
        if arm_scales is not None:
            line = arm_scales.get(m["label"])
            if not line:
                sys.exit(f"{m['label']}: no arms line in {arms_path}")
            applied = float(t["applied"]) if t else float(f["lnScale"])
            m["arms"] = {}
            for name, sv in sorted(line.items()):
                if not np.isfinite(sv) or sv <= 0:
                    sys.exit(f"{m['label']}: arm {name} has no finite scale ({sv})")
                m["arms"][name] = sv / applied
        members.append(m)
    cfg = dict(reference=ref_fits, master=master, plane=0, stars=stars, half=32, sigmaLow=5.0, sigmaHigh=3.5,
               fRef=float(f_ref), fPhot=float(f_phot), interpolation="bicubicBSpline", clamping=0.3,
               members=members, outStars=out.replace(".json", "-stars.csv"), outFrames=out.replace(".json", "-frames.csv"))
    json.dump(cfg, open(out, "w"))
    print(f"{len(stars)} stars, {len(members)} members -> {out}")


if __name__ == "__main__":
    main()
