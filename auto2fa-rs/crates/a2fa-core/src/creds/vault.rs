//! Single-item credential storage ("the vault").
//!
//! # Why
//!
//! Credentials used to live in TWO Keychain items per host (`<host>.password`
//! and `<host>.otpauth`). Every Keychain item carries its OWN access-control
//! list, and macOS asks permission per item whenever the reading binary's code
//! identity changes — which it does on every release, because the daemon is a
//! separately-signed helper. Six hosts therefore meant **twelve** "Always
//! Allow" prompts after each update, one per secret. That is the single most
//! irritating thing about updating, and no amount of signing hygiene fixes it:
//! the count comes from the number of items, not from the signature.
//!
//! One item ⇒ at most ONE prompt per update, no matter how many hosts.
//!
//! # Layout
//!
//! Service `auto2fa`, account [`VAULT_ACCOUNT`], value:
//!
//! ```json
//! { "version": 1, "hosts": { "k6": { "password": "…", "otpauth": "…" } } }
//! ```
//!
//! Reads fall back to the legacy per-host items so an un-migrated install keeps
//! working; [`migrate_to_vault`] folds them in and removes them.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::{otpauth_acct, password_acct, SecretStore};

/// Keychain account holding every host's credentials.
///
/// It cannot collide with a per-host legacy item because those always carry a
/// `.password` / `.otpauth` SUFFIX (`password_acct` / `otpauth_acct`), and this
/// name has none. Note the collision-safety does NOT come from the name being
/// an illegal host name — `is_safe_host_name` permits a leading underscore, so
/// `__ssh2fa_vault__` is in fact a perfectly legal host name. The suffix is the
/// whole guarantee; `vault_account_cannot_collide_with_a_host_entry` pins it.
pub const VAULT_ACCOUNT: &str = "__ssh2fa_vault__";

const VAULT_VERSION: u32 = 1;

/// One host's secrets.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostCreds {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub otpauth: String,
}

/// The decoded vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Vault {
    pub version: u32,
    #[serde(default)]
    pub hosts: HashMap<String, HostCreds>,
}

impl Default for Vault {
    fn default() -> Self {
        Vault {
            version: VAULT_VERSION,
            hosts: HashMap::new(),
        }
    }
}

/// Serializes every read-modify-write of the vault.
///
/// Without it, re-keying two hosts concurrently would interleave load→mutate→
/// save and silently drop one host's update — the same lost-update race
/// `passwords_store::update_meta` guards against.
fn vault_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Read the vault. A missing item is an empty vault, not an error.
///
/// A vault that fails to PARSE is an error: overwriting unreadable-but-present
/// credentials would destroy them, and silently returning "empty" would make
/// every login fail with no explanation.
pub fn load_vault<S: SecretStore>(store: &S) -> Result<Vault> {
    let raw = match store.get(VAULT_ACCOUNT)? {
        Some(r) if !r.trim().is_empty() => r,
        _ => return Ok(Vault::default()),
    };
    let vault: Vault = serde_json::from_str(&raw)
        .map_err(|e| Error::Internal(format!("credential vault is corrupt: {e}")))?;
    if vault.version > VAULT_VERSION {
        return Err(Error::Internal(format!(
            "credential vault version {} is newer than this build (v{VAULT_VERSION})",
            vault.version
        )));
    }
    Ok(vault)
}

fn save_vault<S: SecretStore>(store: &S, vault: &Vault) -> Result<()> {
    let json = serde_json::to_string(vault)
        .map_err(|e| Error::Internal(format!("serialize credential vault: {e}")))?;
    store.set(VAULT_ACCOUNT, &json)
}

/// Serialized load → mutate → save.
pub fn update_vault<S: SecretStore, F: FnOnce(&mut Vault)>(store: &S, f: F) -> Result<()> {
    let _g = vault_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut vault = load_vault(store)?;
    f(&mut vault);
    vault.version = VAULT_VERSION;
    save_vault(store, &vault)
}

