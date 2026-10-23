//! Frame geometry for calibration matching (owner rulings 2026-10-20).
//!
//! A calibration frame of a different size can never calibrate a source
//! frame. That holds for every pair the matcher knows — lights → flat / dark /
//! bias, flats → dark flat / dark / bias, darks → bias — in auto-link, in the
//! manual list and in a manual assignment, and it is not a `MatchMode` a user
//! can relax.
//!
//! - **Size** is `frames.naxis1` × `frames.naxis2`: the array as stored, so
//!   after binning (a 2×2 capture of a 9576×6388 sensor is 4784×3194). Binning
//!   itself stays the matcher's own parameter.
//! - **Origin** is the sub-frame origin `XORGSUBF` / `YORGSUBF`, read from the
//!   stored header blob (`fits_header.header`) — the catalog has no column for
//!   it. It is compared only when BOTH sides record it, and an origin refusal
//!   binds automatic choices only (owner ruling G-7, "auto no, manual yes"):
//!   auto-link never takes such a set, a person may — with a warning. Callers
//!   that accept a manual choice tell `Refused(Origin)` from the other two.
//! - A size one side never recorded blocks nothing; the caller logs it.
//! - A calibration set's size is its members' agreed size; members that
//!   disagree make the set `mixed`, and a mixed set fits nothing. Its origin is
//!   its lowest-frame-id member's (the stored-header precedent of
//!   `calibration_library::headers`).

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::models::{FileFormat, Frame};

/// What a frame or a calibration set records about its pixel grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Geometry {
    /// `(NAXIS1, NAXIS2)` as stored. `None` when not recorded — or, for a set,
    /// when its members disagree (then `mixed` is set).
    pub size: Option<(i32, i32)>,
    /// `(XORGSUBF, YORGSUBF)`; `None` unless the stored header records both.
    pub origin: Option<(i32, i32)>,
    /// A calibration set whose members are not all one size.
    pub mixed: bool,
}

impl Geometry {
    /// `9576×6388`, `6384×4258 @ 1584,1064`, `mixed sizes` or
    /// `size not recorded` — the one spelling every message, log field and
    /// card uses.
    pub fn label(&self) -> String {
        if self.mixed {
            return "mixed sizes".to_string();
        }
        let size = match self.size {
            Some((w, h)) => format!("{w}×{h}"),
            None => "size not recorded".to_string(),
        };
        match self.origin {
            Some((x, y)) => format!("{size} @ {x},{y}"),
            None => size,
        }
    }
}

/// Why a calibration set cannot calibrate a frame. `Size` and `Mixed` bind
/// everywhere; `Origin` binds automatic choices only (G-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Size,
    Origin,
    Mixed,
}

/// The answer to "can this set calibrate that frame?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Fits,
    /// One side records no size: nothing is blocked (ruling R1); the caller
    /// counts it and logs once.
    Unknown,
    Refused(Refusal),
}

impl Fit {
    pub fn refused(self) -> bool {
        matches!(self, Fit::Refused(_))
    }
}

/// The rule. `source` is the frame (or the flat set) being calibrated.
pub fn fit(source: &Geometry, calibration: &Geometry) -> Fit {
    if source.mixed || calibration.mixed {
        return Fit::Refused(Refusal::Mixed);
    }
    if let (Some(a), Some(b)) = (source.size, calibration.size) {
        if a != b {
            return Fit::Refused(Refusal::Size);
        }
    }
    if let (Some(a), Some(b)) = (source.origin, calibration.origin) {
        if a != b {
            return Fit::Refused(Refusal::Origin);
        }
    }
    if source.size.is_none() || calibration.size.is_none() {
        Fit::Unknown
    } else {
        Fit::Fits
    }
}

/// A `Frame`'s recorded size.
pub fn size_of(frame: &Frame) -> Option<(i32, i32)> {
    frame.naxis1.zip(frame.naxis2)
}

/// A set's `(size, mixed)` from `MIN`/`MAX` of its members' `naxis1`/`naxis2`
/// (SQL aggregates skip NULLs, so members that record no size do not vote).
pub(crate) fn size_from_aggregate(
    w_min: Option<i32>,
    w_max: Option<i32>,
    h_min: Option<i32>,
    h_max: Option<i32>,
) -> (Option<(i32, i32)>, bool) {
    match (w_min, w_max, h_min, h_max) {
        (Some(a), Some(b), Some(c), Some(d)) if a == b && c == d => (Some((a, c)), false),
        (Some(_), Some(_), Some(_), Some(_)) => (None, true),
        _ => (None, false),
    }
}

