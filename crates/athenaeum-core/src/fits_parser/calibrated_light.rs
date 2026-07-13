//! Recognize a calibrated-LIGHT FITS artifact from its header cards (B5
//! in-app light calibration, design spec 2026-07-05-light-calibration-design.md
//! §4/§7). Calibrated lights are self-describing via the `CALSTAT` + `ATH_C*`
//! provenance cards written by [`crate::calibration_library::light_headers`];
//! the scanner uses [`calibrated_light_identity`] to divert them onto the
//! reconcile-adopt path instead of registering them as frames.
//!
//! Input is the already-parsed keyword map
//! ([`crate::fits_parser::stored_header::parse_stored_header_keys`]) — the same
//! access pattern the flat-norm reader uses — so this module does no FITS I/O.

use std::collections::HashMap;

/// A single master reference parsed from an `ATH_CDRK`/`ATH_CFLT`/`ATH_CBIA`
/// card, whose value shape is `"<uuid> <path>"` (a path may be empty when the
/// source had none). Either component may be an empty string.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterRef {
    pub uuid: String,
    pub path: String,
}

/// The identity + provenance a calibrated LIGHT output carries in its header,
/// enough to reconcile it against the `light_calibrations` table or adopt it
/// into a rebuilt catalog (design §4).
#[derive(Debug, Clone, PartialEq)]
pub struct CalibratedIdentity {
    /// `ATH_CSRC` — uuid of the source frame. `None` when the card is absent
    /// OR an empty string (a source frame that had no uuid at build time).
    pub source_uuid: Option<String>,
    /// `ATH_CSRN` — source filename, the adoption fallback key.
    pub source_filename: Option<String>,
    /// `OBJECT` — target name copied through from the source frame (§7).
    /// Disambiguates the filename fallback: astro filenames collide across
    /// nights/objects (`L_0001.fits`), so adoption also matches on OBJECT when
    /// the calibrated file carries it. `None` when the card is absent/empty.
    pub source_object: Option<String>,
    /// `DATE-OBS` — observation timestamp copied through from the source (§7),
    /// verbatim as it appears in the header. A second filename disambiguator;
    /// compared instant-aware (the DB stores a re-serialized RFC3339 form), not
    /// byte-for-byte. `None` when the card is absent/empty.
    pub source_date_obs: Option<String>,
    /// `CALSTAT` — honest applied-state flags (`"BDF"`, `"BD"`, `"BF"`, `"F"`, …).
    pub calstat: String,
    /// `ATH_CDRK` master reference actually applied, if any.
    pub dark: Option<MasterRef>,
    /// `ATH_CFLT` master reference actually applied, if any.
    pub flat: Option<MasterRef>,
    /// `ATH_CBIA` master reference actually applied, if any.
    pub bias: Option<MasterRef>,
    /// `ATH_CFNM` — flat-normalization divisor actually applied (`1.0` = off).
    pub flat_norm_divisor: Option<f64>,
    /// `ATH_CVER` — engine version the file was built with.
    pub engine_version: Option<i64>,
    /// `ATH_PRJ` — Stage-II project id stamped at publish (slice 4). Present ONLY
    /// on a received project contribution; `None` for a personal calibrated
    /// light. Its presence diverts the scanner onto the project-contribution
    /// reconcile (sibling of the light-cal reconcile) instead of frame-registering
    /// or light-cal-adopting the file. `None` when the card is absent/empty.
    pub project_id: Option<String>,
}

