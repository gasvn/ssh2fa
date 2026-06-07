//! Single-instance flock guard.
//!
//! Mirrors `_acquire_singleton_lock` in daemon.py.  Uses `fs2::FileExt` for a
//! non-blocking exclusive flock so the second daemon to start exits cleanly
//! rather than blocking or clobbering the first one's socket.
//!
//! The caller must keep the returned `File` alive for the process lifetime.
//! Dropping it releases the flock, which is correct only at clean shutdown.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use fs2::FileExt;

/// Acquire an exclusive, non-blocking flock on `lock_path`.
///
/// Returns:
/// - `Ok(Some(file))` — lock acquired; **keep** the file alive.
/// - `Ok(None)`       — lock already held by another process; caller should exit.
/// - `Err(_)`         — unexpected I/O error.
pub fn acquire_lock(lock_path: &Path) -> Result<Option<File>> {
    // Ensure the parent directory exists.
    if let Some(dir) = lock_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create lock dir {:?}", dir))?;
    }

    let file = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .with_context(|| format!("open lock file {:?}", lock_path))?;

    match file.try_lock_exclusive() {
        Ok(()) => {
            // Write our PID so an operator can `cat ~/.auto2fa/lock` to find us.
            use std::io::Write;
            let mut f = &file;
            let _ = f.write_all(format!("{}\n", std::process::id()).as_bytes());
            Ok(Some(file))
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            // Another daemon already holds the lock — don't fight over it.
            Ok(None)
        }
        Err(e) => Err(e).context("try_lock_exclusive failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn acquires_when_no_other_holder() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("lock");
        let result = acquire_lock(&lock_path).unwrap();
        assert!(result.is_some(), "should have acquired the lock");
    }

    #[test]
    fn second_attempt_returns_none() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("lock");
        let _first = acquire_lock(&lock_path).unwrap().unwrap();
        let second = acquire_lock(&lock_path).unwrap();
        assert!(second.is_none(), "second acquire should fail (lock held)");
    }
}
