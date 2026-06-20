use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

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
    #[serde(default)]
    direct_host: Option<String>,
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

/// Boot-time guard: if `path` exists, is non-empty, and does NOT parse as a
/// tunnels file, copy it aside to `<path>.corrupt-<unix-secs>` before anyone
/// loads (and therefore before any later persist overwrites it).
///
/// WHY: a hand-edited/corrupt tunnels.json loads as EMPTY, and the first
/// persist (any tunnel transition) then rewrote the file from the empty
/// in-memory list — silently destroying the user's only copy. passwords.json
/// refuses such writes; tunnels.json instead preserves the evidence and
/// carries on (tunnels are recreatable, but not silently-losable).
///
/// Returns `true` if a backup was made.
pub fn backup_if_unparseable(path: &Path) -> bool {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return false, // missing/unreadable — nothing to preserve
    };
    if text.trim().is_empty() {
        return false;
    }
    if serde_json::from_str::<TunnelsFile>(&text).is_ok() {
        return false;
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = {
        let mut p = path.to_path_buf();
        let name = p
            .file_name()
            .map(|n| {
                let mut s = n.to_os_string();
                s.push(format!(".corrupt-{secs}"));
                s
            })
            .unwrap_or_else(|| std::ffi::OsString::from(format!("tunnels.json.corrupt-{secs}")));
        p.set_file_name(name);
        p
    };
    match std::fs::copy(path, &backup) {
        Ok(_) => {
            log::error!(
                "tunnels.json is unparseable — preserved a copy at {:?} before the daemon \
                 overwrites it. Fix and restore it manually if needed.",
                backup
            );
            true
        }
        Err(e) => {
            log::error!("tunnels.json is unparseable AND backing it up failed: {e}");
            false
        }
    }
}

