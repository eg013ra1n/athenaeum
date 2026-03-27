// Settings management with precedence: runtime > DB > defaults

use crate::db::{get_setting, set_setting as db_set_setting};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Mutex;

/// Application defaults for grouping settings
pub mod defaults {
    // Frame set clustering (for grouping LIGHT frames by sky coordinates)
    pub const GROUPING_THRESHOLD_VALUE: &str = "3.0";
    pub const GROUPING_THRESHOLD_UNIT: &str = "deg";
    pub const SESSION_GAP_THRESHOLD_HOURS: &str = "6.0";

    // Dark Library creation (batch organization of ALL dark/bias frames for a camera)
    // Note: These are separate from calibration matching settings (calibration.matching_config)
    // which are used for finding calibrations for specific light frames.
    // Dark Library uses broader thresholds because it organizes existing frames,
    // while calibration matching uses narrower thresholds for precision matching.
    pub const DARK_LIBRARY_DATE_THRESHOLD_DAYS: &str = "180";
    pub const DARK_LIBRARY_TEMP_THRESHOLD_CELSIUS: &str = "1.0";

    // Duplicate detection
    pub const DUPLICATES_USE_CONTENT_HASH: &str = "false";
    // Used by frontend via get_setting/set_setting as a UI flag (not directly in Rust)
    #[allow(dead_code)]
    pub const DUPLICATES_CONTENT_HASH_RESCANNED: &str = "false";

    // Blink viewer
    pub const BLINK_THREADS: &str = "0"; // 0 = auto (half of available cores)
    pub const BLINK_MEMORY_CACHE_SIZE: &str = "200";
    pub const BLINK_MEMORY_RETENTION_MINUTES: &str = "30";
}

/// Setting keys used throughout the application
pub mod keys {
    // Frame set clustering
    pub const GROUPING_THRESHOLD_VALUE: &str = "grouping.threshold.value";
    pub const GROUPING_THRESHOLD_UNIT: &str = "grouping.threshold.unit";
    pub const SESSION_GAP_THRESHOLD_HOURS: &str = "session_gap_threshold_hours";

    // Dark Library creation (see defaults module for explanation of why these
    // are separate from calibration.matching_config settings)
    pub const DARK_LIBRARY_DATE_THRESHOLD_DAYS: &str = "dark_library.date_threshold_days";
    pub const DARK_LIBRARY_TEMP_THRESHOLD_CELSIUS: &str = "dark_library.temp_threshold_celsius";

    // Duplicate detection
    pub const DUPLICATES_USE_CONTENT_HASH: &str = "duplicates.use_content_hash";
    // Used by frontend via get_setting/set_setting as a UI flag (not directly in Rust)
    #[allow(dead_code)]
    pub const DUPLICATES_CONTENT_HASH_RESCANNED: &str = "duplicates.content_hash_rescanned";

    // Blink viewer
    pub const BLINK_THREADS: &str = "blink.threads";
    pub const BLINK_MEMORY_CACHE_SIZE: &str = "blink.memory_cache_size";
    pub const BLINK_MEMORY_RETENTION_MINUTES: &str = "blink.memory_retention_minutes";
}

/// Runtime overrides for settings (session-specific)
pub struct SettingsManager {
    runtime_overrides: Mutex<HashMap<String, String>>,
}

impl SettingsManager {
    pub fn new() -> Self {
        Self {
            runtime_overrides: Mutex::new(HashMap::new()),
        }
    }

    /// Get a setting with precedence: runtime > DB > default
    pub fn get_with_precedence(
        &self,
        conn: &Connection,
        key: &str,
        default: &str,
    ) -> Result<String> {
        // Check runtime override first
        if let Ok(overrides) = self.runtime_overrides.lock() {
            if let Some(value) = overrides.get(key) {
                return Ok(value.clone());
            }
        }

        // Check database
        if let Some(value) = get_setting(conn, key)? {
            return Ok(value);
        }

        // Return default
        Ok(default.to_string())
    }

    /// Set a runtime override (session-specific, not persisted)
    #[allow(dead_code)]
    pub fn set_runtime_override(&self, key: String, value: String) {
        if let Ok(mut overrides) = self.runtime_overrides.lock() {
            overrides.insert(key, value);
        }
    }

    /// Clear a runtime override
    #[allow(dead_code)]
    pub fn clear_runtime_override(&self, key: &str) {
        if let Ok(mut overrides) = self.runtime_overrides.lock() {
            overrides.remove(key);
        }
    }

    /// Clear all runtime overrides
    #[allow(dead_code)]
    pub fn clear_all_runtime_overrides(&self) {
        if let Ok(mut overrides) = self.runtime_overrides.lock() {
            overrides.clear();
        }
    }

    /// Persist a setting to the database (bypasses runtime override)
    pub fn persist_setting(&self, conn: &Connection, key: &str, value: &str) -> Result<()> {
        db_set_setting(conn, key, value)?;
        Ok(())
    }
}

impl Default for SettingsManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper functions for common grouping settings
impl SettingsManager {
    /// Get the grouping threshold value (e.g., "5.0")
    pub fn get_grouping_threshold_value(&self, conn: &Connection) -> Result<String> {
        self.get_with_precedence(
            conn,
            keys::GROUPING_THRESHOLD_VALUE,
            defaults::GROUPING_THRESHOLD_VALUE,
        )
    }

