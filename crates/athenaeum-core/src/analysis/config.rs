use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::star_measure::{self, seeds};

fn default_detection_threshold() -> f64 {
    star_measure::DEFAULT_THRESHOLD
}
fn default_max_stars() -> u32 {
    500
}
fn default_batch_concurrency() -> u32 {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    // Each concurrent frame needs ~3 rayon pool threads for efficient PSF fitting.
    // Auto-tune: cores/3 gives good balance of throughput vs per-frame parallelism.
    (cores / 3).max(2).min(16) as u32
}

/// The PSF model the Analysis tab fits (PC-10): the stacking path's two public choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisPsfModel {
    /// Moffat with beta fixed at 4 (the default; no own-beta work).
    #[default]
    Moffat4,
    /// The frame's own beta, fitted from its brightest unsaturated stars.
    Auto,
}

/// Star detection and analysis configuration (PC-10).
/// Stored as JSON in the settings table under key "analysis.config". A stored document of the older shape decodes
/// with the new fields at their defaults (its removed keys are ignored).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct AnalysisConfig {
    /// Matched-filter detection threshold in sigma. Default: 6.0 (range 2.0 to 20.0)
    #[serde(default = "default_detection_threshold")]
    pub detection_threshold: f64,
    /// The PSF model of the fit. Default: moffat4
    #[serde(default)]
    pub psf_model: AnalysisPsfModel,
    /// The measurement budget N: the brightest good stars kept. Default: 500
    #[serde(default = "default_max_stars")]
    pub max_stars: u32,
    /// Concurrent frames during batch analysis. Default: auto (cores/3, min 2).
    /// Higher values increase throughput but use more memory (several hundred MB per concurrent 36 MP frame: a plane is 4 bytes a pixel and the measurement copies it; a debayered file keeps only its G plane while measured).
    #[serde(default = "default_batch_concurrency")]
    pub batch_concurrency: u32,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            detection_threshold: default_detection_threshold(),
            psf_model: AnalysisPsfModel::Moffat4,
            max_stars: default_max_stars(),
            batch_concurrency: default_batch_concurrency(),
        }
    }
}

impl AnalysisConfig {
    /// Deserialize from JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// SHA256 of the config JSON followed by the module's `MEASURE_VERSION`, for staleness detection.
    /// Excludes `batch_concurrency` since it's a runtime performance knob
    /// that doesn't affect analysis results.
    pub fn config_hash(&self) -> String {
        self.config_hash_for(star_measure::MEASURE_VERSION)
    }

    fn config_hash_for(&self, measure_version: u32) -> String {
        let mut for_hash = self.clone();
        for_hash.batch_concurrency = 0;
        let json = serde_json::to_string(&for_hash).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        hasher.update(measure_version.to_le_bytes());
        // digest 0.11 dropped `LowerHex` on `Output` (generic-array ->
        // hybrid-array); hex the bytes by hand — same lowercase digest string.
        hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Validate config values are within acceptable ranges.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.detection_threshold >= seeds::MIN_THRESHOLD
            && self.detection_threshold <= seeds::MAX_THRESHOLD)
        {
            return Err(format!(
                "detection_threshold must be between {} and {}",
                seeds::MIN_THRESHOLD,
                seeds::MAX_THRESHOLD
            ));
        }
        if self.max_stars < 10 || self.max_stars > 2000 {
            return Err("max_stars must be between 10 and 2000".into());
        }
        if self.batch_concurrency > 16 {
            return Err("batch_concurrency must be 0 (auto) or 1-16".into());
        }
        Ok(())
    }
}

/// Load analysis config from the database settings table.
/// Returns default config if not found; a stored document that does not decode is logged at `warn!` first.
pub fn load_config(conn: &rusqlite::Connection) -> AnalysisConfig {
    match crate::db::get_setting(conn, "analysis.config") {
        Ok(Some(json)) => AnalysisConfig::from_json(&json).unwrap_or_else(|e| {
            tracing::warn!(
                key = "analysis.config",
                error = %e,
                "stored analysis config does not decode; using the default"
            );
            AnalysisConfig::default()
        }),
        Ok(None) => AnalysisConfig::default(),
        Err(e) => {
            tracing::warn!(key = "analysis.config", error = %e, "analysis config could not be read; using the default");
            AnalysisConfig::default()
        }
    }
}

