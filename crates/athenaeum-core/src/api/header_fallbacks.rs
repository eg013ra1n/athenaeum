//! The FITS keyword rules (issue #21): read, replace and reset the user's ordered header
//! keywords for the twelve catalog fields. The document is stored as `catalog.header_fallbacks`
//! (compact form, see `crate::header_fallbacks::store`).
//!
//! The server owns `retired`: the frontend never shows it, so `set` and `reset` merge it from
//! the stored document (D12) and every keyword dropped from an order ends up retired.

use crate::api::{db, ApiError, PathPolicy};
use crate::header_fallbacks::apply::HeaderFallbackCounts;
use crate::events::ProgressEmitter;
use crate::header_fallbacks::transfer::{self, HeaderFallbackImportPlan, ImportError};
use crate::header_fallbacks::{self, HeaderFallbackConfig};
use crate::services::ServiceContext;

const SAVEPOINT: &str = "header_fallbacks_rules";

fn internal(what: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "{what}");
    ApiError::Internal(format!("{what}: {e}"))
}

/// The stored rules for a merge. A row that does not decode is logged and read as the default,
/// so the write that follows replaces it (its `retired` list is unreadable and lost).
fn previous_for_merge(conn: &rusqlite::Connection) -> HeaderFallbackConfig {
    header_fallbacks::load(conn).unwrap_or_else(|e| {
        tracing::error!(
            key = crate::settings::keys::CATALOG_HEADER_FALLBACKS,
            error = %format!("{e:#}"),
            "stored FITS keyword rules could not be read; replacing them without their retired keywords"
        );
        HeaderFallbackConfig::default()
    })
}

/// The stored rules, expanded: all twelve fields, `retired` included. No stored row reads the
/// default document. A stored document that cannot be decoded is an error (Settings shows it).
pub fn get_header_fallbacks(ctx: &ServiceContext) -> Result<HeaderFallbackConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    header_fallbacks::load(&conn).map_err(|e| internal("cannot read the FITS keyword rules", format!("{e:#}")))
}

/// Replace the rules. A document that breaks a rule is refused (`ApiError::Invalid`, nothing
/// stored); `retired` is merged from the stored document, so the keywords this call removes
/// from an order are retired. A stored row that does not decode is logged and replaced (the
/// way out of a broken row).
pub fn set_header_fallbacks(ctx: &ServiceContext, config: HeaderFallbackConfig) -> Result<(), ApiError> {
    let validated = header_fallbacks::validate(&config).map_err(|e| {
        tracing::warn!(reason = %e, "FITS keyword rules refused");
        ApiError::Invalid(e.to_string())
    })?;
    let db = db(ctx)?;
    let conn = db.conn();
    let tx = crate::db::SavepointGuard::new(&conn, SAVEPOINT)
        .map_err(|e| internal("cannot start the FITS keyword rules write", e))?;
    let previous = previous_for_merge(&conn);
    let merged = validated.with_retirements_from(&previous);
    header_fallbacks::store(&conn, &merged)
        .map_err(|e| internal("cannot store the FITS keyword rules", format!("{e:#}")))?;
    tx.commit().map_err(|e| internal("cannot commit the FITS keyword rules", e))?;
    tracing::info!("FITS keyword rules stored");
    Ok(())
}

/// Every field back to its default chain. What the orders held beyond the defaults is retired.
/// Returns the stored, expanded document. A stored row that does not decode is logged and
/// replaced by the defaults.
pub fn reset_header_fallbacks(ctx: &ServiceContext) -> Result<HeaderFallbackConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let tx = crate::db::SavepointGuard::new(&conn, SAVEPOINT)
        .map_err(|e| internal("cannot start the FITS keyword rules reset", e))?;
    let previous = previous_for_merge(&conn);
    let reset = previous.reset_orders();
    header_fallbacks::store(&conn, &reset)
        .map_err(|e| internal("cannot store the FITS keyword rules", format!("{e:#}")))?;
    tx.commit().map_err(|e| internal("cannot commit the FITS keyword rules", e))?;
    tracing::info!("FITS keyword rules reset to the default orders");
    Ok(reset)
}

