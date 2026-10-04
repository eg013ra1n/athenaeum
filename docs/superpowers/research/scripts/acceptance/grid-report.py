#!/usr/bin/env python3
"""Run B's report (spec §15.3, plan 6b-3 D21, D23): for each group of the
latest finished run of a project in an acceptance copy — the grid; the
coverage map (min, max, histogram, the coverage >= K rectangle); and, on the
master cropped to that rectangle, FWHM in px and arcsec, eccentricity, the
noise per pixel and per arcsec² (D23: sigma_px * scale, the noise of a
one-arcsec² mean of WHITE noise — a stack's pixels are correlated by
resampling, more so by drizzle, so this is a white-noise equivalent that
understates the true per-area noise by a scale-dependent factor; the
review's caveat on D23). No tolerance: the geometry differs from every
baseline.

    grid-report.py <work-dir> <project-id> [--k K] [--overlap] [--ln-truth]
    grid-report.py --self-check

K defaults to half the group's included frames, rounded up (D8's median
footprint). The crops are written under <work-dir>/grid-report/ and measured
with measure_probe (built into .superpowers/target-acc, as tier1-compare.py
expects). --overlap adds each member's overlap with its group's LN reference
on plane 0 (finite in both / finite in the member), the number D7's 25 %
rule reads; it reads every registered frame once.

Every master of the run also prints its pixel dims, the crop rectangle when the
run cropped (`stacking_runs.summary_json` carries no rectangle: it comes from the
run's "outputs cropped" info event, cross-checked against the FITS master's
CRPIX1/2 and the uncropped grid centre (W+1)/2, (H+1)/2) and the drizzle
master's dims.

--ln-truth (plan 6b-3 ruling R1): per group and per member, the local
normalization scale next to a photometric truth. The LN scale of a frame can
depend on its PSF width (+1/3 px^2 of PSF variance moves it by about 2.6 %) and
a project's members come from different instruments, so the scale is shown
beside the ratio of aperture fluxes of ~30-60 bright, isolated, unsaturated
reference stars measured in the member's and in the LN reference's registered
artifacts (median of reference/member, the same direction as the scale: the
sidecar applies v' = A*v + B, so A = reference/member). Per plane for OSC.
No tolerance, no pass/fail: the numbers go to the owner. The scale comes from
the "ln relative scale" debug events of <work-dir>/logs/*.jsonl, identified by
the `ln_plane` span (fields `frame_id` = the project-frame handle, `plane`), by
the run's [startedAt, finishedAt] window, and never under `ln_seeds_calibration`
(the throwaway arms of the seeds calibration); the applied scale is that event's
`ln_scale` times the group's seeds-calibration `k` for the plane when the event
came from the seeds path (`ln_scale_source`), cross-checked per member against
the main pass's "ln frame normalized" event (its mean over planes) and the
summary's `lnScale`. Every member of the run summary is listed with the cause
when it has no scale (excluded, cached, global normalization, LN failed), or
with "artifact unreadable" when it has one but its registered frame cannot be read.

The photometric truth is adaptive, per (member, plane), the same radii in both
frames: FWHM per frame from the half-maximum area of ~100 isolated stars (the
connected region around the peak, so a neighbour inside the window cannot
inflate it), F = max of the two; aperture radius max(3F, 5 px), sky annulus
[10F, 12F] (3-sigma-clipped mean), curve-of-growth correction c = median
flux(3F)/flux(8F) per frame, ratio = median(ref/member at 3F) * c_mem / c_ref.
Stars: no other candidate peak within 14F, no pixel brighter than
max(bg + 5 sigma, 2 % of the peak) between 3F and 14F in either frame (the
brief says "outside 1.5F" — a bright star's own wings exceed 5 sigma there),
the window finite in both frames (and non-zero in the LN reference), no
saturation in either frame (3x3 core with >= 5 pixels at (1 - 1.4/FWHM^2) of its peak above the sky, or
a peak above half of that frame's THIRD-brightest candidate peak, so one hot pixel or cosmic ray on a
candidate cannot disable the rule). n >= 10 is required;
with fewer, the crowded fallback (annulus [5F, 7F], r_ap = 5F, no correction,
isolation 9F) is tried and said so. The printed "+- SE" (labelled "bootstrap SE") is the sd of the WHOLE ratio
(the 3F median times the two curve-of-growth medians; the plain 5F median in the crowded fallback)
over 400 resamples of the kept stars with a fixed generator seed, so a report is reproducible.
--self-check runs synthetic checks of the photometry (Gaussian and Moffat, different widths), the
bootstrap SE against the seed-to-seed scatter, the saturation rules and the log parser.
"""
import json
import math
import re
import sqlite3
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from imgcmp import read  # noqa: E402

REPO_ROOT = HERE.parents[4]
MEASURE_PROBE = REPO_ROOT / ".superpowers" / "target-acc" / "release" / "examples" / "measure_probe"


def write_fits(path, data):
    """A minimal BITPIX -32 primary HDU: (h, w) or (c, h, w)."""
    dims = list(data.shape[::-1])
    cards = ["SIMPLE  =                    T", "BITPIX  =                  -32", f"NAXIS   = {len(dims):>20}"]
    cards += [f"NAXIS{i + 1:<3}= {d:>20}" for i, d in enumerate(dims)]
    cards.append("END")
    header = "".join(c.ljust(80) for c in cards).encode("ascii")
    header += b" " * (-len(header) % 2880)
    body = data.astype(">f4").tobytes()
    body += b"\0" * (-len(body) % 2880)
    Path(path).write_bytes(header + body)


def plane0(path):
    a = read(path)
    return a[0] if a.ndim == 3 else a


# --- master dims and crop rectangle (R2) ---------------------------------------


def fits_keys(path):
    """Header keywords of a FITS file as {key: raw value text}; {} for anything else."""
    if not path.lower().endswith((".fits", ".fit")):
        return {}
    keys, hdr_end = {}, False
    with open(path, "rb") as f:
        while not hdr_end:
            block = f.read(2880)
            if not block:
                break
            for i in range(0, len(block), 80):
                card = block[i:i + 80].decode("latin-1")
                kw = card[:8].strip()
                if kw == "END":
                    hdr_end = True
                    break
                if card[8:10] == "= " and kw not in keys:
                    keys[kw] = card[10:].split("/")[0].strip().strip("'").strip()
    return keys


