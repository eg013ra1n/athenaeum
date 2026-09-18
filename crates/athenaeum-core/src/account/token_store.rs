//! Device-token storage (task B4).
//!
//! The hub device token is a bearer credential — it is stored in the OS keychain
//! (service `com.vsharifov.athenaeum`, account = the hub host so multiple hubs
//! coexist) and **never** in the catalog DB, logs, or any error message.
//!
//! Backend selection:
//! - **macOS / Windows** — the native keychain (`keyring` 4 with its `v1`
//!   compatibility surface, which selects the `apple-native-keyring-store` /
//!   `windows-native-keyring-store` backends). If a keychain call fails at
//!   runtime (locked, sandbox denial), it transparently falls back to the 0600
//!   file.
//! - **Linux / everything else** — a 0600 file directly (the `keyring`
//!   dependency is target-scoped to mac/windows in `Cargo.toml`, so no
//!   secret-service/dbus backend is ever compiled and there is no reliable
//!   native store; the web/Docker and headless builds run here). This is the
//!   documented file-0600 fallback (Perseus pattern).
//!
//! [`TokenStore::file_only`] forces the file backend regardless of platform —
//! used by tests (so they never touch the real login keychain / trigger a
//! prompt) and available to any headless shell that wants to opt out of the
//! keychain.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};

/// Keychain service name shared by all hubs (account = hub host disambiguates).
pub const KEYRING_SERVICE: &str = "com.vsharifov.athenaeum";

/// How long [`TokenStore::load`] waits on a keychain lookup before falling
/// back to the 0600 file. A prompt-blocked `SecKeychainFindGenericPassword`
/// (an unsigned release binary, nobody at the keyboard to answer the OS
/// permission dialog) hangs indefinitely — this bounds every caller's wait to
/// a few seconds no matter how stuck the underlying call is.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Outcome of [`bounded_lookup`].
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(crate) enum LookupOutcome {
    /// The lookup — this call's own spawn, or one already running for the
    /// same key — produced a result within the timeout.
    Done(Result<Option<String>, String>),
    /// Still running past the timeout. The lookup keeps going in the
    /// background; the NEXT call for this key (whenever it comes) picks up
    /// its result instead of spawning a second one.
    Pending,
}

/// One in-flight (or just-finished, not yet collected) keychain lookup.
struct Probe {
    result: Mutex<Option<Result<Option<String>, String>>>,
    cv: Condvar,
}

/// Process-wide table of in-flight probes, keyed by `"<service>/<account>"`.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn probes() -> &'static Mutex<HashMap<String, Arc<Probe>>> {
    static PROBES: OnceLock<Mutex<HashMap<String, Arc<Probe>>>> = OnceLock::new();
    PROBES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Run `lookup` for `key`, waiting up to `timeout` for a result.
///
/// At most one `std::thread` (named `keychain-probe`) ever runs `lookup` for
/// a given `key` at a time: if a lookup for `key` is already in flight, this
/// call waits on ITS result instead of spawning a second thread. A call that
/// times out leaves the probe registered — the still-running thread's
/// eventual result is picked up by whichever call (this one retried, or a
/// fresh one) asks for `key` next. This bounds a process to at most one stuck
/// thread per key, no matter how many times a caller polls while the
/// underlying lookup (e.g. a blocked OS keychain prompt) never returns.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(crate) fn bounded_lookup(
    key: &str,
    timeout: Duration,
    lookup: impl FnOnce() -> Result<Option<String>, String> + Send + 'static,
) -> LookupOutcome {
    let table = probes();
    let (probe, spawned) = {
        let mut guard = table.lock().unwrap();
        if let Some(existing) = guard.get(key) {
            (existing.clone(), false)
        } else {
            let probe = Arc::new(Probe {
                result: Mutex::new(None),
                cv: Condvar::new(),
            });
            guard.insert(key.to_string(), probe.clone());
            (probe, true)
        }
    };

    if spawned {
        let probe = probe.clone();
        std::thread::Builder::new()
            .name("keychain-probe".into())
            .spawn(move || {
                let result = lookup();
                *probe.result.lock().unwrap() = Some(result);
                probe.cv.notify_all();
            })
            .expect("spawn keychain-probe thread");
    }

    let guard = probe.result.lock().unwrap();
    let (guard, timed_out) = probe
        .cv
        .wait_timeout_while(guard, timeout, |r| r.is_none())
        .unwrap();
    let done = guard.clone();
    drop(guard);

    match done {
        Some(result) => {
            // Collected — the next call for this key starts a fresh probe.
            table.lock().unwrap().remove(key);
            LookupOutcome::Done(result)
        }
        None => {
            debug_assert!(timed_out.timed_out());
            LookupOutcome::Pending
        }
    }
}

/// Stores/loads the hub device token for one account (hub host).
pub struct TokenStore {
    /// Keychain account / file discriminator — the hub host. Read only by the
    /// keychain backend, which is compiled out on non-mac/win targets — hence
    /// the target-scoped `dead_code` allowance (Linux/headless builds warn
    /// otherwise; the field must still exist so constructors are portable).
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    account: String,
    /// 0600 file used as the fallback (or the sole backend under `file_only`).
    fallback_path: PathBuf,
    /// When true, never touch the OS keychain — file backend only. Same
    /// target-scoped allowance as `account`: only the keychain backend reads it.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    file_only: bool,
}

