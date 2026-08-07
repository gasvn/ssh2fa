//! Owner-only file vault — the credential backend for a headless Linux box.
//!
//! # Why this exists, and why it is opt-in
//!
//! macOS always has a login Keychain and a Linux desktop always has a Secret
//! Service, but a compute server logged into over SSH usually has neither.
//! Without a fallback SSH2FA simply cannot run there.
//!
//! # Why it is not encrypted with a passphrase
//!
//! The product's whole promise is that the daemon restores connections *by
//! itself* after a reboot, before anyone types anything. A passphrase-encrypted
//! vault cannot do that: either the daemon blocks until a human attaches, or the
//! passphrase sits next to the ciphertext and buys nothing. So this backend is
//! deliberately honest — `0600`, owner-only, no theatre — and refuses to run
//! unless the operator opts in (`SSH2FA_VAULT=file`) having read that.
//!
//! In this threat model that is a smaller step than it sounds: `~/.ssh/config`
//! already points at `ControlPath ~/.ssh/cm-ssh2fa-*`, and any process running
//! as this user can ride those live sockets into every host with no password
//! and no 2FA at all. An attacker who can read `$HOME` has already won.
//!
//! What the permission check DOES buy is protection against the classic
//! multi-user-server mistake: a group- or world-readable `$HOME`, where every
//! other account on the box could read the file. That is refused loudly.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::guard::ensure_enabled;
use super::SecretStore;

/// Opt-in switch. Any other value (or none) leaves the file vault disabled.
pub const VAULT_ENV: &str = "SSH2FA_VAULT";

#[derive(Debug, Default, Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

