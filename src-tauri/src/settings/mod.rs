// Settings management with precedence: runtime > DB > defaults

use crate::db::{get_setting, set_setting as db_set_setting};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Mutex;

/// Application defaults for grouping settings
pub mod defaults {
    pub const GROUPING_THRESHOLD_VALUE: &str = "3.0";
    pub const GROUPING_THRESHOLD_UNIT: &str = "deg";
    pub const GROUPING_COORD_FRAME: &str = "ICRS";
    pub const UI_OBJECTS_AUTO_NAME_MODE: &str = "majority-object";
    pub const DARK_LIBRARY_DATE_THRESHOLD_DAYS: &str = "180";
    pub const DARK_LIBRARY_TEMP_THRESHOLD_CELSIUS: &str = "1.0";
    pub const DUPLICATES_USE_CONTENT_HASH: &str = "false";
    pub const DUPLICATES_CONTENT_HASH_RESCANNED: &str = "false";

    // Flat calibration defaults
    pub const FLATS_MAX_AGE_DAYS: &str = "30";
    pub const FLATS_TIME_CLUSTER_MINUTES: &str = "30";
    pub const TEMPERATURE_MATCH_WEIGHT: &str = "0.3";

    // Dark calibration defaults
    pub const DARKS_MAX_AGE_DAYS: &str = "30";
    pub const DARKS_TIME_CLUSTER_MINUTES: &str = "30";

    // Bias calibration defaults
    pub const BIAS_MAX_AGE_DAYS: &str = "30";
    pub const BIAS_TIME_CLUSTER_MINUTES: &str = "30";
}

/// Setting keys used throughout the application
pub mod keys {
    pub const GROUPING_THRESHOLD_VALUE: &str = "grouping.threshold.value";
    pub const GROUPING_THRESHOLD_UNIT: &str = "grouping.threshold.unit";
    pub const GROUPING_COORD_FRAME: &str = "grouping.coord.frame";
    pub const UI_OBJECTS_AUTO_NAME_MODE: &str = "ui.objects.auto_name_mode";
    pub const DARK_LIBRARY_DATE_THRESHOLD_DAYS: &str = "dark_library.date_threshold_days";
    pub const DARK_LIBRARY_TEMP_THRESHOLD_CELSIUS: &str = "dark_library.temp_threshold_celsius";
    pub const DUPLICATES_USE_CONTENT_HASH: &str = "duplicates.use_content_hash";
    pub const DUPLICATES_CONTENT_HASH_RESCANNED: &str = "duplicates.content_hash_rescanned";

    // Flat calibration keys
    pub const FLATS_MAX_AGE_DAYS: &str = "flats.max_age_days";
    pub const FLATS_TIME_CLUSTER_MINUTES: &str = "flats.time_cluster_minutes";
    pub const TEMPERATURE_MATCH_WEIGHT: &str = "temperature.match_weight";

    // Dark calibration keys
    pub const DARKS_MAX_AGE_DAYS: &str = "darks.max_age_days";
    pub const DARKS_TIME_CLUSTER_MINUTES: &str = "darks.time_cluster_minutes";

    // Bias calibration keys
    pub const BIAS_MAX_AGE_DAYS: &str = "bias.max_age_days";
    pub const BIAS_TIME_CLUSTER_MINUTES: &str = "bias.time_cluster_minutes";
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
    pub fn set_runtime_override(&self, key: String, value: String) {
        if let Ok(mut overrides) = self.runtime_overrides.lock() {
            overrides.insert(key, value);
        }
    }

    /// Clear a runtime override
    pub fn clear_runtime_override(&self, key: &str) {
        if let Ok(mut overrides) = self.runtime_overrides.lock() {
            overrides.remove(key);
        }
    }

    /// Clear all runtime overrides
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

    /// Get the coordinate frame (e.g., "ICRS")
    pub fn get_grouping_coord_frame(&self, conn: &Connection) -> Result<String> {
        self.get_with_precedence(
            conn,
            keys::GROUPING_COORD_FRAME,
            defaults::GROUPING_COORD_FRAME,
        )
    }

