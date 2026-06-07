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
}