def report_masters_dims(db, run_id, key, grid, summary, crops):
    """R2: pixel dims of every master row of the group, the crop rectangle, the drizzle dims.
    The rectangle comes from the "outputs cropped" info event of the run's log (any format) and,
    for a FITS master, from its CRPIX1/2 against the uncropped grid centre; a mismatch is flagged."""
    rows = db.execute(
        "SELECT kind, path, width, height, channels FROM master_lights WHERE run_id = ? AND group_key = ? ORDER BY kind",
        (run_id, key),
    ).fetchall()
    crop_k = ((summary.get("config") or {}).get("output") or {}).get("cropToCoverage", 0)
    gw, gh = grid["widthPx"], grid["heightPx"]
    master_dims = next(((w, h) for kind, _, w, h, _ in rows if kind == "master"), (gw, gh))
    for kind, path, w, h, ch in rows:
        print(f"  {kind}: {w} x {h} px, {ch} plane(s)  (grid {gw} x {gh})")
        if kind == "master":
            ev = crops.get(key)
            from_log = None
            if ev:
                x0, y0, cw, chh, message = ev
                from_log = (x0, y0, cw, chh)
                print(f"  crop (log event \"{message}\"): x0 {x0} y0 {y0} width {cw} height {chh}")
            elif crop_k:
                print("  crop: cropToCoverage is on but the log holds no 'outputs cropped' event for this group "
                      "(run made below info, or logs rotated away)")
            keys = fits_keys(path)
            if crop_k and "CRPIX1" in keys and "CRPIX2" in keys:
                x0 = round((gw + 1) / 2.0 - float(keys["CRPIX1"]))
                y0 = round((gh + 1) / 2.0 - float(keys["CRPIX2"]))
                print(f"  crop (FITS CRPIX1/2 {keys['CRPIX1']}/{keys['CRPIX2']}): x0 {x0} y0 {y0} width {w} height {h}")
                if from_log and from_log != (x0, y0, w, h):
                    print(f"  CROP MISMATCH: log {from_log} vs master header {(x0, y0, w, h)}")
            elif not crop_k:
                print(f"  crop: cropToCoverage off; master {'equals' if (w, h) == (gw, gh) else 'DIFFERS FROM'} the grid")
        elif kind == "drizzle":
            mw, mh = master_dims
            print(f"  drizzle master {w} x {h} px = {w / mw:.3f} x {h / mh:.3f} of the master's own dims")
    if not any(r[0] == "drizzle" for r in rows):
        print("  drizzle master: none")


# --- the log (R1) ----------------------------------------------------------------------------


def parse_ts(text):
    """RFC 3339 -> aware datetime; any fraction length, Z or an offset."""
    m = re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(\.\d+)?(Z|[+-]\d\d:\d\d)?$", text)
    if not m:
        raise ValueError(f"unparseable timestamp {text!r}")
    base, frac, tz = m.groups()
    frac = (frac or ".0")[:7].ljust(7, "0")
    tz = "+00:00" if tz in (None, "Z") else tz
    return datetime.fromisoformat(f"{base}{frac}{tz}")


def parse_event(line):
    """One JSONL line -> a tagged tuple, or None.
      ("plane", ts, frame_id, plane, ln_scale, source)  an "ln relative scale" debug event inside an
          `ln_plane` span and NOT inside `ln_seeds_calibration` (the calibration's throwaway arms);
      ("frame", ts, run_id, frame_id, ln_scale)         the main pass's "ln frame normalized" event;
      ("crop", ts, run_id, group_key, x0, y0, w, h, message)  "outputs cropped" / "outputs finalized uncropped"."""
    try:
        o = json.loads(line)
    except ValueError:
        return None
    f = o.get("fields") or {}
    msg = f.get("message")
    try:
        ts = parse_ts(o["timestamp"])
        if msg == "ln relative scale" and "ln_scale" in f:
            spans = o.get("spans") or [o.get("span") or {}]
            if any(sp.get("name") == "ln_seeds_calibration" for sp in spans):
                return None
            sp = next((sp for sp in reversed(spans) if sp.get("name") == "ln_plane"), None)
            if sp is None or "frame_id" not in sp or "plane" not in sp:
                return None
            return ("plane", ts, int(sp["frame_id"]), int(sp["plane"]), float(f["ln_scale"]), f.get("ln_scale_source", ""))
        if msg == "ln frame normalized" and "run_id" in f and "frame_id" in f and "ln_scale" in f:
            return ("frame", ts, int(f["run_id"]), int(f["frame_id"]), float(f["ln_scale"]))
        if msg in ("outputs cropped", "outputs finalized uncropped") and "run_id" in f and "group_key" in f:
            return ("crop", ts, int(f["run_id"]), f["group_key"], int(f["crop_x0"]), int(f["crop_y0"]),
                    int(f["crop_width"]), int(f["crop_height"]), msg)
    except (KeyError, ValueError, TypeError):
        return None
    return None


def read_log(log_dir, t0, t1, run_id):
    """The run's events: {"plane": {(frame_id, plane): (scale, source)}, "frame": {frame_id: scale},
    "crop": {group_key: (x0, y0, w, h, message)}, "seen": n}. Events carrying a run_id are filtered by it,
    those that do not (the plane events) by the [t0, t1] window; the last one wins."""
    out = {"plane": {}, "frame": {}, "crop": {}, "seen": 0}
    for path in sorted(Path(log_dir).glob("*")):
        if not path.is_file():
            continue
        with open(path, errors="replace") as fh:
            for line in fh:
                if "ln relative scale" not in line and "ln frame normalized" not in line and "outputs " not in line:
                    continue
                ev = parse_event(line)
                if ev is None:
                    continue
                if ev[0] == "plane":
                    if not (t0 <= ev[1] <= t1):
                        continue
                    out["plane"][(ev[2], ev[3])] = (ev[4], ev[5])
                elif ev[0] == "frame":
                    if ev[2] != run_id:
                        continue
                    out["frame"][ev[3]] = ev[4]
                else:
                    if ev[2] != run_id:
                        continue
                    out["crop"][ev[3]] = (ev[4], ev[5], ev[6], ev[7], ev[8])
                out["seen"] += 1
    return out


# --- adaptive aperture photometry (R1, fix round 1) ----------------------------------------------


def covered(win, zero_is_uncovered):
    """True when a window holds no uncovered pixel: a non-finite one always, a zero one only for the
    LN reference (registered artifacts of members are NaN-filled outside their footprint)."""
    if not np.isfinite(win).all():
        return False
    return not (zero_is_uncovered and (win == 0).any())


def sky_stats(img, zero_is_uncovered=False):
    ok = np.isfinite(img)
    if zero_is_uncovered:
        ok &= img != 0
    vals = img[ok]
    if vals.size == 0:
        return float("nan"), float("nan")
    bg = float(np.median(vals))
    return bg, 1.4826 * float(np.median(np.abs(vals - bg)))


def reference_candidates(ref):
    """Local maxima (brightest in their 7x7 neighbourhood) more than 20 sigma above the sky of the
    LN reference plane: rows (y, x, peak above the sky), brightest first, and the sky (bg, sigma)."""
    bg, sigma = sky_stats(ref, True)
    if not (sigma > 0):
        return np.zeros((0, 3)), bg, sigma
    ok = np.isfinite(ref) & (ref != 0)
    ys, xs = np.nonzero(np.where(ok, ref, bg) > bg + 20 * sigma)
    order = np.argsort(-ref[ys, xs])[:20000]
    rows = []
    for j in order:
        y, x = int(ys[j]), int(xs[j])
        if y < 3 or x < 3 or y >= ref.shape[0] - 3 or x >= ref.shape[1] - 3:
            continue
        w = ref[y - 3:y + 4, x - 3:x + 4]
        if not np.isfinite(w).all() or ref[y, x] < w.max():
            continue
        rows.append((y, x, float(ref[y, x]) - bg))
    return np.array(rows).reshape(-1, 3), bg, sigma


