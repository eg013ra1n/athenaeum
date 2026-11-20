//! "Did you mean": the existing catalog values nearest to a value that matched nothing.

use super::normalize_value;

/// At most this many values are offered.
const MAX_NEAREST: usize = 5;
/// A value this many single-character edits away still counts as near.
const MAX_EDITS: usize = 3;

/// Levenshtein distance over chars.
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = (diag + usize::from(ca != *cb)).min(row[j] + 1).min(up + 1);
            diag = up;
        }
    }
    row[b.len()]
}

/// The values of `existing` nearest to `given`: one contains the other after normalisation (distance 0), or they
/// are at most three edits apart; at most five, by distance, then by value.
pub fn nearest(existing: &[String], given: &str) -> Vec<String> {
    let g = normalize_value(given);
    if g.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, &String)> = existing
        .iter()
        .filter_map(|v| {
            let n = normalize_value(v);
            if n.is_empty() {
                None
            } else if n.contains(&g) || g.contains(&n) {
                Some((0, v))
            } else {
                let d = levenshtein(&n, &g);
                (d <= MAX_EDITS).then_some((d, v))
            }
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.into_iter().take(MAX_NEAREST).map(|(_, v)| v.clone()).collect()
}
