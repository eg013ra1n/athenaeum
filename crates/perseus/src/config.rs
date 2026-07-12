//! Perseus TOML configuration: parse + validate.
//!
//! The binding contract is the `perseus.toml` shape from the task brief. Perseus
//! is a **send-only** capability (Sync 2C): it signs into an account and sends
//! captures to explicit **targets**. There are two routes — an **account** (with
//! ≥1 target) or a **dev ticket** — and at least one must be present:
//!
//! ```toml
//! capture_dir = "/data/capture"
//! data_dir = "/var/lib/perseus"
//! mode = "auto"                             # "auto" or "manual" (Phase 2)
//! auto_quiet_secs = 60                       # auto: flush after N idle seconds
//! device_name = "Observatory Pi"            # optional; defaults to the hostname
//!
//! # Route A — account (recommended). Sign in with `perseus login`; the device
//! # token lands in a 0600 file in data_dir (NEVER in this TOML). `targets` names
//! # the account devices to send to (by device name or id).
//! targets = ["Studio Mac"]
//! [account]
//! hub_url = "https://projects.artfrom.space"
//! email = "me@example.com"                  # optional; prompted at login if absent
//! allow_default_relays = false              # dev only; see AccountConfig docs
//!
//! # Route B — dev ticket (offline dev / tests). Optional now that [account] exists.
//! pairing_ticket = "<paste from a receiver's Settings → Sync (dev)>"
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

/// Default auto-mode quiet window (seconds): in `Mode::Auto` the batcher flushes
/// a pending send after this many seconds elapse with no new capture arriving.
pub const DEFAULT_AUTO_QUIET_SECS: u64 = 60;

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
fn default_auto_quiet_secs() -> u64 {
    DEFAULT_AUTO_QUIET_SECS
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

/// Default local status-page bind address (loopback only). Overridable in TOML;
/// an empty string disables the embedded web server entirely.
pub const DEFAULT_WEB_BIND: &str = "127.0.0.1:8686";

/// Minimum length of a `web_token` protecting a non-loopback bind (finding M1).
/// A LAN-exposed admin page must not be guarded by an operator-invented short
/// string; require enough entropy that guessing is infeasible. Generate one
/// with e.g. `openssl rand -base64 24`.
pub const MIN_WEB_TOKEN_LEN: usize = 16;

fn default_web_bind() -> String {
    DEFAULT_WEB_BIND.to_string()
}

/// Agent send-behaviour mode. The enum exists so an unknown mode is a clear
/// parse error rather than a silently ignored string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Auto-send every new frame the watcher stabilizes (batched by the
    /// `auto_quiet_secs` quiet window).
    Auto,
    /// Queue stabilized frames; the operator triggers the send explicitly from
    /// the web page (Phase 2).
    Manual,
}

/// A snapshot of the send-behaviour knobs the batcher (Task 4) and web page
/// (Task 6) read live: the current [`Mode`] plus the auto-mode quiet window.
/// Cheap to copy so a `watch` channel can hand out the latest value without a
/// lock on the whole [`Config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendCfg {
    pub mode: Mode,
    pub auto_quiet_secs: u64,
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

/// The `[account]` table. Present when Perseus signs into the hub account
/// (a `perseus login` device token) rather than pairing via a raw dev ticket.
/// The token itself is NEVER in TOML — it lives in a 0600 file in `data_dir`.
/// In the Sync 2C mesh model Perseus is a send-only capability that sends to the
/// explicit [`Config::targets`]; there is no per-account "primary" to pair to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Hub base URL (defaults to the production hub).
    #[serde(default = "default_hub_url")]
    pub hub_url: String,
    /// Account email. Optional in the file — `perseus login` prompts when absent.
    #[serde(default)]
    pub email: Option<String>,
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
            allow_default_relays: false,
        }
    }
}

/// A gap that still blocks the sync engine from starting. An empty
/// `Vec<SetupNeed>` (see [`Config::setup_needs`]) means the agent is ready; a
/// non-empty one is what the tray/web UI renders as "finish setup" prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupNeed {
    /// No capture folders configured — nothing to watch yet.
    CaptureDirs,
    /// No usable send target — neither a signed-in account with at least one
    /// entry in [`Config::targets`], nor a dev pairing ticket.
    Targets,
}

