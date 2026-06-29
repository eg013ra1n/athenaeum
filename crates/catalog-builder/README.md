# catalog-builder

Offline tool that builds and packages the Gaia DR3 star catalogs the Athenaeum
plate-solver uses, and produces the `smac_gaia.zip` archive the in-app
downloader installs from artfrom.space.

This crate is **build/dev tooling only** — it is not shipped in the app or the
Docker image (excluded from the workspace there).

## What it produces

Two memory-mapped `stars.smac` caches (solvemyastro format) plus a publishable
archive:

| Output | Role |
| ---- | ---- |
| `smac_gaia/stars.smac` | **Deep** catalog (G ≤ 19) — registration + the always-deep verify stage. |
| `smac_gaia_bright/stars.smac` | **Hybrid bright** sub-catalog — fast blind solving. |
| `smac_gaia.zip` + `.sha256` | What you upload; what the app downloads, verifies, extracts. |

### The hybrid bright catalog

Per HEALPix-6 cell (49,152 cells), starting from the deep cache:

1. **Floor** — keep every star brighter than `--bright-floor`.
2. **Sparse top-up** — if a cell holds fewer than `--min-per-cell`, go fainter
   than the floor until it does (or the cell is exhausted). This is what keeps
   sparse, high-galactic-latitude fields solvable instead of a flat G≤16 cut
   that leaves them with a handful of stars.
3. **Dense cap** — keep at most `--max-per-cell`, so galactic-plane cells don't
   bloat the archive.

The bright cache is purely a **speed optimization**: the solver falls back to the
deep cache when a bright cone is too sparse, and verification always uses deep —
so a mis-tuned bright catalog can slow solving but never make it fail. Tune
`--min-per-cell` / `--max-per-cell` against the density table in
`docs/platesolving/README.md` and the fallback frequency on real sparse fields.

## Pipeline

```
1. Acquire  download Gaia DR3 bulk dump (or reuse an existing --gaia-dir)
2. Bin      HEALPix-6 → <work-dir>/catalogs/gaia_dr3/healpix_*.bin
3. Deep     build_cache_from_legacy_dir → smac_gaia/stars.smac
4. Bright   hybrid_select + build_cache → smac_gaia_bright/stars.smac
5. Package  zip both → smac_gaia.zip + .sha256
6. Publish  print the scp upload command
```

Every stage is idempotent: a present Gaia file, a populated `gaia_dr3/` bins
dir, or an existing `stars.smac` is detected and reused, so a re-run resumes.

## Usage

Full build (first run — expect a large download and a long deep build):

```bash
cargo run -p catalog-builder --release -- \
  --gaia-dir /Volumes/NAS/gaia_dr3_bulk \
  --out      /Volumes/NAS/catalog_out
```

Iterate on the bright catalog only (reuses the deep cache under `--out`):

```bash
cargo run -p catalog-builder --release -- \
  --out /Volumes/NAS/catalog_out --bright-only --min-per-cell 300 --max-per-cell 600
```

Key flags (`--help` for all): `--gaia-dir`, `--work-dir`, `--out`, `--epoch`
(default 2016.0), `--bright-floor/--min-per-cell/--max-per-cell`,
`--skip-download`, `--deep-only`, `--bright-only`, `--no-zip`.

## Resource & runtime expectations

- **Disk** (`--gaia-dir` should be a large/NAS volume): ~600 GB raw `.csv.gz` +
  ~10 GB `.bin` intermediate + ~15 GB deep `stars.smac` (plus equal scratch
  during the build) + a few GB bright. ≈ **650 GB working space**.
- **Time:** download is bandwidth-bound (hours–days, resumable). The deep build
  is the slow compute step. The bright build is comparatively quick.

## Publish

Upload **both** files so they resolve at
`https://artfrom.space/catalogs/smac_gaia.zip(.sha256)` (the tool prints the
exact command):

```bash
scp -P 40022 smac_gaia.zip smac_gaia.zip.sha256 \
  <user>@artfrom.space:/var/www/artfrom.space/catalogs/
```

The in-app "Download star catalog" button (`download_gaia_dr3_prebuilt_catalog`)
then fetches, SHA-256-verifies, and extracts it to
`<app-data>/catalogs/smac_gaia/` and `…/smac_gaia_bright/`. Override the source
URL for testing with `ATHENAEUM_STAR_CATALOG_URL`.

Redistribution of this derived Gaia subset is permitted with ESA/Gaia/DPAC
credit.

## Verify a built archive locally

```bash
# Inspect the caches
solvemyastro cache-info /Volumes/NAS/catalog_out/smac_gaia
solvemyastro cache-info /Volumes/NAS/catalog_out/smac_gaia_bright

# Round-trip the consumer contract (point the app at the local zip)
ATHENAEUM_STAR_CATALOG_URL="file:///Volumes/NAS/catalog_out/smac_gaia.zip" \
  # …then run download_gaia_dr3_prebuilt_catalog from the app/web backend
```
