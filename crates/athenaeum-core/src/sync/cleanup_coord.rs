//! Shared-payload cleanup coordinator for the multi-target fan-out (Sync 2C).
//!
//! # The problem it solves
//!
//! Perseus builds ONE package directory and fans it out to N independent
//! [`SyncEngine`](super::engine::SyncEngine)s — one per send target, each with
//! its own peer-scoped outbound rows but all serving the SAME on-disk payload
//! copies. The pre-fix engine cleaned those copies the instant a *single* target
//! confirmed. So if target B was offline when target A confirmed, the shared
//! payload was deleted out from under B — B's retry then re-served a
//! manifest-only collection and B **silently never received the frame**.
//!
//! # The fix
//!
//! Every engine in a fan-out is handed the SAME [`SharedPackageCleanup`] as its
//! [`PackageCleanupSink`](super::engine::PackageCleanupSink). Perseus
//! [`register`](SharedPackageCleanup::register)s each fanned-out dir with the
//! number of targets it actually reached (`expected`); each engine calls
//! [`on_terminal`](super::engine::PackageCleanupSink::on_terminal) exactly once
//! when its target reaches a terminal state — **confirmed, failed, or
//! cancelled** (a dead/offline target must never block cleanup forever). The
//! coordinator fires [`cleanup_package_payloads`] exactly ONCE, only when the
//! terminal count reaches `expected`.
//!
//! # Keyed on the directory, not a `PackageId`
//!
//! The shared identity across the N engines is the **package directory path**:
//! each engine mints its own per-session announce `PackageId`, so those are not
//! shared and cannot be the key, and a pre-announce failure has no `PackageId`
//! at all. The dir is the one identity present at every register + terminal site.
//!
//! # Buffering makes it order-independent
//!
//! `on_terminal` for a dir that has not been `register`ed yet is buffered (the
//! terminal count still increments); the later `register` sets `expected` and
//! re-checks the gate. This tolerates the loopback race where a target can
//! confirm before the enqueue caller has finished the fan-out loop and called
//! `register`. A terminal for a dir that is never registered simply retains the
//! payload (the safe direction — the bug we fix is *premature* deletion).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::engine::{cleanup_package_payloads, PackageCleanupSink};

/// Per-dir cleanup bookkeeping. `expected` is `None` until the owning package is
/// registered (a terminal may arrive first — see module docs); `terminal` counts
/// terminal signals seen so far; `cleaned` guarantees the once-only cleanup.
#[derive(Default)]
struct PackageState {
    expected: Option<usize>,
    terminal: usize,
    cleaned: bool,
}

/// Thread-safe coordinator that cleans a fanned-out package dir's payloads
/// exactly once, after every target that received it is terminal. One instance
/// is shared (via `Arc<dyn PackageCleanupSink>`) by all N engines of a
/// multi-target send.
#[derive(Default)]
pub struct SharedPackageCleanup {
    inner: Mutex<HashMap<PathBuf, PackageState>>,
}

impl SharedPackageCleanup {
    /// A fresh coordinator with no registered packages.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare that `dir`'s payload must be kept until `expected` targets have
    /// reached a terminal state. Safe to call after some terminals have already
    /// been recorded (they were buffered) — this sets `expected` and fires the
    /// once-only cleanup immediately if the gate is already satisfied.
    ///
    /// `expected == 0` (a package that reached zero targets — nothing will ever
    /// transfer it, so no target's retry can be starved) cleans the orphaned dir
    /// right away.
    pub fn register(&self, dir: &Path, expected: usize) {
        let mut map = self
            .inner
            .lock()
            .expect("cleanup coordinator mutex poisoned");
        let state = map.entry(dir.to_path_buf()).or_default();
        // Raise, never lower: a re-register (e.g. reconcile after a runtime
        // register) must not shrink the target count below what has been seen.
        state.expected = Some(state.expected.map_or(expected, |e| e.max(expected)));
        maybe_clean(dir, state);
    }

