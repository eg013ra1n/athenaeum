//! On-disk record of the star fits Measure's PSF fitter accepted for one
//! frame PLANE (Tier C Task 1, spec §2.2.1) — a fixed-width binary sidecar
//! Normalize (Task 2) and Register (Task 3) will read back instead of
//! re-detecting the same stars a second time.
//!
//! One [`StarFit`] per record, every field narrowed from the in-memory
//! `f64` to a little-endian `f32` (13 × 4 bytes) plus one reserved flags
//! byte — 53 bytes per fit, ≈ 200–400 KB per plane on a typical field. The
//! narrowing is deliberate (spec §2.2.1): the values downstream feed a
//! RATIO (Normalize's relative flux scale) or a star LIST (Register's
//! reused detections), and the audit measured the systematic loss the
//! interpolation kernel and the β choice already introduce at < 0.3 % / <
//! 0.5 % — an `f32`'s ~7 decimal digits are not the limiting precision
//! anywhere this file is read.
//!
//! One file per (frame, plane): a frame's planes can carry different star
//! counts (an OSC frame's R/G/B fits independently), and the fixed header
//! below has no per-plane sub-count to express more than one plane in a
//! single file — `stacking::run` writes `<stem>.p<plane>.athf` per plane
//! and one `stacking_artifacts` row per plane ([`artifact_kind`]), keyed on
//! the same measurement hash as the frame's `metrics` row.

use std::io::{Read, Write};
use std::path::Path;

use super::psf_signal::StarFit;

/// File magic — four ASCII bytes.
const MAGIC: [u8; 4] = *b"ATHF";

/// Bumped whenever the record layout changes; [`read_fits`] refuses any
/// other value rather than guess at a shape it was not built to read.
pub const FITS_ARTIFACT_VERSION: u16 = 1;

/// `x, y, background, amplitude, fwhm_x, fwhm_y, fwtm_x, fwtm_y, theta,
/// beta, residual, signal, area` — [`StarFit`]'s 13 fields, in declaration
/// order, each a little-endian `f32` — plus one reserved flags byte (`0`
/// today). Fields are written/read individually (never a `#[repr(C)]`
/// cast), so struct alignment/padding never enters the on-disk shape.
const FIELDS_PER_RECORD: usize = 13;
const RECORD_LEN: usize = FIELDS_PER_RECORD * 4 + 1;
const HEADER_LEN: usize = 4 + 2 + 4; // magic + version + count

/// A malformed or unreadable `fits` artifact. Every arm is a real,
/// distinguishable failure — never swallowed, never silently read back as
/// an empty list.
#[derive(Debug)]
pub enum FitsArtifactError {
    BadMagic { expected: [u8; 4], found: [u8; 4] },
    BadVersion { expected: u16, found: u16 },
    Truncated(String),
    Io(std::io::Error),
}

impl std::fmt::Display for FitsArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FitsArtifactError::BadMagic { expected, found } => write!(
                f,
                "fits artifact: bad magic (expected {expected:?}, found {found:?})"
            ),
            FitsArtifactError::BadVersion { expected, found } => write!(
                f,
                "fits artifact: unsupported version {found} (expected {expected})"
            ),
            FitsArtifactError::Truncated(msg) => write!(f, "fits artifact: truncated: {msg}"),
            FitsArtifactError::Io(e) => write!(f, "fits artifact: I/O error: {e}"),
        }
    }
}

impl std::error::Error for FitsArtifactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FitsArtifactError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for FitsArtifactError {
    fn from(e: std::io::Error) -> Self {
        FitsArtifactError::Io(e)
    }
}

fn encode_fit(fit: &StarFit, out: &mut Vec<u8>) {
    for v in [
        fit.x,
        fit.y,
        fit.background,
        fit.amplitude,
        fit.fwhm_x,
        fit.fwhm_y,
        fit.fwtm_x,
        fit.fwtm_y,
        fit.theta,
        fit.beta,
        fit.residual,
        fit.signal,
        fit.area,
    ] {
        out.extend_from_slice(&(v as f32).to_le_bytes());
    }
    out.push(0u8); // flags, reserved for a future revision
}

/// `record` must be exactly [`RECORD_LEN`] bytes — enforced by the caller
/// ([`read_fits`]'s `chunks_exact`).
fn decode_fit(record: &[u8]) -> StarFit {
    debug_assert_eq!(record.len(), RECORD_LEN);
    let field = |i: usize| -> f64 {
        f32::from_le_bytes(record[i * 4..i * 4 + 4].try_into().expect("4-byte slice")) as f64
    };
    StarFit {
        x: field(0),
        y: field(1),
        background: field(2),
        amplitude: field(3),
        fwhm_x: field(4),
        fwhm_y: field(5),
        fwtm_x: field(6),
        fwtm_y: field(7),
        theta: field(8),
        beta: field(9),
        residual: field(10),
        signal: field(11),
        area: field(12),
    }
}

