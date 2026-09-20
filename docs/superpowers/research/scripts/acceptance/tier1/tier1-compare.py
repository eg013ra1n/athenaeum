#!/usr/bin/env python3
"""Byte-compare two acceptance runs' outputs: masters (raw, then header card SET + data bytes),
registration_results rows, .athln sidecars. usage: tier1-compare.py <W_base> <W_head>

--numeric mode (Tier C, spec `docs/superpowers/specs/2026-09-20-stacking-compute-tierC-design.md`
§8): Tier C changes outputs on purpose, so it needs a DELTA report against the spec's own
tolerance table instead of byte identity.

    tier1-compare.py --numeric <W_head> <W_base> [--set-id N]

Note the arg order flips vs the identity mode above: numeric ratios are always head/base, so the
positional order matches that (identity mode's <base> <head> order is unchanged for backward
compatibility with `checkpoint.sh`'s existing default call). `--set-id` defaults to 204
(`LDN1272-test`, the Tier A/Tier C reduced set); it selects which `stacking_runs` row (the latest
`status='done'` for that `frames_set_id`) each side's DB numbers come from.

What's compared and where every number is read from — the spec §8 table, verbatim:

    | Metric                                              | Tolerance                              |
    | ---------------------------------------------------- | --------------------------------------- |
    | Master pixel median / MAD vs tierA-baseline           | +/- 0.1 % / +/- 1 %                     |
    | Master noise (MRS sigma) vs baseline                  | +/- 2 %                                 |
    | FWHM (mono, OSC per plane) vs baseline                | +/- 1 %                                 |
    | Rejected fraction (linear fit) vs baseline            | +/- 0.3 pp (baseline 2.985 / 2.733 %)   |
    | Per-frame weights: Spearman rho vs baseline           | >= 0.99; top-20 overlap >= 18/20        |
    | LN relative scale `s` per frame vs baseline           | median ratio 1 +/- 0.5 %, scatter <= 1 %|
    | Drizzled master level vs undrizzled (R-M3-2)          | 0.998-1.002 (baseline 0.9987-0.99999)   |
    | Drizzle coverage                                      | 1.0 on every plane                      |
    | Wall (interleaved with a fresh baseline re-run, R-TA-9)| reported, target <= 16 min             |

- Master median / MAD / noise (MRS sigma) / FWHM: one `measure_probe <master.fits|.xisf>` run per
  side per master file (mono and every OSC plane in one call — `measure_probe` prints a
  `FrameMeasurement.channels` array for FITS, or a bare `ChannelMeasurement` array for XISF; see
  `crates/athenaeum-core/examples/measure_probe.rs`). `ChannelMeasurement.median`/`.mad` are a
  stratified-and-clipped sample of the WHOLE plane (`stacking::measure::measure_plane`, not a
  background-only estimate) — the same field every earlier acceptance doc's "master median/MAD"
  line reads (M4a/M4c). `.noise` is the MRS estimate (`psf_signal::noise_mrs`,
  `NoiseSource::Mrs`), `.fwhmPx` is `psf_signal::frame_shape`'s PSF FWHM. Master files are found by
  globbing `<W>/stackout/*.fits` and `*.xisf` directly (never `stacking_run_groups.master_path` —
  that column can go stale relative to a checkpoint's own directory, see the Task 0 report) and
  paired base/plain vs `_drizzleNx` by filename.
- Rejected fraction (linear fit): `stacking_run_groups.stats_json`, decoded as `GroupStats`
  (`crates/athenaeum-core/src/stacking/integrate.rs`, `#[serde(rename_all = "camelCase")]`) —
  `(rejectedLowFraction + rejectedHighFraction) * 100`, the same sum
  `src/components/stacking/ResultsPanel.tsx` renders as "N% rejected". Per group (`group_key`).
  `#[cfg(test)]`
  aside: this is a config-hash-per-stage cache key elsewhere in the pipeline, not here — here it's
  just a stored result column, keyed by (run_id, group_key).
- Per-frame weights: `stacking_run_frames.weight` (a plain REAL column — no summary_json digging
  needed), joined across runs by `frame_id` — both checkpoints are copies of the SAME template
  catalog, so `frames.id` is stable between them (checkpoint.sh's `cp -R` + a `settings` path
  substitution, never a re-scan). Spearman rho (rank correlation, stdlib, no numpy needed for
  this) + top-20-by-weight overlap over the intersection of frame ids with a non-null weight on
  both sides, restricted to `included = 1`.
- LN relative scale: the `.athln` sidecar's per-channel `A` grid mean (`stacking::ln::grid::
  LnGrid`; `A` is constant across the grid — equal to `global_scale`, the PSF-flux relative scale
  — unless `normalization.local.localScale` is on, hence "the grid's mean" rather than assuming
  the constant). Read directly from the binary format documented in
  `crates/athenaeum-core/src/stacking/ln/grid.rs` (`LnFrameGrids::encode`/`read_inner`) with
  Python's `struct` module — no Rust runtime, no xxh3 (the trailer checksum is not verified here;
  a corrupt sidecar fails to unpack and is skipped with a warning, same as a missing one). Per
  frame: the mean of each channel's `A` grid, averaged over channels. Matched between head and
  base by path relative to `<W>/stackwork/` (the same relative path a Tier A identity-mode
  `.athln` byte-compare already uses).
- Drizzle level / coverage: level = `median(drizzled plane) / median(plain plane)` from the SAME
  `measure_probe` runs used for row 1-3 (no extra I/O) — a property of the HEAD run alone
  (R-M3-2's level-preservation contract), the base's own value is shown for reference, not
  compared against. Coverage = fraction of pixels > 0 in the drizzle's `..._weight.fits` sibling
  (`imgcmp.py`'s FITS/XISF reader, `numpy`) when `drizzle.writeWeightMap` was on; `n/a` otherwise
  (neither Tier A checkpoint's config turns it on — no weight-map file exists to read).
- Wall: `run.txt`'s `wall_s=<seconds>` line, reported only (no PASS/FAIL — the ≤ 16 min target is
  Tier C's own acceptance run, not this pin, which reuses two Tier-A-identical checkpoints).

Numeric mode's own pin: `--numeric tierA-tierA tierA-baseline` must read ratio 1.000 / identical /
PASS on every row (the two checkpoints are the SAME code run twice — Tier A's own byte-identity
pin already proved every file below matches bit for bit, so every ratio here is an exact 1.0, not
merely within tolerance). Exits 0 on `NUMERIC: PASS`, 1 on `NUMERIC: FAIL`.
"""
import glob
import hashlib
import json
import os
import re
import sqlite3
import statistics
import struct
import subprocess
import sys
from pathlib import Path