/// What the stored rules would change in the catalog, counted from the stored headers; writes
/// nothing. Blocking (it reads every frame): the hosts call it from `spawn_blocking`. A stored
/// document that cannot be decoded is an error, as for [`get_header_fallbacks`].
pub fn preview_header_fallbacks(ctx: &ServiceContext) -> Result<HeaderFallbackCounts, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let rules = header_fallbacks::load(&conn)
        .map_err(|e| internal("cannot read the FITS keyword rules", format!("{e:#}")))?;
    header_fallbacks::apply::preview(&conn, &rules)
        .map_err(|e| internal("cannot preview the FITS keyword rules", format!("{e:#}")))
}

/// Clears `apply_running` when the run ends, however it ends.
struct ApplyRunning<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for ApplyRunning<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Re-read the catalog's frames from their stored headers under the stored rules and write what
/// changed (D3, D16, D17); regroups the raw calibration frames whose grouping values changed.
/// Progress goes out as `header-fallbacks-progress`. Blocking: the hosts call it from
/// `spawn_blocking`. A second Apply on this context while one runs is a conflict.
pub fn apply_header_fallbacks(
    ctx: &ServiceContext,
    emitter: &dyn ProgressEmitter,
) -> Result<HeaderFallbackCounts, ApiError> {
    use std::sync::atomic::Ordering;
    if ctx.apply_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        tracing::warn!("header fallbacks apply refused: one is running");
        return Err(ApiError::Conflict("FITS keyword rules are already being applied".to_string()));
    }
    let _running = ApplyRunning(&ctx.apply_running);
    let db = db(ctx)?;
    let conn = db.conn();
    let rules = header_fallbacks::load(&conn)
        .map_err(|e| internal("cannot read the FITS keyword rules", format!("{e:#}")))?;
    let gap_hours = ctx
        .settings
        .get_session_gap_threshold_hours(&conn)
        .map_err(|e| internal("cannot read the session gap threshold", format!("{e:#}")))?;
    let report = header_fallbacks::apply::apply(&conn, &rules, gap_hours, emitter)
        .map_err(|e| internal("cannot apply the FITS keyword rules", format!("{e:#}")))?;
    #[cfg(all(feature = "render", feature = "solver"))]
    crate::api::collab_autopublish::request_auto_publish_for_sets(&report.frame_sets_touched);
    // The frames and the calibration sets are written and auto-publish is asked for; the nights
    // that could not be re-derived are the error (the same kind as any other Apply failure).
    if !report.failed_night_sets.is_empty() {
        let ids = report.failed_night_sets.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
        return Err(internal(
            "cannot apply the FITS keyword rules",
            format!("the nights of frame set(s) {ids} could not be re-derived; the frames and calibration sets are updated"),
        ));
    }
    Ok(report.counts)
}

/// The file name of a path, for messages (never the whole path).
fn file_name_of(path: &std::path::Path) -> String {
    path.file_name().map_or_else(|| "the file".to_string(), |n| n.to_string_lossy().into_owned())
}

/// The stored rules as the JSON file of the spec's §5.7 (only the fields whose order differs
/// from the default; never `retired`). With `path` the text is also written there, after
/// `policy` has checked it; the text is returned either way (the web host downloads it).
pub fn export_header_fallbacks(
    ctx: &ServiceContext,
    path: Option<String>,
    policy: &PathPolicy,
) -> Result<String, ApiError> {
    let rules = {
        let db = db(ctx)?;
        let conn = db.conn();
        header_fallbacks::load(&conn)
            .map_err(|e| internal("cannot read the FITS keyword rules", format!("{e:#}")))?
    };
    let text = transfer::export_text(&rules);
    if let Some(path) = path {
        let path = std::path::PathBuf::from(path);
        policy.check(&path)?;
        std::fs::write(&path, &text)
            .map_err(|e| internal(&format!("cannot write {}", file_name_of(&path)), e))?;
    }
    tracing::info!("FITS keyword rules exported");
    Ok(text)
}