/// Trim a header value; treat empty as absent.
fn non_empty(keys: &HashMap<String, String>, key: &str) -> Option<String> {
    keys.get(key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse a `"<uuid> <path>"` master-reference card value. A value with no
/// space yields `path = ""`; both components are taken verbatim otherwise.
fn parse_master_ref(keys: &HashMap<String, String>, key: &str) -> Option<MasterRef> {
    let v = non_empty(keys, key)?;
    let mut parts = v.splitn(2, ' ');
    let uuid = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").trim().to_string();
    Some(MasterRef { uuid, path })
}

/// Recognize a calibrated-LIGHT artifact from its parsed header keywords.
///
/// Present iff `CALSTAT` is set AND at least one identity key is usable —
/// `ATH_CSRC` non-empty OR `ATH_CSRN` present (design §4: `ATH_CSRC` may be an
/// empty string when the source frame had no uuid, in which case adoption
/// falls back to the filename). Any other file (a normal light, a master, a
/// raw calibration frame) returns `None` and flows through normal ingestion.
pub fn calibrated_light_identity(keys: &HashMap<String, String>) -> Option<CalibratedIdentity> {
    let calstat = non_empty(keys, "CALSTAT")?;
    let source_uuid = non_empty(keys, "ATH_CSRC");
    let source_filename = non_empty(keys, "ATH_CSRN");
    if source_uuid.is_none() && source_filename.is_none() {
        return None;
    }
    Some(CalibratedIdentity {
        source_uuid,
        source_filename,
        source_object: non_empty(keys, "OBJECT"),
        source_date_obs: non_empty(keys, "DATE-OBS"),
        calstat,
        dark: parse_master_ref(keys, "ATH_CDRK"),
        flat: parse_master_ref(keys, "ATH_CFLT"),
        bias: parse_master_ref(keys, "ATH_CBIA"),
        flat_norm_divisor: keys.get("ATH_CFNM").and_then(|s| s.trim().parse::<f64>().ok()),
        engine_version: keys.get("ATH_CVER").and_then(|s| s.trim().parse::<i64>().ok()),
        project_id: non_empty(keys, "ATH_PRJ"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn detects_full_identity() {
        let k = keys(&[
            ("CALSTAT", "BDF"),
            ("ATH_CSRC", "uuid-1"),
            ("ATH_CSRN", "L_0001.fits"),
            ("OBJECT", "M42"),
            ("DATE-OBS", "2025-01-01T22:00:00"),
            ("ATH_CDRK", "dark-uuid /lib/dark.fits"),
            ("ATH_CFLT", "flat-uuid /lib/flat.fits"),
            ("ATH_CFNM", "1234.5"),
            ("ATH_CVER", "1"),
        ]);
        let id = calibrated_light_identity(&k).expect("identity present");
        assert_eq!(id.calstat, "BDF");
        assert_eq!(id.source_uuid.as_deref(), Some("uuid-1"));
        assert_eq!(id.source_filename.as_deref(), Some("L_0001.fits"));
        // OBJECT + DATE-OBS are captured as filename-disambiguation keys (§7).
        assert_eq!(id.source_object.as_deref(), Some("M42"));
        assert_eq!(id.source_date_obs.as_deref(), Some("2025-01-01T22:00:00"));
        assert_eq!(id.dark, Some(MasterRef { uuid: "dark-uuid".into(), path: "/lib/dark.fits".into() }));
        assert_eq!(id.flat, Some(MasterRef { uuid: "flat-uuid".into(), path: "/lib/flat.fits".into() }));
        assert!(id.bias.is_none());
        assert_eq!(id.flat_norm_divisor, Some(1234.5));
        assert_eq!(id.engine_version, Some(1));
        // No ATH_PRJ card -> a personal calibrated light, not a project contribution.
        assert!(id.project_id.is_none());
    }

    #[test]
    fn parses_project_id_from_ath_prj() {
        // A published contribution carries the calibrated-light cards PLUS the
        // ATH_PRJ project stamp -> identity is present AND flags the project.
        let k = keys(&[
            ("CALSTAT", "BDF"),
            ("ATH_CSRC", "uuid-1"),
            ("ATH_CSRN", "L_0001.fits"),
            ("ATH_PRJ", "proj-abc"),
        ]);
        let id = calibrated_light_identity(&k).expect("identity present");
        assert_eq!(id.project_id.as_deref(), Some("proj-abc"));
        // An empty ATH_PRJ is treated as absent (same as every other card).
        let k = keys(&[("CALSTAT", "BDF"), ("ATH_CSRC", "uuid-1"), ("ATH_PRJ", "  ")]);
        let id = calibrated_light_identity(&k).expect("identity present");
        assert!(id.project_id.is_none(), "blank ATH_PRJ is absent");
    }

    #[test]
    fn empty_ath_csrc_falls_back_to_filename() {
        // Source frame had no uuid: ATH_CSRC is the empty string. Identity is
        // still present, anchored on the filename.
        let k = keys(&[("CALSTAT", "BD"), ("ATH_CSRC", ""), ("ATH_CSRN", "L_9.fits")]);
        let id = calibrated_light_identity(&k).expect("identity present via filename");
        assert!(id.source_uuid.is_none(), "empty ATH_CSRC treated as absent");
        assert_eq!(id.source_filename.as_deref(), Some("L_9.fits"));
        // No OBJECT/DATE-OBS cards -> no disambiguation keys captured.
        assert!(id.source_object.is_none());
        assert!(id.source_date_obs.is_none());
    }

    #[test]
    fn absent_without_calstat() {
        // ATH_C* present but no CALSTAT -> not a calibrated light.
        let k = keys(&[("ATH_CSRC", "uuid-1"), ("ATH_CSRN", "L.fits")]);
        assert!(calibrated_light_identity(&k).is_none());
    }

    #[test]
    fn absent_without_any_identity_key() {
        // CALSTAT alone (e.g. a MaxIm-tagged raw file) with no ATH_C* keys.
        let k = keys(&[("CALSTAT", "BDF")]);
        assert!(calibrated_light_identity(&k).is_none());
        // ATH_CSRC empty AND no ATH_CSRN -> still absent.
        let k = keys(&[("CALSTAT", "BDF"), ("ATH_CSRC", "")]);
        assert!(calibrated_light_identity(&k).is_none());
    }

    #[test]
    fn normal_light_is_not_calibrated() {
        let k = keys(&[("OBJECT", "M42"), ("IMAGETYP", "LIGHT"), ("EXPTIME", "120")]);
        assert!(calibrated_light_identity(&k).is_none());
    }
}