impl TokenStore {
    /// Keychain-backed store (with the file as automatic fallback on error /
    /// unsupported platform).
    pub fn new(account: impl Into<String>, fallback_path: PathBuf) -> Self {
        Self { account: account.into(), fallback_path, file_only: false }
    }

    /// File-only store — no OS keychain. For tests and headless/CI shells where
    /// a keychain is unavailable or inappropriate.
    pub fn file_only(account: impl Into<String>, path: PathBuf) -> Self {
        Self { account: account.into(), fallback_path: path, file_only: true }
    }

    /// Persist the token. Prefers the keychain (mac/win); falls back to the
    /// 0600 file. Never logs the token.
    pub fn store(&self, token: &str) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if !self.file_only {
            match self.keyring_entry().and_then(|e| Ok(e.set_password(token)?)) {
                Ok(()) => return Ok(()),
                Err(e) => tracing::warn!(
                    account = %self.account,
                    error = %e,
                    "keychain store failed; using 0600 file fallback"
                ),
            }
        }
        self.file_store(token)
    }

    /// Load the token, or `None` when signed out. On mac/win an empty keychain
    /// is authoritative (`None`); the file is consulted only when the keychain
    /// itself is unavailable — or when the lookup is still stuck behind a
    /// blocked OS permission prompt past [`KEYCHAIN_TIMEOUT`] (see
    /// [`bounded_lookup`]), so a caller never hangs indefinitely on this call.
    pub fn load(&self) -> Result<Option<String>> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if !self.file_only {
            match self.keyring_entry() {
                Ok(entry) => {
                    let key = format!("{KEYRING_SERVICE}/{}", self.account);
                    match bounded_lookup(&key, KEYCHAIN_TIMEOUT, move || {
                        match entry.get_password() {
                            Ok(token) => Ok(Some(token)),
                            Err(keyring::Error::NoEntry) => Ok(None),
                            Err(e) => Err(e.to_string()),
                        }
                    }) {
                        LookupOutcome::Done(Ok(token)) => return Ok(token),
                        LookupOutcome::Done(Err(e)) => tracing::warn!(
                            account = %self.account,
                            error = %e,
                            "keychain load failed; consulting 0600 file fallback"
                        ),
                        LookupOutcome::Pending => tracing::warn!(
                            account = %self.account,
                            timeout_ms = KEYCHAIN_TIMEOUT.as_millis() as u64,
                            "keychain lookup pending — a permission prompt may be waiting; consulting 0600 file fallback"
                        ),
                    }
                }
                Err(e) => tracing::warn!(
                    account = %self.account,
                    error = %e,
                    "keychain unavailable; consulting 0600 file fallback"
                ),
            }
        }
        self.file_load()
    }

    /// Remove the token from every backend (keychain + file). Idempotent.
    pub fn delete(&self) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if !self.file_only {
            if let Ok(entry) = self.keyring_entry() {
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(e) => tracing::warn!(
                        account = %self.account,
                        error = %e,
                        "keychain delete failed"
                    ),
                }
            }
        }
        self.file_delete()
    }

    /// Build the keychain entry for this account (service + hub host).
    ///
    /// keyring 4 installs the platform store into a process-global slot the
    /// first time an entry is built, and it flips its "installed" flag *before*
    /// registering the store — a second thread entering that window gets
    /// `NoDefaultStore`, which [`Self::load`] would read as "keychain
    /// unavailable" and answer from the (normally absent) file fallback, i.e. a
    /// spurious signed-out. keyring 3 had no such window (`OnceLock`), so we
    /// close it ourselves: the first construction in the process is serialized,
    /// every later one is uncontended. Building an entry is a pure value
    /// construction — no keychain I/O — so doing it twice on that first call
    /// costs nothing.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn keyring_entry(&self) -> Result<keyring::Entry, keyring::Error> {
        static STORE_INIT: std::sync::Once = std::sync::Once::new();
        STORE_INIT.call_once(|| {
            if let Err(e) = keyring::Entry::new(KEYRING_SERVICE, &self.account) {
                tracing::debug!(error = %e, "keychain store init probe failed");
            }
        });
        keyring::Entry::new(KEYRING_SERVICE, &self.account)
    }

    // ── File-0600 backend ───────────────────────────────────────────────────

    fn file_store(&self, token: &str) -> Result<()> {
        if let Some(parent) = self.fallback_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create token dir {}", parent.display()))?;
        }
        write_secret_0600(&self.fallback_path, token.as_bytes())
    }

    fn file_load(&self) -> Result<Option<String>> {
        if !self.fallback_path.exists() {
            return Ok(None);
        }
        #[cfg(unix)]
        tighten_permissions_if_needed(&self.fallback_path)?;
        let bytes = std::fs::read(&self.fallback_path)
            .with_context(|| format!("read token file {}", self.fallback_path.display()))?;
        let token = String::from_utf8(bytes)
            .with_context(|| format!("token file {} not utf-8", self.fallback_path.display()))?;
        let token = token.trim().to_string();
        Ok(if token.is_empty() { None } else { Some(token) })
    }

    fn file_delete(&self) -> Result<()> {
        match std::fs::remove_file(&self.fallback_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e)
                .with_context(|| format!("delete token file {}", self.fallback_path.display())),
        }
    }
}

