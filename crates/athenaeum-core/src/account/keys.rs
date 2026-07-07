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
    match write_secret_0600(path, &secret) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "generated new device key");
            Ok(secret)
        }
        Err(create_err) => {
            // First-run race: another caller (e.g. `account_sign_in_verify`
            // and the sync transport's `ensure_started` both racing to create
            // the SAME shared key on a fresh install) won `create_new` between
            // our `path.exists()` check above and this write. Read its key
            // back rather than erroring — there is still exactly one file /
            // one identity, just authored by whichever caller got there
            // first. Only treated as a race when the file is now actually
            // present; any other create failure (permissions, disk full, …)
            // still propagates.
            if path.exists() {
                tracing::debug!(
                    path = %path.display(),
                    "device key created concurrently by another caller; reading back"
                );
                read_device_key(path)
            } else {
                Err(create_err)
            }
        }
    }
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

#[cfg(unix)]
fn write_secret_0600(path: &Path, secret: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create device key {}", path.display()))?;
    f.write_all(secret)
        .with_context(|| format!("write device key {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_0600(path: &Path, secret: &[u8; 32]) -> Result<()> {
    std::fs::write(path, secret)
        .with_context(|| format!("write device key {}", path.display()))?;
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
