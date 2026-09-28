# Collab v3 contributor path — app Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A contributor can publish every light of a linked frame set: raw names and empty `FILTER` headers get a one-time mapping, an externally calibrated set is attested and seeded in place, every gate reason names its remedy with a button, and the frame set's own page shows and links its project.

**Architecture:** Core first (`athenaeum-core`): a `collab_filter_mappings` table read by a new `collab::filters::resolve_filter`, a `FilterResolution` in the gate replacing the bare `Option<String>`, a blocker derivation next to the gate, an external branch in `run_publish`'s split that skips generation and seeds the original path, and one `contributor_state` derivation shared by both pages. Four new commands on both hosts (`get_collab_filter_mapping_sheet`, `set_collab_filter_mappings`, `set_frame_set_attestation`, `get_frame_set_project_status`). Frontend last: the blocker list and the Filter mapping modal on the project page, the attestation checkbox and the Project block/column on the frame-set page, the auto-publish switch moved into the Contribute tab.

**Tech Stack:** Rust (rusqlite, tokio, `ts-rs`), Tauri 2 commands + Axum routes, React/TypeScript with Vitest + Testing Library, Tailwind design tokens.

**Spec:** `docs/superpowers/specs/2026-09-28-collab-v3-contributor-path-design.md` (Part I §3, §5; Part II §6–§9; §10–§11). The hub side is the sibling plan `2026-09-28-collab-v3-contributor-path-hub-portal-plan.md`; this plan does not need it built — the fake hub carries the `None` entry itself (Task 2). Branch `contributor-path-app` from local `main` (`3db00071`).

## Global Constraints

