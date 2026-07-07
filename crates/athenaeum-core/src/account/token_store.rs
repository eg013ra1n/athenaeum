//! Device-token storage (task B4).
//!
//! The hub device token is a bearer credential — it is stored in the OS keychain
//! (service `com.vsharifov.athenaeum`, account = the hub host so multiple hubs
//! coexist) and **never** in the catalog DB, logs, or any error message.
//!
//! Backend selection:
//! - **macOS / Windows** — the native keychain (`keyring` crate,
//!   `apple-native` / `windows-native`). If a keychain call fails at runtime
//!   (locked, sandbox denial), it transparently falls back to the 0600 file.
//! - **Linux / everything else** — a 0600 file directly (the `keyring` crate is
//!   compiled without a secret-service/dbus backend, so there is no reliable
//!   native store; the web/Docker and headless builds run here). This is the
//!   documented file-0600 fallback (Perseus pattern).
//!
//! [`TokenStore::file_only`] forces the file backend regardless of platform —
//! used by tests (so they never touch the real login keychain / trigger a
//! prompt) and available to any headless shell that wants to opt out of the
//! keychain.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Keychain service name shared by all hubs (account = hub host disambiguates).
pub const KEYRING_SERVICE: &str = "com.vsharifov.athenaeum";

/// Stores/loads the hub device token for one account (hub host).
pub struct TokenStore {
    /// Keychain account / file discriminator — the hub host.
    account: String,
    /// 0600 file used as the fallback (or the sole backend under `file_only`).
    fallback_path: PathBuf,
    /// When true, never touch the OS keychain — file backend only.
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
    /// itself is unavailable.
    pub fn load(&self) -> Result<Option<String>> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if !self.file_only {
            match self.keyring_entry() {
                Ok(entry) => match entry.get_password() {
                    Ok(token) => return Ok(Some(token)),
                    Err(keyring::Error::NoEntry) => return Ok(None),
                    Err(e) => tracing::warn!(
                        account = %self.account,
                        error = %e,
                        "keychain load failed; consulting 0600 file fallback"
                    ),
                },
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

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn keyring_entry(&self) -> Result<keyring::Entry, keyring::Error> {
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
}
