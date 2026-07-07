//! Exhaustive tests for the retention evaluator — the module that guards a
//! user's only copy of a night's data.
//!
//! Every acceptance test runs in **both** dry-run and real-delete mode over real
//! tempdir files. In these core tests the `package_ref` of each `sync_outbound`
//! row is pointed straight at a real temp file, so the deleter (plain
//! `remove_file`) actually removes the subject and the assertions can check the
//! filesystem. Perseus's own tests cover the package_ref → source-capture-file
//! mapping; here the concern is purely the eligibility decision and the hard
//! invariant.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use rusqlite::{params, Connection};

use crate::sharing::types::NodeId;

use super::retention::{evaluate_and_apply, DeleteOutcome, RetentionPolicy};
use super::store::{StandaloneSyncStore, SyncStore};

const PEER: NodeId = [7u8; 32];

fn open_store(tmp: &tempfile::TempDir) -> (StandaloneSyncStore, PathBuf) {
    let path = tmp.path().join("sync.db");
    let store = StandaloneSyncStore::open(&path).unwrap();
    (store, path)
}

/// Create a real file under `tmp` and return its path. Used AS the `package_ref`
/// so the deleter can actually remove it and the invariant is checked against
/// real disk state.
fn make_file(tmp: &tempfile::TempDir, name: &str) -> PathBuf {
    let p = tmp.path().join(name);
    std::fs::write(&p, b"frame-bytes").unwrap();
    p
}

/// Enqueue a package whose `package_ref` is `path`; return the durable row id.
fn enqueue(store: &StandaloneSyncStore, path: &Path) -> i64 {
    store.enqueue(&path.to_string_lossy(), PEER).unwrap()
}

/// Force a row's `confirmed_at` to an explicit RFC3339 value via a second raw
/// connection to the same WAL db — the test's way to place confirmations at
/// chosen points in time without sleeping on a real clock.
fn set_confirmed_at(db_path: &Path, id: i64, ts: &str) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute(
        "UPDATE sync_outbound SET confirmed_at = ?1 WHERE id = ?2",
        params![ts, id],
    )
    .unwrap();
}

/// A call-counting deleter that really removes the file and reports
/// `DeleteOutcome::Removed`. `calls` is a `Cell` so the count can be read after
/// the closure is done borrowing (interior mutability, no lingering `&mut`).
fn counting_deleter(calls: &Cell<usize>) -> impl FnMut(&Path) -> anyhow::Result<DeleteOutcome> + '_ {
    move |p: &Path| {
        calls.set(calls.get() + 1);
        std::fs::remove_file(p)?;
        Ok(DeleteOutcome::Removed)
    }
}

// ── untransferred_never_eligible_even_on_full_disk ───────────────────────────

/// The invariant, at maximum pressure: disk 99% full, DiskPct policy, but
/// nothing confirmed. Nothing is eligible, the deleter is never called, every
/// untransferred file survives, and the pass flags disk pressure. Holds in both
/// dry-run and real-delete mode.
#[test]
fn untransferred_never_eligible_even_on_full_disk() {
    for dry_run in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _path) = open_store(&tmp);

        let f1 = make_file(&tmp, "a.fits");
        let f2 = make_file(&tmp, "b.fits");
        let f3 = make_file(&tmp, "c.fits");
        enqueue(&store, &f1);
        enqueue(&store, &f2);
        enqueue(&store, &f3); // none confirmed — all still in flight

        let probe = || 99u8;
        let calls = Cell::new(0usize);
        let mut deleter = counting_deleter(&calls);

        let outcome = evaluate_and_apply(
            &store,
            &RetentionPolicy::DiskPct { max_pct: 80 },
            dry_run,
            Utc::now(),
            &probe,
            &mut deleter,
        )
        .unwrap();

        assert!(
            outcome.eligible.is_empty(),
            "nothing confirmed → nothing eligible (dry_run={dry_run})"
        );
        assert!(outcome.deleted.is_empty());
        assert!(
            outcome.would_warn_disk_pressure,
            "full disk with nothing to free must flag a would-warn"
        );
        assert_eq!(calls.get(), 0, "the deleter must never see an unconfirmed file");
        assert!(
            f1.exists() && f2.exists() && f3.exists(),
            "untransferred files are untouchable"
        );
    }
}

// ── on_confirm_deletes_only_confirmed ────────────────────────────────────────