def fits_split(path):
    data = open(path, "rb").read()
    cards = set()
    off = 0
    while True:
        block = data[off : off + 2880]
        off += 2880
        done = False
        for i in range(0, 2880, 80):
            c = block[i : i + 80].decode("latin1")
            if c.startswith("END") and c.strip() == "END":
                done = True
                break
            if c.strip():
                cards.add(c)
        if done or off >= len(data):
            break
    return cards, hashlib.blake2b(data[off:]).hexdigest(), len(data) - off


def run_identity(A, B):
    ok = True
    print("== masters ==")
    for pa in sorted(glob.glob(os.path.join(A, "stackout", "*.fits"))):
        pb = os.path.join(B, "stackout", os.path.basename(pa))
        if not os.path.exists(pb):
            print(f"  MISSING in head: {os.path.basename(pa)}")
            ok = False
            continue
        raw = open(pa, "rb").read() == open(pb, "rb").read()
        ca, da, na = fits_split(pa)
        cb, db_, nb = fits_split(pb)
        print(
            f"  {os.path.basename(pa)}: raw_identical={raw} data_identical={da==db_} ({na} B) "
            f"cards_equal={ca==cb} (+{len(cb-ca)} -{len(ca-cb)})"
        )
        if da != db_:
            ok = False
        if ca != cb:
            for c in sorted(ca - cb)[:5]:
                print("     base-only:", c.strip())
            for c in sorted(cb - ca)[:5]:
                print("     head-only:", c.strip())
    print("== registration_results ==")

    def reg(w):
        db = sqlite3.connect(os.path.join(w, "athenaeum.db"))
        return db.execute(
            "select frame_id, transform_json from registration_results order by frame_id"
        ).fetchall()

    ra, rb = reg(A), reg(B)
    diff = [(x[0]) for x, y in zip(ra, rb) if x != y]
    print(f"  rows base={len(ra)} head={len(rb)} differing={len(diff)} {diff[:10]}")
    if len(ra) != len(rb) or diff:
        ok = False
    print("== .athln sidecars ==")
    sa = sorted(glob.glob(os.path.join(A, "stackwork", "**", "*.athln"), recursive=True))
    nd = 0
    missing = 0
    for pa in sa:
        pb = pa.replace(A, B, 1)
        if not os.path.exists(pb):
            missing += 1
            continue
        if open(pa, "rb").read() != open(pb, "rb").read():
            nd += 1
    print(f"  sidecars base={len(sa)} missing_in_head={missing} differing={nd}")
    if missing or nd:
        ok = False
    print("== calibrated frames (pixel data + card set) ==")
    ca_ = sorted(
        glob.glob(os.path.join(A, "stackwork", "**", "calibrated", "**", "*.fits"), recursive=True)
    )
    ndat = 0
    ncard = 0
    miss = 0
    for pa in ca_:
        pb = pa.replace(A, B, 1)
        if not os.path.exists(pb):
            miss += 1
            continue
        ca, da, _ = fits_split(pa)
        cb, db_, _ = fits_split(pb)
        if da != db_:
            ndat += 1
        if ca != cb:
            ncard += 1
    print(f"  calibrated base={len(ca_)} missing_in_head={miss} data_differing={ndat} cardset_differing={ncard}")
    if miss or ndat:
        ok = False
    print("RESULT:", "IDENTICAL" if ok else "DIFFERENCES FOUND")
    return 0


