//! Daemon log rotation — mirrors Python `_rotate_log_if_huge`.
//!
//! Called once at daemon startup *before* the logger is initialised, and then
//! periodically from a dedicated thread ([`spawn_periodic_rotation`]) so a
//! daemon that stays up for weeks can't grow the log without bound (the boot
//! check alone only capped growth ACROSS restarts).
//!
//! Runtime rotation is safe with the live logger because the logger opens the
//! file with O_APPEND: after the in-place truncate, the next write lands at
//! the (new) EOF. Lines appended between the copy and the truncate are lost —
//! a handful of log lines at a 10 MB boundary, accepted (same race the boot
//! rotation always had with a still-exiting previous daemon).

use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
const LOG_PATH: &str = "/tmp/ssh2fa_daemon.log";

/// How many rotated `<log>.<secs>.gz` archives to keep. Older ones are pruned
/// on each rotation so the archives can't accumulate without bound in /tmp.
const KEEP_ROTATIONS: usize = 3;

/// Convenience entry-point used by `main.rs`.
pub fn rotate_daemon_log() {
    rotate_log_if_huge(LOG_PATH, DEFAULT_MAX_BYTES);
}

/// How often the runtime rotation check runs. The check itself is one
/// `fs::metadata` when under the threshold — effectively free.
const PERIODIC_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

/// Spawn the detached background thread that re-checks the log size every
/// ~10 minutes. Panics inside one iteration are caught so a single bad
/// rotation can never kill the thread (let alone the daemon).
pub fn spawn_periodic_rotation() {
    let res = std::thread::Builder::new()
        .name("log-rotation".into())
        .spawn(|| loop {
            std::thread::sleep(PERIODIC_CHECK_INTERVAL);
            let _ = std::panic::catch_unwind(rotate_daemon_log);
        });
    if let Err(e) = res {
        log::warn!("log-rotation thread failed to spawn (boot-only rotation remains): {e}");
    }
}

/// If `path` exists and its size >= `max_bytes`:
/// - gzip the current contents to `<path>.<unix_secs>.gz`
/// - truncate `path` to 0 bytes so a fresh log starts
///
/// Any error is printed to stderr and swallowed — this must never crash
/// the daemon.
pub fn rotate_log_if_huge(path: &str, max_bytes: u64) {
    if let Err(e) = try_rotate(path, max_bytes) {
        eprintln!("[daemon] log rotation failed (continuing): {e}");
    }
}

fn try_rotate(path: &str, max_bytes: u64) -> io::Result<()> {
    // Missing file is fine — nothing to rotate.
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    if meta.len() < max_bytes {
        return Ok(());
    }

    // Build a timestamp suffix from UNIX seconds (no extra crate needed).
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let rotated = format!("{}.{}.gz", path, secs);

    // Stream the existing log into the gzip — never fs::read the whole file
    // into RAM (a multi-GB log would pin gigabytes on the boot path).
    let mut src = fs::File::open(path)?;
    let out_file = fs::File::create(&rotated)?;
    let mut encoder = GzEncoder::new(out_file, Compression::default());
    io::copy(&mut src, &mut encoder)?;
    encoder.finish()?;
    drop(src);

    // Truncate the original to 0 bytes so the daemon starts a fresh log.
    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;

    // Prune old archives so .gz files don't accumulate forever in /tmp.
    prune_old_rotations(path, KEEP_ROTATIONS);

    Ok(())
}

