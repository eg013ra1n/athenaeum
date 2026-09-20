#!/bin/zsh
# Tier C's external-reference gate (fix round 1, controller ruling C-8) — the N-INDEPENDENT
# checks from the M-run acceptance docs (2026-09-11 M4a, 2026-09-14 M4d) against the external
# tool's own masters and per-frame log of the same frames, gated this time (PASS/FAIL), not
# merely reported. The earlier version of this script (Task 0) only printed ratios with no gate,
# because no real external-masters directory was available when it was written; ruling C-8 names
# one, so this round replaces that draft with a real, gated comparison.
#
#   tierC-external.sh <checkpoint-dir> [external-dir]
#
# [external-dir] defaults to $TIERC_EXTERNAL_MASTERS, else the read-only directory ruling C-8
# names:
#   /Volumes/BigMac/Users/astrobureau/Pictures/Calibration Test/LDN1272-Output/master/
# — masterLight_…mono….xisf / …RGB….xisf / …drizzle_2x.xisf are the masters this script reads;
# LN_Reference_… files in the same directory are NOT masters (the external tool's own LN
# reference build) and are never matched.
#
# WHY THESE ROWS ARE GATED AND THE OTHERS ARE NOT (ruling C-8): the external masters are of the
# FULL LDN 1272 catalog (368 frames: 208 mono + 160 OSC), our checkpoints stack the 197-frame
# reduced set (`LDN1272-test`: 92 mono + 105 OSC) — so master noise and pixel median are NOT
# comparable at equal N (a smaller stack is noisier and, under this pipeline's sky-penalized
# normalization anchor, can pick a different reference frame's own sky level — see the M4a
# report's "mono master level follows the sky-penalized anchor" finding). Four quantities ARE
# N-independent and are what this script gates:
#   - FWHM per plane (a star's width does not depend on how many frames were stacked to measure
#     it, only on the registration/seeing/optics) — within +/- 2 %;
#   - the drizzled/undrizzled FWHM RATIO (a self-normalizing quantity — ruling R-T6-1's own bar)
#     — within 25 %;
#   - per-frame PSF-signal weights (a property of each individual calibrated frame, not of the
#     stack) — Spearman rho >= 0.9, top-20 overlap >= 15 (the M4a bar, tightened from its own
#     top-20 >= 14);
#   - rejected fraction (a property of the REJECTION ALGORITHM at its configured thresholds, not
#     of stack depth, though in practice it drifts a little with N — hence the wider +/- 0.5 pp
#     tolerance here versus spec §8's +/- 0.3 pp against our own bit-identical baseline).
# Noise is reported with an explicit sqrt(N) SCALING as information only, never gated — the
# scaling is the textbook sqrt(N) improvement for UNCORRELATED read/shot noise; it is not a
# tested model of this pipeline's actual noise-vs-N behaviour (rejection, local normalization and
# the reference-frame pick all interact with N in ways this one-line scaling does not capture),
# so a mismatch there proves nothing on its own.
#
# Prints `EXTERNAL: PASS|FAIL`; a row that cannot be computed prints `n/a` with the reason —
# never a silent PASS (ruling C-8's own requirement).
set -euo pipefail
CKPT="${1:?checkpoint dir}"
EXT="${2:-${TIERC_EXTERNAL_MASTERS:-/Volumes/BigMac/Users/astrobureau/Pictures/Calibration Test/LDN1272-Output/master/}}"
[ -d "$CKPT/stackout" ] || { echo "no $CKPT/stackout — is this a checkpoint dir?" >&2; exit 1; }
[ -d "$EXT" ] || { echo "no such directory: $EXT" >&2; exit 1; }

REPO="$(cd "$(dirname "$0")/../../../../.." && pwd)"
PROBE="$REPO/.superpowers/target-acc/release/examples/measure_probe"
[ -x "$PROBE" ] || { echo "measure_probe not found at $PROBE — build it first" >&2; exit 1; }
WAC="$REPO/docs/superpowers/research/scripts/weight_audit_compare.py"
[ -f "$WAC" ] || { echo "weight_audit_compare.py not found at $WAC" >&2; exit 1; }

