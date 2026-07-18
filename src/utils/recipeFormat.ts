import type { IntegrationRecipe } from '../types/models';

// Shared formatting for the two-axis integration recipe (spec §4). Kept in one
// place so the Create-Master dialog (live preview / batch rows) and the master
// provenance display render identical, Rust-`describe`-matching strings.

// Byte-identical to `fmt_param` in crates/athenaeum-core/src/integration/combine.rs:
// integer thresholds render with a trailing `.0` (`3` → `"3.0"`, matching spec §4's
// `(3.0/3.0)` style) while fractional ones keep their precision (`0.02` → `"0.02"`).
// Must stay byte-identical to the Rust `describe` output.
export function fmtParam(x: number): string {
  return Number.isInteger(x) ? x.toFixed(1) : String(x);
}

// "Average | Winsorized sigma (3.0/3.0)" — mirrors `IntegrationRecipe::describe()`
// / `Rejection::label()` in combine.rs. ASCII-only separator: the Rust side
// writes this string into the ATH_REJ FITS card (printable-ASCII values only).
export function formatCombine(r: IntegrationRecipe): string {
  const comb = r.combination === 'average' ? 'Average' : 'Median';
  const rej = r.rejection;
  let rejLabel: string;
  switch (rej.method) {
    case 'none': rejLabel = 'no rejection'; break;
    case 'percentile_clip': rejLabel = `Percentile clip (${fmtParam(rej.low)}/${fmtParam(rej.high)})`; break;
    case 'sigma_clip': rejLabel = `Sigma clip (${fmtParam(rej.sigma_low)}/${fmtParam(rej.sigma_high)})`; break;
    case 'winsorized_sigma': rejLabel = `Winsorized sigma (${fmtParam(rej.sigma_low)}/${fmtParam(rej.sigma_high)})`; break;
    case 'linear_fit_clip': rejLabel = `Linear fit clip (${fmtParam(rej.sigma_low)}/${fmtParam(rej.sigma_high)})`; break;
  }
  return `${comb} | ${rejLabel}`;
}

// Narrow an unknown parsed value to a new-shape `IntegrationRecipe`.
function isIntegrationRecipe(v: unknown): v is IntegrationRecipe {
  if (typeof v !== 'object' || v === null) return false;
  const r = v as { combination?: unknown; rejection?: unknown };
  const combOk = r.combination === 'average' || r.combination === 'median';
  const rej = r.rejection as { method?: unknown } | undefined;
  const rejOk = typeof rej === 'object' && rej !== null && typeof rej.method === 'string';
  return combOk && rejOk;
}

// Human-readable render of a `master_provenance.recipe_json` blob for the
// provenance display (spec §3 reader). The blob wraps the resolved recipe under
// a `combine` key (see api/masters.rs). A new-shape `IntegrationRecipe` renders
// via the shared `formatCombine`; anything else — legacy `CombineMethod` blobs,
// or a hand-edited / foreign JSON — falls back to the raw string unchanged
// (mirroring the backend `describe_recipe_json` raw fallback). Never throws.
export function describeRecipeJson(recipeJson: string): string {
  try {
    const combine = (JSON.parse(recipeJson) as { combine?: unknown })?.combine;
    if (isIntegrationRecipe(combine)) return formatCombine(combine);
  } catch {
    /* not JSON — fall through to raw */
  }
  return recipeJson;
}
