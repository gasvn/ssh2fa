//! Credential storage abstraction — mirrors `credentials.py`.
//!
//! The `SecretStore` trait decouples business logic from the real macOS
//! Keychain, so unit tests can inject a `FakeStore` without touching the
//! system credential store.

pub mod keychain;
pub mod migrate;

use crate::error::Result;

/// A generic secret store: get / set / delete by account name.
pub trait SecretStore {
    fn get(&self, acct: &str) -> Result<Option<String>>;
    fn set(&self, acct: &str, val: &str) -> Result<()>;
    fn delete(&self, acct: &str) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Account-name helpers — must match credentials.py exactly.
// ---------------------------------------------------------------------------

fn password_acct(host: &str) -> String {
    format!("{host}.password")
}

fn otpauth_acct(host: &str) -> String {
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
pub fn store_credentials<S: SecretStore>(
    store: &S,
    host: &str,
    password: &str,
    otpauth: &str,
) -> Result<()> {
    store.set(&password_acct(host), password)?;
    if let Err(e) = store.set(&otpauth_acct(host), otpauth) {
        // Roll back the password write.
        let _ = store.delete(&password_acct(host));
        return Err(e);
    }
    Ok(())
}

/// Retrieve the SSH password for `host`, or `None` if absent.
pub fn get_password<S: SecretStore>(store: &S, host: &str) -> Result<Option<String>> {
    store.get(&password_acct(host))
}

/// Retrieve the otpauth URL for `host`, or `None` if absent.
pub fn get_otpauth<S: SecretStore>(store: &S, host: &str) -> Result<Option<String>> {
    store.get(&otpauth_acct(host))
}

/// Delete both the password and otpauth entries for `host`.
/// Errors on individual deletes are ignored (absent entries are not errors).
pub fn delete_credentials<S: SecretStore>(store: &S, host: &str) -> Result<()> {
    let _ = store.delete(&password_acct(host));
    let _ = store.delete(&otpauth_acct(host));
    Ok(())
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

    #[test]
    fn store_rolls_back_if_second_write_fails() {
        let s = FakeStore {
            map: RefCell::new(HashMap::new()),
            fail_on: Some("k6.otpauth".into()),
        };
        let r = store_credentials(
            &s,
            "k6",
            "pw",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP",
        );
        assert!(r.is_err());
        // first write (password) must have been rolled back
        assert!(
            s.get("k6.password").unwrap().is_none(),
            "password not rolled back"
        );
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
        assert_eq!(s.get("k6.password").unwrap().as_deref(), Some("pw"));
        assert!(s.get("k6.otpauth").unwrap().is_some());
    }
}
