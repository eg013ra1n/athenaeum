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
run cropped (`stacking_runs.summary_json` carries no rectangle: it is read from
the master's CRPIX1/2 against the uncropped grid centre (W+1)/2, (H+1)/2 — FITS
masters only) and the drizzle master's dims.

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
the `ln_scale` span (fields `frame_id` = the project-frame handle, `plane`) and
by the run's [started_at, finished_at] window; the applied scale is that event's
`ln_scale` times the group's seeds-calibration `k` for the plane when the event
came from the seeds path (`ln_scale_source`). --self-check runs a synthetic
check of the photometric ratio and the log parser, no real data.
"""
import json
import math
import os
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


def report_masters_dims(db, run_id, key, grid, summary):
    """R2: pixel dims of every master row of the group, the crop rectangle, the drizzle dims."""
    rows = db.execute(
        "SELECT kind, path, width, height, channels FROM master_lights WHERE run_id = ? AND group_key = ? ORDER BY kind",
        (run_id, key),
    ).fetchall()
    crop_k = ((summary.get("config") or {}).get("output") or {}).get("cropToCoverage", 0)
    gw, gh = grid["widthPx"], grid["heightPx"]
    for kind, path, w, h, ch in rows:
        print(f"  {kind}: {w} x {h} px, {ch} plane(s)  (grid {gw} x {gh})")
        if kind == "master":
            if not crop_k:
                print(f"  crop: cropToCoverage off; master {'equals' if (w, h) == (gw, gh) else 'DIFFERS FROM'} the grid")
                continue
            keys = fits_keys(path)
            if "CRPIX1" in keys and "CRPIX2" in keys:
                x0 = round((gw + 1) / 2.0 - float(keys["CRPIX1"]))
                y0 = round((gh + 1) / 2.0 - float(keys["CRPIX2"]))
                print(f"  crop (cropToCoverage = {crop_k}): x0 {x0} y0 {y0} width {w} height {h} "
                      f"(from the master's CRPIX1/2 {keys['CRPIX1']}/{keys['CRPIX2']}; summary_json carries no rectangle)")
            else:
                print(f"  crop (cropToCoverage = {crop_k}): {w} x {h} of the {gw} x {gh} grid; the rectangle's offset is not "
                      f"recoverable from a non-FITS master (no CRPIX read here)")
        elif kind == "drizzle":
            print(f"  drizzle master {w} x {h} px = {w / gw:.3f} x {h / gh:.3f} of the grid's own pixel count")
    if not any(r[0] == "drizzle" for r in rows):
        print("  drizzle master: none")


# --- LN truth (R1) ----------------------------------------------------------------


def parse_ts(text):
    """RFC 3339 -> aware datetime; any fraction length, Z or an offset."""
    m = re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(\.\d+)?(Z|[+-]\d\d:\d\d)?$", text)
    if not m:
        raise ValueError(f"unparseable timestamp {text!r}")
    base, frac, tz = m.groups()
    frac = (frac or ".0")[:7].ljust(7, "0")
    tz = "+00:00" if tz in (None, "Z") else tz
    return datetime.fromisoformat(f"{base}{frac}{tz}")


def parse_ln_line(line):
    """One JSONL line -> (timestamp, frame_id, plane, ln_scale, source) for an "ln relative scale"
    debug event inside an `ln_scale` span, else None. The span fields come from the innermost
    `ln_scale` entry of `spans` (the log layer writes the whole chain)."""
    try:
        o = json.loads(line)
    except ValueError:
        return None
    f = o.get("fields") or {}
    if f.get("message") != "ln relative scale" or "ln_scale" not in f:
        return None
    span = None
    for sp in reversed(o.get("spans") or [o.get("span") or {}]):
        if sp.get("name") == "ln_scale":
            span = sp
            break
    if span is None or "frame_id" not in span or "plane" not in span:
        return None
    return (parse_ts(o["timestamp"]), int(span["frame_id"]), int(span["plane"]),
            float(f["ln_scale"]), f.get("ln_scale_source", ""))


def read_ln_events(log_dir, t0, t1):
    """{(frame_id, plane): (ln_scale, source)} of the last matching event inside [t0, t1]."""
    out, seen = {}, 0
    for path in sorted(Path(log_dir).glob("*")):
        if not path.is_file():
            continue
        with open(path, errors="replace") as fh:
            for line in fh:
                if "ln relative scale" not in line:
                    continue
                ev = parse_ln_line(line)
                if ev is None or not (t0 <= ev[0] <= t1):
                    continue
                seen += 1
                out[(ev[1], ev[2])] = (ev[3], ev[4])
    return out, seen


def pick_stars(ref, want=60, edge=15, half=14):
    """Bright, isolated, unsaturated stars of one reference plane: (y, x) peak positions.
    Local maxima well above the background, the brightest in a (2*half+1)^2 window, at least
    `edge` px from the border, the whole window finite and nonzero (covered), and below the
    saturated end (the top 10 % of candidates and anything within 2 % of the image maximum)."""
    ok = np.isfinite(ref) & (ref != 0)
    vals = ref[ok]
    if vals.size == 0:
        return []
    bg = float(np.median(vals))
    sigma = 1.4826 * float(np.median(np.abs(vals - bg)))
    if sigma <= 0:
        return []
    hi = float(vals.max())
    h, w = ref.shape
    ys, xs = np.nonzero(np.where(ok, ref, bg) > bg + 20 * sigma)
    order = np.argsort(-ref[ys, xs])[:20000]
    cands = []
    taken = np.zeros(ref.shape, bool)
    for j in order:
        y, x = int(ys[j]), int(xs[j])
        if y < edge or x < edge or y >= h - edge or x >= w - edge or taken[y, x]:
            continue
        win = ref[y - half:y + half + 1, x - half:x + half + 1]
        if not np.isfinite(win).all() or (win == 0).any():
            continue
        if ref[y, x] < win.max():
            continue
        taken[y - half:y + half + 1, x - half:x + half + 1] = True
        cands.append((y, x, float(ref[y, x])))
    if not cands:
        return []
    cands = [c for c in cands if c[2] < 0.98 * hi]
    cands = cands[max(1, len(cands) // 10):] if len(cands) > 10 else cands
    return [(c[0], c[1]) for c in cands[:want]]


def aperture_fluxes(img, stars, r_ap=7.0, r_in=10.0, r_out=14.0):
    """Flux per star: sum of (pixel - annulus median) inside r_ap; NaN when the window holds a
    non-finite or zero (uncovered) pixel or the flux is not positive."""
    half = int(r_out)
    yy, xx = np.mgrid[-half:half + 1, -half:half + 1]
    rr = np.hypot(yy, xx)
    ap, ann = rr <= r_ap, (rr >= r_in) & (rr <= r_out)
    out = []
    for y, x in stars:
        win = img[y - half:y + half + 1, x - half:x + half + 1]
        if win.shape != rr.shape or not np.isfinite(win).all() or (win == 0).any():
            out.append(float("nan"))
            continue
        flux = float((win[ap] - np.median(win[ann])).sum())
        out.append(flux if flux > 0 else float("nan"))
    return np.array(out)


def photometric_ratio(ref, mem, stars=None):
    """(median of reference/member aperture flux, star count, scatter) for one plane."""
    stars = pick_stars(ref) if stars is None else stars
    fr, fm = aperture_fluxes(ref, stars), aperture_fluxes(mem, stars)
    good = np.isfinite(fr) & np.isfinite(fm)
    if good.sum() < 5:
        return float("nan"), int(good.sum()), float("nan")
    r = fr[good] / fm[good]
    med = float(np.median(r))
    return med, int(good.sum()), 1.4826 * float(np.median(np.abs(r - med)))


def planes_of(path):
    a = read(path)
    return [a] if a.ndim == 2 else [a[i] for i in range(a.shape[0])]


def ln_truth_group(db, work, pid, key, run_id, summary, window, events):
    ref = db.execute(
        "SELECT path FROM stacking_artifacts WHERE project_id = ? AND group_key = ? AND kind = 'ln_reference'",
        (pid, key),
    ).fetchone()
    if not ref or not ref[0]:
        print("  ln truth: no LN reference for this group")
        return []
    members = {}
    for handle, path in db.execute(
        "SELECT project_frame, path FROM stacking_artifacts WHERE project_id = ? AND group_key = ? "
        "AND kind = 'registered' AND project_frame IS NOT NULL ORDER BY id",
        (pid, key),
    ):
        members[handle] = path
    group = next((g for g in summary.get("groups", []) if g.get("key") == key), {})
    kcal = group.get("seedsCalibration") or []
    ref_planes = planes_of(ref[0])
    stars = [pick_stars(p) for p in ref_planes]
    print(f"  ln truth: reference stars per plane {[len(s) for s in stars]} (aperture r 7 px, annulus 10-14 px); "
          f"{len(members)} registered members; seeds calibration k {kcal or 'none'}")
    rows = []
    for handle in sorted(members):
        r = db.execute(
            "SELECT p.manifest_json FROM stack_project_frames s JOIN project_frames_local p "
            "ON p.project_id = s.project_id AND p.frame_uuid = s.frame_uuid WHERE s.id = ?",
            (handle,),
        ).fetchone()
        meta = (json.loads(r[0]).get("meta") or {}) if r else {}
        instr = meta.get("instrume") or meta.get("INSTRUME") or "unknown"
        mem_planes = planes_of(members[handle])
        for p, mp in enumerate(mem_planes):
            if p >= len(ref_planes):
                break
            phot, n, scat = photometric_ratio(ref_planes[p], mp, stars[p])
            ev = events.get((handle, p))
            if ev is None:
                print(f"    handle {handle} plane {p} [{instr}]: no ln event in the run window; photometric "
                      f"{phot:.5f} ({n} stars, scatter {scat:.4f})")
                continue
            raw, source = ev
            k = kcal[p] if source == "seeds" and p < len(kcal) else 1.0
            applied = raw * k
            diff = applied / phot - 1.0 if phot == phot and phot > 0 else float("nan")
            rows.append((instr, diff))
            print(f"    handle {handle} plane {p} [{instr}]: ln_scale {raw:.5f} x k {k:.5f} = {applied:.5f} ({source})  "
                  f"photometric {phot:.5f} ({n} stars, scatter {scat:.4f})  rel diff {diff * 100:+.3f} %")
    return rows


def ln_truth_summary(rows):
    by = {}
    for instr, d in rows:
        if d == d:
            by.setdefault(instr, []).append(abs(d))
    print("\n== ln truth: |ln_scale / photometric - 1| per instrument (no tolerance, no verdict)")
    for instr, v in sorted(by.items()):
        print(f"  {instr}: n {len(v)}  median {np.median(v) * 100:.3f} %  max {max(v) * 100:.3f} %")
    if not by:
        print("  no member with both a scale and a photometric ratio")


# --- the report ----------------------------------------------------------------------


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
            m = np.isfinite(plane0(path))
            fractions.append((float((m & ref_ok).sum()) / max(1, int(m.sum())), handle))
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
    events, truth_rows, window = {}, [], None
    if want_truth:
        if not summary.get("finishedAt"):
            sys.exit("the run's summary has no finishedAt: cannot window the log events")
        window = (parse_ts(summary["startedAt"]), parse_ts(summary["finishedAt"]))
        events, seen = read_ln_events(work / "logs", *window)
        print(f"ln truth: {seen} 'ln relative scale' events in {work / 'logs'} inside the run window "
              f"{window[0].isoformat()} .. {window[1].isoformat()} ({len(events)} distinct frame/plane)")
        if not events:
            print("  none found: was the run made at debug (project-run.sh sets ATHENAEUM_LOG), and does the build carry the ln_scale span?")
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
        report_masters_dims(db, run_id, key, grid, summary)
        if want_truth:
            truth_rows += ln_truth_group(db, work, pid, key, run_id, summary, window, events)
    if want_truth:
        ln_truth_summary(truth_rows)


# --- self-check (no real data) ------------------------------------------------------------


def synthetic_frame(rng, size, stars, amp_scale=1.0, planes=1, extra=0):
    yy, xx = np.mgrid[0:size, 0:size].astype(np.float64)
    img = np.full((size, size), 100.0)
    for (y, x, a) in stars:
        img += a * np.exp(-((yy - y) ** 2 + (xx - x) ** 2) / (2 * 2.0 ** 2))
    for _ in range(extra):  # stars only this frame has: must not move the ratio
        y, x = rng.uniform(20, size - 20, 2)
        img += 900.0 * np.exp(-((yy - y) ** 2 + (xx - x) ** 2) / (2 * 2.0 ** 2))
    img += rng.normal(0, 1.0, img.shape)
    return [(img - 100.0) * amp_scale + 100.0 for _ in range(planes)]


def self_check():
    rng = np.random.default_rng(7)
    size = 240
    stars = [(float(rng.integers(20, size - 20)), float(rng.integers(20, size - 20)), float(rng.uniform(300, 3000)))
             for _ in range(70)]
    ref = synthetic_frame(rng, size, stars)[0]
    # B = 0.8 * A + a few stars of its own: reference / member = 1.25.
    base = synthetic_frame(rng, size, stars, 0.8, extra=5)[0]
    with tempfile.TemporaryDirectory() as d:
        write_fits(Path(d) / "a.fits", ref)
        write_fits(Path(d) / "b.fits", base)
        a, b = plane0(str(Path(d) / "a.fits")), plane0(str(Path(d) / "b.fits"))
        ratio, n, scatter = photometric_ratio(a, b)
        print(f"photometric ratio {ratio:.5f} from {n} stars (scatter {scatter:.5f}); truth 1.25000")
        assert n >= 20, n
        assert abs(ratio / 1.25 - 1) < 0.01, ratio
        # OSC: three planes with three different scales, a (3, h, w) file.
        planes = [synthetic_frame(rng, size, stars, s)[0] for s in (0.9, 0.8, 0.5)]
        write_fits(Path(d) / "c.fits", np.stack([ref] * 3))
        write_fits(Path(d) / "d.fits", np.stack(planes))
        ra, rb = planes_of(str(Path(d) / "c.fits")), planes_of(str(Path(d) / "d.fits"))
        for p, s in enumerate((0.9, 0.8, 0.5)):
            r, n, _ = photometric_ratio(ra[p], rb[p])
            print(f"  osc plane {p}: ratio {r:.5f} truth {1 / s:.5f}")
            assert abs(r * s - 1) < 0.01, (p, r)
    # The real event shape: tracing-subscriber's JSON layer, the `ln_scale` span of normalize_frame.
    line = ('{"timestamp":"2026-10-03T10:11:12.345678Z","level":"DEBUG","fields":{"message":"ln relative scale",'
            '"ln_scale":0.8123,"sigma":0.004,"ln_matches":120,"rejected":3,"ln_pass":1,"ln_local_nodes":0,'
            '"ln_scale_source":"seeds"},"target":"athenaeum_core::stacking::ln::scale",'
            '"span":{"frame_id":42,"plane":1,"name":"ln_scale"},'
            '"spans":[{"name":"start_project_stacking"},{"frame_id":42,"plane":1,"name":"ln_scale"}]}')
    ev = parse_ln_line(line)
    assert ev is not None
    ts, frame, plane, scale, source = ev
    assert (frame, plane, scale, source) == (42, 1, 0.8123, "seeds"), ev
    assert ts == datetime(2026, 10, 3, 10, 11, 12, 345678, tzinfo=timezone.utc), ts
    assert parse_ln_line(line.replace('"ln relative scale"', '"something else"')) is None
    assert parse_ln_line(line.replace('"name":"ln_scale"', '"name":"other"')) is None
    assert parse_ts("2026-10-03T10:11:12.123456789+00:00") == datetime(2026, 10, 3, 10, 11, 12, 123456, tzinfo=timezone.utc)
    with tempfile.TemporaryDirectory() as d:
        (Path(d) / "athenaeum-web.2026-10-03").write_text(line + "\n" + "not json\n")
        got, seen = read_ln_events(d, parse_ts("2026-10-03T10:00:00Z"), parse_ts("2026-10-03T11:00:00Z"))
        assert got == {(42, 1): (0.8123, "seeds")} and seen == 1, (got, seen)
        got, _ = read_ln_events(d, parse_ts("2026-10-03T11:00:00Z"), parse_ts("2026-10-03T12:00:00Z"))
        assert got == {}
    print("grid-report self-check ok")


if __name__ == "__main__":
    main()
