// Hand-written companion to the generated `FlatNormMode` union in `./models`.
//
// The whole-frame trimmed-mean mode is `FlatNormMode::TrimmedMeanWhole` in Rust.
// Its persisted / over-the-wire spelling is deliberately the pre-rename string
// (stacking configs in the catalog, the saved Export-tab preference, the
// `flatNormMode` argument of the export and send commands), so builds of
// either age read each other's data. The Rust enum accepts the new spelling
// `trimmedMeanWhole` on read as an alias; the frontend only ever sends this one.
// Compare against this constant, never against a string literal.

import type { FlatNormMode } from './models';

// `satisfies` is the compile-time guard: if the Rust wire string ever changes,
// the regenerated `models.ts` union stops accepting this literal and the build
// fails, instead of the comparisons silently never matching.
export const FLAT_NORM_TRIMMED_MEAN_WHOLE = 'pixinsightTrimmed' satisfies FlatNormMode;
