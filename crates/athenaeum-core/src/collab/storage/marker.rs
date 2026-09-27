//! The Collaboration root's storage marker (spec §9.1, plan P22): a random
//! store id plus the device that designated it. Unmounted is not deleted —
//! an unavailable store stops serving and fetching and changes no frame.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

pub const MARKER_REL: &str = ".athenaeum/store-id";
pub const WRITE_PROBE_REL: &str = ".athenaeum/.write-probe";
pub const REPLACE_PROMPT_AFTER: chrono::Duration = chrono::Duration::days(7);
pub const RETIRE_PROPOSAL_AFTER: chrono::Duration = chrono::Duration::days(30);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreMarker {
    pub store_id: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnavailableReason {
    PathMissing,
    NotADirectory,
    MarkerMissing,
    MarkerMismatch,
    OtherDevice { device_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreState {
    Available,
    ReadOnly,
    Unavailable(UnavailableReason),
}

impl StoreState {
    /// A read-only remount must not take a full replica off the swarm.
    pub fn serving(&self) -> bool {
        matches!(self, StoreState::Available | StoreState::ReadOnly)
    }
    pub fn fetching(&self) -> bool {
        matches!(self, StoreState::Available)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    State(StoreState),
    Adopt(StoreMarker),
}

pub fn read_marker(root: &Path) -> std::io::Result<Option<StoreMarker>> {
    match std::fs::read(root.join(MARKER_REL)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write the marker atomically (`.tmp` + rename). Creates `.athenaeum` only
/// inside an EXISTING root — never the root itself.
pub fn write_marker(root: &Path, m: &StoreMarker) -> std::io::Result<()> {
    if !root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "collaboration root is not an existing folder",
        ));
    }
    let dir = root.join(".athenaeum");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("store-id.tmp");
    std::fs::write(&tmp, serde_json::to_vec(m).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, root.join(MARKER_REL))
}

/// A best-effort write probe under `.athenaeum` — the ONE signal telling
/// `Available` (this device can both serve and fetch) from `ReadOnly` (a
/// remount that can still serve what is already on disk, spec §9.1). Never
/// swallows its error (CLAUDE.md rule): a failed write is logged at `debug!`
/// (expected in normal operation — a read-only remount is not a fault) and a
/// leftover probe file (the write itself succeeded but the cleanup did not)
/// is logged at `warn!`.
pub(crate) fn writable(root: &Path) -> bool {
    let probe = root.join(WRITE_PROBE_REL);
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            if let Err(e) = std::fs::remove_file(&probe) {
                tracing::warn!(path = %probe.display(), error = %e, "collaboration write probe left behind");
            }
            true
        }
        Err(e) => {
            tracing::debug!(path = %probe.display(), error = %e, "collaboration write probe failed; store read-only");
            false
        }
    }
}

pub fn check_store(root: &Path, recorded: Option<&StoreMarker>, me: &str) -> CheckOutcome {
    use CheckOutcome::*;
    use StoreState::*;
    use UnavailableReason::*;
    if !root.exists() {
        return State(Unavailable(PathMissing));
    }
    if !root.is_dir() {
        return State(Unavailable(NotADirectory));
    }
    let marker = match read_marker(root) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %root.display(), error = %e, "collaboration store marker unreadable");
            return State(Unavailable(MarkerMissing));
        }
    };
    match (marker, recorded) {
        (None, None) => Adopt(StoreMarker {
            store_id: uuid::Uuid::new_v4().to_string(),
            device_id: me.to_string(),
        }),
        (None, Some(_)) => State(Unavailable(MarkerMissing)),
        (Some(m), _) if m.device_id != me => State(Unavailable(OtherDevice {
            device_id: m.device_id,
        })),
        (Some(m), None) => Adopt(m),
        (Some(m), Some(r)) if m.store_id != r.store_id => State(Unavailable(MarkerMismatch)),
        (Some(_), Some(_)) => State(if writable(root) { Available } else { ReadOnly }),
    }
}

