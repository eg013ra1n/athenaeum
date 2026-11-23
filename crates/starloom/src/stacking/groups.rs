//! The plain median helper of Athenaeum core's `stacking/groups.rs` (the catalog grouping stays there).

/// The median of `values` (a plain, unweighted list of numbers, sorted in place) — the SAME odd/even
/// convention `weights.rs::median_of` uses, a separate copy rather than a
/// shared one since the two operate on unrelated quantities. `pub`
/// (M4b R-T6-7): `plan.rs::build_plan` reuses this SAME function to derive
/// a group's INCLUDED-only median pixel scale, rather than adding a third
/// copy of the same six lines. An empty slice has no median and panics.
pub fn median_f64(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}