impl std::fmt::Display for SetupNeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupNeed::CaptureDirs => write!(f, "no capture folders configured"),
            SetupNeed::Targets => write!(f, "no send target configured (sign in and add a target)"),
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
    /// Account sign-in route.
    #[serde(default)]
    pub account: Option<AccountConfig>,
    /// The devices in the account to send captures to, by device name or id
    /// (Sync 2C mesh model). Perseus is send-only, so at least one target is
    /// required for the account route (the dev ticket is the alternative). Task 6
    /// resolves `targets[0]`; the full multi-target send is task 7.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Optional friendly name for this node in the account device list. When
    /// absent, sign-in defaults to the machine hostname
    /// ([`athenaeum_core::account::default_device_name`]).
    #[serde(default)]
    pub device_name: Option<String>,
    pub mode: Mode,
    /// Auto-mode quiet window in seconds: in [`Mode::Auto`] the batcher flushes a
    /// pending send once this many seconds pass with no new capture. Additive/
    /// defaulted (absent from the contract sample); ignored in [`Mode::Manual`].
    #[serde(default = "default_auto_quiet_secs")]
    pub auto_quiet_secs: u64,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default = "default_stability_secs")]
    pub stability_secs: u64,
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Local web status page bind address (task 9). Defaults to loopback
    /// (`127.0.0.1:8686`). An empty string disables the embedded server. A
    /// non-loopback bind requires [`web_token`](Self::web_token) —
    /// [`validate`](Self::validate) refuses to start otherwise.
    #[serde(default = "default_web_bind")]
    pub web_bind: String,
    /// Bearer token required for a non-loopback [`web_bind`](Self::web_bind).
    /// Absent by default; only meaningful when the status page is exposed off
    /// loopback, where it becomes mandatory (validation-enforced).
    #[serde(default)]
    pub web_token: Option<String>,
}