- Every command: logic in `athenaeum-core/src/api/…`, a 3–5-line wrapper in `crates/athenaeum-tauri/src/commands/collab.rs` (or `frame_sets.rs`) AND `crates/athenaeum-web/src/routes/<same>.rs`, `#[tracing::instrument(skip_all, err)]` (`err(Debug)` on the web), registered in `invoke_handler![]` (`tauri/src/lib.rs`) and `build_router` (`web/src/routes/mod.rs`). Both backends in the same task.
- New `ts_rs::TS` types go into the `ts_export.rs` registry; regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`; `src/types/models.ts` is generated, never hand-edited. Serde: `#[serde(rename_all = "camelCase")]`.
- Never swallow: every `Err` at a boundary logs (`tracing::error!`/`warn!`) before returning; catalog reads that fail inside the gate degrade to a reason sentence, never a silent pass (P3's fail-closed rule stays).
- Design tokens only (`bg-surface`, `text-content-muted`, `text-error`, `bg-accent`…); icons from `lucide-react`; `notify()` from `useNotifications()` for outcomes; the StrictMode-safe `api.listen` pattern from CLAUDE.md for every listener.
- Gate reason sentences, verbatim (spec §5.1): `no FILTER header — needs a filter mapping`, `filter "<raw>" needs a filter mapping`, `filter "<raw>" is mapped to "<canonical>", which is not in this project's dictionary`.
- `frames_set.calibrated_externally` / `attested_at` are added with the idempotent `column_exists` + `ALTER TABLE` pattern of `db/schema.rs`.
- The whole core suite (`cargo test -p athenaeum-core`, ALL targets, not `--lib`) and `npm test` green before the branch is merged; `cargo check -p athenaeum-core --no-default-features` also green (the release gate). One Rust compile at a time on this Mac; never beside the hub suite.
- No third-party project names in code, comments, docs or commit messages.

## Review Focus

1. An account mapping whose canonical was later removed from the project's dictionary must show reason 3, never publish under the stale name — Task 4's `mapped_to_missing_reason_names_both`.
2. An attested set whose original file is overwritten outside the app must publish a NEW version, not be reported `unchanged` — Task 6's `external_recipe_changes_with_size_or_mtime`.
3. Two attested lights with the same basename from two sessions of one set must not both announce the same `fileName` (the hub refuses the whole batch) — Task 6's `attested_duplicate_basename_is_held_back`.
4. A frame that fails BOTH calibration and filter must appear under both blockers with its one row — Task 3's `a_row_counts_under_every_blocker_it_fails`.
5. A send-only member must see and toggle "Auto-publish my frames" — Task 8's `auto_publish_switch_visible_without_receive`.

---

### Task 1: Schema and DB — `collab_filter_mappings`, frame-set attestation columns

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (after the `collab_foreign_files` table, ~line 2715; the `frames_set` ALTER block near line 1494)
- Modify: `crates/athenaeum-core/src/db/collab.rs` (append)
- Modify: `crates/athenaeum-core/src/models.rs:264-283` (`FramesSet`)
- Modify: `crates/athenaeum-core/src/db/operations.rs:2652-2693` (`get_frames_sets_by_project` SELECT + mapping)
- Test: `crates/athenaeum-core/src/db/collab.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub struct FilterMappingRow { pub account: String, pub instrume: String, pub filter_raw: String, pub canonical: String }`
  - `pub fn filter_mappings_for_account(conn, account: &str) -> Result<Vec<FilterMappingRow>>`
  - `pub fn upsert_filter_mapping(conn, account: &str, instrume: &str, filter_raw: &str, canonical: &str) -> Result<()>`
  - `pub fn delete_filter_mapping(conn, account: &str, instrume: &str, filter_raw: &str) -> Result<bool>`
  - `pub fn set_frames_set_attestation(conn, frames_set_id: i64, attested: bool) -> Result<bool>` (false when the set does not exist)
  - `pub fn frames_set_attested(conn, frames_set_id: i64) -> Result<bool>`
  - `FramesSet { …, pub calibrated_externally: bool, pub attested_at: Option<String> }`

- [ ] **Step 1: Write the failing DB tests**

Append to the `#[cfg(test)] mod tests` of `crates/athenaeum-core/src/db/collab.rs` (create the module if the file has none; the pattern is `let db = crate::db::Database::new(tempdir.path().join("c.db")).unwrap(); let conn = db.conn();`):

```rust
    #[test]
    fn filter_mappings_are_scoped_by_account_and_upserted_by_key() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::new(tmp.path().join("c.db")).unwrap();
        let conn = db.conn();
        upsert_filter_mapping(&conn, "a@x.io", "QHY268M", "Slot 0", "L").unwrap();
        upsert_filter_mapping(&conn, "a@x.io", "QHY268M", "", "None").unwrap();
        upsert_filter_mapping(&conn, "b@x.io", "QHY268M", "Slot 0", "R").unwrap();
        // Same key again: replaced, not duplicated.
        upsert_filter_mapping(&conn, "a@x.io", "QHY268M", "Slot 0", "Ha").unwrap();

        let a = filter_mappings_for_account(&conn, "a@x.io").unwrap();
        assert_eq!(a.len(), 2);
        let slot = a.iter().find(|m| m.filter_raw == "Slot 0").unwrap();
        assert_eq!(slot.canonical, "Ha");
        assert_eq!(a.iter().find(|m| m.filter_raw.is_empty()).unwrap().canonical, "None");
        assert_eq!(filter_mappings_for_account(&conn, "b@x.io").unwrap()[0].canonical, "R");
        assert!(filter_mappings_for_account(&conn, "nobody@x.io").unwrap().is_empty());

        assert!(delete_filter_mapping(&conn, "a@x.io", "QHY268M", "Slot 0").unwrap());
        assert!(!delete_filter_mapping(&conn, "a@x.io", "QHY268M", "Slot 0").unwrap());
        assert_eq!(filter_mappings_for_account(&conn, "a@x.io").unwrap().len(), 1);
    }

    #[test]
    fn frames_set_attestation_flag_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::new(tmp.path().join("c.db")).unwrap();
        let conn = db.conn();
        conn.execute("INSERT INTO frames_set (name) VALUES ('S')", []).unwrap();
        let id = conn.last_insert_rowid();
        assert!(!frames_set_attested(&conn, id).unwrap());
        assert!(set_frames_set_attestation(&conn, id, true).unwrap());
        assert!(frames_set_attested(&conn, id).unwrap());
        let at: Option<String> = conn.query_row("SELECT attested_at FROM frames_set WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert!(at.is_some());
        assert!(set_frames_set_attestation(&conn, id, false).unwrap());
        assert!(!frames_set_attested(&conn, id).unwrap());
        let at: Option<String> = conn.query_row("SELECT attested_at FROM frames_set WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert!(at.is_none());
        assert!(!set_frames_set_attestation(&conn, 9999, true).unwrap());
        // The set reader carries the flag.
        set_frames_set_attestation(&conn, id, true).unwrap();
        let (set, _) = crate::db::get_frames_sets_by_project(&conn, 1).unwrap().into_iter().find(|(s, _)| s.id == Some(id)).unwrap();
        assert!(set.calibrated_externally);
        assert!(set.attested_at.is_some());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core --lib db::collab::tests::filter_mappings db::collab::tests::frames_set_attestation`
Expected: compile errors (functions and fields missing).

- [ ] **Step 3: Schema**

In `db/schema.rs`, after the `collab_foreign_files` `CREATE TABLE`, add:

```rust
    // Contributor path (spec 2026-09-28 §3.1, F2): the publisher's filter
    // mappings, one row per (account e-mail, camera, raw FILTER). `''` is
    // a real key — a frame without a FILTER header (F1). Never cleared at
    // sign-out; the account e-mail is the scope.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS collab_filter_mappings (
            account    TEXT NOT NULL,
            instrume   TEXT NOT NULL,
            filter_raw TEXT NOT NULL,
            canonical  TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (account, instrume, filter_raw)
        )",
        [],
    )?;
```

In the `frames_set` ALTER block (next to `flat_pattern`, using the same `column_exists` helper the later blocks use):

```rust
    // Contributor path (spec 2026-09-28 §6.1, F5): "calibrated by an
    // external tool" — the user's word that the lights are already
    // calibrated single-channel frames; projects seed them in place.
    if !column_exists(conn, "frames_set", "calibrated_externally")? {
        conn.execute("ALTER TABLE frames_set ADD COLUMN calibrated_externally INTEGER NOT NULL DEFAULT 0", [])?;
    }
    if !column_exists(conn, "frames_set", "attested_at")? {
        conn.execute("ALTER TABLE frames_set ADD COLUMN attested_at TEXT", [])?;
    }
```

- [ ] **Step 4: DB accessors**

Append to `db/collab.rs`:

```rust
// ── Filter mappings (spec 2026-09-28 §3.1) ──────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct FilterMappingRow {
    pub account: String,
    pub instrume: String,
    pub filter_raw: String,
    pub canonical: String,
}

/// Every mapping of `account` (lower-cased by the caller — `api::collab`
/// lower-cases the signed-in e-mail once).
pub fn filter_mappings_for_account(conn: &Connection, account: &str) -> Result<Vec<FilterMappingRow>> {
    let mut stmt = conn.prepare(
        "SELECT account, instrume, filter_raw, canonical FROM collab_filter_mappings \
         WHERE account = ?1 ORDER BY instrume, filter_raw",
    )?;
    let rows = stmt
        .query_map(params![account], |r| {
            Ok(FilterMappingRow {
                account: r.get(0)?,
                instrume: r.get(1)?,
                filter_raw: r.get(2)?,
                canonical: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn upsert_filter_mapping(
    conn: &Connection,
    account: &str,
    instrume: &str,
    filter_raw: &str,
    canonical: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_filter_mappings (account, instrume, filter_raw, canonical, updated_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(account, instrume, filter_raw) DO UPDATE SET \
           canonical = excluded.canonical, updated_at = excluded.updated_at",
        params![account, instrume, filter_raw, canonical],
    )?;
    Ok(())
}

/// `true` when a row was removed.
pub fn delete_filter_mapping(conn: &Connection, account: &str, instrume: &str, filter_raw: &str) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM collab_filter_mappings WHERE account = ?1 AND instrume = ?2 AND filter_raw = ?3",
        params![account, instrume, filter_raw],
    )?;
    Ok(n > 0)
}

// ── Frame-set attestation (spec 2026-09-28 §6.1) ────────────────────────────

/// `false` when no such set. Clearing also clears `attested_at`.
pub fn set_frames_set_attestation(conn: &Connection, frames_set_id: i64, attested: bool) -> Result<bool> {
    let n = if attested {
        conn.execute(
            "UPDATE frames_set SET calibrated_externally = 1, attested_at = datetime('now') WHERE id = ?1",
            params![frames_set_id],
        )?
    } else {
        conn.execute(
            "UPDATE frames_set SET calibrated_externally = 0, attested_at = NULL WHERE id = ?1",
            params![frames_set_id],
        )?
    };
    Ok(n > 0)
}

pub fn frames_set_attested(conn: &Connection, frames_set_id: i64) -> Result<bool> {
    let v: Option<i64> = conn
        .query_row(
            "SELECT calibrated_externally FROM frames_set WHERE id = ?1",
            params![frames_set_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v == Some(1))
}
```

(`use rusqlite::OptionalExtension;` if not already imported.)

- [ ] **Step 5: `FramesSet` and its reader**

In `models.rs` add to `FramesSet` after `updated_at`:

```rust
    /// Spec 2026-09-28 F5: the user attested the lights as calibrated by an
    /// external tool; projects seed them in place.
    #[serde(default)]
    pub calibrated_externally: bool,
    #[serde(default)]
    pub attested_at: Option<String>,
```

In `operations.rs::get_frames_sets_by_project` append `, fs.calibrated_externally, fs.attested_at` to the SELECT list (indices 18, 19) and map `calibrated_externally: row.get::<_, i32>(18).unwrap_or(0) == 1, attested_at: row.get(19)?`. Fix every other `FramesSet { … }` literal the compiler reports (grep `FramesSet {` — tests and `db/operations.rs` — add the two fields with `false`/`None`).

- [ ] **Step 6: Run the tests**

Run: `cargo test -p athenaeum-core --lib db::collab::tests`
Expected: pass. Then `cargo check -p athenaeum-core --no-default-features` — green.

- [ ] **Step 7: Commit**

```bash
git add crates/athenaeum-core/src/db/schema.rs crates/athenaeum-core/src/db/collab.rs crates/athenaeum-core/src/models.rs crates/athenaeum-core/src/db/operations.rs
git commit -m "feat(collab): collab_filter_mappings table and frames_set attestation columns with their accessors (§3.1, §6.1)"
```

---

### Task 2: `collab::filters` — `FilterResolution`, `resolve_filter`, `propose_canonical`; the fake hub's `None`

**Files:**
- Modify: `crates/athenaeum-core/src/collab/filters.rs`
- Modify: `crates/athenaeum-core/src/collab/fake_hub.rs` (`default_dictionary()` near line 1103's neighbourhood — the fn the live tests call)

**Interfaces:**
- Produces:
  - `pub enum FilterResolution { Mapped(String), MappedToMissing(String), Matched(String), Unmapped }` with `pub fn canonical(&self) -> Option<&str>` (Some for `Mapped`/`Matched`) and `pub fn is_unresolved(&self) -> bool` (`MappedToMissing`/`Unmapped`).
  - `pub fn resolve_filter(raw: &str, instrume: &str, mappings: &[FilterMappingRow], dict: &[DictionaryEntry]) -> FilterResolution` (`FilterMappingRow` from `crate::db::collab`).
  - `pub fn propose_canonical(raw: &str, dict: &[DictionaryEntry]) -> Option<String>`.
  - `match_filter` unchanged.

- [ ] **Step 1: Write the failing tests**

Append to `filters.rs`'s test module:

```rust
    fn dict() -> Vec<DictionaryEntry> {
        serde_json::from_str(
            r#"[{"canonical":"L","aliases":["lum","luminance","clear","l"],"kind":"luminance"},
                {"canonical":"R","aliases":["red","r"],"kind":"broadband"},
                {"canonical":"Ha","aliases":["h-alpha","halpha","h_alpha","hα","ha"],"kind":"narrowband"},
                {"canonical":"OIII","aliases":["o3","oiii","o-iii"],"kind":"narrowband"},
                {"canonical":"SII","aliases":["s2","sii","s-ii"],"kind":"narrowband"},
                {"canonical":"None","aliases":["none","nofilter","no filter","no-filter","unfiltered"],"kind":"unfiltered"}]"#,
        )
        .unwrap()
    }
    fn map(instrume: &str, raw: &str, canonical: &str) -> crate::db::collab::FilterMappingRow {
        crate::db::collab::FilterMappingRow { account: "a@x.io".into(), instrume: instrume.into(), filter_raw: raw.into(), canonical: canonical.into() }
    }

    #[test]
    fn resolution_order_mapping_then_alias_then_unmapped() {
        let d = dict();
        let m = vec![map("QHY268M", "Slot 0", "Ha"), map("QHY268M", "", "None"), map("QHY268M", "L", "None"), map("ASI294", "H", "Hb")];
        assert_eq!(resolve_filter("Slot 0", "QHY268M", &m, &d), FilterResolution::Mapped("Ha".into()));
        assert_eq!(resolve_filter("", "QHY268M", &m, &d), FilterResolution::Mapped("None".into()));
        // Explicit mapping beats the alias hit.
        assert_eq!(resolve_filter("L", "QHY268M", &m, &d), FilterResolution::Mapped("None".into()));
        // Alias hit without a mapping row, dictionary spelling returned.
        assert_eq!(resolve_filter(" red ", "QHY268M", &m, &d), FilterResolution::Matched("R".into()));
        // Mapping whose canonical the dictionary lacks.
        assert_eq!(resolve_filter("H", "ASI294", &m, &d), FilterResolution::MappedToMissing("Hb".into()));
        // Another camera: the QHY mapping does not apply.
        assert_eq!(resolve_filter("Slot 0", "ASI294", &m, &d), FilterResolution::Unmapped);
        // Empty raw without a row is always Unmapped (F1), never alias-matched.
        assert_eq!(resolve_filter("", "ASI294", &m, &d), FilterResolution::Unmapped);
        assert_eq!(resolve_filter("Slot 0", "QHY268M", &m, &d).canonical(), Some("Ha"));
        assert!(resolve_filter("H", "ASI294", &m, &d).is_unresolved());
        assert!(!resolve_filter(" red ", "QHY268M", &m, &d).is_unresolved());
    }

    #[test]
    fn normaliser_proposes_only_what_the_dictionary_offers() {
        let d = dict();
        for (raw, want) in [
            ("Red", Some("R")), ("LUM", Some("L")), ("Clear", Some("L")),
            ("H", Some("Ha")), ("h-alpha", Some("Ha")), ("Ha 3nm", Some("Ha")), ("Baader Ha 3.5nm", Some("Ha")),
            ("O", Some("OIII")), ("O3", Some("OIII")), ("S", Some("SII")), ("s2", Some("SII")),
            ("Astronomik OIII-filter", Some("OIII")),
            ("Slot 0", None), ("Filter#1", None), ("1", None), ("UV/IR cut", None), ("Dualband", None),
        ] {
            assert_eq!(propose_canonical(raw, &d).as_deref(), want, "{raw:?}");
        }
        // Empty raw: the sole unfiltered entry.
        assert_eq!(propose_canonical("", &d).as_deref(), Some("None"));
        let mut two = d.clone();
        two.push(DictionaryEntry { canonical: "Open".into(), aliases: vec![], kind: "unfiltered".into() });
        assert_eq!(propose_canonical("", &two), None, "two unfiltered entries: no proposal");
        // A synonym whose target the dictionary lacks: alias fallback, then none.
        let no_ha: Vec<DictionaryEntry> = d.iter().filter(|e| e.canonical != "Ha").cloned().collect();
        assert_eq!(propose_canonical("H", &no_ha), None);
        // Green is a synonym but this dictionary has no G.
        assert_eq!(propose_canonical("Green", &d), None);
    }
```

Add `#[derive(PartialEq)]` needs: `FilterResolution` derives `Debug, Clone, PartialEq, Eq`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core --lib collab::filters`
Expected: compile errors.

- [ ] **Step 3: Implement**

Replace the module doc comment of `filters.rs` with one that says wave 2's automatic match is now step 3 of the resolution order (spec 2026-09-28 §3.2) and add:

```rust
use crate::db::collab::FilterMappingRow;

/// Spec 2026-09-28 §3.2 — how one frame's raw `FILTER` resolves for one
/// project. Only `Mapped`/`Matched` may reach `ATH_FILT` and the announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterResolution {
    /// A mapping row of the account, and the project's dictionary has it.
    Mapped(String),
    /// A mapping row exists, but this project's dictionary lacks the canonical.
    MappedToMissing(String),
    /// No row; the raw name equals a canonical or alias (P3, `match_filter`).
    Matched(String),
    /// None of the above; an empty raw name without a row is always here (F1).
    Unmapped,
}

impl FilterResolution {
    pub fn canonical(&self) -> Option<&str> {
        match self {
            FilterResolution::Mapped(c) | FilterResolution::Matched(c) => Some(c),
            FilterResolution::MappedToMissing(_) | FilterResolution::Unmapped => None,
        }
    }
    pub fn is_unresolved(&self) -> bool {
        matches!(self, FilterResolution::MappedToMissing(_) | FilterResolution::Unmapped)
    }
}

/// Explicit mapping → dictionary exact/alias → unmapped. `raw` and
/// `instrume` are trimmed here; `mappings` are the account's rows.
pub fn resolve_filter(
    raw: &str,
    instrume: &str,
    mappings: &[FilterMappingRow],
    dict: &[DictionaryEntry],
) -> FilterResolution {
    let raw = raw.trim();
    let instrume = instrume.trim();
    if let Some(m) = mappings.iter().find(|m| m.instrume == instrume && m.filter_raw == raw) {
        return if dict.iter().any(|e| e.canonical == m.canonical) {
            FilterResolution::Mapped(m.canonical.clone())
        } else {
            FilterResolution::MappedToMissing(m.canonical.clone())
        };
    }
    match match_filter(raw, dict) {
        Some(c) => FilterResolution::Matched(c),
        None => FilterResolution::Unmapped,
    }
}

const VENDOR_TOKENS: [&str; 8] = ["filter", "astronomik", "baader", "optolong", "antlia", "chroma", "zwo", "svbony"];

/// The fixed synonym table of spec §3.3 step 4: normalised key → canonical
/// NAME (matched against the dictionary case-insensitively, its spelling
/// returned). Single letters h/o/s come from the dev catalog's 4 228 frames.
const SYNONYMS: [(&str, &str); 26] = [
    ("l", "L"), ("lum", "L"), ("luminance", "L"), ("clear", "L"),
    ("r", "R"), ("red", "R"),
    ("g", "G"), ("green", "G"),
    ("b", "B"), ("blue", "B"),
    ("ha", "Ha"), ("h", "Ha"), ("h-alpha", "Ha"), ("halpha", "Ha"), ("h_alpha", "Ha"), ("hα", "Ha"), ("h-a", "Ha"),
    ("oiii", "OIII"), ("o3", "OIII"), ("o", "OIII"), ("o-iii", "OIII"), ("o_iii", "OIII"),
    ("sii", "SII"), ("s2", "SII"), ("s", "SII"), ("s-ii", "SII"),
];

/// Spec §3.3: lower-case, trim, collapse spaces; drop a trailing `<n>nm`
/// token; drop vendor tokens; look up the synonym table (target must be a
/// canonical of `dict`), else `match_filter` on the remainder, else none.
/// An empty raw proposes the dictionary's sole `unfiltered` entry. Never
/// writes anything — the modal preselects with it (F3).
pub fn propose_canonical(raw: &str, dict: &[DictionaryEntry]) -> Option<String> {
    let mut key = raw.trim().to_lowercase();
    if key.is_empty() {
        let mut unfiltered = dict.iter().filter(|e| e.kind == "unfiltered");
        return match (unfiltered.next(), unfiltered.next()) {
            (Some(only), None) => Some(only.canonical.clone()),
            _ => None,
        };
    }
    key = key.split_whitespace().collect::<Vec<_>>().join(" ");
    // Trailing bandwidth: "ha 3nm", "oiii 6.5nm", "ha3nm".
    if let Some(idx) = key.rfind("nm") {
        if idx + 2 == key.len() {
            let head = key[..idx].trim_end();
            let digits_start = head.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.').len();
            if digits_start < head.len() {
                key = head[..digits_start].trim_end().to_string();
            }
        }
    }
    let tokens: Vec<&str> = key
        .split(|c: char| c == ' ' || c == '-' || c == '_')
        .filter(|t| !t.is_empty() && !VENDOR_TOKENS.contains(t))
        .collect();
    let joined_space = tokens.join(" ");
    let joined_dash = tokens.join("-");
    let joined_under = tokens.join("_");
    let joined_none = tokens.concat();
    let lookup = |k: &str| {
        SYNONYMS
            .iter()
            .find(|(s, _)| *s == k)
            .and_then(|(_, name)| dict.iter().find(|e| e.canonical.eq_ignore_ascii_case(name)))
            .map(|e| e.canonical.clone())
    };
    [joined_space.as_str(), joined_dash.as_str(), joined_under.as_str(), joined_none.as_str()]
        .into_iter()
        .find_map(lookup)
        .or_else(|| match_filter(&joined_space, dict))
        .or_else(|| match_filter(&joined_none, dict))
}
```

`match_filter(&joined_space, dict)` is what makes "Astronomik OIII-filter" → tokens `[oiii]` → synonym `OIII`; and "UV/IR cut" → tokens `[uv/ir, cut]` → nothing. In `fake_hub.rs::default_dictionary()` append the `None` entry `DictionaryEntry { canonical: "None".into(), aliases: vec!["none".into(), "nofilter".into(), "no filter".into(), "no-filter".into(), "unfiltered".into()], kind: "unfiltered".into() }` so the app's fake mirrors the hub after migration 0026.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core --lib collab::filters collab::fake_hub`
Expected: pass (adjust the synonym pass if one table row fails — the test is the contract).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/collab/filters.rs crates/athenaeum-core/src/collab/fake_hub.rs
git commit -m "feat(collab): FilterResolution, resolve_filter and the proposal-only normaliser (§3.2–§3.3); fake hub carries None"
```

---

### Task 3: Gate — the resolution in `GateFrameInput`, three reasons, blockers

**Files:**
- Modify: `crates/athenaeum-core/src/collab/gate.rs`

**Interfaces:**
- Consumes: `FilterResolution` (Task 2).
- Produces:
  - `GateFrameInput { …, pub filter: FilterResolution }` (field `filter_canonical` removed; `filter_raw` stays).
  - `#[derive(Serialize, TS)] pub struct UnmappedFilter { pub instrume: String, pub filter_raw: String, pub frames: i64 }`
  - `#[derive(Serialize, TS)] pub struct GateBlocker { pub kind: String, pub frames: i64, pub sets: Vec<i64>, pub names: Vec<UnmappedFilter> }` — `kind` ∈ `analyze | solve | linkCalibration | buildMasters | attest | mapFilter | threshold | uuid | outsideTarget` in that order.
  - `pub struct BlockerRow<'a> { pub frame_id: i64, pub set_id: Option<i64>, pub instrume: &'a str, pub filter_raw: &'a str, pub filter_unresolved: bool, pub failures: &'a [String] }`
  - `pub fn derive_blockers(rows: &[BlockerRow<'_>]) -> Vec<GateBlocker>`
  - `pub const BLOCKER_ORDER: [&str; 9]`.

- [ ] **Step 1: Write the failing tests**

In `gate.rs` tests, replace `unmapped_filter_fails_with_its_name` with:

```rust
    #[test]
    fn filter_reasons_name_the_raw_name_and_the_missing_canonical() {
        use crate::collab::filters::FilterResolution;
        let mut i = passing_input(); // the existing helper that builds a fully passing GateFrameInput
        i.filter_raw = String::new();
        i.filter = FilterResolution::Unmapped;
        let row = evaluate_frame(&i, &target(), &[]);
        assert!(row.failures.iter().any(|f| f == "no FILTER header — needs a filter mapping"), "{:?}", row.failures);

        i.filter_raw = "Slot 0".into();
        let row = evaluate_frame(&i, &target(), &[]);
        assert!(row.failures.iter().any(|f| f == "filter \"Slot 0\" needs a filter mapping"), "{:?}", row.failures);

        i.filter_raw = "H".into();
        i.filter = FilterResolution::MappedToMissing("Hb".into());
        let row = evaluate_frame(&i, &target(), &[]);
        assert!(row.failures.iter().any(|f| f == "filter \"H\" is mapped to \"Hb\", which is not in this project's dictionary"), "{:?}", row.failures);

        i.filter = FilterResolution::Matched("Ha".into());
        assert!(evaluate_frame(&i, &target(), &[]).publishable);
    }

    #[test]
    fn a_row_counts_under_every_blocker_it_fails() {
        let f1 = vec!["3 lights have no calibration links".to_string(), "filter \"Slot 0\" needs a filter mapping".to_string()];
        let f2 = vec!["no analysis".to_string(), "unknown pixel scale".to_string(), "no coordinates".to_string()];
        let f3 = vec!["Build masters first — 1 set without a master".to_string(), "FWHM 3.40″ > 3.00″".to_string()];
        let f4 = vec!["frame has no uuid".to_string(), "outside target radius (2.1° > 1.5°)".to_string(), "no FILTER header — needs a filter mapping".to_string()];
        let rows = vec![
            BlockerRow { frame_id: 1, set_id: Some(10), instrume: "QHY268M", filter_raw: "Slot 0", filter_unresolved: true, failures: &f1 },
            BlockerRow { frame_id: 2, set_id: Some(10), instrume: "QHY268M", filter_raw: "L", filter_unresolved: false, failures: &f2 },
            BlockerRow { frame_id: 3, set_id: Some(11), instrume: "ASI294", filter_raw: "L", filter_unresolved: false, failures: &f3 },
            BlockerRow { frame_id: 4, set_id: Some(11), instrume: "ATR2600M", filter_raw: "", filter_unresolved: true, failures: &f4 },
            BlockerRow { frame_id: 5, set_id: Some(11), instrume: "ATR2600M", filter_raw: "", filter_unresolved: true, failures: &f4 },
        ];
        let b = derive_blockers(&rows);
        let kinds: Vec<&str> = b.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(kinds, ["analyze", "solve", "linkCalibration", "buildMasters", "attest", "mapFilter", "threshold", "uuid", "outsideTarget"]);
        let by = |k: &str| b.iter().find(|x| x.kind == k).unwrap();
        assert_eq!(by("analyze").frames, 1);
        assert_eq!(by("solve").frames, 1, "one frame, two solve reasons");
        assert_eq!(by("linkCalibration").frames, 1);
        assert_eq!(by("linkCalibration").sets, vec![10]);
        assert_eq!(by("buildMasters").frames, 1);
        assert_eq!(by("buildMasters").sets, vec![11]);
        assert_eq!(by("attest").frames, 2, "every calibration reason offers attest");
        assert_eq!(by("attest").sets, vec![10, 11]);
        assert_eq!(by("mapFilter").frames, 3);
        assert_eq!(by("mapFilter").names.len(), 2);
        assert_eq!(by("mapFilter").names[0].filter_raw, "", "most frames first");
        assert_eq!(by("mapFilter").names[0].frames, 2);
        assert_eq!(by("mapFilter").names[1].filter_raw, "Slot 0");
        assert_eq!(by("threshold").frames, 1);
        assert_eq!(by("uuid").frames, 2);
        assert_eq!(by("outsideTarget").frames, 2);
        assert!(derive_blockers(&[]).is_empty());
        // A passing row contributes nothing.
        let none: Vec<String> = vec![];
        assert!(derive_blockers(&[BlockerRow { frame_id: 9, set_id: Some(1), instrume: "", filter_raw: "L", filter_unresolved: false, failures: &none }]).is_empty());
    }
```

If the test module has no `passing_input()`/`target()` helpers, add them from the existing test that builds a passing `GateFrameInput` (line ~320: `filter_raw: "L"`, `filter_canonical: Some("L")` → now `filter: FilterResolution::Matched("L".into())`).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core --lib collab::gate`
Expected: compile errors.

- [ ] **Step 3: Implement**

In `gate.rs`: replace the `filter_canonical` field with `pub filter: crate::collab::filters::FilterResolution` (doc: "spec 2026-09-28 §3.2"); in `evaluate_frame` replace the filter block:

```rust
    use crate::collab::filters::FilterResolution;
    match &input.filter {
        FilterResolution::Mapped(_) | FilterResolution::Matched(_) => {}
        FilterResolution::Unmapped if input.filter_raw.trim().is_empty() => {
            failures.push("no FILTER header — needs a filter mapping".to_string());
        }
        FilterResolution::Unmapped => {
            failures.push(format!("filter \"{}\" needs a filter mapping", input.filter_raw.trim()));
        }
        FilterResolution::MappedToMissing(c) => {
            failures.push(format!(
                "filter \"{}\" is mapped to \"{c}\", which is not in this project's dictionary",
                input.filter_raw.trim()
            ));
        }
    }
```

Update the `FrameGateRow.failures` doc comment's example sentence. Add the blocker types and derivation:

```rust
/// Spec 2026-09-28 §7.1: one distinct (camera, raw name) needing a mapping.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UnmappedFilter {
    pub instrume: String,
    pub filter_raw: String,
    pub frames: i64,
}

