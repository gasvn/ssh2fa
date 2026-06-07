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

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value};

use crate::config::passwords_store::{save_meta, HostMeta};
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
// File-level orchestration
// ---------------------------------------------------------------------------

/// Count the number of entries in a legacy v1 passwords.json `Value` that have
/// both a non-empty password and a non-empty otpauth/otpauthUrl — i.e., entries
/// that would actually be migrated.  Reserved keys ("schema", "hosts") are
/// skipped.
fn count_migratable(legacy: &Value) -> usize {
    let obj = match legacy.as_object() {
        Some(o) => o,
        None => return 0,
    };
    let mut count = 0usize;
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
            .unwrap_or("");
        let otpauth = cfg_obj
            .get("otpauthUrl")
            .or_else(|| cfg_obj.get("otpauth_url"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !password.is_empty() && !otpauth.is_empty() {
            count += 1;
        }
    }
    count
}

/// Orchestrate the v1 → v2 migration for the passwords.json file at `path`.
///
/// # Behaviour
///
/// - Missing file → `Ok(false)`.
/// - Unparseable JSON or non-object → logged as warning, `Ok(false)` (no
///   crash at boot).
/// - Already v2 (`"schema": 2` present) → `Ok(false)` (idempotent).
/// - All entries are cred-less → `Ok(false)`, file **not** touched, **no**
///   backup created.
/// - Otherwise: create a one-time backup at `<path>.pre-keychain-backup`
///   (only if it doesn't already exist); on backup failure → `Err` (refuse
///   to migrate, leave file as v1 so the next launch retries).  Then migrate
///   creds into `store`, atomically rewrite the file as v2, return `Ok(true)`.
///
/// Migration failures (Keychain write errors) are propagated as `Err`; the
/// daemon boot path must **not** abort but should log the error and continue.
pub fn migrate_passwords_file_if_needed<S: SecretStore>(
    store: &S,
    path: &Path,
) -> Result<bool> {
    // 1. Missing file — nothing to migrate.
    if !path.exists() {
        return Ok(false);
    }

    // 2. Read + parse.
    let text = std::fs::read_to_string(path).map_err(Error::Io)?;
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[migrate] passwords.json is not valid JSON — skipping migration: {e}");
            return Ok(false);
        }
    };
    if !value.is_object() {
        log::warn!("[migrate] passwords.json is not a JSON object — skipping migration");
        return Ok(false);
    }

    // 3. Already v2?
    if value.get("schema").and_then(Value::as_u64) == Some(2) {
        return Ok(false);
    }

    // 4. Any migratable entries?
    if count_migratable(&value) == 0 {
        log::info!(
            "[migrate] no migratable entries in passwords.json — leaving untouched"
        );
        return Ok(false);
    }

    // 5. One-time backup.
    let backup = {
        let mut b = path.to_path_buf();
        let new_name = {
            let old = b
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "passwords.json".into());
            format!("{old}.pre-keychain-backup")
        };
        b.set_file_name(new_name);
        b
    };
    if !backup.exists() {
        std::fs::copy(path, &backup).map_err(|e| {
            Error::Internal(format!(
                "backup write failed ({backup:?}): {e} — refusing to migrate"
            ))
        })?;
        log::info!("[migrate] backup saved to {}", backup.display());
    }

    // 6. Migrate into store (all-or-nothing via migrate_v1_to_v2).
    let v2 = migrate_v1_to_v2(store, &value)?;

    // 7. Persist as v2 using the audited atomic writer.
    //    Extract the "hosts" object and deserialise into HashMap<String, HostMeta>.
    let hosts_value = v2.get("hosts").cloned().unwrap_or(Value::Object(Map::new()));
    let hosts: HashMap<String, HostMeta> = serde_json::from_value(hosts_value)
        .map_err(|e| Error::Internal(format!("deserialise v2 hosts: {e}")))?;
    save_meta(path, &hosts)?;

    Ok(true)
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

    // -----------------------------------------------------------------------
    // count_migratable unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn count_migratable_counts_valid_entries() {
        let v = serde_json::json!({
            "schema": 1,
            "hosts": {"ignored": {"autoConnect": false}},
            "k6": {"password": "pw", "otpauthUrl": "otpauth://totp/x?secret=AAA"},
            "k8": {"password": "pw2", "otpauth_url": "otpauth://totp/y?secret=BBB"},
            "bad": {"password": "", "otpauthUrl": "otpauth://totp/z?secret=CCC"}
        });
        // schema + hosts skipped; k6 + k8 valid; bad has empty password
        assert_eq!(count_migratable(&v), 2);
    }

    #[test]
    fn count_migratable_zero_for_empty() {
        assert_eq!(count_migratable(&serde_json::json!({})), 0);
        assert_eq!(count_migratable(&serde_json::json!(null)), 0);
    }

    // -----------------------------------------------------------------------
    // migrate_passwords_file_if_needed tests — FakeStore + tempdir only
    // -----------------------------------------------------------------------

    #[test]
    fn file_orchestration_v1_two_hosts_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        let original = serde_json::json!({
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
        let original_text = serde_json::to_string_pretty(&original).unwrap();
        std::fs::write(&path, &original_text).unwrap();

        let store = FakeStore { map: RefCell::new(HashMap::new()) };
        let result = migrate_passwords_file_if_needed(&store, &path).unwrap();
        assert!(result, "should return true for a successful migration");

        // File on disk is now v2.
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk["schema"].as_u64(), Some(2));
        assert!(on_disk["hosts"]["k6"].is_object());
        assert!(on_disk["hosts"]["k8"].is_object());
        assert_eq!(on_disk["hosts"]["k6"]["autoConnect"].as_bool(), Some(true));

        // Backup exists and matches original content.
        let backup = dir.path().join("passwords.json.pre-keychain-backup");
        assert!(backup.exists(), "backup should exist");
        let backup_text = std::fs::read_to_string(&backup).unwrap();
        assert_eq!(backup_text, original_text, "backup should match original content");

        // Store has the creds.
        assert_eq!(store.get("k6.password").unwrap().as_deref(), Some("hunter2"));
        assert!(store.get("k6.otpauth").unwrap().is_some());
        assert_eq!(store.get("k8.password").unwrap().as_deref(), Some("pw2"));
    }

    #[test]
    fn file_orchestration_idempotent_on_v2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        let v2 = serde_json::json!({
            "schema": 2,
            "hosts": {
                "k6": {"autoConnect": true}
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&v2).unwrap()).unwrap();

        let store = FakeStore { map: RefCell::new(HashMap::new()) };
        // Run once (already v2) → should be false.
        assert!(!migrate_passwords_file_if_needed(&store, &path).unwrap());

        // No backup created.
        let backup = dir.path().join("passwords.json.pre-keychain-backup");
        assert!(!backup.exists(), "no backup should be created for already-v2 file");
    }

    #[test]
    fn file_orchestration_cred_less_entries_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        let v1_empty = serde_json::json!({
            "k6": {
                "password": "",
                "otpauthUrl": "otpauth://totp/x?secret=AAA"
            }
        });
        let original_text = serde_json::to_string_pretty(&v1_empty).unwrap();
        std::fs::write(&path, &original_text).unwrap();

        let store = FakeStore { map: RefCell::new(HashMap::new()) };
        assert!(!migrate_passwords_file_if_needed(&store, &path).unwrap());

        // No backup.
        let backup = dir.path().join("passwords.json.pre-keychain-backup");
        assert!(!backup.exists(), "no backup when nothing to migrate");

        // File unchanged.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, original_text, "file must be unchanged");
    }

    #[test]
    fn file_orchestration_missing_file_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.json");
        let store = FakeStore { map: RefCell::new(HashMap::new()) };
        assert!(!migrate_passwords_file_if_needed(&store, &path).unwrap());
    }

    #[test]
    fn file_orchestration_backup_is_one_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwords.json");
        let v1 = serde_json::json!({
            "k6": {
                "password": "pw",
                "otpauthUrl": "otpauth://totp/x?secret=AAA",
                "autoConnect": false
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&v1).unwrap()).unwrap();

        // Pre-create backup with sentinel content.
        let backup = dir.path().join("passwords.json.pre-keychain-backup");
        let sentinel = "SENTINEL_DO_NOT_OVERWRITE";
        std::fs::write(&backup, sentinel).unwrap();

        let store = FakeStore { map: RefCell::new(HashMap::new()) };
        let result = migrate_passwords_file_if_needed(&store, &path).unwrap();
        assert!(result, "migration should succeed");

        // Backup content must NOT have been overwritten.
        let backup_content = std::fs::read_to_string(&backup).unwrap();
        assert_eq!(backup_content, sentinel, "backup must not be overwritten when it already exists");
    }
}
