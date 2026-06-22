use std::path::PathBuf;

/// Resolve the directory that holds passwords.json / tunnels.json.
///
/// Honors `SSH_CONFIG_PATH` only when it points at a directory that actually
/// exists (after `~` expansion). A stale or foreign value — e.g. another
/// machine's path injected by a leftover `.env` — is silently ignored so the
/// daemon never reads an empty config from a non-existent directory.
///
/// Falls back to `$HOME/.ssh`, where auto2fa has always stored its config.
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SSH_CONFIG_PATH") {
        if !p.is_empty() {
            let expanded = expand_tilde(&p);
            if expanded.is_dir() {
                return expanded;
            }
            log::warn!(
                "[config] SSH_CONFIG_PATH={:?} is not an existing directory; falling back to ~/.ssh",
                p
            );
        }
    }
    // Fallback: $HOME/.ssh
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        // Absolute last resort — shouldn't happen on any real Unix.
        "/tmp".to_owned()
    });
    PathBuf::from(home).join(".ssh")
}

/// The user's `~/.ssh` directory — where the ControlPath sockets, the managed
/// `ssh2fa.conf`, and the daemon wrapper live. Distinct from [`config_dir`]
/// (which holds passwords.json and may be elsewhere via `SSH_CONFIG_PATH`).
pub fn ssh_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
    std::path::PathBuf::from(home).join(".ssh")
}

/// The app-owned ssh config the daemon reads via `ssh -F`. The app writes it
/// (Includes the managed hosts file + the user's `~/.ssh/config`); the daemon
/// only reads it. Co-located with `~/.ssh` so a relative `Include ssh2fa.conf`
/// resolves correctly.
pub fn daemon_ssh_config_path() -> std::path::PathBuf {
    ssh_dir().join("ssh2fa-daemon.conf")
}

/// ssh args that point ssh at the app-managed config: `["-F", <wrapper>]` when
/// the wrapper exists, else EMPTY — so a daemon running before the app has
/// written the wrapper (or an older install) falls back to resolving from the
/// user's own `~/.ssh/config` exactly as before. Never hard-fails on absence.
pub fn managed_config_args() -> Vec<String> {
    managed_config_args_for(&daemon_ssh_config_path())
}

/// Testable core of [`managed_config_args`].
pub fn managed_config_args_for(wrapper: &std::path::Path) -> Vec<String> {
    if wrapper.is_file() {
        vec!["-F".to_owned(), wrapper.to_string_lossy().into_owned()]
    } else {
        Vec::new()
    }
}

/// Expand a leading `~/` or a bare `~` to the value of `$HOME`.
fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        return PathBuf::from(
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned()),
        );
    }
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var mutations are process-global. Use a mutex so parallel test
    // threads don't stomp each other's SSH_CONFIG_PATH reads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn falls_back_to_dot_ssh_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("SSH_CONFIG_PATH");
        assert!(config_dir().to_string_lossy().ends_with(".ssh"));
    }

    #[test]
    fn falls_back_when_path_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SSH_CONFIG_PATH", "/definitely/not/here/xyz123");
        assert!(config_dir().to_string_lossy().ends_with(".ssh"));
        std::env::remove_var("SSH_CONFIG_PATH");
    }

    #[test]
    fn honors_existing_dir() {
        let _g = ENV_LOCK.lock().unwrap();
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("SSH_CONFIG_PATH", d.path());
        assert_eq!(config_dir(), d.path());
        std::env::remove_var("SSH_CONFIG_PATH");
    }

    #[test]
    fn ssh_dir_is_home_dot_ssh() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(ssh_dir(), std::path::PathBuf::from(home).join(".ssh"));
    }

    #[test]
    fn daemon_ssh_config_path_is_in_ssh_dir() {
        assert_eq!(daemon_ssh_config_path(), ssh_dir().join("ssh2fa-daemon.conf"));
    }

    #[test]
    fn managed_config_args_empty_when_wrapper_absent() {
        let missing = std::path::PathBuf::from("/no/such/ssh2fa-daemon.conf");
        assert!(managed_config_args_for(&missing).is_empty());
    }

    #[test]
    fn managed_config_args_has_dash_f_when_wrapper_present() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ssh2fa-daemon.conf");
        std::fs::write(&p, "Include ~/.ssh/config\n").unwrap();
        let args = managed_config_args_for(&p);
        assert_eq!(args, vec!["-F".to_string(), p.to_string_lossy().into_owned()]);
    }
}
