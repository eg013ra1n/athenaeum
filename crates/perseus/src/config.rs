//! Perseus TOML configuration: parse + validate.
//!
//! The binding contract is the `perseus.toml` shape from the task brief. As of
//! task M1 there are two pairing routes — an **account** (the primary is resolved
//! from the hub device list) or a **dev ticket** — and at least one must be
//! present:
//!
//! ```toml
//! capture_dir = "/data/capture"
//! data_dir = "/var/lib/perseus"
//! mode = "auto"                             # only value in MVP
//!
//! # Route A — account pairing (recommended). Sign in with `perseus login`; the
//! # device token lands in a 0600 file in data_dir (NEVER in this TOML).
//! [account]
//! hub_url = "https://projects.artfrom.space"
//! email = "me@example.com"                  # optional; prompted at login if absent
//! primary_device_id = "dev-abc"             # optional; auto-picks the single primary
//! allow_default_relays = false              # dev only; see AccountConfig docs
//!
//! # Route B — dev ticket (offline dev / tests). Optional now that [account] exists.
//! pairing_ticket = "<paste from primary Settings → Sync (dev)>"
//!
//! [retention]
//! policy = "keep_everything"                # keep_everything | on_confirm | keep_days | disk_pct
//! dry_run = true                            # MUST stay true until M-Perseus-MVP sign-off
//! # i_have_verified_the_soak = true         # REQUIRED to allow dry_run = false (task M4)
//! ```
//!
//! # The soak opt-in (task M4)
//!
//! Live deletion (`dry_run = false`) is only accepted when the config ALSO sets
//! the explicit, greppable flag `i_have_verified_the_soak = true`. The two-key
//! handshake is deliberate: `dry_run = false` alone is treated as a
//! misconfiguration and rejected with an actionable error, so no operator can
//! enable irreversible source deletion without having consciously typed the soak
//! acknowledgement. Both default to the safe value (`dry_run = true`,
//! `i_have_verified_the_soak = false`), so a fresh config never deletes anything.
//!
//! Two optional tuning fields are additive (defaulted, absent from the contract
//! sample): `stability_secs` (write-stability quiet window, default 10) and
//! `poll_interval_secs` (re-stat cadence, default 2). Validation errors are
//! actionable — they name the offending field and the accepted values.

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

/// Default `keep_days` threshold (days) for the `keep_days` policy.
pub const DEFAULT_KEEP_DAYS: u32 = 30;
/// Default disk-usage cap (percent) for the `disk_pct` policy.
pub const DEFAULT_DISK_MAX_PCT: u8 = 90;
/// Default retention evaluation cadence (seconds) — hourly.
pub const DEFAULT_RETENTION_INTERVAL_SECS: u64 = 3600;

