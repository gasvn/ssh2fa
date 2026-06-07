//! V1 → V2 credential migration — Rust port of `_migrate_v1_to_v2` from
//! `credentials.py`.
//!
//! The old "v1" passwords.json stored SSH passwords and otpauth URLs as plain
//! text under each hostname key.  This module moves those values into the
//! `SecretStore` (Keychain on macOS) and rewrites the JSON in the slim "v2"
//! format that holds only metadata.
//!
//! The function is a pure data transformer: it receives the parsed v1 JSON and
//! a reference to any `SecretStore`, writes to the store, and returns the new
//! v2 JSON value.  File I/O and backup creation are the caller's
//! responsibility.

use serde_json::{Map, Value};

use crate::error::{Error, Result};

use super::{delete_credentials, store_credentials, SecretStore};

/// The schema version written into migrated passwords.json files.
pub const SCHEMA_V2: u64 = 2;

/// Migrate a v1 passwords.json `Value` into the store.
///
/// Returns the new v2 JSON `Value` (`{ "schema": 2, "hosts": { … } }`) that
/// the caller should persist atomically.
///
/// # Errors
///
/// Returns an error if any store write fails.  In that case, any Keychain
/// entries already written are rolled back (deleted), matching the Python
/// implementation's all-or-nothing guarantee.
pub fn migrate_v1_to_v2<S: SecretStore>(store: &S, legacy: &Value) -> Result<Value> {
    let obj = legacy
        .as_object()
        .ok_or_else(|| Error::BadParams("passwords.json must be a JSON object".into()))?;

    // Collect valid entries, skipping reserved keys.
    let mut to_write: Vec<(String, String, String, bool)> = Vec::new();

    for (host, cfg) in obj {
        if host == "schema" || host == "hosts" {
            continue;
        }
        let cfg_obj = match cfg.as_object() {
            Some(o) => o,
            None => continue,
        };
        let password = cfg_obj
            .get("password")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let otpauth = cfg_obj
            .get("otpauthUrl")
            .or_else(|| cfg_obj.get("otpauth_url"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        if password.is_empty() || otpauth.is_empty() {
            log::warn!(
                "[migrate] {host} legacy entry missing creds — skipped"
            );
            continue;
        }

        let auto_connect = cfg_obj
            .get("autoConnect")
            .or_else(|| cfg_obj.get("auto_connect"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        to_write.push((host.clone(), password, otpauth, auto_connect));
    }

    if to_write.is_empty() {
        // Nothing to migrate — return legacy unchanged (Python does the same).
        return Ok(legacy.clone());
    }

    // All-or-nothing write: collect what we've written so we can roll back.
    let mut written: Vec<String> = Vec::new();
    for (host, pw, otp, _) in &to_write {
        if let Err(e) = store_credentials(store, host, pw, otp) {
            // Roll back.
            for done in &written {
                let _ = delete_credentials(store, done);
            }
            return Err(Error::Internal(format!(
                "Keychain migration failed at {host}: {e}. \
                 passwords.json left as v1; check Keychain access and retry."
            )));
        }
        written.push(host.clone());
    }

    // Build the v2 JSON value.
    let mut hosts = Map::new();
    for (host, _, _, auto_connect) in &to_write {
        let mut meta = Map::new();
        meta.insert("autoConnect".into(), Value::Bool(*auto_connect));
        hosts.insert(host.clone(), Value::Object(meta));
    }
    let mut v2 = Map::new();
    v2.insert("schema".into(), Value::Number(SCHEMA_V2.into()));
    v2.insert("hosts".into(), Value::Object(hosts));

    log::info!(
        "[migrate] migration complete — {} hosts now in Keychain",
        written.len()
    );

    Ok(Value::Object(v2))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    use crate::creds::SecretStore;


    struct FakeStore {
        map: RefCell<HashMap<String, String>>,
    }

    impl SecretStore for FakeStore {
        fn get(&self, a: &str) -> crate::error::Result<Option<String>> {
            Ok(self.map.borrow().get(a).cloned())
        }
        fn set(&self, a: &str, v: &str) -> crate::error::Result<()> {
            self.map.borrow_mut().insert(a.into(), v.into());
            Ok(())
        }
        fn delete(&self, a: &str) -> crate::error::Result<()> {
            self.map.borrow_mut().remove(a);
            Ok(())
        }
    }

    #[test]
    fn migrate_happy_path() {
        let store = FakeStore {
            map: RefCell::new(HashMap::new()),
        };
        let legacy = serde_json::json!({
            "k6": {
                "password": "hunter2",
                "otpauthUrl": "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP",
                "autoConnect": true
            },
            "k8": {
                "password": "pw2",
                "otpauthUrl": "otpauth://totp/y?secret=JBSWY3DPEHPK3PXP",
                "autoConnect": false
            }
        });

        let v2 = migrate_v1_to_v2(&store, &legacy).unwrap();

        // Schema must be 2.
        assert_eq!(v2["schema"].as_u64(), Some(2));

        // Both hosts present in metadata.
        let hosts = v2["hosts"].as_object().unwrap();
        assert!(hosts.contains_key("k6"));
        assert!(hosts.contains_key("k8"));
        assert_eq!(hosts["k6"]["autoConnect"].as_bool(), Some(true));

        // Credentials written to the store.
        assert_eq!(
            store.get("k6.password").unwrap().as_deref(),
            Some("hunter2")
        );
        assert!(store.get("k6.otpauth").unwrap().is_some());
        assert_eq!(store.get("k8.password").unwrap().as_deref(), Some("pw2"));
    }

    #[test]
    fn migrate_skips_entry_missing_creds() {
        let store = FakeStore {
            map: RefCell::new(HashMap::new()),
        };
        let legacy = serde_json::json!({
            "k6": {
                "password": "",          // empty — should be skipped
                "otpauthUrl": "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP",
                "autoConnect": false
            }
        });
        // No valid entries → returns legacy unchanged.
        let result = migrate_v1_to_v2(&store, &legacy).unwrap();
        // Returns the original value (no "schema" key at top level for v1).
        assert!(result.get("schema").is_none());
    }

    #[test]
    fn migrate_rolls_back_on_store_error() {
        use crate::error::Error as E;

        struct FailingStore {
            map: RefCell<HashMap<String, String>>,
            fail_count: RefCell<usize>,
        }
        impl SecretStore for FailingStore {
            fn get(&self, a: &str) -> crate::error::Result<Option<String>> {
                Ok(self.map.borrow().get(a).cloned())
            }
            fn set(&self, a: &str, v: &str) -> crate::error::Result<()> {
                let mut count = self.fail_count.borrow_mut();
                *count += 1;
                if *count > 2 {
                    // Fail on the 3rd set (second host's password).
                    return Err(E::Internal("injected failure".into()));
                }
                self.map.borrow_mut().insert(a.into(), v.into());
                Ok(())
            }
            fn delete(&self, a: &str) -> crate::error::Result<()> {
                self.map.borrow_mut().remove(a);
                Ok(())
            }
        }

        let store = FailingStore {
            map: RefCell::new(HashMap::new()),
            fail_count: RefCell::new(0),
        };
        let legacy = serde_json::json!({
            "k6": {
                "password": "pw1",
                "otpauthUrl": "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP",
                "autoConnect": false
            },
            "k8": {
                "password": "pw2",
                "otpauthUrl": "otpauth://totp/y?secret=JBSWY3DPEHPK3PXP",
                "autoConnect": false
            }
        });

        let result = migrate_v1_to_v2(&store, &legacy);
        assert!(result.is_err(), "should have failed on injected error");
    }
}
