"""Run the acceptance harness's own truth photometry (grid-report.py photometry) on a
reference plane and dumped warped member planes (ln-scale-audit; reuse, not a rebuild)."""
import importlib.util
import json
import sys
from pathlib import Path

import numpy as np
GR = str(Path(__file__).resolve().parents[1] / "acceptance" / "grid-report.py")
spec = importlib.util.spec_from_file_location("grid_report", GR)
gr = importlib.util.module_from_spec(spec); spec.loader.exec_module(gr)
ref_path, plane, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
members = sys.argv[4:]
ref = gr.planes_of(ref_path)[plane]
# The LN reference of a frame set (Legacy) has no uncovered pixels; zero_is_uncovered as the harness does.
cand = gr.reference_candidates(ref)
fref = gr.frame_fwhm(ref, cand[0], True)
print(f"reference FWHM {fref}", file=sys.stderr)
with open(out, "a") as f:
    for m in members:
        label, path = m.split("=", 1)
        mem = gr.planes_of(path)[0]
        ph = gr.photometry(ref, mem, cand, fref)
        row = dict(label=label, plane=plane, ratio=ph["ratio"], se=ph["se"], n=ph["n"], mode=ph["mode"],
                   fwhm_ref=ph["fwhm_ref"], fwhm_mem=ph["fwhm_mem"], unc=ph["unc"], note=ph["note"][:200])
        f.write(json.dumps(row) + "\n"); f.flush()
        print(label, f"{ph['ratio']:.5f}", ph["mode"], ph["n"], f"{ph['fwhm_mem']:.2f}", file=sys.stderr)