fn default_stability_secs() -> u64 {
    DEFAULT_STABILITY_SECS
}
fn default_poll_interval_secs() -> u64 {
    DEFAULT_POLL_INTERVAL_SECS
}
fn default_true() -> bool {
    true
}
fn default_keep_days() -> u32 {
    DEFAULT_KEEP_DAYS
}
fn default_disk_max_pct() -> u8 {
    DEFAULT_DISK_MAX_PCT
}
fn default_retention_interval_secs() -> u64 {
    DEFAULT_RETENTION_INTERVAL_SECS
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
    /// default and stays on until the M-Perseus-MVP gate passes. Setting it to
    /// `false` is a **gated** action (task M4): it is only accepted when
    /// [`i_have_verified_the_soak`](Self::i_have_verified_the_soak) is also
    /// `true`; otherwise validation rejects the config. This keeps live deletion
    /// impossible-by-accident while making the go-live an explicit two-key edit
    /// the owner performs after the A9 soak sign-off.
    #[serde(default = "default_true")]
    pub dry_run: bool,
    /// Explicit soak opt-in (task M4). Live deletion (`dry_run = false`) is only
    /// permitted when this is `true`. The flag name is deliberately long,
    /// unmistakable, and greppable — it is the operator's typed acknowledgement
    /// that the M-Perseus-MVP soak has been observed and real, irreversible
    /// deletion of confirmed source frames may begin. Defaults to `false`, so a
    /// config that merely sets `dry_run = false` is rejected as a
    /// misconfiguration rather than silently going live.
    #[serde(default)]
    pub i_have_verified_the_soak: bool,
    /// `keep_days` threshold in days (only consulted by the `keep_days` policy).
    /// Additive/defaulted; absent from the contract sample.
    #[serde(default = "default_keep_days")]
    pub keep_days: u32,
    /// Disk-usage cap in percent (only consulted by the `disk_pct` policy).
    /// Additive/defaulted; absent from the contract sample.
    #[serde(default = "default_disk_max_pct")]
    pub disk_max_pct: u8,
    /// How often the retention evaluator runs, in seconds (default hourly).
    /// Additive/defaulted; absent from the contract sample.
    #[serde(default = "default_retention_interval_secs")]
    pub interval_secs: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            policy: RetentionPolicy::KeepEverything,
            dry_run: true,
            i_have_verified_the_soak: false,
            keep_days: DEFAULT_KEEP_DAYS,
            disk_max_pct: DEFAULT_DISK_MAX_PCT,
            interval_secs: DEFAULT_RETENTION_INTERVAL_SECS,
        }
    }
}

impl RetentionConfig {
    /// Map the parsed config policy onto the core evaluator's parameterised
    /// [`athenaeum_core::sync::RetentionPolicy`], injecting the configured
    /// `keep_days` / `disk_max_pct` tuning values. This is the single seam
    /// between Perseus's flat TOML enum and core's carrying enum.
    pub fn to_core_policy(&self) -> athenaeum_core::sync::RetentionPolicy {
        use athenaeum_core::sync::RetentionPolicy as Core;
        match self.policy {
            RetentionPolicy::KeepEverything => Core::KeepEverything,
            RetentionPolicy::OnConfirm => Core::OnConfirm,
            RetentionPolicy::KeepDays => Core::KeepDays(self.keep_days),
            RetentionPolicy::DiskPct => Core::DiskPct {
                max_pct: self.disk_max_pct,
            },
        }
    }

    /// Retention evaluation cadence as a [`Duration`].
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }
}

/// Default hub base URL — matches the app's `account.hub_url` default so a
/// bare `[account]` table (email only) points at the production hub.
pub const DEFAULT_HUB_URL: &str = "https://projects.artfrom.space";

fn default_hub_url() -> String {
    DEFAULT_HUB_URL.to_string()
}

/// The `[account]` table (task M1). Present when Perseus pairs via the hub account
/// (a `perseus login` device token) rather than a raw pairing ticket. The token
/// itself is NEVER in TOML — it lives in a 0600 file in `data_dir`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Hub base URL (defaults to the production hub).
    #[serde(default = "default_hub_url")]
    pub hub_url: String,
    /// Account email. Optional in the file — `perseus login` prompts when absent.
    #[serde(default)]
    pub email: Option<String>,
    /// The paired primary's hub device id. Optional — when absent, Perseus
    /// auto-picks the account's single `primary` device.
    #[serde(default)]
    pub primary_device_id: Option<String>,
    /// **Dev only.** When the hub's relay map is empty/unreachable and there is
    /// no cached relay map yet, allow falling back to iroh's public default
    /// relays (task M1 review finding #1). Defaults to `false` — a signed-in
    /// production Perseus agent must not silently start riding the public n0
    /// relays just because the hub is misconfigured; that failure should be
    /// loud. Set `true` only for dev/test environments without a hub relay
    /// deployment.
    #[serde(default)]
    pub allow_default_relays: bool,
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            hub_url: default_hub_url(),
            email: None,
            primary_device_id: None,
            allow_default_relays: false,
        }
    }
}