# ---------------------------------------------------------------------------
# --numeric mode (Tier C, spec §8)
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parents[6]
MEASURE_PROBE = REPO_ROOT / ".superpowers" / "target-acc" / "release" / "examples" / "measure_probe"

# imgcmp.py (one directory up, `acceptance/`) already has a numpy FITS/monolithic-XISF float32
# reader — reused here only for the drizzle weight-map coverage check (row 8), the one number
# `measure_probe` doesn't already report.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

# Spec §8's tolerance table, copied verbatim (see the module docstring for the full text with
# where each number comes from).
TOLERANCE_TABLE = [
    ("Master pixel median / MAD vs tierA-baseline", "+/- 0.1 % / +/- 1 %"),
    ("Master noise (MRS sigma) vs baseline", "+/- 2 %"),
    ("FWHM (mono, OSC per plane) vs baseline", "+/- 1 %"),
    ("Rejected fraction (linear fit) vs baseline", "+/- 0.3 pp (baseline 2.985 / 2.733 %)"),
    ("Per-frame weights: Spearman rho vs baseline", ">= 0.99; top-20 overlap >= 18/20"),
    ("LN relative scale s per frame vs baseline", "median ratio 1 +/- 0.5 %, scatter <= 1 %"),
    ("Drizzled master level vs undrizzled (R-M3-2)", "0.998-1.002 (baseline 0.9987-0.99999)"),
    ("Drizzle coverage", "1.0 on every plane"),
    ("Wall (interleaved with a fresh baseline re-run, R-TA-9)", "reported, target <= 16 min"),
]

MEDIAN_TOL = 0.001  # +/- 0.1 %
MAD_TOL = 0.01  # +/- 1 %
NOISE_TOL = 0.02  # +/- 2 %
FWHM_TOL = 0.01  # +/- 1 %
REJECTED_FRACTION_TOL_PP = 0.3
WEIGHTS_RHO_MIN = 0.99
WEIGHTS_TOP20_MIN = 18
LN_MEDIAN_TOL = 0.005  # +/- 0.5 %
LN_SCATTER_TOL = 0.01  # <= 1 %
DRIZZLE_LEVEL_LO = 0.998
DRIZZLE_LEVEL_HI = 1.002
DRIZZLE_COVERAGE_MIN = 0.999995  # "1.0" with float slack for a fraction-of-pixels count


def die(msg):
    print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(2)


def run_id_for_set(db_path, set_id):
    if not os.path.exists(db_path):
        die(f"no athenaeum.db at {db_path}")
    conn = sqlite3.connect(db_path)
    row = conn.execute(
        "SELECT id FROM stacking_runs WHERE frames_set_id = ? AND status = 'done' ORDER BY id DESC LIMIT 1",
        (set_id,),
    ).fetchone()
    conn.close()
    if not row:
        die(f"no done stacking run for set {set_id} in {db_path}")
    return row[0]


