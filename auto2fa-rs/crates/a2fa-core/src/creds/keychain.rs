//! macOS Keychain backend — implements `SecretStore` via the `keyring` crate.
//!
//! Service name matches `KEYCHAIN_SERVICE = "auto2fa"` from `credentials.py`.
//! Not unit-tested here; real Keychain access requires the system credential
//! store (unavailable in CI).

use keyring::Entry;

use crate::error::{Error, Result};

use super::SecretStore;

/// The Keychain service name used for all auto2fa credentials.
///
/// Must equal `KEYCHAIN_SERVICE` in `credentials.py`.
pub const SERVICE: &str = "auto2fa";

/// A `SecretStore` backed by the macOS Keychain (via the `keyring` crate).
pub struct KeychainStore;

impl SecretStore for KeychainStore {
    fn get(&self, acct: &str) -> Result<Option<String>> {
        let entry = Entry::new(SERVICE, acct)
            .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
        match entry.get_password() {
            Ok(pw) => Ok(Some(pw)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Internal(format!("keyring get({acct}): {e}"))),
        }
    }

    fn set(&self, acct: &str, val: &str) -> Result<()> {
        let entry = Entry::new(SERVICE, acct)
            .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
        entry
            .set_password(val)
            .map_err(|e| Error::Internal(format!("keyring set({acct}): {e}")))
    }

    fn delete(&self, acct: &str) -> Result<()> {
        let entry = Entry::new(SERVICE, acct)
            .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            // Already absent — treat as success, matching credentials.py behaviour.
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Internal(format!("keyring delete({acct}): {e}"))),
        }
    }
}