python3 - "$CKPT" "$EXT" "$PROBE" "$WAC" <<'PYEOF'
import glob
import importlib.util
import json
import os
import re
import sqlite3
import statistics
import subprocess
import sys
import time

ckpt, ext_dir, probe, wac_path = sys.argv[1:5]

spec = importlib.util.spec_from_file_location("weight_audit_compare", wac_path)
wac = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wac)

FWHM_TOL = 0.02  # +/- 2 %
DRIZZLE_RATIO_TOL = 0.25  # within 25 % (ruling R-T6-1's own bar)
REJECTED_TOL_PP = 0.5
WEIGHTS_RHO_MIN = 0.9  # the M4a bar
WEIGHTS_TOP20_MIN = 15  # tightened from the M4a bar's own >= 14 (ruling C-8)
EXTERNAL_FRAME_COUNT_DEFAULT = 368  # the full LDN 1272 catalog (208 mono + 160 OSC)

gate = {"fwhm": [], "drizzle_ratio": [], "weights": [], "rejected": []}


def verdict(ok):
    return "PASS" if ok else "FAIL"


def pct(ratio):
    return (ratio - 1.0) * 100.0


# --- measure_probe ------------------------------------------------------------

_measure_cache = {}


def measure(path):
    key = os.path.realpath(path)
    if key in _measure_cache:
        return _measure_cache[key]
    proc = subprocess.run([probe, path], capture_output=True, text=True, timeout=300)
    if proc.returncode != 0:
        raise RuntimeError(f"measure_probe failed on {path}: {proc.stderr.strip()[-500:]}")
    data = json.loads(proc.stdout)
    channels = data if isinstance(data, list) else data["channels"]
    _measure_cache[key] = channels
    return channels


# --- our own masters ----------------------------------------------------------

DRIZZLE_RE = re.compile(r"^(?P<base>.+)_drizzle(?P<scale>\d+)x$")


def classify_ours(stackout_dir):
    plain, drizzle = {}, {}
    for extn in ("fits", "xisf"):
        for p in sorted(glob.glob(os.path.join(stackout_dir, f"*.{extn}"))):
            stem = os.path.basename(p)[: -(len(extn) + 1)]
            if stem.endswith("_weight"):
                continue
            m = DRIZZLE_RE.match(stem)
            if m:
                drizzle[m.group("base")] = p
            else:
                plain[stem] = p
    return plain, drizzle


def our_masters(ckpt_dir):
    """-> {"mono": {"plain": path, "drizzle": path|None}, "osc": {...}}, classified by
    measured channel count (never by our own filename convention)."""
    plain, drizzle = classify_ours(os.path.join(ckpt_dir, "stackout"))
    out = {"mono": {"plain": None, "drizzle": None}, "osc": {"plain": None, "drizzle": None}}
    for key, path in plain.items():
        ch = measure(path)
        color = "mono" if len(ch) == 1 else ("osc" if len(ch) == 3 else None)
        if color is None:
            print(f"  note: our {key} has {len(ch)} channels (neither 1 nor 3) — skipped", file=sys.stderr)
            continue
        if out[color]["plain"] is not None:
            print(f"  note: more than one {color} plain master in {ckpt_dir}/stackout — using {path}", file=sys.stderr)
        out[color]["plain"] = path
        if key in drizzle:
            out[color]["drizzle"] = drizzle[key]
    return out


# --- external masters ----------------------------------------------------------


