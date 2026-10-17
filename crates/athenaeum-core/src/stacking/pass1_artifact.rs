//! The Measure stage's first-pass artifact (Plan A Task 11c, rulings R11c-1 .. R11c-6): every plane's pass-1 records
//! of one calibrated frame, written once to `<working_dir>/<set_slug>/measured/<group_key>/<stem>.athm` and read back
//! by pass 2 and by every later run whose key and source stamp still match. A second pass over the same pixels then
//! costs a read and the fits, never the detection and the verdict pass again.
//!
//! Layout (all integers little-endian), in the style of the `.athf` file ([`super::fits_artifact`]):
//!
//! ```text
//! b"ATHM" | u16 PASS1_ARTIFACT_VERSION | u32 MEASURE_VERSION | u32 head length | u64 xxh3(head) | head
//!         | u64 body length | body | u64 xxh3(every byte before this one)
//! ```
//!
//! The head ([`Pass1Head`]: the source stamp, the geometry, every plane's own beta) and the body ([`Pass1Body`]: the
//! per-plane `PlanePrep` records and the timings) are postcard. postcard writes every f64 as eight raw bytes, so NaN,
//! +-inf and every value round-trip exactly (JSON would refuse the first two, which the records do hold:
//! `SaturationPlane::UNTESTED`, `top3x3 = -inf`, a failed verdict's NaNs). postcard is not self-describing: a layout
//! change of any serialised type changes the bytes without any error on the read side. The golden pin
//! `star_measure::tests::the_plane_prep_encoding_is_pinned` fails on such a change and forces
//! [`PASS1_ARTIFACT_VERSION`] to move; `RefusalReason`'s variant order is part of the format for the same reason.
//! [`PASS1_ARTIFACT_VERSION`] versions the bytes, `MEASURE_VERSION` the numbers (it is in the measurement subtree, so
//! in the key, and in the header as a loud second check). A refused decode is a typed error the caller logs; the
//! caller re-runs pass 1, never mis-decodes.

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::stacking::measure::{FramePass1, MeasureOptions, Pass1Body, Pass1BodyRef};
use crate::star_measure::MEASURE_VERSION;

/// The `stacking_artifacts.kind` of a frame's first-pass artifact.
pub const PASS1_ARTIFACT_KIND: &str = "measured";

/// Bumped whenever the bytes of a serialised pass-1 type change (the golden pin names this constant).
pub const PASS1_ARTIFACT_VERSION: u16 = 1;

const MAGIC: [u8; 4] = *b"ATHM";
/// Magic, version, measure version, head length, head checksum.
pub(crate) const PREFIX_LEN: usize = 4 + 2 + 4 + 4 + 8;
/// The head is a few dozen bytes; a declared length past this is a damaged file, not a head.
const MAX_HEAD_LEN: usize = 1 << 20;

/// The calibrated file's identity at pass 1's first plane read (the read guard's identity, stored): its size and its
/// modification time as signed nanoseconds since the Unix epoch (`None` where the filesystem has none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStamp {
    pub len: u64,
    pub modified_ns: Option<i128>,
}

/// What the controller reads without the body: the stamp, the geometry and every plane's own beta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pass1Head {
    pub source: Option<SourceStamp>,
    pub width: u32,
    pub height: u32,
    pub beta_own: Vec<f64>,
}