/// One host's credentials: vault first, then the legacy per-host items.
///
/// The fallback is what lets an un-migrated install keep working — and what
/// lets migration be a deliberate, user-initiated step rather than something
/// that must succeed before the app can log in at all.
pub fn get_host_creds<S: SecretStore>(store: &S, host: &str) -> Result<HostCreds> {
    let vault = load_vault(store)?;
    if let Some(c) = vault.hosts.get(host) {
        return Ok(c.clone());
    }
    let legacy = HostCreds {
        password: store.get(&password_acct(host))?.unwrap_or_default(),
        otpauth: store.get(&otpauth_acct(host))?.unwrap_or_default(),
    };

    // OPPORTUNISTIC MIGRATION. Reaching this line means the legacy items were
    // just read successfully — i.e. their per-item authorization has ALREADY
    // been paid for this build. Folding them into the vault right now is
    // therefore free: it adds no prompt, and it means the next update asks once
    // for the vault instead of twice per host.
    //
    // This used to be deferred to an explicit "consolidate" button, on the
    // reasoning that the migration costs one last round of the old prompts. That
    // was wrong: the user pays that round on EVERY update regardless, so
    // deferring it just meant paying repeatedly and never collecting the
    // benefit. Observed live — five releases in, the vault still did not exist
    // and all twelve items were still being re-authorized every time.
    //
    // Best-effort: a failure here must never fail the read. The caller wants to
    // log in; consolidation is housekeeping.
    if !legacy.password.is_empty() || !legacy.otpauth.trim().is_empty() {
        match set_host_creds(store, host, legacy.clone()) {
            Ok(()) => log::info!("[{host}] credentials folded into the single Keychain item"),
            Err(e) => log::warn!("[{host}] could not consolidate credentials (will retry): {e}"),
        }
    }
    Ok(legacy)
}

/// Write one host's credentials into the vault, then remove that host's legacy
/// items so the secret is not left duplicated in a second place.
///
/// Legacy removal is best-effort: the vault write is what matters, and failing
/// the whole operation because a stale copy could not be deleted would block a
/// password change for no user benefit.
pub fn set_host_creds<S: SecretStore>(
    store: &S,
    host: &str,
    creds: HostCreds,
) -> Result<()> {
    update_vault(store, |v| {
        v.hosts.insert(host.to_string(), creds);
    })?;
    let _ = store.delete(&password_acct(host));
    let _ = store.delete(&otpauth_acct(host));
    Ok(())
}

/// Remove one host from the vault AND its legacy items.
pub fn delete_host_creds<S: SecretStore>(store: &S, host: &str) -> Result<()> {
    // Only rewrite the vault if the host is actually in it — otherwise a
    // delete for an unknown host would pointlessly re-save (and, on a corrupt
    // vault, fail) when there was nothing to do.
    let vault = load_vault(store)?;
    if vault.hosts.contains_key(host) {
        update_vault(store, |v| {
            v.hosts.remove(host);
        })?;
    }
    let _ = store.delete(&password_acct(host));
    let _ = store.delete(&otpauth_acct(host));
    Ok(())
}

/// Outcome of a migration run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MigrationReport {
    /// Hosts folded into the vault by this run.
    pub migrated: usize,
    /// Hosts that already lived in the vault.
    pub already: usize,
    /// Hosts with no credentials in either place.
    pub missing: usize,
}