#[test]
fn on_confirm_deletes_only_confirmed() {
    for dry_run in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _path) = open_store(&tmp);

        let c1 = make_file(&tmp, "c1.fits");
        let c2 = make_file(&tmp, "c2.fits");
        let u = make_file(&tmp, "u.fits");
        let id1 = enqueue(&store, &c1);
        let id2 = enqueue(&store, &c2);
        let _idu = enqueue(&store, &u);
        store.confirm(id1, &[]).unwrap();
        store.confirm(id2, &[]).unwrap();

        let probe = || 0u8;
        let calls = Cell::new(0usize);
        let mut deleter = counting_deleter(&calls);

        let outcome = evaluate_and_apply(
            &store,
            &RetentionPolicy::OnConfirm,
            dry_run,
            Utc::now(),
            &probe,
            &mut deleter,
        )
        .unwrap();

        assert_eq!(outcome.eligible.len(), 2, "both confirmed are eligible");
        assert!(outcome.eligible.contains(&c1) && outcome.eligible.contains(&c2));
        assert!(!outcome.eligible.contains(&u), "the unconfirmed file is never eligible");

        if dry_run {
            assert_eq!(calls.get(), 0);
            assert!(outcome.deleted.is_empty());
            assert!(c1.exists() && c2.exists(), "dry-run deletes nothing");
        } else {
            assert_eq!(calls.get(), 2);
            assert_eq!(outcome.deleted.len(), 2);
            assert!(!c1.exists() && !c2.exists(), "confirmed sources are deleted");
        }
        assert!(u.exists(), "the unconfirmed source is never touched");
    }
}

// ── keep_days_respects_confirmed_at ──────────────────────────────────────────

#[test]
fn keep_days_respects_confirmed_at() {
    for dry_run in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let (store, path) = open_store(&tmp);

        let old = make_file(&tmp, "old.fits");
        let recent = make_file(&tmp, "recent.fits");
        let u = make_file(&tmp, "u.fits");
        let id_old = enqueue(&store, &old);
        let id_recent = enqueue(&store, &recent);
        let _idu = enqueue(&store, &u);
        store.confirm(id_old, &[]).unwrap();
        store.confirm(id_recent, &[]).unwrap();

        let now = Utc::now();
        set_confirmed_at(&path, id_old, &(now - Duration::days(40)).to_rfc3339());
        set_confirmed_at(&path, id_recent, &(now - Duration::days(5)).to_rfc3339());

        let probe = || 0u8;
        let calls = Cell::new(0usize);
        let mut deleter = counting_deleter(&calls);

        let outcome = evaluate_and_apply(
            &store,
            &RetentionPolicy::KeepDays(30),
            dry_run,
            now,
            &probe,
            &mut deleter,
        )
        .unwrap();

        assert_eq!(
            outcome.eligible,
            vec![old.clone()],
            "only the >30d-old confirmed file is eligible (dry_run={dry_run})"
        );

        if dry_run {
            assert_eq!(calls.get(), 0);
            assert!(outcome.deleted.is_empty());
            assert!(old.exists());
        } else {
            assert_eq!(outcome.deleted, vec![old.clone()]);
            assert!(!old.exists(), "the aged confirmed file is deleted");
        }
        assert!(recent.exists(), "a recently-confirmed file is kept");
        assert!(u.exists(), "an unconfirmed file is always kept");
    }
}

// ── disk_pct_deletes_oldest_confirmed_first_until_under_threshold ─────────────

#[test]
fn disk_pct_deletes_oldest_confirmed_first_until_under_threshold() {
    for dry_run in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let (store, path) = open_store(&tmp);

        // Five confirmed files with ascending confirmed_at: f0 oldest … f4 newest.
        let now = Utc::now();
        let mut files = Vec::new();
        for i in 0..5 {
            let f = make_file(&tmp, &format!("f{i}.fits"));
            let id = enqueue(&store, &f);
            store.confirm(id, &[]).unwrap();
            set_confirmed_at(&path, id, &(now - Duration::days((5 - i) as i64)).to_rfc3339());
            files.push(f);
        }

        // Usage = 50 + 10 per file still on disk → 5 files = 100%. Each real
        // delete frees 10%. Cap 75: 100→90→80→70(stop) = 3 oldest deleted.
        let probe = || {
            let remaining = files.iter().filter(|p| p.exists()).count() as u8;
            50 + 10 * remaining
        };
        let calls = Cell::new(0usize);
        let mut deleter = counting_deleter(&calls);

        let outcome = evaluate_and_apply(
            &store,
            &RetentionPolicy::DiskPct { max_pct: 75 },
            dry_run,
            now,
            &probe,
            &mut deleter,
        )
        .unwrap();

        assert_eq!(outcome.eligible.len(), 5, "all confirmed are candidates under pressure");

        if dry_run {
            assert_eq!(calls.get(), 0, "dry-run never deletes");
            assert!(outcome.deleted.is_empty());
            assert!(files.iter().all(|f| f.exists()));
            assert!(outcome.would_warn_disk_pressure, "still over cap in dry-run");
        } else {
            assert_eq!(
                outcome.deleted,
                vec![files[0].clone(), files[1].clone(), files[2].clone()],
                "the three oldest confirmed are deleted, in order"
            );
            assert!(!files[0].exists() && !files[1].exists() && !files[2].exists());
            assert!(
                files[3].exists() && files[4].exists(),
                "deletion stops as soon as usage is back under the cap"
            );
            assert!(!outcome.would_warn_disk_pressure, "back under cap → no warn");
        }
    }
}