/// Serializes read-modify-write across threads, mirroring the per-process
/// serialization the Keychain/Secret Service backends get from their locks.
/// Cross-PROCESS safety comes from the atomic tmp+rename in `save`.
fn file_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// A `SecretStore` backed by one owner-only JSON file.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// Vault at the default location, `<config_dir>/credentials.json`.
    ///
    /// Uses the same directory as `passwords.json` so `SSH_CONFIG_PATH` moves
    /// the credentials with the rest of the state — which is what makes an
    /// isolated test daemon actually isolated.
    pub fn at_default_path() -> Self {
        Self {
            path: crate::config::paths::config_dir().join("credentials.json"),
        }
    }

    /// Vault at an explicit path (tests).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reject a vault that anyone but the owner can read.
    ///
    /// Checked on every read as well as every write: permissions can be widened
    /// after the file is created (a careless `chmod -R`, a restore from an
    /// archive that lost its modes), and a silent downgrade to world-readable
    /// credentials is exactly what must never pass unnoticed.
    fn check_perms(path: &Path) -> Result<()> {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Internal(format!("vault {path:?}: {e}"))),
        };
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(Error::Internal(format!(
                "credential vault {path:?} is mode {mode:o} — readable beyond its owner. \
                 Run: chmod 600 {}",
                path.display()
            )));
        }
        Ok(())
    }

    fn load(&self) -> Result<VaultFile> {
        Self::check_perms(&self.path)?;
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(VaultFile::default()),
            Err(e) => return Err(Error::Internal(format!("read vault: {e}"))),
        };
        if raw.trim().is_empty() {
            return Ok(VaultFile::default());
        }
        serde_json::from_str(&raw).map_err(|e| {
            // Never silently start from an empty vault: that would look like
            // "no credentials saved" and quietly re-prompt for everything,
            // and the next write would overwrite the damaged (recoverable) file.
            Error::Internal(format!(
                "credential vault {:?} is not valid JSON ({e}) — refusing to overwrite it",
                self.path
            ))
        })
    }

    /// Atomic replace: write a 0600 temp file in the same directory, fsync, then
    /// rename over the vault. A crash mid-write leaves the old file intact
    /// rather than a truncated one.
    fn save(&self, vault: &VaultFile) -> Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| Error::Internal("vault path has no parent directory".into()))?;
        std::fs::create_dir_all(dir).map_err(|e| Error::Internal(format!("create {dir:?}: {e}")))?;
        // 0700 on the directory too — a readable dir leaks the host list via
        // the file names even when the vault itself is locked down.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));

        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(vault)
            .map_err(|e| Error::Internal(format!("serialize vault: {e}")))?;
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| Error::Internal(format!("open {tmp:?}: {e}")))?;
            f.write_all(&body)
                .map_err(|e| Error::Internal(format!("write {tmp:?}: {e}")))?;
            f.sync_all()
                .map_err(|e| Error::Internal(format!("fsync {tmp:?}: {e}")))?;
        }
        // mode() only applies at creation — tighten a pre-existing temp file.
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| Error::Internal(format!("rename into {:?}: {e}", self.path)))?;
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn get(&self, acct: &str) -> Result<Option<String>> {
        ensure_enabled()?;
        let _serial = file_lock().lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.load()?.secrets.get(acct).cloned())
    }

    fn set(&self, acct: &str, val: &str) -> Result<()> {
        ensure_enabled()?;
        let _serial = file_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut vault = self.load()?;
        vault.version = 1;
        vault.secrets.insert(acct.to_owned(), val.to_owned());
        self.save(&vault)
    }

    fn delete(&self, acct: &str) -> Result<()> {
        ensure_enabled()?;
        let _serial = file_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut vault = self.load()?;
        if vault.secrets.remove(acct).is_none() {
            return Ok(());
        }
        vault.version = 1;
        self.save(&vault)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `ensure_enabled` guard blocks a Cargo test process, so these tests
    /// exercise the file layer directly rather than through `SecretStore`.
    fn roundtrip_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn save_then_load_roundtrips_and_is_owner_only() {
        let d = roundtrip_dir();
        let s = FileStore::at(d.path().join("credentials.json"));
        let mut v = VaultFile { version: 1, ..Default::default() };
        v.secrets.insert("k6.password".into(), "pw".into());
        s.save(&v).unwrap();

        let mode = std::fs::metadata(s.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "vault must be owner-only, got {mode:o}");
        let dir_mode = std::fs::metadata(d.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "vault directory must be owner-only");

        let back = s.load().unwrap();
        assert_eq!(back.secrets.get("k6.password").map(String::as_str), Some("pw"));
    }

    #[test]
    fn missing_vault_reads_as_empty_not_as_an_error() {
        let d = roundtrip_dir();
        let s = FileStore::at(d.path().join("nope.json"));
        assert!(s.load().unwrap().secrets.is_empty());
    }

    /// A widened mode must fail LOUDLY. Silently reading a world-readable
    /// vault is the failure this check exists to prevent.
    #[test]
    fn a_group_or_world_readable_vault_is_refused() {
        let d = roundtrip_dir();
        let s = FileStore::at(d.path().join("credentials.json"));
        s.save(&VaultFile::default()).unwrap();
        for bad in [0o640, 0o604, 0o666] {
            std::fs::set_permissions(s.path(), std::fs::Permissions::from_mode(bad)).unwrap();
            let e = s.load().expect_err("mode {bad:o} must be refused");
            assert!(e.to_string().contains("readable beyond its owner"), "{e}");
            assert!(e.to_string().contains("chmod 600"), "must name the fix: {e}");
        }
    }

    /// Corrupt JSON must not read as "no credentials" — that would look like a
    /// fresh install and let the next write destroy a recoverable file.
    #[test]
    fn corrupt_vault_refuses_rather_than_starting_over() {
        let d = roundtrip_dir();
        let p = d.path().join("credentials.json");
        std::fs::write(&p, "{not json").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let e = FileStore::at(&p).load().expect_err("corrupt vault must error");
        assert!(e.to_string().contains("refusing to overwrite"), "{e}");
    }

    #[test]
    fn an_empty_file_is_treated_as_an_empty_vault() {
        let d = roundtrip_dir();
        let p = d.path().join("credentials.json");
        std::fs::write(&p, "").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(FileStore::at(&p).load().unwrap().secrets.is_empty());
    }

    /// The write must be atomic and leave no temp file behind — a leftover
    /// `credentials.json.tmp` would be a second copy of every secret.
    #[test]
    fn save_leaves_no_temp_file() {
        let d = roundtrip_dir();
        let s = FileStore::at(d.path().join("credentials.json"));
        s.save(&VaultFile::default()).unwrap();
        let strays: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "temp file left behind: {strays:?}");
    }
}
