//! The one device identity (task B4, spec D-5).
//!
//! An Athenaeum install has exactly **one** cryptographic identity: an iroh
//! ed25519 [`SecretKey`] persisted at `<sync_dir>/device_key`. It is reused by:
//!
//! - the **sync transport** — the iroh endpoint binds this key, so its node id
//!   (== ed25519 public key) is the peer address other devices dial
//!   ([`crate::sharing::iroh::IrohTransport::new`]);
//! - the **account layer** — the same public key, base64-encoded, is the
//!   `devicePubkey` registered with the hub on sign-in.
//!
//! There is one key format and one file. [`crate::sync::receiver`] loads the
//! transport secret through [`DeviceKey`] so a second identity can never be
//! minted. The on-disk format + 0600 handling mirror the Perseus loader.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use iroh::SecretKey;

/// Filename of the shared device identity key inside the sync data dir.
pub const DEVICE_KEY_FILENAME: &str = "device_key";

/// Path of the shared device key relative to a sync data dir.
pub fn device_key_path(sync_dir: &Path) -> PathBuf {
    sync_dir.join(DEVICE_KEY_FILENAME)
}

/// Sidecar lock file guarding the device identity. The lock must NOT be on
/// `device_key` itself: fd-lock maps to `LockFileEx` on Windows, a MANDATORY
/// lock that fails every other handle's read of the key (os error 33) —
/// including account sign-in — for as long as the iroh node holds it. Unix
/// `flock` is advisory, which is why this only broke on Windows.
pub fn device_key_lock_path(sync_dir: &Path) -> PathBuf {
    sync_dir.join("device_key.lock")
}

/// The persisted device identity. Cheap to clone (32 bytes).
#[derive(Clone)]
pub struct DeviceKey {
    secret: [u8; 32],
}

