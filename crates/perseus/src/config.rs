//! Perseus TOML configuration: parse + validate.
//!
//! The binding contract is the `perseus.toml` shape from the task brief:
//!
//! ```toml
//! capture_dir = "/data/capture"
//! data_dir = "/var/lib/perseus"
//! pairing_ticket = "<paste from primary Settings → Sync (dev)>"
//! mode = "auto"                             # only value in MVP
//! [retention]
//! policy = "keep_everything"                # keep_everything | on_confirm | keep_days | disk_pct
//! dry_run = true                            # MUST stay true until M-Perseus-MVP sign-off
//! ```
//!
//! Two optional tuning fields are additive (defaulted, absent from the contract
//! sample): `stability_secs` (write-stability quiet window, default 10) and
//! `poll_interval_secs` (re-stat cadence, default 2). Everything else is
//! required; validation errors are actionable — they name the offending field
//! and the accepted values.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Default write-stability quiet window (seconds). Capture software writes FITS
/// progressively; a file is only enqueued after its size+mtime hold steady for
/// this long.
pub const DEFAULT_STABILITY_SECS: u64 = 10;

/// Default re-stat cadence (seconds) for pending capture files.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 2;

fn default_stability_secs() -> u64 {
    DEFAULT_STABILITY_SECS
}
fn default_poll_interval_secs() -> u64 {
    DEFAULT_POLL_INTERVAL_SECS
}
fn default_true() -> bool {
    true
}

/// Agent operating mode. MVP has a single value; the enum exists so an unknown
/// mode is a clear parse error rather than a silently ignored string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Auto-send every new frame the watcher stabilizes.
    Auto,
}

/// Retention policy. Parsed and validated here; the evaluator that acts on it is
/// task A8. Until then retention is inert (no deletion anywhere in this crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPolicy {
    /// Never delete anything (the safe default).
    KeepEverything,
    /// Delete a local frame only once the peer has confirmed receipt (A8).
    OnConfirm,
    /// Delete confirmed frames older than a threshold in days (A8).
    KeepDays,
    /// Delete confirmed frames to keep the volume under a disk-usage percent (A8).
    DiskPct,
}

/// The `[retention]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetentionConfig {
    pub policy: RetentionPolicy,
    /// Dry-run flag. Hard invariant (plan Global Constraints): dry-run is the
    /// default and stays on until the M-Perseus-MVP gate passes. A6 therefore
    /// rejects `dry_run = false` — there is no deletion evaluator yet (A8), so
    /// turning it off could only mislead.
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            policy: RetentionPolicy::KeepEverything,
            dry_run: true,
        }
    }
}

/// Parsed + validated Perseus configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub capture_dir: PathBuf,
    pub data_dir: PathBuf,
    pub pairing_ticket: String,
    pub mode: Mode,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default = "default_stability_secs")]
    pub stability_secs: u64,
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
}

impl Config {
    /// Parse + validate a config from a TOML file on disk.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read perseus config {}", path.display()))?;
        Self::from_toml_str(&text)
            .with_context(|| format!("invalid perseus config {}", path.display()))
    }

    /// Parse + validate a config from a TOML string. Split out so validation is
    /// unit-testable without touching the filesystem.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).map_err(|e| {
            anyhow::anyhow!(
                "could not parse config TOML: {e}. Expected keys: capture_dir, \
                 data_dir, pairing_ticket, mode = \"auto\", and a [retention] table \
                 with policy = keep_everything|on_confirm|keep_days|disk_pct and \
                 dry_run = true"
            )
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Structural validation of already-parsed fields. Iroh-ticket well-formedness
    /// is deliberately NOT checked here — that lives in the production transport
    /// wiring (`run`), so tests and the loopback path can supply a placeholder.
    pub fn validate(&self) -> Result<()> {
        if self.capture_dir.as_os_str().is_empty() {
            bail!("capture_dir must not be empty");
        }
        if !self.capture_dir.exists() {
            bail!(
                "capture_dir {} does not exist — create it (or point at the \
                 right path) before starting Perseus",
                self.capture_dir.display()
            );
        }
        if self.data_dir.as_os_str().is_empty() {
            bail!("data_dir must not be empty");
        }
        if self.pairing_ticket.trim().is_empty() {
            bail!(
                "pairing_ticket must not be empty — paste the ticket from the \
                 primary's Settings → Sync (dev)"
            );
        }
        // `mode` is an enum, so any non-`auto` value already failed to parse.
        // Hard invariant: no deletion path exists before A8; refuse to run with
        // dry-run disabled so the config can never imply live deletion.
        if !self.retention.dry_run {
            bail!(
                "retention.dry_run = false is not allowed yet: the retention \
                 evaluator ships in a later task and deletion stays disabled \
                 until the M-Perseus-MVP gate passes. Set dry_run = true"
            );
        }
        if self.stability_secs == 0 {
            bail!("stability_secs must be >= 1");
        }
        if self.poll_interval_secs == 0 {
            bail!("poll_interval_secs must be >= 1");
        }
        Ok(())
    }

    /// Write-stability quiet window as a [`Duration`].
    pub fn stability(&self) -> Duration {
        Duration::from_secs(self.stability_secs)
    }

    /// Re-stat cadence as a [`Duration`].
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs)
    }

    /// Path to the standalone sync SQLite store inside the data dir.
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("perseus.db")
    }

    /// Path to the persisted 32-byte device secret key inside the data dir.
    pub fn device_key_path(&self) -> PathBuf {
        self.data_dir.join("device_key")
    }

    /// Directory rolling JSONL logs are written into.
    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Directory built packages are staged under before/while sending.
    pub fn packages_dir(&self) -> PathBuf {
        self.data_dir.join("packages")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract-shape TOML with `capture_dir` interpolated. `capture_dir`
    /// must exist on disk (validate() now enforces that — see
    /// `nonexistent_capture_dir_is_rejected`), so every test that needs a
    /// *valid* config builds this against a live [`tempfile::TempDir`] kept
    /// alive for the test's duration.
    fn good_toml(capture_dir: &Path) -> String {
        format!(
            r#"
capture_dir = "{}"
data_dir = "/var/lib/perseus"
pairing_ticket = "ticket-abc"
mode = "auto"
[retention]
policy = "keep_everything"
dry_run = true
"#,
            capture_dir.display()
        )
    }

    #[test]
    fn parses_the_contract_shape() {
        let capture = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&good_toml(capture.path())).expect("valid config");
        assert_eq!(cfg.capture_dir, capture.path());
        assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/perseus"));
        assert_eq!(cfg.pairing_ticket, "ticket-abc");
        assert_eq!(cfg.mode, Mode::Auto);
        assert_eq!(cfg.retention.policy, RetentionPolicy::KeepEverything);
        assert!(cfg.retention.dry_run);
        // Defaults applied for the additive tuning fields.
        assert_eq!(cfg.stability_secs, DEFAULT_STABILITY_SECS);
        assert_eq!(cfg.poll_interval_secs, DEFAULT_POLL_INTERVAL_SECS);
    }

    #[test]
    fn dry_run_defaults_to_true_when_omitted() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            r#"
