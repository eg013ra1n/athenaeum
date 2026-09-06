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

    // Duplicate detection
    pub const DUPLICATES_USE_CONTENT_HASH: &str = "false";
    // Used by frontend via get_setting/set_setting as a UI flag (not directly in Rust)
    #[allow(dead_code)]
    pub const DUPLICATES_CONTENT_HASH_RESCANNED: &str = "false";

    // Blink viewer
    pub const BLINK_THREADS: &str = "0"; // 0 = auto (half of available cores)
    pub const BLINK_MEMORY_CACHE_SIZE: &str = "200";
    /// Megabytes of rendered JPEG the preview cache may hold. Entry count alone
    /// is not a memory bound — a preview JPEG is ~300 KB and a full-resolution
    /// one-shot-colour JPEG ~17 MB.
    pub const BLINK_MEMORY_CACHE_MAX_MB: &str = "512";
    pub const BLINK_MEMORY_RETENTION_MINUTES: &str = "30";

    // Background scan monitoring
    pub const MONITORING_INTERVAL_MINUTES: &str = "1";
    pub const MONITORING_ENABLED_GLOBAL: &str = "true";
    // Used by frontend via get_setting/set_setting as a UI flag (not directly in Rust)
    #[allow(dead_code)]
    pub const AUTO_MERGE_ON_BUTTON_CLICK: &str = "false";
    pub const AUTO_MERGE_ON_MONITOR_DETECT: &str = "false";

    // Archive feature
    pub const ARCHIVE_COMPRESSION: &str = "store"; // "store" | "deflate"

    // Compute queue (global FIFO admission for heavy CPU jobs)
    pub const COMPUTE_MAX_CONCURRENT: &str = "1";

    // Personal sync (Stage I). Dev-only ticket pairing gate — the primary-side
    // receiver + iroh transport only start when this is explicitly enabled.
    pub const SYNC_DEV_TICKET_PAIRING: &str = "false";

    // Full-app capture-node auto mode (task M2). When enabled on a signed-in
    // `capture` device, files newly ingested by a scan are enqueued to the
    // paired primary automatically. Default off — sending is manual until the
    // operator opts in.
    pub const SYNC_AUTO_MODE: &str = "false";

    // Device-wide sync UPLOAD throttle (W1). Bytes/sec across every peer and
    // every concurrent GET. `0` = unlimited, and that is the default — a fresh
    // install never throttles; the pacer only engages once the operator sets a
    // cap. A nonzero value is floored at 100 KB/s on read (see
    // `SettingsManager::get_sync_max_upload_bytes_per_sec`).
    pub const SYNC_MAX_UPLOAD_BYTES_PER_SEC: &str = "0";

    // Cap on simultaneous INCOMING transfers (W2). Mirrors
    // `sync::DEFAULT_MAX_CONCURRENT_RECEIVES` — the fetch+ingest phase is bound
    // by disk and the shared connection, not by the network, so two lanes keep a
    // small transfer from queueing behind a large one without buying seek thrash.
    // Clamped to 1..=8 on read (see
    // `SettingsManager::get_sync_max_concurrent_receives`).
    pub const SYNC_MAX_CONCURRENT_RECEIVES: &str = "2";

    // Account layer (task B4). Base URL of the athenaeum-hub. The device token
    // lives in the OS keychain (never here), keyed per hub host — so the prod
    // and test sign-ins coexist and switching is safe.
    //
    // Debug (dev) builds default to the TEST hub so day-to-day development
    // never touches the production account registry; release builds (prod +
    // betas) default to the production hub. The `account.hub_url` setting
    // overrides either way (Settings → Account hub selector).
    #[cfg(debug_assertions)]
    pub const ACCOUNT_HUB_URL: &str = "https://test-hub.artfrom.space";
    #[cfg(not(debug_assertions))]
    pub const ACCOUNT_HUB_URL: &str = "https://projects.artfrom.space";
}

/// Setting keys used throughout the application
pub mod keys {
    // Frame set clustering
    pub const GROUPING_THRESHOLD_VALUE: &str = "grouping.threshold.value";
    pub const GROUPING_THRESHOLD_UNIT: &str = "grouping.threshold.unit";
    pub const SESSION_GAP_THRESHOLD_HOURS: &str = "session_gap_threshold_hours";