    /// Get the grouping threshold unit (e.g., "arcmin")
    pub fn get_grouping_threshold_unit(&self, conn: &Connection) -> Result<String> {
        self.get_with_precedence(
            conn,
            keys::GROUPING_THRESHOLD_UNIT,
            defaults::GROUPING_THRESHOLD_UNIT,
        )
    }

    /// Get the grouping threshold in arcseconds (converted from configured unit)
    pub fn get_grouping_threshold_arcsec(&self, conn: &Connection) -> Result<f64> {
        let value: f64 = self.get_grouping_threshold_value(conn)?.parse()?;
        let unit = self.get_grouping_threshold_unit(conn)?;

        let arcsec = match unit.as_str() {
            "arcsec" => value,
            "arcmin" => value * 60.0,
            "deg" => value * 3600.0,
            _ => return Err(anyhow::anyhow!("Invalid threshold unit: {}", unit)),
        };

        Ok(arcsec)
    }

    /// Get the grouping threshold in degrees (converted from configured unit)
    pub fn get_grouping_threshold_deg(&self, conn: &Connection) -> Result<f64> {
        let arcsec = self.get_grouping_threshold_arcsec(conn)?;
        Ok(arcsec / 3600.0)
    }

    /// Get the session gap threshold in hours (for imaging night detection)
    pub fn get_session_gap_threshold_hours(&self, conn: &Connection) -> Result<f64> {
        let value = self.get_with_precedence(
            conn,
            keys::SESSION_GAP_THRESHOLD_HOURS,
            defaults::SESSION_GAP_THRESHOLD_HOURS,
        )?;
        Ok(value.parse()?)
    }

    /// Get the dark library date threshold in days (for batch Dark Library creation).
    /// Note: This is different from calibration.matching_config clustering settings
    /// which are used for per-frame calibration matching.
    pub fn get_dark_library_date_threshold(&self, conn: &Connection) -> Result<i64> {
        let value = self.get_with_precedence(
            conn,
            keys::DARK_LIBRARY_DATE_THRESHOLD_DAYS,
            defaults::DARK_LIBRARY_DATE_THRESHOLD_DAYS,
        )?;
        Ok(value.parse()?)
    }

    /// Get the dark library temperature threshold in Celsius (for batch Dark Library creation).
    /// Note: This is different from calibration.matching_config warning thresholds
    /// which are used for per-frame calibration matching warnings.
    pub fn get_dark_library_temp_threshold(&self, conn: &Connection) -> Result<f64> {
        let value = self.get_with_precedence(
            conn,
            keys::DARK_LIBRARY_TEMP_THRESHOLD_CELSIUS,
            defaults::DARK_LIBRARY_TEMP_THRESHOLD_CELSIUS,
        )?;
        Ok(value.parse()?)
    }

    /// Get whether to use content hash (xxhash) for duplicate detection
    pub fn get_duplicates_use_content_hash(&self, conn: &Connection) -> Result<bool> {
        let value = self.get_with_precedence(
            conn,
            keys::DUPLICATES_USE_CONTENT_HASH,
            defaults::DUPLICATES_USE_CONTENT_HASH,
        )?;
        Ok(value.to_lowercase() == "true")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    #[test]
    fn test_precedence_defaults() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // Should return default when nothing is set
        let value = manager
            .get_with_precedence(&conn, keys::GROUPING_THRESHOLD_VALUE, defaults::GROUPING_THRESHOLD_VALUE)
            .unwrap();
        assert_eq!(value, "3.0");
    }

    #[test]
    fn test_precedence_database() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // Set in database
        db_set_setting(&conn, keys::GROUPING_THRESHOLD_VALUE, "10.0").unwrap();

        // Should return DB value
        let value = manager
            .get_with_precedence(&conn, keys::GROUPING_THRESHOLD_VALUE, defaults::GROUPING_THRESHOLD_VALUE)
            .unwrap();
        assert_eq!(value, "10.0");
    }

    #[test]
    fn test_precedence_runtime() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // Set in database
        db_set_setting(&conn, keys::GROUPING_THRESHOLD_VALUE, "10.0").unwrap();

        // Set runtime override
        manager.set_runtime_override(
            keys::GROUPING_THRESHOLD_VALUE.to_string(),
            "15.0".to_string(),
        );

        // Should return runtime value
        let value = manager
            .get_with_precedence(&conn, keys::GROUPING_THRESHOLD_VALUE, defaults::GROUPING_THRESHOLD_VALUE)
            .unwrap();
        assert_eq!(value, "15.0");
    }

    #[test]
    fn test_threshold_unit_conversion() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // Test arcmin to arcsec
        db_set_setting(&conn, keys::GROUPING_THRESHOLD_VALUE, "5.0").unwrap();
        db_set_setting(&conn, keys::GROUPING_THRESHOLD_UNIT, "arcmin").unwrap();
        let arcsec = manager.get_grouping_threshold_arcsec(&conn).unwrap();
        assert_eq!(arcsec, 300.0);

        // Test degrees to arcsec
        db_set_setting(&conn, keys::GROUPING_THRESHOLD_VALUE, "1.0").unwrap();
        db_set_setting(&conn, keys::GROUPING_THRESHOLD_UNIT, "deg").unwrap();
        let arcsec = manager.get_grouping_threshold_arcsec(&conn).unwrap();
        assert_eq!(arcsec, 3600.0);
    }
}