/// Why a first-pass artifact is not usable. Every arm is a re-run of pass 1 at the call site, never a measurement.
#[derive(Debug)]
pub enum Pass1ArtifactError {
    BadMagic {
        found: [u8; 4],
    },
    BadVersion {
        found: u16,
    },
    BadMeasureVersion {
        found: u32,
    },
    /// A stored checksum (the head's or the whole file's) does not match its bytes.
    Checksum,
    /// The file is shorter than its own lengths say, or longer, or its parts disagree.
    Truncated(String),
    /// The head describes a different frame geometry or plane count than the calibrated file has now.
    Mismatch(String),
    Decode(postcard::Error),
    /// The calibrated file is not the one the first pass read.
    Stale {
        stored: Option<SourceStamp>,
        now: Option<SourceStamp>,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for Pass1ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pass1ArtifactError::BadMagic { found } => {
                write!(f, "not a first-pass artifact (magic {found:?})")
            }
            Pass1ArtifactError::BadVersion { found } => write!(
                f,
                "first-pass artifact version {found}, this build reads {PASS1_ARTIFACT_VERSION}"
            ),
            Pass1ArtifactError::BadMeasureVersion { found } => write!(
                f,
                "first-pass artifact made at measure version {found}, this build is {MEASURE_VERSION}"
            ),
            Pass1ArtifactError::Checksum => write!(f, "first-pass artifact checksum mismatch"),
            Pass1ArtifactError::Truncated(what) => {
                write!(f, "first-pass artifact is damaged: {what}")
            }
            Pass1ArtifactError::Mismatch(what) => {
                write!(f, "first-pass artifact does not match the calibrated file: {what}")
            }
            Pass1ArtifactError::Decode(e) => write!(f, "first-pass artifact does not decode: {e}"),
            Pass1ArtifactError::Stale { stored, now } => write!(
                f,
                "the calibrated file changed since its first pass was stored ({stored:?} -> {now:?})"
            ),
            Pass1ArtifactError::Io(e) => write!(f, "first-pass artifact i/o: {e}"),
        }
    }
}

impl std::error::Error for Pass1ArtifactError {}

impl From<std::io::Error> for Pass1ArtifactError {
    fn from(e: std::io::Error) -> Self {
        Pass1ArtifactError::Io(e)
    }
}

fn xxh3(bytes: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(bytes)
}