def clipped_mean(v, sigma=3.0, iters=6):
    v = v[np.isfinite(v)]
    for _ in range(iters):
        if v.size < 4:
            break
        m, s = v.mean(), v.std()
        keep = np.abs(v - m) <= sigma * s if s > 0 else np.ones(v.size, bool)
        if keep.all():
            break
        v = v[keep]
    return float(v.mean()) if v.size else float("nan")


class Neighbours:
    """Count of candidate peaks within r of (y, x), via a y-sorted index (a dense field holds thousands)."""

    def __init__(self, pos):
        order = np.argsort(pos[:, 0])
        self.y, self.x = pos[order, 0], pos[order, 1]

    def count(self, y, x, r):
        lo, hi = np.searchsorted(self.y, y - r, side="left"), np.searchsorted(self.y, y + r, side="right")
        return int((((self.y[lo:hi] - y) ** 2 + (self.x[lo:hi] - x) ** 2) <= r * r).sum())


def is_saturated(img, y, x, bg, fwhm):
    """A flat top: the 3x3 core holds >= 5 of its 9 pixels at or above (1 - 1.4 / FWHM^2) of the peak ABOVE
    THE SKY (`bg`, this frame's). The sky is subtracted because a pedestal makes every core pixel look
    equal; the tolerance follows the PSF because a broad unsaturated star really is that flat. Five, not
    three: a star centred on a pixel corner has FOUR equal core pixels (a threshold of 3 flagged 26-35 % of
    unsaturated stars at random sub-pixel positions), and at most four can be that equal without a clip.
    Hard saturation is also caught by the "> 50 % of the third-brightest candidate peak" rule
    (`peak_yardstick`) in `photometry` and `frame_fwhm`. `fwhm` is this frame's, F0 before it is known."""
    core = img[y - 1:y + 2, x - 1:x + 2] - bg
    m = float(core.max())
    if not m > 0:
        return False
    tol = 1.0 - 1.4 / max(float(fwhm), 2.0) ** 2
    return int((core >= tol * m).sum()) >= 5


def peak_yardstick(peaks):
    """The saturation yardstick of the "> 50 %" brightness rule: the THIRD-brightest finite candidate
    peak above the sky (the brightest when fewer than three are finite; NaN when none is). Not the
    brightest: one outlier pixel (cosmic ray, hot pixel) sitting on a candidate would raise the
    yardstick past every clipped star and disable the rule."""
    v = np.sort(np.asarray(peaks, dtype=np.float64))
    v = v[np.isfinite(v)]
    if v.size == 0:
        return float("nan")
    return float(v[-3] if v.size >= 3 else v[-1])


def half_max_fwhm(win, f0):
    """FWHM of the star at the centre of `win` (side 2*half+1): bg = median of the annulus
    [3 f0, 4 f0]; peak = max of the 3x3 core - bg; N = size of the connected region (4-neighbours)
    within 3 f0 of the centre with (v - bg) >= peak/2, containing the peak pixel; FWHM = 2 sqrt(N/pi).
    The connected region is the choice that keeps a neighbour inside the window from inflating N."""
    half = win.shape[0] // 2
    yy, xx = np.mgrid[-half:half + 1, -half:half + 1]
    rr = np.hypot(yy, xx)
    ann = (rr >= 3 * f0) & (rr <= 4 * f0)
    bg = float(np.median(win[ann]))
    core = win[half - 1:half + 2, half - 1:half + 2] - bg
    peak = float(core.max())
    if peak <= 0:
        return float("nan")
    cy, cx = np.unravel_index(int(np.argmax(core)), core.shape)
    start = (half - 1 + cy, half - 1 + cx)
    mask = ((win - bg) >= 0.5 * peak) & (rr <= 3 * f0)
    seen = {start}
    stack = [start]
    while stack:
        y, x = stack.pop()
        for ny, nx in ((y - 1, x), (y + 1, x), (y, x - 1), (y, x + 1)):
            if (ny, nx) not in seen and 0 <= ny < win.shape[0] and 0 <= nx < win.shape[1] and mask[ny, nx]:
                seen.add((ny, nx))
                stack.append((ny, nx))
    return 2.0 * math.sqrt(len(seen) / math.pi)


def frame_fwhm(img, cand, zero_is_uncovered, f0=8.0, want=100):
    """Median FWHM (px) of up to `want` isolated, unsaturated candidates of one frame, and how many
    stars it came from. A star is isolated for this when no other candidate lies within 30 px."""
    half = int(math.ceil(4 * f0))
    if len(cand) == 0:
        return float("nan"), 0
    bg_frame, _ = sky_stats(img, zero_is_uncovered)
    pos = cand[:, :2]
    peaks = cand[:, 2]
    yard = peak_yardstick(peaks)
    nb = Neighbours(pos)
    out = []
    for i in range(len(cand)):
        y, x = int(pos[i, 0]), int(pos[i, 1])
        if y < half or x < half or y >= img.shape[0] - half or x >= img.shape[1] - half:
            continue
        if nb.count(y, x, 30.0) > 1:
            continue
        win = img[y - half:y + half + 1, x - half:x + half + 1]
        if not covered(win, zero_is_uncovered) or is_saturated(img, y, x, bg_frame, f0):
            continue
        if peaks[i] > 0.5 * yard:
            continue
        fw = half_max_fwhm(win, f0)
        if fw == fw:
            out.append(fw)
        if len(out) >= want:
            break
    if not out:
        return float("nan"), 0
    return float(np.median(out)), len(out)


def aperture_flux(win, rr, r, ann_mask):
    bg = clipped_mean(win[ann_mask])
    return float((win[rr <= r] - bg).sum())


BOOT_RESAMPLES = 400
BOOT_SEED = 61003  # fixed: the same frames always print the same SE


