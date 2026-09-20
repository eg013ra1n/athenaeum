#!/bin/zsh
# Tier C's external-reference comparison — the per-master median/MAD/noise/FWHM ratio table the
# M-run acceptance docs (2026-09-11 M4a, 2026-09-14 M4d) printed against the external tool's own
# masters of the same frames, e.g. "the OSC master matching the external one in level and
# background shape" / "within 25 % of the external CFA drizzle's". Those runs used ad-hoc scripts
# (`drzcheck.py`, `planestats.py`, `bayercheck.py`, …) built in a scratch dir for that one session
# and never checked in — this is the minimal, generic, checked-in equivalent Task 0 was asked to
# write in their place. `docs/superpowers/research/scripts/weight_audit_compare.py` is the sibling
# for PER-FRAME PSF-weight terms against the external tool's run LOG (a different comparison —
# masters here, not per-frame log lines); it stays untouched.
#
#   tierC-external.sh <checkpoint-dir> <external-masters-dir>
#
# <checkpoint-dir> is one of our own runs (`checkpoint.sh`'s output, or `prepare-catalog.sh`'s
# working dir directly) — the same `<dir>/stackout/*.fits|*.xisf` layout `tier1-compare.py`
# reads, never a `stacking_run_groups.master_path` DB lookup (see that script's own docstring for
# why: the column can go stale relative to a checkpoint's own directory). <external-masters-dir>
# is an arbitrary directory tree holding the external tool's own master files — its location is
# never hard-coded here (the owner's own scratch dirs move between cycles, e.g. `~/acc-xisf/` was
# raw/calibrated LIGHT frames for an unrelated XISF acceptance run, not masters at all — always
# pass the actual directory as an argument).
#
# MATCHING (best-effort, since the external tool's own naming convention is whatever the owner's
# session called it, not ours): for each of OUR masters, classified mono/OSC by measure_probe's
# own channel count (1 vs 3) and plain/drizzle by our `_drizzleNx` filename suffix, the matching
# external file is:
#   1. an explicit override, if set: TIERC_EXTERNAL_MONO / TIERC_EXTERNAL_MONO_DRIZZLE /
#      TIERC_EXTERNAL_OSC / TIERC_EXTERNAL_OSC_DRIZZLE (absolute paths) — set these when the
#      filename heuristic below can't find (or picks the wrong) file;
#   2. else the first *.fits/*.xisf under <external-masters-dir> (recursive) whose name contains
#      "mono"/"osc" (case-insensitive) for the colour mode, and "drizzle" (case-insensitive) or
#      not, for plain vs drizzle. A directory with more than one plausible candidate per slot, or
#      none, is reported and that slot is skipped (n/a) rather than guessed at.
#
# This script has not been run against a real external-masters directory (none was available on
# this machine when it was written — see the Task 0 report) — the matching heuristic and the
# override env vars are the fallback for whatever naming convention shows up; a live run against
# real files is owed to the owner, same as the external-tool "open our .xisf" checks in the M4d
# acceptance doc.
set -euo pipefail
CKPT="${1:?checkpoint dir}"
EXT="${2:?external masters dir}"
[ -d "$CKPT/stackout" ] || { echo "no $CKPT/stackout — is this a checkpoint dir?" >&2; exit 1; }
[ -d "$EXT" ] || { echo "no such directory: $EXT" >&2; exit 1; }

REPO="$(cd "$(dirname "$0")/../../../../.." && pwd)"
PROBE="$REPO/.superpowers/target-acc/release/examples/measure_probe"
[ -x "$PROBE" ] || { echo "measure_probe not found at $PROBE — build it first" >&2; exit 1; }

python3 - "$CKPT" "$EXT" "$PROBE" <<'PYEOF'
import glob
import json
import os
import re
import statistics
import subprocess
import sys

ckpt, ext_dir, probe = sys.argv[1:4]

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


def measure(path):
    proc = subprocess.run([probe, path], capture_output=True, text=True, timeout=120)
    if proc.returncode != 0:
        raise RuntimeError(f"measure_probe failed on {path}: {proc.stderr.strip()[-500:]}")
    data = json.loads(proc.stdout)
    return data if isinstance(data, list) else data["channels"]


def find_external_candidates(ext_dir, color_word, want_drizzle):
    out = []
    for extn in ("fits", "xisf", "FITS", "XISF"):
        out.extend(glob.glob(os.path.join(ext_dir, "**", f"*.{extn}"), recursive=True))
    out = sorted(set(out))
    hits = []
    for p in out:
        name = os.path.basename(p).lower()
        if color_word not in name:
            continue
        has_drizzle = "drizzle" in name
        if has_drizzle != want_drizzle:
            continue
        hits.append(p)
    return hits


