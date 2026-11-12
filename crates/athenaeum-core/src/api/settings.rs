//! The Settings page's one source of defaults (Settings redesign spec
//! 2026-09-18 §6): the same constructors the `reset_*` commands write, plus
//! every KV default in [`crate::settings::defaults`]. The frontend loads this
//! once per Settings mount and every field compares against it — a default
//! can never differ between "reset" and "show me the default".

use crate::analysis::config::AnalysisConfig;
use crate::api::ApiError;
use crate::calibration::config::CalibrationMatchingConfig;
use crate::logging::config::LoggingConfig;
use crate::monitor::MonitorService;
use crate::plate_solve::config::PlateSolveConfig;
use crate::services::ServiceContext;
use crate::stacking::config::StackingConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDefaults {
    pub kv: BTreeMap<String, String>,
    pub analysis: AnalysisConfig,
    pub plate_solve: PlateSolveConfig,
    pub calibration_matching: CalibrationMatchingConfig,
    pub logging: LoggingConfig,
    pub stacking: StackingConfig,
}

/// Every default a fresh Settings page needs, built without touching the
/// database — these are DEFAULTS, not the current effective settings.
pub fn get_settings_defaults(_ctx: &ServiceContext) -> Result<SettingsDefaults, ApiError> {
    Ok(SettingsDefaults {
        kv: crate::settings::defaults::all()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        analysis: AnalysisConfig::default(),
        plate_solve: PlateSolveConfig::default(),
        calibration_matching: CalibrationMatchingConfig::default(),
        logging: LoggingConfig::default(),
        stacking: StackingConfig::default(),
    })
}

/// Set a setting value (persists to database), then apply the keys that have a
/// live side effect: the blink memory-cache size, byte budget and retention
/// resize the cache now, and the monitoring keys wake the monitor loop.
/// `monitor` is passed explicitly because `MonitorService` lives beside (not
/// inside) `ServiceContext` on both hosts' state.
pub fn set_setting(
    ctx: &ServiceContext,
    key: &str,
    value: &str,
    monitor: &MonitorService,
) -> Result<(), ApiError> {
    let db = crate::api::db(ctx)?;
    let conn = db.conn();

    ctx.settings
        .persist_setting(&conn, key, value)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Update in-memory cache settings when they change
    if key == crate::settings::keys::BLINK_MEMORY_CACHE_SIZE {
        let size: usize = value.parse().unwrap_or(200);
        ctx.memory_cache.lock().unwrap().set_max_entries(size);
    } else if key == crate::settings::keys::BLINK_MEMORY_CACHE_MAX_MB {
        let mb = crate::settings::resolve_blink_memory_cache_max_mb(Some(value));
        ctx.memory_cache
            .lock()
            .unwrap()
            .set_max_bytes(mb * 1024 * 1024);
    } else if key == crate::settings::keys::BLINK_MEMORY_RETENTION_MINUTES {
        let minutes: u64 = value.parse().unwrap_or(30);
        ctx.memory_cache.lock().unwrap().set_retention(minutes);
    } else if key == crate::settings::keys::MONITORING_INTERVAL_MINUTES
        || key == crate::settings::keys::MONITORING_ENABLED_GLOBAL
    {
        // Wake the monitor loop so the new interval / enabled flag takes
        // effect now instead of after the current sleep expires.
        monitor.kick();
    }

    Ok(())
}