/// Parsed + validated Perseus configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Single capture directory (legacy form). Exactly one of `capture_dir` /
    /// `capture_dirs` must be set — [`validate`](Self::validate) enforces this.
    #[serde(default)]
    pub capture_dir: Option<PathBuf>,
    /// One or more capture directories to watch. The multi-directory form: a
    /// separate watcher is armed per entry, all feeding the one enqueue pipeline.
    #[serde(default)]
    pub capture_dirs: Vec<PathBuf>,
    pub data_dir: PathBuf,
    /// Dev-ticket pairing route (task M1: optional now that `[account]` exists).
    #[serde(default)]
    pub pairing_ticket: Option<String>,
    /// Account-pairing route (task M1).
    #[serde(default)]
    pub account: Option<AccountConfig>,
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
                 data_dir, mode = \"auto\", a pairing route (either an [account] \
                 table or pairing_ticket), and a [retention] table with policy = \
                 keep_everything|on_confirm|keep_days|disk_pct and dry_run = true"
            )
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The effective watch list: the singular `capture_dir` as a one-item list,
    /// or the `capture_dirs` array. [`validate`](Self::validate) guarantees
    /// exactly one form is populated, so this is unambiguous. Every consumer of
    /// a capture directory (the watcher spawn loop, the retention disk probe,
    /// the status banner) goes through here — never the raw fields.
    pub fn capture_dirs_resolved(&self) -> Vec<PathBuf> {
        if let Some(d) = &self.capture_dir {
            vec![d.clone()]
        } else {
            self.capture_dirs.clone()
        }
    }

    /// Structural validation of already-parsed fields. Iroh-ticket well-formedness
    /// is deliberately NOT checked here — that lives in the production transport
    /// wiring (`run`), so tests and the loopback path can supply a placeholder.
    pub fn validate(&self) -> Result<()> {
        // Exactly one of the two capture-directory forms must be set.
        match (&self.capture_dir, self.capture_dirs.is_empty()) {
            (Some(_), false) => {
                bail!("set either capture_dir or capture_dirs in perseus.toml, not both")
            }
            (None, true) => bail!("capture_dir (or capture_dirs) is required"),
            _ => {}
        }
        for dir in self.capture_dirs_resolved() {
            if dir.as_os_str().is_empty() {
                bail!("capture directory must not be empty");
            }
            if !dir.exists() {
                bail!(
                    "capture directory does not exist: {} — create it (or point \
                     at the right path) before starting Perseus",
                    dir.display()
                );
            }
        }
        if self.data_dir.as_os_str().is_empty() {
            bail!("data_dir must not be empty");
        }
        // Pairing route (task M1): at least one of [account] or pairing_ticket.
        let has_account = self.account.is_some();
        let ticket_present = self
            .pairing_ticket
            .as_ref()
            .is_some_and(|t| !t.trim().is_empty());
        if !has_account && !ticket_present {
            bail!(
                "no pairing route configured — add an [account] table (then run \
                 `perseus login`) or set pairing_ticket to the ticket from the \
                 primary's Settings → Sync (dev)"
            );
        }
        // A present-but-blank pairing_ticket is a misconfiguration, not "absent".
        if self.pairing_ticket.as_ref().is_some_and(|t| t.trim().is_empty()) {
            bail!(
                "pairing_ticket is present but empty — remove it to use [account] \
                 pairing, or paste a real ticket"
            );
        }
        if let Some(account) = &self.account {
            if account.hub_url.trim().is_empty() {
                bail!("[account].hub_url must not be empty");
            }
            if let Some(email) = &account.email {
                if email.trim().is_empty() {
                    bail!("[account].email is present but empty — remove it or set a real address");
                }
            }
        }
        // `mode` is an enum, so any non-`auto` value already failed to parse.
        // Hard invariant (plan Global Constraints): dry-run is the default and
        // live deletion is a GATED, explicit action. `dry_run = false` is only
        // accepted alongside the soak opt-in — otherwise it is a
        // misconfiguration, refused with an actionable message, so the config
        // can never quietly imply irreversible source deletion (task M4).
        if !self.retention.dry_run && !self.retention.i_have_verified_the_soak {
            bail!(
                "retention.dry_run = false requires the explicit soak opt-in: \
                 also set `i_have_verified_the_soak = true` in the [retention] \
                 table once the M-Perseus-MVP soak has been signed off. Until \
                 then, keep dry_run = true (the safe default)"
            );
        }
        if self.stability_secs == 0 {
            bail!("stability_secs must be >= 1");
        }
        if self.poll_interval_secs == 0 {
            bail!("poll_interval_secs must be >= 1");
        }
        // Retention tuning: only meaningful for keep_days / disk_pct, but
        // validate unconditionally so a bad value is caught early, not on the
        // first tick after a policy change.
        if self.retention.keep_days == 0 {
            bail!("retention.keep_days must be >= 1");
        }
        if self.retention.disk_max_pct < 1 || self.retention.disk_max_pct > 100 {
            bail!(
                "retention.disk_max_pct must be between 1 and 100 (got {})",
                self.retention.disk_max_pct
            );
        }
        if self.retention.interval_secs == 0 {
            bail!("retention.interval_secs must be >= 1");
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

    /// Minimal valid-shape TOML with the caller's own capture line(s) spliced in
    /// — used by the multiple-capture-directory tests so they supply
    /// `capture_dir` / `capture_dirs` themselves (unlike [`good_toml`], which
    /// hardcodes the singular form).
    fn toml_with(capture_line: &str) -> String {
        format!(
            r#"
{capture_line}
data_dir = "/var/lib/perseus"
pairing_ticket = "ticket-abc"
mode = "auto"
[retention]
policy = "keep_everything"
dry_run = true
"#
        )
    }

    /// Task 7: the array form parses and `capture_dirs_resolved()` returns every
    /// listed directory in order.
    #[test]
    fn capture_dirs_array_parses() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let toml = toml_with(&format!(
            "capture_dirs = [\"{}\", \"{}\"]",
            a.path().display(),
            b.path().display()
        ));
        let c = Config::from_toml_str(&toml).expect("array form is valid");
        assert_eq!(
            c.capture_dirs_resolved(),
            vec![a.path().to_path_buf(), b.path().to_path_buf()]
        );
    }

    /// Task 7: the legacy singular `capture_dir` still works and resolves to a
    /// one-item watch list.
    #[test]
    fn capture_dir_singular_still_works() {
        let a = tempfile::tempdir().unwrap();
        let toml = toml_with(&format!("capture_dir = \"{}\"", a.path().display()));
        let c = Config::from_toml_str(&toml).expect("singular form is valid");
        assert_eq!(c.capture_dirs_resolved(), vec![a.path().to_path_buf()]);
    }

    /// Task 7: setting BOTH forms is a misconfiguration — rejected with an
    /// actionable message naming both keys. (Uses real dirs so it is the
    /// exactly-one guard that fires, not the existence check.)
    #[test]
    fn both_forms_rejected() {
        let a = tempfile::tempdir().unwrap();
        let toml = toml_with(&format!(
            "capture_dir = \"{d}\"\ncapture_dirs = [\"{d}\"]",
            d = a.path().display()
        ));
        let err = Config::from_toml_str(&toml).expect_err("both forms must be rejected");
        assert!(
            err.to_string().contains("either capture_dir or capture_dirs"),
            "error should name both keys: {err:#}"
        );
    }

    /// Task 7: neither form present → rejected (a capture directory is required).
    #[test]
    fn neither_form_rejected() {
        let err = Config::from_toml_str(&toml_with("")).expect_err("neither form must be rejected");
        assert!(
            err.chain().any(|c| c.to_string().contains("capture_dir")),
            "error should demand a capture dir: {err:#}"
        );
    }

    #[test]
    fn parses_the_contract_shape() {
        let capture = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&good_toml(capture.path())).expect("valid config");
        assert_eq!(cfg.capture_dir.as_deref(), Some(capture.path()));
        assert_eq!(cfg.capture_dirs_resolved(), vec![capture.path().to_path_buf()]);
        assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/perseus"));
        assert_eq!(cfg.pairing_ticket.as_deref(), Some("ticket-abc"));
        assert!(cfg.account.is_none(), "no [account] table in the ticket-only contract shape");
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

    /// Task M4: `dry_run = false` WITHOUT the soak opt-in is rejected, and the
    /// error names the exact flag the operator must add — actionable, not cryptic.
    #[test]
    fn dry_run_false_without_soak_optin_is_rejected_with_actionable_message() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path()).replace("dry_run = true", "dry_run = false");
        let err = Config::from_toml_str(&text).expect_err("dry_run=false without opt-in must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("i_have_verified_the_soak"),
            "error must name the soak opt-in flag: {msg}"
        );
    }

    /// Task M4: `dry_run = false` WITH `i_have_verified_the_soak = true` is the
    /// only accepted live-deletion configuration — it parses and validates.
    #[test]
    fn dry_run_false_with_soak_optin_is_accepted() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path())
            .replace("dry_run = true", "dry_run = false\ni_have_verified_the_soak = true");
        let cfg = Config::from_toml_str(&text).expect("dry_run=false + opt-in must be accepted");
        assert!(!cfg.retention.dry_run, "live mode is enabled");
        assert!(cfg.retention.i_have_verified_the_soak, "the opt-in is recorded");
    }

    /// Task M4: the soak opt-in defaults to `false` and, on its own (with the
    /// safe `dry_run = true`), changes nothing — it is inert until paired with
    /// `dry_run = false`.
    #[test]
    fn soak_optin_defaults_false_and_is_inert_under_dry_run() {
        let capture = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&good_toml(capture.path())).unwrap();
        assert!(!cfg.retention.i_have_verified_the_soak, "opt-in defaults to false");
        assert!(cfg.retention.dry_run, "dry-run stays the default");
        // opt-in true while dry_run stays true is still valid (inert).
        let text = good_toml(capture.path())
            .replace("dry_run = true", "dry_run = true\ni_have_verified_the_soak = true");
        let cfg = Config::from_toml_str(&text).expect("opt-in with dry_run stays valid");
        assert!(cfg.retention.dry_run, "dry-run wins — nothing is deleted");
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

    /// Task M1: an `[account]` table alone is a valid pairing route — no
    /// `pairing_ticket` required. `hub_url` defaults to the production hub.
    #[test]
    fn account_only_config_parses_without_ticket() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            r#"