    /// Raise `dir`'s `expected` target count by `delta` — for extra outbound rows
    /// added to an ALREADY-registered fan-out dir after its initial [`register`]
    /// (the Perseus web retry: re-enqueueing a failed package mints a new row on
    /// the sinked engine, whose eventual terminal would otherwise over-count
    /// against the stale `expected` and free the payload while a still-offline
    /// target has yet to receive it — the exact data loss `register` fixed,
    /// reopened via retry).
    ///
    /// Distinct from [`register`](Self::register), which raises-to-max: a retry
    /// genuinely ADDS a terminal signal, so its count must be added, not maxed.
    /// If the dir is absent it is created with `expected = delta` (a retry can
    /// only follow the original enqueue+register in practice, so this is
    /// defensive); if already **cleaned** it is a no-op — the payload is gone and
    /// the web layer guards retries on payload presence, so a post-cleanup bump
    /// must neither resurrect state nor re-run the once-only cleanup.
    pub fn bump(&self, dir: &Path, delta: usize) {
        let mut map = self
            .inner
            .lock()
            .expect("cleanup coordinator mutex poisoned");
        let state = map.entry(dir.to_path_buf()).or_default();
        if state.cleaned {
            return;
        }
        // Add `delta` to the known target count. `expected` is `Some` whenever a
        // register preceded this bump (the production path); an unregistered dir's
        // unknown count is treated as 0 so the added row still raises the gate.
        state.expected = Some(state.expected.unwrap_or(0) + delta);
        maybe_clean(dir, state);
    }

    /// Re-arm `dir`'s cleanup gate for a retry that may follow the once-only
    /// cleanup (the Perseus reset-in-place resend, which REBUILDS a cleaned
    /// dir's payloads from the original capture files and re-drives the same
    /// outbound row):
    ///
    /// - not yet cleaned → exactly [`bump`](Self::bump): the reset row will
    ///   terminalize a second time, so its extra terminal must raise `expected`
    ///   or it would over-count and free the payload while another target is
    ///   still pending;
    /// - already cleaned → the payload is back on disk, so the once-only state
    ///   is REPLACED with a fresh gate of `expected = delta`, `terminal = 0`:
    ///   the rebuilt dir again waits for its (new) terminals before the next
    ///   once-only cleanup.
    ///
    /// Distinct from [`bump`], which must stay a no-op after cleanup (its
    /// callers do NOT restore payloads — resurrecting the gate there would arm
    /// a second cleanup for a dir that stays manifest-only).
    pub fn rearm(&self, dir: &Path, delta: usize) {
        let mut map = self
            .inner
            .lock()
            .expect("cleanup coordinator mutex poisoned");
        let state = map.entry(dir.to_path_buf()).or_default();
        if state.cleaned {
            *state = PackageState {
                expected: Some(delta),
                terminal: 0,
                cleaned: false,
            };
            return;
        }
        state.expected = Some(state.expected.unwrap_or(0) + delta);
        maybe_clean(dir, state);
    }
}

impl PackageCleanupSink for SharedPackageCleanup {
    fn on_terminal(&self, dir: &Path) {
        let mut map = self
            .inner
            .lock()
            .expect("cleanup coordinator mutex poisoned");
        let state = map.entry(dir.to_path_buf()).or_default();
        state.terminal += 1;
        maybe_clean(dir, state);
    }
}