impl Config {
    /// Parse + strictly validate a config from a TOML file on disk.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read perseus config {}", path.display()))?;
        Self::from_toml_str(&text)
            .with_context(|| format!("invalid perseus config {}", path.display()))
    }

    /// Parse + structurally validate a config from a TOML file on disk — the
    /// setup-state counterpart to [`load`](Self::load). A file with no capture
    /// folders and no pairing route loads successfully; the supervisor turns the
    /// remaining gaps into [`SetupNeed`]s (see [`setup_needs`](Self::setup_needs))
    /// rather than refusing to start.
    pub fn load_lenient(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read perseus config {}", path.display()))?;
        Self::from_toml_str_lenient(&text)
            .with_context(|| format!("invalid perseus config {}", path.display()))
    }

    /// Parse + strictly validate a config from a TOML string. Split out so
    /// validation is unit-testable without touching the filesystem.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let cfg = Self::parse_toml(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Lenient counterpart to [`from_toml_str`](Self::from_toml_str): parse +
    /// [`validate_structure`](Self::validate_structure) only. A structurally sound
    /// but incomplete config (empty capture list, no pairing route) is accepted;
    /// use [`setup_needs`](Self::setup_needs) to learn what is still missing.
    pub fn from_toml_str_lenient(text: &str) -> Result<Self> {
        let cfg = Self::parse_toml(text)?;
        cfg.validate_structure()?;
        Ok(cfg)
    }

    /// Deserialize the TOML with the shared friendly parse-error message. Both the
    /// strict and lenient constructors go through here so the "expected keys" hint
    /// stays in one place.
    fn parse_toml(text: &str) -> Result<Self> {
        toml::from_str(text).map_err(|e| {
            anyhow::anyhow!(
                "could not parse config TOML: {e}. Expected keys: capture_dir, \
                 data_dir, mode = \"auto\", a send route (either an [account] table \
                 with targets = [..], or pairing_ticket), and a [retention] table \
                 with policy = keep_everything|on_confirm|keep_days|disk_pct and \
                 dry_run = true"
            )
        })
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

    /// Strict validation (unchanged external behavior): structure + readiness.
    ///
    /// A config that passes this is both well-formed AND complete enough to start
    /// the sync engine. The two halves are separable: [`validate_structure`] is
    /// the "is this file broken?" check, `validate_ready` (private) adds the "is
    /// setup finished?" demands that lenient callers skip (see [`setup_needs`]).
    ///
    /// [`validate_structure`]: Self::validate_structure
    /// [`setup_needs`]: Self::setup_needs
    pub fn validate(&self) -> Result<()> {
        self.validate_structure()?;
        self.validate_ready()
    }

    /// Structural checks only — a config that fails here is BROKEN. A config that
    /// passes may still be incomplete (see [`setup_needs`](Self::setup_needs)):
    /// empty capture list and missing pairing route are legal setup states.
    ///
    /// Iroh-ticket well-formedness is deliberately NOT checked here — that lives
    /// in the production transport wiring (`run`), so tests and the loopback path
    /// can supply a placeholder.
    pub fn validate_structure(&self) -> Result<()> {
        // Setting BOTH capture-directory forms is a structural misconfiguration
        // (an empty list, by contrast, is a legal setup state — see
        // `validate_ready`). Keep the wording actionable; tests pin the substring.
        if self.capture_dir.is_some() && !self.capture_dirs.is_empty() {
            bail!("set either capture_dir or capture_dirs in perseus.toml, not both");
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
        // Web status page (task 9). Empty disables it. A non-loopback bind MUST
        // carry a bearer token — otherwise anyone who can reach the port could
        // read transfer history and (task 10) edit retention. Refuse to START
        // rather than silently binding wide-open; this is a hard startup gate,
        // not a runtime 401.
        if !self.web_bind.is_empty() {
            let addr: std::net::SocketAddr = self.web_bind.parse().map_err(|e| {
                anyhow::anyhow!("web_bind is not a valid socket address ({}): {e}", self.web_bind)
            })?;
            if !addr.ip().is_loopback() {
                let token = self.web_token.as_deref().unwrap_or("");
                if token.is_empty() {
                    bail!(
                        "web_bind {} is not loopback — set web_token to protect the status page",
                        self.web_bind
                    );
                }
                // Finding M1: reject an operator-invented weak token on a
                // LAN-exposed bind — it must carry real entropy.
                if token.chars().count() < MIN_WEB_TOKEN_LEN {
                    bail!(
                        "web_token is too weak ({} chars) for the non-loopback bind {} — use at least {} random characters (e.g. `openssl rand -base64 24`)",
                        token.chars().count(),
                        self.web_bind,
                        MIN_WEB_TOKEN_LEN
                    );
                }
            }
        }
        Ok(())
    }

    /// The two readiness demands strict mode adds on top of structure: at least
    /// one capture directory, and a send target. A config that fails only here
    /// is not broken — it is a valid "freshly installed, not yet set up" state
    /// (surfaced as [`SetupNeed`]s rather than an error by the lenient path).
    fn validate_ready(&self) -> Result<()> {
        if self.capture_dirs_resolved().is_empty() {
            bail!("capture_dir (or capture_dirs) is required");
        }
        // Send target (Sync 2C): at least one entry in `targets` (account route),
        // or a dev pairing ticket. The old single-primary pairing requirement is
        // replaced by explicit targets.
        if !self.has_configured_send_target() {
            bail!(
                "no send target configured — add at least one device to `targets` \
                 (a device name or id from your account, then run `perseus login`) \
                 or set pairing_ticket to the ticket from a receiver's Settings → \
                 Sync (dev)"
            );
        }
        Ok(())
    }

    /// Whether the config, on its own, names a send target: a non-empty
    /// `targets` list, or a dev pairing ticket. This is the config-only half of
    /// readiness (it cannot know whether a device token is stored — that is the
    /// `token_present` argument to [`setup_needs`](Self::setup_needs)).
    fn has_configured_send_target(&self) -> bool {
        let ticket_present = self
            .pairing_ticket
            .as_ref()
            .is_some_and(|t| !t.trim().is_empty());
        let has_target = self.targets.iter().any(|t| !t.trim().is_empty());
        ticket_present || has_target
    }

    /// What still blocks the sync engine from starting for this config. An empty
    /// result means "ready" — the same bar the private `validate_ready` enforces,
    /// expressed as a list the tray/web UI can render instead of a hard error.
    /// `token_present` is the caller's answer to "is there a stored hub
    /// device token" (see `account::token_present`); config alone cannot know,
    /// because the token lives in a 0600 file in `data_dir`, never in the TOML.
    pub fn setup_needs(&self, token_present: bool) -> Vec<SetupNeed> {
        let mut needs = Vec::new();
        if self.capture_dirs_resolved().is_empty() {
            needs.push(SetupNeed::CaptureDirs);
        }
        // A dev ticket is a self-contained send target (no account/token needed).
        let ticket = self
            .pairing_ticket
            .as_ref()
            .is_some_and(|t| !t.trim().is_empty());
        // The account route is usable only when signed in (a stored token) AND at
        // least one target is named — Perseus is send-only, so with no target
        // there is nowhere to send.
        let account_targets = self.account.is_some()
            && token_present
            && self.targets.iter().any(|t| !t.trim().is_empty());
        if !ticket && !account_targets {
            needs.push(SetupNeed::Targets);
        }
        needs
    }

    /// A cheap, copyable snapshot of the send-behaviour knobs (mode + auto quiet
    /// window). The batcher (Task 4) and web page (Task 6) read this rather than
    /// the whole [`Config`], so a live edit can be published on a `watch` channel.
    pub fn send_cfg(&self) -> SendCfg {
        SendCfg {
            mode: self.mode,
            auto_quiet_secs: self.auto_quiet_secs,
        }
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

    /// A minimal, valid fallback the supervisor uses when the on-disk config
    /// cannot be parsed at startup: platform data dir, loopback status page, no
    /// token, empty capture list, no pairing. It exists only so the always-on
    /// web page can still bind (and then surface the parse error) and logging has
    /// a home; the supervisor loop reloads the real file each pass and publishes
    /// `Failed { error }` until the typo is fixed. Because `web_bind` is loopback
    /// and `web_token` is `None`, the non-loopback-needs-a-token rule is not
    /// weakened — this is used only when the file genuinely cannot be parsed.
    pub fn fallback() -> Self {
        Self {
            capture_dir: None,
            capture_dirs: Vec::new(),
            data_dir: platform_data_dir(),
            pairing_ticket: None,
            account: None,
            targets: Vec::new(),
            device_name: None,
            mode: Mode::Auto,
            auto_quiet_secs: DEFAULT_AUTO_QUIET_SECS,
            retention: RetentionConfig::default(),
            stability_secs: DEFAULT_STABILITY_SECS,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            web_bind: default_web_bind(),
            web_token: None,
        }
    }
}

/// Per-platform application subdirectory name. Linux XDG convention is
/// lowercase (`~/.config/perseus`); macOS/Windows use the capitalized product
/// name to match their Application Support / AppData conventions.
fn app_dir_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "perseus"
    } else {
        "Perseus"
    }
}