/// `(XORGSUBF, YORGSUBF)` from a parsed keyword map (UPPERCASE keys) — both
/// or nothing.
pub fn origin_from_keys(keys: &HashMap<String, String>) -> Option<(i32, i32)> {
    let read = |key: &str| keys.get(key).and_then(|v| card_int(v));
    read("XORGSUBF").zip(read("YORGSUBF"))
}

/// An integer card value; an integral real (`1064.`) counts, anything else
/// does not.
fn card_int(value: &str) -> Option<i32> {
    let v = value.trim();
    v.parse::<i32>().ok().or_else(|| {
        v.parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() <= i32::MAX as f64)
            .map(|f| f as i32)
    })
}

/// The origin a stored header blob records, in any of the shapes the scanner
/// stores (FITS cards, XISF XML, a plain keyword dump).
pub fn origin_from_header(format: FileFormat, header: &str) -> Option<(i32, i32)> {
    origin_from_keys(&parse_stored_header_keys(format, header))
}

/// The origin `file_id`'s newest stored header records. `Ok(None)` when the
/// file has no stored header or it records no origin.
pub fn origin_for_file(conn: &Connection, file_id: i64) -> rusqlite::Result<Option<(i32, i32)>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT fi.format, fh.header FROM files fi
             JOIN fits_header fh ON fh.file_id = fi.id
             WHERE fi.id = ?1
             ORDER BY fh.id DESC
             LIMIT 1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((format, header)) = row else {
        return Ok(None);
    };
    // `files.format` is CHECK-constrained to these two.
    let format = if format == "XISF" {
        FileFormat::XISF
    } else {
        FileFormat::FITS
    };
    Ok(origin_from_header(format, &header))
}

/// One frame's size and origin.
pub fn geometry_for_frame(conn: &Connection, frame_id: i64) -> rusqlite::Result<Geometry> {
    let (file_id, w, h): (i64, Option<i32>, Option<i32>) = conn.query_row(
        "SELECT file_id, naxis1, naxis2 FROM frames WHERE id = ?1",
        params![frame_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(Geometry {
        size: w.zip(h),
        origin: origin_for_file(conn, file_id)?,
        mixed: false,
    })
}

/// A set's origin: its lowest-frame-id member's.
pub fn set_origin(conn: &Connection, set_id: i64) -> rusqlite::Result<Option<(i32, i32)>> {
    let file_id: Option<i64> = conn
        .query_row(
            "SELECT f.file_id FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id = ?1
             ORDER BY csf.frame_id
             LIMIT 1",
            params![set_id],
            |r| r.get(0),
        )
        .optional()?;
    match file_id {
        Some(id) => origin_for_file(conn, id),
        None => Ok(None),
    }
}

/// Fill `detail.xorgsubf` / `yorgsubf` from [`set_origin`].
pub fn fill_set_origin(
    conn: &Connection,
    detail: &mut crate::models::CalibrationSetDetail,
) -> rusqlite::Result<()> {
    if let Some(id) = detail.id {
        let origin = set_origin(conn, id)?;
        detail.xorgsubf = origin.map(|o| o.0);
        detail.yorgsubf = origin.map(|o| o.1);
    }
    Ok(())
}

/// Lights seen by one hierarchy view: each light's size is read once and its
/// stored header at most once, however many sets compare against it (review
/// I-1). Filled lazily, so a view whose sets record no origin reads nothing.
#[derive(Debug, Default)]
pub struct LightGeometryCache {
    lights: HashMap<i64, LazyFrameGeometry>,
    /// Stored-header reads this cache has made (a pin for the tests).
    pub origin_reads: usize,
}

impl LightGeometryCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn fit(
        &mut self,
        conn: &Connection,
        frame_id: i64,
        set: &Geometry,
    ) -> rusqlite::Result<(Fit, Geometry)> {
        let light = match self.lights.entry(frame_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(LazyFrameGeometry::load(conn, frame_id)?)
            }
        };
        let had_origin = light.origin.is_some();
        let verdict = light.fit(conn, set)?;
        if !had_origin && light.origin.is_some() {
            self.origin_reads += 1;
        }
        Ok((verdict, light.geometry()))
    }
}

