//! Symmetric 9×9 eigen-decomposition by cyclic Jacobi rotations — enough to
//! extract the null vector of a normalized-DLT design matrix's Gram matrix
//! without a linear-algebra dependency. 9×9 is the only size the crate
//! needs; the loop is written for `N` but instantiated once.

const N: usize = 9;

/// The unit eigenvector belonging to the smallest eigenvalue of the
/// symmetric matrix `a`. Converges to machine precision in a few sweeps for
/// well-conditioned input; the sweep cap only bounds pathological cases.
pub fn smallest_eigenvector_sym(a: &[[f64; N]; N]) -> [f64; N] {
    let mut m = *a;
    let mut v = [[0f64; N]; N];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for _sweep in 0..100 {
        let mut off = 0.0;
        for i in 0..N {
            for j in (i + 1)..N {
                off += m[i][j] * m[i][j];
            }
        }
        if off < 1e-30 {
            break;
        }
        for p in 0..N {
            for q in (p + 1)..N {
                if m[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (m[q][q] - m[p][p]) / (2.0 * m[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..N {
                    let mkp = m[k][p];
                    let mkq = m[k][q];
                    m[k][p] = c * mkp - s * mkq;
                    m[k][q] = s * mkp + c * mkq;
                }
                for k in 0..N {
                    let mpk = m[p][k];
                    let mqk = m[q][k];
                    m[p][k] = c * mpk - s * mqk;
                    m[q][k] = s * mpk + c * mqk;
                }
                for k in 0..N {
                    let vkp = v[k][p];
                    let vkq = v[k][q];
                    v[k][p] = c * vkp - s * vkq;
                    v[k][q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut best = 0;
    for i in 1..N {
        if m[i][i] < m[best][best] {
            best = i;
        }
    }
    let mut out = [0f64; N];
    for k in 0..N {
        out[k] = v[k][best];
    }
    let norm: f64 = out.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm > 0.0 {
        for x in out.iter_mut() {
            *x /= norm;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smallest_eigenvector_of_a_diagonal_matrix_is_its_smallest_axis() {
        let mut a = [[0f64; 9]; 9];
        for (i, row) in a.iter_mut().enumerate() {
            row[i] = (i as f64 + 1.0) * 10.0;
        }
        a[4][4] = 0.5; // the smallest eigenvalue sits on axis 4
        let v = smallest_eigenvector_sym(&a);
        let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() < 1e-9);
        assert!(v[4].abs() > 0.999, "{v:?}");
    }

    #[test]
    fn null_vector_of_a_rank_deficient_gram_matrix() {
        // Rows of A: 8 random-ish vectors orthogonal to n = (1,1,...,1)/3.
        let n: [f64; 9] = [1.0 / 3.0; 9];
        let mut rows: Vec<[f64; 9]> = Vec::new();
        for k in 0..8 {
            let mut r = [0f64; 9];
            r[k] = 1.0;
            r[k + 1] = -1.0; // (e_k - e_{k+1}) is orthogonal to n
            rows.push(r);
        }
        let mut a = [[0f64; 9]; 9];
        for r in &rows {
            for i in 0..9 {
                for j in 0..9 {
                    a[i][j] += r[i] * r[j];
                }
            }
        }
        let v = smallest_eigenvector_sym(&a);
        let dot: f64 = v.iter().zip(n.iter()).map(|(a, b)| a * b).sum();
        assert!(dot.abs() > 0.9999, "null vector must be ±n, got dot {dot}");
    }
}