def pick_ext_light_masters(ext_dir):
    """-> ({"mono_plain":path,...}, [notes]). Only *masterLight* files (never the
    LN_Reference_… ones), classified mono/OSC by filename ("_mono" / "_rgb"|"cfa") and
    plain/drizzle by "drizzle" in the name. When more than one plain candidate exists for a
    colour (this real directory has "_(1)" reruns), prefer the one whose mtime falls on the
    SAME CALENDAR DAY as that colour's (unique) drizzle master — the drizzle master exists only
    once per colour here, so it anchors which session's plain master belongs with it; ambiguous
    otherwise (never guessed silently)."""
    lights = []
    for extn in ("xisf", "fits", "XISF", "FITS"):
        lights.extend(glob.glob(os.path.join(ext_dir, f"*.{extn}")))
    lights = sorted(set(p for p in lights if "masterlight" in os.path.basename(p).lower()))
    drizzle_candidates = {"mono": [], "osc": []}
    plain_candidates = {"mono": [], "osc": []}
    for p in lights:
        name = os.path.basename(p).lower()
        if "_rgb" in name or "cfa" in name:
            color = "osc"
        elif "_mono" in name:
            color = "mono"
        else:
            continue
        (drizzle_candidates if "drizzle" in name else plain_candidates)[color].append(p)
    result = {}
    notes = []
    for color in ("mono", "osc"):
        dz = drizzle_candidates[color]
        if len(dz) == 1:
            result[f"{color}_drizzle"] = dz[0]
        elif not dz:
            notes.append(f"{color}: no drizzle master found under {ext_dir}")
        else:
            notes.append(f"{color}: {len(dz)} drizzle masters found, ambiguous: {dz}")
        pcs = plain_candidates[color]
        chosen = None
        if len(pcs) == 1:
            chosen = pcs[0]
        elif len(pcs) > 1:
            dz_key = f"{color}_drizzle"
            if dz_key in result:
                dz_day = time.localtime(os.path.getmtime(result[dz_key]))[:3]
                same_day = [p for p in pcs if time.localtime(os.path.getmtime(p))[:3] == dz_day]
                if len(same_day) == 1:
                    chosen = same_day[0]
                    notes.append(
                        f"{color}: {len(pcs)} plain masters found, picked the one sharing the "
                        f"drizzle master's session date: {os.path.basename(chosen)}"
                    )
            if chosen is None:
                notes.append(f"{color}: {len(pcs)} plain masters found, ambiguous: {pcs}")
        elif not pcs:
            notes.append(f"{color}: no plain master found under {ext_dir}")
        if chosen:
            result[f"{color}_plain"] = chosen
    # Explicit overrides always win.
    for color in ("mono", "osc"):
        for kind, env in ((f"{color}_plain", f"TIERC_EXTERNAL_{color.upper()}"),
                          (f"{color}_drizzle", f"TIERC_EXTERNAL_{color.upper()}_DRIZZLE")):
            override = os.environ.get(env)
            if override:
                if not os.path.exists(override):
                    notes.append(f"{env} is set but does not exist: {override}")
                else:
                    result[kind] = override
                    notes.append(f"{kind}: using {env} override: {override}")
    return result, notes


# --- FWHM + drizzle-ratio gate --------------------------------------------------


