use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::config::paths::config_dir;
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Schema constants
// ---------------------------------------------------------------------------

const SCHEMA_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Per-host metadata stored in passwords.json v2.
///
/// The actual password and otpauth URL live in the macOS Keychain; this struct
/// holds only the JSON-backed metadata for each host.
///
/// Schema (passwords.json v2):
/// ```json
/// {
///   "schema": 2,
///   "hosts": {
///     "k6": { "autoConnect": true },
///     "k8": { "autoConnect": false }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[derive(Default)]
pub struct HostMeta {
    /// Whether the host should auto-connect at daemon start.
    #[serde(rename = "autoConnect")]
    pub auto_connect: bool,
}


// On-disk v2 layout
#[derive(Debug, Serialize, Deserialize)]
struct PasswordsFile {
    schema: u32,
    #[serde(default)]
    hosts: HashMap<String, HostMeta>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Path to `passwords.json` inside `config_dir()`.
pub fn passwords_path() -> PathBuf {
    config_dir().join("passwords.json")
}

/// Load per-host metadata from `path`.
///
/// - Missing file → empty map.
/// - Malformed JSON or unexpected schema version → logged + empty map.
/// - Only schema v2 files are accepted; v1 (legacy plaintext) is intentionally
///   not migrated here — migration is handled by the creds module.
pub fn load_meta(path: &Path) -> HashMap<String, HostMeta> {
    if !path.exists() {
        return HashMap::new();
    }

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            log::error!("Failed to read passwords file {:?}: {}", path, e);
            return HashMap::new();
        }
    };

    let file: PasswordsFile = match serde_json::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            log::error!("Failed to parse passwords file {:?}: {}", path, e);
            return HashMap::new();
        }
    };

    if file.schema != SCHEMA_VERSION {
        log::warn!(
            "[passwords] passwords.json schema v{} not understood (expected v{}); \
             skipping load. Run migration or a newer build.",
            file.schema,
            SCHEMA_VERSION
        );
        return HashMap::new();
    }

    file.hosts
}

/// Process-global lock serializing every load→modify→save of passwords.json.
/// Two handler threads doing concurrent read-modify-write (host toggle +
/// host_add) could otherwise interleave and silently drop one side's update.
fn meta_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serialized read-modify-write of the metadata map: lock → load → mutate →
/// save. ALL incremental metadata updates (toggle persistence, host_add) must
/// go through this — calling `load_meta` + `save_meta` separately reintroduces
/// the lost-update race.
pub fn update_meta<F: FnOnce(&mut HashMap<String, HostMeta>)>(path: &Path, f: F) -> Result<()> {
    let _g = meta_write_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut hosts = load_meta(path);
    f(&mut hosts);
    save_meta(path, &hosts)
}

/// Atomically write the per-host metadata map to `path`.
///
/// Writes to `<path>.tmp`, fsyncs the file, then renames over `path`
/// (mirrors `_atomic_write_json` in credentials.py). Refuses to write if the
/// on-disk schema is newer than SCHEMA_VERSION, and refuses to OVERWRITE a
/// file that is not parseable v2 — that's an un-migrated legacy v1 file (or
/// corrupt data). `load_meta` returns an EMPTY map for such a file, so a
/// blind load→modify→save (e.g. a host toggle right after the boot migration
/// timed out on a locked Keychain) would rewrite passwords.json as v2 with
/// ONE host: every legacy entry silently dropped AND migration permanently
/// skipped (the next boot sees schema 2). The boot migration commits through
/// [`commit_migration_meta`], which is the only writer allowed to replace a
/// v1 file.
pub fn save_meta(path: &Path, hosts: &HashMap<String, HostMeta>) -> Result<()> {
    if path.exists() {
        let existing = std::fs::read_to_string(path).map_err(Error::Io)?;
        let trimmed = existing.trim();
        match serde_json::from_str::<PasswordsFile>(&existing) {
            Ok(existing_file) if existing_file.schema > SCHEMA_VERSION => {
                return Err(Error::Internal(format!(
                    "passwords.json schema v{} is newer than this build (v{}); \
                     refusing to write to avoid data loss",
                    existing_file.schema, SCHEMA_VERSION
                )));
            }
            // An older schema number still parses (unknown fields are ignored)
            // but load_meta returned {} for it — overwriting would lose data
            // exactly like the unparseable-v1 case below.
            Ok(existing_file) if existing_file.schema < SCHEMA_VERSION => {
                return Err(Error::Internal(format!(
                    "passwords.json schema v{} is older than this build (v{}); \
                     refusing to overwrite — the boot migration converts it",
                    existing_file.schema, SCHEMA_VERSION
                )));
            }
            Ok(_) => {}
            // An empty file / empty object carries no data — safe to claim.
            // ("{}" is what a fresh Python install wrote; refusing it would
            // brick metadata persistence forever, since the migration skips
            // cred-less files and never converts them to v2.)
            Err(_) if trimmed.is_empty() || trimmed == "{}" => {}
            Err(_) => {
                return Err(Error::Internal(
                    "passwords.json is not a v2 file — likely un-migrated legacy v1 \
                     (or corrupt); refusing to overwrite it. The boot migration will \
                     convert it; recover from <passwords.json>.pre-keychain-backup if \
                     needed."
                        .into(),
                ));
            }
        }
    }
    write_meta_atomic(path, hosts)
}