fn refuse_import(e: ImportError) -> ApiError {
    tracing::warn!(reason = %e, "header fallbacks import refused");
    ApiError::Invalid(e.to_string())
}

/// Read a rules file (its `text`, or a file at `path`; exactly one of them) against the stored
/// rules and say what importing it would change. Nothing is stored: the frontend saves
/// `plan.config` through the normal set once the user confirms. A file that breaks a rule is
/// refused as a whole (`ApiError::Invalid`, the reason logged).
pub fn plan_header_fallbacks_import(
    ctx: &ServiceContext,
    text: Option<String>,
    path: Option<String>,
    policy: &PathPolicy,
) -> Result<HeaderFallbackImportPlan, ApiError> {
    let text = match (text, path) {
        (Some(text), None) => text,
        (None, Some(path)) => {
            let path = std::path::PathBuf::from(path);
            policy.check(&path)?;
            let name = file_name_of(&path);
            let len = std::fs::metadata(&path).map_err(|e| {
                tracing::warn!(error = %e, "header fallbacks import file unreadable");
                ApiError::Invalid(format!("cannot read {name}: {e}"))
            })?.len();
            if len > transfer::MAX_IMPORT_BYTES as u64 {
                return Err(refuse_import(ImportError::TooLarge(usize::try_from(len).unwrap_or(usize::MAX))));
            }
            std::fs::read_to_string(&path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    refuse_import(ImportError::NotJson("the file is not UTF-8 text".to_string()))
                } else {
                    tracing::warn!(error = %e, "header fallbacks import file unreadable");
                    ApiError::Invalid(format!("cannot read {name}: {e}"))
                }
            })?
        }
        _ => return Err(ApiError::Invalid("give the file's text or its path".to_string())),
    };
    let current = {
        let db = db(ctx)?;
        let conn = db.conn();
        previous_for_merge(&conn)
    };
    transfer::plan_import(&current, &text).map_err(refuse_import)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header_fallbacks::{FieldRule, HeaderField};

    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        (tmp, ctx)
    }

    fn doc(field: HeaderField, order: &[&str]) -> HeaderFallbackConfig {
        let mut c = HeaderFallbackConfig::default();
        c.fields.insert(
            field,
            FieldRule { order: order.iter().map(|s| (*s).to_string()).collect(), retired: vec![] },
        );
        c
    }

    fn raw_row(ctx: &ServiceContext) -> Option<String> {
        let conn = ctx.db.get().unwrap().conn();
        crate::db::get_setting(&conn, crate::settings::keys::CATALOG_HEADER_FALLBACKS).unwrap()
    }

    #[test]
    fn a_fresh_catalog_reads_the_default_document() {
        let (_tmp, ctx) = test_ctx();
        assert_eq!(get_header_fallbacks(&ctx).unwrap(), HeaderFallbackConfig::default());
    }

    #[test]
    fn set_then_get_round_trips_and_stores_the_compact_form() {
        let (_tmp, ctx) = test_ctx();
        set_header_fallbacks(&ctx, doc(HeaderField::Gain, &["G2", "GAIN"])).unwrap();
        let got = get_header_fallbacks(&ctx).unwrap();
        assert_eq!(got.order(HeaderField::Gain), ["G2", "GAIN"]);
        assert_eq!(got.fields.len(), 12);
        let raw: serde_json::Value = serde_json::from_str(&raw_row(&ctx).unwrap()).unwrap();
        assert_eq!(raw["fields"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn set_refuses_a_bad_document_and_stores_nothing() {
        let (_tmp, ctx) = test_ctx();
        let err = set_header_fallbacks(&ctx, doc(HeaderField::CcdTemp, &["CCD-TEMP"])).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(ref m) if m.contains("DET-TEMP")), "{err}");
        let err = set_header_fallbacks(&ctx, doc(HeaderField::Gain, &["GAIN", "A=B"])).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err}");
        assert_eq!(raw_row(&ctx), None);
    }

    #[test]
    fn defaults_are_not_written() {
        let (_tmp, ctx) = test_ctx();
        set_header_fallbacks(&ctx, HeaderFallbackConfig::default()).unwrap();
        assert_eq!(raw_row(&ctx), None);
    }

    #[test]
    fn set_moves_a_removed_user_keyword_to_retired() {
        let (_tmp, ctx) = test_ctx();
        set_header_fallbacks(&ctx, doc(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "MY-TEMP"])).unwrap();
        set_header_fallbacks(&ctx, HeaderFallbackConfig::default()).unwrap();
        let got = get_header_fallbacks(&ctx).unwrap();
        assert_eq!(got.order(HeaderField::CcdTemp), ["CCD-TEMP", "DET-TEMP"]);
        assert_eq!(got.retired(HeaderField::CcdTemp), ["MY-TEMP"]);
        // A client that sends an empty `retired` does not erase it.
        set_header_fallbacks(&ctx, doc(HeaderField::Gain, &["GAIN", "G2"])).unwrap();
        assert_eq!(get_header_fallbacks(&ctx).unwrap().retired(HeaderField::CcdTemp), ["MY-TEMP"]);
        // Putting the keyword back takes it out of `retired`.
        set_header_fallbacks(&ctx, doc(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "MY-TEMP"])).unwrap();
        assert!(get_header_fallbacks(&ctx).unwrap().retired(HeaderField::CcdTemp).is_empty());
    }

    #[test]
    fn reset_retires_user_keywords_and_restores_default_orders() {
        let (_tmp, ctx) = test_ctx();
        set_header_fallbacks(&ctx, doc(HeaderField::CcdTemp, &["MY-TEMP", "CCD-TEMP", "DET-TEMP"])).unwrap();
        let reset = reset_header_fallbacks(&ctx).unwrap();
        assert_eq!(reset.order(HeaderField::CcdTemp), ["CCD-TEMP", "DET-TEMP"]);
        assert_eq!(reset.retired(HeaderField::CcdTemp), ["MY-TEMP"]);
        assert_eq!(get_header_fallbacks(&ctx).unwrap(), reset);
    }

    #[test]
    fn reset_on_a_fresh_catalog_returns_and_stores_the_defaults() {
        let (_tmp, ctx) = test_ctx();
        assert_eq!(reset_header_fallbacks(&ctx).unwrap(), HeaderFallbackConfig::default());
        assert_eq!(raw_row(&ctx), None);
    }

    #[test]
    fn get_reports_a_stored_document_that_does_not_decode() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            crate::db::set_setting(&conn, crate::settings::keys::CATALOG_HEADER_FALLBACKS, "not json").unwrap();
        }
        assert!(matches!(get_header_fallbacks(&ctx), Err(ApiError::Internal(_))));
    }

    fn write_garbage(ctx: &ServiceContext) {
        let conn = ctx.db.get().unwrap().conn();
        crate::db::set_setting(&conn, crate::settings::keys::CATALOG_HEADER_FALLBACKS, "not json").unwrap();
    }

    #[test]
    fn set_recovers_from_a_stored_document_that_does_not_decode() {
        let (_tmp, ctx) = test_ctx();
        write_garbage(&ctx);
        set_header_fallbacks(&ctx, doc(HeaderField::Gain, &["G2", "GAIN"])).unwrap();
        let got = get_header_fallbacks(&ctx).unwrap();
        assert_eq!(got.order(HeaderField::Gain), ["G2", "GAIN"]);
        assert!(got.retired(HeaderField::Gain).is_empty());
    }

    #[test]
    fn reset_recovers_from_a_stored_document_that_does_not_decode() {
        let (_tmp, ctx) = test_ctx();
        write_garbage(&ctx);
        assert_eq!(reset_header_fallbacks(&ctx).unwrap(), HeaderFallbackConfig::default());
        assert_eq!(raw_row(&ctx), None);
        assert_eq!(get_header_fallbacks(&ctx).unwrap(), HeaderFallbackConfig::default());
    }

    #[test]
    fn preview_reads_the_stored_rules_and_counts_nothing_on_an_empty_catalog() {
        let (_tmp, ctx) = test_ctx();
        assert_eq!(preview_header_fallbacks(&ctx).unwrap(), HeaderFallbackCounts::default());
    }

    #[test]
    fn preview_uses_the_stored_rules_and_reports_an_unreadable_row() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES ('a.fits', 'a.fits', 1, '2026-01-01', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (file_id, imagetyp) VALUES (1, 'Light')", []).unwrap();
            conn.execute(
                "INSERT INTO fits_header (file_id, header) VALUES (1, 'MY-TEMP = -3')",
                [],
            )
            .unwrap();
        }
        // The standard keywords find nothing in this header; with MY-TEMP added the column is filled.
        assert_eq!(preview_header_fallbacks(&ctx).unwrap().frames_changed, 0);
        set_header_fallbacks(&ctx, doc(HeaderField::CcdTemp, &["MY-TEMP", "CCD-TEMP", "DET-TEMP"])).unwrap();
        let counts = preview_header_fallbacks(&ctx).unwrap();
        assert_eq!(counts.frames_changed, 1);
        assert_eq!(counts.by_field.len(), 1);
        assert_eq!(counts.by_field[0].field, HeaderField::CcdTemp);
        write_garbage(&ctx);
        assert!(matches!(preview_header_fallbacks(&ctx), Err(ApiError::Internal(_))));
    }

    #[test]
    fn apply_writes_under_the_stored_rules() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES ('a.fits', 'a.fits', 1, '2026-01-01', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (file_id, imagetyp) VALUES (1, 'Light')", []).unwrap();
            conn.execute("INSERT INTO fits_header (file_id, header) VALUES (1, 'MY-TEMP = -3')", []).unwrap();
        }
        set_header_fallbacks(&ctx, doc(HeaderField::CcdTemp, &["MY-TEMP", "CCD-TEMP", "DET-TEMP"])).unwrap();
        let counts = apply_header_fallbacks(&ctx, &crate::events::NullEmitter).unwrap();
        assert_eq!(counts.frames_changed, 1);
        let conn = ctx.db.get().unwrap().conn();
        let t: f64 = conn.query_row("SELECT ccd_temp FROM frames WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(t, -3.0);
        drop(conn);
        assert_eq!(preview_header_fallbacks(&ctx).unwrap(), HeaderFallbackCounts::default());
    }

    #[test]
    fn failed_nights_are_an_error_after_the_frames_are_written() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = ctx.db.get().unwrap().conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES ('a.fits', 'a.fits', 1, '2026-01-01', 'FITS')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, date_obs) VALUES (1, 'Light', '2026-01-01T22:00:00Z')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO fits_header (file_id, header) VALUES (1, 'EXPTIME = 30')", []).unwrap();
            conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
            crate::sessions::rederive_for_frame_set(&conn, 1, &[1], 6.0).unwrap();
            conn.execute_batch(
                "CREATE TRIGGER refuse_night BEFORE INSERT ON imaging_nights
                 BEGIN SELECT RAISE(ABORT, 'refused'); END",
            )
            .unwrap();
        }
        let err = apply_header_fallbacks(&ctx, &crate::events::NullEmitter).unwrap_err();
        assert!(matches!(err, ApiError::Internal(ref m) if m.contains("frame set(s) 1 ")), "{err}");
        let conn = ctx.db.get().unwrap().conn();
        let exptime: f64 = conn.query_row("SELECT exptime FROM frames WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(exptime, 30.0, "the frame is written");
        assert!(!ctx.apply_running.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn second_apply_while_one_runs_is_a_conflict() {
        use std::sync::atomic::Ordering;
        let (_tmp, ctx) = test_ctx();
        let (_tmp2, other) = test_ctx();
        ctx.apply_running.store(true, Ordering::SeqCst);
        let err = apply_header_fallbacks(&ctx, &crate::events::NullEmitter).unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "{err}");
        // A refused call leaves the running one's flag alone; another context is not affected.
        assert!(ctx.apply_running.load(Ordering::SeqCst));
        assert!(apply_header_fallbacks(&other, &crate::events::NullEmitter).is_ok());
        ctx.apply_running.store(false, Ordering::SeqCst);
        assert!(apply_header_fallbacks(&ctx, &crate::events::NullEmitter).is_ok());
        assert!(!ctx.apply_running.load(Ordering::SeqCst), "the flag is released after a run");
    }

    #[test]
    fn a_failed_apply_releases_the_flag() {
        use std::sync::atomic::Ordering;
        let (_tmp, ctx) = test_ctx();
        write_garbage(&ctx);
        assert!(matches!(apply_header_fallbacks(&ctx, &crate::events::NullEmitter), Err(ApiError::Internal(_))));
        assert!(!ctx.apply_running.load(Ordering::SeqCst));
    }

    fn transfer_file(fields: &str) -> String {
        format!(r#"{{"format":"athenaeum.header-fallbacks","version":1,"fields":{fields}}}"#)
    }

    #[test]
    fn export_of_a_fresh_catalog_names_no_field_and_matches_the_core_text() {
        let (_tmp, ctx) = test_ctx();
        let text = export_header_fallbacks(&ctx, None, &PathPolicy::AllowAll).unwrap();
        assert_eq!(text, transfer::export_text(&HeaderFallbackConfig::default()));
    }

    #[test]
    fn export_to_a_path_writes_the_same_text() {
        let (tmp, ctx) = test_ctx();
        set_header_fallbacks(&ctx, doc(HeaderField::Gain, &["G2", "GAIN"])).unwrap();
        let file = tmp.path().join("rules.json");
        let text = export_header_fallbacks(
            &ctx,
            Some(file.to_string_lossy().into_owned()),
            &PathPolicy::AllowAll,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
        assert!(text.contains("\"gain\""), "{text}");
    }

    #[test]
    fn export_to_a_path_is_refused_on_a_restricted_policy() {
        let (tmp, ctx) = test_ctx();
        let file = tmp.path().join("rules.json");
        let err = export_header_fallbacks(
            &ctx,
            Some(file.to_string_lossy().into_owned()),
            &PathPolicy::AllowedRoots(Vec::new()),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(_)), "{err}");
        assert!(!file.exists());
    }

    #[test]
    fn export_to_an_unwritable_path_names_the_file() {
        let (tmp, ctx) = test_ctx();
        let file = tmp.path().join("no-such-dir").join("rules.json");
        let err = export_header_fallbacks(
            &ctx,
            Some(file.to_string_lossy().into_owned()),
            &PathPolicy::AllowAll,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Internal(ref m) if m.contains("rules.json")), "{err}");
    }

    #[test]
    fn import_by_path_is_refused_on_a_restricted_policy() {
        let (tmp, ctx) = test_ctx();
        let file = tmp.path().join("rules.json");
        std::fs::write(&file, transfer_file("{}")).unwrap();
        let err = plan_header_fallbacks_import(
            &ctx,
            None,
            Some(file.to_string_lossy().into_owned()),
            &PathPolicy::AllowedRoots(Vec::new()),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(_)), "{err}");
    }

    #[test]
    fn import_by_path_and_by_text_give_the_same_plan() {
        let (tmp, ctx) = test_ctx();
        let body = transfer_file(r#"{"gain":["MY-G","GAIN"]}"#);
        let file = tmp.path().join("rules.json");
        std::fs::write(&file, &body).unwrap();
        let by_path = plan_header_fallbacks_import(
            &ctx,
            None,
            Some(file.to_string_lossy().into_owned()),
            &PathPolicy::AllowAll,
        )
        .unwrap();
        let by_text =
            plan_header_fallbacks_import(&ctx, Some(body), None, &PathPolicy::AllowedRoots(Vec::new()))
                .unwrap();
        assert_eq!(by_path, by_text);
        assert_eq!(by_text.config.order(HeaderField::Gain), ["MY-G", "GAIN"]);
        assert_eq!(by_text.changes.len(), 1);
    }

    #[test]
    fn import_needs_exactly_one_of_text_and_path() {
        let (_tmp, ctx) = test_ctx();
        let neither = plan_header_fallbacks_import(&ctx, None, None, &PathPolicy::AllowAll).unwrap_err();
        assert!(matches!(neither, ApiError::Invalid(ref m) if m.contains("text or its path")), "{neither}");
        let both = plan_header_fallbacks_import(
            &ctx,
            Some(transfer_file("{}")),
            Some("rules.json".to_string()),
            &PathPolicy::AllowAll,
        )
        .unwrap_err();
        assert!(matches!(both, ApiError::Invalid(_)), "{both}");
    }

    #[test]
    fn import_by_path_refuses_a_file_over_one_mib_and_one_that_is_not_text() {
        let (tmp, ctx) = test_ctx();
        let big = tmp.path().join("big.json");
        std::fs::write(&big, vec![b' '; transfer::MAX_IMPORT_BYTES + 1]).unwrap();
        let err = plan_header_fallbacks_import(
            &ctx,
            None,
            Some(big.to_string_lossy().into_owned()),
            &PathPolicy::AllowAll,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(ref m) if m.contains("larger than 1 MB")), "{err}");
        let bin = tmp.path().join("bin.json");
        std::fs::write(&bin, [0xff_u8, 0xfe, 0x00, 0x50, 0x4b]).unwrap();
        let err = plan_header_fallbacks_import(
            &ctx,
            None,
            Some(bin.to_string_lossy().into_owned()),
            &PathPolicy::AllowAll,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(ref m) if m.contains("not a JSON file")), "{err}");
    }

    #[test]
    fn import_of_a_missing_file_is_invalid_and_names_the_file() {
        let (tmp, ctx) = test_ctx();
        let err = plan_header_fallbacks_import(
            &ctx,
            None,
            Some(tmp.path().join("absent.json").to_string_lossy().into_owned()),
            &PathPolicy::AllowAll,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(ref m) if m.contains("absent.json")), "{err}");
    }

    #[test]
    fn a_plan_reads_the_stored_rules_and_stores_nothing() {
        let (_tmp, ctx) = test_ctx();
        set_header_fallbacks(&ctx, doc(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP", "X"])).unwrap();
        let before = raw_row(&ctx);
        let plan = plan_header_fallbacks_import(
            &ctx,
            Some(transfer_file(r#"{"ccdTemp":["DET-TEMP","CCD-TEMP"]}"#)),
            None,
            &PathPolicy::AllowAll,
        )
        .unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].before, ["CCD-TEMP", "DET-TEMP", "X"]);
        assert_eq!(plan.config.retired(HeaderField::CcdTemp), ["X"]);
        assert_eq!(raw_row(&ctx), before);
    }

    #[test]
    fn refused_import_changes_nothing() {
        let (_tmp, ctx) = test_ctx();
        set_header_fallbacks(&ctx, doc(HeaderField::Gain, &["MY-G", "GAIN"])).unwrap();
        let row = raw_row(&ctx);
        let loaded = get_header_fallbacks(&ctx).unwrap();
        for body in [
            "\u{0}PK\u{3}".to_string(),
            r#"{"format":"x","version":1,"fields":{}}"#.to_string(),
            r#"{"format":"athenaeum.header-fallbacks","version":2,"fields":{}}"#.to_string(),
            r#"{"format":"athenaeum.header-fallbacks","version":1}"#.to_string(),
            transfer_file(r#"{"nope":[]}"#),
            transfer_file(r#"{"gain":"GAIN"}"#),
            transfer_file(r#"{"gain":["GAIN",3]}"#),
            transfer_file(r#"{"gain":["GAIN","A=B"]}"#),
            transfer_file(r#"{"ccdTemp":["CCD-TEMP","DET-TEMP","ccd-temp"]}"#),
        ] {
            let err = plan_header_fallbacks_import(&ctx, Some(body.clone()), None, &PathPolicy::AllowAll)
                .unwrap_err();
            assert!(matches!(err, ApiError::Invalid(_)), "{body}: {err}");
            assert_eq!(raw_row(&ctx), row, "{body}");
            assert_eq!(get_header_fallbacks(&ctx).unwrap(), loaded, "{body}");
        }
    }
}