    /// Get the auto-name mode (e.g., "majority-object" or "ra-dec")
    pub fn get_auto_name_mode(&self, conn: &Connection) -> Result<String> {
        self.get_with_precedence(
            conn,
            keys::UI_OBJECTS_AUTO_NAME_MODE,
            defaults::UI_OBJECTS_AUTO_NAME_MODE,
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

    /// Get the dark library date threshold in days
    pub fn get_dark_library_date_threshold(&self, conn: &Connection) -> Result<i64> {
        let value = self.get_with_precedence(
            conn,
            keys::DARK_LIBRARY_DATE_THRESHOLD_DAYS,
            defaults::DARK_LIBRARY_DATE_THRESHOLD_DAYS,
        )?;
        Ok(value.parse()?)
    }

    /// Get the dark library temperature threshold in Celsius
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

    /// Get whether content hash rescan has been completed
    pub fn get_duplicates_content_hash_rescanned(&self, conn: &Connection) -> Result<bool> {
        let value = self.get_with_precedence(
            conn,
            keys::DUPLICATES_CONTENT_HASH_RESCANNED,
            defaults::DUPLICATES_CONTENT_HASH_RESCANNED,
        )?;
        Ok(value.to_lowercase() == "true")
    }

    /// Get the maximum age of flats to consider (in days)
    pub fn get_flats_max_age_days(&self, conn: &Connection) -> Result<i64> {
        let value = self.get_with_precedence(
            conn,
            keys::FLATS_MAX_AGE_DAYS,
            defaults::FLATS_MAX_AGE_DAYS,
        )?;
        Ok(value.parse()?)
    }

    /// Get the time clustering threshold for flat frames (in minutes)
    pub fn get_flats_time_cluster_minutes(&self, conn: &Connection) -> Result<i64> {
        let value = self.get_with_precedence(
            conn,
            keys::FLATS_TIME_CLUSTER_MINUTES,
            defaults::FLATS_TIME_CLUSTER_MINUTES,
        )?;
        Ok(value.parse()?)
    }

    /// Get the temperature match weight for flat selection (0.0-1.0)
    pub fn get_temperature_match_weight(&self, conn: &Connection) -> Result<f64> {
        let value = self.get_with_precedence(
            conn,
            keys::TEMPERATURE_MATCH_WEIGHT,
            defaults::TEMPERATURE_MATCH_WEIGHT,
        )?;
        Ok(value.parse()?)
    }

    /// Get the maximum age of darks to consider (in days)
    pub fn get_darks_max_age_days(&self, conn: &Connection) -> Result<i64> {
        let value = self.get_with_precedence(
            conn,
            keys::DARKS_MAX_AGE_DAYS,
            defaults::DARKS_MAX_AGE_DAYS,
        )?;
        Ok(value.parse()?)
    }

    /// Get the time clustering threshold for dark frames (in minutes)
    pub fn get_darks_time_cluster_minutes(&self, conn: &Connection) -> Result<i64> {
        let value = self.get_with_precedence(
            conn,
            keys::DARKS_TIME_CLUSTER_MINUTES,
            defaults::DARKS_TIME_CLUSTER_MINUTES,
        )?;
        Ok(value.parse()?)
    }

    /// Get the maximum age of bias to consider (in days)
    pub fn get_bias_max_age_days(&self, conn: &Connection) -> Result<i64> {
        let value = self.get_with_precedence(
            conn,
            keys::BIAS_MAX_AGE_DAYS,
            defaults::BIAS_MAX_AGE_DAYS,
        )?;
        Ok(value.parse()?)
    }

    /// Get the time clustering threshold for bias frames (in minutes)
    pub fn get_bias_time_cluster_minutes(&self, conn: &Connection) -> Result<i64> {
        let value = self.get_with_precedence(
            conn,
            keys::BIAS_TIME_CLUSTER_MINUTES,
            defaults::BIAS_TIME_CLUSTER_MINUTES,
        )?;
        Ok(value.parse()?)
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
