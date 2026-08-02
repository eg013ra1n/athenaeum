# OSC / CFA Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every finding of the 2026-08-02 OSC/CFA review (`docs/superpowers/research/2026-08-02-osc-cfa-review.md`): Bayer metadata becomes trustworthy on every output, per-CFA-channel flat scaling reaches tool parity, CFA incompatibilities become visible, and the CFA path gets real mosaic tests.

**Architecture:** Three packages. P1 — Bayer metadata integrity (parse offsets/ROWORDER into the catalog, stop fabricating values, deterministic member-consensus master cards, XISF `<ColorFilterArray>` support, warn on every silent drop, fix the blob↔frames drift). P2 — per-CFA-channel flat scaling (channel math utils, per-channel constants stamped on CFA master flats, a light-cal scaling mode default-ON for CFA lights, PI-parity). P3 — advisory compatibility warnings + end-to-end CFA tests + doc truth-ups.

**Tech Stack:** Rust (athenaeum-core + thin Tauri/Axum wrappers), rusqlite, React/TS dialog toggle.

## Global Constraints

- Two backends in sync for any surface change; serde camelCase on new wire fields; new model types → `ts_export.rs`.
- Never swallow errors: `tracing::warn!`/`error!` before every degraded path; message = short stable phrase, data in snake_case fields.
- New log field names require the audit-doc dictionary note (same pattern as `covered`/`uncovered`).
- No third-party tool names in code/comments ("established stacking tools" phrasing).
- Existing files are rustfmt-non-conformant — match surrounding style; new files rustfmt-clean.
- Commit as `eg013ra1n <vilen.sharifov@gmail.com>`, one commit per task, on the active version branch. Never Claude as author/co-author.
- Gates per task: named `cargo test -p athenaeum-core --lib <module>` filters; full gates before merge: `cargo build --workspace`, `cargo test -p athenaeum-core --lib`, `npx tsc --noEmit`.
- Schema changes: guarded `ALTER TABLE` + `column_exists` pattern only (mirror `bayerpat`, `db/schema.rs:1026-1038`).
- Design decision (ratified by tool parity, owner-vetoable): per-channel flat scaling defaults **ON** when the light carries a Bayer pattern; CentralThird mode only in v1 (PixinsightTrimmed stays global — documented follow-up). Compatibility checks are **advisory, never blocking**.
- Line numbers below are from HEAD 0581b5e4 — locate by content if drifted.

---

## Package 1 — Bayer metadata integrity

### Task 1: Parse XBAYROFF / YBAYROFF / ROWORDER into the catalog

**Files:**
- Modify: `crates/athenaeum-core/src/models.rs` (Frame struct, next to `bayerpat` ~:90)
- Modify: `crates/athenaeum-core/src/fits_parser/mod.rs` (FITS ~:275/:438; XISF keyword path ~:589/:782)
- Modify: `crates/athenaeum-core/src/db/schema.rs` (three guarded ALTERs next to the bayerpat one ~:1026)
- Modify: `crates/athenaeum-core/src/db/operations.rs` (insert_frame column list ~:255/:291)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` (`write_reparse_rows` ~:1249-1290 — add the three params)

**Interfaces:**
- Produces (Tasks 3/8/9 consume): `Frame { …, bayerpat: Option<String>, xbayroff: Option<i64>, ybayroff: Option<i64>, roworder: Option<String> }`; columns `frames.xbayroff INTEGER`, `frames.ybayroff INTEGER`, `frames.roworder TEXT`.

- [ ] **Step 1: Failing parser test** (fits_parser tests, model on the existing bayerpat fixture):

```rust
#[test]
fn bayer_offsets_and_roworder_are_parsed() {
    let header = fixture_header_with_cards(&[
        ("BAYERPAT", "'RGGB    '"),
        ("XBAYROFF", "1"),
        ("YBAYROFF", "0"),
        ("ROWORDER", "'TOP-DOWN'"),
    ]);
    let frame = parse_frame_from(header);
    assert_eq!(frame.bayerpat.as_deref(), Some("RGGB"));
    assert_eq!(frame.xbayroff, Some(1));
    assert_eq!(frame.ybayroff, Some(0));
    assert_eq!(frame.roworder.as_deref(), Some("TOP-DOWN"));
}
```

(Adapt fixture construction to the module's existing test helpers; also add the XISF-keyword-path variant.)

- [ ] **Step 2: Run — FAIL.** **Step 3: Implement** — FITS: `header.get_int("XBAYROFF")` / `get_int("YBAYROFF")` / `get_str("ROWORDER")` next to the BAYERPAT read; XISF: same three from `fits_keywords` (parse ints with `.and_then(|s| s.trim().parse().ok())`). Model fields with doc comments ("CFA phase offsets / row order; None when the source doesn't declare them — never fabricate 0").

- [ ] **Step 4: Schema + writes.** Three guarded ALTERs (copy the bayerpat block verbatim, one per column). Extend `insert_frame`'s column list + params, and `write_reparse_rows`' UPDATE + params. Round-trip test in db tests: insert a frame with offsets → SELECT them back.

- [ ] **Step 5:** `cargo test -p athenaeum-core --lib fits_parser operations` green; commit — `feat(calibration): parse Bayer offsets and row order into the catalog`

### Task 2: XISF native `<ColorFilterArray>` support

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/mod.rs` (XISF header parse — where FITSKeyword map is built ~:589)