/// Persist the blink worker count and return the permit count the host's
/// semaphore must be rebuilt with. `threads` is `0..=max_threads` (`0` = auto,
/// half of `max_threads`, at least 2); a larger value is refused and nothing is
/// stored. Both hosts call this, then rebuild their own semaphore.
pub fn set_blink_threads(
    ctx: &ServiceContext,
    threads: u32,
    max_threads: usize,
) -> Result<usize, ApiError> {
    let max = u32::try_from(max_threads).unwrap_or(u32::MAX);
    if threads > max {
        return Err(ApiError::Invalid(format!(
            "Blink threads must be between 0 and {} (0 = auto)",
            max
        )));
    }

    {
        let db = crate::api::db(ctx)?;
        let conn = db.conn();
        ctx.settings
            .persist_setting(
                &conn,
                crate::settings::keys::BLINK_THREADS,
                &threads.to_string(),
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    Ok(if threads == 0 {
        (max_threads / 2).max(2)
    } else {
        threads as usize
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real-`Database` [`ServiceContext`] over a tempdir catalog.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        (tmp, ctx)
    }

    /// `reset_*` each construct `T::default()` (`api/analysis.rs`,
    /// `api/calibration.rs`, `api/stacking.rs`) — pin the equivalence through
    /// serde so a future reset that seeds differently fails here.
    #[test]
    fn typed_defaults_equal_what_reset_writes() {
        let (_tmp, ctx) = test_ctx();
        let d = get_settings_defaults(&ctx).unwrap();
        assert_eq!(
            serde_json::to_value(&d.analysis).unwrap(),
            serde_json::to_value(AnalysisConfig::default()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&d.plate_solve).unwrap(),
            serde_json::to_value(PlateSolveConfig::default()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&d.calibration_matching).unwrap(),
            serde_json::to_value(CalibrationMatchingConfig::default()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&d.stacking).unwrap(),
            serde_json::to_value(StackingConfig::default()).unwrap()
        );
        assert_eq!(
            d.kv.get("calibration.master_format").map(String::as_str),
            Some("fits")
        );
    }

    /// A set value persists and reads back through the same precedence the
    /// frontend uses. The two keys also run the cache-resize and the
    /// monitor-kick branches, but only that they do not fail: neither the
    /// cache nor the monitor exposes what they were set to.
    #[test]
    fn set_setting_persists_and_reads_back() {
        let (_tmp, ctx) = test_ctx();
        let monitor = MonitorService::new();

        set_setting(&ctx, crate::settings::keys::BLINK_MEMORY_CACHE_SIZE, "120", &monitor).expect("set");
        set_setting(&ctx, crate::settings::keys::MONITORING_ENABLED_GLOBAL, "false", &monitor)
            .expect("set monitoring key");

        let conn = ctx.db.get().unwrap().conn();
        assert_eq!(
            ctx.settings
                .get_with_precedence(&conn, crate::settings::keys::BLINK_MEMORY_CACHE_SIZE, "")
                .unwrap(),
            "120"
        );
        assert_eq!(
            ctx.settings
                .get_with_precedence(&conn, crate::settings::keys::MONITORING_ENABLED_GLOBAL, "")
                .unwrap(),
            "false"
        );
    }

    fn stored_blink_threads(ctx: &ServiceContext) -> String {
        let conn = ctx.db.get().unwrap().conn();
        ctx.settings
            .get_with_precedence(&conn, crate::settings::keys::BLINK_THREADS, "unset")
            .unwrap()
    }

    #[test]
    fn set_blink_threads_above_max_is_refused_and_not_stored() {
        let (_tmp, ctx) = test_ctx();
        set_blink_threads(&ctx, 3, 8).expect("in range");
        let err = set_blink_threads(&ctx, 9, 8).expect_err("above max");
        match err {
            ApiError::Invalid(m) => {
                assert_eq!(m, "Blink threads must be between 0 and 8 (0 = auto)")
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        assert_eq!(stored_blink_threads(&ctx), "3");
    }

    #[test]
    fn set_blink_threads_returns_permits_and_stores_value() {
        let (_tmp, ctx) = test_ctx();
        assert_eq!(set_blink_threads(&ctx, 0, 16).unwrap(), 8);
        assert_eq!(stored_blink_threads(&ctx), "0");
        assert_eq!(set_blink_threads(&ctx, 0, 2).unwrap(), 2);
        assert_eq!(set_blink_threads(&ctx, 5, 16).unwrap(), 5);
        assert_eq!(stored_blink_threads(&ctx), "5");
        assert_eq!(set_blink_threads(&ctx, 16, 16).unwrap(), 16);
    }
}