impl DeviceKey {
    /// Load the persisted key at `path`, creating it (mode 0600 on unix) on
    /// first run, and tightening a group/world-readable file back to 0600. This
    /// is the exact secret the iroh transport binds from — same file, same
    /// format.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let secret = load_or_create_device_key(path)?;
        Ok(Self { secret })
    }

    /// Convenience: load-or-create at `<sync_dir>/device_key`, creating the
    /// directory first.
    pub fn load_or_create_in(sync_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(sync_dir)
            .with_context(|| format!("create sync dir {}", sync_dir.display()))?;
        Self::load_or_create(&device_key_path(sync_dir))
    }

    /// The raw 32-byte secret, as consumed by
    /// [`IrohTransport::new`](crate::sharing::iroh::IrohTransport::new).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret
    }

    /// The iroh [`SecretKey`].
    pub fn secret_key(&self) -> SecretKey {
        SecretKey::from_bytes(&self.secret)
    }

    /// The 32-byte public key == iroh node id. Byte-identical to
    /// `endpoint.id().as_bytes()` for an endpoint bound from this secret.
    pub fn node_id(&self) -> [u8; 32] {
        *self.secret_key().public().as_bytes()
    }

    /// Standard-base64 of the 32-byte public key — the `devicePubkey` the hub
    /// verify endpoint expects.
    pub fn pubkey_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.node_id())
    }

    /// Lowercase-hex node id, for logs/display.
    pub fn node_id_hex(&self) -> String {
        self.node_id().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Actionable, key-material-free message for a failed device-key lock (S2). It
/// names the two real causes (a copied identity or a double launch) and the
/// remedy, printing only the path — never any secret bytes.
fn device_key_in_use_message(path: &Path) -> String {
    format!(
        "device key at {} is in use by another process (copied key or double launch) — each install needs its own identity",
        path.display()
    )
}

/// A held, process-lifetime advisory lock on the [`device_key_lock_path`]
/// sidecar (iroh hardening I4). The lock is on the sidecar, never on
/// `device_key` itself: fd-lock is `LockFileEx` on Windows — a MANDATORY lock
/// that would fail every other handle's read of the key (os error 33,
/// including account sign-in) for as long as the node holds it. Taken at
/// [`SharedIrohNode`](crate::sharing::iroh::node) bind so a second install
/// started from a **copied** key — or a double launch of the same install —
/// fails loudly at startup instead of silently dueling with the original on the
/// relay (one relay slot per node id). Dropping it (node shutdown) releases the
/// OS lock so a later re-bind can re-acquire it.
///
/// # How the lock is held
///
/// [`fd_lock::RwLock::try_write`] returns a guard borrowing the lock, and it is
/// the guard's `Drop` that calls `flock(LOCK_UN)`. To hold the lock for this
/// struct's whole lifetime we take the guard, [`std::mem::forget`] it (so
/// `LOCK_UN` never runs), and keep the [`fd_lock::RwLock`] — which owns the open
/// `File` — alive in `_lock`. The OS `flock` is bound to the open file
/// description, so it is released exactly when this struct drops and the `File`
/// closes. `fd_lock::RwLock<T>` has no `Drop` of its own, so nothing else can
/// touch the lock. No `unsafe`, no raw pointers, no leak (the node may re-bind).
pub struct DeviceKeyLock {
    /// Owns the open sidecar `File` whose file-description carries the `flock`.
    /// The lock stays held for as long as this field is alive (its guard was
    /// `mem::forget`-ted, so no `LOCK_UN` runs); closing the `File` on drop is
    /// what releases the OS lock.
    _lock: fd_lock::RwLock<std::fs::File>,
    /// The locked sidecar path, for the release log line (no secret material).
    path: PathBuf,
}

impl DeviceKeyLock {
    /// Acquire the exclusive advisory lock on `lock_path` — the
    /// [`device_key_lock_path`] sidecar, created here if it does not yet exist
    /// (the lock is NEVER taken on `device_key` itself, so its bytes stay
    /// readable while the node runs). A second holder — this process or any
    /// other — fails with an actionable, key-material-free error (I4/S2). A
    /// genuine I/O error (not contention) propagates with context, never
    /// masquerading as "in use".
    pub fn acquire(lock_path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .with_context(|| format!("open device key lock {}", lock_path.display()))?;
        let mut lock = fd_lock::RwLock::new(file);
        match lock.try_write() {
            Ok(guard) => {
                // Keep the OS lock for the struct's lifetime: forget the guard so
                // its `Drop` (the only caller of `flock(LOCK_UN)`) never runs.
                // The lock releases when `_lock` closes the file on struct drop.
                std::mem::forget(guard);
                tracing::debug!(path = %lock_path.display(), "device key lock acquired");
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                anyhow::bail!(device_key_in_use_message(lock_path))
            }
            Err(e) => {
                return Err(e).with_context(|| format!("lock device key {}", lock_path.display()));
            }
        }
        Ok(Self {
            _lock: lock,
            path: lock_path.to_path_buf(),
        })
    }
}

impl Drop for DeviceKeyLock {
    fn drop(&mut self) {
        // Log only. The OS lock is released by `_lock` closing its file as this
        // struct's fields drop after this method returns — nothing here touches
        // the lock.
        tracing::debug!(path = %self.path.display(), "device key lock released");
    }
}

/// Load the persisted 32-byte device secret, creating it (mode 0600 on unix) on
/// first run. The identity secret must never be group/world-readable — an
/// existing file with loose bits is tightened on load.
fn load_or_create_device_key(path: &Path) -> Result<[u8; 32]> {
    if path.exists() {
        return read_device_key(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create device key dir {}", parent.display()))?;
    }
    let secret = crate::sharing::iroh::random_secret();
    create_device_key_atomically(path, &secret)
}

/// Atomically create the device key at `path` with content `secret`,
/// arbitrating a concurrent first-run race (two callers — e.g.
/// `account_sign_in_verify` and the sync transport's `ensure_started` — both
/// calling `load_or_create` before either has written the file).
///
/// # Protocol
///
/// A plain `create_new` + `write_all` directly on `path` is NOT atomic: the
/// name becomes visible (0 bytes) at `create_new`, and the 32 secret bytes
/// land in a SEPARATE `write_all` syscall after. A concurrent loser whose
/// existence check lands in that window sees a real, present, but genuinely
/// incomplete file — reading it back trips the 32-byte validation as a hard
/// error. (This is not hypothetical: it is the exact root cause of a flake
/// this fn's previous version had under full-suite load/scheduling pressure.)
///
/// So instead: the FULL content is written to a private, uniquely-named temp
/// file in the same directory first — created 0600, then `sync_all`'d so it
/// is complete and durable on disk before anything else can observe it. ONLY
/// THEN is the final name claimed, via [`std::fs::hard_link`] (`tmp -> path`).
/// Hard-linking, like `create_new`, is atomic and fails with `AlreadyExists`
/// if `path` already exists — but unlike a `rename`, it never REPLACES an
/// existing target. That distinction is load-bearing: a naive tmp-then-RENAME
/// scheme is actively wrong here, because two concurrent first-runs would each
/// finish their own tmp file and then EACH rename would unconditionally
/// succeed, with the second one silently clobbering the first — two racing
/// processes would walk away disagreeing about which key is "the" identity.
/// `hard_link`'s fail-if-exists semantics keep exactly one winner: whichever
/// call lands first wins the name; every other call errors without touching
/// the winner's file. (`renameat2(RENAME_NOREPLACE)` gives the same
/// fail-if-exists guarantee as a plain rename, but is Linux-only, no macOS
/// equivalent; `hard_link` is the POSIX-portable primitive with that
/// guarantee, and is also supported on Windows.)
///
/// On loss (`AlreadyExists`), the loser's own tmp file is unlinked — it never
/// owned the final name — and it reads `path` back instead. That read is now
/// genuinely, unconditionally complete: the winner's `hard_link` could only
/// have succeeded after its tmp file's content was fully written and synced,
/// so there is no window in which `path` exists but is incomplete. The 32-byte
/// validation in [`read_device_key`] therefore stays a hard error — it is now
/// truly impossible to hit here, not a race to retry around.
fn create_device_key_atomically(path: &Path, secret: &[u8; 32]) -> Result<[u8; 32]> {
    let tmp = tmp_path_for(path);
    let write_result = write_new_key_file(&tmp, secret);
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    match std::fs::hard_link(&tmp, path) {
        Ok(()) => {
            // Two links to the same inode now (`tmp` and `path`); drop ours —
            // `path` is all that matters going forward.
            let _ = std::fs::remove_file(&tmp);
            tracing::info!(path = %path.display(), "generated new device key");
            Ok(*secret)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&tmp);
            tracing::debug!(
                path = %path.display(),
                "device key created concurrently by another caller; reading back"
            );
            read_device_key(path)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e).with_context(|| format!("claim device key {}", path.display()))
        }
    }
}

