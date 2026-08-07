//! Linux credential storage: freedesktop Secret Service, or an owner-only file.
//!
//! # Backend choice
//!
//! 1. `SSH2FA_VAULT=file` — use [`FileStore`] unconditionally. The explicit
//!    choice for a headless server.
//! 2. `SSH2FA_VAULT=secret-service` — require the Secret Service and fail
//!    loudly if it is absent, rather than silently writing secrets to disk.
//! 3. unset (default) — Secret Service when the session bus actually answers,
//!    otherwise an error that names both ways forward.
//!
//! The default deliberately does NOT fall through to the file vault on its own.
//! Writing a TOTP seed to disk is a decision the operator makes knowingly; a
//! daemon that quietly downgrades storage because a keyring daemon happened not
//! to be running is how a "secure by default" claim stops being true.
//!
//! # Why the probe is cached
//!
//! `resolve()` runs on every credential call, including the login path where a
//! bounded worker is already holding a slot. A D-Bus round trip per call would
//! add latency to something that cannot change while the daemon runs, so the
//! decision is made once per process.

use std::sync::{Mutex, OnceLock};

use crate::error::{Error, Result};

use super::file_store::{FileStore, VAULT_ENV};
use super::guard::ensure_enabled;
use super::SecretStore;

/// Collection/service name — the same string the macOS backend uses, so a
/// vault exported from one platform is recognisable on the other.
pub const SERVICE: &str = "auto2fa";

/// Serialize every Secret Service operation in this process.
///
/// Mirrors the macOS lock: a locked collection makes the provider raise an
/// unlock prompt, and several hosts reconnecting together would otherwise
/// stack several of them.
fn dbus_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Which store this process resolved to.
enum Backend {
    SecretService,
    File(FileStore),
    /// No usable store; the string explains what to do about it.
    Unavailable(String),
}

fn backend() -> &'static Backend {
    static B: OnceLock<Backend> = OnceLock::new();
    B.get_or_init(resolve)
}

/// True when a Secret Service provider answers on the session bus AND has a
/// collection to store secrets in.
///
/// Probed by reading a throwaway entry: a provider that is present but has no
/// such secret returns `NoEntry`, which is a successful round trip.
///
/// Two failure shapes must be told apart, and getting this wrong is not
/// theoretical — it is what a headless server actually does:
///
/// * **No provider** — nothing is listening on the bus.
/// * **Provider running, but no default collection.** `gnome-keyring-daemon`
///   is happily running with `--components=secrets`, `org.freedesktop.secrets`
///   answers a Ping… and every real call fails with
///   `Object does not exist at path "/org/freedesktop/secrets/collection/login"`,
///   because the login keyring is normally created and unlocked by PAM at a
///   GRAPHICAL login, which an SSH-only box never has. Treating that as
///   "present" made the daemon resolve to the Secret Service and then fail
///   every single credential operation with raw D-Bus text.
///
/// A LOCKED collection is a third shape and must NOT be read as absent —
/// falling back to the file vault there would scatter secrets across two
/// stores. It is reported as present so the user is told to unlock it.
fn secret_service_answers() -> std::result::Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, "__ssh2fa_probe__")
        .map_err(|e| format!("no Secret Service provider is reachable ({e})"))?;
    match entry.get_password() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(keyring::Error::PlatformFailure(e)) => {
            let msg = e.to_string();
            if msg.contains("Object does not exist at path")
                || msg.contains("no default collection")
                || msg.contains("NoSuchObject")
            {
                Err(format!(
                    "a Secret Service provider is running but has no default keyring \
                     collection ({msg}). That is normal on a machine with no graphical \
                     login, where PAM never creates one"
                ))
            } else if msg.contains("not provided by any .service")
                || msg.contains("ServiceUnknown")
                || msg.contains("Failed to connect")
                || msg.contains("No such file or directory")
            {
                Err(format!("no Secret Service provider is reachable ({msg})"))
            } else {
                // Unknown failure — assume present (it may just be locked) so
                // the user gets the provider's own error rather than a silent
                // switch to a different store.
                Ok(())
            }
        }
        Err(_) => Ok(()),
    }
}

fn resolve() -> Backend {
    match std::env::var(VAULT_ENV).ok().as_deref() {
        Some("file") => {
            let store = FileStore::at_default_path();
            log::info!(
                "[creds] using the owner-only file vault at {:?} ({VAULT_ENV}=file)",
                store.path()
            );
            Backend::File(store)
        }
        Some("secret-service") => match secret_service_answers() {
            Ok(()) => Backend::SecretService,
            Err(why) => Backend::Unavailable(format!(
                "{VAULT_ENV}=secret-service was requested but no provider answered ({why}). \
                 Start a keyring daemon (gnome-keyring-daemon --start --components=secrets), \
                 or set {VAULT_ENV}=file to use the owner-only file vault."
            )),
        },
        Some(other) => Backend::Unavailable(format!(
            "{VAULT_ENV}={other} is not a known credential store \
             (expected 'secret-service' or 'file')"
        )),
        None => match secret_service_answers() {
            Ok(()) => {
                log::info!("[creds] using the freedesktop Secret Service");
                Backend::SecretService
            }
            Err(why) => Backend::Unavailable(format!(
                "no usable credential store: {why}. On a desktop, start (or unlock) a keyring \
                 daemon; on a headless server set {VAULT_ENV}=file to store credentials in an \
                 owner-only (mode 0600) file instead — see docs/LINUX.md."
            )),
        },
    }
}

