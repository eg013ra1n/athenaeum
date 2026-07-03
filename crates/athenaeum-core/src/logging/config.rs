use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SETTINGS_KEY: &str = "logging.config";
const LEVELS: [&str; 4] = ["error", "warn", "info", "debug"]; // trace is env-only by spec

/// UI module key -> tracing filter targets.
const MODULE_TARGETS: [(&str, &[&str]); 4] = [
    ("scanner", &["athenaeum_core::scanner"]),
    ("solver", &["athenaeum_core::plate_solve", "solvemyastro"]),
    ("calibration", &["athenaeum_core::calibration"]),
    (
        "archive",
        &["athenaeum_core::archive", "athenaeum_core::file_op"],
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoggingConfig {
    pub level: String,
    pub modules: BTreeMap<String, String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            modules: BTreeMap::new(),
        }
    }
}

impl LoggingConfig {
    pub fn to_directives(&self) -> String {
        let base = if LEVELS.contains(&self.level.as_str()) {
            self.level.as_str()
        } else {
            "info"
        };
        let mut out = base.to_string();
        for (key, level) in &self.modules {
            if !LEVELS.contains(&level.as_str()) {
                continue;
            }
            if let Some((_, targets)) = MODULE_TARGETS.iter().find(|(k, _)| k == key) {
                for t in *targets {
                    out.push_str(&format!(",{t}={level}"));
                }
            }
        }
        out
    }

    /// Reject a config with an out-of-range top-level `level` or an
    /// out-of-range per-module level, plus a belt-and-suspenders check that
    /// the resulting directive string actually parses as an `EnvFilter`.
    ///
    /// Unknown module *keys* (e.g. `"bogus"`) are intentionally NOT an error
    /// here — `to_directives()` already skips them silently by design (see
    /// `unknown_module_key_is_skipped_not_fatal`), so rejecting them at the
    /// command boundary would be inconsistent with that established
    /// behavior. Only level *values* (top-level and per-module) are checked
    /// against the accepted set.
    pub fn validate(&self) -> Result<(), String> {
        if !LEVELS.contains(&self.level.as_str()) {
            return Err(format!(
                "invalid level {:?}; expected one of {:?}",
                self.level, LEVELS
            ));
        }
        for (module, level) in &self.modules {
            if !LEVELS.contains(&level.as_str()) {
                return Err(format!(
                    "invalid level {:?} for module {:?}; expected one of {:?}",
                    level, module, LEVELS
                ));
            }
        }
        self.to_directives()
            .parse::<tracing_subscriber::EnvFilter>()
            .map_err(|e| format!("invalid logging directives: {e}"))?;
        Ok(())
    }
}

/// Response body for `get_logging_config` — the effective config plus
/// whether the `ATHENAEUM_LOG` env override is active (in which case
/// `apply_config` no-ops and the UI should show the config as read-only).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingConfigResponse {
    pub config: LoggingConfig,
    pub env_override_active: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_config_is_info() {
        assert_eq!(LoggingConfig::default().to_directives(), "info");
    }
    #[test]
    fn module_overrides_map_to_targets() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "warn".into();
        cfg.modules.insert("scanner".into(), "debug".into());
        cfg.modules.insert("solver".into(), "debug".into());
        // solver expands to BOTH the core plate_solve target and the solvemyastro crate
        assert_eq!(
            cfg.to_directives(),
            "warn,athenaeum_core::scanner=debug,athenaeum_core::plate_solve=debug,solvemyastro=debug"
        );
    }
    #[test]
    fn unknown_module_key_is_skipped_not_fatal() {
        let mut cfg = LoggingConfig::default();
        cfg.modules.insert("bogus".into(), "debug".into());
        assert_eq!(cfg.to_directives(), "info");
    }
    #[test]
    fn invalid_level_falls_back_to_info() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "chatty".into();
        assert_eq!(cfg.to_directives(), "info");
    }

    #[test]
    fn validate_accepts_default_config() {
        assert!(LoggingConfig::default().validate().is_ok());
    }

    #[test]
    fn validate_accepts_known_module_overrides() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "warn".into();
        cfg.modules.insert("scanner".into(), "debug".into());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_invalid_top_level_level() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "chatty".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_module_level() {
        let mut cfg = LoggingConfig::default();
        cfg.modules.insert("scanner".into(), "chatty".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_unknown_module_key_with_valid_level() {
        // Unknown module keys are skipped by to_directives(), not rejected —
        // validate() must stay consistent with that (see
        // unknown_module_key_is_skipped_not_fatal above).
        let mut cfg = LoggingConfig::default();
        cfg.modules.insert("bogus".into(), "debug".into());
        assert!(cfg.validate().is_ok());
    }
}
