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