/// The artifact's bytes for `head` and `body` (no I/O).
pub(crate) fn encode(head: &Pass1Head, body: &Pass1BodyRef<'_>) -> std::io::Result<Vec<u8>> {
    let head_bytes = postcard::to_allocvec(head).map_err(std::io::Error::other)?;
    let body_bytes = postcard::to_allocvec(body).map_err(std::io::Error::other)?;
    let mut out = Vec::with_capacity(PREFIX_LEN + head_bytes.len() + 8 + body_bytes.len() + 8);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&PASS1_ARTIFACT_VERSION.to_le_bytes());
    out.extend_from_slice(&MEASURE_VERSION.to_le_bytes());
    out.extend_from_slice(&(head_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&xxh3(&head_bytes).to_le_bytes());
    out.extend_from_slice(&head_bytes);
    out.extend_from_slice(&(body_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&body_bytes);
    let sum = xxh3(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    Ok(out)
}

/// The magic, the versions and the head length of `prefix` (exactly [`PREFIX_LEN`] bytes), and the head checksum.
fn parse_prefix(prefix: &[u8]) -> Result<(usize, u64), Pass1ArtifactError> {
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&prefix[0..4]);
    if magic != MAGIC {
        return Err(Pass1ArtifactError::BadMagic { found: magic });
    }
    let version = u16::from_le_bytes(prefix[4..6].try_into().expect("2-byte slice"));
    if version != PASS1_ARTIFACT_VERSION {
        return Err(Pass1ArtifactError::BadVersion { found: version });
    }
    let measure = u32::from_le_bytes(prefix[6..10].try_into().expect("4-byte slice"));
    if measure != MEASURE_VERSION {
        return Err(Pass1ArtifactError::BadMeasureVersion { found: measure });
    }
    let head_len = u32::from_le_bytes(prefix[10..14].try_into().expect("4-byte slice")) as usize;
    let head_sum = u64::from_le_bytes(prefix[14..22].try_into().expect("8-byte slice"));
    if head_len > MAX_HEAD_LEN {
        return Err(Pass1ArtifactError::Truncated(format!(
            "head length {head_len}"
        )));
    }
    Ok((head_len, head_sum))
}

fn decode_head(head_bytes: &[u8], head_sum: u64) -> Result<Pass1Head, Pass1ArtifactError> {
    if xxh3(head_bytes) != head_sum {
        return Err(Pass1ArtifactError::Checksum);
    }
    let (head, rest) =
        postcard::take_from_bytes::<Pass1Head>(head_bytes).map_err(Pass1ArtifactError::Decode)?;
    if !rest.is_empty() {
        return Err(Pass1ArtifactError::Truncated(format!(
            "{} bytes after the head",
            rest.len()
        )));
    }
    Ok(head)
}

/// The head and the body of `bytes` (no stamp check): the versions, every length, the head's and the whole file's
/// checksum. The body is decoded only after the whole-file checksum passed.
pub(crate) fn decode(bytes: &[u8]) -> Result<(Pass1Head, Pass1Body), Pass1ArtifactError> {
    if bytes.len() < PREFIX_LEN {
        return Err(Pass1ArtifactError::Truncated(format!(
            "{} bytes, the header alone is {PREFIX_LEN}",
            bytes.len()
        )));
    }
    let (head_len, head_sum) = parse_prefix(&bytes[..PREFIX_LEN])?;
    let head_end = PREFIX_LEN + head_len;
    if bytes.len() < head_end + 8 {
        return Err(Pass1ArtifactError::Truncated("the head".to_string()));
    }
    let head = decode_head(&bytes[PREFIX_LEN..head_end], head_sum)?;
    let body_len = u64::from_le_bytes(
        bytes[head_end..head_end + 8]
            .try_into()
            .expect("8-byte slice"),
    );
    let body_start = head_end + 8;
    let expected = (body_start as u64)
        .checked_add(body_len)
        .and_then(|n| n.checked_add(8));
    if expected != Some(bytes.len() as u64) {
        return Err(Pass1ArtifactError::Truncated(format!(
            "{} bytes, the lengths say {expected:?}",
            bytes.len()
        )));
    }
    let sum_at = bytes.len() - 8;
    let stored = u64::from_le_bytes(bytes[sum_at..].try_into().expect("8-byte slice"));
    if xxh3(&bytes[..sum_at]) != stored {
        return Err(Pass1ArtifactError::Checksum);
    }
    let (body, rest) = postcard::take_from_bytes::<Pass1Body>(&bytes[body_start..sum_at])
        .map_err(Pass1ArtifactError::Decode)?;
    if !rest.is_empty() {
        return Err(Pass1ArtifactError::Truncated(format!(
            "{} bytes after the body",
            rest.len()
        )));
    }
    Ok((head, body))
}

/// Writes `pass1` to `path` as `.athf` is written ([`super::fits_artifact::write_fits`]): the bytes to a temporary
/// file beside it, then [`crate::fits_writer::writer::rename_replace`] (no fsync: a write a crash loses or truncates
/// reads back as a refused artifact, never a wrong answer). The records are serialised through a borrowed view, never
/// cloned. Returns the bytes written.
pub fn write_pass1(path: &Path, pass1: &FramePass1) -> std::io::Result<u64> {
    if let Some(plane) = pass1
        .body()
        .planes
        .iter()
        .position(|r| !r.prep.is_storable())
    {
        let e = std::io::Error::other(
            "a Mode::Population plane record cannot be stored: its per-seed records are per fitted seed",
        );
        tracing::error!(path = %path.display(), plane, error = %e, "first-pass artifact refused");
        return Err(e);
    }
    let bytes = encode(&pass1.head(), &pass1.body())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        path.with_extension(format!("athm.tmp.{}.{}", std::process::id(), seq))
    };
    let written = (|| -> std::io::Result<()> {
        let file = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::new(file);
        w.write_all(&bytes)?;
        w.flush()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = crate::fits_writer::writer::rename_replace(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(bytes.len() as u64)
}

/// The magic, the versions and the head with its checksum; the body is not read (the controller's step (0) reads every
/// member's own beta this way).
pub fn read_pass1_head(path: &Path) -> Result<Pass1Head, Pass1ArtifactError> {
    let mut file = std::fs::File::open(path)?;
    let mut prefix = [0u8; PREFIX_LEN];
    file.read_exact(&mut prefix)
        .map_err(|e| Pass1ArtifactError::Truncated(format!("header: {e}")))?;
    let (head_len, head_sum) = parse_prefix(&prefix)?;
    let mut head_bytes = vec![0u8; head_len];
    file.read_exact(&mut head_bytes)
        .map_err(|e| Pass1ArtifactError::Truncated(format!("head: {e}")))?;
    decode_head(&head_bytes, head_sum)
}

/// The whole record: the checksum, the stamp against `calibrated` now ([`Pass1ArtifactError::Stale`]), the heads
/// rebuilt from `opts` ([`FramePass1::from_record`]).
pub fn read_pass1(
    path: &Path,
    calibrated: &Path,
    opts: &MeasureOptions,
) -> Result<FramePass1, Pass1ArtifactError> {
    let bytes = std::fs::read(path)?;
    let (head, body) = decode(&bytes)?;
    FramePass1::from_record(head, body, calibrated, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::measure::tests::{field, write};
    use crate::stacking::measure::{
        check_pass1_head, measure_frame_pass1, measure_frame_pass2, FrameMeasurement,
        PlaneRecordRef,
    };
    use crate::stacking::psf_signal::group_beta;
    use crate::star_measure::PlanePrep;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    /// A mono frame and a three-plane frame of the measure tests' synthetic star field, written into `dir`.
    fn frames(dir: &Path) -> Vec<PathBuf> {
        let (a, w, h) = field(7, 1.0, 0.002);
        let (b, _, _) = field(8, 1.0, 0.002);
        let (c, _, _) = field(9, 1.0, 0.002);
        vec![
            write(dir, "mono.fits", &[&a], w, h),
            write(dir, "rgb.fits", &[&a, &b, &c], w, h),
        ]
    }

    /// The hand-built record of the encoding pin, as a stored frame: NaN, +inf, a negative stamp time, a starless plane.
    fn pin_bytes() -> Vec<u8> {
        let (full, starless) = (PlanePrep::for_pin(false), PlanePrep::for_pin(true));
        let plane = |prep| PlaneRecordRef {
            prep,
            adapter_detect_ms: 7,
            own_phases: Some(crate::star_measure::PhaseTimings::default()),
            wall: std::time::Duration::new(1, 250),
        };
        let body = Pass1BodyRef {
            gain: Some(0.5),
            read_ms: &[3, 4],
            elapsed: std::time::Duration::from_millis(900),
            planes: vec![plane(&full), plane(&starless)],
        };
        let head = Pass1Head {
            source: Some(SourceStamp {
                len: 4096,
                modified_ns: Some(-1_500_000_000),
            }),
            width: 64,
            height: 48,
            beta_own: vec![f64::NAN, f64::INFINITY],
        };
        encode(&head, &body).unwrap()
    }

    /// R11c-2: the bytes are the value, to the bit. A real first pass of a mono and of a three-plane frame goes to a
    /// file and back, and the read-back value writes the same bytes again; so does the hand-built record with NaN,
    /// +-inf and a starless plane (decode, then encode the view of what was decoded).
    #[test]
    fn round_trips_a_mono_and_a_three_plane_first_pass_bit_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let opts = MeasureOptions::default();
        for path in frames(dir.path()) {
            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
            let pass1 = measure_frame_pass1(&path, &opts, None, &AtomicBool::new(false)).unwrap();
            let first = dir.path().join(format!("{name}.athm"));
            let written = write_pass1(&first, &pass1).unwrap();
            assert_eq!(written, std::fs::metadata(&first).unwrap().len(), "{name}");
            let back = read_pass1(&first, &path, &opts).unwrap();
            let bits = |b: &[f64]| b.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(back.beta_own()), bits(pass1.beta_own()), "{name}");
            assert_eq!(back.source(), pass1.source(), "{name}");
            let second = dir.path().join(format!("{name}-again.athm"));
            write_pass1(&second, &back).unwrap();
            assert_eq!(
                std::fs::read(&first).unwrap(),
                std::fs::read(&second).unwrap(),
                "{name}: re-encoding the read-back value gives the written bytes"
            );
        }
        let bytes = pin_bytes();
        let (head, body) = decode(&bytes).unwrap();
        assert!(head.beta_own[0].is_nan() && head.beta_own[1] == f64::INFINITY);
        assert_eq!(head.source.unwrap().modified_ns, Some(-1_500_000_000));
        assert_eq!(encode(&head, &body.view()).unwrap(), bytes);
    }

    /// R11c-1: pass 2 from a first pass read back from its artifact is pass 2 on the first pass in memory — the
    /// measurement minus its wall-clock `duration_ms`, and every plane's fits, equal (Debug, so NaN fields compare).
    #[test]
    fn pass_two_from_a_read_back_first_pass_equals_pass_two_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let strip = |m: &FrameMeasurement| {
            let mut v = serde_json::to_value(m).unwrap();
            v.as_object_mut().unwrap().remove("durationMs");
            v
        };
        // The default matched filter, and the peak detector (the `SeedSpec::Given` rebuild: `prep.seeds()` back
        // into the head).
        let sources = [
            crate::stacking::config::SeedSource::MatchedFilter,
            crate::stacking::config::SeedSource::Peak,
        ];
        let frame_paths = frames(dir.path());
        for (source, path) in sources
            .iter()
            .flat_map(|&s| frame_paths.iter().map(move |p| (s, p.clone())))
        {
            let opts = MeasureOptions {
                seed_source: source,
                ..MeasureOptions::default()
            };
            let name = format!("{}_{source:?}", path.file_stem().unwrap().to_str().unwrap());
            let in_memory = measure_frame_pass1(&path, &opts, None, &cancel).unwrap();
            let through_file = {
                let fresh = measure_frame_pass1(&path, &opts, None, &cancel).unwrap();
                let artifact = dir.path().join(format!("{name}.athm"));
                write_pass1(&artifact, &fresh).unwrap();
                read_pass1(&artifact, &path, &opts).unwrap()
            };
            let beta = group_beta(in_memory.beta_own());
            let betas = vec![beta; in_memory.beta_own().len()];
            let (m_mem, fits_mem) =
                measure_frame_pass2(&path, in_memory, &betas, &opts, None, &cancel).unwrap();
            let (m_file, fits_file) =
                measure_frame_pass2(&path, through_file, &betas, &opts, None, &cancel).unwrap();
            assert_eq!(strip(&m_file), strip(&m_mem), "{name}: the measurement");
            assert_eq!(
                format!("{fits_file:?}"),
                format!("{fits_mem:?}"),
                "{name}: the fits"
            );
        }
    }

    /// R11c-2: a wrong magic, format version, measure version, checksum (head, body, trailer) and every length
    /// mismatch is one typed arm, never an `Ok`.
    #[test]
    fn refuses_bad_magic_versions_checksum_and_truncation() {
        let good = pin_bytes();
        assert!(decode(&good).is_ok());
        let flip = |at: usize| {
            let mut b = good.clone();
            b[at] ^= 0x01;
            b
        };
        assert!(matches!(
            decode(&flip(0)),
            Err(Pass1ArtifactError::BadMagic { .. })
        ));
        assert!(matches!(
            decode(&flip(4)),
            Err(Pass1ArtifactError::BadVersion { found }) if found == PASS1_ARTIFACT_VERSION ^ 1
        ));
        assert!(matches!(
            decode(&flip(6)),
            Err(Pass1ArtifactError::BadMeasureVersion { found }) if found == MEASURE_VERSION ^ 1
        ));
        assert!(matches!(
            decode(&flip(PREFIX_LEN + 1)),
            Err(Pass1ArtifactError::Checksum)
        ));
        assert!(matches!(
            decode(&flip(good.len() / 2)),
            Err(Pass1ArtifactError::Checksum)
        ));
        assert!(matches!(
            decode(&flip(good.len() - 1)),
            Err(Pass1ArtifactError::Checksum)
        ));
        assert!(matches!(
            decode(&good[..good.len() - 9]),
            Err(Pass1ArtifactError::Truncated(_))
        ));
        assert!(matches!(
            decode(&good[..10]),
            Err(Pass1ArtifactError::Truncated(_))
        ));
        let mut longer = good.clone();
        longer.push(0);
        assert!(matches!(
            decode(&longer),
            Err(Pass1ArtifactError::Truncated(_))
        ));
        assert!(matches!(decode(&[]), Err(Pass1ArtifactError::Truncated(_))));
    }

    /// Plan A Task 13b review (M1): a first pass run under `Mode::Population` keeps one verdict record per FITTED seed
    /// and does not store its population, so the artifact refuses to write it (a read-back would index the records by
    /// the wrong seeds). The stacking Measure runs `Mode::All`, which writes as always.
    #[test]
    fn a_population_first_pass_is_refused_not_stored() {
        use crate::stacking::measure::StarPopulation;
        let dir = tempfile::tempdir().unwrap();
        let path = frames(dir.path()).remove(0);
        let artifact = dir.path().join("mono.athm");
        let opts = MeasureOptions {
            population: Some(StarPopulation::probe_default(2000)),
            ..MeasureOptions::default()
        };
        let pass1 = measure_frame_pass1(&path, &opts, None, &AtomicBool::new(false)).unwrap();
        let err =
            write_pass1(&artifact, &pass1).expect_err("a population record must not be stored");
        assert!(err.to_string().contains("Population"), "{err}");
        assert!(!artifact.exists());
        let all = measure_frame_pass1(
            &path,
            &MeasureOptions::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        write_pass1(&artifact, &all).unwrap();
    }

    /// R11c-5 step (0): the head is read without the body — a corrupt body leaves it `Ok` while the whole read says
    /// `Checksum`; a flipped head byte refuses the head itself.
    #[test]
    fn the_head_reads_without_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let opts = MeasureOptions::default();
        let path = frames(dir.path()).remove(0);
        let pass1 = measure_frame_pass1(&path, &opts, None, &AtomicBool::new(false)).unwrap();
        let artifact = dir.path().join("mono.athm");
        write_pass1(&artifact, &pass1).unwrap();
        let head = read_pass1_head(&artifact).unwrap();
        assert_eq!(head.beta_own, pass1.beta_own());
        assert_eq!(head.source, pass1.source());

        let good = std::fs::read(&artifact).unwrap();
        let mut body_flipped = good.clone();
        let at = good.len() - 20;
        body_flipped[at] ^= 0x40;
        std::fs::write(&artifact, &body_flipped).unwrap();
        assert_eq!(
            read_pass1_head(&artifact).unwrap(),
            head,
            "the body is not read"
        );
        assert!(matches!(
            read_pass1(&artifact, &path, &opts),
            Err(Pass1ArtifactError::Checksum)
        ));

        let mut head_flipped = good;
        head_flipped[PREFIX_LEN + 1] ^= 0x40;
        std::fs::write(&artifact, &head_flipped).unwrap();
        assert!(matches!(
            read_pass1_head(&artifact),
            Err(Pass1ArtifactError::Checksum)
        ));
    }

    /// R11c-4: the stamp is the calibrated file's size and mtime at pass 1's first read; a file touched since reads
    /// `Stale` with both stamps — from the whole read and from the controller's head check — and an untouched one
    /// reads fine.
    #[test]
    fn a_changed_source_reads_stale() {
        let dir = tempfile::tempdir().unwrap();
        let opts = MeasureOptions::default();
        let path = frames(dir.path()).remove(0);
        let pass1 = measure_frame_pass1(&path, &opts, None, &AtomicBool::new(false)).unwrap();
        let artifact = dir.path().join("mono.athm");
        write_pass1(&artifact, &pass1).unwrap();
        let head = read_pass1_head(&artifact).unwrap();
        check_pass1_head(&head, &path).unwrap();
        read_pass1(&artifact, &path, &opts).unwrap();

        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10))
            .unwrap();
        drop(file);
        let stored = pass1.source().unwrap();
        match read_pass1(&artifact, &path, &opts) {
            Err(Pass1ArtifactError::Stale {
                stored: s,
                now: Some(n),
            }) => {
                assert_eq!(s, Some(stored));
                assert_eq!(n.len, stored.len);
                assert_ne!(n.modified_ns, stored.modified_ns);
            }
            other => panic!("expected Stale, got {:?}", other.map(|_| ())),
        }
        assert!(matches!(
            check_pass1_head(&head, &path),
            Err(Pass1ArtifactError::Stale { .. })
        ));
    }
}