def photometry(ref, mem, cand_info=None, f_ref=None):
    """Adaptive ratio ref/member for one plane (fix round 1, ruling set): see the module doc.
    Returns a dict with fwhm_ref, fwhm_mem, mode, r_ap, ann, n, ratio, se (bootstrap sd of the whole
    ratio over the kept stars), unc {3F, 5F, 8F}, stars [(y, x) of the stars used], note."""
    cand, bg_r, sig_r = cand_info if cand_info is not None else reference_candidates(ref)
    res = {"fwhm_ref": float("nan"), "fwhm_mem": float("nan"), "mode": "", "r_ap": float("nan"),
           "ann": (float("nan"), float("nan")), "n": 0, "ratio": float("nan"), "se": float("nan"),
           "unc": {}, "stars": [], "note": ""}
    if len(cand) == 0:
        res["note"] = "no candidate star above 20 sigma in the LN reference"
        return res
    fr = f_ref if f_ref is not None else frame_fwhm(ref, cand, True)
    # The member's candidate peaks at the reference's own positions: its saturation yardstick.
    mpk = np.array([float(mem[int(y), int(x)]) for y, x, _ in cand])
    bg_m, sig_m = sky_stats(mem)
    fm, _ = frame_fwhm(mem, np.column_stack([cand[:, :2], mpk - bg_m]), False)
    res["fwhm_ref"], res["fwhm_mem"] = fr[0], fm
    if not (fr[0] == fr[0] and fm == fm):
        res["note"] = f"FWHM not measurable (reference {fr[1]} stars, member {'ok' if fm == fm else 'none'})"
        return res
    big_f = max(fr[0], fm)
    cut_log = []
    mem_peak_max = peak_yardstick(mpk - bg_m)
    ref_peak_max = peak_yardstick(cand[:, 2])
    for mode in ("normal", "crowded"):
        if mode == "normal":
            r_ap, a_in, a_out, r_iso = max(3 * big_f, 5.0), 10 * big_f, 12 * big_f, 14 * big_f
            a_out = max(a_out, a_in + 4.0)
        else:
            r_ap, a_in, a_out, r_iso = 5 * big_f, 5 * big_f, 7 * big_f, 9 * big_f
            a_out = max(a_out, a_in + 4.0)
        half = int(math.ceil(max(r_iso, a_out)))
        yy, xx = np.mgrid[-half:half + 1, -half:half + 1]
        rr = np.hypot(yy, xx)
        ann = (rr >= a_in) & (rr <= a_out)
        beyond = (rr > 3 * big_f) & (rr <= r_iso)
        nb = Neighbours(cand[:, :2])
        stars = []
        cuts = {"edge": 0, "candidate isolation": 0, "brightness": 0, "coverage": 0, "flat top": 0,
                "pixel isolation": 0}
        for i in range(len(cand)):
            y, x, pk = int(cand[i, 0]), int(cand[i, 1]), cand[i, 2]
            if (y < half or x < half or y >= ref.shape[0] - half or x >= ref.shape[1] - half
                    or y >= mem.shape[0] - half or x >= mem.shape[1] - half):
                cuts["edge"] += 1
                continue
            if nb.count(y, x, r_iso) > 1:
                cuts["candidate isolation"] += 1
                continue
            if pk > 0.5 * ref_peak_max or (mpk[i] - bg_m) > 0.5 * mem_peak_max:
                cuts["brightness"] += 1
                continue
            wr = ref[y - half:y + half + 1, x - half:x + half + 1]
            wm = mem[y - half:y + half + 1, x - half:x + half + 1]
            if not covered(wr, True) or not covered(wm, False):
                cuts["coverage"] += 1
                continue
            if is_saturated(ref, y, x, bg_r, fr[0]) or is_saturated(mem, y, x, bg_m, fm):
                cuts["flat top"] += 1
                continue
            if ((wr[beyond] > bg_r + max(5 * sig_r, 0.02 * pk)).any()
                    or (wm[beyond] > bg_m + max(5 * sig_m, 0.02 * (mpk[i] - bg_m))).any()):
                cuts["pixel isolation"] += 1
                continue
            stars.append((y, x, wr, wm))
            if len(stars) >= 60:
                break
        cut_text = f"{mode}: " + ", ".join(f"{k} {v}" for k, v in cuts.items()) + f"; kept {len(stars)}"
        cut_log.append(cut_text)
        if len(stars) >= 10:
            break
    res["mode"], res["r_ap"], res["ann"], res["n"] = mode, r_ap, (a_in, a_out), len(stars)
    if len(stars) < 10:
        res["note"] = (f"only {len(stars)} usable stars (>= 10 needed) even in the crowded fallback "
                       f"[5F, 7F]: no ratio (F = {big_f:.2f} px; rejected per cut, of {len(cand)} candidates: "
                       + " | ".join(cut_log) + ")")
        return res
    # Normal: apertures at 3F (r_ap), 5F, 8F. Crowded: the annulus starts at 5F, so only r_ap = 5F exists.
    k_ap = "3F" if mode == "normal" else "5F"
    radii = ({"3F": r_ap, "5F": 5 * big_f, "8F": 8 * big_f} if mode == "normal" else {"5F": r_ap})
    fl = {"ref": {k: [] for k in radii}, "mem": {k: [] for k in radii}}
    for _, _, wr, wm in stars:
        for name, w in (("ref", wr), ("mem", wm)):
            bgm = clipped_mean(w[ann])
            for k, r in radii.items():
                fl[name][k].append(float((w[rr <= r] - bgm).sum()))
    fl = {n: {k: np.array(v) for k, v in d.items()} for n, d in fl.items()}
    good = np.ones(len(stars), bool)
    for n in fl:
        for k in radii:
            good &= fl[n][k] > 0
    if good.sum() < 10:
        res["note"] = (f"only {int(good.sum())} stars with positive fluxes: no ratio (rejected per cut: "
                       + " | ".join(cut_log) + ")")
        res["n"] = int(good.sum())
        return res
    res["n"] = int(good.sum())
    res["stars"] = [(y, x) for (y, x, _, _), g in zip(stars, good) if g]
    for k in radii:
        res["unc"][k] = float(np.median(fl["ref"][k][good] / fl["mem"][k][good]))
    per = fl["ref"][k_ap][good] / fl["mem"][k_ap][good]
    med = float(np.median(per))
    # The SE is a bootstrap over the kept stars of the WHOLE estimator: in normal mode the 3F median AND
    # the two curve-of-growth medians move together on each resampled star set (their noise is most of
    # the total; an SE of the 3F median alone was about 4x too small against the seed-to-seed scatter).
    idx = np.random.default_rng(BOOT_SEED).integers(0, res["n"], size=(BOOT_RESAMPLES, res["n"]))
    if mode == "normal":
        cog_ref = fl["ref"]["3F"][good] / fl["ref"]["8F"][good]
        cog_mem = fl["mem"]["3F"][good] / fl["mem"]["8F"][good]
        c_ref, c_mem = float(np.median(cog_ref)), float(np.median(cog_mem))
        res["ratio"] = med * c_mem / c_ref
        boot = (np.median(per[idx], axis=1) * np.median(cog_mem[idx], axis=1)
                / np.median(cog_ref[idx], axis=1))
        res["note"] = f"curve of growth c_ref {c_ref:.4f} c_mem {c_mem:.4f}"
    else:
        res["ratio"] = med
        boot = np.median(per[idx], axis=1)
        res["note"] = ("crowded fallback: isolation 9F, annulus [5F, 7F], r_ap = 5F, no curve-of-growth correction; "
                       "systematic about +0.2 % at beta 3, +0.6 to +0.8 % at beta 2.5 (uncorrected, the aperture "
                       "loses the wings); rejected per cut: " + " | ".join(cut_log))
    res["se"] = float(np.std(boot, ddof=1))
    return res


def planes_of(path):
    a = read(path)
    return [a] if a.ndim == 2 else [a[i] for i in range(a.shape[0])]


# --- the LN truth section ---------------------------------------------------------------------------


def finite_pos(x):
    return isinstance(x, (int, float)) and math.isfinite(x) and x > 0


