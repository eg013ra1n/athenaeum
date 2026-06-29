# catalog-builder

Offline tool that builds the **density-limited, additive tier** star catalogs the
Athenaeum plate-solver uses, and emits a ready-to-upload `publish/` tree for
`artfrom.space/catalogs/`.

This crate is **build/dev tooling only** — not shipped in the app or the Docker
image (excluded from the workspace there). Design:
[`docs/superpowers/specs/2026-06-29-tiered-additive-star-catalog-design.md`](../../docs/superpowers/specs/2026-06-29-tiered-additive-star-catalog-design.md).

## What it produces

Four **disjoint** density tiers (each a `stars.smac` cache dir) plus a publishable
tree. Each star lives in exactly one tier — zero duplication across download/disk.

| Tier | density (cumulative) | covers FOV ≳ |
| ---- | ---- | ---- |
| `tier_500/` | 500 stars/deg² | 0.6° (wide) |
| `tier_2000/` | 2 000 | 0.3° |
| `tier_5000/` | 5 000 | 0.2° |
| `tier_8000/` | 8 000 | 0.15° (long FL) |

The app downloads only the tiers a given field of view needs (every tier with
`density ≤ target`), merging them at query time.

## Pipeline

```text
1. Acquire  download Gaia DR3 bulk dump (or reuse an existing --gaia-dir)
2. Bin      HEALPix-6 → <work-dir>/catalogs/gaia_dr3/healpix_*.bin  (G<--mag-limit)
3. Tiers    slice each cell into 4 rank bands → tier_<density>/stars.smac
4. Publish  per-tier zip + sha256 + manifest.json → <out>/publish/
```

Each stage is idempotent: a present Gaia file or a populated `gaia_dr3/` bins dir
is detected and reused, so a re-run resumes.

## Usage

```bash
cargo run -p catalog-builder --release -- \
  --gaia-dir /Volumes/NAS/gaia_dr3_bulk \
  --out      /Volumes/NAS/catalog_out
```

Reuse an existing G<21 bin set (skip download + re-bin):

```bash
cargo run -p catalog-builder --release -- \
  --gaia-dir /Volumes/NAS/gaia_dr3_bulk --out /Volumes/NAS/catalog_out \
  --work-dir /path/with/catalogs/gaia_dr3 --skip-download
```

Flags (`--help` for all): `--gaia-dir`, `--work-dir`, `--out`, `--epoch`
(default 2016.0), `--mag-limit` (default 21), `--skip-download`, `--no-zip`,
`--download-concurrency`, `--ingest-concurrency`.

## Publish

`catalog-builder` writes `<out>/publish/` exactly as the maintainer uploads it to
`artfrom.space/catalogs/`:

```text
manifest.json
tier_500.zip    tier_500.zip.sha256
tier_2000.zip   tier_2000.zip.sha256
tier_5000.zip   tier_5000.zip.sha256
tier_8000.zip   tier_8000.zip.sha256
```

Each `tier_<d>.zip` contains `tier_<d>/stars.smac`. The in-app downloader reads
`manifest.json`, SHA-256-verifies, and extracts each tier to
`<app-data>/catalogs/smac_gaia/tier_<d>/stars.smac`. Override the source base URL
with `ATHENAEUM_CATALOG_BASE_URL`.

Redistribution of this derived Gaia subset is permitted with ESA/Gaia/DPAC credit.

## Verify a built archive locally

```bash
solvemyastro cache-info /Volumes/NAS/catalog_out/tier_500
solvemyastro cache-info /Volumes/NAS/catalog_out/tier_8000
```