    // Duplicate detection
    pub const DUPLICATES_USE_CONTENT_HASH: &str = "duplicates.use_content_hash";

    // Blink viewer
    pub const BLINK_THREADS: &str = "blink.threads";
    pub const BLINK_MEMORY_CACHE_SIZE: &str = "blink.memory_cache_size";
    pub const BLINK_MEMORY_CACHE_MAX_MB: &str = "blink.memory_cache_max_mb";
    pub const BLINK_MEMORY_RETENTION_MINUTES: &str = "blink.memory_retention_minutes";

    // Background scan monitoring (see docs spec 2026-04-23 auto-scanning)
    pub const MONITORING_INTERVAL_MINUTES: &str = "monitoring.interval_minutes";
    pub const MONITORING_ENABLED_GLOBAL: &str = "monitoring.enabled_global";
    // Used by frontend via get_setting/set_setting as a UI flag (not directly in Rust)
    #[allow(dead_code)]
    pub const AUTO_MERGE_ON_BUTTON_CLICK: &str = "auto_merge.on_button_click";
    pub const AUTO_MERGE_ON_MONITOR_DETECT: &str = "auto_merge.on_monitor_detect";

    // Archive feature
    pub const ARCHIVE_ROOT_PATH: &str = "archive.root_path";
    pub const ARCHIVE_COMPRESSION: &str = "archive.compression";

    // Calibration library — master-frame write destination. Set when the
    // operator picks a folder INSIDE an existing monitored directory (no
    // second scan root is created there; the parent root already provides
    // scan coverage). Present-but-empty means "explicitly cleared" (blocks
    // the legacy calibration_library-root fallback); absent means "never
    // set" (legacy root fallback applies). See
    // `api::scan_roots::resolve_calibration_library_dir`.
    pub const CALIBRATION_LIBRARY_DIR: &str = "calibration.library_dir";

    // Compute queue (global FIFO admission for heavy CPU jobs)
    pub const COMPUTE_MAX_CONCURRENT: &str = "compute.max_concurrent";

    /// Dev-only gate for personal-sync ticket pairing (task A7). When `"true"`,
    /// `get_sync_pairing_ticket` lazily starts the receiver + iroh transport.
    pub const SYNC_DEV_TICKET_PAIRING: &str = "sync.dev_ticket_pairing";

    /// Full-app capture-node auto mode (task M2). `"true"` → a signed-in
    /// `capture` device enqueues freshly-scanned files to its paired primary
    /// automatically at scan-finished. Default `"false"` (manual send only).
    pub const SYNC_AUTO_MODE: &str = "sync.auto_mode";

    /// Device-wide sync UPLOAD throttle in bytes/sec (W1). `0` = unlimited.
    /// One budget for the whole device — applied to the shared iroh node's
    /// upload pacer at bind (`api::sync::ensure_iroh_node`) and live on every
    /// `api::sync::set_sync_upload_limit`.
    pub const SYNC_MAX_UPLOAD_BYTES_PER_SEC: &str = "sync.max_upload_bytes_per_sec";

    /// Cap on simultaneous INCOMING transfers (W2), 1..=8. Applied to the
    /// receiver's [`ReceiveGate`](crate::sync::ReceiveGate) at receiver start
    /// (`api::sync::receiver_hooks` → `SyncRuntime::ensure_started`) and live on
    /// every `api::sync::set_sync_max_concurrent_receives` — a change never
    /// interrupts a transfer already in flight.
    pub const SYNC_MAX_CONCURRENT_RECEIVES: &str = "sync.max_concurrent_receives";

    /// Absolute path of the folder that holds prepared outgoing packages
    /// (`<dir>/<uuid>/…`). Empty/unset = `<identity_dir>/packages`
    /// (transfer-prepare spec §6.1). Applies to the next preparation.
    pub const SYNC_OUTGOING_STAGING_DIR: &str = "sync.outgoing_staging_dir";
    /// Absolute path of the folder that holds the iroh blob store (`blobs/`),
    /// receive staging (`staging/`), the incoming fallback and collab dirs.
    /// Empty/unset = `<identity_dir>`. Applies at the next transport start
    /// (spec §6.4).
    pub const SYNC_INCOMING_WORKING_DIR: &str = "sync.incoming_working_dir";
    /// The previous custom working dir, recorded by `set_transfer_paths` when
    /// the working dir changes, so the storage report can count its leftovers
    /// (spec §6.5). Cleared when nothing is left there.
    pub const SYNC_INCOMING_WORKING_DIR_PREVIOUS: &str = "sync.incoming_working_dir_previous";