/// The calibration hierarchy's origin warning for one set (ruling G-7): the
/// set and the first of `light_frame_ids` whose size equals the set's and
/// whose recorded sub-frame origin differs. `set` is the set's geometry as
/// the caller already holds it. `None` when the set records no origin (then no
/// light header is read) or every light agrees. A size misfit is not reported
/// here — the readiness gate blocks it (Task 6).
pub fn origin_mismatch_for_lights(
    conn: &Connection,
    set: Geometry,
    light_frame_ids: &[i64],
    cache: &mut LightGeometryCache,
) -> rusqlite::Result<Option<(Geometry, Geometry)>> {
    if set.mixed || set.size.is_none() || set.origin.is_none() {
        return Ok(None);
    }
    for &frame_id in light_frame_ids {
        let (verdict, light) = cache.fit(conn, frame_id, &set)?;
        if verdict == Fit::Refused(Refusal::Origin) {
            return Ok(Some((set, light)));
        }
    }
    Ok(None)
}

/// A set's agreed size and whether its members disagree — the aggregate
/// part of [`geometry_for_set`], no header read.
pub fn size_for_set(
    conn: &Connection,
    set_id: i64,
) -> rusqlite::Result<(Option<(i32, i32)>, bool)> {
    let (w_min, w_max, h_min, h_max): (Option<i32>, Option<i32>, Option<i32>, Option<i32>) = conn
        .query_row(
        "SELECT MIN(f.naxis1), MAX(f.naxis1), MIN(f.naxis2), MAX(f.naxis2)
             FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id = ?1",
        params![set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    Ok(size_from_aggregate(w_min, w_max, h_min, h_max))
}

/// A set's agreed size, origin, and whether its members disagree.
pub fn geometry_for_set(conn: &Connection, set_id: i64) -> rusqlite::Result<Geometry> {
    let (w_min, w_max, h_min, h_max): (Option<i32>, Option<i32>, Option<i32>, Option<i32>) = conn
        .query_row(
        "SELECT MIN(f.naxis1), MAX(f.naxis1), MIN(f.naxis2), MAX(f.naxis2)
             FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             WHERE csf.set_id = ?1",
        params![set_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let (size, mixed) = size_from_aggregate(w_min, w_max, h_min, h_max);
    let origin = if mixed {
        None
    } else {
        set_origin(conn, set_id)?
    };
    Ok(Geometry {
        size,
        origin,
        mixed,
    })
}

/// What a set made of exactly `frame_ids` would record — the same rules as
/// [`geometry_for_set`] (agreed size, `mixed`, the lowest frame id's origin)
/// — so a raw group is judged BEFORE any set row is written for it (ruling
/// G-8, review I-2). Empty → nothing recorded.
pub fn geometry_for_frames(conn: &Connection, frame_ids: &[i64]) -> rusqlite::Result<Geometry> {
    let Some(&lowest) = frame_ids.iter().min() else {
        return Ok(Geometry::default());
    };
    let placeholders = vec!["?"; frame_ids.len()].join(",");
    let (w_min, w_max, h_min, h_max): (Option<i32>, Option<i32>, Option<i32>, Option<i32>) = conn
        .query_row(
        &format!(
            "SELECT MIN(naxis1), MAX(naxis1), MIN(naxis2), MAX(naxis2)
                 FROM frames WHERE id IN ({placeholders})"
        ),
        rusqlite::params_from_iter(frame_ids.iter()),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let (size, mixed) = size_from_aggregate(w_min, w_max, h_min, h_max);
    let origin = if mixed {
        None
    } else {
        let file_id: i64 = conn.query_row(
            "SELECT file_id FROM frames WHERE id = ?1",
            params![lowest],
            |r| r.get(0),
        )?;
        origin_for_file(conn, file_id)?
    };
    Ok(Geometry {
        size,
        origin,
        mixed,
    })
}

/// A frame whose stored header is parsed only when it can decide a verdict
/// (ruling R10, review I-3): the size comes from `frames`; the origin is read
/// at most once, and only when a set of the same size records one.
#[derive(Debug)]
pub struct LazyFrameGeometry {
    file_id: i64,
    size: Option<(i32, i32)>,
    /// `None` = not read yet; `Some(None)` = read, records no origin.
    origin: Option<Option<(i32, i32)>>,
}

impl LazyFrameGeometry {
    pub fn load(conn: &Connection, frame_id: i64) -> rusqlite::Result<Self> {
        let (file_id, w, h): (i64, Option<i32>, Option<i32>) = conn.query_row(
            "SELECT file_id, naxis1, naxis2 FROM frames WHERE id = ?1",
            params![frame_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(Self {
            file_id,
            size: w.zip(h),
            origin: None,
        })
    }

    /// [`fit`] against `set`, reading this frame's origin first only when the
    /// sizes agree and `set` records an origin.
    pub fn fit(&mut self, conn: &Connection, set: &Geometry) -> rusqlite::Result<Fit> {
        if self.origin.is_none()
            && !set.mixed
            && set.origin.is_some()
            && set.size.is_some()
            && set.size == self.size
        {
            self.origin = Some(origin_for_file(conn, self.file_id)?);
        }
        Ok(fit(&self.geometry(), set))
    }

    /// What has been read so far (the origin stays `None` until a fit needed it).
    pub fn geometry(&self) -> Geometry {
        Geometry {
            size: self.size,
            origin: self.origin.flatten(),
            mixed: false,
        }
    }

    /// The label a report shows for `refusal`: with the origin only when the
    /// origin decided it, so lights of one size share one report row however
    /// many origins were read for their other links.
    pub fn report_label(&self, refusal: Refusal) -> String {
        let g = self.geometry();
        if refusal == Refusal::Origin {
            g.label()
        } else {
            Geometry { origin: None, ..g }.label()
        }
    }
}

/// Whose links [`prune_unfit_auto_links`] sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSource {
    Frame(i64),
    Set(i64),
}

/// Delete every AUTOMATIC link of `source` whose set does not fit
/// `geometry` - size, mixed or origin (plan ruling R9, G-2, G-7); manual
/// links are never touched. Each removal is logged. Returns how many were
/// removed.
pub fn prune_unfit_auto_links(
    conn: &Connection,
    source: LinkSource,
    geometry: &Geometry,
) -> rusqlite::Result<usize> {
    let (source_type, source_id) = match source {
        LinkSource::Frame(id) => ("frame", id),
        LinkSource::Set(id) => ("calibration_set", id),
    };
    let links: Vec<(i64, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, calibration_set_id, calibration_type FROM calibration_set_to_frames
             WHERE source_type = ?1 AND source_id = ?2 AND is_manual_override = 0
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![source_type, source_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut removed = 0;
    for (link_id, set_id, calibration_type) in links {
        let set = geometry_for_set(conn, set_id)?;
        if !fit(geometry, &set).refused() {
            continue;
        }
        conn.execute(
            "DELETE FROM calibration_set_to_frames WHERE id = ?1",
            params![link_id],
        )?;
        removed += 1;
        match source {
            LinkSource::Frame(frame_id) => tracing::warn!(
                frame_id,
                set_id,
                calibration_type = %calibration_type,
                source_geometry = %geometry.label(),
                set_geometry = %set.label(),
                "automatic calibration link removed: the set does not fit the frame geometry"
            ),
            LinkSource::Set(source_set_id) => tracing::warn!(
                source_set_id,
                set_id,
                calibration_type = %calibration_type,
                source_geometry = %geometry.label(),
                set_geometry = %set.label(),
                "automatic calibration link removed: the set does not fit the frame geometry"
            ),
        }
    }
    Ok(removed)
}

#[cfg(test)]
pub(crate) mod test_support {
    use rusqlite::{params, Connection};

    /// A FITS stored header in the scanner's shape (80-column cards joined by
    /// `\n`, `FitsHeader::to_header_text`) that records `origin` when given.
    pub(crate) fn fits_header_text(origin: Option<(i32, i32)>) -> String {
        let mut cards = vec![format!("{:<80}", "SIMPLE  =                    T")];
        if let Some((x, y)) = origin {
            cards.push(format!(
                "{:<80}",
                format!("XORGSUBF= {x:>20} / Subframe X position")
            ));
            cards.push(format!(
                "{:<80}",
                format!("YORGSUBF= {y:>20} / Subframe Y position")
            ));
        }
        cards.push(format!("{:<80}", "END"));
        cards.join("\n")
    }

    /// One FITS `files` row (`/geometry/<tag>.fits`, tags must be unique per
    /// database) with a stored header. Returns `files.id`.
    pub(crate) fn seed_file(conn: &Connection, tag: &str, origin: Option<(i32, i32)>) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, 0, '2025-01-01T00:00:00Z', 'FITS')",
            params![format!("/geometry/{tag}.fits"), format!("{tag}.fits")],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            params![file_id, fits_header_text(origin)],
        )
        .unwrap();
        file_id
    }

    /// One `frames` row of `imagetyp` on a fresh file. Returns `frames.id`.
    pub(crate) fn seed_frame(
        conn: &Connection,
        tag: &str,
        imagetyp: &str,
        size: Option<(i32, i32)>,
        origin: Option<(i32, i32)>,
    ) -> i64 {
        let file_id = seed_file(conn, tag, origin);
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, naxis1, naxis2) VALUES (?1, ?2, ?3, ?4)",
            params![file_id, imagetyp, size.map(|s| s.0), size.map(|s| s.1)],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// A bare `calibration_set` row of `imagetyp`. Returns its id.
    pub(crate) fn seed_set(conn: &Connection, imagetyp: &str) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date) VALUES (?1, '2025-01-01')",
            params![imagetyp],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Join `frame_ids` into calibration set `set_id`.
    pub(crate) fn add_members(conn: &Connection, set_id: i64, frame_ids: &[i64]) {
        for &frame_id in frame_ids {
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, frame_id],
            )
            .unwrap();
        }
    }

    pub(crate) fn file_id_of(conn: &Connection, frame_id: i64) -> i64 {
        conn.query_row(
            "SELECT file_id FROM frames WHERE id = ?1",
            [frame_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Run `f` under a thread-local subscriber and count the WARN events
    /// whose message is exactly `message` — the in-process pattern of
    /// `stacking::config::tests::resolve_counting_warns`, narrowed to one
    /// event so an unrelated warn cannot pass or fail the count.
    pub(crate) fn count_warns<R>(message: &'static str, f: impl FnOnce() -> R) -> (R, usize) {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use tracing_subscriber::layer::SubscriberExt;
        struct Message(Option<String>);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = Some(format!("{value:?}"));
                }
            }
        }
        struct Count(Arc<AtomicUsize>, &'static str);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() != tracing::Level::WARN {
                    return;
                }
                let mut m = Message(None);
                event.record(&mut m);
                if m.0.as_deref() == Some(self.1) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        let n = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Count(n.clone(), message));
        let out = tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            f()
        });
        (out, n.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::db::schema::init_db;

    const FULL: (i32, i32) = (9576, 6388);
    const ROI: (i32, i32) = (6384, 4258);

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn g(size: Option<(i32, i32)>, origin: Option<(i32, i32)>) -> Geometry {
        Geometry {
            size,
            origin,
            mixed: false,
        }
    }

    #[test]
    fn fit_truth_table() {
        let full = g(Some(FULL), None);
        assert_eq!(fit(&full, &full), Fit::Fits);
        assert_eq!(fit(&full, &g(Some(ROI), None)), Fit::Refused(Refusal::Size));
        // Origin only when BOTH record it (O2).
        assert_eq!(
            fit(
                &g(Some(ROI), Some((1584, 1064))),
                &g(Some(ROI), Some((0, 0)))
            ),
            Fit::Refused(Refusal::Origin)
        );
        assert_eq!(
            fit(&g(Some(ROI), Some((1584, 1064))), &g(Some(ROI), None)),
            Fit::Fits
        );
        assert_eq!(
            fit(&g(Some(ROI), None), &g(Some(ROI), Some((0, 0)))),
            Fit::Fits
        );
        // An unrecorded size blocks nothing (R1).
        assert_eq!(fit(&g(None, None), &full), Fit::Unknown);
        assert_eq!(fit(&full, &g(None, None)), Fit::Unknown);
        // A mixed set fits nothing, whatever the source (R12).
        let mixed = Geometry {
            size: None,
            origin: None,
            mixed: true,
        };
        assert_eq!(fit(&full, &mixed), Fit::Refused(Refusal::Mixed));
        assert_eq!(fit(&g(None, None), &mixed), Fit::Refused(Refusal::Mixed));
        assert!(
            Fit::Refused(Refusal::Size).refused()
                && !Fit::Unknown.refused()
                && !Fit::Fits.refused()
        );
    }

    #[test]
    fn label_spelling() {
        assert_eq!(g(Some(FULL), None).label(), "9576×6388");
        assert_eq!(
            g(Some(ROI), Some((1584, 1064))).label(),
            "6384×4258 @ 1584,1064"
        );
        assert_eq!(g(None, None).label(), "size not recorded");
        assert_eq!(
            Geometry {
                size: None,
                origin: None,
                mixed: true
            }
            .label(),
            "mixed sizes"
        );
    }

    #[test]
    fn origin_from_fits_cards_xisf_xml_and_keyword_dump() {
        assert_eq!(
            origin_from_header(FileFormat::FITS, &fits_header_text(Some((1584, 1064)))),
            Some((1584, 1064))
        );
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xisf version="1.0"><Image geometry="6384:4258:1">
<FITSKeyword name="XORGSUBF" value="1584" comment="Subframe X position"/>
<FITSKeyword name="YORGSUBF" value="1064" comment="Subframe Y position"/>
</Image></xisf>"#;
        assert_eq!(
            origin_from_header(FileFormat::XISF, xml),
            Some((1584, 1064))
        );
        // The plain "KEY = value" dump some writers leave (seen on XISF rows of
        // the dev catalog) goes through the dispatcher's fallback.
        let dump = "XISF FITS Keywords:\n==================\n\nXORGSUBF = 0\nYORGSUBF = 0\n";
        assert_eq!(origin_from_header(FileFormat::XISF, dump), Some((0, 0)));
    }

    #[test]
    fn origin_needs_both_keywords() {
        let mut keys = HashMap::new();
        keys.insert("XORGSUBF".to_string(), "1584".to_string());
        assert_eq!(origin_from_keys(&keys), None);
        keys.insert("YORGSUBF".to_string(), "1064.".to_string());
        assert_eq!(
            origin_from_keys(&keys),
            Some((1584, 1064)),
            "an integral real is an integer"
        );
        keys.insert("YORGSUBF".to_string(), "abc".to_string());
        assert_eq!(origin_from_keys(&keys), None);
        assert_eq!(
            origin_from_header(FileFormat::FITS, &fits_header_text(None)),
            None
        );
    }

    #[test]
    fn set_geometry_uniform_mixed_and_unrecorded() {
        let conn = db();
        let uniform = seed_set(&conn, "Dark");
        let a = seed_frame(&conn, "u1", "Dark", Some(FULL), Some((0, 0)));
        let b = seed_frame(&conn, "u2", "Dark", Some(FULL), None);
        add_members(&conn, uniform, &[a, b]);
        assert_eq!(
            geometry_for_set(&conn, uniform).unwrap(),
            g(Some(FULL), Some((0, 0)))
        );

        let mixed = seed_set(&conn, "Dark");
        let c = seed_frame(&conn, "m1", "Dark", Some(FULL), None);
        let d = seed_frame(&conn, "m2", "Dark", Some(ROI), None);
        add_members(&conn, mixed, &[c, d]);
        let m = geometry_for_set(&conn, mixed).unwrap();
        assert!(m.mixed && m.size.is_none(), "{m:?}");

        let unrecorded = seed_set(&conn, "Dark");
        let e = seed_frame(&conn, "n1", "Dark", None, None);
        add_members(&conn, unrecorded, &[e]);
        assert_eq!(geometry_for_set(&conn, unrecorded).unwrap(), g(None, None));

        let empty = seed_set(&conn, "Dark");
        assert_eq!(geometry_for_set(&conn, empty).unwrap(), g(None, None));
    }

    #[test]
    fn set_origin_is_the_lowest_frame_id_members() {
        let conn = db();
        let set = seed_set(&conn, "Flat");
        let first = seed_frame(&conn, "o1", "Flat", Some(ROI), Some((1584, 1064)));
        let second = seed_frame(&conn, "o2", "Flat", Some(ROI), Some((0, 0)));
        add_members(&conn, set, &[second, first]);
        assert_eq!(set_origin(&conn, set).unwrap(), Some((1584, 1064)));
    }

    #[test]
    fn frame_geometry_reads_size_and_origin() {
        let conn = db();
        let id = seed_frame(&conn, "light", "Light", Some(ROI), Some((1584, 1064)));
        assert_eq!(
            geometry_for_frame(&conn, id).unwrap(),
            g(Some(ROI), Some((1584, 1064)))
        );
        // No stored header at all: size alone.
        conn.execute(
            "DELETE FROM fits_header WHERE file_id = ?1",
            [file_id_of(&conn, id)],
        )
        .unwrap();
        assert_eq!(geometry_for_frame(&conn, id).unwrap(), g(Some(ROI), None));
        assert_eq!(
            origin_for_file(&conn, 999_999).unwrap(),
            None,
            "an unknown file records nothing"
        );
    }

    /// Review I-2: a raw group is judged as the set it would become — agreed
    /// size, `mixed`, the lowest frame id's origin — before any row exists.
    #[test]
    fn group_geometry_is_what_its_set_would_record() {
        let conn = db();
        let a = seed_frame(&conn, "g1", "Flat", Some(ROI), Some((1584, 1064)));
        let b = seed_frame(&conn, "g2", "Flat", Some(ROI), Some((0, 0)));
        let c = seed_frame(&conn, "g3", "Flat", Some(FULL), None);
        assert_eq!(
            geometry_for_frames(&conn, &[b, a]).unwrap(),
            g(Some(ROI), Some((1584, 1064)))
        );
        let m = geometry_for_frames(&conn, &[a, c]).unwrap();
        assert!(m.mixed && m.size.is_none(), "{m:?}");
        assert_eq!(geometry_for_frames(&conn, &[]).unwrap(), g(None, None));
    }

    /// Review I-3 / ruling R10: the stored header is parsed only when a
    /// same-size set records an origin, and the report label carries the
    /// origin only when the origin decided the refusal.
    #[test]
    fn lazy_frame_reads_the_origin_only_when_it_can_decide() {
        let conn = db();
        let id = seed_frame(&conn, "lazy", "Light", Some(ROI), Some((1584, 1064)));
        let mut light = LazyFrameGeometry::load(&conn, id).unwrap();
        assert_eq!(
            light.fit(&conn, &g(Some(FULL), Some((0, 0)))).unwrap(),
            Fit::Refused(Refusal::Size)
        );
        assert_eq!(
            light.geometry(),
            g(Some(ROI), None),
            "a size refusal reads no header"
        );
        assert_eq!(light.fit(&conn, &g(Some(ROI), None)).unwrap(), Fit::Fits);
        assert_eq!(
            light.geometry(),
            g(Some(ROI), None),
            "a set without an origin reads no header"
        );
        assert_eq!(
            light.fit(&conn, &g(Some(ROI), Some((0, 0)))).unwrap(),
            Fit::Refused(Refusal::Origin)
        );
        assert_eq!(light.geometry(), g(Some(ROI), Some((1584, 1064))));
        assert_eq!(light.report_label(Refusal::Origin), "6384×4258 @ 1584,1064");
        assert_eq!(light.report_label(Refusal::Size), "6384×4258");
    }

    /// Review T5-r2 I-1: N agreeing lights against three same-size sets that
    /// record an origin cost N stored-header reads, not 3N; a set that records
    /// no origin reads none.
    #[test]
    fn light_cache_reads_each_header_once_across_sets() {
        let conn = db();
        let lights: Vec<i64> = (0..5)
            .map(|i| seed_frame(&conn, &format!("l{i}"), "Light", Some(ROI), Some((7, 9))))
            .collect();
        let mut cache = LightGeometryCache::new();
        for _ in 0..3 {
            let set = g(Some(ROI), Some((7, 9)));
            assert_eq!(
                origin_mismatch_for_lights(&conn, set, &lights, &mut cache).unwrap(),
                None
            );
        }
        assert_eq!(cache.origin_reads, lights.len());
        let none = g(Some(ROI), None);
        origin_mismatch_for_lights(&conn, none, &lights, &mut cache).unwrap();
        assert_eq!(
            cache.origin_reads,
            lights.len(),
            "no origin on the set, no read"
        );
        // The first mismatching light is the one named.
        let other = g(Some(ROI), Some((0, 0)));
        let (set, light) = origin_mismatch_for_lights(&conn, other, &lights, &mut cache)
            .unwrap()
            .unwrap();
        assert_eq!((set, light), (other, g(Some(ROI), Some((7, 9)))));
        assert_eq!(cache.origin_reads, lights.len(), "served from the cache");
    }
}