def ln_truth_group(db, pid, key, summary, log):
    """One group: every member of the run's own summary, the cause when it has no LN scale, and for
    the members with one the scale next to the photometric ratio. Returns tagged rows: ("member", instrument, cause), one per member (scaled,
    excluded, global, cached, failed or artifact unreadable), and ("plane", instrument, outcome, diff, mode)."""
    group = next((g for g in summary.get("groups", []) if g.get("key") == key), None)
    if group is None:
        print("  ln truth: the run summary has no such group")
        return []
    ref = db.execute(
        "SELECT path FROM stacking_artifacts WHERE project_id = ? AND group_key = ? AND kind = 'ln_reference'",
        (pid, key),
    ).fetchone()
    if not ref or not ref[0]:
        print("  ln truth: no LN reference for this group")
        return []
    registered = {}
    for handle, path in db.execute(
        "SELECT project_frame, path FROM stacking_artifacts WHERE project_id = ? AND group_key = ? "
        "AND kind = 'registered' AND project_frame IS NOT NULL ORDER BY id",
        (pid, key),
    ):
        registered[handle] = path
    globals_ = {}
    for handle, payload in db.execute(
        "SELECT project_frame, payload_json FROM stacking_artifacts WHERE project_id = ? AND group_key = ? "
        "AND kind = 'ln_global' AND project_frame IS NOT NULL ORDER BY id",
        (pid, key),
    ):
        try:
            globals_[handle] = json.loads(payload or "{}").get("overlap")
        except ValueError:
            globals_[handle] = None
    kcal = group.get("seedsCalibration") or []
    try:
        ref_planes = planes_of(ref[0])
    except Exception as e:  # noqa: BLE001
        print(f"  ln truth: the LN reference could not be read: {e}")
        return []
    cand = [reference_candidates(p) for p in ref_planes]
    fref = [frame_fwhm(p, c[0], True) for p, c in zip(ref_planes, cand)]
    print(f"  ln truth: LN reference FWHM per plane {[round(f[0], 3) for f in fref]} px "
          f"({[f[1] for f in fref]} stars); {len(group.get('frames', []))} members in the summary; "
          f"seeds calibration k {kcal or 'none'}")
    rows = []
    for fr in sorted(group.get("frames", []), key=lambda f: f["frameId"]):
        handle = fr["frameId"]
        r = db.execute(
            "SELECT p.manifest_json FROM stack_project_frames s JOIN project_frames_local p "
            "ON p.project_id = s.project_id AND p.frame_uuid = s.frame_uuid WHERE s.id = ?",
            (handle,),
        ).fetchone()
        meta = (json.loads(r[0]).get("meta") or {}) if r else {}
        instr = meta.get("instrume") or meta.get("INSTRUME") or "unknown"
        if not fr.get("included", False):
            print(f"    handle {handle} [{instr}]: excluded ({fr.get('exclusionReason')})")
            rows.append(("member", instr, "excluded"))
            continue
        if fr.get("lnScale") is None and handle in globals_:
            # Checked before the cached branch: a cached member on global normalization is a global one.
            ov = globals_[handle]
            print(f"    handle {handle} [{instr}]: global normalization{' (cached)' if fr.get('cachedLn') else ''}, "
                  f"overlap {('%.2f' % ov) if isinstance(ov, (int, float)) else '?'} < 0.25")
            rows.append(("member", instr, "global"))
            continue
        if fr.get("cachedLn"):
            print(f"    handle {handle} [{instr}]: LN cached from an earlier run (no event in this run); "
                  f"summary lnScale {fr.get('lnScale')}")
            rows.append(("member", instr, "cached"))
            continue
        if fr.get("lnScale") is None:
            print(f"    handle {handle} [{instr}]: LN FAILED (included, not cached, no lnScale, no global marker)")
            rows.append(("member", instr, "failed"))
            continue
        path = registered.get(handle)
        applied = []
        for p in range(len(ref_planes)):
            ev = log["plane"].get((handle, p))
            if ev is None:
                applied.append(None)
                continue
            raw, source = ev
            k = kcal[p] if source == "seeds" and p < len(kcal) and finite_pos(kcal[p]) else 1.0
            applied.append((raw, k, source, raw * k))
        done = [a[3] for a in applied if a]
        # cross-check against the main pass's event and the summary
        frame_scale = log["frame"].get(handle)
        if done and len(done) == len(ref_planes):
            mean = sum(done) / len(done)
            for label, want in (("'ln frame normalized' event", frame_scale), ("summary lnScale", fr.get("lnScale"))):
                if want is not None and abs(mean - want) > 1e-9 * abs(want):
                    print(f"    handle {handle}: MISMATCH applied mean {mean!r} vs {label} {want!r}")
        elif frame_scale is None and fr.get("lnScale") is not None:
            print(f"    handle {handle}: no 'ln frame normalized' event in this run's log")
        try:
            if not path:
                raise ValueError("no registered artifact path")
            mem_planes = planes_of(path)
        except Exception as e:  # noqa: BLE001 - one member's failure is a line of the report
            # A member-level cause (no plane of it was measured), in place of "scaled".
            print(f"    handle {handle} [{instr}]: scaled, but the registered artifact is unreadable: {e}")
            rows.append(("member", instr, "artifact unreadable"))
            continue
        rows.append(("member", instr, "scaled"))
        for p, mp in enumerate(mem_planes[:len(ref_planes)]):
            ph = photometry(ref_planes[p], mp, cand[p], fref[p])
            a = applied[p]
            head = f"    handle {handle} plane {p} [{instr}]: "
            if a is None:
                print(head + "no ln_plane event in the run's log window")
                rows.append(("plane", instr, "no event", float("nan"), ""))
                continue
            raw, k, source, app = a
            if ph["ratio"] != ph["ratio"]:
                print(head + f"ln_scale {raw:.5f} x k {k:.5f} = {app:.5f} ({source}); photometry: {ph['note']}")
                rows.append(("plane", instr, "no photometry", float("nan"), ph["mode"]))
                continue
            diff = app / ph["ratio"] - 1.0
            unc = " ".join(f"{k_}:{v:.4f}" for k_, v in ph["unc"].items())
            print(head + f"ln_scale {raw:.5f} x k {k:.5f} = {app:.5f} ({source})  photometric {ph['ratio']:.5f} "
                  f"+- {ph['se']:.5f} (bootstrap SE, n {ph['n']}, {ph['mode']}, FWHM ref {ph['fwhm_ref']:.2f} mem {ph['fwhm_mem']:.2f} px, "
                  f"r_ap {ph['r_ap']:.1f} annulus {ph['ann'][0]:.1f}-{ph['ann'][1]:.1f}; uncorrected {unc})  "
                  f"rel diff {diff * 100:+.3f} %")
            rows.append(("plane", instr, "ok", diff, ph["mode"]))
    return rows


def ln_truth_summary(rows):
    by = {}
    for row in rows:
        e = by.setdefault(row[1], {"member": {}, "plane": {}, "normal": [], "crowded": []})
        if row[0] == "member":
            e["member"][row[2]] = e["member"].get(row[2], 0) + 1
        else:
            e["plane"][row[2]] = e["plane"].get(row[2], 0) + 1
            if row[3] == row[3]:
                e["crowded" if row[4] == "crowded" else "normal"].append(abs(row[3]))
    print("\n== ln truth: |ln_scale / photometric - 1| per instrument (no tolerance, no verdict)")
    for instr, e in sorted(by.items()):
        fmt = lambda d: ", ".join(f"{c} {n}" for c, n in sorted(d.items())) or "none"  # noqa: E731
        print(f"  {instr}:")
        print(f"    members by cause (member level): {fmt(e['member'])}")
        print(f"    planes by outcome (plane level): {fmt(e['plane'])}")
        for label, v in (("normal rows", e["normal"]), ("crowded-fallback rows (carry a small uncorrected systematic)", e["crowded"])):
            if v:
                print(f"    {label}: n {len(v)}  median {np.median(v) * 100:.3f} %  max {max(v) * 100:.3f} %")
        if not e["normal"] and not e["crowded"]:
            print("    no plane with both a scale and a photometric ratio (a refusal, not a failure)")
    if not by:
        print("  no member in the run summary")