def fwhm_report(ours, ext, color):
    print(f"-- {color} FWHM per plane (head vs external, +/- 2 %) --")
    op, dp = ours[color]["plain"], ours[color].get("drizzle")
    ep, ed = ext.get(f"{color}_plain"), ext.get(f"{color}_drizzle")
    if op is None:
        print("  n/a: no plain master for this colour on head")
        return
    if ep is None:
        print(f"  n/a: no external plain master matched for {color}")
        return
    oc = measure(op)
    ecm = measure(ep)
    if len(oc) != len(ecm):
        print(f"  FAIL channel count differs: head={len(oc)} external={len(ecm)}")
        gate["fwhm"].append(False)
        return
    head_plain_fwhm, ext_plain_fwhm = [], []
    for i, (o, e) in enumerate(zip(oc, ecm)):
        ratio = o["fwhmPx"] / e["fwhmPx"] if e["fwhmPx"] else float("nan")
        ok = abs(pct(ratio)) <= FWHM_TOL * 100
        gate["fwhm"].append(ok)
        print(
            f"  plane {i}: head={o['fwhmPx']:.4f}px external={e['fwhmPx']:.4f}px "
            f"ratio={ratio:.4f} ({pct(ratio):+.2f}%) {verdict(ok)}"
        )
        head_plain_fwhm.append(o["fwhmPx"])
        ext_plain_fwhm.append(e["fwhmPx"])

    print(f"-- {color} drizzled/undrizzled FWHM ratio (head vs external, within 25 %) --")
    if dp is None:
        print("  n/a: no drizzle master for this colour on head")
        return
    if ed is None:
        print(f"  n/a: no external drizzle master matched for {color}")
        return
    odc = measure(dp)
    edc = measure(ed)
    if len(odc) != len(oc) or len(edc) != len(ecm):
        print(
            f"  FAIL channel count mismatch (head plain={len(oc)} drizzle={len(odc)}, "
            f"external plain={len(ecm)} drizzle={len(edc)})"
        )
        gate["drizzle_ratio"].append(False)
        return
    for i in range(len(oc)):
        head_ratio = odc[i]["fwhmPx"] / head_plain_fwhm[i] if head_plain_fwhm[i] else float("nan")
        ext_ratio = edc[i]["fwhmPx"] / ext_plain_fwhm[i] if ext_plain_fwhm[i] else float("nan")
        rel = (head_ratio / ext_ratio - 1.0) if ext_ratio else float("nan")
        ok = abs(rel) <= DRIZZLE_RATIO_TOL
        gate["drizzle_ratio"].append(ok)
        print(
            f"  plane {i}: head ratio={head_ratio:.4f} external ratio={ext_ratio:.4f} "
            f"head/external-1={rel*100:+.2f}% (tol +/-{DRIZZLE_RATIO_TOL*100:.0f}%) {verdict(ok)}"
        )

    print(f"-- {color} noise (informational, sqrt(N) scaled — NOT gated) --")
    n_head = our_total_included(ckpt)
    n_ext = int(os.environ.get("TIERC_EXTERNAL_FRAME_COUNT", EXTERNAL_FRAME_COUNT_DEFAULT))
    scale = (n_head / n_ext) ** 0.5 if n_ext else float("nan")
    for i, (o, e) in enumerate(zip(oc, ecm)):
        scaled = o["noise"] / (e["noise"] * scale) if e["noise"] and scale else float("nan")
        print(
            f"  plane {i}: head_noise={o['noise']:.4g} external_noise={e['noise']:.4g} "
            f"head/(external*sqrt({n_head}/{n_ext}))={scaled:.4f}"
        )


_total_included_cache = {}


def our_total_included(ckpt_dir):
    if ckpt_dir in _total_included_cache:
        return _total_included_cache[ckpt_dir]
    conn = sqlite3.connect(os.path.join(ckpt_dir, "athenaeum.db"))
    row = conn.execute(
        "SELECT id FROM stacking_runs WHERE frames_set_id = 204 AND status = 'done' ORDER BY id DESC LIMIT 1"
    ).fetchone()
    n = 0
    if row:
        n = conn.execute(
            "SELECT COUNT(*) FROM stacking_run_frames WHERE run_id = ? AND included = 1", (row[0],)
        ).fetchone()[0]
    conn.close()
    _total_included_cache[ckpt_dir] = n
    return n


# --- per-frame weights gate (weight_audit_compare.py's own functions) ----------


def export_ours_for_wac(ckpt_dir, set_id=204):
    conn = sqlite3.connect(os.path.join(ckpt_dir, "athenaeum.db"))
    row = conn.execute(
        "SELECT id FROM stacking_runs WHERE frames_set_id = ? AND status = 'done' ORDER BY id DESC LIMIT 1",
        (set_id,),
    ).fetchone()
    if not row:
        conn.close()
        return {}
    rows = conn.execute(
        """
        SELECT srf.metrics_json, fi.path
        FROM stacking_run_frames srf
        JOIN frames fr ON fr.id = srf.frame_id
        JOIN files fi ON fi.id = fr.file_id
        WHERE srf.run_id = ? AND srf.included = 1 AND srf.metrics_json IS NOT NULL
        """,
        (row[0],),
    ).fetchall()
    conn.close()
    out = {}
    for metrics_json, path in rows:
        m = json.loads(metrics_json)
        stem = os.path.splitext(os.path.basename(path))[0]
        out[stem] = m["channels"]
    return out


