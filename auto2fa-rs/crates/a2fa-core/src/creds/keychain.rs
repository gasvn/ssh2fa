//! macOS Keychain backend.
//!
//! Credentials are read and written through the Security framework (via the
//! `keyring` crate), so the Keychain ACL is attached to the signed daemon — not
//! to a generic command-line tool that every process can invoke.
//!
//! Release builds pin both halves of the daemon's designated requirement:
//!
//! ```text
//! identifier "com.auto2fa.daemon" and certificate leaf = H"…"
//! ```
//!
//! That requirement is stable across rebuilds.  A Keychain item created by one
//! correctly packaged release therefore remains readable by later releases
//! without another authorization dialog.  Ad-hoc builds are deliberately
//! rejected by `package-app.sh`, because their cdhash-based requirement changes
//! on every build.

use std::sync::{Mutex, OnceLock};

use crate::error::{Error, Result};

use super::SecretStore;

/// The Keychain service name used for all SSH2FA credentials.
///
/// Must equal `KEYCHAIN_SERVICE` in `credentials.py`.
pub const SERVICE: &str = "auto2fa";

/// Serialize every Keychain operation in this process.
///
/// macOS presents authorization per operation.  Without this lock, several
/// hosts reconnecting together can stack several dialogs before the first one
/// has been answered.  The lock is held for one framework call only, never
/// across caller logic.
fn keychain_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Test daemons must never touch the developer's real login Keychain.
fn ensure_enabled() -> Result<()> {
    if std::env::var_os("SSH2FA_DISABLE_KEYCHAIN").is_some() {
        return Err(Error::Internal(
            "Keychain access is disabled for this isolated test daemon".into(),
        ));
    }
    Ok(())
}

/// A `SecretStore` backed by the macOS Security framework.
pub struct KeychainStore;

impl SecretStore for KeychainStore {
    fn get(&self, acct: &str) -> Result<Option<String>> {
        ensure_enabled()?;
        let _serial = keychain_lock().lock().unwrap_or_else(|e| e.into_inner());
        let entry = keyring::Entry::new(SERVICE, acct)
            .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
        match entry.get_password() {
            Ok(pw) => Ok(Some(pw)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Internal(format!("keyring get({acct}): {e}"))),
        }
    }

    fn set(&self, acct: &str, val: &str) -> Result<()> {
        ensure_enabled()?;
        let _serial = keychain_lock().lock().unwrap_or_else(|e| e.into_inner());
        let entry = keyring::Entry::new(SERVICE, acct)
            .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
        entry
            .set_password(val)
            .map_err(|e| Error::Internal(format!("keyring set({acct}): {e}")))
    }

    fn delete(&self, acct: &str) -> Result<()> {
        ensure_enabled()?;
        let _serial = keychain_lock().lock().unwrap_or_else(|e| e.into_inner());
        let entry = keyring::Entry::new(SERVICE, acct)
            .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Internal(format!("keyring delete({acct}): {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_serializing_lock_is_released_between_operations() {
        {
            let _a = keychain_lock().lock().unwrap();
        }
        let _b = keychain_lock()
            .try_lock()
            .expect("lock must be free once the previous operation returned");
    }
}