def report_group(db, pid, key, included, rows, run_id, k_arg, want_overlap, scale, out):
    cov = plane0(rows["coverage"])
    k = k_arg if k_arg is not None else max(1, math.ceil(included / 2))
    counts = np.bincount(np.nan_to_num(cov).astype(np.int64).ravel())
    ys, xs = np.nonzero(cov >= k)
    print(f"\n== {key}  ({included} frames, K = {k})")
    print(f"  coverage min {cov.min():.0f} max {cov.max():.0f}")
    print("  histogram " + ", ".join(f"{n}:{c}" for n, c in enumerate(counts) if c))
    if xs.size == 0:
        print(f"  no pixel reaches {k}: nothing measured")
        return
    x0, x1, y0, y1 = xs.min(), xs.max(), ys.min(), ys.max()
    print(f"  coverage >= {k}: x {x0}..{x1}  y {y0}..{y1}  ({x1 - x0 + 1} x {y1 - y0 + 1} px)")
    master = read(rows["master"])
    crop = master[..., y0:y1 + 1, x0:x1 + 1]
    crop_path = out / f"run{run_id}_{key}.fits"
    write_fits(crop_path, crop)
    proc = subprocess.run([str(MEASURE_PROBE), str(crop_path)], capture_output=True, text=True, timeout=600)
    if proc.returncode != 0:
        print(f"  measure_probe failed: {proc.stderr.strip()[-400:]}")
        return
    data = json.loads(proc.stdout)
    for i, c in enumerate(data if isinstance(data, list) else data["channels"]):
        print(
            f"  plane {i}: FWHM {c['fwhmPx']:.3f} px = {c['fwhmPx'] * scale:.3f}\"  "
            f"ecc {c['eccentricity']:.3f}  noise {c['noise']:.6g}/px = {c['noise'] * scale:.6g}/arcsec^2  "
            f"median {c['median']:.6g}  stars fitted {c['starsFitted']}"
        )
    if want_overlap:
        ref = db.execute(
            "SELECT path FROM stacking_artifacts WHERE project_id = ? AND group_key = ? AND kind = 'ln_reference'",
            (pid, key),
        ).fetchone()
        if not ref or not ref[0]:
            print("  overlap: no LN reference for this group")
            return
        ref_ok = np.isfinite(plane0(ref[0]))
        fractions = []
        for handle, path in db.execute(
            "SELECT project_frame, path FROM stacking_artifacts WHERE project_id = ? AND group_key = ? AND kind = 'registered'",
            (pid, key),
        ):
            try:
                if not path:
                    raise ValueError("the registered artifact row has no path")
                m = np.isfinite(plane0(path))
            except Exception as e:  # noqa: BLE001 - a member's failure is a line of the report, never a crash
                print(f"    handle {handle}: overlap not computed: {e}", file=sys.stderr)
                print(f"    handle {handle}: overlap not computed ({e})")
                continue
            fractions.append((float((m & ref_ok).sum()) / max(1, int(m.sum())), handle))
        if not fractions:
            print("  overlap: no readable registered frame")
            return
        fractions.sort()
        low = [f for f in fractions if f[0] < 0.25]
        print(f"  overlap with the LN reference: min {fractions[0][0]:.3f} median "
              f"{fractions[len(fractions) // 2][0]:.3f}; {len(low)} under 25 %")
        for frac, handle in low:
            print(f"    handle {handle}: {frac:.3f}")


def main():
    args = sys.argv[1:]
    if args == ["--self-check"]:
        self_check()
        return
    if len(args) < 2:
        sys.exit(__doc__)
    work, pid = Path(args[0]), args[1]
    k_arg = int(args[args.index("--k") + 1]) if "--k" in args else None
    want_overlap = "--overlap" in args
    want_truth = "--ln-truth" in args
    if not MEASURE_PROBE.exists():
        sys.exit(f"measure_probe not found at {MEASURE_PROBE}: build it into .superpowers/target-acc first")
    db = sqlite3.connect(work / "athenaeum.db")
    row = db.execute(
        "SELECT id, summary_json FROM stacking_runs WHERE project_id = ? AND status = 'done' ORDER BY id DESC LIMIT 1",
        (pid,),
    ).fetchone()
    if not row:
        sys.exit(f"no finished run of project {pid}")
    run_id, summary = row[0], json.loads(row[1])
    grid = summary["grid"]
    scale = grid["scaleArcsec"]
    print("noise per arcsec^2 below is the white-noise equivalent sigma_px * scale (plan 6b-3 D23)")
    print(f"run {run_id}  grid {grid['widthPx']}x{grid['heightPx']} @ {scale:.4f}\"/px  "
          f"centre ({grid['crval1']:.5f}, {grid['crval2']:.5f})  rotation {grid['rotationDeg']:.3f} deg")
    out = work / "grid-report"
    out.mkdir(exist_ok=True)
    if not summary.get("finishedAt"):
        sys.exit("the run's summary has no finishedAt: cannot window the log events")
    window = (parse_ts(summary["startedAt"]), parse_ts(summary["finishedAt"]))
    log = read_log(work / "logs", *window, run_id)
    print(f"log: {log['seen']} relevant events in {work / 'logs'} inside the run window "
          f"{window[0].isoformat()} .. {window[1].isoformat()} (run_id {run_id}); "
          f"{len(log['plane'])} frame/plane scales, {len(log['frame'])} frame events, {len(log['crop'])} crop events")
    if want_truth and not log["plane"]:
        print("  no ln_plane scale events found: was the run made at debug (project-run.sh sets ATHENAEUM_LOG), "
              "and does the build carry the ln_plane span commits?")
    truth_rows = []
    groups = db.execute(
        "SELECT group_key, included_count FROM stacking_run_groups WHERE run_id = ? ORDER BY group_key", (run_id,)
    ).fetchall()
    for key, included in groups:
        rows = dict(
            db.execute(
                "SELECT kind, path FROM master_lights WHERE run_id = ? AND group_key = ?", (run_id, key)
            ).fetchall()
        )
        if "master" not in rows or "coverage" not in rows:
            print(f"\n== {key}: no master or no coverage map (coverageMap off?) — skipped")
        else:
            report_group(db, pid, key, included, rows, run_id, k_arg, want_overlap, scale, out)
        report_masters_dims(db, run_id, key, grid, summary, log["crop"])
        if want_truth:
            truth_rows += ln_truth_group(db, pid, key, summary, log)
    if want_truth:
        ln_truth_summary(truth_rows)


# --- self-check (no real data) ------------------------------------------------------------


def psf_image(size, centres, fluxes, kind, fwhm, rng, bg=100.0, noise=0.5, extra=()):
    """A float image with unit-flux-normalised stars of the given shape: `gauss` or `moffat` (beta 3)."""
    yy, xx = np.mgrid[0:size, 0:size].astype(np.float32)
    img = np.full((size, size), bg, np.float32)
    if kind == "gauss":
        s = fwhm / 2.3548
        norm, alpha, beta = 1.0 / (2 * math.pi * s * s), None, None
    else:
        beta = 3.0
        alpha = fwhm / (2 * math.sqrt(2 ** (1 / beta) - 1))
        norm = (beta - 1) / (math.pi * alpha * alpha)
    for (cy, cx), flux in list(zip(centres, fluxes)) + list(extra):
        r2 = (yy - cy) ** 2 + (xx - cx) ** 2
        img += flux * norm * (np.exp(-r2 / (2 * s * s)) if kind == "gauss" else (1 + r2 / (alpha * alpha)) ** -beta)
    img += rng.normal(0, noise, img.shape).astype(np.float32)
    return img