def find_external_log(ext_dir, our_stems):
    """Searches near `ext_dir` for *.log files and returns the one whose
    `wac.parse_external_log` result matches the most of `our_stems` — self-validating, no
    filename convention assumed. -> (path|None, n_matches, [candidates tried])."""
    search_roots = {ext_dir, os.path.dirname(ext_dir.rstrip("/"))}
    candidates = set()
    for root in search_roots:
        candidates |= set(glob.glob(os.path.join(root, "**", "*.log"), recursive=True))
    best_path, best_n = None, 0
    tried = []
    for c in sorted(candidates):
        try:
            parsed = wac.parse_external_log(c)
        except Exception as e:  # noqa: BLE001
            tried.append(f"{c}: unreadable ({e})")
            continue
        n = len(set(parsed) & our_stems)
        tried.append(f"{c}: {n} of {len(our_stems)} of our frame stems matched")
        if n > best_n:
            best_n = n
            best_path = c
    return best_path, best_n, tried


def weights_gate(ckpt_dir, ext_dir):
    print("-- per-frame weights (weight_audit_compare.py's own functions; rho >= 0.9, top-20 >= 15) --")
    ours = export_ours_for_wac(ckpt_dir)
    if not ours:
        print("  n/a: no per-frame metrics_json found in this checkpoint's run")
        return
    log_path, n_matched, tried = find_external_log(ext_dir, set(ours))
    if log_path is None or n_matched == 0:
        print("  n/a: no external per-frame log found with any matching frame stem. Searched:")
        for t in tried:
            print(f"    {t}")
        return
    print(f"  external log: {log_path} ({n_matched}/{len(ours)} of our frame stems present)")
    ext = wac.parse_external_log(log_path)
    groups = wac.group_stems(ours)
    any_checked = False
    for group_name, stems in groups.items():
        matched = [s for s in stems if s in ext]
        if not matched:
            print(f"  {group_name}: n/a (0 of {len(stems)} frames matched the log)")
            continue
        n_channels = len(ours[matched[0]])
        for ch_idx in range(n_channels):
            xs_o, xs_e = [], []
            for s in matched:
                if ch_idx >= len(ours[s]):
                    continue
                oc = ours[s][ch_idx]
                ec = ext[s]["ch"].get(ch_idx)
                if ec is None:
                    continue
                v = wac.external_psf_signal_weight(ec)
                if v is not None:
                    xs_o.append(oc["psfSignalWeight"])
                    xs_e.append(v)
            rho = wac.spearman(xs_o, xs_e)
            if rho is None:
                print(f"  {group_name} ch{ch_idx}: n/a (fewer than 3 comparable frames)")
                continue
            ok = rho >= WEIGHTS_RHO_MIN
            any_checked = True
            gate["weights"].append(ok)
            print(f"  {group_name} ch{ch_idx}: psfsw_spearman={rho:.4f} (>= {WEIGHTS_RHO_MIN}) {verdict(ok)} n={len(xs_o)}")
        ours_mean, ext_mean = {}, {}
        for s in matched:
            vals = [c["psfSignalWeight"] for c in ours[s]]
            if vals:
                ours_mean[s] = sum(vals) / len(vals)
            evs = []
            for ch_idx in range(n_channels):
                ec = ext[s]["ch"].get(ch_idx)
                if ec is None:
                    continue
                v = wac.external_psf_signal_weight(ec)
                if v is not None:
                    evs.append(v)
            if evs:
                ext_mean[s] = sum(evs) / len(evs)
        if len(ours_mean) >= 20 and len(ext_mean) >= 20:
            om, em = max(ours_mean.values()), max(ext_mean.values())
            on = {s: v / om for s, v in ours_mean.items() if om > 0}
            en = {s: v / em for s, v in ext_mean.items() if em > 0}
            top_o = set(sorted(on, key=lambda s: -on[s])[:20])
            top_e = set(sorted(en, key=lambda s: -en[s])[:20])
            overlap = len(top_o & top_e)
            ok = overlap >= WEIGHTS_TOP20_MIN
            any_checked = True
            gate["weights"].append(ok)
            print(f"  {group_name} top20 overlap={overlap}/20 (>= {WEIGHTS_TOP20_MIN}) {verdict(ok)}")
        else:
            print(f"  {group_name} top20 overlap: n/a (ours={len(ours_mean)} ext={len(ext_mean)}, need >= 20 both)")
    if not any_checked:
        print("  n/a: nothing matched closely enough to grade")


# --- rejected fraction gate -----------------------------------------------------


