"""Master vs LN reference photometry with the acceptance harness's truth photometry, and the
prediction from per-member gaps x weights (ln-scale-audit).

    master_phot.py <ref> <master> <plane> <x0> [<summary.json> <group-key> <decomp.csv>]
                   [--y0 Y0] [--noise]

The last three positionals are optional: the `predicted` block is printed only when all three
are given (a set without a decomposition CSV has none). --y0 is the master's row offset in the
reference canvas (default 0). --noise runs `measure_probe` on the master (the binary
tier1-compare.py uses), reads the plane's `.noise` and adds `noise`, `k` (1 / ratio) and
`snr` (k / noise) to the JSON line (P-17)."""
import csv
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import numpy as np
GR = str(Path(__file__).resolve().parents[1] / "acceptance" / "grid-report.py")
spec = importlib.util.spec_from_file_location("grid_report", GR); gr = importlib.util.module_from_spec(spec); spec.loader.exec_module(gr)

def measure(ref_path, master_path, plane, x0=0, y0=0):
    ref = gr.planes_of(ref_path)[plane]
    m = gr.planes_of(master_path)[plane].astype(np.float64)
    pad = np.full(ref.shape, np.nan)
    pad[y0:y0 + m.shape[0], x0:x0 + m.shape[1]] = m
    pad[pad == 0.0] = np.nan
    cand = gr.reference_candidates(ref)
    fref = gr.frame_fwhm(ref, cand[0], True)
    return gr.photometry(ref, pad.astype(np.float32), cand, fref)

def predicted(summary_json, group_key, decomp_csv, plane, handle_key="frameId"):
    d = json.load(open(summary_json))
    g = [g for g in d["groups"] if g["key"] == group_key][0]
    gaps = {}
    for r in csv.DictReader(open(decomp_csv)):
        if r.get("phot") and int(r["plane"]) == plane:
            gaps[int(r["handle"])] = float(r["applied"]) / float(r["phot"]) - 1
    num = den = 0.0; n = 0; miss = 0
    for f in g["frames"]:
        if not f["included"]: continue
        w = (f.get("weightChannels") or [f["weight"]])[plane] if f.get("weightChannels") else f["weight"]
        e = gaps.get(f[handle_key])
        if e is None: miss += 1; continue
        num += w * e; den += w; n += 1
    return num / den, n, miss, float(np.mean(list(gaps.values())))

MEASURE_PROBE = Path(__file__).resolve().parents[5] / ".superpowers" / "target-acc" / "release" / "examples" / "measure_probe"


def plane_noise(master_path, plane):
    """The plane's MRS `.noise` from `measure_probe <master>` (same read as tier1-compare.py)."""
    if not MEASURE_PROBE.exists():
        sys.exit(f"--noise needs measure_probe, not found at {MEASURE_PROBE} - build it first "
                 "(cargo build --release -p athenaeum-core --example measure_probe, target-dir .superpowers/target-acc)")
    proc = subprocess.run([str(MEASURE_PROBE), master_path], capture_output=True, text=True, timeout=300)
    if proc.returncode != 0:
        sys.exit(f"measure_probe failed on {master_path}: {proc.stderr.strip()[-500:]}")
    data = json.loads(proc.stdout)
    channels = data if isinstance(data, list) else data["channels"]
    return float(channels[plane]["noise"])


if __name__ == "__main__":
    argv = sys.argv[1:]
    want_noise = "--noise" in argv
    argv = [a for a in argv if a != "--noise"]
    y0 = 0
    if "--y0" in argv:
        i = argv.index("--y0")
        y0 = int(argv[i + 1])
        del argv[i:i + 2]
    if len(argv) not in (4, 7):
        sys.exit(__doc__)
    ref, master, plane, x0 = argv[0], argv[1], int(argv[2]), int(argv[3])
    ph = measure(ref, master, plane, x0=x0, y0=y0)
    out = dict(master=master.split('/')[-1], plane=plane, ratio_ref_over_master=ph["ratio"], se=ph["se"], n=ph["n"], mode=ph["mode"],
        fwhm_ref=ph["fwhm_ref"], fwhm_master=ph["fwhm_mem"], unc=ph["unc"])
    if len(argv) == 7:
        summ, key, dec = argv[4:7]
        eps_w, n, miss, eps_mean = predicted(summ, key, dec, plane)
        out.update(eps_weighted=eps_w, eps_mean=eps_mean, members=n, members_missing=miss, predicted_ratio=1 / (1 + eps_w))
    if want_noise:
        noise = plane_noise(master, plane)
        k = 1 / ph["ratio"]
        out.update(noise=noise, k=k, snr=k / noise)
    print(json.dumps(out))