/// Fold every host's legacy per-host items into the vault, then delete them.
///
/// This is the one operation that still costs the old per-item prompts — once.
/// After it, there is a single item to authorize on every future update.
///
/// SAFETY ORDERING: the legacy items are deleted only AFTER the vault has been
/// written and read back successfully. A crash mid-way leaves both copies,
/// which the read path handles (vault wins) — never neither.
pub fn migrate_to_vault<S: SecretStore>(store: &S, hosts: &[String]) -> Result<MigrationReport> {
    let mut report = MigrationReport::default();
    let mut pending: Vec<(String, HostCreds)> = Vec::new();

    {
        let vault = load_vault(store)?;
        for host in hosts {
            if vault.hosts.contains_key(host) {
                report.already += 1;
                continue;
            }
            let password = store.get(&password_acct(host))?.unwrap_or_default();
            let otpauth = store.get(&otpauth_acct(host))?.unwrap_or_default();
            if password.is_empty() && otpauth.trim().is_empty() {
                report.missing += 1;
                continue;
            }
            pending.push((host.clone(), HostCreds { password, otpauth }));
        }
    }

    if pending.is_empty() {
        return Ok(report);
    }

    update_vault(store, |v| {
        for (host, creds) in &pending {
            v.hosts.insert(host.clone(), creds.clone());
        }
    })?;

    // Verify before destroying the originals.
    let written = load_vault(store)?;
    for (host, creds) in &pending {
        match written.hosts.get(host) {
            Some(stored) if stored == creds => {
                let _ = store.delete(&password_acct(host));
                let _ = store.delete(&otpauth_acct(host));
                report.migrated += 1;
            }
            _ => {
                return Err(Error::Internal(format!(
                    "credential vault did not read back correctly for {host}; \
                     leaving the original entries untouched"
                )));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeStore {
        map: RefCell<HashMap<String, String>>,
    }
    impl SecretStore for FakeStore {
        fn get(&self, a: &str) -> Result<Option<String>> {
            Ok(self.map.borrow().get(a).cloned())
        }
        fn set(&self, a: &str, v: &str) -> Result<()> {
            self.map.borrow_mut().insert(a.into(), v.into());
            Ok(())
        }
        fn delete(&self, a: &str) -> Result<()> {
            self.map.borrow_mut().remove(a);
            Ok(())
        }
    }

    fn legacy(store: &FakeStore, host: &str, pw: &str, otp: &str) {
        store.set(&password_acct(host), pw).unwrap();
        store.set(&otpauth_acct(host), otp).unwrap();
    }

    /// The real collision guarantee: legacy per-host accounts always carry a
    /// suffix, so no host name — not even one spelled exactly like the vault
    /// account, which `is_safe_host_name` permits — can produce it.
    #[test]
    fn vault_account_cannot_collide_with_a_host_entry() {
        for host in ["k6", VAULT_ACCOUNT, "_underscore", "__ssh2fa_vault__"] {
            assert_ne!(password_acct(host), VAULT_ACCOUNT, "host {host:?}");
            assert_ne!(otpauth_acct(host), VAULT_ACCOUNT, "host {host:?}");
        }
        // And a host spelled like the vault really is accepted as a host name —
        // the comment used to claim otherwise.
        assert!(crate::model::is_safe_host_name(VAULT_ACCOUNT));
    }

    /// Even a host named exactly like the vault account round-trips without
    /// disturbing the vault itself.
    #[test]
    fn a_host_named_like_the_vault_does_not_corrupt_it() {
        let s = FakeStore::default();
        set_host_creds(&s, "k6", HostCreds { password: "real".into(), otpauth: "o".into() })
            .unwrap();
        set_host_creds(&s, VAULT_ACCOUNT, HostCreds { password: "odd".into(), otpauth: "o2".into() })
            .unwrap();
        assert_eq!(get_host_creds(&s, "k6").unwrap().password, "real");
        assert_eq!(get_host_creds(&s, VAULT_ACCOUNT).unwrap().password, "odd");
        assert_eq!(s.map.borrow().len(), 1, "still one item");
    }

    #[test]
    fn missing_vault_is_empty_not_an_error() {
        let s = FakeStore::default();
        assert_eq!(load_vault(&s).unwrap(), Vault::default());
    }

    /// A corrupt vault must FAIL loudly. Treating it as empty would overwrite
    /// every credential the user has on the next save.
    #[test]
    fn corrupt_vault_is_an_error() {
        let s = FakeStore::default();
        s.set(VAULT_ACCOUNT, "not json").unwrap();
        assert!(load_vault(&s).is_err());
    }

    #[test]
    fn newer_vault_version_is_refused() {
        let s = FakeStore::default();
        s.set(VAULT_ACCOUNT, r#"{"version":99,"hosts":{}}"#).unwrap();
        assert!(load_vault(&s).is_err(), "must not silently downgrade");
    }

    #[test]
    fn set_and_get_round_trip_through_the_vault() {
        let s = FakeStore::default();
        set_host_creds(&s, "k6", HostCreds { password: "pw".into(), otpauth: "otp".into() })
            .unwrap();
        let got = get_host_creds(&s, "k6").unwrap();
        assert_eq!(got.password, "pw");
        assert_eq!(got.otpauth, "otp");
        // And it really is ONE item.
        assert!(s.map.borrow().contains_key(VAULT_ACCOUNT));
        assert!(!s.map.borrow().contains_key(&password_acct("k6")),
                "the legacy copy must not be left behind");
    }

    /// The whole point: N hosts occupy ONE Keychain item.
    #[test]
    fn many_hosts_still_occupy_a_single_item() {
        let s = FakeStore::default();
        for h in ["h1", "h2", "h3", "h4", "h5", "h6"] {
            set_host_creds(&s, h, HostCreds { password: "p".into(), otpauth: "o".into() })
                .unwrap();
        }
        assert_eq!(s.map.borrow().len(), 1, "6 hosts must be 1 item, not 12");
    }

    /// An un-migrated install must keep working.
    #[test]
    fn reads_fall_back_to_legacy_items() {
        let s = FakeStore::default();
        legacy(&s, "k6", "oldpw", "oldotp");
        let got = get_host_creds(&s, "k6").unwrap();
        assert_eq!(got.password, "oldpw");
        assert_eq!(got.otpauth, "oldotp");
    }

    /// THE fix for "it keeps asking every update": a legacy read migrates the
    /// host into the vault immediately, because the authorization for those
    /// items has just been paid. Waiting for an explicit button meant the
    /// migration never happened and the prompts repeated forever.
    #[test]
    fn a_legacy_read_consolidates_immediately() {
        let s = FakeStore::default();
        legacy(&s, "k6", "pw", "otp");
        assert_eq!(s.map.borrow().len(), 2, "starts as two items");

        let got = get_host_creds(&s, "k6").unwrap();
        assert_eq!(got.password, "pw", "the read still returns the credentials");

        assert_eq!(s.map.borrow().len(), 1, "and they now live in ONE item");
        assert!(s.map.borrow().contains_key(VAULT_ACCOUNT));
        // A second read is served from the vault, unchanged.
        assert_eq!(get_host_creds(&s, "k6").unwrap().password, "pw");
    }

    /// Migration must never break the read it rode along on.
    #[test]
    fn a_failed_consolidation_still_returns_the_credentials() {
        struct WriteFails {
            map: RefCell<HashMap<String, String>>,
        }
        impl SecretStore for WriteFails {
            fn get(&self, a: &str) -> Result<Option<String>> {
                Ok(self.map.borrow().get(a).cloned())
            }
            fn set(&self, _a: &str, _v: &str) -> Result<()> {
                Err(Error::Internal("keychain is read-only right now".into()))
            }
            fn delete(&self, a: &str) -> Result<()> {
                self.map.borrow_mut().remove(a);
                Ok(())
            }
        }
        let s = WriteFails { map: RefCell::new(HashMap::new()) };
        s.map.borrow_mut().insert(password_acct("k6"), "pw".into());
        s.map.borrow_mut().insert(otpauth_acct("k6"), "otp".into());

        let got = get_host_creds(&s, "k6").unwrap();
        assert_eq!(got.password, "pw", "a failed migration must not fail the login");
        assert_eq!(got.otpauth, "otp");
    }

    /// Nothing stored → nothing written (no empty vault entries).
    #[test]
    fn a_read_for_an_unknown_host_writes_nothing() {
        let s = FakeStore::default();
        let got = get_host_creds(&s, "ghost").unwrap();
        assert!(got.password.is_empty());
        assert!(s.map.borrow().is_empty(), "must not create an empty vault entry");
    }

    /// The vault WINS over a stale legacy copy (e.g. a crash between write and
    /// delete) — otherwise a password change could silently revert.
    #[test]
    fn vault_wins_over_a_stale_legacy_copy() {
        let s = FakeStore::default();
        legacy(&s, "k6", "stale", "staleotp");
        update_vault(&s, |v| {
            v.hosts.insert("k6".into(), HostCreds { password: "new".into(), otpauth: "newotp".into() });
        })
        .unwrap();
        assert_eq!(get_host_creds(&s, "k6").unwrap().password, "new");
    }

    #[test]
    fn migration_folds_legacy_items_in_and_removes_them() {
        let s = FakeStore::default();
        legacy(&s, "k6", "pw6", "otp6");
        legacy(&s, "k8", "pw8", "otp8");

        let r = migrate_to_vault(&s, &["k6".into(), "k8".into()]).unwrap();
        assert_eq!(r.migrated, 2);
        assert_eq!(s.map.borrow().len(), 1, "everything now lives in one item");
        assert_eq!(get_host_creds(&s, "k6").unwrap().password, "pw6");
        assert_eq!(get_host_creds(&s, "k8").unwrap().otpauth, "otp8");
    }

    #[test]
    fn migration_is_idempotent() {
        let s = FakeStore::default();
        legacy(&s, "k6", "pw", "otp");
        assert_eq!(migrate_to_vault(&s, &["k6".into()]).unwrap().migrated, 1);
        let second = migrate_to_vault(&s, &["k6".into()]).unwrap();
        assert_eq!(second.migrated, 0);
        assert_eq!(second.already, 1);
        assert_eq!(get_host_creds(&s, "k6").unwrap().password, "pw");
    }

    #[test]
    fn migration_skips_hosts_with_nothing_stored() {
        let s = FakeStore::default();
        let r = migrate_to_vault(&s, &["ghost".into()]).unwrap();
        assert_eq!(r.missing, 1);
        assert_eq!(r.migrated, 0);
    }

    #[test]
    fn delete_removes_from_both_places() {
        let s = FakeStore::default();
        legacy(&s, "k6", "pw", "otp");
        set_host_creds(&s, "k6", HostCreds { password: "pw".into(), otpauth: "otp".into() })
            .unwrap();
        delete_host_creds(&s, "k6").unwrap();
        let got = get_host_creds(&s, "k6").unwrap();
        assert!(got.password.is_empty() && got.otpauth.is_empty());
    }

    /// Deleting one host must not disturb another.
    #[test]
    fn delete_is_surgical() {
        let s = FakeStore::default();
        set_host_creds(&s, "k6", HostCreds { password: "a".into(), otpauth: "x".into() }).unwrap();
        set_host_creds(&s, "k8", HostCreds { password: "b".into(), otpauth: "y".into() }).unwrap();
        delete_host_creds(&s, "k6").unwrap();
        assert_eq!(get_host_creds(&s, "k8").unwrap().password, "b");
    }

    /// Concurrent re-keys must both land (lost-update regression).
    #[test]
    fn concurrent_updates_all_survive() {
        let s = std::sync::Arc::new(SyncFakeStore::default());
        std::thread::scope(|sc| {
            for i in 0..8 {
                let s = std::sync::Arc::clone(&s);
                sc.spawn(move || {
                    set_host_creds(&*s, &format!("host{i}"),
                                   HostCreds { password: format!("p{i}"), otpauth: "o".into() })
                        .unwrap();
                });
            }
        });
        let v = load_vault(&*s).unwrap();
        assert_eq!(v.hosts.len(), 8, "every concurrent write must survive");
    }

    /// Thread-safe variant for the concurrency test.
    #[derive(Default)]
    struct SyncFakeStore {
        map: Mutex<HashMap<String, String>>,
    }
    impl SecretStore for SyncFakeStore {
        fn get(&self, a: &str) -> Result<Option<String>> {
            Ok(self.map.lock().unwrap().get(a).cloned())
        }
        fn set(&self, a: &str, v: &str) -> Result<()> {
            self.map.lock().unwrap().insert(a.into(), v.into());
            Ok(())
        }
        fn delete(&self, a: &str) -> Result<()> {
            self.map.lock().unwrap().remove(a);
            Ok(())
        }
    }
}