def load_groups(db_path, run_id):
    """group_key -> {colorMode, stats (decoded GroupStats dict, camelCase)}."""
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT group_key, color_mode, stats_json FROM stacking_run_groups WHERE run_id = ?",
        (run_id,),
    ).fetchall()
    conn.close()
    out = {}
    for group_key, color_mode, stats_json in rows:
        stats = json.loads(stats_json) if stats_json else None
        out[group_key] = {"colorMode": color_mode, "stats": stats}
    return out


def load_frame_weights(db_path, run_id):
    """frame_id -> weight, for included frames with a non-null weight."""
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT frame_id, weight FROM stacking_run_frames WHERE run_id = ? AND included = 1 AND weight IS NOT NULL",
        (run_id,),
    ).fetchall()
    conn.close()
    return {fid: w for fid, w in rows}


def wall_seconds(work_dir):
    p = os.path.join(work_dir, "run.txt")
    if not os.path.exists(p):
        return None
    m = re.search(r"wall_s=(\d+)", open(p).read())
    return int(m.group(1)) if m else None


# --- measure_probe -----------------------------------------------------------

_measure_cache = {}


def measure_master(path):
    """-> list of ChannelMeasurement dicts (camelCase keys: median, mad, noise, fwhmPx, ...),
    one per plane. Runs `measure_probe` once per distinct real path (cached)."""
    key = os.path.realpath(path)
    if key in _measure_cache:
        return _measure_cache[key]
    if not MEASURE_PROBE.exists():
        die(
            f"measure_probe not found at {MEASURE_PROBE} — build it first "
            "(cargo build --release -p athenaeum-core --example measure_probe, "
            "target-dir .superpowers/target-acc)"
        )
    proc = subprocess.run(
        [str(MEASURE_PROBE), path], capture_output=True, text=True, timeout=120
    )
    if proc.returncode != 0:
        raise RuntimeError(f"measure_probe failed on {path}: {proc.stderr.strip()[-500:]}")
    data = json.loads(proc.stdout)
    channels = data if isinstance(data, list) else data["channels"]
    _measure_cache[key] = channels
    return channels


# --- master file discovery ---------------------------------------------------

DRIZZLE_RE = re.compile(r"^(?P<base>.+)_drizzle(?P<scale>\d+)x$")


def classify_masters(stackout_dir):
    """-> (plain: {base_key: path}, drizzle: {base_key: {"path", "scale"}},
    weight: {base_key: path}) from a flat `stackout/*.fits|*.xisf` listing. Never trusts
    `stacking_run_groups.master_path` — see the module docstring."""
    plain, drizzle, weight = {}, {}, {}
    for ext in ("fits", "xisf"):
        for p in sorted(glob.glob(os.path.join(stackout_dir, f"*.{ext}"))):
            stem = os.path.basename(p)[: -(len(ext) + 1)]
            if stem.endswith("_weight"):
                base_stem = stem[: -len("_weight")]
                dm = DRIZZLE_RE.match(base_stem)
                if dm:
                    weight[dm.group("base")] = p
                continue
            dm = DRIZZLE_RE.match(stem)
            if dm:
                drizzle[dm.group("base")] = {"path": p, "scale": int(dm.group("scale"))}
            else:
                plain[stem] = p
    return plain, drizzle, weight


# --- .athln (LN relative scale) ----------------------------------------------

ATHLN_MAGIC = b"ATHLN\0\0\0"


