use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

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
pub struct HostMeta {
    /// Whether the host should auto-connect at daemon start.
    #[serde(rename = "autoConnect")]
    pub auto_connect: bool,
}

impl Default for HostMeta {
    fn default() -> Self {
        Self { auto_connect: false }
    }
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

/// Atomically write the per-host metadata map to `path`.
///
/// Writes to `<path>.tmp`, fsyncs the file, then renames over `path`
/// (mirrors `_atomic_write_json` in credentials.py). Refuses to write if the
/// on-disk schema is newer than SCHEMA_VERSION.
pub fn save_meta(path: &Path, hosts: &HashMap<String, HostMeta>) -> Result<()> {
    // If the file already exists with a newer schema, refuse to downgrade.
    if path.exists() {
        let existing = std::fs::read_to_string(path).map_err(Error::Io)?;
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
        let mut f = std::fs::File::create(&tmp_path).map_err(Error::Io)?;
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
        assert_eq!(loaded["k6"].auto_connect, true);
        assert_eq!(loaded["k8"].auto_connect, false);
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
}
