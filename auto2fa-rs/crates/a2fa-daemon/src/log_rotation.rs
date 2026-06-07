//! Daemon log rotation — mirrors Python `_rotate_log_if_huge`.
//!
//! Called once at daemon startup *before* the logger is initialised so that
//! the log file never grows unbounded across restarts.

use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
const LOG_PATH: &str = "/tmp/auto2fa_daemon.log";

/// Convenience entry-point used by `main.rs`.
pub fn rotate_daemon_log() {
    rotate_log_if_huge(LOG_PATH, DEFAULT_MAX_BYTES);
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

    // Read the existing log and gzip it into the rotated file.
    let src_data = fs::read(path)?;
    let out_file = fs::File::create(&rotated)?;
    let mut encoder = GzEncoder::new(out_file, Compression::default());
    io::copy(&mut src_data.as_slice(), &mut encoder)?;
    encoder.finish()?;

    // Truncate the original to 0 bytes so the daemon starts a fresh log.
    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;

    Ok(())
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
    fn missing_file_does_not_panic() {
        let p = tmp_path("a2fa_log_rotation_nonexistent_zzz.log");
        // Make sure it truly doesn't exist.
        let _ = fs::remove_file(&p);

        // Must return without panicking.
        rotate_log_if_huge(p.to_str().unwrap(), 10);
    }
}