def read_athln_channel_means(path):
    """-> list of per-channel `A`-grid means (`crate::stacking::ln::grid::LnGrid`'s on-disk
    format, `LnFrameGrids::encode`/`read_inner`): magic(8) + version/ref_w/ref_h/scale/nchan
    (5x u32 LE) + per channel [gw, gh (u32 LE) + global_scale/location_ref/location_tgt/
    relative_scale (4x f64 LE) + a[gw*gh] + b[gw*gh] (f32 LE)] + an 8-byte xxh3 trailer (not
    verified here — a truncated/corrupt file simply fails to unpack and is skipped by the
    caller with a warning, same treatment as a missing sidecar)."""
    data = open(path, "rb").read()
    if data[:8] != ATHLN_MAGIC:
        raise ValueError("bad magic")
    _version, _ref_w, _ref_h, _scale, nchan = struct.unpack_from("<5I", data, 8)
    off = 28
    means = []
    for _ in range(nchan):
        gw, gh = struct.unpack_from("<2I", data, off)
        off += 8
        off += 32  # global_scale, location_ref, location_tgt, relative_scale (4x f64)
        n = gw * gh
        if n <= 0:
            raise ValueError(f"bad grid dims {gw}x{gh}")
        a = struct.unpack_from(f"<{n}f", data, off)
        off += n * 4
        off += n * 4  # skip the b[] array — not needed for the relative scale
        means.append(sum(a) / n)
    return means


def collect_athln_scales(work_dir):
    """relative-path-under-stackwork -> mean-over-channels LN relative scale."""
    out = {}
    for p in glob.glob(os.path.join(work_dir, "stackwork", "**", "*.athln"), recursive=True):
        rel = os.path.relpath(p, os.path.join(work_dir, "stackwork"))
        try:
            means = read_athln_channel_means(p)
            out[rel] = sum(means) / len(means)
        except Exception as e:  # noqa: BLE001 - a corrupt/short sidecar is a skip, not a crash
            print(f"  warn: could not read {p}: {e}", file=sys.stderr)
    return out


# --- stats helpers (stdlib only) ---------------------------------------------


def mad(values):
    if not values:
        return 0.0
    m = statistics.median(values)
    return statistics.median([abs(v - m) for v in values])


def _rank(values):
    order = sorted(range(len(values)), key=lambda i: values[i])
    ranks = [0.0] * len(values)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and values[order[j + 1]] == values[order[i]]:
            j += 1
        avg_rank = (i + j) / 2.0 + 1
        for k in range(i, j + 1):
            ranks[order[k]] = avg_rank
        i = j + 1
    return ranks


def spearman(xs, ys):
    n = len(xs)
    if n < 3:
        return None
    rx, ry = _rank(xs), _rank(ys)
    mx, my = statistics.mean(rx), statistics.mean(ry)
    num = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    denx = sum((a - mx) ** 2 for a in rx) ** 0.5
    deny = sum((b - my) ** 2 for b in ry) ** 0.5
    if denx == 0 or deny == 0:
        return None
    return num / (denx * deny)


def verdict(ok):
    return "PASS" if ok else "FAIL"


def pct(ratio):
    return (ratio - 1.0) * 100.0


# --- report sections ----------------------------------------------------------