- [ ] **Step 1: Failing test:** XISF XML fixture containing `<ColorFilterArray pattern="RGGB" width="2" height="2" name="RGGB"/>` and NO BAYERPAT FITSKeyword → `frame.bayerpat == Some("RGGB")`.
- [ ] **Step 2: Implement:** after the FITSKeyword lookup, when `bayerpat.is_none()`, scan the XISF XML for the `ColorFilterArray` element's `pattern` attribute (same lightweight string/attribute extraction style the XISF parser already uses — no new XML dependency) and validate it against `^[RGB]+$` before adopting. FITSKeyword BAYERPAT keeps precedence.
- [ ] **Step 3:** test green; also add a negative test (garbage pattern attribute → None + `warn!(path, pattern, "unrecognized xisf cfa pattern")`). Commit — `feat(calibration): read CFA pattern from XISF ColorFilterArray element`

### Task 3: Master Bayer cards — real values, consensus, no silent drops

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/headers.rs` (`load_header_inputs` ~:94-114, inputs struct ~:125-132, bayer emit ~:184-195, false comments :72-73/:87)
- Modify: `crates/athenaeum-core/src/fits_writer/keywords.rs` (wire `roworder_top_down`-style emit — generalize to `roworder(value)`)

**Interfaces:**
- Consumes: Task 1's `frames.{bayerpat,xbayroff,ybayroff,roworder}` columns.
- Produces: masters emit BAYERPAT + REAL XBAYROFF/YBAYROFF + ROWORDER, all by member consensus; every degraded path warns.

- [ ] **Step 1: Failing tests** (headers.rs tests — replace the `.is_some()`-only assertions):

```rust
#[test]
fn master_bayer_cards_carry_real_offsets_and_roworder() {
    // members: 3 frames, all bayerpat=RGGB, xbayroff=1, ybayroff=0, roworder=BOTTOM-UP
    let cards = build_cards_for_fixture_set();
    assert_card_str(&cards, "BAYERPAT", "RGGB");
    assert_card_int(&cards, "XBAYROFF", 1);
    assert_card_int(&cards, "YBAYROFF", 0);
    assert_card_str(&cards, "ROWORDER", "BOTTOM-UP");
}

#[test]
fn member_disagreement_warns_and_uses_majority() {
    // members: RGGB, RGGB, BGGR → BAYERPAT=RGGB emitted; disagreement warned
    // (assert via the returned consensus struct — log assertion optional)
}