capture_dir = "{}"
data_dir = "/d"
pairing_ticket = "t"
mode = "auto"
[retention]
policy = "on_confirm"
"#,
            capture.path().display()
        );
        let cfg = Config::from_toml_str(&text).expect("valid config");
        assert!(cfg.retention.dry_run, "dry_run must default to true");
    }

    #[test]
    fn retention_table_defaults_when_omitted() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            r#"
capture_dir = "{}"
data_dir = "/d"
pairing_ticket = "t"
mode = "auto"
"#,
            capture.path().display()
        );
        let cfg = Config::from_toml_str(&text).expect("valid config");
        assert_eq!(cfg.retention.policy, RetentionPolicy::KeepEverything);
        assert!(cfg.retention.dry_run);
    }

    #[test]
    fn all_retention_policies_parse() {
        let capture = tempfile::tempdir().unwrap();
        for (s, want) in [
            ("keep_everything", RetentionPolicy::KeepEverything),
            ("on_confirm", RetentionPolicy::OnConfirm),
            ("keep_days", RetentionPolicy::KeepDays),
            ("disk_pct", RetentionPolicy::DiskPct),
        ] {
            let text = format!(
                "capture_dir=\"{}\"\ndata_dir=\"/d\"\npairing_ticket=\"t\"\nmode=\"auto\"\n[retention]\npolicy=\"{s}\"\ndry_run=true\n",
                capture.path().display()
            );
            let cfg = Config::from_toml_str(&text).expect("valid");
            assert_eq!(cfg.retention.policy, want, "policy {s}");
        }
    }

    #[test]
    fn dry_run_false_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path()).replace("dry_run = true", "dry_run = false");
        let err = Config::from_toml_str(&text).expect_err("dry_run=false must fail");
        assert!(
            err.to_string().contains("dry_run")
                || err.chain().any(|c| c.to_string().contains("dry_run")),
            "error should mention dry_run: {err:#}"
        );
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path()).replace("mode = \"auto\"", "mode = \"manual\"");
        assert!(
            Config::from_toml_str(&text).is_err(),
            "only mode = auto is accepted in the MVP"
        );
    }

    #[test]
    fn unknown_retention_policy_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path()).replace("keep_everything", "delete_all_now");
        assert!(Config::from_toml_str(&text).is_err());
    }

    #[test]
    fn empty_pairing_ticket_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path()).replace("ticket-abc", "   ");
        let err = Config::from_toml_str(&text).expect_err("blank ticket must fail");
        assert!(
            err.chain().any(|c| c.to_string().contains("pairing_ticket")),
            "error should mention pairing_ticket: {err:#}"
        );
    }

    #[test]
    fn zero_stability_is_rejected() {
        // stability_secs is a top-level key, so it must precede the [retention]
        // table (a trailing append would nest under [retention] and be ignored).
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            r#"
capture_dir = "{}"
data_dir = "/d"
pairing_ticket = "t"
mode = "auto"
stability_secs = 0
[retention]
policy = "keep_everything"
dry_run = true
"#,
            capture.path().display()
        );
        assert!(Config::from_toml_str(&text).is_err());
    }

    #[test]
    fn missing_required_field_is_rejected() {
        let text = r#"
data_dir = "/d"
pairing_ticket = "t"
mode = "auto"
"#;
        assert!(
            Config::from_toml_str(text).is_err(),
            "missing capture_dir must fail"
        );
    }

    /// Review minor (a): a `capture_dir` that doesn't exist on disk must be
    /// rejected with an actionable message, not silently accepted (the watcher
    /// would otherwise fail confusingly later, or watch nothing).
    #[test]
    fn nonexistent_capture_dir_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let text = good_toml(&missing);
        let err = Config::from_toml_str(&text).expect_err("missing capture_dir must fail");
        assert!(
            err.chain().any(|c| c.to_string().contains("capture_dir")),
            "error should mention capture_dir: {err:#}"
        );
    }

    #[test]
    fn derived_paths_hang_off_data_dir() {
        let capture = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&good_toml(capture.path())).unwrap();
        assert_eq!(cfg.db_path(), PathBuf::from("/var/lib/perseus/perseus.db"));
        assert_eq!(
            cfg.device_key_path(),
            PathBuf::from("/var/lib/perseus/device_key")
        );
        assert_eq!(cfg.log_dir(), PathBuf::from("/var/lib/perseus/logs"));
    }
}