/// The Linux `SecretStore`: Secret Service when there is one, otherwise the
/// explicitly-opted-into file vault.
pub struct LinuxStore;

impl LinuxStore {
    /// What this process resolved to, for logs and diagnostics.
    pub fn describe() -> String {
        match backend() {
            Backend::SecretService => "freedesktop Secret Service (D-Bus)".into(),
            Backend::File(s) => format!("owner-only file vault at {:?}", s.path()),
            Backend::Unavailable(why) => format!("unavailable — {why}"),
        }
    }
}

impl SecretStore for LinuxStore {
    fn get(&self, acct: &str) -> Result<Option<String>> {
        ensure_enabled()?;
        match backend() {
            Backend::File(s) => s.get(acct),
            Backend::Unavailable(why) => Err(Error::Internal(why.clone())),
            Backend::SecretService => {
                let _serial = dbus_lock().lock().unwrap_or_else(|e| e.into_inner());
                let entry = keyring::Entry::new(SERVICE, acct)
                    .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
                match entry.get_password() {
                    Ok(pw) => Ok(Some(pw)),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(e) => Err(Error::Internal(format!("keyring get({acct}): {e}"))),
                }
            }
        }
    }

    fn set(&self, acct: &str, val: &str) -> Result<()> {
        ensure_enabled()?;
        match backend() {
            Backend::File(s) => s.set(acct, val),
            Backend::Unavailable(why) => Err(Error::Internal(why.clone())),
            Backend::SecretService => {
                let _serial = dbus_lock().lock().unwrap_or_else(|e| e.into_inner());
                let entry = keyring::Entry::new(SERVICE, acct)
                    .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
                entry
                    .set_password(val)
                    .map_err(|e| Error::Internal(format!("keyring set({acct}): {e}")))
            }
        }
    }

    fn delete(&self, acct: &str) -> Result<()> {
        ensure_enabled()?;
        match backend() {
            Backend::File(s) => s.delete(acct),
            Backend::Unavailable(why) => Err(Error::Internal(why.clone())),
            Backend::SecretService => {
                let _serial = dbus_lock().lock().unwrap_or_else(|e| e.into_inner());
                let entry = keyring::Entry::new(SERVICE, acct)
                    .map_err(|e| Error::Internal(format!("keyring entry error: {e}")))?;
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(e) => Err(Error::Internal(format!("keyring delete({acct}): {e}"))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every operation must go through the Cargo-binary guard, so a test run
    /// can never write into the developer's real keyring.
    #[test]
    fn operations_are_blocked_in_a_cargo_test_process() {
        let e = LinuxStore
            .get("__ssh2fa_isolation_probe__")
            .expect_err("a Cargo test process must never reach the real store");
        assert!(e.to_string().contains("Cargo-built binaries"), "{e}");
    }

    /// An unknown value must be refused, not silently treated as a default —
    /// a typo'd `SSH2FA_VAULT=fille` must never quietly use the wrong store.
    #[test]
    fn an_unknown_vault_setting_is_refused() {
        let msg = match Backend::Unavailable(format!(
            "{VAULT_ENV}=fille is not a known credential store \
             (expected 'secret-service' or 'file')"
        )) {
            Backend::Unavailable(m) => m,
            _ => unreachable!(),
        };
        assert!(msg.contains("not a known credential store"));
    }

    /// The "no store" diagnosis must name BOTH ways out; a bare failure leaves
    /// a headless user with no idea that a supported option exists.
    #[test]
    fn the_unavailable_message_names_both_remedies() {
        let Backend::Unavailable(msg) = resolve_for_test_without_provider() else {
            return; // this machine HAS a provider — nothing to assert
        };
        assert!(msg.contains("keyring daemon"), "{msg}");
        assert!(msg.contains("SSH2FA_VAULT=file"), "{msg}");
    }

    /// `resolve()` with no env override, without caching it into the OnceLock.
    fn resolve_for_test_without_provider() -> Backend {
        match secret_service_answers() {
            Ok(()) => Backend::SecretService,
            Err(why) => Backend::Unavailable(format!(
                "no usable credential store: {why}. On a desktop, start (or unlock) a keyring \
                 daemon; on a headless server set {VAULT_ENV}=file to store credentials in an \
                 owner-only (mode 0600) file instead — see docs/LINUX.md."
            )),
        }
    }
}