/// Serializes the adoption of a Collaboration root's storage marker across
/// this process: the "no marker on disk yet, mint one and write it" step.
/// Every adopter goes through [`check_and_adopt`] — the designation
/// (`scan_roots::set_collaboration_dir`), the lazy mount the live runtime's
/// first start runs (`collab_exchange::ensure_collab_store`), the mount at
/// bind, and a live session's [`StoreGuard`]. Without it two of them could
/// both find no marker, each mint its own store id and each record its own,
/// leaving the catalog naming a store id the disk does not carry: a
/// permanent `MarkerMismatch` that also refuses a re-designation of the same
/// folder. Held only around the filesystem decision and write — never
/// across an await, a mount or a catalog write — and taken only on the
/// adoption path, so a plain check of a recorded marker never waits on it.
static ADOPTION: Mutex<()> = Mutex::new(());

fn adoption_lock(root: &Path) -> std::sync::MutexGuard<'static, ()> {
    match ADOPTION.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::WouldBlock) => {
            #[cfg(test)]
            test_hooks::on_adoption_wait(root);
            ADOPTION.lock().unwrap_or_else(|poisoned| {
                tracing::warn!(path = %root.display(), "storage marker adoption lock poisoned; continuing");
                poisoned.into_inner()
            })
        }
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            tracing::warn!(path = %root.display(), "storage marker adoption lock poisoned; continuing");
            poisoned.into_inner()
        }
    }
}

/// [`check_store`], with an adoption resolved to a state: a marker already
/// on disk is adopted as it is (never overwritten; the write probe still
/// decides `Available` vs `ReadOnly`), a missing one is minted and written.
/// Returns the state and the adopted marker (`None` when nothing was
/// adopted, or the write failed). An adoption is decided under the
/// process-wide adoption lock, re-checking the disk once it is held, so
/// concurrent adopters of one root all adopt the ONE marker the first of
/// them wrote.
pub(crate) fn check_and_adopt(
    root: &Path,
    recorded: Option<&StoreMarker>,
    me: &str,
) -> (StoreState, Option<StoreMarker>) {
    if let CheckOutcome::State(s) = check_store(root, recorded, me) {
        return (s, None);
    }
    let _adopting = adoption_lock(root);
    match check_store(root, recorded, me) {
        CheckOutcome::State(s) => (s, None),
        CheckOutcome::Adopt(m) => match read_marker(root) {
            Ok(Some(_)) => {
                // Already on disk (a returning device, or another adopter
                // wrote it moments ago) — never overwrite it, but still run
                // the write probe: an existing marker does not itself prove
                // the folder is writable right now.
                let state = if writable(root) {
                    StoreState::Available
                } else {
                    StoreState::ReadOnly
                };
                (state, Some(m))
            }
            _ => {
                #[cfg(test)]
                test_hooks::before_mint(root);
                match write_marker(root, &m) {
                    Ok(()) => {
                        tracing::info!(store_id = %m.store_id, path = %root.display(), "collaboration store marker written");
                        (StoreState::Available, Some(m))
                    }
                    Err(e) => {
                        tracing::warn!(path = %root.display(), error = %e, "collaboration store marker could not be written");
                        (StoreState::ReadOnly, None)
                    }
                }
            }
        },
    }
}