def report_masters(head_dir, base_dir, gate):
    """Rows 1-3 (median/MAD/noise/FWHM) and the head side of rows 7-8 (drizzle level/coverage).
    Returns {base_key: {"plain": [...channels...], "drizzle": [...] or None}} for HEAD, used by
    the caller for nothing further — everything gating happens here, inline."""
    hp, hd, hw = classify_masters(os.path.join(head_dir, "stackout"))
    bp, bd, bw = classify_masters(os.path.join(base_dir, "stackout"))
    keys = sorted(set(hp) & set(bp))
    missing_head = sorted(set(bp) - set(hp))
    missing_base = sorted(set(hp) - set(bp))
    print("-- masters (median / MAD / noise / FWHM per plane) --")
    if missing_head:
        print(f"  MISSING in head: {missing_head}")
        gate["median_mad"].append(False)
    if missing_base:
        print(f"  MISSING in base (new in head — nothing to compare against): {missing_base}")
    for key in keys:
        h_path, b_path = hp[key], bp[key]
        h_ch, b_ch = measure_master(h_path), measure_master(b_path)
        print(f"  {key}")
        if len(h_ch) != len(b_ch):
            print(
                f"    FAIL channel count differs: head={len(h_ch)} base={len(b_ch)} "
                f"(head={os.path.basename(h_path)} base={os.path.basename(b_path)})"
            )
            gate["median_mad"].append(False)
            gate["noise"].append(False)
            gate["fwhm"].append(False)
            continue
        for i, (hc, bc) in enumerate(zip(h_ch, b_ch)):
            m_ratio = hc["median"] / bc["median"] if bc["median"] else float("nan")
            d_ratio = hc["mad"] / bc["mad"] if bc["mad"] else float("nan")
            n_ratio = hc["noise"] / bc["noise"] if bc["noise"] else float("nan")
            f_ratio = hc["fwhmPx"] / bc["fwhmPx"] if bc["fwhmPx"] else float("nan")
            m_ok = abs(pct(m_ratio)) <= MEDIAN_TOL * 100
            d_ok = abs(pct(d_ratio)) <= MAD_TOL * 100
            n_ok = abs(pct(n_ratio)) <= NOISE_TOL * 100
            f_ok = abs(pct(f_ratio)) <= FWHM_TOL * 100
            gate["median_mad"].append(m_ok and d_ok)
            gate["noise"].append(n_ok)
            gate["fwhm"].append(f_ok)
            print(
                f"    plane {i}: median ratio={m_ratio:.6f} ({pct(m_ratio):+.4f}%) {verdict(m_ok)}  "
                f"mad ratio={d_ratio:.6f} ({pct(d_ratio):+.4f}%) {verdict(d_ok)}"
            )
            print(
                f"             noise  ratio={n_ratio:.6f} ({pct(n_ratio):+.4f}%) {verdict(n_ok)}  "
                f"fwhmPx ratio={f_ratio:.6f} ({pct(f_ratio):+.4f}%) {verdict(f_ok)}"
            )
        # Drizzle level (row 7, a property of HEAD alone) + coverage (row 8).
        if key in hd:
            hd_ch = measure_master(hd[key]["path"])
            if len(hd_ch) != len(h_ch):
                print(
                    f"    drizzle FAIL: plain/drizzle channel count differs "
                    f"({len(h_ch)} vs {len(hd_ch)})"
                )
                gate["drizzle_level"].append(False)
            else:
                if key in bd:
                    bd_ch = measure_master(bd[key]["path"])
                    if len(bd_ch) == len(b_ch):
                        b_levels = [
                            (dc["median"] / pc["median"]) if pc["median"] else float("nan")
                            for pc, dc in zip(b_ch, bd_ch)
                        ]
                        print(
                            "    drizzle level, base (reference only, not gated): "
                            + ", ".join(f"{v:.6f}" for v in b_levels)
                        )
                for i, (pc, dc) in enumerate(zip(h_ch, hd_ch)):
                    level = (dc["median"] / pc["median"]) if pc["median"] else float("nan")
                    ok = DRIZZLE_LEVEL_LO <= level <= DRIZZLE_LEVEL_HI
                    gate["drizzle_level"].append(ok)
                    print(
                        f"    drizzle{hd[key]['scale']}x plane {i}: level={level:.6f} "
                        f"(tol {DRIZZLE_LEVEL_LO}-{DRIZZLE_LEVEL_HI}) {verdict(ok)}"
                    )
            if key in hw:
                cov = weight_map_coverage(hw[key])
                if cov is None:
                    print("    drizzle coverage: n/a (weight map unreadable)")
                else:
                    for i, c in enumerate(cov):
                        ok = c >= DRIZZLE_COVERAGE_MIN
                        gate["drizzle_coverage"].append(ok)
                        print(f"    drizzle coverage plane {i}: {c:.6f} {verdict(ok)}")
            else:
                print("    drizzle coverage: n/a (no ..._weight.fits — writeWeightMap is off)")
        else:
            print("    drizzle: n/a (no drizzle master for this key on head)")


def weight_map_coverage(path):
    try:
        import imgcmp  # noqa: PLC0415 - only needed for this one check

        arr = imgcmp.read(path)
    except Exception as e:  # noqa: BLE001
        print(f"  warn: could not read weight map {path}: {e}", file=sys.stderr)
        return None
    import numpy as np  # noqa: PLC0415

    if arr.ndim == 2:
        return [float((arr > 0).mean())]
    return [float((arr[c] > 0).mean()) for c in range(arr.shape[0])]


