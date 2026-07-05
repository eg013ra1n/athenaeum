# Golden comparison: Athenaeum master vs. Siril master

Dev-only, owner-run procedure — **not part of CI** (Siril isn't installed in
CI, and this needs a real raw calibration set on disk). This is the spec §10
"golden reference test" in its manual form: it verifies the Phase 2 Plan A
integration engine (`crates/athenaeum-core/src/integration/`) produces the
same result, pixel-for-pixel within a small tolerance, as an independently
implemented reference stacker (Siril) applying the equivalent recipe.

Design background: `docs/superpowers/specs/2026-07-04-phase2-calibration-library-design.md`
§9 ("Bias / Dark / DarkFlat" recipe: average combine, no normalization, no
weighting; rejection = Winsorized sigma-clip 3σ/3σ for N ≥ 15) and §10
("Testing" — golden reference test). A **dark set** is used deliberately —
it exercises the plain combine + Winsorized-clip path with no flat
pre-calibration or normalization involved, so the comparison isolates the
combiner/rejection math instead of also depending on the flat fallback
chain.

## Prerequisites

- A real dark calibration set already ingested into the catalog with **at
  least 15 member frames** (the N ≥ 15 floor is what selects Winsorized
  sigma-clip 3σ/3σ over the plain-median fallback — see
  `api::masters::resolve_combine`). Fewer than 15 frames exercises a
  different code path and isn't a like-for-like comparison.
- [Siril](https://siril.org/) installed, ≥ 1.2 (has the `w` — Winsorized —
  rejection type in `stack`).
- Python 3 + `astropy` + `numpy` (`pip install astropy numpy`) for the
  comparison script below.
- **Siril preference check before stacking**: open Siril → Preferences →
  make sure no "normalize to 0–1" / 16-bit rescale option is enabled for FITS
  output. Athenaeum's masters keep the input ADU scale verbatim (spec §9
  policy 2, no rescale) — if Siril silently rescales its output, the
  comparison below will report a huge (bogus) diff that has nothing to do
  with the combine/rejection math.

## Step 1 — Build the master in Athenaeum

1. Settings → designate a Calibration Library folder (if not already done).
2. Equipment → the camera that owns the dark set → **Darks** tab.
3. Find the raw dark set (≥ 15 frames) → row action **Create Master** →
   Auto preset (resolves to Winsorized sigma-clip 3σ/3σ for N ≥ 15) → Build.
4. On completion, note the master's path — printed in the completion toast
   and visible under the Masters tab / provenance panel:
   `<LibraryRoot>/<Camera>/MasterDark/master_dark_<exptime>s_<temp>C_g<gain>_bin<binning>_<date>.fits`.

## Step 2 — Build the same master in Siril

Using the **same raw frames** the Athenaeum set was built from (find them via
the raw set's file list in Equipment, or `master_provenance.member_frame_uuids`
if the raw set has since been archived — restore first if so):

```bash
siril-cli -s - <<'EOF'
cd /path/to/raw/darks
convert dark -out=../process
cd ../process
stack dark rej w 3 3 -nonorm -out=master_dark_siril
EOF
```

- `convert dark -out=../process` ingests the raw FITS frames into a Siril
  sequence named `dark_00001.fit`, `dark_00002.fit`, ….