/// Test hooks for the adoption race (`check_and_adopt`): pause the first
/// adopter of a root just before it writes a freshly minted marker, and
/// report another adopter of that root waiting for the adoption lock.
/// Keyed by root, so parallel tests never see each other's hooks.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Mutex, OnceLock};

    struct Hook {
        /// Taken by the first adopter to reach the mint: it reports itself
        /// paused, then blocks until released.
        pause: Option<(Sender<()>, Receiver<()>)>,
        waiting: Sender<()>,
    }

    /// One spelling per folder (a temp dir may be reached through a
    /// symlinked prefix).
    fn key(root: &Path) -> PathBuf {
        std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
    }

    fn hooks() -> &'static Mutex<HashMap<PathBuf, Hook>> {
        static HOOKS: OnceLock<Mutex<HashMap<PathBuf, Hook>>> = OnceLock::new();
        HOOKS.get_or_init(Default::default)
    }

    /// The test's side of an armed root.
    pub(crate) struct Armed {
        root: PathBuf,
        /// One message once the first adopter paused before its mint.
        pub paused: Receiver<()>,
        /// One message per adopter found waiting for the adoption lock.
        pub waiting: Receiver<()>,
        release: Sender<()>,
    }

    impl Armed {
        /// Let the paused adopter write its marker.
        pub(crate) fn release(&self) {
            let _ = self.release.send(());
        }
    }

    impl Drop for Armed {
        fn drop(&mut self) {
            let _ = self.release.send(());
            hooks()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.root);
        }
    }

    pub(crate) fn arm(root: &Path) -> Armed {
        let (paused_tx, paused) = channel();
        let (release, release_rx) = channel();
        let (waiting_tx, waiting) = channel();
        hooks().lock().unwrap_or_else(|p| p.into_inner()).insert(
            key(root),
            Hook {
                pause: Some((paused_tx, release_rx)),
                waiting: waiting_tx,
            },
        );
        Armed {
            root: key(root),
            paused,
            waiting,
            release,
        }
    }

    pub(super) fn before_mint(root: &Path) {
        let pause = hooks()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(&key(root))
            .and_then(|h| h.pause.take());
        if let Some((paused, release)) = pause {
            let _ = paused.send(());
            let _ = release.recv();
        }
    }

    pub(super) fn on_adoption_wait(root: &Path) {
        if let Some(h) = hooks()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key(root))
        {
            let _ = h.waiting.send(());
        }
    }
}

/// The marker check a scan of the root runs FIRST (spec §9.1: "the marker
/// is checked before every … scan of the root") — for a caller that holds
/// only the catalog connection (the scanner), not this device's id. `None`
/// = the scan may proceed. `recorded` is what the catalog recorded for THIS
/// root (`None` when nothing is recorded for it yet — a wave-2 root not yet
/// adopted — in which case only the path itself is checked). A recorded
/// marker that is missing on disk, names another store, or names another
/// device refuses: an unmounted or swapped disk must never be reconciled
/// (its missing files would be misread as moves or deletions). Never writes
/// anything, not even the write probe.
pub fn scan_refusal(root: &Path, recorded: Option<&StoreMarker>) -> Option<UnavailableReason> {
    use UnavailableReason::*;
    if !root.exists() {
        return Some(PathMissing);
    }
    if !root.is_dir() {
        return Some(NotADirectory);
    }
    let recorded = recorded?;
    match read_marker(root) {
        Ok(None) => Some(MarkerMissing),
        Ok(Some(m)) if m.store_id != recorded.store_id => Some(MarkerMismatch),
        Ok(Some(m)) if m.device_id != recorded.device_id => Some(OtherDevice {
            device_id: m.device_id,
        }),
        Ok(Some(_)) => None,
        Err(e) => {
            tracing::warn!(path = %root.display(), error = %e, "collaboration store marker unreadable");
            Some(MarkerMissing)
        }
    }
}