/// Save analysis config to the database settings table.
pub fn save_config(conn: &rusqlite::Connection, config: &AnalysisConfig) -> Result<(), String> {
    config.validate()?;
    let json = config.to_json().map_err(|e| e.to_string())?;
    crate::db::set_setting(conn, "analysis.config", &json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const OLD_CONFIG: &str = r#"{"detection_sigma":5.0,"min_star_area":5,"max_star_area":2000,"saturation_fraction":0.95,"max_stars":800,"trail_threshold":0.4,"mrs_layers":0,"measure_cap":2000,"fit_max_iter":25,"fit_tolerance":0.0001,"fit_max_rejects":5,"batch_concurrency":3}"#;

    #[test]
    fn an_old_stored_analysis_config_decodes_with_new_defaults() {
        let c = AnalysisConfig::from_json(OLD_CONFIG).unwrap();
        assert_eq!(c.max_stars, 800);
        assert_eq!(c.batch_concurrency, 3);
        assert_eq!(c.detection_threshold, 6.0);
        assert_eq!(c.psf_model, AnalysisPsfModel::Moffat4);
        c.validate().unwrap();
    }

    #[test]
    fn the_psf_model_is_lowercase_on_the_wire() {
        let c = AnalysisConfig {
            psf_model: AnalysisPsfModel::Auto,
            ..Default::default()
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["psf_model"], "auto");
        assert_eq!(
            serde_json::to_value(AnalysisPsfModel::Moffat4).unwrap(),
            "moffat4"
        );
    }

    #[test]
    fn validate_bounds_follow_pc_10() {
        let ok = AnalysisConfig::default();
        let with = |f: &dyn Fn(&mut AnalysisConfig)| {
            let mut c = ok.clone();
            f(&mut c);
            c.validate()
        };
        assert!(with(&|c| c.detection_threshold = 1.9).is_err());
        assert!(with(&|c| c.detection_threshold = 20.1).is_err());
        assert!(with(&|c| c.detection_threshold = f64::NAN).is_err());
        assert!(with(&|c| c.detection_threshold = 2.0).is_ok());
        assert!(with(&|c| c.detection_threshold = 20.0).is_ok());
        assert!(with(&|c| c.max_stars = 9).is_err());
        assert!(with(&|c| c.max_stars = 2001).is_err());
        assert!(with(&|c| c.batch_concurrency = 17).is_err());
    }

    #[test]
    fn config_hash_folds_measure_version_and_ignores_batch_concurrency() {
        let base = AnalysisConfig {
            batch_concurrency: 4,
            ..Default::default()
        };
        assert_eq!(
            base.config_hash(),
            "e5175903f1569d075093c45378c31d9a2999648082f3ed7e6e7b4f2398b8d62a",
            "the default config's hash at MEASURE_VERSION {}",
            star_measure::MEASURE_VERSION
        );
        assert_ne!(
            base.config_hash(),
            base.config_hash_for(star_measure::MEASURE_VERSION + 1)
        );
        let (a, b) = (
            AnalysisConfig {
                batch_concurrency: 1,
                ..Default::default()
            },
            AnalysisConfig {
                batch_concurrency: 9,
                ..Default::default()
            },
        );
        assert_eq!(a.config_hash(), b.config_hash());
        let c = AnalysisConfig {
            detection_threshold: 7.0,
            ..Default::default()
        };
        assert_ne!(a.config_hash(), c.config_hash());
    }

    /// Captures the `key` field of every WARN event.
    #[derive(Clone, Default)]
    struct WarnKeys(Arc<Mutex<Vec<String>>>);

    struct KeyVisitor(Option<String>);
    impl tracing::field::Visit for KeyVisitor {
        fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
            if f.name() == "key" {
                self.0 = Some(v.to_string());
            }
        }
        fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarnKeys {
        fn on_event(
            &self,
            e: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *e.metadata().level() == tracing::Level::WARN {
                let mut v = KeyVisitor(None);
                e.record(&mut v);
                if let Some(k) = v.0 {
                    self.0.lock().unwrap().push(k);
                }
            }
        }
    }

    #[test]
    fn an_undecodable_stored_config_warns_and_falls_back() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::set_setting(&conn, "analysis.config", "{not json").unwrap();
        let warns = WarnKeys::default();
        let _g = tracing_subscriber::registry()
            .with(warns.clone())
            .set_default();
        let c = load_config(&conn);
        assert_eq!(c.max_stars, AnalysisConfig::default().max_stars);
        assert_eq!(*warns.0.lock().unwrap(), vec!["analysis.config".to_string()]);
    }
}
