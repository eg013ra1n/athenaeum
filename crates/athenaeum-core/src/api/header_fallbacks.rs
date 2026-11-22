//! The FITS keyword rules (issue #21): read, replace and reset the user's ordered header
//! keywords for the twelve catalog fields. The document is stored as `catalog.header_fallbacks`
//! (compact form, see `crate::header_fallbacks::store`).
//!
//! The server owns `retired`: the frontend never shows it, so `set` and `reset` merge it from
//! the stored document (D12) and every keyword dropped from an order ends up retired.

use crate::api::{db, ApiError};
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
            "stored FITS keyword rules do not decode; replacing them without their retired keywords"
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
}
