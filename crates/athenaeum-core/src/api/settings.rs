//! The Settings page's one source of defaults (Settings redesign spec
//! 2026-09-18 §6): the same constructors the `reset_*` commands write, plus
//! every KV default in [`crate::settings::defaults`]. The frontend loads this
//! once per Settings mount and every field compares against it — a default
//! can never differ between "reset" and "show me the default".

use crate::analysis::config::AnalysisConfig;
use crate::api::ApiError;
use crate::calibration::config::CalibrationMatchingConfig;
use crate::logging::config::LoggingConfig;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal real-`Database` [`ServiceContext`] (tempdir SQLite, no
    /// keychain involved anywhere). Copied verbatim from
    /// `api::frame_sets::tests::test_ctx` (itself copied from `api::collab` /
    /// `api::sync`): a TEMPDIR-FILE-backed `Database` (not `:memory:`) so the
    /// pool can hand out multiple connections that all see one database.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::RwLock;
        use std::sync::{Arc, Mutex, OnceLock};

        let tmp = tempfile::tempdir().unwrap();
        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let db_cell = OnceLock::new();
        let _ = db_cell.set(database);
        let ctx = ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        };
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
}