    // Account layer (task B4). Non-secret account state persisted so
    // `account_status` works offline. The device TOKEN is never here — it lives
    // in the OS keychain (`account::token_store`). Cleared on sign-out.
    /// Base URL of the athenaeum-hub the app authenticates against.
    pub const ACCOUNT_HUB_URL: &str = "account.hub_url";
    /// Email of the signed-in account (display only).
    pub const ACCOUNT_EMAIL: &str = "account.email";
    /// This device's hub-assigned device id.
    pub const ACCOUNT_DEVICE_ID: &str = "account.device_id";

    // Personal sync pairing caches (task M1). Best-effort offline fallbacks that
    // let the capture-role sender / Perseus start when the hub is briefly
    // unreachable. Neither is a secret. Both are refreshed on the next successful
    // hub resolution (staleness: a role/peer change on the hub takes effect on
    // the next successful refresh, not instantly on the cached start).
    /// Last successfully resolved peer node id (64-char lowercase hex).
    pub const SYNC_CACHED_PEER: &str = "sync.cached_peer_node_id";
    /// Last successfully fetched relay map (newline-separated relay URLs).
    pub const SYNC_CACHED_RELAYS: &str = "sync.cached_relay_map";
    /// Authorized inbound-sync peers for a primary receiver (finding H1):
    /// newline-separated 64-char lowercase hex node ids of the capture devices
    /// paired to THIS primary, refreshed from the hub device list. The receiver
    /// only ingests announces from a peer on this list. Empty = accept nobody
    /// (fail closed) until the first successful hub refresh.
    pub const SYNC_AUTHORIZED_PEERS: &str = "sync.authorized_peer_ids";
    /// Cached node-id-hex → current device name map (JSON object), refreshed
    /// alongside [`SYNC_AUTHORIZED_PEERS`] from the hub device list. Lets the
    /// receiver name an incoming sender's landing folder by that sender's CURRENT
    /// friendly device name WITHOUT a per-package hub round-trip (it reads this
    /// cache only). Absent / a hex not in the map / a name that sanitizes to empty
    /// ⇒ the receiver falls back to the hex-derived slug. Purely cosmetic (folder
    /// naming); never gates a transfer.
    pub const SYNC_DEVICE_NAMES: &str = "sync.device_names";
    /// Cached node-id-hex → device capability map (JSON object, values
    /// `"athenaeum"` / `"perseus"`), refreshed alongside [`SYNC_AUTHORIZED_PEERS`]
    /// from the hub device list. The receiver reads this at announce time to stamp
    /// the announcing peer's capability onto its `sync_inbound` row (so the
    /// Transfers UI can show whether a received transfer came from a full
    /// Athenaeum peer or a send-only Perseus agent). Persisting the stamp onto the
    /// row means it survives a later device revocation that empties this cache.
    /// Absent / a hex not in the map ⇒ no stamp (the row's `peer_capability` stays
    /// `NULL`); purely informational, never gates a transfer.
    pub const SYNC_PEER_CAPABILITIES: &str = "sync.peer_capabilities";
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

    /// Get whether to use content hash (xxhash) for duplicate detection
    pub fn get_duplicates_use_content_hash(&self, conn: &Connection) -> Result<bool> {
        let value = self.get_with_precedence(
            conn,
            keys::DUPLICATES_USE_CONTENT_HASH,
            defaults::DUPLICATES_USE_CONTENT_HASH,
        )?;
        Ok(value.to_lowercase() == "true")
    }

