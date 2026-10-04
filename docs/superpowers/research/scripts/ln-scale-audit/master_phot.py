"""Master vs LN reference photometry with the acceptance harness's truth photometry, and the
prediction from per-member gaps x weights (ln-scale-audit)."""
import csv
import importlib.util
import json
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

if __name__ == "__main__":
    ref, master, plane, x0, summ, key, dec = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5], sys.argv[6], sys.argv[7]
    ph = measure(ref, master, plane, x0=x0)
    eps_w, n, miss, eps_mean = predicted(summ, key, dec, plane)
    print(json.dumps(dict(master=master.split('/')[-1], plane=plane, ratio_ref_over_master=ph["ratio"], se=ph["se"], n=ph["n"], mode=ph["mode"],
        fwhm_ref=ph["fwhm_ref"], fwhm_master=ph["fwhm_mem"], unc=ph["unc"], eps_weighted=eps_w, eps_mean=eps_mean, members=n, members_missing=miss,
        predicted_ratio=1 / (1 + eps_w))))