def report_rejected_fraction(head_groups, base_groups, gate):
    print("-- rejected fraction (linear fit): (rejectedLowFraction + rejectedHighFraction) * 100 --")
    keys = sorted(set(head_groups) & set(base_groups))
    for gk in keys:
        hs = head_groups[gk]["stats"]
        bs = base_groups[gk]["stats"]
        if not hs or not bs:
            print(f"  {gk}: n/a (no stats_json on one side)")
            continue
        h_pct = (hs["rejectedLowFraction"] + hs["rejectedHighFraction"]) * 100.0
        b_pct = (bs["rejectedLowFraction"] + bs["rejectedHighFraction"]) * 100.0
        delta = h_pct - b_pct
        ok = abs(delta) <= REJECTED_FRACTION_TOL_PP
        gate["rejected_fraction"].append(ok)
        print(f"  {gk}: head={h_pct:.4f}% base={b_pct:.4f}% delta={delta:+.4f}pp {verdict(ok)}")


def report_weights(head_dir, base_dir, head_run, base_run, head_groups, base_groups, gate):
    print("-- per-frame weights (stacking_run_frames.weight, joined by frame_id) --")
    hw = load_frame_weights(os.path.join(head_dir, "athenaeum.db"), head_run)
    bw = load_frame_weights(os.path.join(base_dir, "athenaeum.db"), base_run)
    common = sorted(set(hw) & set(bw))
    if len(common) < 3:
        print(f"  n/a: only {len(common)} frame(s) in common (need >= 3 for Spearman)")
        return
    xs = [hw[f] for f in common]
    ys = [bw[f] for f in common]
    rho = spearman(xs, ys)
    n_top = min(20, len(common))
    top_h = set(sorted(common, key=lambda f: -hw[f])[:n_top])
    top_b = set(sorted(common, key=lambda f: -bw[f])[:n_top])
    overlap = len(top_h & top_b)
    rho_ok = rho is not None and rho >= WEIGHTS_RHO_MIN
    overlap_ok = n_top == 20 and overlap >= WEIGHTS_TOP20_MIN
    gate["weights"].append(rho_ok)
    if n_top == 20:
        gate["weights"].append(overlap_ok)
    print(
        f"  overall n={len(common)} rho={rho if rho is not None else 'n/a'} "
        f"(>= {WEIGHTS_RHO_MIN}) {verdict(rho_ok)}  top{n_top} overlap={overlap}/{n_top} "
        f"(>= {WEIGHTS_TOP20_MIN}/20) {'PASS' if overlap_ok else ('FAIL' if n_top == 20 else 'n/a')}"
    )
    # Per-group breakdown (informational — the spec's own row is one number, this is extra
    # visibility, not a second gate).
    for gk in sorted(set(head_groups) & set(base_groups)):
        conn = sqlite3.connect(os.path.join(head_dir, "athenaeum.db"))
        gid_row = conn.execute(
            "SELECT id FROM stacking_run_groups WHERE run_id = ? AND group_key = ?",
            (head_run, gk),
        ).fetchone()
        conn.close()
        if not gid_row:
            continue
        conn = sqlite3.connect(os.path.join(head_dir, "athenaeum.db"))
        fids = {
            r[0]
            for r in conn.execute(
                "SELECT frame_id FROM stacking_run_frames WHERE run_id = ? AND group_id = ?",
                (head_run, gid_row[0]),
            ).fetchall()
        }
        conn.close()
        g_common = [f for f in common if f in fids]
        if len(g_common) < 3:
            continue
        g_rho = spearman([hw[f] for f in g_common], [bw[f] for f in g_common])
        print(f"    {gk}: n={len(g_common)} rho={g_rho}")


def report_ln_scale(head_dir, base_dir, gate):
    print("-- LN relative scale (.athln, A-grid mean, averaged over channels) --")
    h = collect_athln_scales(head_dir)
    b = collect_athln_scales(base_dir)
    common = sorted(set(h) & set(b))
    if not common:
        print("  n/a: no shared .athln sidecars")
        return
    ratios = [h[k] / b[k] if b[k] else float("nan") for k in common]
    ratios = [r for r in ratios if r == r]  # drop NaN
    if not ratios:
        print("  n/a: every ratio was NaN (a zero base scale)")
        return
    med = statistics.median(ratios)
    scatter = mad(ratios)
    med_ok = abs(pct(med)) <= LN_MEDIAN_TOL * 100
    scatter_ok = scatter * 100 <= LN_SCATTER_TOL * 100
    gate["ln_scale"].append(med_ok)
    gate["ln_scale"].append(scatter_ok)
    print(
        f"  n={len(ratios)} median_ratio={med:.6f} ({pct(med):+.4f}%, tol +/-{LN_MEDIAN_TOL*100:.1f}%) "
        f"{verdict(med_ok)}   scatter(MAD of ratio)={scatter*100:.4f}% "
        f"(tol <= {LN_SCATTER_TOL*100:.1f}%) {verdict(scatter_ok)}"
    )