def pick_external(env_var, ext_dir, color_word, want_drizzle, label):
    override = os.environ.get(env_var)
    if override:
        if not os.path.exists(override):
            print(f"  {label}: {env_var} is set but does not exist: {override}", file=sys.stderr)
            return None
        return override
    hits = find_external_candidates(ext_dir, color_word, want_drizzle)
    if len(hits) == 1:
        return hits[0]
    if not hits:
        print(f"  {label}: n/a — no file under {ext_dir} matched '{color_word}'"
              f"{' + drizzle' if want_drizzle else ' (non-drizzle)'}; set {env_var} to name it explicitly")
        return None
    print(f"  {label}: n/a — {len(hits)} ambiguous candidates under {ext_dir}, set {env_var}:")
    for h in hits[:10]:
        print(f"    {h}")
    return None


def pct(ratio):
    return (ratio - 1.0) * 100.0


def report_pair(label, our_path, ext_path):
    print(f"-- {label} --")
    print(f"  ours:     {our_path}")
    print(f"  external: {ext_path}")
    our_ch, ext_ch = measure(our_path), measure(ext_path)
    if len(our_ch) != len(ext_ch):
        print(f"  FAIL channel count differs: ours={len(our_ch)} external={len(ext_ch)}")
        return
    for i, (oc, ec) in enumerate(zip(our_ch, ext_ch)):
        m = oc["median"] / ec["median"] if ec["median"] else float("nan")
        d = oc["mad"] / ec["mad"] if ec["mad"] else float("nan")
        n = oc["noise"] / ec["noise"] if ec["noise"] else float("nan")
        f = oc["fwhmPx"] / ec["fwhmPx"] if ec["fwhmPx"] else float("nan")
        print(
            f"  plane {i}: median ratio={m:.6f} ({pct(m):+.4f}%)  mad ratio={d:.6f} ({pct(d):+.4f}%)"
        )
        print(
            f"           noise  ratio={n:.6f} ({pct(n):+.4f}%)  fwhmPx ratio={f:.6f} ({pct(f):+.4f}%)"
        )


print(f"=== Tier C external-reference comparison ===")
print(f"checkpoint: {ckpt}")
print(f"external:   {ext_dir}")
print()

plain, drizzle = classify_ours(os.path.join(ckpt, "stackout"))
if not plain:
    print(f"no masters found under {ckpt}/stackout", file=sys.stderr)
    sys.exit(1)

# Classify OUR OWN masters mono/OSC by measured channel count (never by filename — the filename
# convention is ours, not a contract with the external tool's).
by_color = {"mono": None, "osc": None}
by_color_drizzle = {"mono": None, "osc": None}
for key, path in plain.items():
    ch = measure(path)
    color = "mono" if len(ch) == 1 else ("osc" if len(ch) == 3 else None)
    if color is None:
        print(f"  note: {key} has {len(ch)} channels (neither 1 nor 3) — skipped", file=sys.stderr)
        continue
    if by_color[color] is not None:
        print(f"  note: more than one {color} plain master in {ckpt}/stackout — using {path}", file=sys.stderr)
    by_color[color] = path
    if key in drizzle:
        by_color_drizzle[color] = drizzle[key]

any_compared = False
for color in ("mono", "osc"):
    our_plain = by_color[color]
    if our_plain is None:
        print(f"-- {color}: n/a (no {color} master in this checkpoint) --")
        continue
    ext_plain = pick_external("TIERC_EXTERNAL_" + color.upper(), ext_dir, color, False, f"{color} plain")
    if ext_plain:
        report_pair(f"{color} plain master", our_plain, ext_plain)
        any_compared = True
    our_drizzle = by_color_drizzle[color]
    if our_drizzle:
        ext_drizzle = pick_external(
            "TIERC_EXTERNAL_" + color.upper() + "_DRIZZLE", ext_dir, color, True, f"{color} drizzle"
        )
        if ext_drizzle:
            report_pair(f"{color} drizzle master", our_drizzle, ext_drizzle)
            any_compared = True

print()
if not any_compared:
    print("RESULT: NO COMPARISONS MADE — no external master could be matched; "
          "see the notes above and set the TIERC_EXTERNAL_* overrides.")
    sys.exit(1)
print("RESULT: reported above — this script prints ratios only, it does not gate PASS/FAIL "
      "(the M-run docs' own targets are directional/data-shape targets, e.g. \"within 25 % of "
      "the external CFA drizzle's, same direction\" — not a fixed tolerance table like spec §8's, "
      "which tier1-compare.py --numeric already gates).")
PYEOF