capture_dir = "{}"
data_dir = "/d"
mode = "auto"
[account]
email = "me@example.com"
[retention]
policy = "keep_everything"
dry_run = true
"#,
            capture.path().display()
        );
        let cfg = Config::from_toml_str(&text).expect("account-only config is valid");
        assert!(cfg.pairing_ticket.is_none(), "no ticket needed with [account]");
        let account = cfg.account.expect("account table present");
        assert_eq!(account.hub_url, DEFAULT_HUB_URL, "hub_url defaults to the production hub");
        assert_eq!(account.email.as_deref(), Some("me@example.com"));
        assert!(account.primary_device_id.is_none(), "primary auto-picked when omitted");
        assert!(
            !account.allow_default_relays,
            "allow_default_relays must default to false (task M1 review finding #1)"
        );
    }

    /// Task M1: account + explicit hub_url + primary_device_id all parse.
    #[test]
    fn account_config_with_all_fields_parses() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            r#"
capture_dir = "{}"
data_dir = "/d"
mode = "auto"
[account]
hub_url = "https://staging.example.org"
email = "me@example.com"
primary_device_id = "dev-primary-1"
allow_default_relays = true
[retention]
policy = "keep_everything"
dry_run = true
"#,
            capture.path().display()
        );
        let account = Config::from_toml_str(&text).unwrap().account.unwrap();
        assert_eq!(account.hub_url, "https://staging.example.org");
        assert_eq!(account.primary_device_id.as_deref(), Some("dev-primary-1"));
        assert!(account.allow_default_relays, "explicit true must parse through");
    }

    /// Task M1: neither a pairing route present → rejected with an actionable
    /// message naming both routes.
    #[test]
    fn no_pairing_route_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            "capture_dir=\"{}\"\ndata_dir=\"/d\"\nmode=\"auto\"\n[retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
            capture.path().display()
        );
        let err = Config::from_toml_str(&text).expect_err("no pairing route must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("pairing") && (msg.contains("account") || msg.contains("pairing_ticket")),
            "error should name the pairing routes: {msg}"
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
            err.chain().any(|c| c.to_string().contains("does not exist")),
            "error should say the capture directory does not exist: {err:#}"
        );
    }

    #[test]
    fn retention_tuning_defaults_when_omitted() {
        let capture = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&good_toml(capture.path())).unwrap();
        assert_eq!(cfg.retention.keep_days, DEFAULT_KEEP_DAYS);
        assert_eq!(cfg.retention.disk_max_pct, DEFAULT_DISK_MAX_PCT);
        assert_eq!(cfg.retention.interval_secs, DEFAULT_RETENTION_INTERVAL_SECS);
    }

    #[test]
    fn retention_tuning_parses_when_present() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            r#"