/// Keep only the newest `keep` `<path>.<secs>.gz` archives, deleting older ones.
/// Best-effort: errors are swallowed (pruning must never crash the daemon).
/// The `<secs>` timestamp is fixed-width for the next ~250 years, so a lexical
/// sort of the file names is also chronological.
fn prune_old_rotations(path: &str, keep: usize) {
    let p = std::path::Path::new(path);
    let (dir, prefix) = match (p.parent(), p.file_name().and_then(|n| n.to_str())) {
        (Some(d), Some(f)) => (d.to_path_buf(), format!("{f}.")),
        _ => return,
    };
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut archives: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|pb| {
            pb.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix) && n.ends_with(".gz"))
                .unwrap_or(false)
        })
        .collect();
    if archives.len() <= keep {
        return;
    }
    archives.sort(); // chronological by embedded unix-secs
    let remove_count = archives.len() - keep;
    for old in archives.into_iter().take(remove_count) {
        let _ = fs::remove_file(old);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(name);
        p
    }

    fn cleanup(p: &PathBuf) {
        let _ = fs::remove_file(p);
        // Also remove any .gz siblings with the same base name.
        if let Some(dir) = p.parent() {
            if let Some(fname) = p.file_name().and_then(|n| n.to_str()) {
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let s = name.to_string_lossy();
                        if s.starts_with(fname) && s.ends_with(".gz") {
                            let _ = fs::remove_file(entry.path());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn small_file_is_left_untouched() {
        let p = tmp_path("a2fa_log_rotation_small.log");
        cleanup(&p);

        // Write well under the threshold.
        let content = b"hello log\n";
        fs::write(&p, content).unwrap();

        rotate_log_if_huge(p.to_str().unwrap(), 1024 * 1024); // 1 MB threshold

        // File still has its original contents.
        let after = fs::read(&p).unwrap();
        assert_eq!(after, content, "small file should not be modified");

        // No .gz sibling should exist.
        let dir = p.parent().unwrap();
        let fname = p.file_name().unwrap().to_string_lossy().into_owned();
        let gz_found = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.starts_with(&fname) && s.ends_with(".gz")
            });
        assert!(!gz_found, "no .gz file should be created for a small log");

        cleanup(&p);
    }

    #[test]
    fn large_file_is_rotated() {
        let p = tmp_path("a2fa_log_rotation_large.log");
        cleanup(&p);

        // Write 11 bytes with a 10-byte threshold → triggers rotation.
        let content = b"hello world"; // 11 bytes
        fs::write(&p, content).unwrap();

        rotate_log_if_huge(p.to_str().unwrap(), 10); // 10 byte threshold

        // Original should be truncated to 0.
        let after_size = fs::metadata(&p).unwrap().len();
        assert_eq!(after_size, 0, "original log should be truncated to 0 bytes after rotation");

        // A .gz sibling must exist.
        let dir = p.parent().unwrap();
        let fname = p.file_name().unwrap().to_string_lossy().into_owned();
        let gz_found = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.starts_with(&fname) && s.ends_with(".gz")
            });
        assert!(gz_found, "a .gz rotated file should exist after rotation");

        cleanup(&p);
    }

    #[test]
    fn prune_keeps_only_newest_archives() {
        let base = tmp_path("a2fa_log_prune_test.log");
        cleanup(&base);
        let base_str = base.to_str().unwrap();

        // Create 6 fake rotated archives with ascending unix-secs suffixes.
        let mut made = Vec::new();
        for secs in [100u64, 200, 300, 400, 500, 600] {
            let gz = format!("{base_str}.{secs}.gz");
            fs::write(&gz, b"x").unwrap();
            made.push(gz);
        }

        prune_old_rotations(base_str, 3);

        // Only the 3 newest (400,500,600) should survive.
        for secs in [100u64, 200, 300] {
            assert!(!std::path::Path::new(&format!("{base_str}.{secs}.gz")).exists(),
                "old archive {secs} should be pruned");
        }
        for secs in [400u64, 500, 600] {
            assert!(std::path::Path::new(&format!("{base_str}.{secs}.gz")).exists(),
                "recent archive {secs} should be kept");
        }
        cleanup(&base);
    }

    #[test]
    fn missing_file_does_not_panic() {
        let p = tmp_path("a2fa_log_rotation_nonexistent_zzz.log");
        // Make sure it truly doesn't exist.
        let _ = fs::remove_file(&p);

        // Must return without panicking.
        rotate_log_if_huge(p.to_str().unwrap(), 10);
    }
}