/// A unique, sibling temp-file path for `path` (same directory, required for
/// [`std::fs::hard_link`] — hard links cannot cross filesystems, and a sibling
/// path is guaranteed to be on the same one). Mirrors the pid+sequence
/// unique-suffix pattern used by `fits_writer`'s atomic-rename writer: the
/// pid disambiguates across processes, the monotonic per-process counter
/// disambiguates concurrent threads within one process (exactly what the
/// concurrency test below hammers).
fn tmp_path_for(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{file_name}.tmp.{}.{}", std::process::id(), seq))
}

/// Read + validate an existing device key file, tightening loose permissions
/// first (unix). Shared by the normal load path and the first-run-race
/// read-back in [`load_or_create_device_key`].
fn read_device_key(path: &Path) -> Result<[u8; 32]> {
    #[cfg(unix)]
    tighten_permissions_if_needed(path)?;
    let bytes =
        std::fs::read(path).with_context(|| format!("read device key {}", path.display()))?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "device key {} is {} bytes, expected 32 — delete it to regenerate",
            path.display(),
            bytes.len()
        )
    })?;
    Ok(arr)
}

#[cfg(unix)]
fn tighten_permissions_if_needed(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta =
        std::fs::metadata(path).with_context(|| format!("stat device key {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("tighten device key permissions {}", path.display()))?;
        tracing::warn!(path = %path.display(), old_mode = format!("{mode:o}"), "device key permissions tightened");
    }
    Ok(())
}

/// Create `path` (a tmp file — always a fresh, uniquely-named path, so
/// `create_new` here is just defense-in-depth, not the race arbiter), write
/// the full secret, and `sync_all` before returning — so by the time the
/// caller hard-links it into place, its content is complete and durable.
#[cfg(unix)]
fn write_new_key_file(path: &Path, secret: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create device key tmp {}", path.display()))?;
    f.write_all(secret)
        .with_context(|| format!("write device key tmp {}", path.display()))?;
    f.sync_all()
        .with_context(|| format!("sync device key tmp {}", path.display()))?;
    Ok(())
}

#[cfg(windows)]
fn write_new_key_file(path: &Path, secret: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create device key tmp {}", path.display()))?;
    f.write_all(secret)
        .with_context(|| format!("write device key tmp {}", path.display()))?;
    f.sync_all()
        .with_context(|| format!("sync device key tmp {}", path.display()))?;
    drop(f);
    // Windows has no chmod; restrict the private identity key to the current
    // user so it is not readable by other accounts on a shared host (finding
    // L2). Best-effort — the key already exists and the account still works if
    // this fails; only the hardening is skipped.
    restrict_windows_acl(path);
    Ok(())
}

/// Best-effort owner-only ACL on the device key (finding L2, Windows). Uses
/// `icacls` (always present on Windows) to disable ACL inheritance and grant
/// only the current user full control, dropping any inherited group/Everyone
/// access. Never fails key creation — logs and continues on any error.
#[cfg(windows)]
fn restrict_windows_acl(path: &Path) {
    let user = std::env::var("USERNAME").unwrap_or_default();
    if user.is_empty() {
        tracing::warn!(path = %path.display(), "cannot resolve USERNAME to restrict device key ACL");
        return;
    }
    match std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:F"))
        .output()
    {
        Ok(out) if out.status.success() => {
            tracing::debug!(path = %path.display(), "device key ACL restricted to current user");
        }
        Ok(out) => {
            tracing::warn!(path = %path.display(), code = ?out.status.code(), "icacls did not restrict device key ACL");
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to run icacls to restrict device key ACL");
        }
    }
}

#[cfg(all(not(unix), not(windows)))]
fn write_new_key_file(path: &Path, secret: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create device key tmp {}", path.display()))?;
    f.write_all(secret)
        .with_context(|| format!("write device key tmp {}", path.display()))?;
    f.sync_all()
        .with_context(|| format!("sync device key tmp {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_persisted_once_stable_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = device_key_path(dir.path());

        // First load creates it; second load reads the SAME bytes back.
        let k1 = DeviceKey::load_or_create(&path).unwrap();
        assert!(path.exists(), "device key must be persisted on first load");
        let k2 = DeviceKey::load_or_create(&path).unwrap();

        assert_eq!(k1.node_id(), k2.node_id(), "node id must be stable across loads");
        assert_eq!(k1.secret_bytes(), k2.secret_bytes());

        // pubkey base64 decodes to exactly the 32-byte node id.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(k1.pubkey_base64())
            .unwrap();
        assert_eq!(decoded.as_slice(), &k1.node_id()[..]);
    }

    /// Fix (B4 review, minor #2): two callers racing to create the device key
    /// on a truly fresh install (e.g. `account_sign_in_verify` and the sync
    /// transport's `ensure_started`, both calling `load_or_create` before
    /// either has written the file) must NOT error — exactly one caller's
    /// `create_new` wins the file, and every other caller reads that winner's
    /// key back instead of propagating an `AlreadyExists` I/O error. A
    /// `Barrier` maximizes the chance every thread's `path.exists()` check
    /// misses before any of them writes, so this reliably exercises the race.
    #[test]
    fn concurrent_first_run_race_all_agree_on_one_key() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(device_key_path(dir.path()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        const N: usize = 8;
        let barrier = Arc::new(Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let path = Arc::clone(&path);
                std::thread::spawn(move || {
                    barrier.wait();
                    DeviceKey::load_or_create(&path).map(|k| k.secret_bytes())
                })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let first = results[0]
            .as_ref()
            .unwrap_or_else(|e| panic!("thread 0 failed under concurrent first-run race: {e:#}"))
            .to_owned();
        for (i, r) in results.into_iter().enumerate() {
            let bytes = r.unwrap_or_else(|e| {
                panic!("thread {i} failed under concurrent first-run race: {e:#}")
            });
            assert_eq!(bytes, first, "all concurrent first-run callers must agree on ONE key");
        }
    }

    /// Unification with the transport: the node id `DeviceKey` derives from the
    /// secret is exactly the id the iroh endpoint binds from that same secret.
    #[test]
    fn pubkey_is_iroh_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = DeviceKey::load_or_create_in(dir.path()).unwrap();
        let expected = *SecretKey::from_bytes(&key.secret_bytes()).public().as_bytes();
        assert_eq!(key.node_id(), expected);
    }

    /// End-to-end unification assertion: an [`IrohTransport`] bound from the
    /// shared device key reports the same node id `DeviceKey` computes — proof
    /// the account pubkey and the transport peer address are one identity.
    #[tokio::test]
    async fn device_key_matches_transport_node_id() {
        use crate::sharing::iroh::{BlobStore, IrohTransport};

        let dir = tempfile::tempdir().unwrap();
        let key = DeviceKey::load_or_create_in(dir.path()).unwrap();

        let transport = IrohTransport::new(
            key.secret_bytes(),
            iroh::RelayMode::Disabled,
            BlobStore::Memory,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            transport.node_id(),
            key.node_id(),
            "iroh endpoint id must equal the DeviceKey node id"
        );
        transport.shutdown().await;
    }

    #[test]
    fn key_file_stays_readable_while_lock_is_held() {
        // fd-lock on Windows is LockFileEx — a MANDATORY lock: any other handle's
        // read of the key file fails with os error 33 while the node holds it
        // (seen live: account_sign_in_verify in the 2026-07-18 user log). The
        // lock must therefore live on a sidecar, never on the key file itself.
        let dir = tempfile::tempdir().unwrap();
        let key = DeviceKey::load_or_create_in(dir.path()).unwrap();

        let lock_path = device_key_lock_path(dir.path());
        let _held = DeviceKeyLock::acquire(&lock_path).unwrap();
        assert!(lock_path.ends_with("device_key.lock"), "lock must be the sidecar");

        // Re-reading the key while the lock is held must work on EVERY platform.
        let reread = DeviceKey::load_or_create_in(dir.path()).unwrap();
        assert_eq!(key.secret_bytes(), reread.secret_bytes());

        // Exclusivity is preserved: a second acquire on the sidecar fails.
        assert!(DeviceKeyLock::acquire(&lock_path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_tightened_on_load() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = device_key_path(dir.path());
        DeviceKey::load_or_create(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        DeviceKey::load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "loose device key permissions must be tightened to 0600");
    }
}