pub fn offer_flags(
    last_seen: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> (Option<i64>, bool, bool) {
    match last_seen {
        None => (None, true, false),
        Some(t) => {
            let off = now - t;
            (
                Some(off.num_days()),
                off > REPLACE_PROMPT_AFTER,
                off > RETIRE_PROPOSAL_AFTER,
            )
        }
    }
}

pub struct StoreGuard {
    root: PathBuf,
    me: String,
    recorded: Mutex<Option<StoreMarker>>,
    adoption: Mutex<Option<StoreMarker>>,
    state: RwLock<StoreState>,
}

impl StoreGuard {
    pub fn new(root: PathBuf, me: String, recorded: Option<StoreMarker>) -> Self {
        Self {
            root,
            me,
            recorded: Mutex::new(recorded),
            adoption: Mutex::new(None),
            state: RwLock::new(StoreState::Unavailable(UnavailableReason::MarkerMissing)),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state(&self) -> StoreState {
        self.state.read().expect("store guard poisoned").clone()
    }

    pub fn take_adoption(&self) -> Option<StoreMarker> {
        self.adoption.lock().expect("store guard poisoned").take()
    }

    /// The marker check WITHOUT the write probe — a landing's precondition
    /// (Task 15 R4: the live session owns ONE guard and no landing writes a
    /// probe file). Path and marker as [`Self::check_now`]; writability as
    /// the last full check found it (the storage engine runs one every few
    /// seconds). With nothing recorded yet (an adoption is due) it falls
    /// back to the full check.
    pub fn check_marker(&self) -> StoreState {
        let recorded = self.recorded.lock().expect("store guard poisoned").clone();
        if recorded.is_none() {
            return self.check_now();
        }
        let last = self.state();
        let next = match scan_refusal(&self.root, recorded.as_ref()) {
            Some(reason) => StoreState::Unavailable(reason),
            None if last == StoreState::ReadOnly => StoreState::ReadOnly,
            None => StoreState::Available,
        };
        if next != last {
            // Only the unavailable direction is decided here; a store coming
            // back is confirmed (probe included) by the next full check.
            if let StoreState::Unavailable(_) = next {
                tracing::warn!(path = %self.root.display(), state = ?next, "collaboration storage not available");
                *self.state.write().expect("store guard poisoned") = next.clone();
            }
        }
        next
    }

    pub fn check_now(&self) -> StoreState {
        let recorded = self.recorded.lock().expect("store guard poisoned").clone();
        let (next, adopted) = check_and_adopt(&self.root, recorded.as_ref(), &self.me);
        if let Some(m) = adopted {
            *self.recorded.lock().expect("store guard poisoned") = Some(m.clone());
            *self.adoption.lock().expect("store guard poisoned") = Some(m);
        }
        let mut cur = self.state.write().expect("store guard poisoned");
        if *cur != next {
            match &next {
                StoreState::Available => {
                    tracing::info!(path = %self.root.display(), "collaboration storage available")
                }
                other => {
                    tracing::warn!(path = %self.root.display(), state = ?other, "collaboration storage not available")
                }
            }
            *cur = next.clone();
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(store: &str, dev: &str) -> StoreMarker {
        StoreMarker {
            store_id: store.into(),
            device_id: dev.into(),
        }
    }

    #[test]
    fn missing_path_and_file_root_are_unavailable_and_never_recreated() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        assert_eq!(
            check_store(&gone, Some(&m("s", "ME")), "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::PathMissing))
        );
        assert!(!gone.exists());
        let file = tmp.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            check_store(&file, Some(&m("s", "ME")), "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::NotADirectory))
        );
    }

    #[test]
    fn marker_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // nothing recorded yet (a wave-2 root): adopt, write a marker naming me
        let CheckOutcome::Adopt(new) = check_store(root, None, "ME") else {
            panic!("adopt")
        };
        assert_eq!(new.device_id, "ME");
        write_marker(root, &new).unwrap();
        assert_eq!(read_marker(root).unwrap(), Some(new.clone()));
        assert_eq!(
            check_store(root, Some(&new), "ME"),
            CheckOutcome::State(StoreState::Available)
        );
        // another disk mounted at the same path
        assert_eq!(
            check_store(root, Some(&m("other-store", "ME")), "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::MarkerMismatch))
        );
        // a recorded store whose marker vanished
        std::fs::remove_file(root.join(MARKER_REL)).unwrap();
        assert_eq!(
            check_store(root, Some(&new), "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::MarkerMissing))
        );
        // another device's root (two machines on one NAS folder, or a reinstall)
        write_marker(root, &m(&new.store_id, "OTHER")).unwrap();
        assert_eq!(
            check_store(root, Some(&new), "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::OtherDevice {
                device_id: "OTHER".into()
            }))
        );
        assert_eq!(
            check_store(root, None, "ME"),
            CheckOutcome::State(StoreState::Unavailable(UnavailableReason::OtherDevice {
                device_id: "OTHER".into()
            }))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_root_still_serves() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mk = m("s", "ME");
        write_marker(root, &mk).unwrap();
        let athenaeum = root.join(".athenaeum");
        std::fs::set_permissions(&athenaeum, std::fs::Permissions::from_mode(0o555)).unwrap();
        let got = check_store(root, Some(&mk), "ME");
        std::fs::set_permissions(&athenaeum, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(got, CheckOutcome::State(StoreState::ReadOnly));
        assert!(StoreState::ReadOnly.serving() && !StoreState::ReadOnly.fetching());
    }

    #[test]
    fn a_scan_is_refused_on_an_unmounted_or_swapped_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mk = m("s", "ME");
        assert_eq!(
            scan_refusal(&root.join("gone"), Some(&mk)),
            Some(UnavailableReason::PathMissing)
        );
        // nothing recorded for this root yet: only the path is checked
        assert_eq!(scan_refusal(root, None), None);
        assert_eq!(
            scan_refusal(root, Some(&mk)),
            Some(UnavailableReason::MarkerMissing)
        );
        write_marker(root, &mk).unwrap();
        assert_eq!(scan_refusal(root, Some(&mk)), None);
        assert_eq!(
            scan_refusal(root, Some(&m("other", "ME"))),
            Some(UnavailableReason::MarkerMismatch)
        );
        write_marker(root, &m("s", "OTHER")).unwrap();
        assert_eq!(
            scan_refusal(root, Some(&mk)),
            Some(UnavailableReason::OtherDevice {
                device_id: "OTHER".into()
            })
        );
    }

    #[test]
    fn replace_prompt_after_seven_days_and_retire_proposal_after_thirty() {
        let now = chrono::Utc::now();
        assert_eq!(
            offer_flags(Some(now - chrono::Duration::days(2)), now),
            (Some(2), false, false)
        );
        assert_eq!(
            offer_flags(Some(now - chrono::Duration::days(8)), now),
            (Some(8), true, false)
        );
        assert_eq!(
            offer_flags(Some(now - chrono::Duration::days(31)), now),
            (Some(31), true, true)
        );
        assert_eq!(offer_flags(None, now), (None, true, false));
    }

    #[test]
    fn the_guard_records_state_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = m("s", "ME");
        write_marker(tmp.path(), &mk).unwrap();
        let g = StoreGuard::new(tmp.path().to_path_buf(), "ME".into(), Some(mk));
        assert_eq!(g.check_now(), StoreState::Available);
        std::fs::remove_file(tmp.path().join(MARKER_REL)).unwrap();
        assert_eq!(
            g.check_now(),
            StoreState::Unavailable(UnavailableReason::MarkerMissing)
        );
        assert_eq!(
            g.state(),
            StoreState::Unavailable(UnavailableReason::MarkerMissing)
        );
    }

    /// Fix round 1 (folded minor): adopting a marker that already exists on
    /// disk and already names `me` (recorded is `None` — a returning device)
    /// still runs the write probe. Read-only must not silently read back as
    /// `Available`.
    #[cfg(unix)]
    #[test]
    fn the_guard_adoption_of_an_existing_marker_runs_the_write_probe() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_marker(root, &m("s", "ME")).unwrap();
        let athenaeum = root.join(".athenaeum");
        std::fs::set_permissions(&athenaeum, std::fs::Permissions::from_mode(0o555)).unwrap();
        let g = StoreGuard::new(root.to_path_buf(), "ME".into(), None);
        let got = g.check_now();
        std::fs::set_permissions(&athenaeum, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(got, StoreState::ReadOnly);
        assert!(
            g.take_adoption().is_some(),
            "still adopted the existing marker"
        );
    }
}
