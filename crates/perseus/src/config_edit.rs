//! Comment-preserving write-back of the web-editable config fields.
//!
//! The web settings page (tasks 9/10) lets an operator tune retention, capture
//! dirs, targets, device name, and send mode live. Each `apply_*_edit` performs
//! the file edit: it rewrites **only** the handful of whitelisted keys via
//! [`toml_edit`], leaving every other key, comment, and layout byte untouched (a
//! trailing inline comment on a rewritten value line is the sole exception —
//! value replacement drops that key's own suffix decor), then re-parses +
//! re-validates the whole file and swaps it in atomically (`tmp` + rename).
//!
//! # Two-tier validation: field edits check syntax, not run-readiness
//!
//! Re-validation here runs at the **parse-valid** tier
//! ([`Config::from_toml_str_lenient`] → [`Config::validate_structure`]): syntax,
//! every field-level constraint, path shapes, and the internal consistency of
//! what IS present. It deliberately does NOT run the **run-ready** tier
//! ([`Config::validate`] → [`Config::validate_ready`], which additionally demands
//! at least one capture directory AND a send target). Run-readiness is a
//! run/start concern the supervisor surfaces as a [`crate::config::SetupNeed`];
//! it must never block editing an unrelated field. Before this split, editing any
//! field (e.g. the device name) on a fresh config that had no `targets` /
//! `pairing_ticket` yet was wrongly rejected with "no send target configured".
//!
//! The two live-deletion soak keys (`dry_run = false` requires
//! `i_have_verified_the_soak = true`) live in the parse-valid tier, so they are
//! still enforced here: the only deletion-bearing field a web edit can touch is
//! `dry_run`, and turning it off while the on-disk `i_have_verified_the_soak`
//! stays `false` is caught by [`Config::validate_structure`] and refused — the
//! file is left untouched (the edit happens on an in-memory copy, written only
//! after validation passes). Enabling live deletion therefore remains a
//! deliberate two-key edit an operator makes by hand, never something the web UI
//! can do.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::{Config, Mode, RetentionPolicy};

/// The subset of `[retention]` a web edit may change. Deliberately omits
/// `i_have_verified_the_soak`: the soak opt-in is never web-writable (see the
/// module docs), so live deletion cannot be enabled from the settings page.
///
/// Becomes a web request DTO in task 10 — hence the `camelCase` serde rename.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionEdit {
    pub policy: RetentionPolicy,
    pub keep_days: u32,
    pub disk_max_pct: u8,
    pub interval_secs: u64,
    pub dry_run: bool,
}

