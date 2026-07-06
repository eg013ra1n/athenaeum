# Master Integration — PixInsight-Model Recipes — Design — 2026-07-06

Owner request (B5 UI round follow-up): restructure master-build integration
around PixInsight's two-axis model — **Combination** × **Rejection
algorithm** — matching the ImageIntegration panel's mental model, and add
the two missing practical rejection algorithms. Builds on Phase 2
(`integration/combine.rs`, `api/masters.rs` recipes, `CreateMasterDialog`).

Deferred explicitly (owner-approved): ESD, RCR, large-scale pixel
rejection, min/max combination.

## 1. Model

```
IntegrationRecipe {
    combination: Combination,        // Average | Median   (of survivors)
    rejection:   Rejection,          // what gets excluded first
}
Rejection = None
          | PercentileClip   { low, high }        // existing algorithm
          | SigmaClip        { sigma_low, sigma_high }   // NEW
          | WinsorizedSigma  { sigma_low, sigma_high }   // existing
          | LinearFitClip    { sigma_low, sigma_high }   // NEW
```

- `Average` = mean of surviving samples (PI terminology for our `Mean`).
- Rejection runs per pixel stack first; combination applies to survivors.
  Every rejection composes with either combination (PI semantics).
- Equivalences with the legacy flat enum: `Mean` = Average+None; `Median` =
  Median+None; `WinsorizedSigmaClip{..}` = Average+WinsorizedSigma;
  `PercentileClip{..}` = Average+PercentileClip.

**New algorithms** (per-pixel stack, iterate to convergence, ≤ a fixed
iteration cap to bound worst-case):

- `SigmaClip`: mean m and stddev σ of the current survivor set; reject
  samples `< m − sigma_low·σ` or `> m + sigma_high·σ`; repeat until no
  rejection. Plain (non-winsorized) variant.
- `LinearFitClip`: sort the stack; least-squares fit a line over
  (index, value); residual dispersion = mean absolute deviation of
  residuals; reject samples with residual `< −sigma_low·d` or
  `> +sigma_high·d`; refit and repeat until stable. PI-recommended for
  larger sets with drifting illumination.

**Defaults** (PI panel values): sigma 4.0 / 3.0; linear fit 5.0 / 3.5;
percentile 0.2 / 0.1 (dialog defaults; Auto keeps its own tuned values
below).

## 2. Auto resolution (unchanged behavior, new shape)

`resolve_recipe(explicit, imagetyp, n)`:
- explicit `Some(recipe)` wins;
- n ≥ 15 → Average + WinsorizedSigma{3.0, 3.0};
- flat with n < 15 → Average + PercentileClip{0.2, 0.02};
- else → Median + None.

## 3. Compatibility

- `MasterRecipe.combine` becomes `Option<IntegrationRecipe>` (field name
  kept). ts_export regenerated; the dialog is the only producer.
- `master_provenance.recipe_json` stores the RESOLVED `IntegrationRecipe`
  going forward. Old rows (legacy `CombineMethod` JSON) remain: readers
  (provenance dialog / describe strings) parse new-shape first, fall back
  to legacy-shape, else show the raw JSON. Nothing replays recipe_json
  (rebuild resolves fresh Auto — Phase 2 invariant), so no migration.
- Engine entry points (`integrate_bias_like`, `integrate_flat`) take
  `IntegrationRecipe`; the banded runner gains the rejection/combination
  split internally. Existing tests keep passing with mapped equivalents.

## 4. UI (CreateMasterDialog)

Replaces the single combine dropdown:
- **Combination**: Auto (recommended) | Average | Median.
- **Rejection algorithm** (hidden when Auto): No rejection | Percentile
  clipping | Sigma clipping | Winsorized sigma clipping | Linear fit
  clipping — with the matching parameter inputs shown underneath
  (percentile low/high; sigma low/high; linear-fit low/high), PI defaults
  pre-filled, persisted per last use.
- Preview/`formatCombine` strings switch to "Average · Winsorized sigma
  (3.0/3.0)" style; Auto's description keeps showing the resolved recipe.

## 5. Testing

- Exactness per new algorithm on synthetic stacks (hot outlier rejected by
  SigmaClip at 3σ but kept at 10σ; LinearFitClip keeps a clean linear ramp
  intact and rejects a spike; iteration cap terminates on adversarial
  stacks).
- Median-of-survivors composition (Median + SigmaClip) sanity test.
- Legacy-equivalence pins: Average+None ≡ old Mean, Average+WinsorizedSigma
  ≡ old WinsorizedSigmaClip byte-for-byte on a fixture stack.
- Legacy provenance JSON still renders (fallback parse test).
- Real-data check via the existing e2e harness path (rebuild a master with
  an explicit Average+SigmaClip recipe on sandbox copies).