/// Migration-only commit: replaces the file even when the current contents
/// are legacy v1 (that's the whole point of the migration). Still refuses a
/// schema DOWNGRADE.
pub fn commit_migration_meta(path: &Path, hosts: &HashMap<String, HostMeta>) -> Result<()> {
    if path.exists() {
        if let Ok(existing) = std::fs::read_to_string(path) {
            if let Ok(existing_file) = serde_json::from_str::<PasswordsFile>(&existing) {
                if existing_file.schema > SCHEMA_VERSION {
                    return Err(Error::Internal(format!(
                        "passwords.json schema v{} is newer than this build (v{}); \
                         refusing to write to avoid data loss",
                        existing_file.schema, SCHEMA_VERSION
                    )));
                }
            }
        }
    }
    write_meta_atomic(path, hosts)
}

fn write_meta_atomic(path: &Path, hosts: &HashMap<String, HostMeta>) -> Result<()> {
    let file = PasswordsFile {
        schema: SCHEMA_VERSION,
        hosts: hosts.clone(),
    };
    let json_text = serde_json::to_string_pretty(&file)
        .map_err(|e| Error::Internal(format!("serialize passwords: {e}")))?;

    let tmp_path = {
        let mut p = path.to_path_buf();
        let file_name = p
            .file_name()
            .map(|n| {
                let mut s = n.to_os_string();
                s.push(".tmp");
                s
            })
            .unwrap_or_else(|| std::ffi::OsString::from("passwords.json.tmp"));
        p.set_file_name(file_name);
        p
    };

    {
        // 0600: the file holds only v2 metadata (no secrets), but it lives in
        // ~/.ssh and there's no reason for other users to read it. The rename
        // below carries these permissions onto the final path.
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)
            .map_err(Error::Io)?;
        f.write_all(json_text.as_bytes()).map_err(Error::Io)?;
        f.flush().map_err(Error::Io)?;
        f.sync_all().map_err(Error::Io)?;
    }

    std::fs::rename(&tmp_path, path).map_err(Error::Io)?;

    // fsync directory for durability of the rename
    if let Some(dir) = path.parent() {
        if let Ok(dir_file) = std::fs::File::open(dir) {
            let _ = dir_file.sync_all();
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_meta() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("passwords.json");

        let mut hosts = HashMap::new();
        hosts.insert("k6".to_owned(), HostMeta { auto_connect: true });
        hosts.insert("k8".to_owned(), HostMeta { auto_connect: false });

        save_meta(&p, &hosts).unwrap();

        let loaded = load_meta(&p);
        assert_eq!(loaded.len(), 2);
        assert!(loaded["k6"].auto_connect);
        assert!(!loaded["k8"].auto_connect);
    }

    #[test]
    fn save_meta_writes_0600() {
        // The file lives in ~/.ssh; it must not be group/world-readable. The
        // rename must carry the temp file's 0600 onto the final path — even
        // when overwriting a pre-existing looser (0644) file.
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("passwords.json");
        std::fs::write(&p, "{}").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mut hosts = HashMap::new();
        hosts.insert("k8".to_owned(), HostMeta { auto_connect: true });
        save_meta(&p, &hosts).unwrap();

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "passwords.json must be 0600, got {mode:o}");
    }

    #[test]
    fn missing_file_is_empty() {
        let d = tempfile::tempdir().unwrap();
        assert!(load_meta(&d.path().join("nope.json")).is_empty());
    }

    #[test]
    fn unknown_schema_returns_empty() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("passwords.json");
        std::fs::write(
            &p,
            r#"{"schema":99,"hosts":{"k6":{"autoConnect":true}}}"#,
        )
        .unwrap();
        assert!(load_meta(&p).is_empty());
    }

    #[test]
    fn passwords_path_ends_with_filename() {
        // Just verify the function returns a path ending in "passwords.json".
        // We can't assert the exact prefix without knowing $HOME / SSH_CONFIG_PATH.
        let p = passwords_path();
        assert_eq!(p.file_name().unwrap(), "passwords.json");
    }

    /// REGRESSION (legacy-clobber): save_meta over an un-migrated v1 file must
    /// REFUSE. load_meta returns {} for v1, so a blind load→modify→save (host
    /// toggle after a timed-out boot migration) would rewrite the file as v2
    /// with one host — every legacy credential entry dropped AND migration
    /// permanently skipped.
    #[test]
    fn save_meta_refuses_to_overwrite_v1_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("passwords.json");
        // Legacy v1 shape: top-level host entries with plaintext creds, no "schema".
        std::fs::write(
            &p,
            r#"{"k6":{"password":"hunter2","otpauthUrl":"otpauth://totp/x?secret=AAAA","autoConnect":true}}"#,
        )
        .unwrap();

        let mut hosts = HashMap::new();
        hosts.insert("k8".to_owned(), HostMeta { auto_connect: true });
        assert!(save_meta(&p, &hosts).is_err(), "must refuse to clobber v1");
        // And update_meta (which routes through save_meta) propagates the refusal…
        assert!(update_meta(&p, |m| {
            m.insert("k8".to_owned(), HostMeta { auto_connect: true });
        })
        .is_err());
        // …while the v1 contents survive untouched.
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("hunter2"), "v1 file must be untouched");
    }

    /// The migration commit is the ONE writer allowed to replace a v1 file.
    #[test]
    fn commit_migration_meta_replaces_v1_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("passwords.json");
        std::fs::write(
            &p,
            r#"{"k6":{"password":"hunter2","otpauthUrl":"otpauth://totp/x?secret=AAAA"}}"#,
        )
        .unwrap();

        let mut hosts = HashMap::new();
        hosts.insert("k6".to_owned(), HostMeta { auto_connect: true });
        commit_migration_meta(&p, &hosts).unwrap();

        let loaded = load_meta(&p);
        assert_eq!(loaded.len(), 1);
        assert!(loaded["k6"].auto_connect);
    }

    /// An empty (0-byte) file carries no data — save_meta may claim it (a
    /// permanent refusal would brick metadata persistence forever).
    #[test]
    fn save_meta_claims_empty_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("passwords.json");
        std::fs::write(&p, "").unwrap();
        let mut hosts = HashMap::new();
        hosts.insert("k8".to_owned(), HostMeta { auto_connect: false });
        save_meta(&p, &hosts).unwrap();
        assert_eq!(load_meta(&p).len(), 1);
    }

    /// REGRESSION (lost-update): two concurrent update_meta calls must both
    /// land — the old separate load_meta/save_meta pattern let one side's
    /// insert vanish.
    #[test]
    fn update_meta_serializes_concurrent_writers() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("passwords.json");
        save_meta(&p, &HashMap::new()).unwrap();

        std::thread::scope(|s| {
            for i in 0..8 {
                let p = p.clone();
                s.spawn(move || {
                    update_meta(&p, |m| {
                        m.insert(format!("host{i}"), HostMeta { auto_connect: i % 2 == 0 });
                    })
                    .unwrap();
                });
            }
        });

        let loaded = load_meta(&p);
        assert_eq!(loaded.len(), 8, "every concurrent insert must survive");
    }
}