    /// Get the configured archive root path (or None if unset).
    pub fn get_archive_root_path(&self, conn: &Connection) -> Result<Option<String>> {
        let value = self.get_with_precedence(conn, keys::ARCHIVE_ROOT_PATH, "")?;
        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// Get the archive compression mode ("store" or "deflate").
    pub fn get_archive_compression(&self, conn: &Connection) -> Result<String> {
        self.get_with_precedence(
            conn,
            keys::ARCHIVE_COMPRESSION,
            defaults::ARCHIVE_COMPRESSION,
        )
    }

    /// Get the configured concurrency ceiling for the global compute queue
    /// (heavy CPU jobs: analysis, master build, light calibration). Clamped
    /// to the same 1..=8 range `api::compute::set_compute_max_concurrent`
    /// enforces on write — defense in depth against a value that reached the
    /// `compute.max_concurrent` row some other way (direct DB edit, a future
    /// settings import, a botched migration), so a stray `0` can never
    /// permanently stall the admission queue and a stray huge value can
    /// never defeat the point of having one.
    pub fn get_compute_max_concurrent(&self, conn: &Connection) -> Result<usize> {
        let value = self.get_with_precedence(
            conn,
            keys::COMPUTE_MAX_CONCURRENT,
            defaults::COMPUTE_MAX_CONCURRENT,
        )?;
        let n: usize = value.parse()?;
        Ok(n.clamp(1, 8))
    }

    /// Get the device-wide sync UPLOAD limit in bytes/sec. `0` means
    /// **unlimited** (the default — a fresh install never throttles), and is
    /// passed through untouched: it is the unlimited sentinel, not a very
    /// small cap.
    ///
    /// Any nonzero value is floored at 100 KB/s — the same floor
    /// `api::sync::validate_upload_limit` enforces on write, applied again
    /// here as defense in depth against a value that reached the
    /// `sync.max_upload_bytes_per_sec` row some other way (direct DB edit, a
    /// future settings import, a botched migration). Below that floor a
    /// transfer stops looking slow and starts looking dead: every progress bar
    /// would read as stalled and a single frame would take hours, so a stray
    /// tiny value can never strand the device's sync.
    pub fn get_sync_max_upload_bytes_per_sec(&self, conn: &Connection) -> Result<u64> {
        let value = self.get_with_precedence(
            conn,
            keys::SYNC_MAX_UPLOAD_BYTES_PER_SEC,
            defaults::SYNC_MAX_UPLOAD_BYTES_PER_SEC,
        )?;
        let n: u64 = value.parse()?;
        Ok(if n == 0 { 0 } else { n.max(100_000) })
    }

    /// Get the cap on simultaneous INCOMING transfers (W2), clamped to `1..=8`.
    ///
    /// The clamp is defense in depth against a value that reached the
    /// `sync.max_concurrent_receives` row some other way than
    /// `api::sync::set_sync_max_concurrent_receives` (direct DB edit, a future
    /// settings import, a botched migration): an out-of-range value must degrade
    /// to a sane cap rather than panic or run unbounded. `0` in particular is not
    /// "no limit" — it is "admit nobody", which would park every inbound transfer
    /// forever, so it clamps UP to 1; anything above 8 clamps down, since beyond
    /// that the lanes only fight over the same disk. Same 1..=8 window the gate
    /// itself enforces ([`ReceiveGate::set_limit`](crate::sync::ReceiveGate::set_limit)),
    /// so the two can never disagree.
    pub fn get_sync_max_concurrent_receives(&self, conn: &Connection) -> Result<usize> {
        let value = self.get_with_precedence(
            conn,
            keys::SYNC_MAX_CONCURRENT_RECEIVES,
            defaults::SYNC_MAX_CONCURRENT_RECEIVES,
        )?;
        let n: usize = value.parse()?;
        Ok(n.clamp(1, 8))
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
            .get_with_precedence(
                &conn,
                keys::GROUPING_THRESHOLD_VALUE,
                defaults::GROUPING_THRESHOLD_VALUE,
            )
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
            .get_with_precedence(
                &conn,
                keys::GROUPING_THRESHOLD_VALUE,
                defaults::GROUPING_THRESHOLD_VALUE,
            )
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
            .get_with_precedence(
                &conn,
                keys::GROUPING_THRESHOLD_VALUE,
                defaults::GROUPING_THRESHOLD_VALUE,
            )
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

    #[test]
    fn test_archive_root_path_unset_returns_none() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        let value = manager.get_archive_root_path(&conn).unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn test_archive_root_path_set_returns_some() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        manager
            .persist_setting(&conn, keys::ARCHIVE_ROOT_PATH, "/tmp/archive")
            .unwrap();
        assert_eq!(
            manager.get_archive_root_path(&conn).unwrap(),
            Some("/tmp/archive".to_string())
        );
    }

    #[test]
    fn test_archive_compression_default_is_store() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        assert_eq!(manager.get_archive_compression(&conn).unwrap(), "store");
    }