/// One cause that blocks publishing, with the frames it holds back, the
/// sets they belong to (for the set-scoped actions) and, for `mapFilter`,
/// the raw names (§7.1 table). Kinds in `BLOCKER_ORDER` order.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GateBlocker {
    pub kind: String,
    pub frames: i64,
    pub sets: Vec<i64>,
    pub names: Vec<UnmappedFilter>,
}

pub const BLOCKER_ORDER: [&str; 9] = [
    "analyze", "solve", "linkCalibration", "buildMasters", "attest", "mapFilter", "threshold", "uuid", "outsideTarget",
];

/// What `derive_blockers` needs per gate row — the caller pairs each
/// `FrameGateRow` with its set and raw filter.
pub struct BlockerRow<'a> {
    pub frame_id: i64,
    pub set_id: Option<i64>,
    pub instrume: &'a str,
    pub filter_raw: &'a str,
    pub filter_unresolved: bool,
    pub failures: &'a [String],
}

fn is_calibration_reason(f: &str) -> bool {
    f.contains("no calibration links") || f.contains("No calibration is linked")
        || f.contains("Build masters first") || f.contains("no master") || f.contains("master file missing")
        || f.starts_with("could not verify calibration") || f == "frame set unresolved"
}
fn is_build_masters_reason(f: &str) -> bool {
    f.contains("Build masters first") || f.contains("no master") || f.contains("master file missing")
}

/// Spec §7.1 — the table, applied to every row's failure sentences. A row
/// may count under several kinds; each kind counts a frame once.
pub fn derive_blockers(rows: &[BlockerRow<'_>]) -> Vec<GateBlocker> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut frames: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
    let mut sets: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
    let mut names: BTreeMap<(String, String), BTreeSet<i64>> = BTreeMap::new();
    for r in rows {
        let mut add = |kind: &'static str| {
            frames.entry(kind).or_default().insert(r.frame_id);
            if let Some(s) = r.set_id {
                sets.entry(kind).or_default().insert(s);
            }
        };
        for f in r.failures {
            let f = f.as_str();
            if f == "no analysis" { add("analyze"); }
            else if f == "no coordinates" || f == "unknown pixel scale" { add("solve"); }
            else if is_build_masters_reason(f) { add("buildMasters"); add("attest"); }
            else if is_calibration_reason(f) { add("linkCalibration"); add("attest"); }
            else if f.contains("needs a filter mapping") || f.contains("is mapped to") { add("mapFilter"); }
            else if f == "frame has no uuid" { add("uuid"); }
            else if f.starts_with("outside target radius") { add("outsideTarget"); }
            else { add("threshold"); }
        }
        if r.filter_unresolved {
            names.entry((r.instrume.trim().to_string(), r.filter_raw.trim().to_string())).or_default().insert(r.frame_id);
        }
    }
    let mut unmapped: Vec<UnmappedFilter> = names
        .into_iter()
        .map(|((instrume, filter_raw), ids)| UnmappedFilter { instrume, filter_raw, frames: ids.len() as i64 })
        .collect();
    unmapped.sort_by(|a, b| b.frames.cmp(&a.frames).then(a.instrume.cmp(&b.instrume)).then(a.filter_raw.cmp(&b.filter_raw)));
    BLOCKER_ORDER
        .iter()
        .filter_map(|kind| {
            let ids = frames.get(kind)?;
            Some(GateBlocker {
                kind: kind.to_string(),
                frames: ids.len() as i64,
                sets: sets.get(kind).map(|s| s.iter().copied().collect()).unwrap_or_default(),
                names: if *kind == "mapFilter" { unmapped.clone() } else { Vec::new() },
            })
        })
        .collect()
}
```

`threshold` is the catch-all for rule sentences (`FWHM …`, `eccentricity …`, `stars …`, `frame appears trailed`, and any readiness sentence the calibration matcher misses is caught earlier by `is_calibration_reason`'s "could not verify" prefix — extend the substrings if a test shows a readiness sentence landing in `threshold`).

- [ ] **Step 4: Fix the compile fallout and run**

`api/collab.rs` constructs `GateFrameInput` (`filter_canonical: …`) — Task 4 rewrites that; for this task make it compile with `filter: match match_filter(&filter_raw, dictionary) { Some(c) => FilterResolution::Matched(c), None => FilterResolution::Unmapped }` and `filter_canonical: i.filter.canonical().map(str::to_string)` in `project_gate`'s `GateIdentity`.

Run: `cargo test -p athenaeum-core --lib collab::gate api::collab`
Expected: gate tests pass; the api tests still pass (the sentences they assert on are calibration ones).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/collab/gate.rs crates/athenaeum-core/src/api/collab.rs
git commit -m "feat(collab): gate reads a FilterResolution, names the three mapping reasons, derives blockers per cause (§5.1, §7.1)"
```

---

### Task 4: Gate inputs read the mappings and the attestation; `GateReport.blockers`

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs` (`FrameRow` + `frame_gate_inputs` ~lines 394-560, `frame_cal_verdict` 355, `project_gate` 741-795, `evaluate_project_gate` 699-740, `GateReport` 93-98, `GateIdentity`)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `GateBlocker`, `UnmappedFilter`)
- Test: `api/collab.rs` tests

**Interfaces:**
- Consumes: Task 1 accessors, Task 2 `resolve_filter`, Task 3 types.
- Produces:
  - `GateReport { …, pub blockers: Vec<GateBlocker> }`
  - `pub(crate) fn current_account_email(conn) -> Option<String>` (lower-cased `settings::keys::ACCOUNT_EMAIL`, `None` when signed out)
  - `project_gate` returns `Vec<(GateIdentity, FrameGateRow)>` where `GateIdentity { uuid, filter_canonical: Option<String>, set_id: Option<i64>, attested: bool }`.

- [ ] **Step 1: Write the failing api tests**

In `api/collab.rs`'s test module add (the `seed_set` helper inserts LIGHTs with `instrume 'ASI2600MM'` and `filter 'L'`; the cached dictionary is `[L]`):

```rust
    fn sign_in_as(conn: &rusqlite::Connection, email: &str) {
        crate::db::set_setting(conn, crate::settings::keys::ACCOUNT_EMAIL, email).unwrap();
    }

    #[test]
    fn gate_resolves_through_the_account_mapping_and_reports_blockers() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "A@x.io");
            let (set_id, frames) = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 3);
            // Frame 0 has no FILTER, frame 1 a slot name, frame 2 stays "L".
            conn.execute("UPDATE frames SET filter = NULL WHERE id = ?1", [frames[0]]).unwrap();
            conn.execute("UPDATE frames SET filter = 'Slot 0' WHERE id = ?1", [frames[1]]).unwrap();
            (set_id, frames)
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        let row = |id: i64| report.rows.iter().find(|r| r.frame_id == id).unwrap().clone();
        assert!(row(frames[0]).failures.contains(&"no FILTER header — needs a filter mapping".to_string()));
        assert!(row(frames[1]).failures.contains(&"filter \"Slot 0\" needs a filter mapping".to_string()));
        assert!(!row(frames[2]).failures.iter().any(|f| f.contains("filter")));
        let map_filter = report.blockers.iter().find(|b| b.kind == "mapFilter").expect("mapFilter blocker");
        assert_eq!(map_filter.frames, 2);
        assert_eq!(map_filter.names.iter().map(|n| n.filter_raw.as_str()).collect::<Vec<_>>(), ["", "Slot 0"]);
        assert_eq!(map_filter.names[0].instrume, "ASI2600MM");
        assert!(report.blockers.iter().any(|b| b.kind == "linkCalibration" && b.sets == vec![set_id]));
        assert!(report.blockers.iter().any(|b| b.kind == "attest" && b.frames == 3));

        // Mapping rows of the signed-in account resolve both; another account's do not.
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::upsert_filter_mapping(&conn, "a@x.io", "ASI2600MM", "", "L").unwrap();
            crate::db::collab::upsert_filter_mapping(&conn, "b@x.io", "ASI2600MM", "Slot 0", "L").unwrap();
        }
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert!(!row_of(&report, frames[0]).failures.iter().any(|f| f.contains("filter")));
        assert!(row_of(&report, frames[1]).failures.contains(&"filter \"Slot 0\" needs a filter mapping".to_string()));
    }

    fn row_of(report: &GateReport, id: i64) -> FrameGateRow {
        report.rows.iter().find(|r| r.frame_id == id).unwrap().clone()
    }

    #[test]
    fn mapped_to_missing_reason_names_both() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "a@x.io");
            let r = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            conn.execute("UPDATE frames SET filter = 'H' WHERE id = ?1", [r.1[0]]).unwrap();
            crate::db::collab::upsert_filter_mapping(&conn, "a@x.io", "ASI2600MM", "H", "Hb").unwrap();
            r
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        assert!(row_of(&report, frames[0]).failures.contains(
            &"filter \"H\" is mapped to \"Hb\", which is not in this project's dictionary".to_string()
        ));
        assert_eq!(report.blockers.iter().find(|b| b.kind == "mapFilter").unwrap().names[0].filter_raw, "H");
    }

    #[test]
    fn an_attested_set_passes_the_calibration_precondition() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            let r = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 1);
            crate::db::collab::set_frames_set_attestation(&conn, r.0, true).unwrap();
            r
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let report = evaluate_project_gate(&ctx, "p-1").unwrap();
        let row = row_of(&report, frames[0]);
        assert!(!row.failures.iter().any(|f| f.contains("calibration")), "{:?}", row.failures);
        assert!(!report.blockers.iter().any(|b| b.kind == "attest" || b.kind == "linkCalibration"));
    }
```

`crate::db::set_setting` exists next to `get_setting` in `db/operations.rs` (check the name; `SettingsManager` writes through it).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core --lib api::collab::tests::gate_resolves api::collab::tests::mapped_to_missing api::collab::tests::an_attested_set`
Expected: `blockers` missing / reasons absent.

- [ ] **Step 3: Implement**

In `api/collab.rs`:

1. `FrameRow` gains `instrume: Option<String>`; the SELECT in `frame_gate_inputs` becomes `SELECT id, ra, dec, objctra, objctdec, xpixsz, focallen, filter, uuid, instrume` (index 9).
2. Add near `frame_cal_verdict`:

```rust
/// The signed-in e-mail, lower-cased — the scope of `collab_filter_mappings`
/// (spec 2026-09-28 §3.1). `None` while signed out.
pub(crate) fn current_account_email(conn: &Connection) -> Option<String> {
    match crate::db::get_setting(conn, crate::settings::keys::ACCOUNT_EMAIL) {
        Ok(v) => v.map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()),
        Err(e) => {
            tracing::warn!(error = %e, "reading the account e-mail failed; no filter mappings apply");
            None
        }
    }
}
```

3. `frame_gate_inputs` gains two parameters: `mappings: &[crate::db::collab::FilterMappingRow]` and `attested_sets: &HashSet<i64>`. Inside the loop:

```rust
        let instrume = frow.and_then(|f| f.instrume.clone()).unwrap_or_default().trim().to_string();
        let filter = crate::collab::filters::resolve_filter(&filter_raw, &instrume, mappings, dictionary);
```

and the calibration verdict becomes: when `frame_set_id_by_frame.get(frame_id)` is in `attested_sets` → `cal_blocker = None` (F5: "the app checks nothing about the pixels"), else the existing walk. Push `filter` (instead of `filter_canonical`) into `GateFrameInput`.

4. `project_gate`: load `let mappings = current_account_email(conn).map(|a| crate::db::collab::filter_mappings_for_account(conn, &a)).transpose().map_err(internal)?.unwrap_or_default();` and `let attested: HashSet<i64> = set_ids.iter().copied().filter(|s| crate::db::collab::frames_set_attested(conn, *s).unwrap_or_else(|e| { tracing::warn!(set_id = s, error = %e, "attestation read failed; treated as not attested"); false })).collect();`. `GateIdentity` gains `set_id: Option<i64>` (from `frame_sets.get(&i.frame_id)`), `attested: bool`, `instrume: String`, `filter_raw: String`, `filter_unresolved: bool`; `filter_canonical: i.filter.canonical().map(str::to_string)`.

5. `evaluate_project_gate` builds the blockers from the pairs:

```rust
    let blocker_rows: Vec<crate::collab::gate::BlockerRow<'_>> = gated
        .iter()
        .map(|(id, row)| crate::collab::gate::BlockerRow {
            frame_id: row.frame_id,
            set_id: id.set_id,
            instrume: &id.instrume,
            filter_raw: &id.filter_raw,
            filter_unresolved: id.filter_unresolved,
            failures: &row.failures,
        })
        .collect();
    let blockers = crate::collab::gate::derive_blockers(&blocker_rows);
```

and `GateReport { …, blockers }` (field `pub blockers: Vec<crate::collab::gate::GateBlocker>`). Every other `GateReport { … }` literal (tests) gets `blockers: Vec::new()`.

6. `ts_export.rs`: add `crate::collab::gate::GateBlocker` and `crate::collab::gate::UnmappedFilter` next to `FrameGateRow`.

- [ ] **Step 4: Run, regenerate TS, run the contract**

Run: `cargo test -p athenaeum-core --lib api::collab collab::gate && TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract && cargo test -p athenaeum-core --test ts_contract`
Expected: green; `src/types/models.ts` gains `GateBlocker`, `UnmappedFilter`, `GateReport.blockers`, `FramesSet.calibrated_externally`/`attested_at`. Then `npx tsc --noEmit -p .` — fix the three frontend fixtures that build a `GateReport` (`ProjectDetail.test.tsx::gateFixture`: add `blockers: []`).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/api/collab.rs crates/athenaeum-core/src/ts_export.rs src/types/models.ts src/pages/ProjectDetail.test.tsx
git commit -m "feat(collab): the gate reads the account's filter mappings and the set's attestation; GateReport carries blockers (§7.1)"
```

---

### Task 5: Commands — `get_collab_filter_mapping_sheet`, `set_collab_filter_mappings` (both hosts)

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab.rs` (new section after the gate; `ts_export.rs` registry)
- Modify: `crates/athenaeum-tauri/src/commands/collab.rs`, `crates/athenaeum-tauri/src/lib.rs` (`invoke_handler![]` after `evaluate_collab_gate`)
- Modify: `crates/athenaeum-web/src/routes/collab.rs`, `crates/athenaeum-web/src/routes/mod.rs` (after `/api/evaluate_collab_gate`)
- Test: `api/collab.rs` tests

**Interfaces:**
- Produces (all `#[derive(Serialize, Deserialize, TS)] #[serde(rename_all = "camelCase")]`):
  - `FilterMappingRowView { instrume, filter_raw, frames: i64, resolution: String /* mapped|mappedToMissing|matched|unmapped */, canonical: Option<String>, proposal: Option<String> }`
  - `FilterMappingSheet { project_id, dictionary: Vec<DictionaryEntry>, rows: Vec<FilterMappingRowView> }`
  - `FilterMappingEdit { instrume: String, filter_raw: String, canonical: Option<String> }`
  - `pub fn get_filter_mapping_sheet(ctx, project_id) -> Result<FilterMappingSheet, ApiError>`
  - `pub fn set_filter_mappings(ctx, project_id, edits: Vec<FilterMappingEdit>) -> Result<GateReport, ApiError>`
  - commands `get_collab_filter_mapping_sheet { projectId }`, `set_collab_filter_mappings { projectId, mappings }`.