#[test]
fn missing_bayer_data_emits_no_bayer_cards_and_no_zero_fabrication() {
    // members with bayerpat=None → no BAYERPAT/XBAYROFF/YBAYROFF/ROWORDER cards at all
}
```

- [ ] **Step 2: Implement.** Replace the blob-`LIMIT 1` route with a consensus query over the member frames' columns:

```rust
// Majority value per column, deterministic (count DESC, value ASC); NULLs excluded.
fn consensus_text(conn: &Connection, set_id: i64, col: &str) -> Result<(Option<String>, bool)> {
    // returns (winner, disagreement_present)
    let mut stmt = conn.prepare(&format!(
        "SELECT fr.{col}, COUNT(*) c FROM calibration_set_frames csf
           JOIN frames fr ON fr.id = csf.frame_id
          WHERE csf.set_id = ?1 AND fr.{col} IS NOT NULL
          GROUP BY fr.{col} ORDER BY c DESC, fr.{col} ASC"
    ))?;
    let rows: Vec<String> = stmt.query_map([set_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok((rows.first().cloned(), rows.len() > 1))
}
```

(`col` comes from a fixed internal list — bayerpat/roworder/xbayroff/ybayroff — never user input; offsets use the integer variant.) Disagreement → `warn!(set_id, field = col, "bayer metadata disagrees across set members — using majority")`. Emit XBAYROFF/YBAYROFF ONLY when a consensus value exists (`if let Some(x) = xoff` — delete the `.unwrap_or(0)` fabrication; when the pattern exists but offsets are absent, emit BAYERPAT alone — absent beats fabricated). Unknown pattern string → `warn!(set_id, bayerpat = %p, "unrecognized bayer pattern — bayer cards omitted from master")` in the `_` arm. Emit ROWORDER via a generalized `HeaderBuilder::roworder(value: &str)` (keep `roworder_top_down` delegating to it). Fix both false "frames has no bayerpat column" comments. Keep BAYERPAT-from-blob as a fallback ONLY when the column consensus is empty (pre-Task-1 catalogs that haven't rescanned), with a `debug!` naming the fallback.

- [ ] **Step 3:** `cargo test -p athenaeum-core --lib headers` green (update the three blob-shape tests to keep passing via the fallback path); commit — `fix(calibration): master bayer cards use member consensus — real offsets, roworder, no fabricated zeros`

### Task 4: Light-cal copy-through completeness + silent-drop warns

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/light_headers.rs` (whitelist ~:40)
- Modify: `crates/athenaeum-core/src/api/lights.rs` (missing-blob path ~:688-690)
- Modify: `crates/athenaeum-core/src/integration/banded.rs` (channel error message ~:86-91)

- [ ] **Step 1:** Add `"ROWORDER"` to the Bayer group of `COPY_THROUGH_KEYWORDS`; extend the existing `bayer_cards_copied_through` test to assert it.
- [ ] **Step 2:** Missing-blob path gets a warn before the empty return:

```rust
let Some(header_text) = header_text else {
    tracing::warn!(file_id, "no stored header for light — calibrated output will carry no copied-through cards");
    return Ok(Vec::new());
};
```

- [ ] **Step 3:** Neutral channel error (it fires for lights too): `"{}: {}-channel image — frames must be 1-channel for calibration (CFA mosaics stay 1-channel; debayered files cannot be calibrated)"`. Update any test pinning the old string.
- [ ] **Step 4:** `cargo test -p athenaeum-core --lib light_headers lights integration` green; commit — `fix(lights): roworder copy-through, warn on missing header blob, honest channel error`

### Task 5: Close the blob↔frames drift (reparse + rebuild)

**Files:**
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` (`write_reparse_rows` ~:1249-1290)
- Modify: `crates/athenaeum-core/src/api/masters.rs` (Rebuild arm ~:1118-1135)

- [ ] **Step 1: Verify the contract first.** CLAUDE.md documents reparse as updating `files`/`frames`/`fits_header` in one transaction; the code updates only the first two. Confirm by reading `write_reparse_rows` end-to-end; if a deliberate reason for skipping the blob is documented anywhere, STOP and report (NEEDS_CONTEXT) instead of changing behavior.
- [ ] **Step 2: Failing test:** reparse a file whose on-disk BAYERPAT changed → both `frames.bayerpat` AND the `fits_header` blob reflect the new value (the blob is what light-cal copy-through reads).
- [ ] **Step 3: Implement:** add `UPDATE fits_header SET header = ? WHERE file_id = ?` (or the module's upsert helper) inside the same reparse transaction, using the freshly parsed header text — the documented contract.
- [ ] **Step 4: Rebuild re-parses.** After the Rebuild arm's atomic replace + `UPDATE files SET size, modified_at`, re-run `parse_fits_with_header` on the written master and UPDATE the master's `frames` row fields (at minimum bayerpat/offsets/roworder — reuse the reparse row-writer if callable) and its `fits_header` blob, preserving ids. Test: rebuild → catalog matches the new file's cards.
- [ ] **Step 5:** `cargo test -p athenaeum-core --lib scanner masters` green; commit — `fix(calibration): reparse and rebuild keep fits_header blob in sync with disk`

---

## Package 2 — Per-CFA-channel flat scaling

### Task 6: CFA channel math utilities

**Files:**
- Create: `crates/athenaeum-core/src/integration/cfa.rs`
- Modify: `crates/athenaeum-core/src/integration/mod.rs` (declare + re-export)

**Interfaces (Tasks 7/8 consume — verbatim):**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfaChannel { R, G, B }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CfaGeometry { pub pattern: Bayer, pub xoff: i64, pub yoff: i64 }

pub fn cfa_channel_at(x: usize, y: usize, g: CfaGeometry) -> CfaChannel;
/// Per-channel means over the central-third window: [R, G, B].
pub fn central_third_channel_means(data: &[f32], w: usize, h: usize, g: CfaGeometry) -> [f64; 3];
```

- [ ] **Step 1: Failing unit tests** — all four patterns × offset (0,0) and (1,0): assert the 2×2 cell mapping (e.g. RGGB at (0,0)=R, (1,0)=G, (0,1)=G, (1,1)=B; with xoff=1 the row shifts); per-channel means on a synthetic 6×6 mosaic with R=2000/G=4000/B=1000 → `[2000.0, 4000.0, 1000.0]` exactly.
- [ ] **Step 2: Implement:**

```rust
pub fn cfa_channel_at(x: usize, y: usize, g: CfaGeometry) -> CfaChannel {
    // 2×2 cell grids, row-major (row 0 = top row of the pattern string).
    const RGGB: [[CfaChannel; 2]; 2] = [[CfaChannel::R, CfaChannel::G], [CfaChannel::G, CfaChannel::B]];
    const BGGR: [[CfaChannel; 2]; 2] = [[CfaChannel::B, CfaChannel::G], [CfaChannel::G, CfaChannel::R]];
    const GBRG: [[CfaChannel; 2]; 2] = [[CfaChannel::G, CfaChannel::B], [CfaChannel::R, CfaChannel::G]];
    const GRBG: [[CfaChannel; 2]; 2] = [[CfaChannel::G, CfaChannel::R], [CfaChannel::B, CfaChannel::G]];
    let grid = match g.pattern { Bayer::Rggb => RGGB, Bayer::Bggr => BGGR, Bayer::Gbrg => GBRG, Bayer::Grbg => GRBG };
    let row = ((y as i64 + g.yoff).rem_euclid(2)) as usize;
    let col = ((x as i64 + g.xoff).rem_euclid(2)) as usize;
    grid[row][col]
}

pub fn central_third_channel_means(data: &[f32], w: usize, h: usize, g: CfaGeometry) -> [f64; 3] {
    let (x0, x1) = (w / 3, (2 * w) / 3);
    let (y0, y1) = (h / 3, (2 * h) / 3);
    let (mut sum, mut n) = ([0f64; 3], [0u64; 3]);
    for y in y0..y1 {
        for x in x0..x1 {
            let c = cfa_channel_at(x, y, g) as usize;
            sum[c] += data[y * w + x] as f64;
            n[c] += 1;
        }
    }
    [0, 1, 2].map(|c| if n[c] == 0 { 0.0 } else { sum[c] / n[c] as f64 })
}
```

(`Bayer` is the existing enum in `fits_writer/keywords.rs` — re-export or import as the module structure prefers; `CfaChannel as usize` requires `#[repr(usize)]` or an explicit `idx()` — implementer's choice, keep it obvious.)

- [ ] **Step 3:** rustfmt the new file; `cargo test -p athenaeum-core --lib cfa` green; commit — `feat(integration): CFA channel geometry + per-channel central-third means`

### Task 7: Stamp per-channel constants on CFA master flats

**Files:**
- Modify: `crates/athenaeum-core/src/api/masters.rs` (flat build path, after integration output where ATH_FNRM flows)
- Modify: `crates/athenaeum-core/src/calibration_library/headers.rs` (card emission next to ATH_FNRM ~:205-210)

**Interfaces:**
- Produces: CFA master flats carry `ATH_FNR`, `ATH_FNG`, `ATH_FNB` (real-valued per-channel central-third means) alongside the existing global `ATH_FNRM`. Mono flats unchanged.

- [ ] **Step 1: Failing test:** build-cards fixture for a flat set whose members are RGGB with offsets (0,0) and a mosaic data plane R=2000/G=4000/B=1000 → cards contain ATH_FNRM (global blend) AND ATH_FNR=2000/ATH_FNG=4000/ATH_FNB=1000; a mono flat set → only ATH_FNRM.
- [ ] **Step 2: Implement:** in the flat build path, when the set's consensus bayer geometry (Task 3's consensus, offsets defaulting to (0,0) ONLY here — for math, not for header emission — with a `debug!` when defaulted) is available, compute `central_third_channel_means(&output.data, w, h, geom)` and pass the three values into the card builder; validate each finite & > 0 (else `warn!` + omit the three cards, keep ATH_FNRM). Emit with the comment "per-channel flat normalization constant".
- [ ] **Step 3:** `cargo test -p athenaeum-core --lib headers masters` green; commit — `feat(calibration): per-channel flat normalization constants stamped on CFA master flats`

### Task 8: Per-channel flat scaling in light calibration (default ON for CFA)

**Files:**
- Modify: `crates/athenaeum-core/src/models.rs` (`LightCalParams` ~:1181 — new field), `db/schema.rs` (guarded ALTER `light_calibrations ADD COLUMN cfa_scaling_applied INTEGER`), `db/light_calibrations.rs` (row write + `derive_status` arm)
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs` (divisor becomes an enum; pixel loop; per-channel resolution + on-the-fly recompute)
- Modify: `crates/athenaeum-core/src/calibration_library/light_headers.rs` + `api/lights.rs` (ATH_CFNR/ATH_CFNG/ATH_CFNB + ATH_CCFA cards; param plumbing; readiness surface)
- Modify: `src/components/calibration/CalibrateLightsDialog.tsx` + calibration TS types (toggle)

**Interfaces:**
- `LightCalParams { …, #[serde(default = "default_true")] cfa_flat_scaling: bool }` (wire-compatible; both backends untouched — the params struct already travels).
- Engine: `enum FlatNormDivisor { Global(f64), PerChannel { geom: CfaGeometry, k: [f64; 3] } }` replacing the scalar `flat_norm_divisor` inside `calibrate_light`; `LightCalOutcome.flat_norm_divisor` keeps the global value (mean of k in per-channel mode) for row compatibility.

- [ ] **Step 1: Failing engine test (the tool-parity pin):**

```rust
#[test]
fn per_channel_scaling_preserves_the_lights_channel_ratios() {
    // Light: RGGB mosaic R=1000/G=2000/B=500. Flat: same mosaic R=2000/G=4000/B=1000
    // (i.e. the flat carries a strong color; vignette-free).
    // Per-channel ON: every channel divides by its own normalized-to-1 flat
    //   → output ratios == light ratios (R:G:B = 2:4:1 pre-scale-divisor), bit-checked per pixel.
    // Per-channel OFF (global): output ratios are flat-white-balanced
    //   → R and B pixels shifted by the flat's channel response; pin the exact expected values.
}
```

Both arms bit-exact via an independent f64 helper (follow the module's `expect_px` pattern).

- [ ] **Step 2: Implement the engine.** Resolution order in `calibrate_light` when `flat_norm && flat.is_some() && inputs.cfa_geometry.is_some()` (a new `Option<CfaGeometry>` on `LightCalInputs`, set by the orchestrator from the LIGHT frame's bayerpat/offsets — offsets default (0,0) with `debug!` when absent):
  1. Read ATH_FNR/ATH_FNG/ATH_FNB from the master flat (same reader style as `read_ath_fnrm`); each must be finite & > 0, else
  2. recompute `central_third_channel_means` over the flat plane on the fly (imported flats),
  3. degenerate channel (≤ 0/non-finite after recompute) → `warn!` + fall back to Global for the whole frame (never mixed-mode within a frame).
  Pixel loop: `let k = match &divisor { Global(k) => *k, PerChannel { geom, k } => k[cfa_channel_at(x, y, *geom) as usize] };` — the existing `FLAT_DENOM_FLOOR` branch operates on `band_bufs[fi][idx] as f64 / k` unchanged. NOTE: the loop is indexed by flat `idx` within a band — derive `(x, y)` as `(idx % w, y0 + idx / w)` with the band's global row offset, exactly like the precal row-index handling (see the pinned `multi_band_precal_uses_global_row_index` pattern).
- [ ] **Step 3: Cards + row + staleness.** Per-channel mode stamps ATH_CFNR/ATH_CFNG/ATH_CFNB (applied constants), ATH_CCFA logical T, and keeps ATH_CFNM (global-equivalent mean of k) for continuity. Row: `cfa_scaling_applied` 0/1. `derive_status`: mismatch between the row's flag and the current `cfa_flat_scaling` param counts as *stale* ONLY when a flat was applied AND the frame has a bayerpat (mirror the existing flat-norm-toggle arm — no engine-version bump). Tests for the staleness arm both ways + the mono-frame exemption.
- [ ] **Step 4: PixinsightTrimmed stays global** — in that mode `cfa_flat_scaling` is ignored with a `debug!`; document on the param.
- [ ] **Step 5: UI.** `CalibrateLightsDialog.tsx`: checkbox "Per-channel CFA flat scaling" (default checked, help text "applies to color (CFA) lights; mono lights are unaffected"), disabled when flat-norm mode is PixInsight-trimmed; param flows through the existing start call. TS type updated; `npx tsc --noEmit` clean.
- [ ] **Step 6:** `cargo test -p athenaeum-core --lib light_cal lights light_calibrations` green + tsc; commit — `feat(lights): per-CFA-channel flat scaling, default on for color lights`

---

## Package 3 — Visibility + tests + truth-ups

### Task 9: Advisory CFA-compatibility warnings

**Files:**
- Modify: `crates/athenaeum-core/src/api/lights.rs` (readiness + per-frame calibrate path)
- Modify: TS readiness type + `CalibrateLightsDialog.tsx` (render the advisory list)

**Interfaces:**
- Readiness payload gains `cfaWarnings: Vec<String>` (camelCase; empty = clean). Never blocks.

- [ ] **Step 1: Failing test:** readiness for a set whose OSC light (bayerpat RGGB) links to a flat master whose member frame has `bayerpat = NULL` → one warning "flat master has no CFA pattern (mono?) while lights are RGGB"; matching CFA both sides → empty; differing offsets (RGGB/(1,0) vs RGGB/(0,0)) → phase warning.
- [ ] **Step 2: Implement:** during readiness resolution, for each applied master (dark/flat/bias) fetch its member frame's bayerpat/xbayroff/ybayroff (one query via calibration_set_frames→frames) and compare against the light's; build human strings; also `warn!(set_id, frame_id, kind, "cfa mismatch between light and master")` at calibrate time per frame (once per frame, not per pixel). Advisory only — no behavior change.
- [ ] **Step 3: UI:** readiness dialog renders `cfaWarnings` in the existing warning style (tokens: `text-warning`).
- [ ] **Step 4:** gates + tsc; commit — `feat(lights): advisory CFA compatibility warnings in readiness and calibrate`

### Task 10: End-to-end CFA tests, doc truth-ups, follow-up ledger

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs` + `tests/` (e2e additions)
- Modify: `CLAUDE.md` (B5 OSC sentence), `docs/superpowers/research/2026-08-02-calibration-audit.md` (dictionary + deferred list), `docs/superpowers/research/2026-08-02-osc-cfa-review.md` (status line)

- [ ] **Step 1: E2E CFA batch:** (a) full light-cal run on an RGGB fixture asserting the OUTPUT FITS carries BAYERPAT/XBAYROFF/YBAYROFF/ROWORDER cards verbatim from the source (closes the "no end-to-end Bayer pin" gap); (b) master flat build from mosaic subs → written file carries consensus Bayer cards with REAL offset values (pin the integers, not `.is_some()`); (c) XISF ColorFilterArray source → calibrated output carries BAYERPAT.
- [ ] **Step 2: CLAUDE.md:** rewrite the B5 OSC sentence: un-debayered CFA path; BAYERPAT/XBAYROFF/YBAYROFF/ROWORDER copied through; per-CFA-channel flat scaling option (default on for color lights, CentralThird mode; global scalar otherwise); masters carry consensus Bayer cards + per-channel ATH_FNR/G/B.
- [ ] **Step 3: Audit-doc bookkeeping:** dictionary note for any new log fields introduced by this cycle; deferred list additions: PixinsightTrimmed per-channel variant; Flat Analysis contour plot on CFA (display-only); matcher-level bayerpat parameter (needs set-level denormalization); mono-flat-on-OSC hard-block decision (currently advisory).
- [ ] **Step 4:** full gates (`cargo build --workspace`, `cargo test -p athenaeum-core --lib`, `npx tsc --noEmit`); commit — `test(calibration): end-to-end CFA coverage; docs truth-up`

---

## Final gates (before merge)

- [ ] `cargo build --workspace` · `cargo test -p athenaeum-core --lib` · `npx tsc --noEmit`
- [ ] Owner smoke: build a master flat from real OSC subs → inspect Bayer cards (real offsets, ROWORDER, ATH_FNR/G/B); calibrate real OSC lights with scaling ON vs OFF → visually compare color cast; readiness on a deliberate mono-flat link → advisory shows; rescan after an external header edit → calibrated output picks up the new BAYERPAT (blob sync).
- [ ] Release-note lines: per-channel CFA flat scaling (new, default on for color); trustworthy Bayer metadata on masters and calibrated lights; XISF CFA support.