/// Rewrite only the whitelisted `[retention]` keys in `config_path`, preserving
/// all comments/layout ([`toml_edit`]), then re-parse + validate the whole file
/// and atomically replace it. Returns the freshly re-validated [`Config`] so the
/// caller can push it onto the retention watch channel.
///
/// The two live-deletion keys are intentionally not written here (module docs):
/// a `dry_run = false` edit against a file whose `i_have_verified_the_soak` is
/// still `false` fails [`Config::validate_structure`]'s two-key gate (a
/// parse-valid-tier check, so still enforced on edits). On **any** error the
/// file is left byte-identical — the edit is applied to an in-memory copy and
/// only written (via `tmp` + atomic rename) after validation succeeds, so a
/// rejected edit never leaves a partial or orphaned tmp file behind.
pub fn apply_retention_edit(config_path: &Path, edit: &RetentionEdit) -> Result<Config> {
    let original = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut doc: toml_edit::DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;

    // Ensure the `[retention]` table exists, then overwrite only the whitelisted
    // keys. `i_have_verified_the_soak` is never touched.
    let table = doc["retention"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    table["policy"] = toml_edit::value(policy_str(&edit.policy));
    table["keep_days"] = toml_edit::value(i64::from(edit.keep_days));
    table["disk_max_pct"] = toml_edit::value(i64::from(edit.disk_max_pct));
    table["interval_secs"] = toml_edit::value(edit.interval_secs as i64);
    table["dry_run"] = toml_edit::value(edit.dry_run);

    let candidate = doc.to_string();
    // Re-validate the whole edited document at the PARSE-VALID tier (see the
    // module docs): `from_toml_str_lenient` runs `validate_structure` only, so
    // every field-level constraint (incl. the soak gate + capture-dir existence)
    // still fires, but run-readiness (`validate_ready`: needs a capture dir + send
    // target) is NOT demanded — an unrelated field edit must not be blocked by a
    // config that is merely mid-setup.
    let cfg = Config::from_toml_str_lenient(&candidate).context("re-parse edited config")?;

    // Atomic replace: write the validated candidate to a sibling tmp, then rename
    // over the original. Only reached once validation has passed.
    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, &candidate)
        .with_context(|| format!("write tmp config {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("replace config {}", config_path.display()))?;

    tracing::info!(
        policy = policy_str(&edit.policy),
        dry_run = edit.dry_run,
        keep_days = edit.keep_days,
        disk_max_pct = edit.disk_max_pct,
        interval_secs = edit.interval_secs,
        "retention config edited via web"
    );
    Ok(cfg)
}

/// Rewrite the capture-directory selection in `config_path` to the multi-dir
/// `capture_dirs` array form, preserving all comments/layout ([`toml_edit`]),
/// then re-parse + validate the whole file and atomically replace it. Returns
/// the freshly re-validated [`Config`] so the caller can adopt it into the live
/// web state (the running watchers keep their spawn-time directories — this edit
/// is restart-to-apply, which is what makes the web page's `restartPending`
/// honest).
///
/// Two specifics versus [`apply_retention_edit`]: the whitelisted write is the
/// `capture_dirs` array AND the legacy singular `capture_dir` key is **removed**
/// — [`Config::validate_structure`] treats both forms present as a
/// misconfiguration, so an edit that left the singular key behind would be
/// self-rejecting. An **empty** list is refused up front (Perseus must watch at
/// least one directory), before the file is read or written — this edit keeps its
/// own capture-required check even though the shared re-validate no longer runs
/// the run-ready tier.
///
/// On **any** error the file is left byte-identical: the empty-list guard errors
/// before touching disk, and every later step edits an in-memory copy that is
/// only written (via `tmp` + atomic rename) after [`Config::validate_structure`]
/// passes — so a directory that does not exist on the box (a parse-valid-tier
/// existence check) leaves no partial or orphaned tmp file behind.
pub fn apply_capture_dirs_edit(config_path: &Path, dirs: &[String]) -> Result<Config> {
    // Refuse an empty selection before reading anything — a rejected edit must
    // leave the file untouched, and this is the cheapest place to enforce it.
    if dirs.is_empty() {
        bail!("at least one capture directory is required");
    }

    let original = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut doc: toml_edit::DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;

    // Write the multi-dir array form and drop the legacy singular key. Both
    // forms present is a validation error, so removing `capture_dir` is not
    // optional — it is what keeps the re-validate step below from rejecting our
    // own edit.
    let array: toml_edit::Array = dirs.iter().map(|d| d.as_str()).collect();
    doc["capture_dirs"] = toml_edit::value(array);
    doc.remove("capture_dir");

    let candidate = doc.to_string();
    // Re-validate the whole edited document at the PARSE-VALID tier (see the
    // module docs): `from_toml_str_lenient` runs `validate_structure` only, so
    // every field-level constraint (incl. the soak gate + capture-dir existence)
    // still fires, but run-readiness (`validate_ready`: needs a capture dir + send
    // target) is NOT demanded — an unrelated field edit must not be blocked by a
    // config that is merely mid-setup.
    let cfg = Config::from_toml_str_lenient(&candidate).context("re-parse edited config")?;

    // Atomic replace: write the validated candidate to a sibling tmp, then rename
    // over the original. Only reached once validation has passed.
    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, &candidate)
        .with_context(|| format!("write tmp config {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("replace config {}", config_path.display()))?;

    tracing::info!(count = dirs.len(), "capture dirs edited via web");
    Ok(cfg)
}

/// Rewrite the `targets` send list in `config_path` to the array form (the
/// account devices this node sends captures to, by name or id), preserving all
/// comments/layout ([`toml_edit`]), then re-parse + validate the whole file and
/// atomically replace it. Returns the freshly re-validated [`Config`] so the
/// caller can adopt it into the live web state. Like [`apply_capture_dirs_edit`],
/// this is **restart-to-apply**: the running engines are bound to their peers at
/// spawn, so a targets change is picked up by the supervisor's engine relaunch
/// (which is the window the web page's `restartPending` reports).
///
/// An **empty** `targets` list is accepted: it is **parse-valid** (structurally
/// sound). "At least one send target" is a run-ready demand ([`Config::validate`]
/// / [`crate::config::SetupNeed`]), never an edit-path gate — so clearing every
/// target on an account-only config (no ticket) SUCCEEDS and persists the empty
/// list. A running engine keeps its in-memory targets until restart, so saving an
/// empty list does not tear out a live send route; the supervisor surfaces the
/// missing target as a setup need on the next reload. Edit-on-copy,
/// write-after-validate: any error still leaves the file byte-identical with no
/// orphan tmp.
pub fn apply_targets_edit(config_path: &Path, targets: &[String]) -> Result<Config> {
    let original = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut doc: toml_edit::DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;

    // Write the array form at the document root. `targets` is a top-level key
    // (not under `[account]`), matching the config contract.
    let array: toml_edit::Array = targets.iter().map(|t| t.as_str()).collect();
    doc["targets"] = toml_edit::value(array);

    let candidate = doc.to_string();
    // Re-validate the whole edited document at the PARSE-VALID tier (see the
    // module docs): `from_toml_str_lenient` runs `validate_structure` only, so
    // every field-level constraint (incl. the soak gate + capture-dir existence)
    // still fires, but run-readiness (`validate_ready`: needs a capture dir + send
    // target) is NOT demanded — an unrelated field edit must not be blocked by a
    // config that is merely mid-setup.
    let cfg = Config::from_toml_str_lenient(&candidate).context("re-parse edited config")?;

    // Atomic replace: write the validated candidate to a sibling tmp, then rename
    // over the original. Only reached once validation has passed.
    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, &candidate)
        .with_context(|| format!("write tmp config {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("replace config {}", config_path.display()))?;

    tracing::info!(count = targets.len(), "targets edited via web");
    Ok(cfg)
}

/// Rewrite this node's `device_name` (its friendly name in the account device
/// list) in `config_path`, preserving all comments/layout ([`toml_edit`]), then
/// re-parse + validate the whole file and atomically replace it. Returns the
/// freshly re-validated [`Config`].
///
/// A blank name **removes** the `device_name` key entirely so registration falls
/// back to the machine-hostname default
/// ([`athenaeum_core::account::default_device_name`]) — clearing the field in the
/// UI means "use the hostname", never "register as the empty string". A non-blank
/// name is written trimmed. Applied live (no engine restart needed — the name
/// only affects hub registration); the web route additionally best-effort renames
/// the live hub device. Edit-on-copy, write-after-validate: a rejected edit
/// leaves the file byte-identical with no orphan tmp.
pub fn apply_device_name_edit(config_path: &Path, name: &str) -> Result<Config> {
    let original = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut doc: toml_edit::DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;

    let trimmed = name.trim();
    if trimmed.is_empty() {
        // Clearing the field reverts to the hostname default rather than
        // registering the device as an empty string.
        doc.remove("device_name");
    } else {
        doc["device_name"] = toml_edit::value(trimmed);
    }

    let candidate = doc.to_string();
    // Re-validate the whole edited document at the PARSE-VALID tier (see the
    // module docs): `from_toml_str_lenient` runs `validate_structure` only, so
    // every field-level constraint (incl. the soak gate + capture-dir existence)
    // still fires, but run-readiness (`validate_ready`: needs a capture dir + send
    // target) is NOT demanded — an unrelated field edit must not be blocked by a
    // config that is merely mid-setup.
    let cfg = Config::from_toml_str_lenient(&candidate).context("re-parse edited config")?;

    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, &candidate)
        .with_context(|| format!("write tmp config {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("replace config {}", config_path.display()))?;

    tracing::info!(device_name = trimmed, "device name edited via web");
    Ok(cfg)
}

/// Rewrite the top-level send `mode` + `auto_quiet_secs` keys in `config_path`
/// — and, when the caller supplies them, the [`Mode::Scheduled`] knobs
/// `schedule_times` + `schedule_catchup` — preserving all comments/layout
/// ([`toml_edit`]), then re-parse + validate the whole file and atomically
/// replace it. Returns the freshly re-validated [`Config`] so the caller can
/// publish the new [`crate::config::SendCfg`] onto the batcher's live `watch`
/// channel (Task 4) — this edit applies live, no engine restart.
///
/// **Mode and times are written in ONE edit** (0.5.1 T14). That is not a
/// convenience: `mode = "scheduled"` with an empty `schedule_times` is a
/// validation error, so a two-step "write the mode, then write the times" would
/// have to either refuse a legitimate switch-with-times or leave the file in a
/// state the validator rejects. One document, one validate, one atomic write —
/// so a `scheduled` edit that brings its own times succeeds, and one that brings
/// none is refused as a whole with the validator's own actionable message.
///
/// `schedule_times = None` / `schedule_catchup = None` mean **leave that key
/// exactly as it is** — a page (or a stale browser tab) that only knows about
/// mode + quiet window must never silently erase an operator's schedule.
/// `Some(&[])` is an explicit "clear the times", which the validator then
/// refuses if the mode is `scheduled`.
///
/// Supplied times are written **canonically** (zero-padded `HH:MM`, sorted,
/// deduped) when every entry parses; if any entry does not, the list is written
/// verbatim so the shared validator — not this function — produces the rejection
/// message naming the offending entry.
///
/// Edit-on-copy, write-after-validate: on **any** error the file is left
/// byte-identical, with no partial or orphaned tmp file.
pub fn apply_send_mode_edit(
    config_path: &Path,
    mode: Mode,
    auto_quiet_secs: u64,
    schedule_times: Option<&[String]>,
    schedule_catchup: Option<bool>,
) -> Result<Config> {
    let original = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut doc: toml_edit::DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;

    doc["mode"] = toml_edit::value(mode_str(mode));
    doc["auto_quiet_secs"] = toml_edit::value(auto_quiet_secs as i64);
    if let Some(times) = schedule_times {
        let written = canonical_schedule_times(times);
        let array: toml_edit::Array = written.iter().map(|t| t.as_str()).collect();
        doc["schedule_times"] = toml_edit::value(array);
    }
    if let Some(catchup) = schedule_catchup {
        doc["schedule_catchup"] = toml_edit::value(catchup);
    }

    let candidate = doc.to_string();
    // Re-validate the whole edited document at the PARSE-VALID tier (see the
    // module docs): `from_toml_str_lenient` runs `validate_structure` only, so
    // every field-level constraint (incl. the soak gate + capture-dir existence)
    // still fires, but run-readiness (`validate_ready`: needs a capture dir + send
    // target) is NOT demanded — an unrelated field edit must not be blocked by a
    // config that is merely mid-setup.
    let cfg = Config::from_toml_str_lenient(&candidate).context("re-parse edited config")?;

    // Atomic replace: write the validated candidate to a sibling tmp, then rename
    // over the original. Only reached once validation has passed.
    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, &candidate)
        .with_context(|| format!("write tmp config {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("replace config {}", config_path.display()))?;

    tracing::info!(
        mode = mode_str(mode),
        auto_quiet_secs,
        schedule_points = schedule_times.map(<[String]>::len),
        schedule_catchup,
        "send mode edited via web"
    );
    Ok(cfg)
}

/// The `HH:MM` strings [`apply_send_mode_edit`] writes for a supplied schedule:
/// canonical (zero-padded, sorted, deduped) when **every** entry parses, and the
/// caller's list verbatim otherwise.
///
/// The verbatim fallback is deliberate. Normalising a partly-broken list would
/// mean this function inventing its own error for the bad entry, competing with
/// — and drifting from — the one [`Config::validate_structure`] already emits.
/// Writing it through unchanged keeps a single source of that message, and the
/// candidate is still only a validated in-memory document at this point, so the
/// bad list never reaches disk.
fn canonical_schedule_times(times: &[String]) -> Vec<String> {
    match crate::schedule::parse_points(times) {
        Ok(points) => points
            .iter()
            .map(|(h, m)| format!("{h:02}:{m:02}"))
            .collect(),
        Err(_) => times.to_vec(),
    }
}

/// Rewrite the top-level `max_upload_mbps` key in `config_path`, preserving all
/// comments/layout ([`toml_edit`]), then re-parse + validate the whole file and
/// atomically replace it. Returns the freshly re-validated [`Config`] so the
/// caller can apply the new rate to the running node
/// ([`SharedIrohNode::set_upload_limit`](athenaeum_core::sharing::iroh::node::SharedIrohNode::set_upload_limit))
/// — this edit applies live, no engine restart.
///
/// `mbps` is in decimal MB/s; `0` means unlimited and is written explicitly (the
/// key is never removed, so the knob stays visible in the file). One root key is
/// touched. Edit-on-copy, write-after-validate: on **any** error the file is left
/// byte-identical, with no partial or orphaned tmp file.
pub fn apply_upload_limit_edit(config_path: &Path, mbps: u32) -> Result<Config> {
    let original = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut doc: toml_edit::DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;

    doc["max_upload_mbps"] = toml_edit::value(i64::from(mbps));

    let candidate = doc.to_string();
    // Re-validate the whole edited document at the PARSE-VALID tier (see the
    // module docs): `from_toml_str_lenient` runs `validate_structure` only, so
    // every field-level constraint (incl. the soak gate + capture-dir existence)
    // still fires, but run-readiness (`validate_ready`: needs a capture dir + send
    // target) is NOT demanded — an unrelated field edit must not be blocked by a
    // config that is merely mid-setup.
    let cfg = Config::from_toml_str_lenient(&candidate).context("re-parse edited config")?;

    // Atomic replace: write the validated candidate to a sibling tmp, then rename
    // over the original. Only reached once validation has passed.
    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, &candidate)
        .with_context(|| format!("write tmp config {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("replace config {}", config_path.display()))?;

    tracing::info!(max_upload_mbps = mbps, "upload limit edited via web");
    Ok(cfg)
}

/// The snake_case TOML string for a [`Mode`] variant. Kept in lock-step with the
/// enum's `#[serde(rename_all = "snake_case")]` so a round-trip through the file
/// re-parses to the same variant. `pub(crate)` so the web status page renders the
/// mode with the same canonical string.
pub(crate) fn mode_str(m: Mode) -> &'static str {
    match m {
        Mode::Auto => "auto",
        Mode::Manual => "manual",
        Mode::Scheduled => "scheduled",
    }
}

/// The snake_case TOML string for a [`RetentionPolicy`] variant. Kept in lock-step
/// with the enum's `#[serde(rename_all = "snake_case")]` so a round-trip through
/// the file re-parses to the same variant. `pub(crate)` so the web status page
/// ([`crate::web`]) renders the policy with the same canonical string.
pub(crate) fn policy_str(p: &RetentionPolicy) -> &'static str {
    match p {
        RetentionPolicy::KeepEverything => "keep_everything",
        RetentionPolicy::OnConfirm => "on_confirm",
        RetentionPolicy::KeepDays => "keep_days",
        RetentionPolicy::DiskPct => "disk_pct",
    }
}

#[cfg(test)]
mod tests {
    use super::*; // brings in `RetentionPolicy` too (via the module's own import)

    /// A comment-carrying config with the two live-deletion soak keys present.
    /// `/tmp` exists on every unix test host, so `validate()`'s capture-dir
    /// existence check passes; `pairing_ticket` satisfies the pairing-route gate.
    fn with_comments() -> String {
        r#"
# my precious comment
capture_dir = "/tmp"
data_dir = "/tmp"
pairing_ticket = "ticket-abc"
mode = "auto"

[retention]
policy = "keep_everything"   # inline comment
dry_run = true
i_have_verified_the_soak = false
"#
        .to_string()
    }

    /// A web edit rewrites only the whitelisted `[retention]` keys, preserving
    /// every comment and never touching the two soak keys.
    #[test]
    fn retention_edit_preserves_comments_and_soak_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("perseus.toml");
        std::fs::write(&p, with_comments()).unwrap();

        let edit = RetentionEdit {
            policy: RetentionPolicy::KeepDays,
            keep_days: 14,
            disk_max_pct: 90,
            interval_secs: 1800,
            dry_run: true,
        };
        let cfg = apply_retention_edit(&p, &edit).unwrap();
        assert_eq!(cfg.retention.keep_days, 14);
        assert_eq!(cfg.retention.policy, RetentionPolicy::KeepDays);
        assert_eq!(cfg.retention.interval_secs, 1800);

        let text = std::fs::read_to_string(&p).unwrap();
        // Comments/layout on lines the editor does not touch are preserved.
        assert!(text.contains("# my precious comment"), "top comment preserved");
        assert!(
            text.contains("i_have_verified_the_soak = false"),
            "soak key untouched"
        );
        assert!(text.contains("keep_days = 14"));
        // The soak key is never written by the editor, so it is present exactly
        // once (the operator's original line), never duplicated.
        assert_eq!(
            text.matches("i_have_verified_the_soak").count(),
            1,
            "soak key must not be duplicated"
        );
    }

    // ── Task 3 (S1.5.1): capture-dirs editor ─────────────────────────────────

    /// A capture-dirs edit writes the `capture_dirs` array, removes the legacy
    /// singular `capture_dir` key (both-forms is a validation error), and leaves
    /// every other comment/key untouched.
    #[test]
    fn capture_dirs_edit_writes_array_and_removes_singular() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("perseus.toml");
        // Comments live *elsewhere* than the removed `capture_dir` line — an
        // inline comment on `data_dir` and a standalone comment on `[retention]`
        // — so they must survive. (A comment attached directly to the removed
        // key legitimately goes with it; see the module contract.)
        let original = "\
capture_dir = \"/tmp\"
data_dir = \"/tmp\"  # keep this data dir
pairing_ticket = \"ticket-abc\"
mode = \"auto\"

# retention settings below
[retention]
policy = \"keep_everything\"
dry_run = true
i_have_verified_the_soak = false
";
        std::fs::write(&p, original).unwrap();

        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let dirs = vec![
            a.path().display().to_string(),
            b.path().display().to_string(),
        ];
        let cfg = apply_capture_dirs_edit(&p, &dirs).unwrap();
        // The resolved list is the new array, in order; the singular field is gone.
        assert_eq!(
            cfg.capture_dirs_resolved(),
            vec![a.path().to_path_buf(), b.path().to_path_buf()]
        );
        assert!(cfg.capture_dir.is_none(), "the singular capture_dir is cleared");

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# keep this data dir"), "unrelated inline comment preserved");
        assert!(text.contains("# retention settings below"), "unrelated comment preserved");
        assert!(
            text.contains("i_have_verified_the_soak = false"),
            "unrelated retention key preserved"
        );
        assert!(text.contains("capture_dirs"), "the array key is written");
        // The singular `capture_dir = …` line is removed (note the trailing space
        // before `=`, which the plural `capture_dirs =` never matches).
        assert!(
            !text.contains("capture_dir ="),
            "the legacy singular key must be removed: {text}"
        );
    }

    /// A capture-dirs edit naming a directory that does not exist on disk is
    /// rejected by the whole-config re-validate step, and the file is left
    /// byte-identical (edit-on-copy, write-after-validate) with no orphan tmp.
    #[test]
    fn capture_dirs_edit_nonexistent_dir_rejected_and_file_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("perseus.toml");
        let original = with_comments();
        std::fs::write(&p, &original).unwrap();

        let missing = dir.path().join("does-not-exist");
        let dirs = vec![missing.display().to_string()];
        let err = apply_capture_dirs_edit(&p, &dirs).expect_err("a missing dir must be rejected");
        assert!(
            format!("{err:#}").contains("does not exist"),
            "error should say the directory does not exist: {err:#}"
        );

        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(after, original, "a rejected edit leaves the file byte-identical");
        assert!(
            !p.with_extension("toml.tmp").exists(),
            "no orphan tmp file after a rejected edit"
        );
    }

    /// An empty capture-dirs list is refused before the file is ever read or
    /// written — Perseus must always watch at least one directory.
    #[test]
    fn capture_dirs_edit_empty_list_rejected_and_file_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("perseus.toml");
        let original = with_comments();
        std::fs::write(&p, &original).unwrap();

        let err = apply_capture_dirs_edit(&p, &[]).expect_err("an empty list must be rejected");
        assert!(
            format!("{err:#}").contains("at least one capture directory"),
            "error should demand at least one capture directory: {err:#}"
        );

        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(after, original, "an empty-list reject touches nothing on disk");
    }

    // ── Task 7 (Sync 2C): targets + device-name editors ──────────────────────

    /// A minimal, ready account-based config over `dir` (used as the capture dir
    /// so `validate()`'s existence check passes) with one pre-existing target, so
    /// the config is strictly valid before any edit.
    fn write_min_config(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("perseus.toml");
        let text = format!(
            "# top comment\ncapture_dirs = [\"{d}\"]\ndata_dir = \"{d}\"\nmode = \"auto\"\ntargets = [\"studio-mac\"]\ndevice_name = \"old-name\"\n[account]\nemail = \"me@example.com\"\n[retention]\npolicy = \"keep_everything\"\ndry_run = true\n",
            d = dir.display()
        );
        std::fs::write(&p, text).unwrap();
        p
    }

    /// The brief's RED test: a targets edit writes the new list, re-parses, and
    /// persists to disk so a fresh load returns it.
    #[test]
    fn apply_targets_edit_writes_and_reparses() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let cfg = apply_targets_edit(&path, &["studio-mac".into(), "nas-01".into()]).unwrap();
        assert_eq!(cfg.targets, vec!["studio-mac".to_string(), "nas-01".to_string()]);
        // Re-load from disk to prove the write-back persisted + re-parses.
        let reloaded = Config::load_lenient(&path).unwrap();
        assert_eq!(reloaded.targets.len(), 2);
        assert_eq!(reloaded.targets, vec!["studio-mac".to_string(), "nas-01".to_string()]);
    }

    /// A targets edit preserves unrelated comments/layout (comment-preserving
    /// write-back), touching only the `targets` array.
    #[test]
    fn apply_targets_edit_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        apply_targets_edit(&path, &["a".into(), "b".into(), "c".into()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# top comment"), "top comment preserved: {text}");
        assert!(text.contains("device_name = \"old-name\""), "unrelated key preserved");
        assert!(text.contains("\"c\""), "the new array is written");
    }

    /// Field edits validate at the **parse-valid** tier, not run-readiness: an
    /// account-only config whose `targets` are cleared is structurally sound (the
    /// account route just has nothing to send to *yet*), so the edit SUCCEEDS and
    /// persists an empty list. "At least one send target" is a run/start demand
    /// (a `SetupNeed` the supervisor surfaces), never an edit-path gate — a
    /// running engine keeps its in-memory targets until restart, so no live send
    /// route is torn out by saving the file. (Before the 2026-07-15 fix this was
    /// wrongly rejected with "no send target".)
    #[test]
    fn apply_targets_edit_empty_on_account_is_parse_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let cfg = apply_targets_edit(&path, &[]).expect("empty targets is parse-valid");
        assert!(cfg.targets.is_empty(), "targets cleared to empty");
        // The write-back persisted the empty list and still re-parses (lenient).
        let reloaded = Config::load_lenient(&path).unwrap();
        assert!(reloaded.targets.is_empty(), "empty targets persisted to disk");
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "no orphan tmp file after a successful edit"
        );
    }

    /// The brief's RED test (Phase 2 Task 1): a send-mode edit writes
    /// `Mode::Manual` + `auto_quiet_secs`, re-parses, and persists to disk so a
    /// fresh load returns both.
    #[test]
    fn apply_send_mode_edit_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let cfg = apply_send_mode_edit(&path, Mode::Manual, 30, None, None).unwrap();
        assert_eq!(cfg.mode, Mode::Manual);
        assert_eq!(cfg.auto_quiet_secs, 30);
        let reloaded = Config::load_lenient(&path).unwrap();
        assert_eq!(reloaded.mode, Mode::Manual);
        assert_eq!(reloaded.auto_quiet_secs, 30);
    }

    /// A send-mode edit preserves unrelated comments/layout, touching only the
    /// `mode` and `auto_quiet_secs` keys.
    #[test]
    fn apply_send_mode_edit_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        apply_send_mode_edit(&path, Mode::Manual, 45, None, None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# top comment"), "top comment preserved: {text}");
        assert!(text.contains("device_name = \"old-name\""), "unrelated key preserved");
        assert!(text.contains("mode = \"manual\""), "mode is rewritten");
        assert!(text.contains("auto_quiet_secs = 45"), "quiet window is written");
    }

    /// W1 T1.6 (the brief's RED test): an upload-limit edit writes the single
    /// root key, re-parses, persists, and leaves every unrelated comment and key
    /// byte-intact — including the two soak keys it must never touch.
    #[test]
    fn upload_limit_edit_preserves_comments_and_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("perseus.toml");
        std::fs::write(&p, with_comments()).unwrap();

        let cfg = apply_upload_limit_edit(&p, 8).unwrap();
        assert_eq!(cfg.max_upload_mbps, 8);

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("max_upload_mbps = 8"),
            "the cap is written: {text}"
        );
        assert!(
            text.contains("# my precious comment"),
            "top comment preserved"
        );
        assert!(
            text.contains("# inline comment"),
            "inline comment preserved"
        );
        assert!(
            text.contains("pairing_ticket = \"ticket-abc\""),
            "unrelated key preserved"
        );
        assert!(
            text.contains("i_have_verified_the_soak = false"),
            "soak key untouched"
        );
        // The write-back persisted and re-parses from disk.
        let reloaded = Config::load_lenient(&p).unwrap();
        assert_eq!(reloaded.max_upload_mbps, 8);
        assert!(
            !p.with_extension("toml.tmp").exists(),
            "no orphan tmp file after a successful edit"
        );
        assert!(
            !text.contains("schedule_times"),
            "an edit that passes no schedule must not invent the key: {text}"
        );
    }

    /// 0.5.1 T14 (the brief's RED test): mode + times land in ONE edit, so a
    /// `scheduled` switch that brings its own times validates as a single
    /// document — never as a transient `scheduled`-with-no-times file that the
    /// validator would refuse. The written times are canonical: zero-padded,
    /// sorted, deduped.
    #[test]
    fn apply_send_mode_edit_writes_scheduled_mode_and_times_in_one_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let times: Vec<String> = vec!["14:30".into(), "6:00".into(), "06:00".into()];
        let cfg =
            apply_send_mode_edit(&path, Mode::Scheduled, 30, Some(&times), Some(false)).unwrap();
        assert_eq!(cfg.mode, Mode::Scheduled);
        assert_eq!(cfg.send_cfg().schedule_times, vec![(6, 0), (14, 30)]);
        assert!(!cfg.schedule_catchup);

        let reloaded = Config::load_lenient(&path).unwrap();
        assert_eq!(reloaded.mode, Mode::Scheduled);
        assert_eq!(
            reloaded.schedule_times,
            vec!["06:00".to_string(), "14:30".to_string()],
            "written canonical: padded, sorted, deduped"
        );
        assert!(!reloaded.schedule_catchup);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# top comment"), "comments preserved: {text}");
    }

    /// `scheduled` with no times is refused by the shared validator, and the
    /// refusal leaves the file **byte-identical** — the edit is applied to an
    /// in-memory copy and only written after validation passes.
    #[test]
    fn apply_send_mode_edit_scheduled_without_times_leaves_file_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let before = std::fs::read_to_string(&path).unwrap();

        let err = apply_send_mode_edit(&path, Mode::Scheduled, 30, Some(&[]), None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("at least one send time"),
            "the validator's actionable message reaches the caller: {msg}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "a rejected edit leaves the file byte-identical"
        );
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "no orphan tmp after a rejected edit"
        );
    }

    /// An unparseable time is written verbatim so the SHARED validator (not this
    /// function) produces the message — and the rejection still leaves the file
    /// byte-identical.
    #[test]
    fn apply_send_mode_edit_rejects_a_malformed_time_without_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let before = std::fs::read_to_string(&path).unwrap();

        let times: Vec<String> = vec!["06:00".into(), "6h30".into()];
        let err = apply_send_mode_edit(&path, Mode::Scheduled, 30, Some(&times), None).unwrap_err();
        assert!(
            format!("{err:#}").contains("6h30"),
            "the offending entry is named: {err:#}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    /// `None` means "leave this key alone": a plain auto/manual edit from a page
    /// that never knew about the scheduler must not erase an operator's times.
    #[test]
    fn apply_send_mode_edit_none_schedule_preserves_existing_times() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let times: Vec<String> = vec!["06:00".into()];
        apply_send_mode_edit(&path, Mode::Scheduled, 30, Some(&times), Some(false)).unwrap();

        // A schedule-blind edit: mode + quiet only.
        let cfg = apply_send_mode_edit(&path, Mode::Manual, 45, None, None).unwrap();
        assert_eq!(cfg.mode, Mode::Manual);
        assert_eq!(
            cfg.schedule_times,
            vec!["06:00".to_string()],
            "the times survive a schedule-blind edit"
        );
        assert!(!cfg.schedule_catchup, "so does the catch-up flag");
    }

    /// Clearing the cap back to `0` (unlimited) is a normal edit, not a removal —
    /// the key stays in the file with an explicit `0` so the operator can see the
    /// knob is there.
    #[test]
    fn upload_limit_edit_zero_means_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        apply_upload_limit_edit(&path, 12).unwrap();
        let cfg = apply_upload_limit_edit(&path, 0).unwrap();
        assert_eq!(cfg.max_upload_mbps, 0);
        assert_eq!(cfg.upload_limit_bytes_per_sec(), 0);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("max_upload_mbps = 0"),
            "0 is written explicitly: {text}"
        );
    }

    /// A device-name edit writes the trimmed name, re-parses, and persists.
    #[test]
    fn apply_device_name_edit_writes_and_reparses() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let cfg = apply_device_name_edit(&path, "  Observatory Pi  ").unwrap();
        assert_eq!(cfg.device_name.as_deref(), Some("Observatory Pi"), "name is trimmed");
        let reloaded = Config::load_lenient(&path).unwrap();
        assert_eq!(reloaded.device_name.as_deref(), Some("Observatory Pi"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# top comment"), "unrelated comment preserved");
    }

    /// A minimal account-only config with NO targets and NO pairing_ticket:
    /// parse-valid (structurally sound) but not run-ready. `dir` is the capture
    /// dir so the existence check passes. This is the fresh-setup shape the owner
    /// hit (hub_url set, not yet logged in, no targets yet).
    fn write_account_only_no_targets_config(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("perseus.toml");
        let text = format!(
            "# top comment\ncapture_dir = \"{d}\"\ndata_dir = \"{d}\"\nmode = \"auto\"\n[account]\nhub_url = \"https://test-hub.artfrom.space\"\n[retention]\npolicy = \"keep_everything\"\ndry_run = true\n",
            d = dir.display()
        );
        std::fs::write(&p, text).unwrap();
        p
    }

    /// The bug (2026-07-15): editing an unrelated field (the device name) must
    /// NOT be blocked by a missing send target. With no `targets` and no
    /// `pairing_ticket`, the edit re-validates at the parse-valid tier and
    /// succeeds — run-readiness ("at least one send target") is a run/start
    /// concern, not a file-edit gate.
    #[test]
    fn apply_device_name_edit_succeeds_without_targets_or_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_account_only_no_targets_config(dir.path());
        let cfg = apply_device_name_edit(&path, "Test Instance").expect(
            "editing the device name must not require a send target (parse-valid tier)",
        );
        assert_eq!(cfg.device_name.as_deref(), Some("Test Instance"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("device_name = \"Test Instance\""), "written to disk: {text}");
        assert!(text.contains("# top comment"), "unrelated comment preserved");
    }

    /// Clearing the device name removes the key entirely so registration falls
    /// back to the hostname default (never the empty string).
    #[test]
    fn apply_device_name_edit_empty_clears_to_hostname_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let cfg = apply_device_name_edit(&path, "   ").unwrap();
        assert!(cfg.device_name.is_none(), "a blank name clears the override");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("device_name"), "the device_name key is removed: {text}");
    }

    /// A web edit can never enable live deletion: `dry_run = false` while the
    /// file's `i_have_verified_the_soak` stays `false` is rejected by the
    /// re-validate step, and the file on disk is left byte-identical.
    #[test]
    fn retention_edit_cannot_enable_live_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("perseus.toml");
        let original = with_comments();
        std::fs::write(&p, &original).unwrap();

        let edit = RetentionEdit {
            policy: RetentionPolicy::KeepDays,
            keep_days: 7,
            disk_max_pct: 80,
            interval_secs: 600,
            dry_run: false, // the web UI cannot write the soak key, so this must fail
        };
        let err = apply_retention_edit(&p, &edit).expect_err("live deletion must be rejected");
        assert!(
            format!("{err:#}").contains("i_have_verified_the_soak"),
            "error must name the soak opt-in flag: {err:#}"
        );

        // Edit-on-copy, write-after-validate: a rejected edit leaves the file
        // exactly as it was — byte-for-byte.
        let after = std::fs::read_to_string(&p).unwrap();
        assert_eq!(after, original, "a rejected edit must leave the file untouched");
        assert!(
            !p.with_extension("toml.tmp").exists(),
            "no orphan tmp file after a rejected edit"
        );
    }
}