- `stack dark rej w 3 3 -nonorm` — `rej w 3 3` selects Winsorized sigma-clip
  with low/high sigma 3.0/3.0 (matches Athenaeum's Auto recipe for N ≥ 15);
  `-nonorm` disables Siril's per-frame normalization, matching Athenaeum's
  "no normalization" policy for bias/dark/darkflat masters.
- Result: `process/master_dark_siril.fit` (or `.fits` depending on your
  Siril build's default extension).

If Siril's GUI is preferred over `siril-cli`, the equivalent manual steps are
Scripts-free: File → Conversion (or drag the raw frames into the Conversion
tab) → convert into a sequence → Stacking tab → Rejection: Winsorized,
σ low/high = 3.000/3.000 → Normalization: **None** → Stack.

## Step 3 — Compare

```python
#!/usr/bin/env python3
"""Dev-only: diff an Athenaeum master against a Siril-built master of the
same raw set. Not CI — run manually per this doc.
Usage: python3 compare_masters.py <athenaeum_master.fits> <siril_master.fit>"""
import sys
import numpy as np
from astropy.io import fits

a = fits.getdata(sys.argv[1]).astype(np.float64)
b = fits.getdata(sys.argv[2]).astype(np.float64)
assert a.shape == b.shape, f"shape mismatch: {a.shape} vs {b.shape}"
diff = np.abs(a - b)
p50, p999 = np.percentile(diff, [50, 99.9])
print(f"Athenaeum mean: {a.mean():.4f} ADU   Siril mean: {b.mean():.4f} ADU")
print(f"median |diff| = {p50:.4f} ADU   99.9th pct |diff| = {p999:.4f} ADU")
print("PASS" if p50 <= 1.0 else "FAIL", "— acceptance: median |diff| ≲ 1 ADU")
```

**Acceptance:** median absolute pixel difference ≲ 1 ADU. The 99.9th
percentile is reported for visibility (hot pixels / cosmic-ray survivors
in one stack but not the other are expected outliers) but is not itself a
pass/fail gate — only the median is.

## Results log

Owner-filled after each run. Add a row per verification pass (e.g. once per
minor release, or whenever the integration engine changes).

| Date | Camera / set (N frames) | Athenaeum master | Siril master | median \|diff\| (ADU) | 99.9pct \|diff\| (ADU) | Result | Notes |
| ---- | ------------------------ | ----------------- | -------------- | ---------------------- | ----------------------- | ------ | ----- |
|      |                           |                    |                 |                         |                          |        |       |

---

## Post-merge follow-ups

### Roadmap checkbox handoff

`docs/superpowers/plans/2026-07-02-roadmap.md` and
`docs/superpowers/specs/2026-07-02-target-features-architecture.md` had
uncommitted edits in progress from a parallel session while this task ran,
so this task deliberately did **not** touch either file. Once those edits
land, the following Phase 2 roadmap checkboxes are DONE and can be ticked:

- **rustafits linear decode API** — done, but **scope shrunk**: no new
  rustafits API was added. Instead, `crates/athenaeum-core/src/integration/banded.rs`
  reads bands directly off `astroimage::ImageConverter::read_raw`'s existing
  linear f32 output (BZERO/BSCALE already applied, CFA left mosaiced) —
  the "linear decode" need reduced to a banded *reader* inside core, not a
  new decode API inside the rustafits submodule. Reflect this nuance when
  ticking the box (it's done, just not in the originally-envisioned place).
- **B2 integrator** (calibration stage 1 — master creation) — done:
  `crates/athenaeum-core/src/integration/{combine.rs,engine.rs}`, streaming
  band-wise combine (mean/median/Winsorized sigma-clip/percentile-clip),
  recipes for bias/dark/darkflat/flat with the existing precal fallback
  chain, runs on the new `ComputeQueue`, cancellable.
- **B3 header vocabulary + `master_provenance` table** — done:
  `ATH_*` keyword namespace in `crates/athenaeum-core/src/fits_writer/keywords.rs`
  (`ATH_SRC`, `ATH_N`, `ATH_REJ`, `ATH_VER`, `ATH_HSH`, `ATH_TMIN`/`ATH_TMAX`,
  `ATH_FNRM` on flats), `master_provenance` table + CRUD in
  `crates/athenaeum-core/src/db/master_provenance.rs`, populated from
  `calibration_library/register.rs`.
- **Library folder = managed scan root; master naming; scanner ingestion**
  — done: `scan_roots.kind='calibration_library'` (single-root enforced),
  `crates/athenaeum-core/src/calibration_library/{paths.rs,headers.rs,register.rs}`,
  direct-registration/scanner-ingestion equivalence pinned by
  `direct_registration_matches_scanner_ingestion`.
- **B4 UI** ("Create Master" on calibration set + library browser) — done:
  `src/components/calibration/CreateMasterDialog.tsx` shared by
  Equipment (`CalibrationSetTable.tsx`) and the Coverage tab
  (`CalibrationTableView.tsx`, incl. batch "Create all masters"), global
  queue indicator `src/components/ComputeQueueIndicator.tsx`.

Not done (deliberately out of scope for Plan A): **B5 in-app light
calibration** (Plan B / milestone M2b) and the matcher/export integration
test item (matcher exclusion of superseded sets is pinned by
`matcher_excludes_superseded_sets`, but the roadmap's specific "WBPP/Siril
export consumes library masters" end-to-end test was not added this plan —
worth a follow-up check before M2b).

### Known v1 limitations

- **Rebuild has no recipe override.** `rebuild_master` always resolves a
  fresh Auto recipe from the current source frames/precal state — there is
  no way in v1 to rebuild with a specific non-Auto combine method. See
  `api/masters.rs::rebuild_master`'s doc comment for why (the persisted
  `recipe_json.combine` is already-resolved, not a replayable override).
- **Batch dependency ordering is only guaranteed at `compute.max_concurrent
  = 1`.** Above that, a flat build can be admitted before its precal
  darkflat/dark/bias master finishes. This degrades gracefully (fallback
  chain + a logged warning, never corruption) but the resulting flat may
  use a lesser precal than intended — see `start_master_builds_batch`'s doc
  comment.
- **Single-library-root enforcement has a benign TOCTOU window.** SQLite
  can't express a partial-unique constraint via the guarded-`ALTER TABLE`
  migration pattern this codebase uses, so uniqueness is a pre-insert
  SELECT-then-INSERT check in `api::scan_roots::check_library_root_uniqueness`,
  not a DB constraint. Two concurrent "designate calibration library root"
  calls could both pass the check before either inserts. Low real-world risk
  (this is an infrequent, single-operator Settings action) but not
  race-proof.