#[cfg(unix)]
fn write_secret_0600(path: &std::path::Path, secret: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // Truncate-or-create at 0600 (the token can be re-issued, unlike the device
    // key which is create_new).
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create token file {}", path.display()))?;
    f.write_all(secret)
        .with_context(|| format!("write token file {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_0600(path: &std::path::Path, secret: &[u8]) -> Result<()> {
    std::fs::write(path, secret)
        .with_context(|| format!("write token file {}", path.display()))
}

#[cfg(unix)]
fn tighten_permissions_if_needed(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta =
        std::fs::metadata(path).with_context(|| format!("stat token file {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("tighten token file permissions {}", path.display()))?;
        tracing::warn!(path = %path.display(), old_mode = format!("{mode:o}"), "token file permissions tightened");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip through the file backend. We force `file_only` so the test
    /// never touches the real login keychain (which would prompt on a dev Mac)
    /// and so it passes in headless/CI shells where no keychain exists — the
    /// same fallback the production store uses on Linux.
    #[test]
    fn token_store_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            TokenStore::file_only("projects.artfrom.space", dir.path().join("token_host"));

        assert!(store.load().unwrap().is_none(), "empty store loads None");

        store.store("dev-token-abc123").unwrap();
        assert_eq!(store.load().unwrap().as_deref(), Some("dev-token-abc123"));

        // Overwrite (re-issued token) works.
        store.store("dev-token-xyz789").unwrap();
        assert_eq!(store.load().unwrap().as_deref(), Some("dev-token-xyz789"));

        store.delete().unwrap();
        assert!(store.load().unwrap().is_none(), "deleted store loads None");
        // Delete is idempotent.
        store.delete().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token_host");
        let store = TokenStore::file_only("host", path.clone());
        store.store("secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be 0600");
    }

    // ── bounded_lookup ───────────────────────────────────────────────────
    //
    // Pure std::thread/Mutex/Condvar plumbing — no real keychain involved,
    // so these run on every platform/CI shell, not just mac/win.

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn bounded_lookup_returns_the_result_when_the_lookup_is_fast() {
        let outcome = bounded_lookup("bl-test/fast", Duration::from_millis(200), || {
            Ok(Some("fast-value".to_string()))
        });
        match outcome {
            LookupOutcome::Done(Ok(Some(v))) => assert_eq!(v, "fast-value"),
            _ => panic!("expected Done(Ok(Some(\"fast-value\")))"),
        }
    }

    #[test]
    fn bounded_lookup_reports_pending_on_a_slow_lookup_and_spawns_no_second_thread() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_lookup = calls.clone();

        let first = bounded_lookup("bl-test/slow", Duration::from_millis(50), move || {
            calls_for_lookup.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(500));
            Ok(Some("slow-value".to_string()))
        });
        assert!(matches!(first, LookupOutcome::Pending));

        // A second caller arriving while the first is still stuck must NOT
        // spawn a second thread — its own closure must never run.
        let second = bounded_lookup("bl-test/slow", Duration::from_millis(50), || {
            panic!("must not run — a probe is already pending for this key");
        });
        assert!(matches!(second, LookupOutcome::Pending));

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the slow lookup must have run exactly once"
        );

        // Let the background thread finish so it doesn't outlive the test
        // process in a way that could race a later test reusing the map.
        std::thread::sleep(Duration::from_millis(500));
    }

    #[test]
    fn bounded_lookup_hands_the_late_result_to_the_next_caller() {
        let first = bounded_lookup("bl-test/late", Duration::from_millis(20), || {
            std::thread::sleep(Duration::from_millis(100));
            Ok(Some("late-value".to_string()))
        });
        assert!(matches!(first, LookupOutcome::Pending));

        // Give the background thread time to finish well past the first
        // caller's timeout.
        std::thread::sleep(Duration::from_millis(200));

        let second = bounded_lookup("bl-test/late", Duration::from_millis(200), || {
            panic!("must not run — the first thread's result should be reused");
        });
        match second {
            LookupOutcome::Done(Ok(Some(v))) => assert_eq!(v, "late-value"),
            _ => panic!("expected Done(Ok(Some(\"late-value\"))) reused from the first thread"),
        }

        // The probe was consumed by the call above — the map no longer holds
        // the key, so the NEXT call spawns a fresh lookup.
        let ran = Arc::new(AtomicBool::new(false));
        let ran_for_lookup = ran.clone();
        let third = bounded_lookup("bl-test/late", Duration::from_millis(200), move || {
            ran_for_lookup.store(true, Ordering::SeqCst);
            Ok(None)
        });
        assert!(
            ran.load(Ordering::SeqCst),
            "map must no longer hold the key after the Done above"
        );
        assert!(matches!(third, LookupOutcome::Done(Ok(None))));
    }
}