/// Default on-disk config file location:
/// `~/Library/Application Support/Perseus/perseus.toml` (macOS),
/// `%APPDATA%\Perseus\perseus.toml` (Windows),
/// `~/.config/perseus/perseus.toml` (Linux). Falls back to the current
/// directory if the platform config dir cannot be determined.
pub fn platform_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(app_dir_name())
        .join("perseus.toml")
}

/// Default `data_dir` for a first-run config: the platform data directory plus
/// `<app>/data`. Falls back to the current directory if it cannot be determined.
pub fn platform_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(app_dir_name())
        .join("data")
}

/// Resolve which config file to use: explicit `--config` flag wins; otherwise a
/// legacy `./perseus.toml` in the current directory is honored if present; else
/// the platform path.
pub fn resolve_config_path(explicit: Option<PathBuf>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_config_path_in(&cwd, explicit)
}

/// Testable core of [`resolve_config_path`]: precedence is
/// `explicit > <cwd>/perseus.toml (if it exists) > platform path`.
pub fn resolve_config_path_in(cwd: &Path, explicit: Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    let legacy = cwd.join("perseus.toml");
    if legacy.exists() {
        legacy
    } else {
        platform_config_path()
    }
}

/// First-run bootstrap: write the commented default template (with `data_dir`
/// substituted to the platform default) when `path` does not yet exist, creating
/// parent directories as needed. Returns `true` when this call created the file,
/// `false` when it already existed (a no-op).
pub fn ensure_config_exists(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
    }
    // TOML literal string (single quotes): Windows backslashes need no escaping.
    // Strip any stray single-quote from the path so it can't break out of the
    // literal string it is spliced into.
    let data_dir = platform_data_dir().display().to_string().replace('\'', "");
    let text = include_str!("config_template.toml").replace("{data_dir}", &data_dir);
    std::fs::write(path, text)
        .with_context(|| format!("write default config {}", path.display()))?;
    tracing::info!(path = %path.display(), "default config created");
    Ok(true)
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

    /// The top-level keys of a valid config (no `[retention]` table, which
    /// defaults when omitted). Callers append further **top-level** keys — e.g.
    /// `web_bind` — which must precede any table, then optionally their own
    /// table. Used by the task-9 web-bind tests.
    fn good_toml_top(capture_dir: &Path) -> String {
        format!(
            "capture_dir = \"{}\"\ndata_dir = \"/var/lib/perseus\"\npairing_ticket = \"ticket-abc\"\nmode = \"auto\"\n",
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

    /// Phase 2: `mode = "manual"` is now a valid send mode; only a genuinely
    /// unknown value is a parse error.
    #[test]
    fn manual_mode_parses() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path()).replace("mode = \"auto\"", "mode = \"manual\"");
        let cfg = Config::from_toml_str(&text).expect("manual is a valid send mode");
        assert_eq!(cfg.mode, Mode::Manual);
        // Additive quiet window defaults when the key is absent.
        assert_eq!(cfg.auto_quiet_secs, DEFAULT_AUTO_QUIET_SECS);
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path()).replace("mode = \"auto\"", "mode = \"bogus\"");
        assert!(
            Config::from_toml_str(&text).is_err(),
            "an unknown mode must be a parse error"
        );
    }

    /// The additive `auto_quiet_secs` key parses when present.
    #[test]
    fn auto_quiet_secs_parses_when_present() {
        let capture = tempfile::tempdir().unwrap();
        let text = good_toml(capture.path()).replace(
            "mode = \"auto\"",
            "mode = \"auto\"\nauto_quiet_secs = 15",
        );
        let cfg = Config::from_toml_str(&text).expect("valid config");
        assert_eq!(cfg.auto_quiet_secs, 15);
        assert_eq!(cfg.send_cfg().auto_quiet_secs, 15);
        assert_eq!(cfg.send_cfg().mode, Mode::Auto);
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

    /// Sync 2C: an `[account]` table with `targets` is a valid send route — no
    /// `pairing_ticket` required. `hub_url` defaults to the production hub.
    #[test]
    fn account_with_targets_config_parses_without_ticket() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            r#"
capture_dir = "{}"
data_dir = "/d"
mode = "auto"
targets = ["Studio Mac"]
[account]
email = "me@example.com"
[retention]
policy = "keep_everything"
dry_run = true
"#,
            capture.path().display()
        );
        let cfg = Config::from_toml_str(&text).expect("account+targets config is valid");
        assert!(cfg.pairing_ticket.is_none(), "no ticket needed with [account] + targets");
        assert_eq!(cfg.targets, vec!["Studio Mac".to_string()]);
        let account = cfg.account.expect("account table present");
        assert_eq!(account.hub_url, DEFAULT_HUB_URL, "hub_url defaults to the production hub");
        assert_eq!(account.email.as_deref(), Some("me@example.com"));
        assert!(
            !account.allow_default_relays,
            "allow_default_relays must default to false (task M1 review finding #1)"
        );
    }

    /// Sync 2C: account + explicit hub_url + targets + device_name all parse.
    #[test]
    fn account_config_with_all_fields_parses() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            r#"