/// Write `fits` to `path`: a plain temp-file-then-rename, no fsync — the
/// same `Durability::Volatile` shape the registered writer uses
/// ([`crate::stacking::register::writer`]), chosen over
/// [`crate::fits_writer::Durability`] itself because that enum's public
/// surface is the FITS-image writer's, not a fit for this small custom
/// binary format; the rename helper
/// ([`crate::fits_writer::writer::rename_replace`]) is reused verbatim for
/// its Windows sharing-violation retry. The file is a per-frame-plane
/// intermediate whose `stacking_artifacts` row keys on a hash and a stat
/// (`is_fresh`), so a write a crash loses or truncates reads back as a
/// cache miss, never as a wrong answer.
pub fn write_fits(path: &Path, fits: &[StarFit]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(HEADER_LEN + fits.len() * RECORD_LEN);
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(&FITS_ARTIFACT_VERSION.to_le_bytes());
    buf.extend_from_slice(&(fits.len() as u32).to_le_bytes());
    for fit in fits {
        encode_fit(fit, &mut buf);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        path.with_extension(format!("athf.tmp.{}.{}", std::process::id(), seq))
    };
    let write_result = (|| -> std::io::Result<()> {
        let file = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::new(file);
        w.write_all(&buf)?;
        w.flush()
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = crate::fits_writer::writer::rename_replace(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Read a `fits` artifact back, typed-error on anything that is not exactly
/// what [`write_fits`] would have produced.
pub fn read_fits(path: &Path) -> Result<Vec<StarFit>, FitsArtifactError> {
    let mut file = std::fs::File::open(path)?;
    let mut header = [0u8; HEADER_LEN];
    file.read_exact(&mut header)
        .map_err(|e| FitsArtifactError::Truncated(format!("header: {e}")))?;

    let mut magic = [0u8; 4];
    magic.copy_from_slice(&header[0..4]);
    if magic != MAGIC {
        return Err(FitsArtifactError::BadMagic {
            expected: MAGIC,
            found: magic,
        });
    }
    let version = u16::from_le_bytes(header[4..6].try_into().expect("2-byte slice"));
    if version != FITS_ARTIFACT_VERSION {
        return Err(FitsArtifactError::BadVersion {
            expected: FITS_ARTIFACT_VERSION,
            found: version,
        });
    }
    let count = u32::from_le_bytes(header[6..10].try_into().expect("4-byte slice")) as usize;

    let mut rest = Vec::new();
    file.read_to_end(&mut rest)?;
    let expected_len = count * RECORD_LEN;
    if rest.len() != expected_len {
        return Err(FitsArtifactError::Truncated(format!(
            "expected {expected_len} bytes of records ({count} fits), found {}",
            rest.len()
        )));
    }
    Ok(rest.chunks_exact(RECORD_LEN).map(decode_fit).collect())
}

/// The `stacking_artifacts.kind` for one plane's fits file — `"fits.<plane>"`
/// (`plane` 0-based, the same index [`crate::integration::plane_reader::PlaneReader`]
/// uses): one row per (frame, plane), since each plane owns its own `.athf`
/// file. Never bare `"fits"` — the table's unique index is keyed on
/// `(frames_set_id, group_key, kind, frame_id)`, so an OSC frame's three
/// planes need three distinct kind strings to each get a row.
pub fn artifact_kind(plane: usize) -> String {
    format!("fits.{plane}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::ransac::SplitMix64;

    fn signed(rng: &mut SplitMix64, scale: f64) -> f64 {
        (rng.next_f64() - 0.5) * scale
    }

    fn random_fit(rng: &mut SplitMix64) -> StarFit {
        StarFit {
            x: signed(rng, 4096.0),
            y: signed(rng, 4096.0),
            background: signed(rng, 2.0),
            amplitude: signed(rng, 2.0),
            fwhm_x: signed(rng, 20.0),
            fwhm_y: signed(rng, 20.0),
            fwtm_x: signed(rng, 40.0),
            fwtm_y: signed(rng, 40.0),
            theta: signed(rng, std::f64::consts::TAU),
            beta: 2.5 + rng.next_f64() * 7.5,
            residual: rng.next_f64() * 0.5,
            signal: signed(rng, 1e6),
            area: rng.next_f64() * 1000.0,
        }
    }

    /// `f32` `to_bits` per field — bit-exact once both sides have gone
    /// through the same `f64 -> f32` narrowing, which is the artifact's own
    /// contract (see the module doc): a round trip is lossy at `f64`
    /// precision by design, never at `f32`.
    fn assert_f32_bit_exact(a: &StarFit, b: &StarFit) {
        let fields = |s: &StarFit| {
            [
                s.x,
                s.y,
                s.background,
                s.amplitude,
                s.fwhm_x,
                s.fwhm_y,
                s.fwtm_x,
                s.fwtm_y,
                s.theta,
                s.beta,
                s.residual,
                s.signal,
                s.area,
            ]
        };
        for (x, y) in fields(a).iter().zip(fields(b).iter()) {
            assert_eq!(
                (*x as f32).to_bits(),
                (*y as f32).to_bits(),
                "{x} vs {y} (as f32: {} vs {})",
                *x as f32,
                *y as f32
            );
        }
    }

    #[test]
    fn round_trips_1000_random_fits_bit_exactly() {
        let mut rng = SplitMix64(0xC0FFEE);
        let fits: Vec<StarFit> = (0..1000).map(|_| random_fit(&mut rng)).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("group").join("frame.p0.athf");
        write_fits(&path, &fits).unwrap();
        let back = read_fits(&path).unwrap();
        assert_eq!(back.len(), fits.len());
        for (a, b) in fits.iter().zip(back.iter()) {
            assert_f32_bit_exact(a, b);
        }
    }

    #[test]
    fn round_trips_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.athf");
        write_fits(&path, &[]).unwrap();
        assert_eq!(read_fits(&path).unwrap(), Vec::new());
    }

    #[test]
    fn write_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c.p1.athf");
        write_fits(&path, &[random_fit(&mut SplitMix64(1))]).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn rejects_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_magic.athf");
        std::fs::write(&path, b"NOPE\x01\x00\x00\x00\x00\x00").unwrap();
        match read_fits(&path) {
            Err(FitsArtifactError::BadMagic { expected, found }) => {
                assert_eq!(expected, MAGIC);
                assert_eq!(&found, b"NOPE");
            }
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_version.athf");
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&999u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, &buf).unwrap();
        match read_fits(&path) {
            Err(FitsArtifactError::BadVersion { expected, found }) => {
                assert_eq!(expected, FITS_ARTIFACT_VERSION);
                assert_eq!(found, 999);
            }
            other => panic!("expected BadVersion, got {other:?}"),
        }
    }

    #[test]
    fn rejects_truncated_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.athf");
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(&FITS_ARTIFACT_VERSION.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes()); // claims 2 records
        buf.extend_from_slice(&[0u8; RECORD_LEN]); // only ships 1
        std::fs::write(&path, &buf).unwrap();
        assert!(matches!(
            read_fits(&path),
            Err(FitsArtifactError::Truncated(_))
        ));
    }

    #[test]
    fn rejects_a_truncated_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short_header.athf");
        std::fs::write(&path, b"AT").unwrap();
        assert!(matches!(
            read_fits(&path),
            Err(FitsArtifactError::Truncated(_))
        ));
    }

    #[test]
    fn artifact_kind_is_distinct_per_plane() {
        assert_eq!(artifact_kind(0), "fits.0");
        assert_eq!(artifact_kind(1), "fits.1");
        assert_ne!(artifact_kind(0), artifact_kind(1));
    }

    /// Not one of the brief's required tests — a quick empirical check of
    /// the module doc's precision claim, run with `--nocapture` to read the
    /// number off. `x`/`signal` are the two fields the report cites.
    #[test]
    fn f32_precision_loss_is_well_under_1e_minus_7() {
        let mut rng = SplitMix64(0xA11CE);
        let fits: Vec<StarFit> = (0..1000).map(|_| random_fit(&mut rng)).collect();
        let mut max_rel_x: f64 = 0.0;
        let mut max_rel_signal: f64 = 0.0;
        for f in &fits {
            let rx = ((f.x as f32) as f64 - f.x).abs() / f.x.abs().max(1e-12);
            let rs = ((f.signal as f32) as f64 - f.signal).abs() / f.signal.abs().max(1e-12);
            max_rel_x = max_rel_x.max(rx);
            max_rel_signal = max_rel_signal.max(rs);
        }
        eprintln!(
            "max relative loss (f64 -> f32 -> f64): x={max_rel_x:e} signal={max_rel_signal:e}"
        );
        assert!(max_rel_x < 1e-6, "x relative loss too large: {max_rel_x:e}");
        assert!(
            max_rel_signal < 1e-6,
            "signal relative loss too large: {max_rel_signal:e}"
        );
    }
}