/// Boot-time sweep of leftover atomic-write temp files
/// (`tunnels.json.<pid>.<seq>.tmp`) — a SIGKILL'd daemon (the standard
/// zero-relogin deploy) leaks one per interrupted save, and nothing else
/// ever removes them. Safe at boot: the accept loop isn't running, so no
/// concurrent writer exists.
pub fn sweep_stale_tmp(path: &Path) -> usize {
    let (dir, prefix) = match (path.parent(), path.file_name().and_then(|n| n.to_str())) {
        (Some(d), Some(f)) => (d, format!("{f}.")),
        _ => return 0,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(&prefix) && s.ends_with(".tmp") {
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        log::info!("swept {removed} stale tunnels.json temp file(s)");
    }
    removed
}

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
            direct_host: persisted.direct_host,
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
    // The on-disk map is a HashMap — iteration order is nondeterministic per
    // process, so without this the UI row order shuffled on every daemon
    // restart. Sort by name for a stable, predictable order.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Atomically write `tuns` to `path`.
///
/// Writes to `<path>.tmp`, fsyncs the file, renames over `path`, then fsyncs
/// the directory (mirrors tunnels.py `save()`). Only PERSISTED_FIELDS are
/// written; runtime fields are dropped.
pub fn save_tunnels(path: &Path, tuns: &[Tunnel]) -> Result<()> {
    // UNIQUE temp path per call. The persist sites are intentionally off the
    // State lock (no fsync under the lock — that would wedge the daemon), so two
    // writers (two IPC handler threads, or a handler + a maintenance worker) can
    // run save_tunnels concurrently. A SHARED "tunnels.json.tmp" would let them
    // truncate-interleave each other's tmp or fail the rename with ENOENT. A
    // per-call name (pid + monotonic counter) gives each writer its own tmp, so
    // the atomic rename yields safe last-writer-wins: tunnels.json is always a
    // complete snapshot from exactly one writer.
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp_path = {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let mut p = path.to_path_buf();
        let suffix = format!(".{pid}.{seq}.tmp");
        let file_name = p
            .file_name()
            .map(|n| {
                let mut s = n.to_os_string();
                s.push(&suffix);
                s
            })
            .unwrap_or_else(|| std::ffi::OsString::from(format!("tunnels.json{suffix}")));
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
            "direct_host": t.direct_host,
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

    // Write to the unique tmp, fsync, then atomically rename. On ANY failure,
    // unlink the unique tmp so an aborted writer can't leak temp files.
    let write_result = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp_path).map_err(Error::Io)?;
        f.write_all(json_text.as_bytes()).map_err(Error::Io)?;
        f.flush().map_err(Error::Io)?;
        f.sync_all().map_err(Error::Io)?;
        std::fs::rename(&tmp_path, path).map_err(Error::Io)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_result?;

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

    /// REGRESSION (silent data loss): an unparseable tunnels.json must be
    /// preserved aside at boot — the first persist would otherwise rewrite it
    /// from the empty in-memory list, destroying the user's only copy.
    #[test]
    fn backup_if_unparseable_preserves_corrupt_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("tunnels.json");
        std::fs::write(&p, "{ this is not json").unwrap();

        assert!(backup_if_unparseable(&p), "corrupt file must be backed up");
        let backups: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1, "exactly one backup");
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            "{ this is not json"
        );

        // Valid / empty / missing files are left alone.
        std::fs::write(&p, r#"{"tunnels":{}}"#).unwrap();
        assert!(!backup_if_unparseable(&p));
        std::fs::write(&p, "").unwrap();
        assert!(!backup_if_unparseable(&p));
        assert!(!backup_if_unparseable(&d.path().join("missing.json")));
    }

    /// SIGKILL deploys leak <file>.<pid>.<seq>.tmp files; the boot sweep must
    /// remove them and ONLY them.
    #[test]
    fn sweep_stale_tmp_removes_only_tmp_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("tunnels.json");
        std::fs::write(&p, r#"{"tunnels":{}}"#).unwrap();
        std::fs::write(d.path().join("tunnels.json.123.0.tmp"), "x").unwrap();
        std::fs::write(d.path().join("tunnels.json.456.7.tmp"), "x").unwrap();
        std::fs::write(d.path().join("tunnels.json.corrupt-9"), "x").unwrap(); // NOT a tmp
        std::fs::write(d.path().join("other.tmp"), "x").unwrap(); // different prefix

        assert_eq!(sweep_stale_tmp(&p), 2);
        assert!(p.exists());
        assert!(d.path().join("tunnels.json.corrupt-9").exists());
        assert!(d.path().join("other.tmp").exists());
    }

    /// The on-disk map is a HashMap — without an explicit sort the loaded
    /// order shuffled per process (UI rows reordered on every restart).
    #[test]
    fn load_order_is_deterministic_by_name() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("tunnels.json");
        std::fs::write(
            &p,
            r#"{"tunnels":{
            "zeta":{"local_port":1,"remote_port":1},
            "alpha":{"local_port":2,"remote_port":2},
            "mid":{"local_port":3,"remote_port":3}
        }}"#,
        )
        .unwrap();
        let names: Vec<String> = load_tunnels(&p).into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

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

    #[test]
    fn concurrent_saves_yield_valid_file_and_no_tmp_leak() {
        // With a SHARED tmp path, concurrent off-lock saves truncate-interleave
        // or fail the rename with ENOENT (→ unwrap panic here). With a per-call
        // unique tmp, every writer renames its own complete snapshot, so the
        // published file is always valid and no .tmp leaks.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("tunnels.json");
        std::fs::write(
            &p,
            r#"{"tunnels":{"t1":{"local_port":9001,"remote_port":9001,"status":"idle"}}}"#,
        )
        .unwrap();
        let base = load_tunnels(&p);
        assert_eq!(base.len(), 1);

        let mut handles = vec![];
        for _ in 0..16 {
            let path = p.clone();
            let tuns = base.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..20 {
                    save_tunnels(&path, &tuns).expect("concurrent save must not fail");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Published file is a complete, valid snapshot.
        let final_tuns = load_tunnels(&p);
        assert_eq!(final_tuns.len(), 1);
        assert_eq!(final_tuns[0].name, "t1");

        // No leaked per-call tmp files.
        let leftover: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .map(|e| e.file_name())
            .collect();
        assert!(leftover.is_empty(), "no .tmp files should leak, found {leftover:?}");
    }

    /// REGRESSION: a direct-mode tunnel's `direct_host` must survive a
    /// save→load round-trip. (save_tunnels formerly dropped the field, so the
    /// first daemon persist silently reverted a direct tunnel to compute mode.)
    #[test]
    fn direct_host_survives_save_and_reload() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("tunnels.json");
        std::fs::write(
            &p,
            r#"{"tunnels":{"web":{"local_port":9000,"remote_port":9000,"direct_host":"loginhost"}}}"#,
        )
        .unwrap();
        let loaded = load_tunnels(&p);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].direct_host.as_deref(), Some("loginhost"));
        // Persist them back out, then reload — direct_host must still be there.
        save_tunnels(&p, &loaded).unwrap();
        let reloaded = load_tunnels(&p);
        assert_eq!(reloaded[0].direct_host.as_deref(), Some("loginhost"),
                   "save_tunnels must not drop direct_host");
    }
}