capture_dir = "{}"
data_dir = "/d"
pairing_ticket = "t"
mode = "auto"
[retention]
policy = "keep_days"
dry_run = true
keep_days = 7
disk_max_pct = 80
interval_secs = 600
"#,
            capture.path().display()
        );
        let cfg = Config::from_toml_str(&text).expect("valid config");
        assert_eq!(cfg.retention.keep_days, 7);
        assert_eq!(cfg.retention.disk_max_pct, 80);
        assert_eq!(cfg.retention.interval_secs, 600);
    }

    #[test]
    fn to_core_policy_carries_tuning_values() {
        use athenaeum_core::sync::RetentionPolicy as Core;
        let mut r = RetentionConfig {
            policy: RetentionPolicy::KeepDays,
            dry_run: true,
            i_have_verified_the_soak: false,
            keep_days: 14,
            disk_max_pct: 85,
            interval_secs: 3600,
        };
        assert_eq!(r.to_core_policy(), Core::KeepDays(14));
        r.policy = RetentionPolicy::DiskPct;
        assert_eq!(r.to_core_policy(), Core::DiskPct { max_pct: 85 });
        r.policy = RetentionPolicy::OnConfirm;
        assert_eq!(r.to_core_policy(), Core::OnConfirm);
        r.policy = RetentionPolicy::KeepEverything;
        assert_eq!(r.to_core_policy(), Core::KeepEverything);
    }

    #[test]
    fn zero_keep_days_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            "capture_dir=\"{}\"\ndata_dir=\"/d\"\npairing_ticket=\"t\"\nmode=\"auto\"\n[retention]\npolicy=\"keep_days\"\ndry_run=true\nkeep_days=0\n",
            capture.path().display()
        );
        let err = Config::from_toml_str(&text).expect_err("keep_days=0 must fail");
        assert!(err.chain().any(|c| c.to_string().contains("keep_days")));
    }

    #[test]
    fn out_of_range_disk_max_pct_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            "capture_dir=\"{}\"\ndata_dir=\"/d\"\npairing_ticket=\"t\"\nmode=\"auto\"\n[retention]\npolicy=\"disk_pct\"\ndry_run=true\ndisk_max_pct=150\n",
            capture.path().display()
        );
        let err = Config::from_toml_str(&text).expect_err("disk_max_pct=150 must fail");
        assert!(err.chain().any(|c| c.to_string().contains("disk_max_pct")));
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