- [ ] **Step 1: Write the failing api tests**

```rust
    #[test]
    fn mapping_sheet_lists_every_raw_name_with_its_resolution_and_proposal() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn); // dictionary [L]
            crate::db::collab::set_dictionary(&conn, "p-1", Some(2), Some(
                r#"[{"canonical":"L","aliases":["lum"],"kind":"luminance"},{"canonical":"Ha","aliases":[],"kind":"narrowband"},{"canonical":"None","aliases":["none"],"kind":"unfiltered"}]"#)).unwrap();
            sign_in_as(&conn, "a@x.io");
            let r = seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 4);
            conn.execute("UPDATE frames SET filter = NULL WHERE id IN (?1, ?2)", [r.1[0], r.1[1]]).unwrap();
            conn.execute("UPDATE frames SET filter = 'H' WHERE id = ?1", [r.1[2]]).unwrap();
            r
        };
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let sheet = get_filter_mapping_sheet(&ctx, "p-1").unwrap();
        assert_eq!(sheet.dictionary.len(), 3);
        let raws: Vec<(&str, &str)> = sheet.rows.iter().map(|r| (r.filter_raw.as_str(), r.resolution.as_str())).collect();
        assert_eq!(raws, [("", "unmapped"), ("H", "unmapped"), ("L", "matched")], "unresolved first, then by frames desc");
        assert_eq!(sheet.rows[0].frames, 2);
        assert_eq!(sheet.rows[0].proposal.as_deref(), Some("None"));
        assert_eq!(sheet.rows[1].proposal.as_deref(), Some("Ha"));
        assert_eq!(sheet.rows[2].canonical.as_deref(), Some("L"));
        assert_eq!(sheet.rows[2].proposal, None, "a resolved row proposes nothing");

        // Save: one valid, one null (no row to delete → fine), one invalid → nothing written.
        let bad = set_filter_mappings(&ctx, "p-1", vec![FilterMappingEdit { instrume: "ASI2600MM".into(), filter_raw: "H".into(), canonical: Some("Hb".into()) }]);
        assert!(matches!(bad, Err(crate::api::ApiError::Invalid(m)) if m.contains("\"Hb\" is not in the project dictionary")));
        let report = set_filter_mappings(&ctx, "p-1", vec![
            FilterMappingEdit { instrume: "ASI2600MM".into(), filter_raw: "".into(), canonical: Some("None".into()) },
            FilterMappingEdit { instrume: "ASI2600MM".into(), filter_raw: "H".into(), canonical: Some("Ha".into()) },
        ]).unwrap();
        assert!(!report.blockers.iter().any(|b| b.kind == "mapFilter"));
        let sheet = get_filter_mapping_sheet(&ctx, "p-1").unwrap();
        assert_eq!(sheet.rows.iter().find(|r| r.filter_raw == "H").unwrap().resolution, "mapped");
        // The auto-publish dirty mark was requested.
        assert!(crate::api::collab_autopublish::is_dirty_for_test("p-1"));
        // Back to automatic.
        set_filter_mappings(&ctx, "p-1", vec![FilterMappingEdit { instrume: "ASI2600MM".into(), filter_raw: "H".into(), canonical: None }]).unwrap();
        assert_eq!(get_filter_mapping_sheet(&ctx, "p-1").unwrap().rows.iter().find(|r| r.filter_raw == "H").unwrap().resolution, "unmapped");
        let _ = frames;
    }

    #[test]
    fn mapping_sheet_refuses_without_a_cached_dictionary_or_a_sign_in() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            crate::db::collab::set_dictionary(&conn, "p-1", None, None).unwrap();
        }
        assert!(matches!(get_filter_mapping_sheet(&ctx, "p-1"), Err(crate::api::ApiError::Invalid(m)) if m.contains("has not been fetched")));
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            crate::db::collab::set_dictionary(&conn, "p-1", Some(1), Some(r#"[{"canonical":"L","aliases":[],"kind":"luminance"}]"#)).unwrap();
        }
        assert!(matches!(set_filter_mappings(&ctx, "p-1", vec![FilterMappingEdit { instrume: "X".into(), filter_raw: "L".into(), canonical: Some("L".into()) }]), Err(crate::api::ApiError::SignedOut(_))));
    }
```

`collab_autopublish::is_dirty_for_test(project_id) -> bool` is a `#[cfg(test)] pub(crate)` helper to add in `collab_autopublish.rs` that reads the `DIRTY` set (`dirty().lock().unwrap().contains(project_id)`).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core --lib api::collab::tests::mapping_sheet`
Expected: compile errors.

- [ ] **Step 3: Core implementation**

Add to `api/collab.rs` after the gate section:

```rust
// ── Filter mapping sheet (spec 2026-09-28 §5.2) ─────────────────────────────

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterMappingRowView {
    pub instrume: String,
    pub filter_raw: String,
    pub frames: i64,
    /// `mapped` | `mappedToMissing` | `matched` | `unmapped`
    pub resolution: String,
    pub canonical: Option<String>,
    pub proposal: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterMappingSheet {
    pub project_id: String,
    pub dictionary: Vec<DictionaryEntry>,
    pub rows: Vec<FilterMappingRowView>,
}

#[derive(Debug, Clone, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterMappingEdit {
    pub instrume: String,
    pub filter_raw: String,
    /// `None` = delete the row ("back to automatic").
    pub canonical: Option<String>,
}

fn project_dictionary(project: &CollabProjectRow) -> Result<Vec<DictionaryEntry>, ApiError> {
    match &project.dictionary_json {
        Some(json) => serde_json::from_str(json).map_err(|e| {
            tracing::error!(project_id = %project.project_id, error = %e, "cached filter dictionary does not parse");
            ApiError::Internal(format!("cached filter dictionary does not parse: {e}"))
        }),
        None => Err(ApiError::Invalid("the project's filter dictionary has not been fetched yet".into())),
    }
}

/// Every distinct (camera, raw FILTER) among the project's candidate frames
/// with its resolution and a proposal (F3): unresolved rows first, then by
/// frame count descending, camera, raw name.
pub fn get_filter_mapping_sheet(ctx: &ServiceContext, project_id: &str) -> Result<FilterMappingSheet, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let project = crate::db::collab::get_project(&conn, project_id).map_err(internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {project_id} is not cached — refresh first")))?;
    let dictionary = project_dictionary(&project)?;
    let mappings = current_account_email(&conn)
        .map(|a| crate::db::collab::filter_mappings_for_account(&conn, &a))
        .transpose().map_err(internal)?
        .unwrap_or_default();
    let set_ids = crate::db::collab::linked_set_ids(&conn, project_id).map_err(internal)?;
    let frames = union_light_frames(&conn, &set_ids).map_err(internal)?;
    let mut counts: HashMap<(String, String), i64> = HashMap::new();
    if !frames.is_empty() {
        let ids: Vec<i64> = frames.iter().map(|(id, _)| *id).collect();
        let placeholders = vec!["?"; ids.len()].join(",");
        let mut stmt = conn.prepare(&format!("SELECT instrume, filter FROM frames WHERE id IN ({placeholders})")).map_err(|e| internal(e.into()))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))).map_err(|e| internal(e.into()))?;
        for row in rows {
            let (i, f) = row.map_err(|e| internal(e.into()))?;
            *counts.entry((i.unwrap_or_default().trim().to_string(), f.unwrap_or_default().trim().to_string())).or_default() += 1;
        }
    }
    let mut rows: Vec<FilterMappingRowView> = counts
        .into_iter()
        .map(|((instrume, filter_raw), frames)| {
            let res = crate::collab::filters::resolve_filter(&filter_raw, &instrume, &mappings, &dictionary);
            let (resolution, canonical) = match &res {
                FilterResolution::Mapped(c) => ("mapped", Some(c.clone())),
                FilterResolution::MappedToMissing(c) => ("mappedToMissing", Some(c.clone())),
                FilterResolution::Matched(c) => ("matched", Some(c.clone())),
                FilterResolution::Unmapped => ("unmapped", None),
            };
            let proposal = if res.is_unresolved() { crate::collab::filters::propose_canonical(&filter_raw, &dictionary) } else { None };
            FilterMappingRowView { instrume, filter_raw, frames, resolution: resolution.into(), canonical, proposal }
        })
        .collect();
    rows.sort_by(|a, b| {
        let ua = a.resolution == "unmapped" || a.resolution == "mappedToMissing";
        let ub = b.resolution == "unmapped" || b.resolution == "mappedToMissing";
        ub.cmp(&ua).then(b.frames.cmp(&a.frames)).then(a.instrume.cmp(&b.instrume)).then(a.filter_raw.cmp(&b.filter_raw))
    });
    tracing::info!(project_id, rows = rows.len(), "filter mapping sheet built");
    Ok(FilterMappingSheet { project_id: project_id.to_string(), dictionary, rows })
}

/// Upsert/delete the account's rows in one `BEGIN IMMEDIATE`, refuse a
/// canonical outside the project's dictionary before writing anything, mark
/// the project dirty for auto-publish, return the fresh gate report.
pub fn set_filter_mappings(ctx: &ServiceContext, project_id: &str, edits: Vec<FilterMappingEdit>) -> Result<GateReport, ApiError> {
    {
        let db = db(ctx)?;
        let mut conn = db.conn();
        let project = crate::db::collab::get_project(&conn, project_id).map_err(internal)?
            .ok_or_else(|| ApiError::NotFound(format!("project {project_id} is not cached — refresh first")))?;
        let dictionary = project_dictionary(&project)?;
        let account = current_account_email(&conn).ok_or_else(|| {
            tracing::warn!(project_id, "filter mappings refused: signed out");
            ApiError::SignedOut("Sign in to map filters.".into())
        })?;
        for e in &edits {
            if let Some(c) = &e.canonical {
                if !dictionary.iter().any(|d| &d.canonical == c) {
                    tracing::warn!(project_id, canonical = %c, "filter mapping refused: canonical not in the dictionary");
                    return Err(ApiError::Invalid(format!("{c:?} is not in the project dictionary")));
                }
            }
        }
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| internal(e.into()))?;
        for e in &edits {
            let instrume = e.instrume.trim();
            let raw = e.filter_raw.trim();
            match &e.canonical {
                Some(c) => crate::db::collab::upsert_filter_mapping(&tx, &account, instrume, raw, c).map_err(internal)?,
                None => { crate::db::collab::delete_filter_mapping(&tx, &account, instrume, raw).map_err(internal)?; }
            }
        }
        tx.commit().map_err(|e| internal(e.into()))?;
        tracing::info!(project_id, count = edits.len(), "filter mappings saved");
    }
    crate::api::collab_autopublish::request_auto_publish(Some(project_id));
    evaluate_project_gate(ctx, project_id)
}
```

If `db.conn()` returns a guard that cannot start a transaction, use `conn.execute_batch("BEGIN IMMEDIATE")` / `COMMIT` with a rollback on error, as other `BEGIN IMMEDIATE` sites in `api/collab_live/` do — copy their pattern. Register `FilterMappingRowView`, `FilterMappingSheet`, `FilterMappingEdit` in `ts_export.rs`.

- [ ] **Step 4: Host wrappers**

`crates/athenaeum-tauri/src/commands/collab.rs`:

```rust
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn get_collab_filter_mapping_sheet(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<FilterMappingSheet, String> {
    api::get_filter_mapping_sheet(&state.ctx, &project_id).map_err(|e| e.to_string())
}

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn set_collab_filter_mappings(
    state: State<'_, AppState>,
    project_id: String,
    mappings: Vec<FilterMappingEdit>,
) -> Result<GateReport, String> {
    api::set_filter_mappings(&state.ctx, &project_id, mappings).map_err(|e| e.to_string())
}
```

`crates/athenaeum-web/src/routes/collab.rs` (add `SetFilterMappingsArgs { project_id: String, mappings: Vec<api::FilterMappingEdit> }` with `#[serde(rename_all = "camelCase")]`):

```rust
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collab_filter_mapping_sheet(
    State(state): State<WebAppState>,
    Json(args): Json<ProjectIdArgs>,
) -> Result<Json<api::FilterMappingSheet>, (axum::http::StatusCode, String)> {
    api::get_filter_mapping_sheet(&state.ctx, &args.project_id).map(Json).map_err(api_err)
}

#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collab_filter_mappings(
    State(state): State<WebAppState>,
    Json(args): Json<SetFilterMappingsArgs>,
) -> Result<Json<api::GateReport>, (axum::http::StatusCode, String)> {
    api::set_filter_mappings(&state.ctx, &args.project_id, args.mappings).map(Json).map_err(api_err)
}
```

Register: `commands::get_collab_filter_mapping_sheet, commands::set_collab_filter_mappings,` in `lib.rs`; `.route("/api/get_collab_filter_mapping_sheet", post(collab::get_collab_filter_mapping_sheet)).route("/api/set_collab_filter_mappings", post(collab::set_collab_filter_mappings))` in `routes/mod.rs`. Re-export the types from `commands/mod.rs`/`api` the way `GateReport` is.

- [ ] **Step 5: Run everything that compiles hosts**

Run: `cargo test -p athenaeum-core --lib api::collab && TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract && cargo check -p athenaeum-tauri && cargo check -p athenaeum-web`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/api/collab.rs crates/athenaeum-core/src/api/collab_autopublish.rs crates/athenaeum-core/src/ts_export.rs crates/athenaeum-tauri/src/commands/collab.rs crates/athenaeum-tauri/src/lib.rs crates/athenaeum-web/src/routes/collab.rs crates/athenaeum-web/src/routes/mod.rs src/types/models.ts
git commit -m "feat(collab): filter mapping sheet and save commands on both hosts (§5.2)"
```

---

### Task 6: Attestation command and the external publish branch; `meta.calibration`

**Files:**
- Modify: `crates/athenaeum-core/src/api/frame_sets.rs` (new `set_frame_set_attestation`)
- Modify: `crates/athenaeum-tauri/src/commands/frame_sets.rs`, `lib.rs`; `crates/athenaeum-web/src/routes/frame_sets.rs`, `routes/mod.rs`
- Modify: `crates/athenaeum-core/src/api/collab.rs` (`run_publish` split ~3011-3100, pass 3 ~3147-3208, `run_publish_generation` 2133-2340, the seed loop 3265-3290, `PlannedFrame`, `WrittenFrame`)
- Modify: `crates/athenaeum-core/src/collab/frame_meta.rs` (`FrameMeta.meta` gets `calibration`)
- Test: `api/collab.rs` publish tests (the `fixture(n)` harness at ~6393 drives `run_publish` against the fake hub)

**Interfaces:**
- Produces:
  - `pub fn set_frame_set_attestation(ctx, frames_set_id: i64, attested: bool) -> Result<(), ApiError>` + command `set_frame_set_attestation { framesSetId, attested }` on both hosts.
  - `PlannedFrame { …, pub external: bool }`; `WrittenFrame` unchanged in shape (an external frame's `staged == target`, `identical: false`, `staged_blake3: None`).
  - `pub(crate) fn external_recipe(size: i64, modified_at: &str) -> String` = `format!("external:{size}:{modified_at}")`.
  - `pub(crate) fn calibration_meta(spec: Option<&crate::export::GenerationSpec>, external: bool) -> serde_json::Value` → `{"dark":bool,"flat":bool,"bias":bool,"external":bool}`.

- [ ] **Step 1: Write the failing tests**

Attestation command (in `api/frame_sets.rs` tests, or `api/collab.rs` tests if the ctx helper lives there):

```rust
    #[test]
    fn attestation_command_flips_the_flag_marks_dirty_and_refuses_a_zipped_set() {
        let (_tmp, ctx) = test_ctx();
        let set_id = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            conn.execute("INSERT INTO frames_set (name) VALUES ('S')", []).unwrap();
            conn.last_insert_rowid()
        };
        crate::api::frame_sets::set_frame_set_attestation(&ctx, set_id, true).unwrap();
        {
            let conn = crate::api::db(&ctx).unwrap().conn();
            assert!(crate::db::collab::frames_set_attested(&conn, set_id).unwrap());
            conn.execute("UPDATE frames_set SET is_archived = 1, archived_at = '2026-09-01T00:00:00Z' WHERE id = ?1", [set_id]).unwrap();
        }
        assert!(matches!(crate::api::frame_sets::set_frame_set_attestation(&ctx, set_id, false), Err(crate::api::ApiError::Invalid(_))));
        assert!(matches!(crate::api::frame_sets::set_frame_set_attestation(&ctx, 4242, true), Err(crate::api::ApiError::NotFound(_))));
    }
