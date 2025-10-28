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
}

/// Setting keys used throughout the application
pub mod keys {
    pub const GROUPING_THRESHOLD_VALUE: &str = "grouping.threshold.value";
    pub const GROUPING_THRESHOLD_UNIT: &str = "grouping.threshold.unit";
    pub const GROUPING_COORD_FRAME: &str = "grouping.coord.frame";
    pub const UI_OBJECTS_AUTO_NAME_MODE: &str = "ui.objects.auto_name_mode";
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
        assert_eq!(value, "5.0");
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
