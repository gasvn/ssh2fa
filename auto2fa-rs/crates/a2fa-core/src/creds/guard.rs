//! Safety rail shared by every real credential backend.
//!
//! Cargo-built test and development binaries must never touch the developer's
//! real credential store by accident — not the macOS login Keychain, not the
//! Linux Secret Service collection, not an on-disk vault. The guard lives here
//! rather than inside one backend so a newly added platform cannot silently
//! ship without it.

use std::path::Path;

use crate::error::{Error, Result};

/// True for a binary produced by `cargo build` / `cargo test` / `cargo run`.
///
/// `cfg(test)` is not sufficient: a2fa-core is compiled as an ordinary
/// dependency of the a2fa-daemon test harness, so its own `cfg(test)` is false.
/// Inspecting the executable path protects unit tests, integration tests, and
/// casual `cargo run` sessions. A developer can still opt in deliberately.
pub(crate) fn is_cargo_build_executable(path: &Path) -> bool {
    let path = path.to_string_lossy().replace('\\', "/");
    path.contains("/target/debug/") || path.contains("/target/release/")
}

/// `Ok` when this process is allowed to reach the real credential store.
pub(crate) fn ensure_enabled() -> Result<()> {
    if std::env::var_os("SSH2FA_DISABLE_KEYCHAIN").is_some() {
        return Err(Error::Internal(
            "Keychain access is disabled for this isolated test daemon".into(),
        ));
    }
    if std::env::var_os("SSH2FA_ALLOW_DEVELOPMENT_KEYCHAIN").is_none()
        && std::env::current_exe()
            .ok()
            .as_deref()
            .is_some_and(is_cargo_build_executable)
    {
        return Err(Error::Internal(
            "Keychain access is disabled for Cargo-built binaries; set \
             SSH2FA_ALLOW_DEVELOPMENT_KEYCHAIN=1 only for an intentional manual test"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_test_and_run_paths_require_explicit_keychain_opt_in() {
        assert!(is_cargo_build_executable(Path::new(
            "/repo/target/debug/deps/a2fa_daemon-012cad784be543bf"
        )));
        assert!(is_cargo_build_executable(Path::new(
            "/repo/target/release/ssh2fa-daemon"
        )));
        assert!(!is_cargo_build_executable(Path::new(
            "/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon"
        )));
        assert!(!is_cargo_build_executable(Path::new("/usr/local/bin/a2fa")));
        // Linux install locations must be recognised as NON-cargo too, or the
        // packaged daemon would refuse to read its own credentials.
        assert!(!is_cargo_build_executable(Path::new(
            "/home/user/.local/bin/ssh2fa-daemon"
        )));
        assert!(!is_cargo_build_executable(Path::new(
            "/usr/bin/ssh2fa-daemon"
        )));
    }

    /// The guard must reject THIS process (a Cargo test binary) before any
    /// backend call happens — on every platform.
    #[test]
    fn this_cargo_test_process_is_blocked() {
        let e = ensure_enabled().expect_err("a Cargo test process must never reach the real store");
        assert!(e.to_string().contains("Cargo-built binaries"), "{e}");
    }
}
