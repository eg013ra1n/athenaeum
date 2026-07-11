//! Comment-preserving write-back of the `[retention]` table.
//!
//! The web settings page (tasks 9/10) lets an operator tune retention live. This
//! module performs the file edit: it rewrites **only** the handful of whitelisted
//! `[retention]` keys via [`toml_edit`], leaving every other key, comment, and
//! layout byte untouched (a trailing inline comment on one of the five rewritten
//! value lines is the sole exception — value replacement drops that key's own
//! suffix decor), then re-parses + validates the whole file and swaps it in
//! atomically (`tmp` + rename). The two live-deletion soak keys
//! (`dry_run = false` requires `i_have_verified_the_soak = true`) are deliberately
//! **not** writable here: the only field this can touch that bears on deletion is
//! `dry_run`, and turning it off while the on-disk `i_have_verified_the_soak`
//! stays `false` is caught by the re-validate step and refused — the file is left
//! untouched (the edit happens on an in-memory copy, written only after
//! validation passes). Enabling live deletion therefore remains a deliberate
//! two-key edit an operator makes by hand, never something the web UI can do.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::{Config, RetentionPolicy};

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
/// still `false` fails [`Config::validate`]'s two-key gate. On **any** error the
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

    // Re-parse + validate the ENTIRE edited document before it ever hits disk.
    // `from_toml_str` both parses and runs `validate()` (the two-key soak gate),
    // so an edit that would enable live deletion is rejected here, with the file
    // still untouched.
    let candidate = doc.to_string();
    let cfg = Config::from_toml_str(&candidate).context("re-parse edited config")?;
    cfg.validate().context("edited config failed validation")?;

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
/// — [`Config::validate`] treats both forms present as a misconfiguration, so an
/// edit that left the singular key behind would be self-rejecting. An **empty**
/// list is refused up front (Perseus must watch at least one directory), before
/// the file is read or written.
///
/// On **any** error the file is left byte-identical: the empty-list guard errors
/// before touching disk, and every later step edits an in-memory copy that is
/// only written (via `tmp` + atomic rename) after [`Config::validate`] passes —
/// so a directory that does not exist on the box (validation's existence check)
/// leaves no partial or orphaned tmp file behind.
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

    // Re-parse + validate the ENTIRE edited document before it ever hits disk.
    // `from_toml_str` both parses and runs `validate()` (the exactly-one-form
    // guard AND the per-directory existence check — correct here, since this
    // edit runs on the observatory machine), so a bad selection is rejected with
    // the file still untouched.
    let candidate = doc.to_string();
    let cfg = Config::from_toml_str(&candidate).context("re-parse edited config")?;
    cfg.validate().context("edited config failed validation")?;

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
/// Unlike capture dirs, an **empty** list is NOT refused up front: a config with
/// a dev `pairing_ticket` is a valid send route with zero `targets`. Emptiness is
/// left to the whole-config re-validate step — an account-only config (no ticket)
/// with zero targets fails [`Config::validate`]'s "no send target" check and the
/// edit is rejected with the file left byte-identical (edit-on-copy,
/// write-after-validate), so a rejected edit never leaves a partial or orphaned
/// tmp file behind.
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

    // Re-parse + validate the ENTIRE edited document before it ever hits disk, so
    // a selection that leaves no usable send route is rejected with the file
    // still untouched.
    let candidate = doc.to_string();
    let cfg = Config::from_toml_str(&candidate).context("re-parse edited config")?;
    cfg.validate().context("edited config failed validation")?;

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
    let cfg = Config::from_toml_str(&candidate).context("re-parse edited config")?;
    cfg.validate().context("edited config failed validation")?;

    let tmp = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp, &candidate)
        .with_context(|| format!("write tmp config {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("replace config {}", config_path.display()))?;

    tracing::info!(device_name = trimmed, "device name edited via web");
    Ok(cfg)
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

    /// Clearing every target on an account-only config (no dev ticket) leaves no
    /// send route, so the whole-config re-validate rejects the edit and the file
    /// is left byte-identical (no orphan tmp).
    #[test]
    fn apply_targets_edit_empty_on_account_rejected_and_file_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_min_config(dir.path());
        let original = std::fs::read_to_string(&path).unwrap();
        let err = apply_targets_edit(&path, &[]).expect_err("no send route must be rejected");
        assert!(
            format!("{err:#}").contains("send target"),
            "error should name the missing send target: {err:#}"
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, original, "a rejected edit leaves the file byte-identical");
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "no orphan tmp file after a rejected edit"
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