```

Publish tests, in the `fixture`-based publish module of `api/collab.rs` (read `fixture(n)` first: it builds a project, a linked set with calibrated-ready lights and a fake hub; mirror how an existing test asserts on `hub.announced(...)` / `PublishResult`):

```rust
    #[tokio::test]
    async fn attested_lights_are_seeded_in_place_without_generation() {
        let fx = fixture(2).await;
        {
            let conn = fx.conn();
            crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            // Drop the calibration links: attestation alone must carry the gate.
            conn.execute("DELETE FROM calibration_set_to_frames", []).unwrap();
        }
        let res = publish_collab_frames(&fx.ctx, &fx.project_id, None).await.unwrap();
        assert_eq!(res.announced, 2, "{:?}", res.held_back);
        for f in fx.hub.announced_frames(&fx.project_id) {
            assert_eq!(f.meta["calibration"], serde_json::json!({"dark": false, "flat": false, "bias": false, "external": true}));
            let row = crate::db::collab_frames::get(&fx.conn(), &fx.project_id, &f.frame_uuid).unwrap().unwrap();
            let landed = std::path::PathBuf::from(row.landed_path.clone().unwrap());
            assert!(fx.light_paths.contains(&landed), "landed path is the original: {}", landed.display());
            assert!(row.recipe_hash.as_deref().unwrap().starts_with("external:"));
            assert_eq!(f.file_name, landed.file_name().unwrap().to_string_lossy());
            assert_eq!(f.xxh3, crate::package::xxh3_full_file(&landed).unwrap());
        }
        assert!(std::fs::read_dir(fx.own_dir()).map(|d| d.count()).unwrap_or(0) == 0, "the publisher folder stays empty for attested frames");
    }

    #[tokio::test]
    async fn external_recipe_changes_with_size_or_mtime() {
        let fx = fixture(1).await;
        { let conn = fx.conn(); crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap(); }
        assert_eq!(publish_collab_frames(&fx.ctx, &fx.project_id, None).await.unwrap().announced, 1);
        assert_eq!(publish_collab_frames(&fx.ctx, &fx.project_id, None).await.unwrap().unchanged, 1);
        // The scanner's in-place re-parse bumps files.size/modified_at after an external overwrite.
        {
            let conn = fx.conn();
            conn.execute("UPDATE files SET size = size + 1, modified_at = '2027-01-01T00:00:00Z' WHERE id = (SELECT file_id FROM frames WHERE id = ?1)", [fx.frame_ids[0]]).unwrap();
        }
        std::fs::write(&fx.light_paths[0], b"new bytes that differ").unwrap();
        let res = publish_collab_frames(&fx.ctx, &fx.project_id, None).await.unwrap();
        assert_eq!(res.updated, 1, "{:?}", res.held_back);
    }

    #[tokio::test]
    async fn attested_duplicate_basename_is_held_back() {
        let fx = fixture(2).await;
        {
            let conn = fx.conn();
            crate::db::collab::set_frames_set_attestation(&conn, fx.set_id, true).unwrap();
            // Two originals with the same basename in different folders.
            let dup = fx.light_paths[0].parent().unwrap().join("other").join(fx.light_paths[0].file_name().unwrap());
            std::fs::create_dir_all(dup.parent().unwrap()).unwrap();
            std::fs::copy(&fx.light_paths[1], &dup).unwrap();
            conn.execute("UPDATE files SET path = ?1, filename = ?2 WHERE id = (SELECT file_id FROM frames WHERE id = ?3)",
                rusqlite::params![dup.to_string_lossy(), fx.light_paths[0].file_name().unwrap().to_string_lossy(), fx.frame_ids[1]]).unwrap();
        }
        let res = publish_collab_frames(&fx.ctx, &fx.project_id, None).await.unwrap();
        assert_eq!(res.announced, 1);
        assert_eq!(res.held_back.len(), 1);
        assert!(res.held_back[0].reasons[0].contains("already published by you — rename the file"), "{:?}", res.held_back);
    }

    #[tokio::test]
    async fn generated_lights_report_their_masters_in_meta() {
        let fx = fixture(1).await;
        publish_collab_frames(&fx.ctx, &fx.project_id, None).await.unwrap();
        let f = &fx.hub.announced_frames(&fx.project_id)[0];
        assert_eq!(f.meta["calibration"]["external"], false);
        assert_eq!(f.meta["calibration"]["dark"], true, "the fixture links a master dark");
    }
```

Adapt field names (`fx.conn()`, `fx.set_id`, `fx.light_paths`, `fx.hub.announced_frames`, `fx.own_dir()`) to what `PubFx` actually exposes; add small accessors to the fixture if it lacks them (the fixture is test code).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core --lib api::collab::tests::attested api::collab::tests::external_recipe api::collab::tests::generated_lights api::frame_sets`
Expected: compile errors / `announced == 0` with "no calibration links".

- [ ] **Step 3: Attestation command**

In `api/frame_sets.rs`:

```rust
/// Spec 2026-09-28 §6.1 (F5): the user's word that the set's lights are
/// calibrated by an external tool. Refuses a zipped set (its files are not
/// on disk to seed). Marks the set dirty for auto-publish.
pub fn set_frame_set_attestation(ctx: &ServiceContext, frames_set_id: i64, attested: bool) -> Result<(), ApiError> {
    let db = crate::api::db(ctx)?;
    let conn = db.conn();
    let zipped: Option<Option<String>> = conn
        .query_row("SELECT archived_at FROM frames_set WHERE id = ?1", [frames_set_id], |r| r.get(0))
        .optional()
        .map_err(|e| ApiError::Internal(format!("read frames_set: {e}")))?;
    match zipped {
        None => {
            tracing::warn!(frames_set_id, "attestation refused: no such set");
            return Err(ApiError::NotFound(format!("frame set {frames_set_id} not found")));
        }
        Some(Some(_)) => {
            tracing::warn!(frames_set_id, "attestation refused: the set is zipped");
            return Err(ApiError::Invalid("This set is zipped — unarchive it before attesting its calibration.".into()));
        }
        Some(None) => {}
    }
    crate::db::collab::set_frames_set_attestation(&conn, frames_set_id, attested)
        .map_err(|e| ApiError::Internal(format!("write attestation: {e:#}")))?;
    tracing::info!(frames_set_id, attested, "frame set attestation set");
    #[cfg(all(feature = "render", feature = "solver"))]
    crate::api::collab_autopublish::request_auto_publish_for_sets(&[frames_set_id]);
    Ok(())
}
```

Wrappers: Tauri `set_frame_set_attestation(frames_set_id: i64, attested: bool, state) -> Result<(), String>` in `commands/frame_sets.rs`; web `SetAttestationArgs { frames_set_id, attested }` in `routes/frame_sets.rs`; register both.

- [ ] **Step 4: The external branch in `run_publish`**

In the split (pass 1), before `resolve_frame_inputs`, read the set's attestation once per set (a `HashMap<i64, bool>` filled from `frame_set_ids`/`crate::db::collab::frames_set_attested`) and branch:

```rust
            let external = attested_by_set.get(&cand.frame_id_set).copied().unwrap_or(false);
            let (recipe, osc, light_path) = if external {
                let (path, size, modified): (String, i64, String) = conn.query_row(
                    "SELECT fi.path, fi.size, fi.modified_at FROM frames f JOIN files fi ON fi.id = f.file_id WHERE f.id = ?1",
                    [fid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                ).map_err(|e| { tracing::error!(project_id, frame_id = fid, error = %e, "publish: reading the attested light failed"); internal(e.into()) })?;
                let osc = conn.query_row("SELECT bayerpat FROM frames WHERE id = ?1", [fid], |r| r.get::<_, Option<String>>(0)).ok().flatten().is_some();
                (external_recipe(size, &modified), osc, Some(std::path::PathBuf::from(path)))
            } else {
                // existing resolve_frame_inputs + recipe_hash_of_inputs, unchanged
                …
                (recipe, resolved.cfa_geometry.is_some(), None)
            };
```

(`cand.frame_id_set`: add the set id to `PublishCandidate` from `GateIdentity.set_id` — Task 4 put it there.) Carry `light_path: Option<PathBuf>` through `split`/`targeted` tuples. In pass 3, an external candidate skips `calibrated_output_filename`/`new_frame_target` and uses `target = light_path`, `file_name = target.file_name()`; the basename is checked against `taken_names` and `claimed`:

```rust
                if taken_names.contains(&file_name) || !claimed.insert(target.clone()) {
                    tracing::warn!(project_id, frame_id = fid, file_name = %file_name, "publish: attested basename already published by this publisher");
                    held_back.push(held(fid, &cand.filename, format!("a frame named {file_name:?} is already published by you — rename the file")));
                    continue;
                }
```

(`taken_names` must also grow with each external name accepted in this run: insert into a local `HashSet` after the check.) `PlannedFrame` gains `external: bool`.

In `run_publish_generation`, partition `job.plans` into `external` and `generated`. External plans take no compute permit and no spec: for each, `xxh3 = xxh3_full_file(&plan.target)`, `byte_size = std::fs::metadata(&plan.target)?.len()`, `meta.meta["calibration"] = calibration_meta(None, true)`; a hashing/stat failure is `held(…, format!("cannot read the attested light: {e:#}"))`; push `WrittenFrame { staged: target.clone(), target, recipe: plan.recipe, identical: false, staged_blake3: None, … }`. For generated plans, after `stamp_publish_cards`, set `meta.meta["calibration"] = calibration_meta(Some(&spec), false)` (only when `plan.meta` is `Some` — updates carry `meta: None` and the hub keeps the prior meta). Take the permit only when `generated` is non-empty.

```rust
pub(crate) fn external_recipe(size: i64, modified_at: &str) -> String {
    format!("external:{size}:{modified_at}")
}

/// Spec 2026-09-28 F8 — informational: which masters the generation used,
/// or `external: true` for an attested light.
pub(crate) fn calibration_meta(spec: Option<&crate::export::GenerationSpec>, external: bool) -> serde_json::Value {
    let has = |kind: &str| spec.map(|s| crate::export::spec_master_paths(s).iter().any(|p| p.to_string_lossy().to_lowercase().contains(kind))).unwrap_or(false);
    serde_json::json!({ "dark": has("dark"), "flat": has("flat"), "bias": has("bias"), "external": external })
}
```

If `GenerationSpec` exposes typed master fields (`spec.inputs.dark`, `.flat`, `.bias` — check `export::GenerationSpec`), use them instead of the path-name heuristic; the test only asserts `dark: true` for the fixture's linked dark.

In the seed loop, an external `Update` must not `rename(staged, target)` (they are the same path): guard with `if w.staged != w.target { rename… }`. `remove_temp` on an external frame is a no-op by the same guard (never delete the original — check every `remove_temp(pid, &staged)` call site inside the generation loop is on the generated path only; the external loop has none).

- [ ] **Step 5: Run the publish tests, then the whole collab module**