capture_dir = "{}"
data_dir = "/d"
mode = "auto"
device_name = "Observatory Pi"
targets = ["dev-primary-1", "Laptop"]
[account]
hub_url = "https://staging.example.org"
email = "me@example.com"
allow_default_relays = true
[retention]
policy = "keep_everything"
dry_run = true
"#,
            capture.path().display()
        );
        let cfg = Config::from_toml_str(&text).unwrap();
        assert_eq!(cfg.device_name.as_deref(), Some("Observatory Pi"));
        assert_eq!(cfg.targets, vec!["dev-primary-1".to_string(), "Laptop".to_string()]);
        let account = cfg.account.unwrap();
        assert_eq!(account.hub_url, "https://staging.example.org");
        assert!(account.allow_default_relays, "explicit true must parse through");
    }

    /// Sync 2C: no send route present (no targets, no ticket) → rejected with an
    /// actionable message naming both routes.
    #[test]
    fn no_send_route_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            "capture_dir=\"{}\"\ndata_dir=\"/d\"\nmode=\"auto\"\n[account]\nemail=\"me@example.com\"\n[retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
            capture.path().display()
        );
        let err = Config::from_toml_str(&text).expect_err("no send route must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("send target") && (msg.contains("targets") || msg.contains("pairing_ticket")),
            "error should name the send routes: {msg}"
        );
    }

    /// Sync 2C: `targets` parses as a string list preserving order.
    #[test]
    fn targets_parse_as_ordered_list() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            "capture_dir=\"{}\"\ndata_dir=\"/d\"\nmode=\"auto\"\ntargets=[\"a\", \"b\", \"c\"]\n[account]\n[retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
            capture.path().display()
        );
        let cfg = Config::from_toml_str(&text).expect("targets list is valid");
        assert_eq!(cfg.targets, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    /// Sync 2C: `targets` defaults to an empty list when omitted, and
    /// `device_name` defaults to `None`.
    #[test]
    fn targets_and_device_name_default_when_omitted() {
        let capture = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&good_toml(capture.path())).unwrap();
        assert!(cfg.targets.is_empty(), "targets defaults to []");
        assert!(cfg.device_name.is_none(), "device_name defaults to None");
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

    /// Task 9: `web_bind` defaults to the loopback status-page address when the
    /// key is absent, and needs no token there.
    #[test]
    fn web_bind_defaults_to_loopback() {
        let capture = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&good_toml(capture.path())).unwrap();
        assert_eq!(cfg.web_bind, DEFAULT_WEB_BIND);
        assert_eq!(cfg.web_bind, "127.0.0.1:8686");
        assert!(cfg.web_token.is_none(), "no token needed for a loopback bind");
    }

    /// Task 9: a non-loopback bind WITHOUT a token is refused at validation with
    /// an actionable message — it must never silently bind wide-open.
    #[test]
    fn non_loopback_web_bind_without_token_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            "{}web_bind = \"0.0.0.0:8686\"\n",
            good_toml_top(capture.path())
        );
        let err = Config::from_toml_str(&text).expect_err("wide-open bind without token must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("web_token") && msg.contains("loopback"),
            "error must name web_token and loopback: {msg}"
        );
    }

    /// Task 9: a non-loopback bind WITH a strong token validates.
    #[test]
    fn non_loopback_web_bind_with_token_is_accepted() {
        let capture = tempfile::tempdir().unwrap();
        let strong = "s3cret-9f2b7c1a4e8d6053"; // >= MIN_WEB_TOKEN_LEN
        let text = format!(
            "{}web_bind = \"0.0.0.0:8686\"\nweb_token = \"{strong}\"\n",
            good_toml_top(capture.path())
        );
        let cfg = Config::from_toml_str(&text).expect("token-protected wide bind is valid");
        assert_eq!(cfg.web_bind, "0.0.0.0:8686");
        assert_eq!(cfg.web_token.as_deref(), Some(strong));
    }

    /// Finding M1: a non-loopback bind with a WEAK (too-short) token is refused —
    /// an operator-invented `web_token = "obs"` must not protect a LAN-exposed
    /// admin page.
    #[test]
    fn non_loopback_web_bind_with_weak_token_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!(
            "{}web_bind = \"0.0.0.0:8686\"\nweb_token = \"obs2026\"\n",
            good_toml_top(capture.path())
        );
        let err = Config::from_toml_str(&text).expect_err("a weak token must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("web_token") && msg.to_lowercase().contains("weak"),
            "error must flag the weak web_token: {msg}"
        );
    }

    /// Task 9: an empty `web_bind` disables the server and is always valid — no
    /// token, no address parsing.
    #[test]
    fn empty_web_bind_disables_server_and_is_valid() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!("{}web_bind = \"\"\n", good_toml_top(capture.path()));
        let cfg = Config::from_toml_str(&text).expect("empty web_bind is valid (disabled)");
        assert!(cfg.web_bind.is_empty());
    }

    /// Task 9: a malformed `web_bind` is a parse error at validation.
    #[test]
    fn malformed_web_bind_is_rejected() {
        let capture = tempfile::tempdir().unwrap();
        let text = format!("{}web_bind = \"not-an-address\"\n", good_toml_top(capture.path()));
        let err = Config::from_toml_str(&text).expect_err("malformed web_bind must fail");
        assert!(
            format!("{err:#}").contains("web_bind"),
            "error must name web_bind: {err:#}"
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

    #[test]
    fn lenient_allows_empty_dirs_and_no_pairing() {
        let text = "data_dir = \"/d\"\nmode = \"auto\"\ncapture_dirs = []\n";
        let cfg = Config::from_toml_str_lenient(text).expect("lenient must accept setup-state config");
        assert!(cfg.capture_dirs_resolved().is_empty());
        // strict still refuses the same text
        Config::from_toml_str(text).expect_err("strict must still demand dirs + pairing");
    }

    #[test]
    fn lenient_still_rejects_structural_errors() {
        // both capture forms set is a structural misconfiguration, not a setup gap
        let a = tempfile::tempdir().unwrap();
        let text = format!(
            "data_dir = \"/d\"\nmode = \"auto\"\ncapture_dir = \"{d}\"\ncapture_dirs = [\"{d}\"]\n",
            d = a.path().display()
        );
        Config::from_toml_str_lenient(&text).expect_err("both forms rejected even leniently");
    }

    #[test]
    fn setup_needs_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let dirs_line = format!("capture_dirs = [\"{}\"]", dir.path().display());
        let parse = |body: &str| {
            Config::from_toml_str_lenient(&format!("data_dir = \"/d\"\nmode = \"auto\"\n{body}\n"))
                .unwrap()
        };

        // Nothing configured → both needs.
        let both = parse("capture_dirs = []");
        assert_eq!(both.setup_needs(false), vec![SetupNeed::CaptureDirs, SetupNeed::Targets]);

        // Dirs ok, no send target → just the target need.
        let dirs_ok = parse(&dirs_line);
        assert_eq!(dirs_ok.setup_needs(false), vec![SetupNeed::Targets]);

        // Account + a target but NOT signed in → still needs a target (a target
        // is unresolvable without a stored token).
        let acct = parse(&format!("{dirs_line}\ntargets = [\"Studio\"]\n[account]"));
        assert_eq!(acct.setup_needs(false), vec![SetupNeed::Targets]);
        // Signed in AND a target → ready.
        assert_eq!(acct.setup_needs(true), vec![]);

        // Signed in but NO target → still needs a target (send-only: nowhere to send).
        let acct_no_target = parse(&format!("{dirs_line}\n[account]"));
        assert_eq!(acct_no_target.setup_needs(true), vec![SetupNeed::Targets]);

        // A dev ticket is a self-contained send target (no account/token needed).
        let ticket = parse(&format!("{dirs_line}\npairing_ticket = \"t\""));
        assert_eq!(ticket.setup_needs(false), vec![]);
    }

    #[test]
    fn default_template_parses_lenient_and_substitutes_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("perseus.toml");
        assert!(ensure_config_exists(&path).unwrap(), "first call creates");
        assert!(!ensure_config_exists(&path).unwrap(), "second call is a no-op");
        let cfg = Config::load_lenient(&path).expect("template must be lenient-valid");
        assert!(cfg.capture_dirs_resolved().is_empty());
        assert!(cfg.account.is_some(), "template ships an [account] table for web sign-in");
        assert_eq!(cfg.data_dir, platform_data_dir());
    }

    #[test]
    fn resolve_config_path_precedence() {
        let cwd = tempfile::tempdir().unwrap();
        // explicit flag always wins
        assert_eq!(
            resolve_config_path_in(cwd.path(), Some(PathBuf::from("/x/p.toml"))),
            PathBuf::from("/x/p.toml")
        );
        // no cwd file → platform path
        assert_eq!(resolve_config_path_in(cwd.path(), None), platform_config_path());
        // cwd perseus.toml wins over platform path (legacy compatibility)
        std::fs::write(cwd.path().join("perseus.toml"), "x").unwrap();
        assert_eq!(resolve_config_path_in(cwd.path(), None), cwd.path().join("perseus.toml"));
    }
}