def run_numeric(head_dir, base_dir, set_id):
    print(f"=== Tier C numeric acceptance: HEAD vs BASE (set {set_id}) ===")
    print(f"head: {head_dir}")
    print(f"base: {base_dir}")
    head_run = run_id_for_set(os.path.join(head_dir, "athenaeum.db"), set_id)
    base_run = run_id_for_set(os.path.join(base_dir, "athenaeum.db"), set_id)
    print(f"head run_id={head_run}  base run_id={base_run}")
    head_groups = load_groups(os.path.join(head_dir, "athenaeum.db"), head_run)
    base_groups = load_groups(os.path.join(base_dir, "athenaeum.db"), base_run)
    print()

    gate = {
        "median_mad": [],
        "noise": [],
        "fwhm": [],
        "rejected_fraction": [],
        "weights": [],
        "ln_scale": [],
        "drizzle_level": [],
        "drizzle_coverage": [],
    }

    report_masters(head_dir, base_dir, gate)
    print()
    report_rejected_fraction(head_groups, base_groups, gate)
    print()
    report_weights(head_dir, base_dir, head_run, base_run, head_groups, base_groups, gate)
    print()
    report_ln_scale(head_dir, base_dir, gate)
    print()

    h_wall = wall_seconds(head_dir)
    b_wall = wall_seconds(base_dir)
    print("-- wall (reported only, no gate) --")
    print(
        f"  head={h_wall/60:.2f} min  base={b_wall/60:.2f} min  (target <= 16 min once Tier C "
        "actually changes the pipeline; both sides here are Tier A runs)"
        if h_wall and b_wall
        else f"  head={h_wall} base={b_wall} (one or both run.txt missing wall_s)"
    )
    print()

    print("=== summary (spec §8) ===")
    row_status = {
        "Master pixel median / MAD vs tierA-baseline": gate["median_mad"],
        "Master noise (MRS sigma) vs baseline": gate["noise"],
        "FWHM (mono, OSC per plane) vs baseline": gate["fwhm"],
        "Rejected fraction (linear fit) vs baseline": gate["rejected_fraction"],
        "Per-frame weights: Spearman rho vs baseline": gate["weights"],
        "LN relative scale s per frame vs baseline": gate["ln_scale"],
        "Drizzled master level vs undrizzled (R-M3-2)": gate["drizzle_level"],
        "Drizzle coverage": gate["drizzle_coverage"],
    }
    overall_ok = True
    for label, tol in TOLERANCE_TABLE:
        if label.startswith("Wall"):
            print(f"  {label}: [{tol}]  reported only")
            continue
        checks = row_status.get(label, [])
        if not checks:
            print(f"  {label}: [{tol}]  n/a (no data)")
            continue
        ok = all(checks)
        overall_ok = overall_ok and ok
        print(f"  {label}: [{tol}]  {verdict(ok)} ({sum(checks)}/{len(checks)} sub-checks)")

    print()
    print("NUMERIC:", "PASS" if overall_ok else "FAIL")
    return 0 if overall_ok else 1


def main():
    args = sys.argv[1:]
    numeric = "--numeric" in args
    args = [a for a in args if a != "--numeric"]
    set_id = 204
    if "--set-id" in args:
        i = args.index("--set-id")
        set_id = int(args[i + 1])
        del args[i : i + 2]
    if len(args) != 2:
        print(
            "usage: tier1-compare.py <W_base> <W_head>\n"
            "       tier1-compare.py --numeric <W_head> <W_base> [--set-id N]",
            file=sys.stderr,
        )
        sys.exit(2)
    if numeric:
        head_dir, base_dir = args
        sys.exit(run_numeric(head_dir, base_dir, set_id))
    else:
        base_dir, head_dir = args
        sys.exit(run_identity(base_dir, head_dir))


if __name__ == "__main__":
    main()
