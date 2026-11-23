//! The plain median helper the stacking stages share; grouping frames is the caller's job.

/// The median of `values`, a plain unweighted list of numbers that is sorted in place: the middle value for an odd
/// count, the mean of the two middle values for an even count. The caller uses it for a group's median pixel scale over
/// its included frames. An empty slice has no median and panics.
pub fn median_f64(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}
