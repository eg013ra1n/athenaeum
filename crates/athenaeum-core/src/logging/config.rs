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
}