/// Fire the once-only payload cleanup for `dir` if every target is terminal.
/// A no-op while `expected` is unknown, already cleaned, or the gate is unmet.
fn maybe_clean(dir: &Path, state: &mut PackageState) {
    if state.cleaned {
        return;
    }
    let Some(expected) = state.expected else {
        return;
    };
    if state.terminal < expected {
        return;
    }
    // Flip the flag BEFORE the fs work so a concurrent caller (we hold the lock,
    // so there is none here — but this is the contract) can never double-clean.
    state.cleaned = true;
    match cleanup_package_payloads(dir) {
        Ok(freed_bytes) => tracing::info!(
            dir = %dir.display(),
            freed_bytes,
            expected,
            "fan-out package payloads cleaned (all targets terminal)"
        ),
        Err(e) => tracing::warn!(
            dir = %dir.display(),
            error = %format!("{e:#}"),
            "fan-out package payload cleanup failed"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::MANIFEST_FILENAME;
    use std::path::PathBuf;

    /// Build a package-dir-shaped tempdir: the manifest that cleanup must keep,
    /// plus one payload file it must remove once the gate is met.
    fn make_pkg_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILENAME), b"{}\n").unwrap();
        std::fs::write(dir.join("frame.fits"), vec![7u8; 4096]).unwrap();
        dir
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The shared payload must survive until EVERY target is terminal: one
    /// terminal out of two leaves the payload in place; the second cleans it,
    /// keeping only the manifest.
    #[test]
    fn payload_survives_until_all_targets_confirm() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-a");

        let coord = SharedPackageCleanup::new();
        coord.register(&dir, 2);

        // First target confirmed — the second is still (possibly) offline.
        coord.on_terminal(&dir);
        assert!(
            dir.join("frame.fits").exists(),
            "payload must survive while a second target has not yet terminalized"
        );

        // Second (and last) target terminal → clean, manifest kept.
        coord.on_terminal(&dir);
        assert_eq!(
            entries(&dir),
            vec![MANIFEST_FILENAME.to_string()],
            "once all targets are terminal only the manifest remains"
        );
    }

    /// A failed target still counts as terminal: fail + confirm across two
    /// targets frees the shared payload (a dead peer must not pin it forever).
    #[test]
    fn payload_freed_when_a_target_fails_then_other_confirms() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-b");

        let coord = SharedPackageCleanup::new();
        coord.register(&dir, 2);

        // Target A gives up (Failed) — the engine still reports it terminal.
        coord.on_terminal(&dir);
        assert!(
            dir.join("frame.fits").exists(),
            "one terminal is not enough"
        );

        // Target B confirms — now every target is terminal.
        coord.on_terminal(&dir);
        assert_eq!(
            entries(&dir),
            vec![MANIFEST_FILENAME.to_string()],
            "fail + confirm = both terminal → payload freed"
        );
    }

    /// Cleanup runs exactly once: a late terminal after the gate was met is a
    /// no-op and never re-cleans (proven by a sentinel written post-cleanup).
    #[test]
    fn cleanup_runs_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-c");

        let coord = SharedPackageCleanup::new();
        coord.register(&dir, 2);
        coord.on_terminal(&dir);
        coord.on_terminal(&dir);
        assert_eq!(
            entries(&dir),
            vec![MANIFEST_FILENAME.to_string()],
            "cleaned after the second terminal"
        );

        // A sentinel dropped in after cleanup must NOT be swept by a late,
        // redundant terminal — cleanup is once-only.
        std::fs::write(dir.join("late-sentinel.fits"), b"still here").unwrap();
        coord.on_terminal(&dir);
        assert!(
            dir.join("late-sentinel.fits").exists(),
            "a terminal after the once-only cleanup must be a no-op"
        );
    }

    /// `expected == 0` (a package that reached zero targets) is an orphan whose
    /// payload can be freed immediately — no target's retry can ever need it.
    #[test]
    fn zero_expected_orphan_is_cleaned_on_register() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-d");

        let coord = SharedPackageCleanup::new();
        coord.register(&dir, 0);
        assert_eq!(
            entries(&dir),
            vec![MANIFEST_FILENAME.to_string()],
            "a zero-target orphan is cleaned right away"
        );
    }

    /// A `bump` before a retry's terminal raises `expected` so the retried row's
    /// own terminal cannot prematurely free a still-offline target's payload.
    /// Scenario (the web-retry data-loss hole this closes): 2 targets, target A
    /// fails, the operator retries A onto the sinked engine (one extra row), and
    /// target B is still offline — the retry's confirm must NOT trip cleanup.
    #[test]
    fn bump_before_retry_terminal_prevents_premature_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-f");

        let coord = SharedPackageCleanup::new();
        coord.register(&dir, 2);

        // Target A failed (terminal=1); B still offline, payload must be retained.
        coord.on_terminal(&dir);
        assert!(
            dir.join("frame.fits").exists(),
            "one terminal (A failed) is short of two targets"
        );

        // Operator retries A → one extra row on the sinked engine. Bump raises
        // expected 2 → 3 so the retry's own terminal cannot close the gate.
        coord.bump(&dir, 1);

        // A's retry (A2) confirms: terminal=2, still < 3 → payload retained for B.
        coord.on_terminal(&dir);
        assert!(
            dir.join("frame.fits").exists(),
            "the retried row's terminal must not free the payload while B is offline"
        );

        // B finally confirms: terminal=3 == expected → cleaned, manifest kept.
        coord.on_terminal(&dir);
        assert_eq!(
            entries(&dir),
            vec![MANIFEST_FILENAME.to_string()],
            "only once every original target AND the retry are terminal is the payload freed"
        );
    }

    /// `bump` on an already-cleaned dir is a no-op: the payload is gone and the
    /// web layer guards retries on payload presence, so a post-cleanup bump must
    /// neither resurrect state nor re-run cleanup (proven by a post-clean sentinel).
    #[test]
    fn bump_after_clean_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-g");

        let coord = SharedPackageCleanup::new();
        coord.register(&dir, 1);
        coord.on_terminal(&dir); // gate met (1/1) → cleaned
        assert_eq!(entries(&dir), vec![MANIFEST_FILENAME.to_string()]);

        std::fs::write(dir.join("late-sentinel.fits"), b"still here").unwrap();
        coord.bump(&dir, 1);
        assert!(
            dir.join("late-sentinel.fits").exists(),
            "a bump after the once-only cleanup must be a no-op"
        );
    }

    /// `rearm` after the once-only cleanup replaces the spent gate with a fresh
    /// one: the rebuilt payload waits for the reset row's new terminal, then is
    /// cleaned exactly once more (a further late terminal is again a no-op).
    #[test]
    fn rearm_after_clean_reopens_gate_and_cleans_once_more() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-h");

        let coord = SharedPackageCleanup::new();
        coord.register(&dir, 1);
        coord.on_terminal(&dir); // gate met (1/1) → cleaned, manifest-only
        assert_eq!(entries(&dir), vec![MANIFEST_FILENAME.to_string()]);

        // The resend path rebuilds the payload from the originals, then re-arms.
        std::fs::write(dir.join("frame.fits"), vec![7u8; 4096]).unwrap();
        coord.rearm(&dir, 1);
        assert!(
            dir.join("frame.fits").exists(),
            "re-arming must not itself clean the rebuilt payload"
        );

        // The reset row terminalizes again → the fresh 1-target gate closes.
        coord.on_terminal(&dir);
        assert_eq!(
            entries(&dir),
            vec![MANIFEST_FILENAME.to_string()],
            "the rebuilt payload is cleaned once the re-armed gate is met"
        );

        // And the second cleanup is once-only too.
        std::fs::write(dir.join("late-sentinel.fits"), b"still here").unwrap();
        coord.on_terminal(&dir);
        assert!(
            dir.join("late-sentinel.fits").exists(),
            "a late terminal after the re-armed cleanup is a no-op"
        );
    }

    /// `rearm` before any cleanup is exactly `bump`: it adds the retry's future
    /// terminal to `expected` so the retried row's confirm cannot free the
    /// payload while the sibling target is still pending.
    #[test]
    fn rearm_before_clean_behaves_like_bump() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-i");

        let coord = SharedPackageCleanup::new();
        coord.register(&dir, 2);
        coord.on_terminal(&dir); // target A failed; B still pending
        coord.rearm(&dir, 1); // A is reset-in-place → expected 2 → 3

        // A's retry confirms: terminal=2 < 3 → payload retained for B.
        coord.on_terminal(&dir);
        assert!(
            dir.join("frame.fits").exists(),
            "the reset row's second terminal must not free B's payload"
        );

        // B confirms: terminal=3 == expected → cleaned.
        coord.on_terminal(&dir);
        assert_eq!(entries(&dir), vec![MANIFEST_FILENAME.to_string()]);
    }

    /// A terminal that arrives BEFORE its register is buffered, and the later
    /// register with the real target count still gates correctly.
    #[test]
    fn terminal_before_register_is_buffered() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_pkg_dir(tmp.path(), "pkg-e");

        let coord = SharedPackageCleanup::new();
        // Race: a target confirmed before the enqueue caller registered the dir.
        coord.on_terminal(&dir);
        assert!(
            dir.join("frame.fits").exists(),
            "a buffered terminal must not clean before expected is known"
        );

        coord.register(&dir, 2);
        assert!(
            dir.join("frame.fits").exists(),
            "one buffered terminal is still short of two targets"
        );

        coord.on_terminal(&dir);
        assert_eq!(
            entries(&dir),
            vec![MANIFEST_FILENAME.to_string()],
            "buffered + runtime terminal reach the gate → cleaned"
        );
    }
}
