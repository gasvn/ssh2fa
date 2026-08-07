//! Credential storage abstraction — mirrors `credentials.py`.
//!
//! The `SecretStore` trait decouples business logic from the real system
//! credential store, so unit tests can inject a `FakeStore` without touching
//! it — and so each platform can bring its own store without the daemon
//! knowing which one it got:
//!
//! | platform | backend |
//! |----------|---------|
//! | macOS    | login Keychain via the Security framework ([`keychain`]) |
//! | Linux    | freedesktop Secret Service, or an owner-only file ([`linux`]) |
//!
//! Everything above this layer — the vault, the migrations, every handler —
//! is platform-independent and calls [`platform_store`].

pub mod file_store;
pub(crate) mod guard;
#[cfg(target_os = "macos")]
pub mod keychain;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod migrate;
pub mod vault;

use crate::error::Result;

// ---------------------------------------------------------------------------
// Platform store
// ---------------------------------------------------------------------------

/// This platform's credential store type.
#[cfg(target_os = "macos")]
pub type PlatformStore = keychain::KeychainStore;
/// This platform's credential store type.
#[cfg(target_os = "linux")]
pub type PlatformStore = linux::LinuxStore;

/// The credential store to use on this machine.
///
/// Cheap — the returned value carries no state; any backend probing happens
/// once per process behind the platform module's own cache. Call it at the
/// point of use rather than threading a store through, exactly as the code
/// previously named `KeychainStore` inline.
#[cfg(target_os = "macos")]
pub fn platform_store() -> PlatformStore {
    keychain::KeychainStore
}

/// The credential store to use on this machine.
#[cfg(target_os = "linux")]
pub fn platform_store() -> PlatformStore {
    linux::LinuxStore
}

/// One-line description of the resolved backend, for logs and diagnostics.
pub fn platform_store_description() -> String {
    #[cfg(target_os = "macos")]
    {
        "macOS login Keychain".into()
    }
    #[cfg(target_os = "linux")]
    {
        linux::LinuxStore::describe()
    }
}

/// A generic secret store: get / set / delete by account name.
pub trait SecretStore {
    fn get(&self, acct: &str) -> Result<Option<String>>;
    fn set(&self, acct: &str, val: &str) -> Result<()>;
    fn delete(&self, acct: &str) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Account-name helpers — must match credentials.py exactly.
// ---------------------------------------------------------------------------

pub(crate) fn password_acct(host: &str) -> String {
    format!("{host}.password")
}

pub(crate) fn otpauth_acct(host: &str) -> String {
    format!("{host}.otpauth")
}

// ---------------------------------------------------------------------------
// High-level operations
// ---------------------------------------------------------------------------

/// Store the SSH password **and** the otpauth URL for `host`.
///
/// Both writes must succeed atomically: if the second write fails the first is
/// rolled back (deleted) and the error is returned, leaving no half-credential
/// behind — matching `set_credentials` in `credentials.py`.
/// Both secrets are written together, into the single vault item — so there is
/// no half-credential state to roll back (the old two-item write needed one).
pub fn store_credentials<S: SecretStore>(
    store: &S,
    host: &str,
    password: &str,
    otpauth: &str,
) -> Result<()> {
    vault::set_host_creds(
        store,
        host,
        vault::HostCreds {
            password: password.to_owned(),
            otpauth: otpauth.to_owned(),
        },
    )
}

/// Retrieve the SSH password for `host`, or `None` if absent.
pub fn get_password<S: SecretStore>(store: &S, host: &str) -> Result<Option<String>> {
    let c = vault::get_host_creds(store, host)?;
    Ok(if c.password.is_empty() { None } else { Some(c.password) })
}

/// Retrieve the otpauth URL for `host`, or `None` if absent.
pub fn get_otpauth<S: SecretStore>(store: &S, host: &str) -> Result<Option<String>> {
    let c = vault::get_host_creds(store, host)?;
    Ok(if c.otpauth.trim().is_empty() { None } else { Some(c.otpauth) })
}

/// Delete a host's credentials from the vault AND any legacy per-host entries.
pub fn delete_credentials<S: SecretStore>(store: &S, host: &str) -> Result<()> {
    vault::delete_host_creds(store, host)
}

// ---------------------------------------------------------------------------
// Tests (FakeStore — no real Keychain)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct FakeStore {
        map: RefCell<HashMap<String, String>>,
        fail_on: Option<String>,
    }

    impl SecretStore for FakeStore {
        fn get(&self, a: &str) -> Result<Option<String>> {
            Ok(self.map.borrow().get(a).cloned())
        }
        fn set(&self, a: &str, v: &str) -> Result<()> {
            if self.fail_on.as_deref() == Some(a) {
                return Err(Error::Internal("boom".into()));
            }
            self.map.borrow_mut().insert(a.into(), v.into());
            Ok(())
        }
        fn delete(&self, a: &str) -> Result<()> {
            self.map.borrow_mut().remove(a);
            Ok(())
        }
    }

    /// Both secrets now live in ONE item, so there is no half-written state to
    /// roll back — but a failed write must still leave NOTHING readable, rather
    /// than a password with no 2FA secret (which would fail every login).
    #[test]
    fn failed_store_leaves_no_partial_credentials() {
        let s = FakeStore {
            map: RefCell::new(HashMap::new()),
            fail_on: Some(vault::VAULT_ACCOUNT.into()),
        };
        let r = store_credentials(
            &s,
            "k6",
            "pw",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP",
        );
        assert!(r.is_err(), "a failing store must report the failure");
        assert!(get_password(&s, "k6").unwrap().is_none());
        assert!(get_otpauth(&s, "k6").unwrap().is_none());
    }

    #[test]
    fn store_then_get_both() {
        let s = FakeStore {
            map: RefCell::new(HashMap::new()),
            fail_on: None,
        };
        store_credentials(
            &s,
            "k6",
            "pw",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP",
        )
        .unwrap();
        // Assert through the public API: the on-disk layout is the vault's
        // business, and it deliberately no longer uses per-host accounts.
        assert_eq!(get_password(&s, "k6").unwrap().as_deref(), Some("pw"));
        assert!(get_otpauth(&s, "k6").unwrap().is_some());
        assert_eq!(s.map.borrow().len(), 1, "one host must occupy one item");
    }
}