def self_check():
    rng = np.random.default_rng(7)
    # A 9 x 9 grid of stars 140 px apart: farther than 14F for F <= 7 (98 px), so every star is isolated.
    size, margin, step = 1340, 110, 140
    centres = [(margin + i * step + 0.3 * j, margin + j * step + 0.2 * i) for i in range(9) for j in range(9)]
    fluxes = rng.uniform(5e4, 3e5, len(centres))
    corner = [((30.0, 30.0), 4e4), ((size - 30.0, size - 30.0), 4e4)]  # member-only stars outside every window
    cases = [("moffat", 4.0, "moffat", 6.0), ("gauss", 3.0, "gauss", 7.0), ("gauss", 4.0, "gauss", 4.0)]
    for kr, fr, km, fm in cases:
        ref = psf_image(size, centres, fluxes, kr, fr, rng)
        mem = psf_image(size, centres, fluxes / 1.25, km, fm, rng, extra=corner)
        with tempfile.TemporaryDirectory() as d:  # through the real file writer and reader
            write_fits(Path(d) / "a.fits", ref)
            write_fits(Path(d) / "b.fits", mem)
            a, b = plane0(str(Path(d) / "a.fits")), plane0(str(Path(d) / "b.fits"))
        ph = photometry(a, b)
        unc = " ".join(f"{k}:{v:.4f}" for k, v in ph["unc"].items())
        print(f"{kr} FWHM {fr} vs {km} FWHM {fm}: ratio {ph['ratio']:.5f} +- {ph['se']:.5f} (bootstrap SE, n {ph['n']}, {ph['mode']}, "
              f"measured FWHM {ph['fwhm_ref']:.2f}/{ph['fwhm_mem']:.2f}, r_ap {ph['r_ap']:.1f}); "
              f"uncorrected {unc}; truth 1.25000")
        assert ph["n"] >= 10 and ph["ratio"] == ph["ratio"], ph["note"]
        assert abs(ph["ratio"] / 1.25 - 1) < 0.003, (kr, fr, km, fm, ph["ratio"])
    # The bootstrap SE against the seed-to-seed scatter (fix round 4): Gaussian FWHM 4 vs 6.5 px at noise 10.
    # One field (positions and fluxes fixed), a new noise realisation per seed. The rms of the per-seed
    # bootstrap SEs must sit within a factor 1.5 of the sd of the per-seed ratios. 40 seeds, not 6: the sd
    # of 6 ratios itself scatters by about 30 %, which would make a factor-1.5 test a coin toss.
    se_rng = np.random.default_rng(21)
    se_fluxes = se_rng.uniform(5e4, 3e5, len(centres))
    se_ref = psf_image(size, centres, se_fluxes, "gauss", 4.0, se_rng, noise=0.0)
    se_mem = psf_image(size, centres, se_fluxes / 1.25, "gauss", 6.5, se_rng, noise=0.0)
    seed_ratios, seed_ses, seed_ns = [], [], []
    for seed in range(40):
        nrng = np.random.default_rng(1000 + seed)
        ph = photometry(se_ref + nrng.normal(0, 10.0, se_ref.shape).astype(np.float32),
                        se_mem + nrng.normal(0, 10.0, se_mem.shape).astype(np.float32))
        assert ph["ratio"] == ph["ratio"], (seed, ph["note"])
        seed_ratios.append(ph["ratio"])
        seed_ses.append(ph["se"])
        seed_ns.append(ph["n"])
    seed_sd = float(np.std(seed_ratios, ddof=1))
    boot_se = math.sqrt(float(np.mean(np.square(seed_ses))))
    print(f"gauss FWHM 4.0 vs gauss FWHM 6.5 at noise 10, 40 seeds (n {min(seed_ns)}-{max(seed_ns)}): mean ratio "
          f"{np.mean(seed_ratios):.5f} (truth 1.25000); seed-to-seed sd {seed_sd:.5f} ({seed_sd / 1.25 * 100:.3f} %), "
          f"bootstrap SE rms {boot_se:.5f} ({boot_se / 1.25 * 100:.3f} %, per seed {min(seed_ses):.5f}-{max(seed_ses):.5f}); "
          f"SE / sd {boot_se / seed_sd:.3f} (must be within 1/1.5 .. 1.5)")
    assert 1 / 1.5 <= boot_se / seed_sd <= 1.5, (boot_se, seed_sd)
    # Saturation test: a broad, unsaturated star on a bright sky pedestal is NOT a flat top; a clipped one is.
    sat_rng = np.random.default_rng(3)
    pedestal, peak = 8000.0, 3000.0
    sigma_px = 6.5 / 2.3548
    broad = psf_image(201, [(100.3, 100.2)], [peak * 2 * math.pi * sigma_px ** 2], "gauss", 6.5, sat_rng,
                      bg=pedestal, noise=2.0)
    by, bx = np.unravel_index(int(np.argmax(broad[90:111, 90:111])), (21, 21))
    by, bx = by + 90, bx + 90
    bg_est, _ = sky_stats(broad)
    flagged = is_saturated(broad, by, bx, bg_est, 6.5)
    print(f"broad star (FWHM 6.5 px, peak {peak:.0f} on sky {pedestal:.0f}): flagged = {flagged}")
    assert not flagged
    clipped = np.minimum(broad, pedestal + 0.6 * peak)
    flagged_clip = is_saturated(clipped, by, bx, bg_est, 6.5)
    print(f"clipped star (flat top at 60 % of the peak): flagged = {flagged_clip}")
    assert flagged_clip
    # (a) No unsaturated star is flagged, at any sub-pixel position and any width.
    flag_rng = np.random.default_rng(11)
    for fw in (2.2, 4.0, 6.5):
        sg = fw / 2.3548
        flagged_n = 0
        yy, xx = np.mgrid[0:15, 0:15].astype(np.float64)
        for _ in range(2000):
            cy, cx = 7 + flag_rng.uniform(-0.5, 0.5, 2)
            img = 8000.0 + 3000.0 * np.exp(-((yy - cy) ** 2 + (xx - cx) ** 2) / (2 * sg * sg))
            py, px = np.unravel_index(int(np.argmax(img)), img.shape)
            flagged_n += is_saturated(img, py, px, 8000.0, fw)
        print(f"2000 unsaturated Gaussians at FWHM {fw} px, random sub-pixel centres: flagged {flagged_n}")
        assert flagged_n == 0, (fw, flagged_n)
    # (b) A star clipped at half its peak, FWHM 6.5, centred on a pixel: flagged.
    yy, xx = np.mgrid[0:15, 0:15].astype(np.float64)
    sg = 6.5 / 2.3548
    img = np.minimum(8000.0 + 3000.0 * np.exp(-((yy - 7) ** 2 + (xx - 7) ** 2) / (2 * sg * sg)), 8000.0 + 1500.0)
    assert is_saturated(img, 7, 7, 8000.0, 6.5)
    print("star clipped at 0.5 of its peak (FWHM 6.5, pixel-centred): flagged = True")
    # (c) One outlier pixel does not disable the brightness rule (fix round 4): a member whose stars are
    # clipped at 0.3 of its brightest peak above the sky, plus ONE hot pixel (50x the clip) on a candidate's
    # peak. Keyed on the brightest peak, the hot pixel raised the yardstick past every clipped star and
    # lightly clipped stars (a pixel or two at the clip, no flat top) were used; keyed on the third-brightest
    # it must be a refusal, or at least no clipped star may be used.
    clip_rng = np.random.default_rng(5)
    clip_ref = psf_image(size, centres, fluxes, "gauss", 4.0, clip_rng)
    clip_mem = psf_image(size, centres, fluxes / 1.25, "gauss", 4.0, clip_rng)
    level = 100.0 + 0.3 * (float(clip_mem.max()) - 100.0)
    was_clipped = clip_mem >= level
    n_clipped = sum(bool(was_clipped[round(cy) - 3:round(cy) + 4, round(cx) - 3:round(cx) + 4].any())
                    for cy, cx in centres)
    clip_mem = np.minimum(clip_mem, np.float32(level))
    c_info = reference_candidates(clip_ref)
    hy, hx = int(c_info[0][len(c_info[0]) // 2, 0]), int(c_info[0][len(c_info[0]) // 2, 1])
    clip_mem[hy, hx] = 100.0 + 50.0 * (level - 100.0)
    ph = photometry(clip_ref, clip_mem, c_info)
    used = sum(bool(was_clipped[y - 3:y + 4, x - 3:x + 4].any()) for y, x in ph["stars"])
    print(f"member clipped at 0.3 of its brightest peak ({n_clipped} of {len(centres)} stars clipped) plus one hot "
          f"pixel on the candidate at ({hy}, {hx}): ratio {ph['ratio']:.5f}, n {ph['n']}, clipped stars used {used}; "
          f"{ph['note'][:110]}")
    assert ph["ratio"] != ph["ratio"] or used == 0, (ph["ratio"], used)
    assert peak_yardstick(np.array([1.0, np.nan, 5.0, 1e9, 3.0])) == 3.0
    assert peak_yardstick(np.array([2.0, np.nan, 7.0])) == 7.0 and math.isnan(peak_yardstick(np.array([np.nan])))
    # A refusal prints its rejection counts per cut: too few stars on a crowded field.
    dense = [(margin + i * 35.0, margin + j * 35.0) for i in range(8) for j in range(8)]
    d_ref = psf_image(size, dense, rng.uniform(5e4, 3e5, len(dense)), "gauss", 4.0, rng)
    ph = photometry(d_ref, d_ref.copy())
    print(f"crowded field refusal: ratio {ph['ratio']}; note: {ph['note'][:230]}")
    assert ph["ratio"] != ph["ratio"] and "candidate isolation" in ph["note"], ph["note"]
    # OSC: three planes with three different scales through a (3, h, w) file.
    scales = (1.25, 1.6, 2.0)
    refs = [psf_image(size, centres, fluxes, "gauss", 4.0, rng) for _ in scales]
    mems = [psf_image(size, centres, fluxes / s, "gauss", 4.0, rng) for s in scales]
    with tempfile.TemporaryDirectory() as d:
        write_fits(Path(d) / "c.fits", np.stack(refs))
        write_fits(Path(d) / "d.fits", np.stack(mems))
        ra, rb = planes_of(str(Path(d) / "c.fits")), planes_of(str(Path(d) / "d.fits"))
    for p, s in enumerate(scales):
        ph = photometry(ra[p], rb[p])
        print(f"  osc plane {p}: ratio {ph['ratio']:.5f} truth {s:.5f}")
        assert abs(ph["ratio"] / s - 1) < 0.003, (p, ph["ratio"])
    # The events: the shapes tracing-subscriber's JSON layer writes for the three the report reads.
    plane = ('{"timestamp":"2026-10-03T10:11:12.345678Z","level":"DEBUG","fields":{"message":"ln relative scale",'
             '"ln_scale":0.8123,"sigma":0.004,"ln_matches":120,"rejected":3,"ln_pass":1,"ln_local_nodes":0,'
             '"ln_scale_source":"seeds"},"target":"athenaeum_core::stacking::ln::scale",'
             '"span":{"frame_id":42,"plane":1,"name":"ln_plane"},'
             '"spans":[{"name":"start_project_stacking"},{"frame_id":42,"plane":1,"name":"ln_plane"}]}')
    ev = parse_event(plane)
    assert ev == ("plane", datetime(2026, 10, 3, 10, 11, 12, 345678, tzinfo=timezone.utc), 42, 1, 0.8123, "seeds"), ev
    in_cal = plane.replace('"spans":[{"name":"start_project_stacking"},',
                           '"spans":[{"name":"ln_seeds_calibration"},')
    assert parse_event(in_cal) is None, "an event under ln_seeds_calibration must be skipped"
    assert parse_event(plane.replace('"ln relative scale"', '"something else"')) is None
    assert parse_event(plane.replace('"name":"ln_plane"', '"name":"other"')) is None
    frame = ('{"timestamp":"2026-10-03T10:11:13.000001Z","level":"DEBUG","fields":{"message":"ln frame normalized",'
             '"run_id":9,"frame_id":42,"ln_scale":0.8123,"ln_matches":120},"target":"athenaeum_core::stacking::run"}')
    assert parse_event(frame)[0:1] + parse_event(frame)[2:] == ("frame", 9, 42, 0.8123), parse_event(frame)
    crop = ('{"timestamp":"2026-10-03T10:20:00Z","level":"INFO","fields":{"message":"outputs cropped","run_id":9,'
            '"group_key":"L","crop_x0":10,"crop_y0":20,"crop_width":300,"crop_height":200,"path":"/x/m.fits"},'
            '"target":"athenaeum_core::stacking::run"}')
    assert parse_event(crop)[2:] == (9, "L", 10, 20, 300, 200, "outputs cropped"), parse_event(crop)
    assert parse_ts("2026-10-03T10:11:12.123456789+00:00") == datetime(2026, 10, 3, 10, 11, 12, 123456, tzinfo=timezone.utc)
    with tempfile.TemporaryDirectory() as d:
        (Path(d) / "athenaeum-web.2026-10-03").write_text(
            "\n".join([plane, in_cal.replace("0.8123", "0.5"), frame, frame.replace('"run_id":9', '"run_id":8'), crop, "not json"]) + "\n")
        got = read_log(d, parse_ts("2026-10-03T10:00:00Z"), parse_ts("2026-10-03T11:00:00Z"), 9)
        assert got["plane"] == {(42, 1): (0.8123, "seeds")} and got["frame"] == {42: 0.8123}, got
        assert got["crop"] == {"L": (10, 20, 300, 200, "outputs cropped")}, got
        got = read_log(d, parse_ts("2026-10-03T11:00:00Z"), parse_ts("2026-10-03T12:00:00Z"), 9)
        assert got["plane"] == {}, got
    print("grid-report self-check ok")


if __name__ == "__main__":
    main()