Run: `cargo test -p athenaeum-core --lib api::collab api::frame_sets && cargo check -p athenaeum-tauri && cargo check -p athenaeum-web`
Expected: green. The existing publish tests still pass (non-attested path untouched apart from `meta.calibration`).

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/api/frame_sets.rs crates/athenaeum-core/src/api/collab.rs crates/athenaeum-core/src/collab/frame_meta.rs crates/athenaeum-tauri/src/commands/frame_sets.rs crates/athenaeum-tauri/src/lib.rs crates/athenaeum-web/src/routes/frame_sets.rs crates/athenaeum-web/src/routes/mod.rs
git commit -m "feat(collab): frame-set attestation command; attested lights publish in place with no generation; meta.calibration on every announce (§6, F8)"
```

---

### Task 7: `contributor_state` derivation and `get_frame_set_project_status` (both hosts)

**Files:**
- Create: `crates/athenaeum-core/src/collab/contributor_state.rs` (+ `pub mod contributor_state;` in `collab/mod.rs`)
- Modify: `crates/athenaeum-core/src/api/collab.rs` (the command; `ts_export.rs` registry)
- Modify: `crates/athenaeum-core/src/api/collab_exchange.rs` (`ProjectFrameView` gains `contributor_state: Option<String>`, `contributor_reason: Option<String>` for own rows)
- Modify: hosts (`commands/collab.rs`, `routes/collab.rs`, registrations)

**Interfaces:**
- Produces:
  - `pub enum ContributorState { NotPublished, FailsGate, PendingApproval, Published, UpdatePending, Rejected, PublishedNotOnDisk, PublishedNowFailsGate }` with `pub fn key(&self) -> &'static str` (camelCase strings: `notPublished`, `failsGate`, `pendingApproval`, `published`, `updatePending`, `rejected`, `publishedNotOnDisk`, `publishedNowFailsGate`) and `pub fn short_label(&self) -> &'static str` (`—`, `fails gate`, `pending`, `published`, `update`, `rejected`, `not on disk`, `now fails`).
  - `pub struct OwnRowFacts<'a> { pub state: &'a str, pub content_version: i32, pub recipe_hash: Option<&'a str>, pub on_disk: bool, pub reject_reason: Option<&'a str> }`
  - `pub fn derive(own: Option<OwnRowFacts<'_>>, current_recipe: Option<&str>, gate_publishable: bool, gate_reason: Option<&str>) -> (ContributorState, Option<String>)` (state + reason/`vN` text).
  - `FrameSetProjectStatus { links: Vec<FrameSetProjectLink>, candidates: Vec<FrameSetProjectCandidate> }`, `FrameSetProjectLink { project_id, slug, title, publishing_here, auto_publish, counts: ContributorCounts, frames: Vec<FrameProjectState { frame_id, state, reason: Option<String> }> }`, `ContributorCounts { not_published, fails_gate, pending_approval, published, update_pending, rejected, published_not_on_disk, published_now_fails_gate }` (all `i64`), `FrameSetProjectCandidate { project_id, slug, title, distance_deg }`.
  - `pub fn get_frame_set_project_status(ctx, frames_set_id) -> Result<FrameSetProjectStatus, ApiError>`; command `get_frame_set_project_status { framesSetId }`.

- [ ] **Step 1: Write the failing unit tests**

`contributor_state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn own<'a>(state: &'a str, recipe: Option<&'a str>, on_disk: bool) -> OwnRowFacts<'a> {
        OwnRowFacts { state, content_version: 3, recipe_hash: recipe, on_disk, reject_reason: None }
    }
    #[test]
    fn every_row_of_the_table() {
        assert_eq!(derive(None, Some("r1"), true, None).0, ContributorState::NotPublished);
        let (s, r) = derive(None, Some("r1"), false, Some("no analysis"));
        assert_eq!(s, ContributorState::FailsGate); assert_eq!(r.as_deref(), Some("no analysis"));
        assert_eq!(derive(Some(own("pending", Some("r1"), true)), Some("r1"), true, None).0, ContributorState::PendingApproval);
        let (s, r) = derive(Some(own("published", Some("r1"), true)), Some("r1"), true, None);
        assert_eq!(s, ContributorState::Published); assert_eq!(r.as_deref(), Some("v3"));
        assert_eq!(derive(Some(own("published", Some("r1"), true)), Some("r2"), true, None).0, ContributorState::UpdatePending);
        assert_eq!(derive(Some(own("pending", Some("r1"), true)), Some("r2"), true, None).0, ContributorState::UpdatePending);
        let (s, r) = derive(Some(OwnRowFacts { reject_reason: Some("trailed"), ..own("rejected", Some("r1"), true) }), Some("r1"), true, None);
        assert_eq!(s, ContributorState::Rejected); assert_eq!(r.as_deref(), Some("trailed"));
        assert_eq!(derive(Some(own("published", Some("r1"), false)), Some("r1"), true, None).0, ContributorState::PublishedNotOnDisk);
        let (s, r) = derive(Some(own("published", Some("r1"), true)), Some("r1"), false, Some("FWHM 3.4″ > 3.0″"));
        assert_eq!(s, ContributorState::PublishedNowFailsGate); assert_eq!(r.as_deref(), Some("FWHM 3.4″ > 3.0″"));
        // Not on disk wins over now-fails; update pending wins over now-fails.
        assert_eq!(derive(Some(own("published", Some("r1"), false)), Some("r1"), false, Some("x")).0, ContributorState::PublishedNotOnDisk);
        assert_eq!(derive(Some(own("published", Some("r1"), true)), Some("r2"), false, Some("x")).0, ContributorState::UpdatePending);
        // A recipe the app cannot compute (cannot resolve the light) reads as update pending only if the row had one.
        assert_eq!(derive(Some(own("published", None, true)), Some("r1"), true, None).0, ContributorState::Published);
        assert_eq!(ContributorState::UpdatePending.key(), "updatePending");
        assert_eq!(ContributorState::PublishedNotOnDisk.short_label(), "not on disk");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core --lib collab::contributor_state`
Expected: compile error.

- [ ] **Step 3: Implement the derivation**

```rust
//! Spec 2026-09-28 §8.1 — the ONE derivation of a contributor's own frame
//! state (per-frame spec §3.3), used by the frame set's Project block and
//! column AND the project page's own-frames table.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContributorState {
    NotPublished, FailsGate, PendingApproval, Published, UpdatePending, Rejected, PublishedNotOnDisk, PublishedNowFailsGate,
}

impl ContributorState {
    pub fn key(&self) -> &'static str {
        match self {
            Self::NotPublished => "notPublished", Self::FailsGate => "failsGate", Self::PendingApproval => "pendingApproval",
            Self::Published => "published", Self::UpdatePending => "updatePending", Self::Rejected => "rejected",
            Self::PublishedNotOnDisk => "publishedNotOnDisk", Self::PublishedNowFailsGate => "publishedNowFailsGate",
        }
    }
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::NotPublished => "—", Self::FailsGate => "fails gate", Self::PendingApproval => "pending", Self::Published => "published",
            Self::UpdatePending => "update", Self::Rejected => "rejected", Self::PublishedNotOnDisk => "not on disk", Self::PublishedNowFailsGate => "now fails",
        }
    }
}

pub struct OwnRowFacts<'a> {
    pub state: &'a str,
    pub content_version: i32,
    pub recipe_hash: Option<&'a str>,
    pub on_disk: bool,
    pub reject_reason: Option<&'a str>,
}

/// `current_recipe` is what a publish would compute now (`None` when the
/// light cannot be resolved); `gate_reason` is the row's first failure.
pub fn derive(
    own: Option<OwnRowFacts<'_>>,
    current_recipe: Option<&str>,
    gate_publishable: bool,
    gate_reason: Option<&str>,
) -> (ContributorState, Option<String>) {
    let Some(row) = own else {
        return if gate_publishable {
            (ContributorState::NotPublished, None)
        } else {
            (ContributorState::FailsGate, gate_reason.map(str::to_string))
        };
    };
    if row.state == "rejected" {
        return (ContributorState::Rejected, row.reject_reason.map(str::to_string));
    }
    let recipe_moved = matches!((row.recipe_hash, current_recipe), (Some(a), Some(b)) if a != b);
    if row.state == "published" && !row.on_disk {
        return (ContributorState::PublishedNotOnDisk, None);
    }
    if recipe_moved {
        return (ContributorState::UpdatePending, None);
    }
    if row.state == "pending" {
        return (ContributorState::PendingApproval, None);
    }
    if !gate_publishable {
        return (ContributorState::PublishedNowFailsGate, gate_reason.map(str::to_string));
    }
    (ContributorState::Published, Some(format!("v{}", row.content_version)))
}
```

- [ ] **Step 4: The command**

In `api/collab.rs` (types with `Serialize, TS, camelCase`):

```rust
/// Spec §8.2 — what the frame set's page shows about projects.
pub fn get_frame_set_project_status(ctx: &ServiceContext, frames_set_id: i64) -> Result<FrameSetProjectStatus, ApiError> {
    use crate::collab::contributor_state::{derive, OwnRowFacts};
    let db = db(ctx)?;
    let conn = db.conn();
    let signed_in = current_account_email(&conn).is_some();
    let mut links = Vec::new();
    let mut candidates = Vec::new();
    if !signed_in {
        return Ok(FrameSetProjectStatus { links, candidates });
    }
    let set_frames: Vec<(i64, String)> = union_light_frames(&conn, &[frames_set_id]).map_err(internal)?;
    let set_frame_ids: HashSet<i64> = set_frames.iter().map(|(id, _)| *id).collect();
    let attested = crate::db::collab::frames_set_attested(&conn, frames_set_id).map_err(internal)?;
    for p in crate::db::collab::list_projects(&conn).map_err(internal)? {
        if !crate::db::collab::is_set_linked(&conn, &p.project_id, frames_set_id).map_err(internal)? {
            continue;
        }
        let gated = project_gate(&conn, &p)?;
        let own = crate::db::collab_frames::own_by_source_frame(&conn, &p.project_id).map_err(internal)?;
        let mut counts = ContributorCounts::default();
        let mut frames = Vec::new();
        for (id, row) in gated.iter().filter(|(_, r)| set_frame_ids.contains(&r.frame_id)) {
            let current_recipe = if attested {
                conn.query_row("SELECT fi.size, fi.modified_at FROM frames f JOIN files fi ON fi.id = f.file_id WHERE f.id = ?1", [row.frame_id], |r| Ok(external_recipe(r.get::<_, i64>(0)?, &r.get::<_, String>(1)?))).ok()
            } else {
                crate::calibration_library::light_resolve::resolve_frame_inputs(&conn, row.frame_id, publish_options().flat_norm)
                    .ok().and_then(|res| recipe_hash_of_inputs(&conn, &res).ok())
            };
            let facts = own.get(&row.frame_id).map(|o| OwnRowFacts {
                state: &o.state, content_version: o.content_version, recipe_hash: o.recipe_hash.as_deref(), on_disk: o.on_disk, reject_reason: o.accepted_reason.as_deref(),
            });
            let (state, reason) = derive(facts, current_recipe.as_deref(), row.publishable, row.failures.first().map(String::as_str));
            counts.bump(state);
            frames.push(FrameProjectState { frame_id: row.frame_id, state: state.key().to_string(), reason });
            let _ = id;
        }
        links.push(FrameSetProjectLink {
            project_id: p.project_id.clone(), slug: p.slug.clone(), title: p.title.clone(),
            publishing_here: crate::db::collab::publishing_device(&conn, &p.project_id).map_err(internal)?.map(|b| b.device_id == crate::api::account::own_device_id(ctx).unwrap_or_default()).unwrap_or(false),
            auto_publish: p.auto_publish, counts, frames,
        });
    }
    if links.is_empty() {
        if let Some((ra, dec)) = crate::db::frame_set_center_deg(&conn, frames_set_id).map_err(internal)? {
            for m in find_matching_projects(&conn, ra, dec, frames_set_id).map_err(internal)? {
                candidates.push(FrameSetProjectCandidate { project_id: m.project_id, slug: m.project_slug, title: m.project_title, distance_deg: m.distance_deg });
            }
        }
    }
    tracing::info!(frames_set_id, links = links.len(), candidates = candidates.len(), "frame set project status built");
    Ok(FrameSetProjectStatus { links, candidates })
}
```

`ContributorCounts::bump(state)` increments the matching field. `LocalFrameRow.accepted_reason` — check the actual field name for the reject reason (`ProjectFrameView.accepted_reason` comes from a row column; use that column). `crate::db::frame_set_center_deg` — reuse whatever `api/frame_sets.rs:280-300` uses to get `(ra, dec)` for the set-match hook (it parses `objctra`/`objctdec`); if it is inline there, extract it into a `pub fn` in that file and call it. `ProjectSetMatch` needs `distance_deg` — add the field if `find_matching_projects` does not carry it (it computes `d`). `ProjectFrameView` gains `contributor_state`/`contributor_reason` (`Option<String>`) filled for own rows in `list_collab_frames` with the same `derive` call (same facts, gate row looked up from `project_gate` once per call) so the project page's own table shows the same chip.

Wrappers on both hosts: `get_frame_set_project_status { framesSetId }` → `Json<api::FrameSetProjectStatus>`; register; add the five types to `ts_export.rs`.

- [ ] **Step 5: An api test**

```rust
    #[test]
    fn frame_set_project_status_counts_states_and_lists_candidates() {
        let (_tmp, ctx) = test_ctx();
        let (set_id, frames) = {
            let conn = crate::api::db(&ctx).unwrap().conn();
            cached_project(&conn);
            sign_in_as(&conn, "a@x.io");
            seed_set(&conn, "M101 Set", "14:03:12", "+54:21:00", 210.8, 54.35, 2)
        };
        let st = get_frame_set_project_status(&ctx, set_id).unwrap();
        assert!(st.links.is_empty());
        assert_eq!(st.candidates.len(), 1, "within radius, not linked");
        assert_eq!(st.candidates[0].project_id, "p-1");
        link_frame_set(&ctx, "p-1", set_id).unwrap();
        let st = get_frame_set_project_status(&ctx, set_id).unwrap();
        assert_eq!(st.links.len(), 1);
        assert!(st.candidates.is_empty());
        assert_eq!(st.links[0].counts.fails_gate, 2, "no calibration links");
        assert_eq!(st.links[0].frames.iter().filter(|f| f.state == "failsGate").count(), 2);
        assert!(st.links[0].frames[0].reason.as_deref().unwrap().contains("calibration"));
        let _ = frames;
    }
```

Run: `cargo test -p athenaeum-core --lib collab::contributor_state api::collab && TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract && cargo check -p athenaeum-tauri && cargo check -p athenaeum-web`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/collab/contributor_state.rs crates/athenaeum-core/src/collab/mod.rs crates/athenaeum-core/src/api/collab.rs crates/athenaeum-core/src/api/collab_exchange.rs crates/athenaeum-core/src/api/frame_sets.rs crates/athenaeum-core/src/ts_export.rs crates/athenaeum-tauri/src/commands/collab.rs crates/athenaeum-tauri/src/lib.rs crates/athenaeum-web/src/routes/collab.rs crates/athenaeum-web/src/routes/mod.rs src/types/models.ts
git commit -m "feat(collab): one contributor-state derivation; get_frame_set_project_status on both hosts (§8.1–§8.2)"
```

---

### Task 8: Project page — blocker list, Filter mapping modal, auto-publish switch, dead hint removed

**Files:**
- Create: `src/components/collab/GateBlockers.tsx`, `src/components/collab/FilterMappingDialog.tsx`, `src/components/collab/GateBlockers.test.tsx`, `src/components/collab/FilterMappingDialog.test.tsx`
- Modify: `src/pages/ProjectDetail.tsx` (imports; the `publishBlockedByCalibration` block ~366-383; the Contribute tab header ~484-510; the button block ~520-545; `PublicationHistory` chip), `src/pages/ProjectDetail.test.tsx`
- Modify: `src/components/collab/AutoReplicateBar.tsx` + `.test.tsx` (auto-publish removed)

**Interfaces:**
- Consumes: `GateReport.blockers`, `FilterMappingSheet`, `FilterMappingEdit`, `ProjectFrameView.contributorState` (generated in `models.ts`); commands `get_collab_filter_mapping_sheet`, `set_collab_filter_mappings`, `analyze_frame_set { frameSetId }`, `plate_solve_batch { frameIds }`, `set_project_auto_publish { projectId, enabled }`.
- Produces: `<GateBlockers gate onMapFilters onOpenCalibration(setId) onSolve(frameIds) onAnalyze(setId) />`, `<FilterMappingDialog projectId onClose onSaved(report) />`, `<AutoPublishSwitch projectId enabled onToggled />` (a small component inside `AutoReplicateBar.tsx`'s file or its own — export from `src/components/collab/AutoPublishSwitch.tsx`).

- [ ] **Step 1: Write the failing component tests**

`GateBlockers.test.tsx`:

```tsx
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import GateBlockers from './GateBlockers';
import type { GateReport } from '../../types/models';

afterEach(cleanup);

const gate: GateReport = {
  projectId: 'p', total: 10, publishable: 1, rows: [
    { frameId: 1, filename: 'a.fits', fwhmArcsec: null, eccentricity: null, starsDetected: null, trailed: null, publishable: false, failures: ['no analysis'] },
    { frameId: 2, filename: 'b.fits', fwhmArcsec: null, eccentricity: null, starsDetected: null, trailed: null, publishable: false, failures: ['no coordinates'] },
  ],
  blockers: [
    { kind: 'analyze', frames: 79, sets: [10], names: [] },
    { kind: 'solve', frames: 12, sets: [10], names: [] },
    { kind: 'linkCalibration', frames: 891, sets: [10, 11], names: [] },
    { kind: 'attest', frames: 891, sets: [10, 11], names: [] },
    { kind: 'mapFilter', frames: 903, sets: [10], names: [{ instrume: 'ATR2600M', filterRaw: '', frames: 891 }, { instrume: 'QHY268M', filterRaw: 'Slot 0', frames: 12 }] },
    { kind: 'threshold', frames: 4, sets: [10], names: [] },
    { kind: 'outsideTarget', frames: 3, sets: [10], names: [] },
  ],
};

describe('GateBlockers', () => {
  it('renders one line per cause with its count and the right button', () => {
    const onMapFilters = vi.fn(); const onOpenCalibration = vi.fn(); const onSolve = vi.fn(); const onAnalyze = vi.fn();
    render(<GateBlockers gate={gate} solveBusy={false} analyzeBusy={new Set()} onMapFilters={onMapFilters} onOpenCalibration={onOpenCalibration} onSolve={onSolve} onAnalyze={onAnalyze} />);
    expect(screen.getByText('79 frames have no analysis')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Analyze' }));
    expect(onAnalyze).toHaveBeenCalledWith(10);
    expect(screen.getByText('12 frames have no coordinates or pixel scale')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Solve 12 frames' }));
    expect(onSolve).toHaveBeenCalledWith([2]);
    expect(screen.getByText('891 frames are not calibrated')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Open calibration' }));
    // Two sets: a menu of set ids appears; picking one calls back with it.
    fireEvent.click(screen.getByRole('menuitem', { name: /Set #11/ }));
    expect(onOpenCalibration).toHaveBeenCalledWith(11);
    expect(screen.getByRole('button', { name: 'Attest as calibrated…' })).toBeInTheDocument();
    expect(screen.getByText('2 filter names need a mapping')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Map filters' }));
    expect(onMapFilters).toHaveBeenCalled();
    expect(screen.getByText('4 frames fail a threshold')).toBeInTheDocument();
    expect(screen.getByText('3 frames are outside the target')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /threshold/ })).toBeNull();
  });

  it('renders nothing without blockers', () => {
    const { container } = render(<GateBlockers gate={{ ...gate, blockers: [] }} solveBusy={false} analyzeBusy={new Set()} onMapFilters={vi.fn()} onOpenCalibration={vi.fn()} onSolve={vi.fn()} onAnalyze={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });
});
```

`FilterMappingDialog.test.tsx`:

```tsx
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import FilterMappingDialog from './FilterMappingDialog';
import { api } from '../../api';
import type { FilterMappingSheet, GateReport } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
vi.mock('../../contexts/NotificationContext', () => ({ useNotifications: () => ({ notify: vi.fn() }) }));
afterEach(cleanup);

const sheet: FilterMappingSheet = {
  projectId: 'p',
  dictionary: [
    { canonical: 'L', aliases: ['lum'], kind: 'luminance' },
    { canonical: 'Ha', aliases: [], kind: 'narrowband' },
    { canonical: 'None', aliases: ['none'], kind: 'unfiltered' },
  ],
  rows: [
    { instrume: 'ATR2600M', filterRaw: '', frames: 891, resolution: 'unmapped', canonical: null, proposal: 'None' },
    { instrume: 'QHY268M', filterRaw: 'Slot 0', frames: 53, resolution: 'unmapped', canonical: null, proposal: null },
    { instrume: 'ASI294MM Pro', filterRaw: 'H', frames: 1538, resolution: 'mappedToMissing', canonical: 'Hb', proposal: 'Ha' },
    { instrume: 'QHY268M', filterRaw: 'L', frames: 1093, resolution: 'matched', canonical: 'L', proposal: null },
  ],
};
const report: GateReport = { projectId: 'p', total: 4, publishable: 4, rows: [], blockers: [] };

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'get_collab_filter_mapping_sheet') return Promise.resolve(sheet);
    if (command === 'set_collab_filter_mappings') return Promise.resolve(report);
    return Promise.reject(new Error(`unexpected ${command}`));
  }) as typeof api.invoke);
});

describe('FilterMappingDialog', () => {
  it('lists unresolved rows first with proposals preselected, and sends only the changed rows', async () => {
    const onSaved = vi.fn();
    render(<FilterMappingDialog projectId="p" onClose={vi.fn()} onSaved={onSaved} />);
    await waitFor(() => expect(screen.getByText('(no FILTER) · ATR2600M — 891 frames')).toBeInTheDocument());
    const selects = screen.getAllByRole('combobox');
    expect(selects[0]).toHaveValue('None'); // proposal preselected
    expect(selects[1]).toHaveValue(''); // slot name: no proposal
    expect(selects[2]).toHaveValue('Ha'); // mappedToMissing: proposal, with the stale name shown
    expect(screen.getByText(/mapped to "Hb", not in this project/)).toBeInTheDocument();
    expect(selects[3]).toHaveValue('L'); // resolved row below the divider
    // A preselected proposal is the publisher's pending choice (F3): Save is
    // enabled and confirming writes it.
    const save = screen.getByRole('button', { name: 'Save' });
    expect(save).not.toBeDisabled();
    fireEvent.change(selects[1], { target: { value: 'L' } });
    fireEvent.click(save);
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(report));
    expect(api.invoke).toHaveBeenCalledWith('set_collab_filter_mappings', {
      projectId: 'p',
      mappings: [
        { instrume: 'ATR2600M', filterRaw: '', canonical: 'None' },
        { instrume: 'QHY268M', filterRaw: 'Slot 0', canonical: 'L' },
        { instrume: 'ASI294MM Pro', filterRaw: 'H', canonical: 'Ha' },
      ],
    });
  });

  it('sends null for a row set back to automatic and disables Save when nothing changed', async () => {
    render(<FilterMappingDialog projectId="p" onClose={vi.fn()} onSaved={vi.fn()} />);
    await waitFor(() => expect(screen.getAllByRole('combobox')).toHaveLength(4));
    const selects = screen.getAllByRole('combobox');
    // Undo every proposal: the two proposed rows back to "no choice".
    fireEvent.change(selects[0], { target: { value: '' } });
    fireEvent.change(selects[2], { target: { value: '' } });
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    // The matched row → automatic sends null.
    fireEvent.change(selects[3], { target: { value: '__auto__' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('set_collab_filter_mappings', { projectId: 'p', mappings: [{ instrume: 'QHY268M', filterRaw: 'L', canonical: null }] }));
  });

  it('shows the sheet refusal inline', async () => {
    vi.mocked(api.invoke).mockImplementation(() => Promise.reject(new Error("the project's filter dictionary has not been fetched yet")));
    render(<FilterMappingDialog projectId="p" onClose={vi.fn()} onSaved={vi.fn()} />);
    await waitFor(() => expect(screen.getByText(/has not been fetched yet/)).toBeInTheDocument());
  });
});
```

`ProjectDetail.test.tsx`: add `auto_publish_switch_visible_without_receive` — render with `projectCard({ dataRole: 'send' })` and assert `screen.getByRole('checkbox', { name: /Auto-publish my frames/ })` exists and `set_project_auto_publish` is invoked on click; and a test that the text `not available in this version` never appears when the gate has only calibration failures (`gateFixture` with a row failing `3 lights have no calibration links` and `publishable: 0`).

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/GateBlockers.test.tsx src/components/collab/FilterMappingDialog.test.tsx src/pages/ProjectDetail.test.tsx`
Expected: module-not-found / assertion failures.

- [ ] **Step 3: `GateBlockers.tsx`**

```tsx
import { useState } from 'react';
import { ChevronDown } from 'lucide-react';
import type { GateBlocker, GateReport } from '../../types/models';

const SOLVE_REASONS = new Set(['no coordinates', 'unknown pixel scale']);

function line(b: GateBlocker): string {
  switch (b.kind) {
    case 'analyze': return `${b.frames} frames have no analysis`;
    case 'solve': return `${b.frames} frames have no coordinates or pixel scale`;
    case 'linkCalibration':
    case 'buildMasters': return `${b.frames} frames are not calibrated`;
    case 'mapFilter': return `${b.names.length} filter name${b.names.length === 1 ? '' : 's'} need a mapping`;
    case 'threshold': return `${b.frames} frames fail a threshold`;
    case 'uuid': return `${b.frames} frames have no uuid — re-scan their folder`;
    case 'outsideTarget': return `${b.frames} frames are outside the target`;
    default: return '';
  }
}

const BTN = 'inline-flex items-center gap-1 rounded border border-border px-2 py-0.5 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50';

/** Spec 2026-09-28 §7.2 — "What blocks publishing": one line per cause with
 * one button. `attest` rides on the calibration line; `buildMasters` folds
 * into the calibration line (same navigation). */
export default function GateBlockers({
  gate, solveBusy, analyzeBusy, onMapFilters, onOpenCalibration, onSolve, onAnalyze,
}: {
  gate: GateReport;
  solveBusy: boolean;
  analyzeBusy: Set<number>;
  onMapFilters: () => void;
  onOpenCalibration: (setId: number) => void;
  onSolve: (frameIds: number[]) => void;
  onAnalyze: (setId: number) => void;
}) {
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const shown = gate.blockers.filter((b) => b.kind !== 'attest' && b.kind !== 'buildMasters');
  if (shown.length === 0) return null;
  const calSets = Array.from(new Set(gate.blockers.filter((b) => b.kind === 'linkCalibration' || b.kind === 'buildMasters').flatMap((b) => b.sets)));
  const calFrames = gate.blockers.filter((b) => b.kind === 'linkCalibration' || b.kind === 'buildMasters').reduce((n, b) => Math.max(n, b.frames), 0);
  const solveIds = gate.rows.filter((r) => r.failures.some((f) => SOLVE_REASONS.has(f))).map((r) => r.frameId);

  const setPicker = (key: string, label: string, sets: number[], pick: (id: number) => void) =>
    sets.length === 1 ? (
      <button type="button" className={BTN} onClick={() => pick(sets[0])}>{label}</button>
    ) : (
      <span className="relative">
        <button type="button" className={BTN} onClick={() => setMenuFor(menuFor === key ? null : key)}>{label} <ChevronDown size={11} /></button>
        {menuFor === key && (
          <ul role="menu" className="absolute z-10 mt-1 rounded border border-border bg-surface p-1 text-xs shadow">
            {sets.map((s) => (
              <li key={s} role="menuitem" className="cursor-pointer rounded px-2 py-1 hover:bg-surface-hover" onClick={() => { setMenuFor(null); pick(s); }}>Set #{s}</li>
            ))}
          </ul>
        )}
      </span>
    );

  let calibrationRendered = false;
  return (
    <div className="rounded border border-border bg-surface p-3 text-sm">
      <p className="mb-2 font-medium text-content">What blocks publishing</p>
      <ul className="space-y-1.5">
        {shown.map((b) => {
          if ((b.kind === 'linkCalibration') && calibrationRendered) return null;
          if (b.kind === 'linkCalibration') calibrationRendered = true;
          return (
            <li key={b.kind} className="flex flex-wrap items-center gap-2 text-content-secondary">
              <span>{b.kind === 'linkCalibration' ? `${calFrames} frames are not calibrated` : line(b)}</span>
              {b.kind === 'analyze' && setPicker('analyze', 'Analyze', b.sets, onAnalyze)}
              {b.kind === 'solve' && (
                <button type="button" className={BTN} disabled={solveBusy} onClick={() => onSolve(solveIds)}>Solve {solveIds.length} frames</button>
              )}
              {b.kind === 'linkCalibration' && (
                <>
                  {setPicker('cal', 'Open calibration', calSets, onOpenCalibration)}
                  {setPicker('attest', 'Attest as calibrated…', calSets, onOpenCalibration)}
                </>
              )}
              {b.kind === 'mapFilter' && <button type="button" className={BTN} onClick={onMapFilters}>Map filters</button>}
              {b.kind === 'analyze' && b.sets.some((s) => analyzeBusy.has(s)) && <span className="text-xs text-content-muted">analyzing…</span>}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
```

If a project has ONLY `buildMasters` (no `linkCalibration`), the calibration line must still render: compute `calibration = gate.blockers.find(b => b.kind === 'linkCalibration') ?? gate.blockers.find(b => b.kind === 'buildMasters')` and render it once in its `BLOCKER_ORDER` slot; adjust the filter above accordingly (the test's fixture has `linkCalibration`; add a second `it` for the `buildMasters`-only case).

- [ ] **Step 4: `FilterMappingDialog.tsx`**

```tsx
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Loader2, X } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import type { FilterMappingEdit, FilterMappingRowView, FilterMappingSheet, GateReport } from '../../types/models';

const AUTO = '__auto__';

function label(r: FilterMappingRowView): string {
  return `${r.filterRaw === '' ? '(no FILTER)' : r.filterRaw} · ${r.instrume || '(no INSTRUME)'} — ${r.frames} frame${r.frames === 1 ? '' : 's'}`;
}

/** Spec 2026-09-28 §5.3 — the publish-flow modal of §6.2. Unresolved rows
 * first (a proposal preselected — F3: it is a pending choice until Save),
 * resolved rows below a divider with their current value. Save sends only
 * the changed rows; `__auto__` on a resolved row sends `null`. */
export default function FilterMappingDialog({ projectId, onClose, onSaved }: { projectId: string; onClose: () => void; onSaved: (report: GateReport) => void }) {
  const { notify } = useNotifications();
  const [sheet, setSheet] = useState<FilterMappingSheet | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [choice, setChoice] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const key = (r: FilterMappingRowView) => `${r.instrume}\u0000${r.filterRaw}`;
  const initial = (r: FilterMappingRowView) => (r.resolution === 'mapped' || r.resolution === 'matched' ? (r.canonical ?? '') : (r.proposal ?? ''));

  useEffect(() => {
    let cancelled = false;
    api.invoke<FilterMappingSheet>('get_collab_filter_mapping_sheet', { projectId })
      .then((s) => {
        if (cancelled) return;
        setSheet(s);
        setChoice(Object.fromEntries(s.rows.map((r) => [key(r), initial(r)])));
      })
      .catch((err) => {
        console.error('[projects] filter mapping sheet failed:', err);
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => { cancelled = true; };
  }, [projectId]);

  const edits = useMemo<FilterMappingEdit[]>(() => {
    if (!sheet) return [];
    const out: FilterMappingEdit[] = [];
    for (const r of sheet.rows) {
      const c = choice[key(r)] ?? '';
      const stored = r.resolution === 'mapped' || r.resolution === 'mappedToMissing' ? r.canonical : null;
      if (c === AUTO) { if (stored != null) out.push({ instrume: r.instrume, filterRaw: r.filterRaw, canonical: null }); continue; }
      if (c === '') continue; // no choice on an unresolved row: not sent
      if (c !== stored) out.push({ instrume: r.instrume, filterRaw: r.filterRaw, canonical: c });
    }
    return out;
  }, [sheet, choice]);

  const save = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const report = await api.invoke<GateReport>('set_collab_filter_mappings', { projectId, mappings: edits });
      notify({ title: 'Filter mappings saved', detail: `${report.publishable} frames now pass the gate`, kind: 'project', tone: 'success' });
      onSaved(report);
    } catch (err) {
      console.error('[projects] set_collab_filter_mappings failed:', err);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [projectId, edits, notify, onSaved]);

  const unresolved = sheet?.rows.filter((r) => r.resolution === 'unmapped' || r.resolution === 'mappedToMissing') ?? [];
  const resolved = sheet?.rows.filter((r) => r.resolution === 'mapped' || r.resolution === 'matched') ?? [];

  const row = (r: FilterMappingRowView) => (
    <li key={key(r)} className="flex flex-wrap items-center gap-2 py-1">
      <span className="min-w-[18rem] text-sm text-content">{label(r)}</span>
      {r.resolution === 'mappedToMissing' && <span className="text-xs text-warning">mapped to "{r.canonical}", not in this project</span>}
      <select
        aria-label={`Canonical for ${label(r)}`}
        className="rounded border border-border bg-surface px-2 py-1 text-sm text-content"
        value={choice[key(r)] ?? ''}
        onChange={(e) => setChoice({ ...choice, [key(r)]: e.target.value })}
      >
        <option value="">{r.resolution === 'mapped' || r.resolution === 'matched' ? '' : '— choose —'}</option>
        {(r.resolution === 'mapped' || r.resolution === 'matched') && <option value={AUTO}>— automatic —</option>}
        {sheet!.dictionary.map((d) => (
          <option key={d.canonical} value={d.canonical}>{d.canonical} · {d.kind}</option>
        ))}
      </select>
    </li>
  );

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40" onClick={onClose}>
      <div className="max-h-[80vh] w-[40rem] overflow-auto rounded-lg border border-border bg-surface p-4" onClick={(e) => e.stopPropagation()}>
        <div className="mb-2 flex items-center justify-between">
          <h2 className="text-base font-semibold text-content">Filter mapping</h2>
          <button type="button" onClick={onClose} aria-label="Close" className="text-content-muted hover:text-content"><X size={16} /></button>
        </div>
        <p className="mb-3 text-xs text-content-muted">Pick the project's canonical filter for each raw name. Remembered for your account and asked once.</p>
        {error && <p className="mb-2 text-sm text-error">{error}</p>}
        {!sheet && !error && <Loader2 size={16} className="animate-spin text-content-muted" />}
        {sheet && (
          <>
            <ul>{unresolved.map(row)}</ul>
            {resolved.length > 0 && (
              <>
                <p className="mt-3 border-t border-border pt-2 text-xs text-content-muted">Already resolved</p>
                <ul>{resolved.map(row)}</ul>
              </>
            )}
            <div className="mt-3 flex items-center gap-2">
              <button type="button" onClick={() => void save()} disabled={busy || edits.length === 0} className="inline-flex items-center gap-1.5 rounded bg-accent px-3 py-1.5 text-sm text-surface hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50">
                {busy && <Loader2 size={12} className="animate-spin" />} Save
              </button>
              <button type="button" onClick={onClose} className="rounded border border-border px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover">Cancel</button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
```

(`kind: 'project'` must be an existing `NotificationKind`; the frame-set page already uses it for "Could not start project creation". If `tone` is not a field, drop it.)

- [ ] **Step 5: Wire the project page**

In `ProjectDetail.tsx`:
- Remove `publishBlockedByCalibration` and its `<p>`; `publishTooltip = publishable === 0 ? 'No passing frames to publish yet' : undefined`.
- State: `const [mapOpen, setMapOpen] = useState(false); const [solveBusy, setSolveBusy] = useState(false); const [analyzeBusy, setAnalyzeBusy] = useState<Set<number>>(new Set());`.
- Listeners (StrictMode-safe pattern) on `analysis-complete` and `plate-solve-complete`: on either, `setAnalyzeBusy(new Set())`/`setSolveBusy(false)` and re-run the gate (`loadGate()` — extract the gate fetch of `load()` into its own `useCallback`).
- Above `<GateTable>`: `{gate && <GateBlockers gate={gate} solveBusy={solveBusy} analyzeBusy={analyzeBusy} onMapFilters={() => setMapOpen(true)} onOpenCalibration={(setId) => navigate(`/frame-sets/${setId}?tab=calibration`)} onSolve={async (ids) => { setSolveBusy(true); try { await api.invoke('plate_solve_batch', { frameIds: ids }); } catch (err) { console.error('[projects] solve failed:', err); setSolveBusy(false); } }} onAnalyze={async (setId) => { setAnalyzeBusy((s) => new Set(s).add(setId)); try { await api.invoke('analyze_frame_set', { frameSetId: setId }); } catch (err) { console.error('[projects] analyze failed:', err); setAnalyzeBusy((s) => { const n = new Set(s); n.delete(setId); return n; }); } }} />}` — check the frame-set route path in `App.tsx` (`frame-sets/:id` or `sets/:id`) and whether `FrameSetDetail` reads `?tab=`; if it does not, add that one `useSearchParams` read in Task 9.
- `{mapOpen && id && <FilterMappingDialog projectId={id} onClose={() => setMapOpen(false)} onSaved={(r) => { setGate(r); setMapOpen(false); }} />}`.
- Contribute header row: after the `Link an object` button, `<AutoPublishSwitch projectId={id} enabled={c.autoPublish} onToggled={() => void load()} />`.
- `PublicationHistory`: beside `StateChip`, when `f.contributorState` is set render `<span className="rounded bg-surface-hover px-1.5 text-[10px] text-content-muted" title={f.contributorReason ?? undefined}>{f.contributorState}</span>` using the `short_label` mapping (`const SHORT: Record<string, string> = { notPublished: '—', failsGate: 'fails gate', pendingApproval: 'pending', published: 'published', updatePending: 'update', rejected: 'rejected', publishedNotOnDisk: 'not on disk', publishedNowFailsGate: 'now fails' }` in `src/components/collab/contributorState.ts`, shared with Task 9).

`AutoPublishSwitch.tsx`: the auto-publish `<label>` moved verbatim out of `AutoReplicateBar.tsx` (its `savingPublish` state and `set_project_auto_publish` call with the same error handling); `AutoReplicateBar` keeps auto-download and the volume; move its auto-publish test case into `AutoPublishSwitch.test.tsx`.

- [ ] **Step 6: Run the frontend suite and the type check**

Run: `npx vitest run src/components/collab src/pages/ProjectDetail.test.tsx && npx tsc --noEmit -p . && npm run lint`
Expected: green.

- [ ] **Step 7: Commit**

```bash
git add src/components/collab/GateBlockers.tsx src/components/collab/GateBlockers.test.tsx src/components/collab/FilterMappingDialog.tsx src/components/collab/FilterMappingDialog.test.tsx src/components/collab/AutoPublishSwitch.tsx src/components/collab/AutoPublishSwitch.test.tsx src/components/collab/AutoReplicateBar.tsx src/components/collab/AutoReplicateBar.test.tsx src/components/collab/contributorState.ts src/pages/ProjectDetail.tsx src/pages/ProjectDetail.test.tsx
git commit -m "feat(collab): blocker list with actions, the Filter mapping modal, the auto-publish switch in Contribute; dead decision-C hint removed (§5.3, §7.2, §9)"
```

---

### Task 9: Frame-set page — attestation checkbox, Project block and column

**Files:**
- Create: `src/components/collab/FrameSetProjectBlock.tsx` + `.test.tsx`
- Modify: `src/pages/FrameSetDetail.tsx` (header stats line ~862-870; the calibration tab ~971; the `LightsAnalysisView` mount ~985; a `?tab=` read), `src/components/LightsAnalysisView.tsx` (pass-through prop), `src/components/calibration/LightsAnalysisTable.tsx` (the column)

**Interfaces:**
- Consumes: `get_frame_set_project_status { framesSetId }` → `FrameSetProjectStatus`; `set_frame_set_attestation { framesSetId, attested }`; `set_collab_link { projectId, framesSetId, linked: true }`; `FramesSet.calibrated_externally`.
- Produces: `<FrameSetProjectBlock framesSetId status onChanged />`; `LightsAnalysisView`/`LightsAnalysisTable` prop `projectStates?: Map<number, { state: string; reason: string | null }>` (frame id → state) — the column renders only when the map is non-empty.

- [ ] **Step 1: Write the failing tests**

`FrameSetProjectBlock.test.tsx`:

```tsx
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import FrameSetProjectBlock from './FrameSetProjectBlock';
import { api } from '../../api';
import type { FrameSetProjectStatus } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
afterEach(cleanup);

const counts = { notPublished: 0, failsGate: 79, pendingApproval: 0, published: 812, updatePending: 3, rejected: 0, publishedNotOnDisk: 0, publishedNowFailsGate: 0 };

describe('FrameSetProjectBlock', () => {
  it('shows the linked project with its counts and Open project', () => {
    const status: FrameSetProjectStatus = { links: [{ projectId: 'p1', slug: 'm101', title: 'M 101', publishingHere: true, autoPublish: true, counts, frames: [] }], candidates: [] };
    render(<MemoryRouter><FrameSetProjectBlock framesSetId={5} status={status} onChanged={vi.fn()} /></MemoryRouter>);
    expect(screen.getByText(/Project M 101/)).toBeInTheDocument();
    expect(screen.getByText(/812 published · 79 fail gate · 3 update pending/)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Open project' })).toHaveAttribute('href', '/projects/p1');
  });

  it('offers Link to project for a candidate and links on click', async () => {
    vi.mocked(api.invoke).mockResolvedValue(undefined);
    const onChanged = vi.fn();
    const status: FrameSetProjectStatus = { links: [], candidates: [{ projectId: 'p1', slug: 'm101', title: 'M 101', distanceDeg: 0.42 }] };
    render(<MemoryRouter><FrameSetProjectBlock framesSetId={5} status={status} onChanged={onChanged} /></MemoryRouter>);
    expect(screen.getByText('Matches project M 101 (0.4° away)')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Link to project' }));
    await vi.waitFor(() => expect(api.invoke).toHaveBeenCalledWith('set_collab_link', { projectId: 'p1', framesSetId: 5, linked: true }));
    await vi.waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it('renders nothing with no links and no candidates', () => {
    const { container } = render(<MemoryRouter><FrameSetProjectBlock framesSetId={5} status={{ links: [], candidates: [] }} onChanged={vi.fn()} /></MemoryRouter>);
    expect(container).toBeEmptyDOMElement();
  });
});
```

For the column and the checkbox, add to the existing `LightsAnalysisTable` test file (if one exists; else a new `LightsAnalysisTable.project-column.test.tsx`): with `projectStates = new Map([[1, { state: 'updatePending', reason: null }], [2, { state: 'failsGate', reason: 'no analysis' }]])` the header `Project` renders, row 1 shows `update`, row 2 shows `fails gate` with `title="no analysis"`; with an empty map the header is absent. For the checkbox, a `FrameSetDetail` test is heavy — cover it in the acceptance smoke (§13 step 5) and by the ConfirmDialog copy asserted in a small unit test of the extracted `AttestationToggle` component (`src/components/calibration/AttestationToggle.tsx`): renders unchecked for `calibratedExternally=false`; clicking with `linkedCalibrationSets=2` opens the confirm with the text `This set has 2 linked calibration sets…`; confirming invokes `set_frame_set_attestation { framesSetId, attested: true }`; unchecking invokes with `false` and no confirm.

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/components/collab/FrameSetProjectBlock.test.tsx src/components/calibration`
Expected: module-not-found.

- [ ] **Step 3: `FrameSetProjectBlock.tsx`**

```tsx
import { useState } from 'react';
import { Link } from 'react-router-dom';
import { Loader2, Users } from 'lucide-react';
import { api } from '../../api';
import type { FrameSetProjectLink, FrameSetProjectStatus } from '../../types/models';

function summary(l: FrameSetProjectLink): string {
  const c = l.counts;
  const parts: string[] = [];
  if (c.published) parts.push(`${c.published} published`);
  if (c.pendingApproval) parts.push(`${c.pendingApproval} pending`);
  if (c.failsGate) parts.push(`${c.failsGate} fail gate`);
  if (c.updatePending) parts.push(`${c.updatePending} update pending`);
  if (c.rejected) parts.push(`${c.rejected} rejected`);
  if (c.publishedNotOnDisk) parts.push(`${c.publishedNotOnDisk} not on disk`);
  if (c.publishedNowFailsGate) parts.push(`${c.publishedNowFailsGate} now fail gate`);
  if (c.notPublished) parts.push(`${c.notPublished} not published`);
  return parts.join(' · ');
}

/** Spec 2026-09-28 §8.3 — the set's Project block under the header stats. */
export default function FrameSetProjectBlock({ framesSetId, status, onChanged }: { framesSetId: number; status: FrameSetProjectStatus; onChanged: () => void }) {
  const [busy, setBusy] = useState<string | null>(null);
  if (status.links.length === 0 && status.candidates.length === 0) return null;
  const link = async (projectId: string) => {
    setBusy(projectId);
    try {
      await api.invoke('set_collab_link', { projectId, framesSetId, linked: true });
      onChanged();
    } catch (err) {
      console.error('[projects] link from the set page failed:', err);
    } finally {
      setBusy(null);
    }
  };
  return (
    <div className="space-y-1 text-sm">
      {status.links.map((l) => (
        <div key={l.projectId} className="flex flex-wrap items-center gap-2 text-content-secondary">
          <Users size={14} className="text-content-muted" />
          <span><span className="text-content">Project {l.title}</span> · {summary(l)}</span>
          <Link to={`/projects/${l.projectId}`} className="rounded border border-border px-2 py-0.5 text-xs hover:bg-surface-hover">Open project</Link>
        </div>
      ))}
      {status.links.length === 0 && status.candidates.map((c) => (
        <div key={c.projectId} className="flex flex-wrap items-center gap-2 text-content-secondary">
          <Users size={14} className="text-content-muted" />
          <span>Matches project {c.title} ({c.distanceDeg.toFixed(1)}° away)</span>
          <button type="button" disabled={busy != null} onClick={() => void link(c.projectId)} className="inline-flex items-center gap-1 rounded border border-border px-2 py-0.5 text-xs hover:bg-surface-hover disabled:opacity-50">
            {busy === c.projectId && <Loader2 size={11} className="animate-spin" />} Link to project
          </button>
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 4: `AttestationToggle.tsx`, the column, the page wiring**

`AttestationToggle` (`src/components/calibration/AttestationToggle.tsx`): a `Checkbox`-based row (use the project's one `Checkbox` component from the settings primitives, per CLAUDE.md) labelled **Calibrated by an external tool** with the help line from spec §6.2; props `{ framesSetId, calibratedExternally, linkedCalibrationSets, onChanged }`; turning on with `linkedCalibrationSets > 0` opens `ConfirmDialog` (`title: 'Attest external calibration?'`, `message: \`This set has ${n} linked calibration sets. Attesting it means projects take the lights as they are and ignore those masters. Attest?\``, `confirmText: 'Attest'`); on confirm/off, `api.invoke('set_frame_set_attestation', { framesSetId, attested })`, errors to `console.error` + `notify()` (`title: 'Could not change the attestation'`).

In `FrameSetDetail.tsx`:
- Load `projectStatus` (`get_frame_set_project_status`) in `loadData` beside the hierarchy; keep it `null` on failure with a `console.error`.
- Header stats line: append `· attested` (`text-accent`) when `detail.frames_set?.calibrated_externally`; below the stats row render `<FrameSetProjectBlock framesSetId={…} status={projectStatus} onChanged={loadData} />` when `projectStatus`.
- Calibration tab: render `<AttestationToggle framesSetId calibratedExternally={detail.frames_set?.calibrated_externally ?? false} linkedCalibrationSets={calibrationHierarchy?.calibration_sets?.length ?? 0} onChanged={loadData} />` above `CalibrationHierarchyViewComponent` (use whatever count of linked calibration sets the hierarchy view exposes; `0` if none).
- `?tab=calibration` deep link: if the page does not already read `useSearchParams().get('tab')` into `activeTab` on mount, add it (one effect).
- Pass `projectStates` (a `Map` built from `projectStatus.links.flatMap(l => l.frames)`, first link wins) into `LightsAnalysisView` → `LightsAnalysisTable`.

In `LightsAnalysisTable.tsx`: prop `projectStates?: Map<number, { state: string; reason: string | null }>`; when `projectStates && projectStates.size > 0`, a `<th>Project</th>` after the `Locate` column and a `<td>` per row with `<span className="rounded bg-surface-hover px-1.5 text-[10px] text-content-muted" title={s.reason ?? undefined}>{SHORT[s.state] ?? s.state}</span>` (import `SHORT` from `src/components/collab/contributorState.ts`, Task 8); `—` when the frame has no entry. Thread the prop through `LightsAnalysisView`.

- [ ] **Step 5: Run the frontend suite, type check, lint**

Run: `npx vitest run && npx tsc --noEmit -p . && npm run lint`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/components/collab/FrameSetProjectBlock.tsx src/components/collab/FrameSetProjectBlock.test.tsx src/components/calibration/AttestationToggle.tsx src/components/calibration/AttestationToggle.test.tsx src/components/calibration/LightsAnalysisTable.tsx src/components/LightsAnalysisView.tsx src/pages/FrameSetDetail.tsx
git commit -m "feat(collab): the frame set's attestation toggle, Project block and column (§6.2, §8.3)"
```

---

### Task 10: E2E additions, full gates, docs and ledger

**Files:**
- Modify: `crates/athenaeum-core/src/api/collab_v3_live_e2e_tests.rs` (one frame without `FILTER`; one attested set)
- Modify: `CLAUDE.md` (the Transfers/collab summary: one sentence on mappings + attestation), `docs/transfers/README.md` (the collab section: §3.2 resolution order, F5 in-place seeding, the blockers), `docs/superpowers/open-items.md`

- [ ] **Step 1: E2E**

Read the three-instance e2e's fixture; add, on the contributor, a LIGHT with `filter = NULL` and assert: after the first publish it is held back with `no FILTER header — needs a filter mapping`; after `set_filter_mappings(ctx, pid, [{instrume, "", Some("None")}])` and a second publish it lands on the processor with the manifest `filter_canonical == "None"` and the landed file's header carrying `ATH_FILT = 'None'` (read the card with the FITS header reader). Add a second linked set on the contributor, attested, with one light: after publish the processor's landed bytes equal the contributor's original (`std::fs::read` both) and the header has no `ATH_PRJ` card.

Run: `cargo test -p athenaeum-core --lib api::collab_v3_live_e2e_tests -- --nocapture` (long; run alone).

- [ ] **Step 2: Full gates**

Run, one at a time:
- `cargo test -p athenaeum-core` (ALL targets — `tests/ts_contract.rs` included)
- `cargo check -p athenaeum-core --no-default-features`
- `cargo test -p athenaeum-tauri && cargo test -p athenaeum-web` (if they have tests; at least `cargo check` both)
- `npm test && npx tsc --noEmit -p . && npm run lint`
Expected: all green. Paste the summary lines into the commit message of Step 4.

- [ ] **Step 3: Docs**

`CLAUDE.md`, Transfers/collab paragraph — add one sentence after the wave-3 bullet: `**Contributor path (2026-09-28)**: a raw `FILTER` resolves explicit account mapping → dictionary exact/alias → unmapped (`collab::filters::resolve_filter`); an attested frame set (`frames_set.calibrated_externally`) is seeded in place with no generation; `GateReport.blockers` names one action per cause; the frame set's Project block and the project page share `collab::contributor_state::derive`.` Update the Tauri command count line if the count is stated (four new commands). `docs/transfers/README.md`: a "Contributor path" subsection with the resolution order, the external recipe, the blocker table and the state table (copy from the spec, cite it).

- [ ] **Step 4: Ledger and commit**

In `docs/superpowers/open-items.md` under the contributor-path spec line: `App plan DONE on branch \`contributor-path-app\` (<sha>): core suite (all targets) + no-default-features check + frontend suite green; OWED: merge on the owner's word, the acceptance smoke of spec §13 steps 2–8 against the test hub once the hub plan is deployed.`

```bash
git add CLAUDE.md docs/transfers/README.md docs/superpowers/open-items.md crates/athenaeum-core/src/api/collab_v3_live_e2e_tests.rs
git commit -m "test(collab): e2e covers the unmapped-then-mapped light and the attested set; docs + ledger for the contributor path"
```

Stop here: merging to `main` and pushing happen only on the owner's word.