    #[test]
    fn test_compute_max_concurrent_default_is_one() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        assert_eq!(defaults::COMPUTE_MAX_CONCURRENT, "1");
        assert_eq!(manager.get_compute_max_concurrent(&conn).unwrap(), 1);
    }

    #[test]
    fn test_compute_max_concurrent_reads_persisted_value() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        manager
            .persist_setting(&conn, keys::COMPUTE_MAX_CONCURRENT, "4")
            .unwrap();
        assert_eq!(manager.get_compute_max_concurrent(&conn).unwrap(), 4);
    }

    #[test]
    fn test_compute_max_concurrent_clamps_out_of_range_values() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // A `0` or huge value can only reach the DB via something other
        // than `api::compute::set_compute_max_concurrent` (which itself
        // rejects 0 and >8) — a direct DB edit, a future settings
        // import/export, or a botched migration. The getter clamps
        // defensively so such a value can never permanently stall the
        // queue (0) or defeat the point of having one (huge).
        manager
            .persist_setting(&conn, keys::COMPUTE_MAX_CONCURRENT, "0")
            .unwrap();
        assert_eq!(manager.get_compute_max_concurrent(&conn).unwrap(), 1);

        manager
            .persist_setting(&conn, keys::COMPUTE_MAX_CONCURRENT, "999")
            .unwrap();
        assert_eq!(manager.get_compute_max_concurrent(&conn).unwrap(), 8);
    }

    #[test]
    fn default_upload_limit_is_unlimited() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // A fresh install throttles nothing — the pacer only engages once the
        // operator sets a cap.
        assert_eq!(defaults::SYNC_MAX_UPLOAD_BYTES_PER_SEC, "0");
        assert_eq!(manager.get_sync_max_upload_bytes_per_sec(&conn).unwrap(), 0);
    }

    #[test]
    fn upload_limit_getter_passes_zero_through() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // 0 is not "a very small cap" — it is the unlimited sentinel, so the
        // 100 KB/s floor must NOT lift it.
        manager
            .persist_setting(&conn, keys::SYNC_MAX_UPLOAD_BYTES_PER_SEC, "0")
            .unwrap();
        assert_eq!(manager.get_sync_max_upload_bytes_per_sec(&conn).unwrap(), 0);
    }

    #[test]
    fn default_max_concurrent_receives_is_two() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // The shipped cap has exactly one definition
        // (`sync::DEFAULT_MAX_CONCURRENT_RECEIVES`); this default string is its
        // settings-layer twin and must agree with it.
        assert_eq!(defaults::SYNC_MAX_CONCURRENT_RECEIVES, "2");
        assert_eq!(
            manager.get_sync_max_concurrent_receives(&conn).unwrap(),
            crate::sync::DEFAULT_MAX_CONCURRENT_RECEIVES
        );
    }

    #[test]
    fn max_concurrent_receives_getter_clamps_out_of_range() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // Out-of-range values can only reach the row via something other than
        // `api::sync::set_sync_max_concurrent_receives` (direct DB edit, a
        // future settings import, a botched migration). `0` would mean "admit
        // nobody" — every inbound transfer parks forever; a huge value would
        // defeat the point of having a gate. Clamp both, never panic.
        manager
            .persist_setting(&conn, keys::SYNC_MAX_CONCURRENT_RECEIVES, "0")
            .unwrap();
        assert_eq!(manager.get_sync_max_concurrent_receives(&conn).unwrap(), 1);

        manager
            .persist_setting(&conn, keys::SYNC_MAX_CONCURRENT_RECEIVES, "99")
            .unwrap();
        assert_eq!(manager.get_sync_max_concurrent_receives(&conn).unwrap(), 8);
    }

    #[test]
    fn upload_limit_getter_clamps_tiny_nonzero_values() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let manager = SettingsManager::new();

        // A sub-floor cap can only reach the row via something other than
        // `api::sync::set_sync_upload_limit` (direct DB edit, settings import,
        // botched migration). Clamp defensively: at 5 KB/s a transfer looks
        // dead rather than slow.
        manager
            .persist_setting(&conn, keys::SYNC_MAX_UPLOAD_BYTES_PER_SEC, "5000")
            .unwrap();
        assert_eq!(
            manager.get_sync_max_upload_bytes_per_sec(&conn).unwrap(),
            100_000
        );
    }
}
