use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::Tunnel;
use crate::model::status::TunnelStatus;

// ---------------------------------------------------------------------------
// On-disk schema helpers
// ---------------------------------------------------------------------------

/// The persisted subset of a tunnel entry (mirrors PERSISTED_FIELDS in tunnels.py).
#[derive(Serialize, Deserialize, Debug)]
struct PersistedTunnel {
    local_port: u16,
    #[serde(default)]
    remote_port: u16,
    #[serde(default)]
    jump_candidates: Option<Vec<String>>,
    #[serde(default)]
    last_node: Option<String>,
    #[serde(default)]
    last_user: Option<String>,
    #[serde(default)]
    auto_start: bool,
    #[serde(default)]
    post_connect_cmd: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    url_path: Option<String>,
    #[serde(default)]
    wants_alive: bool,
    // status is runtime-only; we keep it for round-trip read but don't
    // store it — the daemon always resets to idle on load.
    #[serde(default)]
    status: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct TunnelsFile {
    tunnels: HashMap<String, Value>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load tunnels from `path`.
///
/// - Missing file → empty Vec (not an error).
/// - Malformed JSON → logged + empty Vec (file is NOT overwritten).
/// - Individual entries that fail to deserialize, or that are missing
///   `local_port`, are skipped with a warning (mirrors tunnels.py `load()`).
pub fn load_tunnels(path: &Path) -> Vec<Tunnel> {
    if !path.exists() {
        return Vec::new();
    }

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            log::error!("Failed to read tunnels file {:?}: {}", path, e);
            return Vec::new();
        }
    };

    let file: TunnelsFile = match serde_json::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            log::error!("Failed to parse tunnels file {:?}: {}", path, e);
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for (name, raw) in &file.tunnels {
        // The Python code checks `"local_port" not in cfg` as a quick guard
        // before a full deserialize. Mirror that: if the raw JSON object does
        // not have a "local_port" key, skip with a warning.
        let obj = match raw.as_object() {
            Some(o) => o,
            None => {
                log::error!(
                    "tunnels.json: skipping malformed entry {:?} (not an object)",
                    name
                );
                continue;
            }
        };
        if !obj.contains_key("local_port") {
            log::error!(
                "tunnels.json: skipping malformed entry {:?} (missing local_port)",
                name
            );
            continue;
        }

        let persisted: PersistedTunnel = match serde_json::from_value(raw.clone()) {
            Ok(p) => p,
            Err(e) => {
                log::error!(
                    "tunnels.json: skipping malformed entry {:?}: {}",
                    name,
                    e
                );
                continue;
            }
        };

        let remote_port = if persisted.remote_port == 0 {
            persisted.local_port
        } else {
            persisted.remote_port
        };

        let tunnel = Tunnel {
            name: name.clone(),
            local_port: persisted.local_port,
            remote_port,
            jump_candidates: persisted.jump_candidates,
            last_node: persisted.last_node,
            last_user: persisted.last_user,
            auto_start: persisted.auto_start,
            post_connect_cmd: persisted.post_connect_cmd,
            tags: persisted.tags,
            url_path: persisted.url_path,
            wants_alive: persisted.wants_alive,
            // Runtime fields reset to defaults on load
            status: TunnelStatus::Idle,
            active_jump: None,
            last_msg: "Ready".to_owned(),
            last_alive_at: 0.0,
            total_uptime_sec: 0.0,
            connect_count: 0,
            fail_count: 0,
        };
        out.push(tunnel);
    }
    out
}

/// Atomically write `tuns` to `path`.
///
/// Writes to `<path>.tmp`, fsyncs the file, renames over `path`, then fsyncs
/// the directory (mirrors tunnels.py `save()`). Only PERSISTED_FIELDS are
/// written; runtime fields are dropped.
pub fn save_tunnels(path: &Path, tuns: &[Tunnel]) -> Result<()> {
    let tmp_path = {
        let mut p = path.to_path_buf();
        let file_name = p
            .file_name()
            .map(|n| {
                let mut s = n.to_os_string();
                s.push(".tmp");
                s
            })
            .unwrap_or_else(|| std::ffi::OsString::from("tunnels.json.tmp"));
        p.set_file_name(file_name);
        p
    };

    // Build the persisted map
    let mut tunnels_map: HashMap<String, serde_json::Value> = HashMap::new();
    for t in tuns {
        let persisted = serde_json::json!({
            "local_port": t.local_port,
            "remote_port": t.remote_port,
            "jump_candidates": t.jump_candidates,
            "last_node": t.last_node,
            "last_user": t.last_user,
            "auto_start": t.auto_start,
            "post_connect_cmd": t.post_connect_cmd,
            "tags": t.tags,
            "url_path": t.url_path,
            "wants_alive": t.wants_alive,
        });
        tunnels_map.insert(t.name.clone(), persisted);
    }

    let payload = serde_json::json!({ "tunnels": tunnels_map });
    let json_text = serde_json::to_string_pretty(&payload)
        .map_err(|e| Error::Internal(format!("serialize tunnels: {e}")))?;

    // Write to tmp, fsync, then rename
    {
        let mut f = std::fs::File::create(&tmp_path)
            .map_err(|e| Error::Io(e))?;
        f.write_all(json_text.as_bytes())
            .map_err(|e| Error::Io(e))?;
        f.flush().map_err(|e| Error::Io(e))?;
        f.sync_all().map_err(|e| Error::Io(e))?;
    }

    std::fs::rename(&tmp_path, path).map_err(|e| Error::Io(e))?;

    // fsync the directory so the rename is durable
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
    fn round_trip_and_skip_malformed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("tunnels.json");
        std::fs::write(
            &p,
            r#"{"tunnels":{
            "good":{"local_port":8090,"remote_port":8090,"status":"idle"},
            "broken":{"remote_port":8888}
        }}"#,
        )
        .unwrap();
        let tuns = load_tunnels(&p);
        assert_eq!(tuns.len(), 1);
        assert_eq!(tuns[0].name, "good");
        assert_eq!(tuns[0].local_port, 8090);
        // save then reload preserves the good one
        save_tunnels(&p, &tuns).unwrap();
        assert_eq!(load_tunnels(&p).len(), 1);
    }

    #[test]
    fn missing_file_is_empty() {
        let d = tempfile::tempdir().unwrap();
        assert!(load_tunnels(&d.path().join("nope.json")).is_empty());
    }
}