// ── dry_run_deletes_nothing_but_reports ──────────────────────────────────────

#[test]
fn dry_run_deletes_nothing_but_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _path) = open_store(&tmp);

    let c1 = make_file(&tmp, "c1.fits");
    let c2 = make_file(&tmp, "c2.fits");
    let id1 = enqueue(&store, &c1);
    let id2 = enqueue(&store, &c2);
    store.confirm(id1, &[]).unwrap();
    store.confirm(id2, &[]).unwrap();

    let probe = || 0u8;
    // A deleter whose call count must stay 0 in dry-run (it only bumps the count).
    let calls = Cell::new(0usize);
    let mut deleter = |_p: &Path| -> anyhow::Result<DeleteOutcome> {
        calls.set(calls.get() + 1);
        Ok(DeleteOutcome::Removed)
    };

    let outcome = evaluate_and_apply(
        &store,
        &RetentionPolicy::OnConfirm,
        true,
        Utc::now(),
        &probe,
        &mut deleter,
    )
    .unwrap();

    assert_eq!(calls.get(), 0, "the deleter must NEVER be invoked in dry-run");
    assert!(outcome.deleted.is_empty(), "dry-run deletes nothing");
    assert_eq!(outcome.eligible.len(), 2, "but it reports what it would delete");
    assert!(outcome.dry_run);
    assert!(c1.exists() && c2.exists(), "the files remain on disk");
}

// ── skipped_noop_delete_is_not_counted_as_deleted ────────────────────────────

/// Review fix (minor #3): a deleter reporting `SkippedNoop` (e.g. the subject
/// was already handled by a prior pass, or a last-line guard declined) must NOT
/// inflate `outcome.deleted` — it is still reported as `eligible` (a genuine
/// confirmed candidate this pass), but the count of files actually removed must
/// stay honest.
#[test]
fn skipped_noop_delete_is_not_counted_as_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _path) = open_store(&tmp);

    let c1 = make_file(&tmp, "c1.fits");
    let id1 = enqueue(&store, &c1);
    store.confirm(id1, &[]).unwrap();

    let probe = || 0u8;
    let mut deleter = |_p: &Path| -> anyhow::Result<DeleteOutcome> { Ok(DeleteOutcome::SkippedNoop) };

    let outcome = evaluate_and_apply(
        &store,
        &RetentionPolicy::OnConfirm,
        false,
        Utc::now(),
        &probe,
        &mut deleter,
    )
    .unwrap();

    assert_eq!(outcome.eligible.len(), 1, "still reported as a genuine candidate");
    assert!(
        outcome.deleted.is_empty(),
        "a no-op skip must never be counted as an actual deletion"
    );
    assert!(c1.exists(), "the file is untouched by a no-op deleter");
}

// ── keep_everything_never_eligible (defensive extra) ─────────────────────────

/// The safe default deletes nothing even with confirmed data and a full disk.
#[test]
fn keep_everything_never_eligible() {
    for dry_run in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _path) = open_store(&tmp);

        let c1 = make_file(&tmp, "c1.fits");
        let id1 = enqueue(&store, &c1);
        store.confirm(id1, &[]).unwrap();

        let probe = || 99u8;
        let calls = Cell::new(0usize);
        let mut deleter = counting_deleter(&calls);

        let outcome = evaluate_and_apply(
            &store,
            &RetentionPolicy::KeepEverything,
            dry_run,
            Utc::now(),
            &probe,
            &mut deleter,
        )
        .unwrap();

        assert!(outcome.eligible.is_empty());
        assert!(outcome.deleted.is_empty());
        assert!(!outcome.would_warn_disk_pressure);
        assert_eq!(calls.get(), 0);
        assert!(c1.exists(), "keep_everything never deletes");
    }
}