def rejected_fraction_gate(ckpt_dir):
    print(f"-- rejected fraction (head vs external, +/- {REJECTED_TOL_PP} pp) --")
    conn = sqlite3.connect(os.path.join(ckpt_dir, "athenaeum.db"))
    row = conn.execute(
        "SELECT id FROM stacking_runs WHERE frames_set_id = 204 AND status = 'done' ORDER BY id DESC LIMIT 1"
    ).fetchone()
    if not row:
        conn.close()
        print("  n/a: no done run for set 204 in this checkpoint")
        return
    groups = conn.execute(
        "SELECT group_key, stats_json FROM stacking_run_groups WHERE run_id = ?", (row[0],)
    ).fetchall()
    conn.close()
    any_checked = False
    for group_key, stats_json in groups:
        color = "mono" if group_key.startswith("mono") else ("osc" if group_key.startswith("osc") else group_key)
        stats = json.loads(stats_json) if stats_json else None
        if not stats:
            print(f"  {group_key}: n/a (no stats_json)")
            continue
        head_pct = (stats["rejectedLowFraction"] + stats["rejectedHighFraction"]) * 100.0
        ext_val = os.environ.get(f"TIERC_EXTERNAL_REJECTED_{color.upper()}") or os.environ.get("TIERC_EXTERNAL_REJECTED")
        if ext_val is None:
            print(
                f"  {group_key}: head={head_pct:.4f}% external=n/a (no machine-readable rejected-fraction "
                f"source found in the external tool's logs — set TIERC_EXTERNAL_REJECTED_{color.upper()} "
                f"or TIERC_EXTERNAL_REJECTED to grade this row)"
            )
            continue
        ext_pct = float(ext_val)
        delta = head_pct - ext_pct
        ok = abs(delta) <= REJECTED_TOL_PP
        any_checked = True
        gate["rejected"].append(ok)
        print(f"  {group_key}: head={head_pct:.4f}% external={ext_pct:.4f}% delta={delta:+.4f}pp {verdict(ok)}")
    if not any_checked:
        print("  n/a: no TIERC_EXTERNAL_REJECTED(_<GROUP>) set and no machine-readable external value found")


# --- main ------------------------------------------------------------------------

print("=== Tier C external-reference gate (ruling C-8) ===")
print(f"checkpoint: {ckpt}")
print(f"external:   {ext_dir}")
print()

ext_masters, notes = pick_ext_light_masters(ext_dir)
if notes:
    print("-- external master selection notes --")
    for n in notes:
        print(f"  {n}")
    print()

ours = our_masters(ckpt)

for color in ("mono", "osc"):
    if ours[color]["plain"] is None:
        print(f"-- {color}: n/a (no {color} master in this checkpoint) --")
        continue
    fwhm_report(ours, ext_masters, color)
    print()

weights_gate(ckpt, ext_dir)
print()
rejected_fraction_gate(ckpt)
print()

print("=== summary ===")
overall_ok = True
any_data = False
for label, key, tol in (
    ("FWHM per plane vs external", "fwhm", "+/- 2 %"),
    ("Drizzled/undrizzled FWHM ratio vs external", "drizzle_ratio", "within 25 %"),
    ("Per-frame weights vs external log", "weights", f">= {WEIGHTS_RHO_MIN} rho, >= {WEIGHTS_TOP20_MIN}/20 overlap"),
    ("Rejected fraction vs external", "rejected", f"+/- {REJECTED_TOL_PP} pp"),
):
    checks = gate[key]
    if not checks:
        print(f"  {label}: [{tol}]  n/a (no data)")
        continue
    any_data = True
    ok = all(checks)
    overall_ok = overall_ok and ok
    print(f"  {label}: [{tol}]  {verdict(ok)} ({sum(checks)}/{len(checks)} sub-checks)")
print("  Noise (informational, sqrt(N)-scaled): reported above, never gated")

print()
if not any_data:
    print("EXTERNAL: FAIL — nothing could be graded (see the n/a reasons above)")
    sys.exit(1)
print("EXTERNAL:", "PASS" if overall_ok else "FAIL")
sys.exit(0 if overall_ok else 1)
PYEOF
